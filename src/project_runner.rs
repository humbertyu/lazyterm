use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LegacyTaskId {
    pub project_id: u64,
    pub command_id: u64,
    pub project_uuid: uuid::Uuid,
    pub command_uuid: uuid::Uuid,
}

const LEGACY_IDS_FILE: &str = "legacy-runner-ids.json";

pub(crate) fn save_legacy_ids(root: PathBuf, ids: &[LegacyTaskId]) -> io::Result<()> {
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let path = root.join(LEGACY_IDS_FILE);
    let mut all = if path.exists() {
        read_json::<Vec<LegacyTaskId>>(&path)?
    } else {
        Vec::new()
    };
    for id in ids {
        if let Some(saved) = all
            .iter_mut()
            .find(|saved| saved.project_id == id.project_id && saved.command_id == id.command_id)
        {
            *saved = id.clone();
        } else {
            all.push(id.clone());
        }
    }
    write_json(&path, &all)
}

use crate::projects::{
    ProjectCommand, ProjectDefinition, ProjectLifecycle, ProjectOwner, ProjectService, TaskKey,
    TaskStatus,
};

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Snapshot {
    pub owner: ProjectOwner,
    pub directory: PathBuf,
    pub command: String,
    pub status: TaskStatus,
    running: bool,
    pub output: String,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
    socket: PathBuf,
    token: String,
}

impl Snapshot {
    pub fn active(&self) -> bool {
        self.running
    }
}

pub(crate) struct Handle {
    pub snapshot: Snapshot,
    record: PathBuf,
    legacy_key: Option<(u64, u64)>,
    checked: Option<Instant>,
    pending: Option<Receiver<io::Result<Snapshot>>>,
    error: Option<String>,
}

impl Handle {
    pub fn active(&self) -> bool {
        self.snapshot.active()
    }

    fn new(snapshot: Snapshot, record: PathBuf, legacy_key: Option<(u64, u64)>) -> Self {
        Self {
            snapshot,
            record,
            legacy_key,
            checked: None,
            pending: None,
            error: None,
        }
    }

    fn request(
        expected: &Snapshot,
        action: &str,
        legacy_key: Option<(u64, u64)>,
    ) -> io::Result<Snapshot> {
        let mut stream = UnixStream::connect(&expected.socket)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        serde_json::to_writer(&mut stream, &(expected.token.as_str(), action))?;
        stream.write_all(b"\n")?;
        let value: serde_json::Value = serde_json::from_reader(stream)?;
        let snapshot = parse_snapshot(value, expected.owner.key, legacy_key)?;
        if snapshot.token != expected.token || snapshot.owner != expected.owner {
            return Err(io::Error::other("Background run identity changed."));
        }
        Ok(snapshot)
    }

