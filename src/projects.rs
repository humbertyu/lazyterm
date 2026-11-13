use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

pub type EntityId = uuid::Uuid;
pub type TaskKey = (EntityId, EntityId);
const OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectDefinition {
    pub id: EntityId,
    pub name: String,
    pub base_path: PathBuf,
    #[serde(default)]
    pub commands: Vec<ProjectCommand>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub target: ProjectTarget,
    #[serde(default)]
    pub links: Vec<ProjectLink>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectLink {
    pub id: EntityId,
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
    #[serde(default)]
    pub password_storage: PasswordStorage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PasswordStorage {
    #[default]
    Keychain,
    Config,
}

impl PasswordStorage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Keychain => "Keychain",
            Self::Config => "Config file (plaintext)",
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "Keychain" => Some(Self::Keychain),
            "Config file (plaintext)" => Some(Self::Config),
            _ => None,
        }
    }
}

pub fn validate_link_url(value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) || value.chars().any(char::is_whitespace) {
        return Err("URL cannot contain whitespace; encode spaces as %20.".into());
    }
    let parsed = url::Url::parse(value).map_err(|error| format!("Invalid URL: {error}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Keep credentials in the account fields, not in the URL.".into());
    }
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !value
            .split_once("://")
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case(parsed.scheme()))
    {
        return Err("Use a complete http:// or https:// URL with a host.".into());
    }
    Ok(())
}

pub fn open_link_async(value: String) -> Receiver<Result<(), String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = validate_link_url(&value)
            .and_then(|()| webbrowser::open(&value).map_err(|error| error.to_string()));
        let _ = sender.send(result);
    });
    receiver
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProjectLifecycle {
    #[default]
    App,
    Persistent,
}

