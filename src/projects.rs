use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub type TaskKey = (u64, u64);
const OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectDefinition {
    pub id: u64,
    pub name: String,
    pub base_path: PathBuf,
    #[serde(default)]
    pub commands: Vec<ProjectCommand>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub target: ProjectTarget,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProjectTarget {
    #[default]
    Local,
    Host(String),
}

impl ProjectTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Local => "Local".into(),
            Self::Host(alias) => format!("Host: {alias}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum CommandKind {
    Service,
    #[default]
    Task,
}

impl CommandKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Service => "Service",
            Self::Task => "Task",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectCommand {
    pub id: u64,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub kind: CommandKind,
    #[serde(default)]
    pub target: Option<ProjectTarget>,
}

pub fn execution_target<'a>(
    project: &'a ProjectDefinition,
    command: &'a ProjectCommand,
) -> &'a ProjectTarget {
    command.target.as_ref().unwrap_or(&project.target)
}

pub fn working_directory(
    project: &ProjectDefinition,
    command: &ProjectCommand,
) -> io::Result<PathBuf> {
    if *execution_target(project, command) != ProjectTarget::Local {
        return Err(io::Error::other(
            "Remote Project execution is not available yet.",
        ));
    }
    let base = expand_home(&project.base_path)?;
    if !base.is_absolute() {
        return Err(io::Error::other(
            "Project base path must be absolute or start with ~/",
        ));
    }
    let directory = match &command.working_directory {
        Some(path) if path.starts_with("~") => expand_home(path)?,
        Some(path) => base.join(path),
        None => base,
    };
    let directory = directory.canonicalize()?;
    if !directory.is_dir() {
        return Err(io::Error::other("Working directory is not a directory"));
    }
    Ok(directory)
}

fn expand_home(path: &Path) -> io::Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
        Ok(PathBuf::from(home).join(suffix))
    } else {
        Ok(path.to_path_buf())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectOwner {
    pub key: TaskKey,
    pub run_id: u64,
    pub process_group: u32,
    pub project_name: String,
    pub command_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskStatus {
    Running,
    Stopping,
    Exited(i32),
    Stopped,
    Failed(String),
}

impl TaskStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Running => "running".into(),
            Self::Stopping => "stopping".into(),
            Self::Exited(code) => format!("exit {code}"),
            Self::Stopped => "stopped".into(),
            Self::Failed(message) => format!("failed: {message}"),
        }
    }
}

pub struct ProjectTask {
    pub owner: ProjectOwner,
    pub directory: PathBuf,
    pub command: String,
    pub status: TaskStatus,
    pub output: String,
    pub stdout_log: PathBuf,
    pub stderr_log: PathBuf,
    child: Option<Child>,
    finished: Option<ExitStatus>,
    receiver: Receiver<OutputEvent>,
    closed_readers: usize,
    stop_deadline: Option<Instant>,
    user_stopped: bool,
    restart: Option<(ProjectDefinition, ProjectCommand)>,
}

impl ProjectTask {
    pub fn active(&self) -> bool {
        self.child.is_some()
    }

    fn request_stop(&mut self) -> io::Result<()> {
        if !self.active() || self.stop_deadline.is_some() {
            return Ok(());
        }
        signal_group(self.owner.process_group, libc::SIGTERM)?;
        self.user_stopped = true;
        self.status = TaskStatus::Stopping;
        self.stop_deadline = Some(Instant::now() + Duration::from_secs(2));
        Ok(())
    }
}

impl Drop for ProjectTask {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = signal_group(self.owner.process_group, libc::SIGKILL);
            let _ = child.wait();
        }
    }
}

enum OutputEvent {
    Text(String, bool),
    Closed,
    Error(String),
}

pub struct ProjectService {
    pub tasks: BTreeMap<TaskKey, ProjectTask>,
    log_root: Option<PathBuf>,
    next_run: u64,
}

impl ProjectService {
    pub fn new(log_root: Option<PathBuf>) -> Self {
        Self {
            tasks: BTreeMap::new(),
            log_root,
            next_run: 1,
        }
    }