    pub fn stop(&mut self) -> io::Result<()> {
        if self.active() {
            self.pending = None;
            self.snapshot = Self::request(&self.snapshot, "stop", self.legacy_key)?;
            self.error = None;
        }
        Ok(())
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        if let Some(receiver) = &self.pending {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(io::Error::other(
                    "Background status worker disconnected.",
                ))),
            };
            if let Some(result) = result {
                self.pending = None;
                match result {
                    Ok(snapshot) => {
                        self.snapshot = snapshot;
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
        }
        if self.active()
            && self.pending.is_none()
            && self
                .checked
                .is_none_or(|time| time.elapsed() >= Duration::from_millis(250))
        {
            self.checked = Some(Instant::now());
            let snapshot = self.snapshot.clone();
            let record = self.record.clone();
            let legacy_key = self.legacy_key;
            let (sender, receiver) = mpsc::channel();
            self.pending = Some(receiver);
            std::thread::spawn(move || {
                let result = Self::request(&snapshot, "status", legacy_key).or_else(|error| {
                    let saved = reconcile_record(&record, snapshot.owner.key, legacy_key)?;
                    if saved.owner.key != snapshot.owner.key {
                        return Err(io::Error::other("Background record identity changed."));
                    }
                    if !saved.active() {
                        Ok(saved)
                    } else if saved.token != snapshot.token {
                        Self::request(&saved, "status", legacy_key)
                    } else {
                        Err(error)
                    }
                });
                let _ = sender.send(result);
            });
        }
        self.error
            .as_ref()
            .map_or(Ok(()), |error| Err(io::Error::other(error.clone())))
    }
}

fn parse_snapshot(
    mut value: serde_json::Value,
    key: TaskKey,
    legacy_key: Option<(u64, u64)>,
) -> io::Result<Snapshot> {
    if let Some(old_key) = legacy_key {
        let stored = value
            .pointer("/owner/key")
            .and_then(serde_json::Value::as_array)
            .filter(|parts| parts.len() == 2)
            .and_then(|parts| Some((parts[0].as_u64()?, parts[1].as_u64()?)));
        if stored == Some(old_key)
            && let Some(owner) = value.get_mut("owner")
        {
            owner["key"] = serde_json::json!([key.0, key.1]);
        }
    }
    let snapshot: Snapshot = serde_json::from_value(value)?;
    if snapshot.owner.key != key {
        return Err(io::Error::other(
            "Background record identity does not match its command.",
        ));
    }
    Ok(snapshot)
}

fn read_record(path: &Path, key: TaskKey, legacy_key: Option<(u64, u64)>) -> io::Result<Snapshot> {
    let value = read_json::<serde_json::Value>(path)?;
    parse_snapshot(value, key, legacy_key)
}

#[derive(Deserialize, Serialize)]
struct Launch {
    project: ProjectDefinition,
    command: ProjectCommand,
    log_root: PathBuf,
    run_id: u64,
    token: String,
}

fn record_path(root: &Path, key: TaskKey) -> PathBuf {
    root.join(format!("persistent-{}-{}.json", key.0, key.1))
}

fn try_lock_run(root: &Path, key: TaskKey) -> io::Result<Option<File>> {
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(root.join(format!("persistent-{}-{}.lock", key.0, key.1)))?;
    if unsafe {
        libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&lock),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(error);
    }
    Ok(Some(lock))
}

fn lock_run(root: &Path, key: TaskKey) -> io::Result<File> {
    try_lock_run(root, key)?
        .ok_or_else(|| io::Error::other("This command already has a background runner."))
}

pub(crate) fn forget(root: &Path, key: TaskKey) -> io::Result<()> {
    let current_record = record_path(root, key);
    let (record, legacy_key) = if current_record.exists() {
        (current_record, None)
    } else if let Some((old_key, old_record)) = legacy_record_path(root, key)? {
        (old_record, Some(old_key))
    } else {
        return Ok(());
    };
    let _lock = try_lock_legacy_or_current(root, key, legacy_key)?
        .ok_or_else(|| io::Error::other("This command still has a background runner."))?;
    let snapshot = read_record(&record, key, legacy_key)?;
    if snapshot.active() {
        return Err(io::Error::other(
            "Background record is still active or unavailable.",
        ));
    }
    fs::remove_file(record)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    Ok(serde_json::from_reader(File::open(path)?)?)
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, value)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn recover(root: &Path, key: TaskKey) -> io::Result<Option<Handle>> {
    let current_record = record_path(root, key);
    let (record, legacy_key) = if current_record.exists() {
        (current_record, None)
    } else if let Some((old_key, old_record)) = legacy_record_path(root, key)? {
        (old_record, Some(old_key))
    } else {
        return Ok(None);
    };
    let snapshot = reconcile_record(&record, key, legacy_key)?;
    let mut handle = Handle::new(snapshot, record, legacy_key);
    // An active lock means the runner may still own a live process group.
    let _ = handle.refresh();
    Ok(Some(handle))
}

fn reconcile_record(
    record: &Path,
    key: TaskKey,
    legacy_key: Option<(u64, u64)>,
) -> io::Result<Snapshot> {
    let root = record
        .parent()
        .ok_or_else(|| io::Error::other("Missing run directory"))?;
    let mut snapshot = read_record(record, key, legacy_key)?;
    if snapshot.active()
        && let Some(_lock) = try_lock_legacy_or_current(root, key, legacy_key)?
    {
        snapshot = read_record(record, key, legacy_key)?;
        if snapshot.active()
            && !crate::projects::group_has_live_processes(snapshot.owner.process_group)
        {
            snapshot.running = false;
            snapshot.status = TaskStatus::Failed(
                "Background runner exited unexpectedly. Start the command again.".into(),
            );
            write_json(record, &snapshot)?;
            // Only remove our private socket directory, never an arbitrary path from a record.
            if let Some(directory) = snapshot.socket.parent()
                && directory.parent() == Some(Path::new("/tmp"))
                && snapshot.socket.file_name() == Some(std::ffi::OsStr::new("control"))
                && directory.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with(&format!(
                        "lazyterm-run-{}-{:x}-",
                        unsafe { libc::geteuid() },
                        snapshot.owner.run_id
                    ))
                })
                && let Ok(metadata) = fs::symlink_metadata(directory)
                && metadata.is_dir()
            {
                let _ = fs::remove_file(&snapshot.socket);
                let _ = fs::remove_dir(directory);
            }
        }
    }
    Ok(snapshot)
}

