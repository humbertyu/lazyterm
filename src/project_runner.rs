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
    checked: Option<Instant>,
    pending: Option<Receiver<io::Result<Snapshot>>>,
    error: Option<String>,
}

impl Handle {
    pub fn active(&self) -> bool {
        self.snapshot.active()
    }

    fn new(snapshot: Snapshot, record: PathBuf) -> Self {
        Self {
            snapshot,
            record,
            checked: None,
            pending: None,
            error: None,
        }
    }

    fn request(expected: &Snapshot, action: &str) -> io::Result<Snapshot> {
        let mut stream = UnixStream::connect(&expected.socket)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        serde_json::to_writer(&mut stream, &(expected.token.as_str(), action))?;
        stream.write_all(b"\n")?;
        let snapshot: Snapshot = serde_json::from_reader(stream)?;
        if snapshot.token != expected.token || snapshot.owner != expected.owner {
            return Err(io::Error::other("Background run identity changed."));
        }
        Ok(snapshot)
    }

    pub fn stop(&mut self) -> io::Result<()> {
        if self.active() {
            self.pending = None;
            self.snapshot = Self::request(&self.snapshot, "stop")?;
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
            let (sender, receiver) = mpsc::channel();
            self.pending = Some(receiver);
            std::thread::spawn(move || {
                let result = Self::request(&snapshot, "status").or_else(|error| {
                    let saved: Snapshot = read_json(&record)?;
                    if saved.owner.key != snapshot.owner.key {
                        return Err(io::Error::other("Background record identity changed."));
                    }
                    if !saved.active() {
                        Ok(saved)
                    } else if saved.token != snapshot.token {
                        Self::request(&saved, "status")
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

fn lock_run(root: &Path, key: TaskKey) -> io::Result<File> {
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
        return Err(io::Error::other(
            "This command already has a background runner.",
        ));
    }
    Ok(lock)
}

pub(crate) fn forget(root: &Path, key: TaskKey) -> io::Result<()> {
    let record = record_path(root, key);
    if !record.try_exists()? {
        return Ok(());
    }
    let _lock = lock_run(root, key)?;
    let snapshot: Snapshot = read_json(&record)?;
    if snapshot.active() || snapshot.owner.key != key {
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
    let record = record_path(root, key);
    let snapshot: Snapshot = match read_json(&record) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if snapshot.owner.key != key {
        return Err(io::Error::other(
            "Background record identity does not match its command.",
        ));
    }
    let mut handle = Handle::new(snapshot, record);
    // Keep an unreachable active run reserved rather than launching a duplicate.
    let _ = handle.refresh();
    Ok(Some(handle))
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
                break Ok(Handle::new(snapshot, record));
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
}