    pub fn owners(&self) -> Vec<ProjectOwner> {
        self.tasks
            .values()
            .filter(|task| task.active())
            .map(|task| task.owner.clone())
            .collect()
    }

    pub fn active(&self, key: TaskKey) -> bool {
        self.tasks.get(&key).is_some_and(ProjectTask::active)
    }

    pub fn start(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
    ) -> io::Result<()> {
        let key = (project.id, command.id);
        if self.active(key) {
            return Err(io::Error::other("Command is already running"));
        }
        if command.command.trim().is_empty() {
            return Err(io::Error::other("Command is empty"));
        }
        let directory = working_directory(project, command)?;
        let log_root = self
            .log_root
            .as_ref()
            .ok_or_else(|| io::Error::other("Project log directory is unavailable"))?;
        fs::create_dir_all(log_root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(log_root, fs::Permissions::from_mode(0o700))?;
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let run_id = self.next_run;
        self.next_run += 1;
        let prefix = format!(
            "{}-{}-{}-{stamp}-{run_id}",
            project.id,
            command.id,
            std::process::id()
        );
        let stdout_log = log_root.join(format!("{prefix}.stdout.log"));
        let stderr_log = log_root.join(format!("{prefix}.stderr.log"));
        let stdout_file = private_log(&stdout_log)?;
        let stderr_file = private_log(&stderr_log)?;
        let mut process = Command::new("sh");
        process
            .arg("-c")
            .arg(&command.command)
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }
        #[cfg(not(unix))]
        return Err(io::Error::other(
            "Project process groups require macOS or Linux",
        ));
        let mut child = process.spawn()?;
        let process_group = child.id();
        let (sender, receiver) = mpsc::sync_channel(128);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        spawn_reader(stdout, stdout_file, sender.clone(), false);
        spawn_reader(stderr, stderr_file, sender, true);
        self.tasks.insert(
            key,
            ProjectTask {
                owner: ProjectOwner {
                    key,
                    run_id,
                    process_group,
                    project_name: project.name.clone(),
                    command_name: command.name.clone(),
                },
                directory,
                command: command.command.clone(),
                status: TaskStatus::Running,
                output: String::new(),
                stdout_log,
                stderr_log,
                child: Some(child),
                finished: None,
                receiver,
                closed_readers: 0,
                stop_deadline: None,
                user_stopped: false,
                restart: None,
            },
        );
        Ok(())
    }

    pub fn stop(&mut self, key: TaskKey) -> io::Result<()> {
        if let Some(task) = self.tasks.get_mut(&key) {
            task.restart = None;
            task.request_stop()?;
        }
        Ok(())
    }

    pub fn restart(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
    ) -> io::Result<()> {
        working_directory(project, command)?;
        let key = (project.id, command.id);
        if let Some(task) = self.tasks.get_mut(&key)
            && task.active()
        {
            task.request_stop()?;
            task.restart = Some((project.clone(), command.clone()));
            Ok(())
        } else {
            self.start(project, command)
        }
    }