impl ProjectLifecycle {
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "Stop on exit",
            Self::Persistent => "Keep running",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectCommand {
    pub id: EntityId,
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
    #[serde(default)]
    pub lifecycle: ProjectLifecycle,
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
            "Use remote_working_directory for Host targets.",
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

pub fn remote_working_directory(
    project: &ProjectDefinition,
    command: &ProjectCommand,
) -> io::Result<PathBuf> {
    if !matches!(execution_target(project, command), ProjectTarget::Host(_)) {
        return Err(io::Error::other("Project target is not a Host."));
    }
    let base = project.base_path.to_string_lossy();
    if !(base.starts_with('/') || base.starts_with("~/")) {
        return Err(io::Error::other(
            "Remote Project base path must be absolute or start with ~/",
        ));
    }
    Ok(match &command.working_directory {
        Some(path) if path.is_absolute() || path.starts_with("~/") => path.clone(),
        Some(path) => project.base_path.join(path),
        None => project.base_path.clone(),
    })
}

fn remote_shell_command(directory: &Path, command: &str) -> String {
    format!(
        "cd -- {} && exec sh -c {}",
        shell_quote(&directory.to_string_lossy()),
        shell_quote(command)
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn expand_home(path: &Path) -> io::Result<PathBuf> {
    if let Ok(suffix) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
        Ok(PathBuf::from(home).join(suffix))
    } else {
        Ok(path.to_path_buf())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectOwner {
    pub key: TaskKey,
    pub run_id: u64,
    pub process_group: u32,
    pub project_name: String,
    pub command_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    pub lifecycle: ProjectLifecycle,
    persistent: Option<crate::project_runner::Handle>,
    child: Option<crate::process_guard::ManagedChild>,
    finished: Option<ExitStatus>,
    receiver: Receiver<OutputEvent>,
    closed_readers: usize,
    stop_deadline: Option<Instant>,
    user_stopped: bool,
    restart: Option<(
        ProjectDefinition,
        ProjectCommand,
        crate::credentials::HostAuth,
    )>,
}

impl ProjectTask {
    pub fn active(&self) -> bool {
        self.child.is_some()
            || self
                .persistent
                .as_ref()
                .is_some_and(|handle| handle.active())
    }

    fn request_stop(&mut self) -> io::Result<()> {
        if !self.active() || self.stop_deadline.is_some() {
            return Ok(());
        }
        if let Some(handle) = &mut self.persistent {
            handle.stop()?;
            self.status = handle.snapshot.status.clone();
            return Ok(());
        }
        if let Some(child) = self.child.as_mut() {
            child.signal_group(libc::SIGTERM)?;
        }
        self.user_stopped = true;
        self.status = TaskStatus::Stopping;
        self.stop_deadline = Some(Instant::now() + Duration::from_secs(2));
        Ok(())
    }
}

impl Drop for ProjectTask {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.signal_group(libc::SIGKILL);
            let _ = child.kill();
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
    restored_at: Option<Instant>,
}

impl ProjectService {
    pub fn new(log_root: Option<PathBuf>) -> Self {
        Self {
            tasks: BTreeMap::new(),
            log_root,
            next_run: 1,
            restored_at: None,
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

    pub(crate) fn set_next_run(&mut self, run_id: u64) {
        self.next_run = run_id;
    }

    fn recover_task(&mut self, key: TaskKey, lifecycle: ProjectLifecycle) -> io::Result<()> {
        if self
            .tasks
            .get(&key)
            .is_some_and(|task| task.active() && task.persistent.is_none())
        {
            return Ok(());
        }
        let Some(root) = &self.log_root else {
            return Ok(());
        };
        if let Some(handle) = crate::project_runner::recover(root, key)? {
            let replace = self.tasks.get(&key).is_none_or(|task| {
                task.owner != handle.snapshot.owner || task.active() != handle.active()
            });
            if replace {
                self.attach_runner(handle, lifecycle);
            }
        }
        Ok(())
    }

    pub fn restore(&mut self, definitions: &[ProjectDefinition]) {
        if self
            .restored_at
            .is_some_and(|time| time.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.restored_at = Some(Instant::now());
        for project in definitions {
            for command in &project.commands {
                if *execution_target(project, command) == ProjectTarget::Local {
                    let _ = self.recover_task((project.id, command.id), command.lifecycle);
                }
            }
        }
    }

    pub fn forget_persistent_record(&self, key: TaskKey) -> io::Result<()> {
        if self.active(key) {
            return Err(io::Error::other(
                "Stop the background command before deleting its record.",
            ));
        }
        if let Some(root) = &self.log_root {
            crate::project_runner::forget(root, key)?;
        }
        Ok(())
    }

    fn attach_runner(
        &mut self,
        handle: crate::project_runner::Handle,
        lifecycle: ProjectLifecycle,
    ) {
        let snapshot = &handle.snapshot;
        self.tasks.insert(
            snapshot.owner.key,
            ProjectTask {
                owner: snapshot.owner.clone(),
                directory: snapshot.directory.clone(),
                command: snapshot.command.clone(),
                status: snapshot.status.clone(),
                output: snapshot.output.clone(),
                stdout_log: snapshot.stdout_log.clone(),
                stderr_log: snapshot.stderr_log.clone(),
                lifecycle,
                persistent: Some(handle),
                child: None,
                finished: None,
                receiver: mpsc::channel().1,
                closed_readers: 2,
                stop_deadline: None,
                user_stopped: false,
                restart: None,
            },
        );
    }

    pub fn start(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
    ) -> io::Result<()> {
        self.start_with_auth(project, command, &crate::credentials::HostAuth::default())
    }

    pub fn start_with_auth(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
        auth: &crate::credentials::HostAuth,
    ) -> io::Result<()> {
        let target = execution_target(project, command);
        if *target == ProjectTarget::Local {
            working_directory(project, command)?;
        } else if command.lifecycle == ProjectLifecycle::Persistent {
            return Err(io::Error::other(
                "Persistent lifecycle is currently available only for Local Projects.",
            ));
        } else {
            remote_working_directory(project, command)?;
        }
        let key = (project.id, command.id);
        self.recover_task(key, command.lifecycle)?;
        if self.active(key) {
            return Err(io::Error::other("Command is already running"));
        }
        if command.lifecycle == ProjectLifecycle::Persistent {
            if command.command.trim().is_empty() {
                return Err(io::Error::other("Command is empty"));
            }
            let root = self
                .log_root
                .as_ref()
                .ok_or_else(|| io::Error::other("Project log directory is unavailable"))?;
            let handle = crate::project_runner::start(root, project, command)?;
            self.attach_runner(handle, command.lifecycle);
            return Ok(());
        }
        self.start_app_with_auth(project, command, auth)
    }

    pub(crate) fn start_app(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
    ) -> io::Result<()> {
        self.start_app_with_auth(project, command, &crate::credentials::HostAuth::default())
    }

    pub(crate) fn start_app_with_auth(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
        auth: &crate::credentials::HostAuth,
    ) -> io::Result<()> {
        let key = (project.id, command.id);
        if self.active(key) {
            return Err(io::Error::other("Command is already running"));
        }
        if command.command.trim().is_empty() {
            return Err(io::Error::other("Command is empty"));
        }
        let target = execution_target(project, command).clone();
        let directory = if target == ProjectTarget::Local {
            working_directory(project, command)?
        } else {
            remote_working_directory(project, command)?
        };
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
        let mut authentication_trace = None;
        let mut process = if let ProjectTarget::Host(alias) = &target {
            let mut process = Command::new("ssh");
            process.args(["-o", "RequestTTY=no", "-o", "ConnectTimeout=10"]);
            authentication_trace =
                crate::credentials::configure_host_auth(&mut process, auth, alias)?;
            process
                .arg(alias)
                .arg(remote_shell_command(&directory, &command.command));
            process
        } else {
            let mut process = Command::new("sh");
            process
                .arg("-c")
                .arg(&command.command)
                .current_dir(&directory);
            process
        };
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::process_guard::ManagedChild::spawn(&mut process)?;
        let process_group = child.process_group();
        let (sender, receiver) = mpsc::sync_channel(128);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        spawn_reader(stdout, stdout_file, sender.clone(), false);
        spawn_reader(
            crate::authentication::Reader::new(stderr, authentication_trace),
            stderr_file,
            sender,
            true,
        );
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
                lifecycle: ProjectLifecycle::App,
                persistent: None,
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
        self.restart_with_auth(project, command, &crate::credentials::HostAuth::default())
    }

    pub fn restart_with_auth(
        &mut self,
        project: &ProjectDefinition,
        command: &ProjectCommand,
        auth: &crate::credentials::HostAuth,
    ) -> io::Result<()> {
        if *execution_target(project, command) == ProjectTarget::Local {
            working_directory(project, command)?;
        } else {
            remote_working_directory(project, command)?;
        }
        let key = (project.id, command.id);
        if let Some(task) = self.tasks.get_mut(&key)
            && task.active()
        {
            task.request_stop()?;
            task.restart = Some((project.clone(), command.clone(), auth.clone()));
            Ok(())
        } else {
            self.start_with_auth(project, command, auth)
        }
    }

    pub fn poll(&mut self) {
        let mut restarts = VecDeque::new();
        for task in self
            .tasks
            .values_mut()
            .filter(|task| task.persistent.is_some())
        {
            let handle = task.persistent.as_mut().expect("persistent");
            match handle.refresh() {
                Ok(()) => {
                    if task.owner != handle.snapshot.owner {
                        task.owner = handle.snapshot.owner.clone();
                        task.directory.clone_from(&handle.snapshot.directory);
                        task.command.clone_from(&handle.snapshot.command);
                        task.stdout_log.clone_from(&handle.snapshot.stdout_log);
                        task.stderr_log.clone_from(&handle.snapshot.stderr_log);
                        task.restart = None;
                    }
                    task.status = handle.snapshot.status.clone();
                    task.output.clone_from(&handle.snapshot.output);
                }
                Err(error) => {
                    task.status =
                        TaskStatus::Failed(format!("Background runner unavailable: {error}"));
                }
            }
            if !handle.active()
                && let Some(restart) = task.restart.take()
            {
                restarts.push_back(restart);
            }
        }
        for task in self
            .tasks
            .values_mut()
            .filter(|task| task.persistent.is_none() && task.active())
        {
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
                if let Some(child) = task.child.as_mut() {
                    let _ = child.signal_group(libc::SIGTERM);
                }
                task.stop_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            if let Some(deadline) = task.stop_deadline
                && Instant::now() >= deadline
                && let Some(child) = task.child.as_mut()
            {
                let _ = child.signal_group(libc::SIGKILL);
            }
            if task.finished.is_some()
                && task.closed_readers >= 2
                && task
                    .child
                    .as_ref()
                    .is_some_and(|child| !child.group_has_live_processes())
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
        for (project, command, auth) in restarts {
            if let Err(error) = self.start_with_auth(&project, &command, &auth)
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

pub(crate) fn group_has_live_processes(group: u32) -> bool {
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

    #[test]
    fn web_links_accept_complete_web_urls_and_reject_other_targets() {
        for value in [
            "http://localhost:3000/",
            "https://example.com/admin?tab=users#list",
            "http://127.0.0.1:4444/",
            "http://[::1]:3000/",
            "HTTPS://example.com/docs",
            "https://example.com/a%20b",
        ] {
            assert!(validate_link_url(value).is_ok(), "{value}");
        }
        for value in [
            "",
            "localhost:3000",
            "http:example.com",
            "https://",
            "https://example.com:invalid/",
            "file:///tmp/test.html",
            "javascript:alert(1)",
            "ssh://example.com",
            "--help",
            "https://example.com/a b",
            "https://example.com/\n",
            "https://user:secret@example.com/",
            "https://user@example.com/",
        ] {
            assert!(validate_link_url(value).is_err(), "{value}");
        }
    }

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
                    id: uuid::Uuid::from_u128(17),
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
                id: uuid::Uuid::from_u128(29),
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
            println!("connection observed");
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
    fn remote_projects_build_a_quoted_remote_working_directory() {
        let project = ProjectDefinition {
            id: uuid::Uuid::from_u128(7),
            name: "Remote".into(),
            base_path: "~/apps/demo".into(),
            target: ProjectTarget::Host("fixture".into()),
            ..Default::default()
        };
        let command = ProjectCommand {
            working_directory: Some("service's api".into()),
            command: "printf 'remote output'".into(),
            ..Default::default()
        };
        assert_eq!(
            remote_working_directory(&project, &command).unwrap(),
            PathBuf::from("~/apps/demo/service's api")
        );
        assert_eq!(
            remote_shell_command(
                &PathBuf::from("~/apps/demo/service's api"),
                "printf 'remote output'"
            ),
            "cd -- '~/apps/demo/service'\\''s api' && exec sh -c 'printf '\\''remote output'\\'''"
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
        one_shot.id = uuid::Uuid::new_v4();
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

    pub(crate) struct PersistentCleanup {
        root: PathBuf,
        key: TaskKey,
    }

    impl PersistentCleanup {
        pub fn new(fixture: &Fixture, key: TaskKey) -> Self {
            Self {
                root: fixture.root.join("logs"),
                key,
            }
        }
    }

    impl Drop for PersistentCleanup {
        fn drop(&mut self) {
            if let Ok(Some(mut handle)) = crate::project_runner::recover(&self.root, self.key) {
                let _ = handle.stop();
                let deadline = Instant::now() + Duration::from_secs(5);
                while handle.active() && Instant::now() < deadline {
                    let _ = handle.refresh();
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    #[test]
    fn persistent_servers_survive_drop_keep_logging_and_restore_stop_and_restart() {
        let mut fixture = Fixture::new("persistent");
        let mut command = fixture.listener_command(true);
        command.lifecycle = ProjectLifecycle::Persistent;
        fixture.project.commands.push(command.clone());
        let key = (fixture.project.id, command.id);
        let _cleanup = PersistentCleanup {
            root: fixture.root.join("logs"),
            key,
        };
        let mut service = fixture.service();
        service.start(&fixture.project, &command).unwrap();
        until(&mut service, |_| fixture.ready().is_some());
        let (original_pid, original_port) = fixture.ready().unwrap();
        let owner = service.tasks[&key].owner.clone();
        let stdout = service.tasks[&key].stdout_log.clone();
        drop(service);
        assert!(TcpStream::connect(("127.0.0.1", original_port)).is_ok());
        // There is no TUI reader now; the independent runner must keep draining logs.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fs::read_to_string(&stdout)
            .unwrap()
            .contains("connection observed")
        {
            assert!(
                Instant::now() < deadline,
                "Background logs stopped after TUI exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut restored = fixture.service();
        restored.restore(std::slice::from_ref(&fixture.project));
        assert!(restored.active(key));
        assert_eq!(restored.owners().as_slice(), std::slice::from_ref(&owner));
        until(&mut restored, |s| {
            s.tasks[&key].output.contains("connection observed")
        });
        assert!(restored.start(&fixture.project, &command).is_err());
        let mut second_window = fixture.service();
        assert!(second_window.start(&fixture.project, &command).is_err());
        drop(second_window);
        assert!(TcpStream::connect(("127.0.0.1", original_port)).is_ok());
        let mut observer = fixture.service();
        observer.restore(std::slice::from_ref(&fixture.project));
        restored.restart(&fixture.project, &command).unwrap();
        until(&mut restored, |s| {
            s.tasks[&key].owner.run_id != owner.run_id
                && fixture.ready().is_some_and(|(pid, _)| pid != original_pid)
        });
        assert!(TcpStream::connect(("127.0.0.1", original_port)).is_err());
        let (_, new_port) = fixture.ready().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", new_port)).is_ok());
        let deadline = Instant::now() + Duration::from_secs(10);
        while observer.tasks[&key].owner.run_id != restored.tasks[&key].owner.run_id {
            observer.restore(std::slice::from_ref(&fixture.project));
            observer.poll();
            assert!(
                Instant::now() < deadline,
                "Other window did not discover the replacement run"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(observer.active(key));
        drop(observer);
        drop(restored);
        let mut reopened = fixture.service();
        reopened.restore(std::slice::from_ref(&fixture.project));
        assert!(reopened.active(key));
        reopened.stop(key).unwrap();
        until(&mut reopened, |s| !s.active(key));
        assert_eq!(reopened.tasks[&key].status, TaskStatus::Stopped);
        assert!(TcpStream::connect(("127.0.0.1", new_port)).is_err());
        drop(reopened);
        let mut history = fixture.service();
        history.restore(std::slice::from_ref(&fixture.project));
        assert!(!history.active(key));
        assert_eq!(history.tasks[&key].status, TaskStatus::Stopped);
        assert!(
            history.tasks[&key]
                .output
                .contains("disposable server ready")
        );
    }

    #[test]
    fn persistent_tasks_preserve_output_exit_status_and_private_records() {
        let mut fixture = Fixture::new("persistent-output");
        let mut command = fixture.command("printf '\\344\\275\\240\\n'; printf 'problem\\n' >&2; i=0; while [ \"$i\" -lt 5000 ]; do printf 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789--\\n'; i=$((i + 1)); done; exit 7");
        command.lifecycle = ProjectLifecycle::Persistent;
        fixture.project.commands.push(command.clone());
        let key = (fixture.project.id, command.id);
        let _cleanup = PersistentCleanup {
            root: fixture.root.join("logs"),
            key,
        };
        let mut service = fixture.service();
        service.start(&fixture.project, &command).unwrap();
        drop(service);
        let mut restored = fixture.service();
        restored.restore(std::slice::from_ref(&fixture.project));
        until(&mut restored, |s| !s.active(key));
        let task = &restored.tasks[&key];
        assert_eq!(task.status, TaskStatus::Exited(7));
        assert!(task.output.len() <= OUTPUT_LIMIT);
        let stdout = fs::read_to_string(&task.stdout_log).unwrap();
        assert!(stdout.starts_with("\u{4f60}\n"));
        assert!(stdout.len() > OUTPUT_LIMIT);
        assert_eq!(fs::read_to_string(&task.stderr_log).unwrap(), "problem\n");
        use std::os::unix::fs::PermissionsExt;
        let record = fixture
            .root
            .join(format!("logs/persistent-{}-{}.json", key.0, key.1));
        assert_eq!(
            fs::metadata(record).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(restored);
        let mut history = fixture.service();
        history.restore(std::slice::from_ref(&fixture.project));
        assert_eq!(history.tasks[&key].status, TaskStatus::Exited(7));
        assert!(!history.tasks[&key].output.is_empty());
        let stdout_log = history.tasks[&key].stdout_log.clone();
        history.forget_persistent_record(key).unwrap();
        let mut removed = fixture.service();
        removed.restore(std::slice::from_ref(&fixture.project));
        assert!(!removed.tasks.contains_key(&key));
        assert!(
            stdout_log.is_file(),
            "Deleting a definition must preserve raw logs"
        );
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