fn legacy_record_path(root: &Path, key: TaskKey) -> io::Result<Option<((u64, u64), PathBuf)>> {
    let map_path = root.join(LEGACY_IDS_FILE);
    let ids = match read_json::<Vec<LegacyTaskId>>(&map_path) {
        Ok(ids) => ids,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(id) = ids
        .iter()
        .find(|id| (id.project_uuid, id.command_uuid) == key)
    else {
        return Ok(None);
    };
    let old_key = (id.project_id, id.command_id);
    let record = root.join(format!("persistent-{}-{}.json", old_key.0, old_key.1));
    Ok(record.exists().then_some((old_key, record)))
}

fn try_lock_legacy_or_current(
    root: &Path,
    key: TaskKey,
    legacy_key: Option<(u64, u64)>,
) -> io::Result<Option<File>> {
    if let Some((project_id, command_id)) = legacy_key {
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join(format!("persistent-{project_id}-{command_id}.lock")))?;
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&lock),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(lock))
    } else {
        try_lock_run(root, key)
    }
}

pub(crate) fn start(
    root: &Path,
    project: &ProjectDefinition,
    command: &ProjectCommand,
) -> io::Result<Handle> {
    fs::create_dir_all(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    let run_id = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos(),
    )
    .map_err(io::Error::other)?;
    let mut random = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let token = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let spec = root.join(format!("launch-{}-{run_id}.json", std::process::id()));
    write_json(
        &spec,
        &Launch {
            project: project.clone(),
            command: command.clone(),
            log_root: root.to_path_buf(),
            run_id,
            token: token.clone(),
        },
    )?;
    let mut worker = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    worker.arg("--project-runner").arg(&spec);
    #[cfg(test)]
    worker
        .args([
            "--ignored",
            "--exact",
            "project_runner::tests::runner_process",
            "--nocapture",
        ])
        .env("LAZYTERM_RUNNER_SPEC", &spec);
    worker
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach before exec, with no terminal or parent-owned output pipes.
    unsafe {
        worker.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let result = (|| {
        let mut child = worker.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            let record = record_path(root, (project.id, command.id));
            if let Ok(snapshot) = read_json::<Snapshot>(&record)
                && snapshot.token == token
            {
                break Ok(Handle::new(snapshot, record, None));
            }
            if let Some(status) = child.try_wait()? {
                let detail = fs::read_to_string(spec.with_extension("error"))
                    .unwrap_or_else(|_| status.to_string());
                break Err(io::Error::other(format!(
                    "Background start failed: {detail}"
                )));
            }
            if Instant::now() >= deadline {
                break Err(io::Error::other(
                    "Background start timed out; inspect Projects before retrying.",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        result
    })();
    let _ = fs::remove_file(&spec);
    let _ = fs::remove_file(spec.with_extension("error"));
    result
}

pub(crate) fn run(spec: &Path) -> io::Result<()> {
    let result = run_inner(spec);
    if let Err(error) = &result
        && let Ok(mut file) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(spec.with_extension("error"))
    {
        let _ = write!(file, "{error}");
    }
    result
}

struct SocketDirectory(PathBuf);

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("control"));
        let _ = fs::remove_dir(&self.0);
    }
}

fn run_inner(spec: &Path) -> io::Result<()> {
    let mut launch: Launch = read_json(spec)?;
    let key = (launch.project.id, launch.command.id);
    let _lock = lock_run(&launch.log_root, key)?;
    let directory = SocketDirectory(PathBuf::from(format!(
        "/tmp/lazyterm-run-{}-{:x}-{}",
        unsafe { libc::geteuid() },
        launch.run_id,
        std::process::id()
    )));
    fs::create_dir(&directory.0)?;
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700))?;
    let socket = directory.0.join("control");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let mut service = ProjectService::new(Some(launch.log_root.clone()));
    service.set_next_run(launch.run_id);
    // The independent runner owns the ordinary app-scoped supervisor.
    launch.command.lifecycle = ProjectLifecycle::App;
    service.start_app(&launch.project, &launch.command)?;
    let record = record_path(&launch.log_root, key);
    let mut previous = None;
    loop {
        service.poll();
        let task = &service.tasks[&key];
        let snapshot = Snapshot {
            owner: task.owner.clone(),
            directory: task.directory.clone(),
            command: task.command.clone(),
            status: task.status.clone(),
            running: task.active(),
            output: task.output.clone(),
            stdout_log: task.stdout_log.clone(),
            stderr_log: task.stderr_log.clone(),
            socket: socket.clone(),
            token: launch.token.clone(),
        };
        if previous.as_ref() != Some(&(snapshot.status.clone(), snapshot.running)) {
            write_json(&record, &snapshot)?;
            previous = Some((snapshot.status.clone(), snapshot.running));
        }
        if !task.active() {
            break;
        }
        // Bound control work so log draining and process cleanup keep progressing.
        for _ in 0..8 {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            };
            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
            stream.set_write_timeout(Some(Duration::from_millis(500)))?;
            let mut line = String::new();
            if BufReader::new((&mut stream).take(4096))
                .read_line(&mut line)
                .is_err()
            {
                continue;
            }
            let Ok((token, action)) = serde_json::from_str::<(String, String)>(&line) else {
                continue;
            };
            if token != launch.token || !matches!(action.as_str(), "status" | "stop") {
                continue;
            }
            if action == "stop" {
                service.stop(key)?;
            }
            let mut response = snapshot.clone();
            response.status = service.tasks[&key].status.clone();
            let _ = serde_json::to_writer(&mut stream, &response);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigkill_tui_preserves_persistent_runner_then_sigkill_runner_cleans_children_and_recovers() {
        use crate::{
            process_guard::tests::{start_owner, wait_until},
            projects::tests::{Fixture, PersistentCleanup, until},
        };
        use std::net::TcpStream;

        let mut fixture = Fixture::new("sigkill-persistent");
        let mut command = fixture.listener_command(true);
        command.lifecycle = ProjectLifecycle::Persistent;
        fixture.project.commands.push(command.clone());
        let key = (fixture.project.id, command.id);
        let _cleanup = PersistentCleanup::new(&fixture, key);
        let mut owner = start_owner(&fixture, true);
        let (_, port) = fixture.ready().unwrap();
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();

        let root = fixture.root.join("logs");
        let mut service = fixture.service();
        service.restore(std::slice::from_ref(&fixture.project));
        assert!(service.active(key));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        assert!(
            service.start(&fixture.project, &command).is_err(),
            "Must not duplicate the surviving run"
        );

        let saved = read_json::<Snapshot>(&record_path(&root, key)).unwrap();
        let socket_dir = saved.socket.parent().unwrap().to_path_buf();
        let runner_pid: i32 = socket_dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .rsplit('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(runner_pid, libc::SIGKILL) }, 0);
        until(&mut service, |service| !service.active(key));
        assert!(
            matches!(service.tasks[&key].status, TaskStatus::Failed(ref message) if message.contains("exited unexpectedly"))
        );
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        assert!(
            !socket_dir.exists(),
            "Abandoned control socket must be removed"
        );
        assert!(
            !read_json::<Snapshot>(&record_path(&root, key))
                .unwrap()
                .active()
        );
        wait_until(|| !crate::projects::group_has_live_processes(saved.owner.process_group));

        fs::remove_file(fixture.root.join("ready")).unwrap();
        service.start(&fixture.project, &command).unwrap();
        until(&mut service, |_| fixture.ready().is_some());
        assert_ne!(
            service.tasks[&key].owner.process_group,
            saved.owner.process_group
        );
        service.stop(key).unwrap();
        until(&mut service, |service| !service.active(key));
    }

    #[test]
    fn recovery_marks_an_unlocked_active_record_as_exited_and_allows_restart() {
        use crate::projects::tests::{Fixture, PersistentCleanup};
        let mut fixture = Fixture::new("recover-dead-runner");
        let mut command = fixture.command("sleep 30");
        command.lifecycle = ProjectLifecycle::Persistent;
        fixture.project.commands.push(command.clone());
        let key = (fixture.project.id, command.id);
        let root = fixture.root.join("logs");
        fs::create_dir_all(&root).unwrap();
        let record = record_path(&root, key);
        let snapshot = Snapshot {
            owner: ProjectOwner {
                key,
                run_id: 10,
                process_group: u32::MAX - 1,
                project_name: fixture.project.name.clone(),
                command_name: command.name.clone(),
            },
            directory: fixture.root.clone(),
            command: command.command.clone(),
            status: TaskStatus::Running,
            running: true,
            output: String::new(),
            stdout_log: root.join("stdout.log"),
            stderr_log: root.join("stderr.log"),
            socket: root.join("missing/control"),
            token: "dead-runner-token".into(),
        };
        write_json(&record, &snapshot).unwrap();

        let recovered = recover(&root, key).unwrap().unwrap();
        assert!(!recovered.active());
        assert!(matches!(
            recovered.snapshot.status,
            TaskStatus::Failed(ref message) if message.contains("exited unexpectedly")
        ));
        assert!(!read_json::<Snapshot>(&record).unwrap().active());

        let _cleanup = PersistentCleanup::new(&fixture, key);
        let mut service = fixture.service();
        service.start(&fixture.project, &command).unwrap();
        assert!(service.active(key));
    }

    #[test]
    fn startup_restore_recovers_a_dead_app_lifecycle_runner() {
        use crate::projects::tests::Fixture;
        let mut fixture = Fixture::new("restore-dead-app-runner");
        let command = fixture.command("sleep 30");
        fixture.project.commands.push(command.clone());
        let key = (fixture.project.id, command.id);
        let root = fixture.root.join("logs");
        fs::create_dir_all(&root).unwrap();
        let record = record_path(&root, key);
        let snapshot = Snapshot {
            owner: ProjectOwner {
                key,
                run_id: 11,
                process_group: u32::MAX - 1,
                project_name: fixture.project.name.clone(),
                command_name: command.name.clone(),
            },
            directory: fixture.root.clone(),
            command: command.command.clone(),
            status: TaskStatus::Running,
            running: true,
            output: String::new(),
            stdout_log: root.join("stdout.log"),
            stderr_log: root.join("stderr.log"),
            socket: root.join("missing/control"),
            token: "dead-app-runner-token".into(),
        };
        write_json(&record, &snapshot).unwrap();

        let mut service = fixture.service();
        service.restore(std::slice::from_ref(&fixture.project));

        let task = &service.tasks[&key];
        assert_eq!(task.lifecycle, ProjectLifecycle::App);
        assert!(!task.active());
        assert!(matches!(
            task.status,
            TaskStatus::Failed(ref message) if message.contains("exited unexpectedly")
        ));
        service.start(&fixture.project, &command).unwrap();
        assert!(service.active(key));
    }

    #[test]
    fn an_unlocked_record_cannot_signal_or_duplicate_an_unverified_live_group() {
        use crate::projects::tests::{Fixture, until};
        let live = Fixture::new("unverified-live-group");
        let mut live_service = live.service();
        live_service
            .start(&live.project, &live.listener_command(false))
            .unwrap();
        until(&mut live_service, |_| live.ready().is_some());
        let live_group = live_service.owners()[0].process_group;

        let mut fixture = Fixture::new("unverified-record");
        let mut command = fixture.command("sleep 30");
        command.lifecycle = ProjectLifecycle::Persistent;
        fixture.project.commands.push(command.clone());
        let key = (fixture.project.id, command.id);
        let root = fixture.root.join("logs");
        fs::create_dir_all(&root).unwrap();
        write_json(
            &record_path(&root, key),
            &Snapshot {
                owner: ProjectOwner {
                    key,
                    run_id: 12,
                    process_group: live_group,
                    project_name: "unknown".into(),
                    command_name: "unknown".into(),
                },
                directory: fixture.root.clone(),
                command: command.command.clone(),
                status: TaskStatus::Running,
                running: true,
                output: String::new(),
                stdout_log: root.join("stdout.log"),
                stderr_log: root.join("stderr.log"),
                socket: root.join("missing/control"),
                token: "unverified-token".into(),
            },
        )
        .unwrap();
        let mut service = fixture.service();
        service.restore(std::slice::from_ref(&fixture.project));
        assert!(service.active(key));
        assert!(service.start(&fixture.project, &command).is_err());
        let (_, port) = live.ready().unwrap();
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        assert!(crate::projects::group_has_live_processes(live_group));
    }

    #[test]
    #[ignore = "independent project runner subprocess helper"]
    fn runner_process() {
        if let Some(spec) = std::env::var_os("LAZYTERM_RUNNER_SPEC") {
            run(Path::new(&spec)).unwrap();
        }
    }

    #[test]
    fn control_authentication_and_exclusive_runner_lock_prevent_wrong_run_actions() {
        use crate::projects::tests::{Fixture, until};
        let fixture = Fixture::new("runner-control");
        let mut command = fixture.command("sleep 30");
        command.lifecycle = ProjectLifecycle::Persistent;
        let key = (fixture.project.id, command.id);
        let _cleanup = crate::projects::tests::PersistentCleanup::new(&fixture, key);
        let root = fixture.root.join("logs");
        let mut service = fixture.service();
        service.start(&fixture.project, &command).unwrap();
        let mut wrong = recover(&root, key).unwrap().unwrap();
        wrong.snapshot.token = "wrong token".into();
        assert!(wrong.stop().is_err());
        assert!(forget(&root, key).is_err());
        wrong.snapshot.socket = root.join("missing-socket");
        wrong.record = root.join("missing-record");
        let deadline = Instant::now() + Duration::from_secs(5);
        while wrong.refresh().is_ok() {
            assert!(
                Instant::now() < deadline,
                "Unavailable runner was not reported"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(wrong.active(), "Unavailable runs must stay reserved");
        assert!(wrong.refresh().is_err(), "Cached errors must stay visible");
        assert!(recover(&root, key).unwrap().unwrap().active());
        let duplicate = start(&root, &fixture.project, &command);
        assert!(duplicate.is_err());
        assert!(recover(&root, key).unwrap().unwrap().active());
        service.stop(key).unwrap();
        until(&mut service, |s| !s.active(key));
    }

    #[test]
    fn migrated_numeric_runner_is_recovered_refreshed_and_stopped() {
        use crate::projects::tests::Fixture;
        use std::os::fd::AsRawFd;

        let fixture = Fixture::new("legacy-runner-compatibility");
        let root = fixture.root.join("logs");
        fs::create_dir_all(&root).unwrap();
        let old_key = (41_u64, 73_u64);
        let key = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        save_legacy_ids(
            root.clone(),
            &[LegacyTaskId {
                project_id: old_key.0,
                command_id: old_key.1,
                project_uuid: key.0,
                command_uuid: key.1,
            }],
        )
        .unwrap();

        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(format!("persistent-{}-{}.lock", old_key.0, old_key.1)))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        let socket_dir = std::env::temp_dir().join(format!(
            "lt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().as_simple()
        ));
        fs::create_dir(&socket_dir).unwrap();
        let socket = socket_dir.join("control");
        let listener = UnixListener::bind(&socket).unwrap();
        let legacy_snapshot = serde_json::json!({
            "owner": {
                "key": [old_key.0, old_key.1],
                "run_id": 5,
                "process_group": std::process::id(),
                "project_name": "Legacy project",
                "command_name": "Legacy command"
            },
            "directory": fixture.root,
            "command": "sleep 30",
            "status": "Running",
            "running": true,
            "output": "",
            "stdout_log": root.join("stdout.log"),
            "stderr_log": root.join("stderr.log"),
            "socket": socket,
            "token": "old-runner-token"
        });
        write_json(
            &root.join(format!("persistent-{}-{}.json", old_key.0, old_key.1)),
            &legacy_snapshot,
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let (_, action): (String, String) = serde_json::from_str(&request).unwrap();
                let mut response = legacy_snapshot.clone();
                if action == "stop" {
                    response["running"] = false.into();
                    response["status"] = serde_json::json!("Stopped");
                }
                serde_json::to_writer(&mut stream, &response).unwrap();
            }
        });

        let mut handle = recover(&root, key).unwrap().unwrap();
        assert!(handle.active());
        handle.stop().unwrap();
        assert!(!handle.active());
        server.join().unwrap();
        fs::remove_dir_all(socket_dir).unwrap();
    }
}