    pub fn poll(&mut self) {
        let mut restarts = VecDeque::new();
        for task in self.tasks.values_mut().filter(|task| task.active()) {
            // Bound work per frame even if a command continuously floods output.
            for _ in 0..128 {
                let Ok(event) = task.receiver.try_recv() else {
                    break;
                };
                match event {
                    OutputEvent::Text(text, stderr) => {
                        if stderr {
                            task.output.push_str("[stderr] ");
                        }
                        task.output.push_str(&text);
                        if task.output.len() > OUTPUT_LIMIT {
                            let mut offset = task.output.len() - OUTPUT_LIMIT;
                            while !task.output.is_char_boundary(offset) {
                                offset += 1;
                            }
                            task.output.drain(..offset);
                        }
                    }
                    OutputEvent::Closed => task.closed_readers += 1,
                    OutputEvent::Error(error) => task.status = TaskStatus::Failed(error),
                }
            }
            if task.finished.is_none()
                && let Some(child) = task.child.as_mut()
            {
                match child.try_wait() {
                    Ok(status) => task.finished = status,
                    Err(error) => task.status = TaskStatus::Failed(error.to_string()),
                }
            }
            if task.finished.is_some() && task.stop_deadline.is_none() {
                // A shell can exit while a background server still owns its pipes.
                let _ = signal_group(task.owner.process_group, libc::SIGTERM);
                task.stop_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            if let Some(deadline) = task.stop_deadline
                && Instant::now() >= deadline
            {
                let _ = signal_group(task.owner.process_group, libc::SIGKILL);
            }
            if task.finished.is_some()
                && task.closed_readers >= 2
                && !group_has_live_processes(task.owner.process_group)
            {
                task.child = None;
                if !matches!(task.status, TaskStatus::Failed(_)) {
                    task.status = if task.user_stopped {
                        TaskStatus::Stopped
                    } else {
                        TaskStatus::Exited(
                            task.finished.and_then(|status| status.code()).unwrap_or(-1),
                        )
                    };
                }
                if let Some(restart) = task.restart.take() {
                    restarts.push_back(restart);
                }
            }
        }
        for (project, command) in restarts {
            if let Err(error) = self.start(&project, &command)
                && let Some(task) = self.tasks.get_mut(&(project.id, command.id))
            {
                task.status = TaskStatus::Failed(error.to_string());
            }
        }
    }
}

fn private_log(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    mut log: File,
    sender: SyncSender<OutputEvent>,
    stderr: bool,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        let mut parser = vte::Parser::new();
        let mut display = DisplayOutput::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    if let Err(error) = log.write_all(&buffer[..size]) {
                        let _ = sender.send(OutputEvent::Error(format!(
                            "Could not write project log: {error}"
                        )));
                        break;
                    }
                    parser.advance(&mut display, &buffer[..size]);
                    if !display.text.is_empty()
                        && sender
                            .send(OutputEvent::Text(std::mem::take(&mut display.text), stderr))
                            .is_err()
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = sender.send(OutputEvent::Error(error.to_string()));
                    break;
                }
            }
        }
        let _ = sender.send(OutputEvent::Closed);
    });
}

#[derive(Default)]
struct DisplayOutput {
    text: String,
}

impl vte::Perform for DisplayOutput {
    fn print(&mut self, character: char) {
        self.text.push(character);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.text.push('\n'),
            b'\t' => self.text.push_str("    "),
            _ => {}
        }
    }
}

fn signal_group(group: u32, signal: i32) -> io::Result<()> {
    let group = i32::try_from(group).map_err(|_| io::Error::other("Invalid process group"))?;
    if group <= 1 {
        return Err(io::Error::other("Refusing invalid process group"));
    }
    // Groups are created by CommandExt::process_group for our own child processes.
    let result = unsafe { libc::kill(-group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn group_has_live_processes(group: u32) -> bool {
    let Ok(group) = i32::try_from(group) else {
        return false;
    };
    if group <= 1 {
        return false;
    }
    // Signal zero checks existence without sending a signal.
    if unsafe { libc::kill(-group, 0) } != 0 {
        return io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    }
    // Orphan zombies may wait for init to reap them (notably in containers).
    // They cannot serve ports or hold pipes and must not block a restart.
    let Ok(snapshot) = Command::new("ps").args(["-axo", "pgid=,stat="]).output() else {
        return true;
    };
    if !snapshot.status.success() {
        return true;
    }
    String::from_utf8_lossy(&snapshot.stdout)
        .lines()
        .any(|line| {
            let mut fields = line.split_whitespace();
            fields.next().and_then(|value| value.parse::<i32>().ok()) == Some(group)
                && fields.next().is_none_or(|state| !state.starts_with('Z'))
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    pub struct Fixture {
        pub root: PathBuf,
        pub project: ProjectDefinition,
    }

    impl Fixture {
        pub fn new(name: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "lazyterm-project-{name}-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("sub directory")).unwrap();
            Self {
                project: ProjectDefinition {
                    id: 17,
                    name: "Fixture project".into(),
                    base_path: root.clone(),
                    commands: Vec::new(),
                    ..Default::default()
                },
                root,
            }
        }

        pub fn service(&self) -> ProjectService {
            ProjectService::new(Some(self.root.join("logs")))
        }

        pub fn command(&self, command: &str) -> ProjectCommand {
            ProjectCommand {
                id: 29,
                name: "Fixture command".into(),
                command: command.into(),
                working_directory: None,
                ..Default::default()
            }
        }

        pub fn listener_command(&self, ignore_term: bool) -> ProjectCommand {
            self.command(&format!(
                "trap 'wait; exit 0' TERM; {} & wait",
                self.listener_invocation(ignore_term),
            ))
        }

        fn listener_invocation(&self, ignore_term: bool) -> String {
            let exe = std::env::current_exe().unwrap();
            format!(
                "LAZYTERM_PROJECT_FIXTURE={} LAZYTERM_PROJECT_IGNORE_TERM={} {} --ignored --exact projects::tests::disposable_listener --nocapture",
                shell_words::quote(self.root.to_str().unwrap()),
                if ignore_term { "1" } else { "0" },
                shell_words::quote(exe.to_str().unwrap()),
            )
        }

        pub fn ready(&self) -> Option<(u32, u16)> {
            let text = fs::read_to_string(self.root.join("ready")).ok()?;
            let (pid, port) = text.split_once(' ')?;
            Some((pid.parse().ok()?, port.trim().parse().ok()?))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    pub fn until(service: &mut ProjectService, mut condition: impl FnMut(&ProjectService) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            service.poll();
            if condition(service) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Project condition timed out: {:?}",
                service
                    .tasks
                    .values()
                    .map(|task| (&task.status, &task.output))
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "disposable subprocess helper; invoked by project integration tests"]
    fn disposable_listener() {
        let Some(root) = std::env::var_os("LAZYTERM_PROJECT_FIXTURE") else {
            return;
        };
        if std::env::var("LAZYTERM_PROJECT_IGNORE_TERM").as_deref() == Ok("1") {
            // The fixture deliberately ignores TERM to exercise bounded escalation.
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let root = PathBuf::from(root);
        fs::write(
            root.join("cwd"),
            std::env::current_dir().unwrap().to_str().unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("ready.tmp"),
            format!(
                "{} {}",
                std::process::id(),
                listener.local_addr().unwrap().port()
            ),
        )
        .unwrap();
        fs::rename(root.join("ready.tmp"), root.join("ready")).unwrap();
        println!("disposable server ready");
        for connection in listener.incoming() {
            drop(connection.unwrap());
        }
    }

    #[test]
    fn commands_capture_cwd_unicode_both_streams_exit_and_full_logs() {
        let fixture = Fixture::new("output");
        let mut service = fixture.service();
        let mut command = fixture.command("pwd; printf '\\344\\275'; sleep 0.05; printf '\\240\\n'; printf 'problem\\n' >&2; i=0; while [ \"$i\" -lt 5000 ]; do printf 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789--\\n'; i=$((i + 1)); done; exit 7");
        command.working_directory = Some("sub directory".into());
        service.start(&fixture.project, &command).unwrap();
        let key = (fixture.project.id, command.id);
        until(&mut service, |s| !s.active(key));
        let task = &service.tasks[&key];
        assert_eq!(task.status, TaskStatus::Exited(7));
        assert!(task.output.len() <= OUTPUT_LIMIT);
        let stdout = fs::read_to_string(&task.stdout_log).unwrap();
        assert!(
            stdout.starts_with(
                fixture
                    .root
                    .join("sub directory")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
            )
        );
        assert!(stdout.contains("\u{4f60}"));
        assert!(stdout.len() > OUTPUT_LIMIT);
        assert_eq!(fs::read_to_string(&task.stderr_log).unwrap(), "problem\n");
        assert!(service.owners().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&task.stdout_log).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn ansi_parser_preserves_split_unicode_and_ignores_terminal_controls() {
        let mut parser = vte::Parser::new();
        let mut output = DisplayOutput::default();
        parser.advance(&mut output, b"\x1b[3");
        parser.advance(&mut output, b"2mgreen\x1b[0m \xe4\xbd");
        parser.advance(&mut output, b"\xa0\tend\n\x1b]0;window title\x07");
        assert_eq!(output.text, "green \u{4f60}    end\n");
    }

    #[test]
    fn invalid_directory_and_duplicate_start_do_not_replace_a_running_task() {
        let fixture = Fixture::new("validation");
        let mut service = fixture.service();
        let mut command = fixture.command("sleep 30");
        command.working_directory = Some("missing".into());
        assert!(service.start(&fixture.project, &command).is_err());
        command.working_directory = None;
        service.start(&fixture.project, &command).unwrap();
        let key = (fixture.project.id, command.id);
        let owner = service.tasks[&key].owner.clone();
        assert!(service.start(&fixture.project, &command).is_err());
        assert_eq!(service.tasks[&key].owner, owner);
        service.stop(key).unwrap();
        until(&mut service, |s| !s.active(key));
        assert_eq!(service.tasks[&key].status, TaskStatus::Stopped);
        assert!(
            working_directory(
                &ProjectDefinition {
                    base_path: "relative".into(),
                    ..fixture.project.clone()
                },
                &command
            )
            .is_err()
        );
    }

    #[test]
    fn restart_and_stop_clean_child_listeners_in_the_owned_group() {
        let fixture = Fixture::new("restart");
        let mut service = fixture.service();
        let command = fixture.listener_command(true);
        let key = (fixture.project.id, command.id);
        service.start(&fixture.project, &command).unwrap();
        until(&mut service, |_| fixture.ready().is_some());
        let (old_pid, old_port) = fixture.ready().unwrap();
        let old = service.tasks[&key].owner.clone();
        assert_ne!(old_pid, old.process_group);
        assert!(TcpStream::connect(("127.0.0.1", old_port)).is_ok());
        service.restart(&fixture.project, &command).unwrap();
        until(&mut service, |s| {
            s.tasks[&key].owner.run_id != old.run_id
                && fixture.ready().is_some_and(|(pid, _)| pid != old_pid)
        });
        assert!(TcpStream::connect(("127.0.0.1", old_port)).is_err());
        let task = &service.tasks[&key];
        assert_ne!(task.owner.process_group, old.process_group);
        let (_, port) = fixture.ready().unwrap();
        service.stop(key).unwrap();
        until(&mut service, |s| !s.active(key));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        assert_eq!(service.tasks[&key].status, TaskStatus::Stopped);
    }

    #[test]
    fn concurrent_commands_and_drop_cleanup_are_independent() {
        let fixture = Fixture::new("drop");
        let mut service = fixture.service();
        let server = fixture.listener_command(false);
        let mut one_shot = fixture.command("printf 'one shot\\n'");
        one_shot.id += 1;
        service.start(&fixture.project, &server).unwrap();
        service.start(&fixture.project, &one_shot).unwrap();
        until(&mut service, |s| {
            fixture.ready().is_some() && !s.active((fixture.project.id, one_shot.id))
        });
        assert_eq!(
            service.tasks[&(fixture.project.id, one_shot.id)].status,
            TaskStatus::Exited(0)
        );
        let (_, port) = fixture.ready().unwrap();
        drop(service);
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(
                Instant::now() < deadline,
                "Project drop left a server alive"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_finished_shell_cannot_leave_its_background_server_running() {
        let fixture = Fixture::new("background");
        let mut service = fixture.service();
        let command = fixture.command(&format!(
            "{} & while [ ! -f ready ]; do sleep 0.01; done",
            fixture.listener_invocation(false),
        ));
        let key = (fixture.project.id, command.id);
        service.start(&fixture.project, &command).unwrap();
        until(&mut service, |s| !s.active(key));
        let (_, port) = fixture.ready().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        assert_eq!(service.tasks[&key].status, TaskStatus::Exited(0));
    }
}
