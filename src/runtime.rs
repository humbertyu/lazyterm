use crate::projects::ProjectOwner;
use std::{
    collections::{BTreeMap, HashMap},
    process::{Command, Stdio},
    sync::mpsc::Sender,
};

const INSPECT_SCRIPT: &str = r#"
if command -v lsof >/dev/null 2>&1; then
  lsof -nP -iTCP -sTCP:LISTEN -F pcun
elif command -v ss >/dev/null 2>&1; then
  ss -ltnpH
else
  echo "Neither lsof nor ss is available" >&2
  exit 127
fi
"#;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeTarget {
    Local,
    Host(String),
}

#[derive(Default)]
pub(crate) struct RuntimeView {
    pub entries: Vec<RuntimeEntry>,
    pub filter: RuntimeFilter,
    pub selected: usize,
    pub offset: usize,
    pub error: Option<String>,
}

impl RuntimeTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Local => "Local".to_string(),
            Self::Host(alias) => alias.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEntry {
    pub protocol: String,
    pub address: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub process: String,
    pub user: String,
    pub command: String,
    pub working_directory: Option<String>,
    pub owned_by_current_user: bool,
    pub kind: RuntimeProcessKind,
    pub origin: RuntimeOrigin,
    pub origin_reason: String,
    pub project: Option<ProjectOwner>,
    pub external_project: Option<crate::project_monitor::ExternalRun>,
    pub tunnel: Option<uuid::Uuid>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RuntimeOrigin {
    Project,
    ExternalProject,
    Tunnel,
    Agent,
    CommandLine,
    Desktop,
    #[default]
    Unknown,
}

impl RuntimeOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "PROJECT",
            Self::ExternalProject => "PROJECT / EXT",
            Self::Tunnel => "TUNNEL",
            Self::Agent => "AGENT",
            Self::CommandLine => "CLI",
            Self::Desktop => "DESKTOP",
            Self::Unknown => "UNKNOWN",
        }
    }
}

struct ProcessObservation {
    parent: u32,
    group: u32,
    tty: String,
    command: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RuntimeFilter {
    #[default]
    Mine,
    Applications,
    System,
    All,
}

impl RuntimeFilter {
    pub const ALL: [Self; 4] = [Self::Mine, Self::Applications, Self::System, Self::All];

    pub fn title(self) -> &'static str {
        match self {
            Self::Mine => "Mine",
            Self::Applications => "Apps",
            Self::System => "System",
            Self::All => "All",
        }
    }

    pub fn matches(self, entry: &RuntimeEntry) -> bool {
        match self {
            Self::Mine => entry.owned_by_current_user && entry.kind != RuntimeProcessKind::System,
            Self::Applications => entry.kind == RuntimeProcessKind::Application,
            Self::System => entry.kind == RuntimeProcessKind::System,
            Self::All => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RuntimeProcessKind {
    Application,
    Unknown,
    System,
}

impl RuntimeProcessKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Application => "APP",
            Self::Unknown => "UNKNOWN",
            Self::System => "SYSTEM",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Application => 0,
            Self::Unknown => 1,
            Self::System => 2,
        }
    }
}

#[derive(Debug)]
pub enum RuntimeEvent {
    Listed {
        target: RuntimeTarget,
        request: u64,
        entries: Vec<RuntimeEntry>,
    },
    Failed {
        target: RuntimeTarget,
        request: u64,
        message: String,
    },
    ActionCompleted {
        target: RuntimeTarget,
        message: String,
    },
    ActionFailed {
        target: RuntimeTarget,
        message: String,
    },
}

pub fn inspect_async(
    sender: Sender<RuntimeEvent>,
    target: RuntimeTarget,
    request: u64,
    owners: Vec<ProjectOwner>,
    tunnels: Vec<crate::metadata::TunnelDefinition>,
) {
    std::thread::spawn(move || {
        let result = inspect(&target, &owners, &tunnels);
        let event = match result {
            Ok(entries) => RuntimeEvent::Listed {
                target,
                request,
                entries,
            },
            Err(message) => RuntimeEvent::Failed {
                target,
                request,
                message,
            },
        };
        let _ = sender.send(event);
    });
}

pub fn terminate_async(
    sender: Sender<RuntimeEvent>,
    target: RuntimeTarget,
    pid: u32,
    process: String,
) {
    std::thread::spawn(move || {
        let result = terminate(&target, pid);
        let event = match result {
            Ok(()) => RuntimeEvent::ActionCompleted {
                target,
                message: format!("Sent TERM to {process} ({pid})."),
            },
            Err(message) => RuntimeEvent::ActionFailed { target, message },
        };
        let _ = sender.send(event);
    });
}

pub fn kill_async(sender: Sender<RuntimeEvent>, target: RuntimeTarget, pid: u32, process: String) {
    std::thread::spawn(move || {
        let result = signal_process(&target, pid, "KILL");
        let event = match result {
            Ok(()) => RuntimeEvent::ActionCompleted {
                target,
                message: format!("Sent KILL to {process} ({pid})."),
            },
            Err(message) => RuntimeEvent::ActionFailed { target, message },
        };
        let _ = sender.send(event);
    });
}

pub(crate) fn inspect(
    target: &RuntimeTarget,
    owners: &[ProjectOwner],
    tunnels: &[crate::metadata::TunnelDefinition],
) -> Result<Vec<RuntimeEntry>, String> {
    let mut entries = inspect_listeners(target)?;
    enrich_process_details(target, &mut entries);
    let observations = process_observations(target);
    for entry in &mut entries {
        entry.kind = classify_process(entry);
        (entry.origin, entry.origin_reason) = classify_origin(entry.pid, &observations);
        entry.project = project_owner(target, entry.pid, &observations, owners);
        if let Some(owner) = &entry.project {
            entry.origin = RuntimeOrigin::Project;
            entry.origin_reason = format!(
                "Managed command: {} / {}",
                owner.project_name, owner.command_name
            );
            entry.kind = RuntimeProcessKind::Application;
            entry.owned_by_current_user = true;
        }
        if *target == RuntimeTarget::Local {
            entry.tunnel = tunnels
                .iter()
                .find(|tunnel| {
                    matches!(
                        tunnel.kind,
                        crate::metadata::TunnelKind::Local | crate::metadata::TunnelKind::Dynamic
                    ) && tunnel.local_port == entry.port
                })
                .map(|tunnel| tunnel.id);
            if entry.tunnel.is_some() && entry.project.is_none() {
                entry.origin = RuntimeOrigin::Tunnel;
                entry.origin_reason = "Managed by a LazyTerm tunnel".into();
            }
        }
    }
    entries.sort_by_key(|entry| {
        (
            entry.kind.rank(),
            match entry.origin {
                RuntimeOrigin::Project | RuntimeOrigin::ExternalProject | RuntimeOrigin::Tunnel => {
                    0
                }
                RuntimeOrigin::Agent | RuntimeOrigin::CommandLine => 1,
                RuntimeOrigin::Unknown => 2,
                RuntimeOrigin::Desktop => 3,
            },
            entry.port,
            entry.address.clone(),
            entry.process.clone(),
        )
    });
    Ok(entries)
}

pub(crate) fn inspect_listeners(target: &RuntimeTarget) -> Result<Vec<RuntimeEntry>, String> {
    inspect_listeners_with_config(target, None)
}

#[cfg(test)]
pub(crate) fn inspect_listeners_with_test_config(
    target: &RuntimeTarget,
    config_file: &std::path::Path,
) -> Result<Vec<RuntimeEntry>, String> {
    inspect_listeners_with_config(target, Some(config_file))
}

fn inspect_listeners_with_config(
    target: &RuntimeTarget,
    config_file: Option<&std::path::Path>,
) -> Result<Vec<RuntimeEntry>, String> {
    let output = match target {
        RuntimeTarget::Local => Command::new("sh")
            .args(["-c", INSPECT_SCRIPT])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("Could not start local port inspection: {error}"))?,
        RuntimeTarget::Host(alias) => {
            let mut command = Command::new("ssh");
            command.args([
                "-o",
                "BatchMode=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "ConnectTimeout=10",
            ]);
            if let Some(config_file) = config_file {
                command.arg("-F").arg(config_file);
            }
            command
                .arg(alias)
                .arg(INSPECT_SCRIPT)
                .stdin(Stdio::null())
                .output()
                .map_err(|error| format!("Could not start SSH inspection: {error}"))?
        }
    };
    if !(output.status.success()
        || (output.status.code() == Some(1)
            && output.stdout.is_empty()
            && output.stderr.is_empty()))
    {
        let error = crate::diagnostics::redact_sensitive_text(
            String::from_utf8_lossy(&output.stderr).trim(),
        );
        return Err(if error.is_empty() {
            format!(
                "Port inspection exited with {}.",
                exit_label(output.status.code())
            )
        } else {
            error
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries = if stdout.lines().any(|line| line.starts_with('p')) {
        parse_lsof(&stdout)
    } else {
        parse_ss(&stdout)
    };
    Ok(entries)
}

fn project_owner(
    target: &RuntimeTarget,
    pid: Option<u32>,
    observations: &HashMap<u32, ProcessObservation>,
    owners: &[ProjectOwner],
) -> Option<ProjectOwner> {
    if *target != RuntimeTarget::Local {
        return None;
    }
    pid.and_then(|pid| observations.get(&pid))
        .and_then(|process| {
            owners
                .iter()
                .find(|owner| owner.process_group == process.group)
        })
        .cloned()
}

fn process_observations(target: &RuntimeTarget) -> HashMap<u32, ProcessObservation> {
    let output = match target {
        RuntimeTarget::Local => Command::new("ps")
            .args(["-ww", "-axo", "pid=,ppid=,pgid=,tty=,command="])
            .output(),
        RuntimeTarget::Host(alias) => Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "ConnectTimeout=10",
            ])
            .arg(alias)
            .arg("ps -ww -axo pid=,ppid=,pgid=,tty=,command=")
            .stdin(Stdio::null())
            .output(),
    };
    let Ok(output) = output else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    parse_process_observations(&String::from_utf8_lossy(&output.stdout))
}

fn parse_process_observations(text: &str) -> HashMap<u32, ProcessObservation> {
    text.lines()
        .filter_map(|line| {
            let mut remaining = line.trim();
            let mut take_field = || {
                let (field, rest) = remaining.split_once(char::is_whitespace)?;
                remaining = rest.trim_start();
                Some(field)
            };
            let pid = take_field()?.parse().ok()?;
            let parent = take_field()?.parse().ok()?;
            let group = take_field()?.parse().ok()?;
            let tty = take_field()?.to_string();
            Some((
                pid,
                ProcessObservation {
                    parent,
                    group,
                    tty,
                    command: remaining.to_string(),
                },
            ))
        })
        .collect()
}

fn classify_origin(
    pid: Option<u32>,
    observations: &HashMap<u32, ProcessObservation>,
) -> (RuntimeOrigin, String) {
    let mut current = pid.unwrap_or(0);
    let mut seen = std::collections::HashSet::new();
    let mut cli_evidence = None;
    let mut desktop_evidence = None;
    for _ in 0..32 {
        if current <= 1 || !seen.insert(current) {
            break;
        }
        let Some(process) = observations.get(&current) else {
            break;
        };
        let executable = process.command.split_whitespace().next().unwrap_or("");
        let name = executable
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_start_matches('-');
        // Agent ancestry takes precedence over its terminal or desktop launcher.
        if matches!(name, "codex" | "claude" | "aider" | "opencode")
            || executable.ends_with("/Codex.app/Contents/MacOS/Codex")
        {
            return (
                RuntimeOrigin::Agent,
                format!("Agent ancestor PID {current}: {name}"),
            );
        }
        if desktop_evidence.is_none() && is_app_bundle_command(&process.command) {
            desktop_evidence = Some(format!("App bundle on PID {current}"));
        }
        if cli_evidence.is_none() && desktop_evidence.is_none() {
            if !matches!(process.tty.as_str(), "?" | "??" | "-" | "") {
                cli_evidence = Some(format!("Terminal {} on PID {current}", process.tty));
            } else if matches!(name, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh") {
                cli_evidence = Some(format!("Shell ancestor PID {current}: {name}"));
            }
        }
        current = process.parent;
    }
    if let Some(reason) = cli_evidence {
        (RuntimeOrigin::CommandLine, reason)
    } else if let Some(reason) = desktop_evidence {
        (RuntimeOrigin::Desktop, reason)
    } else {
        (
            RuntimeOrigin::Unknown,
            "No terminal, shell, Agent, or app-bundle evidence in visible ancestry.".into(),
        )
    }
}

fn is_app_bundle_command(command: &str) -> bool {
    command
        .split_once(".app/Contents/")
        .is_some_and(|(prefix, _)| {
            prefix.starts_with('/') && !prefix.contains(" /") && !prefix.contains(" -")
        })
}

fn enrich_process_details(target: &RuntimeTarget, entries: &mut [RuntimeEntry]) {
    let mut details = HashMap::new();
    let current_uid = target_uid(target);
    for entry in entries {
        let Some(pid) = entry.pid else {
            continue;
        };
        let detail = details
            .entry(pid)
            .or_insert_with(|| process_details(target, pid));
        if let Some((uid, command, working_directory)) = detail {
            entry.owned_by_current_user = current_uid.as_ref() == Some(uid);
            entry.user.clone_from(uid);
            entry.command.clone_from(command);
            entry.working_directory.clone_from(working_directory);
        }
    }
}

fn target_uid(target: &RuntimeTarget) -> Option<String> {
    let output = match target {
        RuntimeTarget::Local => Command::new("id").arg("-u").output().ok()?,
        RuntimeTarget::Host(alias) => Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "ConnectTimeout=10",
            ])
            .arg(alias)
            .arg("id -u")
            .stdin(Stdio::null())
            .output()
            .ok()?,
    };
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn process_details(target: &RuntimeTarget, pid: u32) -> Option<(String, String, Option<String>)> {
    let command_output = match target {
        RuntimeTarget::Local => Command::new("ps")
            .args([
                "-ww",
                "-p",
                &pid.to_string(),
                "-o",
                "uid=",
                "-o",
                "command=",
            ])
            .output()
            .ok()?,
        RuntimeTarget::Host(alias) => Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "ConnectTimeout=10",
            ])
            .arg(alias)
            .args([
                "ps",
                "-ww",
                "-p",
                &pid.to_string(),
                "-o",
                "uid=",
                "-o",
                "command=",
            ])
            .stdin(Stdio::null())
            .output()
            .ok()?,
    };
    if !command_output.status.success() {
        return None;
    }
    let detail = String::from_utf8_lossy(&command_output.stdout)
        .trim()
        .to_string();
    let (uid, command) = detail.split_once(char::is_whitespace)?;
    let command = command.trim().to_string();
    if command.is_empty() {
        return None;
    }
    Some((
        uid.to_string(),
        command,
        process_working_directory(target, pid),
    ))
}

pub(crate) fn process_working_directory(target: &RuntimeTarget, pid: u32) -> Option<String> {
    let output = match target {
        RuntimeTarget::Local => Command::new("sh")
            .args([
                "-c",
                &format!(
                    "if command -v lsof >/dev/null 2>&1; then lsof -a -p {pid} -d cwd -Fn; elif [ -e /proc/{pid}/cwd ]; then readlink /proc/{pid}/cwd; fi"
                ),
            ])
            .output()
            .ok()?,
        RuntimeTarget::Host(alias) => Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "ConnectTimeout=10",
            ])
            .arg(alias)
            .arg(format!(
                "if command -v lsof >/dev/null 2>&1; then lsof -a -p {pid} -d cwd -Fn; elif [ -e /proc/{pid}/cwd ]; then readlink /proc/{pid}/cwd; fi"
            ))
            .stdin(Stdio::null())
            .output()
            .ok()?,
    };
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.strip_prefix('n')
                .map(str::to_string)
                .or_else(|| line.starts_with('/').then(|| line.to_string()))
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    Some(value)
}

fn classify_process(entry: &RuntimeEntry) -> RuntimeProcessKind {
    let user = entry.user.to_ascii_lowercase();
    let process = entry.process.to_ascii_lowercase();
    let command = entry.command.to_ascii_lowercase();
    let system_user = matches!(
        user.as_str(),
        "daemon" | "nobody" | "systemd-network" | "systemd-resolve"
    ) || user.starts_with('_');
    let system_process = [
        "launchd",
        "kernel_task",
        "systemd",
        "sshd",
        "mDNSResponder",
        "configd",
        "syslogd",
        "WindowServer",
        "systemd-resolve",
    ]
    .iter()
    .any(|name| process == name.to_ascii_lowercase());
    let system_path = ["/system/", "/usr/libexec/"]
        .iter()
        .any(|prefix| command.starts_with(prefix));
    if system_user || system_process || system_path {
        RuntimeProcessKind::System
    } else if entry.user.is_empty() || entry.process.is_empty() {
        RuntimeProcessKind::Unknown
    } else {
        RuntimeProcessKind::Application
    }
}

fn terminate(target: &RuntimeTarget, pid: u32) -> Result<(), String> {
    signal_process(target, pid, "TERM")
}

fn signal_process(target: &RuntimeTarget, pid: u32, signal: &str) -> Result<(), String> {
    let status = match target {
        RuntimeTarget::Local => Command::new("kill")
            .args([&format!("-{signal}"), &pid.to_string()])
            .status()
            .map_err(|error| format!("Could not send {signal} to {pid}: {error}"))?,
        RuntimeTarget::Host(alias) => Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "ConnectTimeout=10",
            ])
            .arg(alias)
            .args(["kill", &format!("-{signal}"), &pid.to_string()])
            .stdin(Stdio::null())
            .status()
            .map_err(|error| format!("Could not send {signal} to {pid}: {error}"))?,
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not send {signal} to process {pid} ({}).",
            exit_label(status.code())
        ))
    }
}

fn parse_lsof(output: &str) -> Vec<RuntimeEntry> {
    let mut current = BTreeMap::<char, String>::new();
    let mut entries = Vec::new();
    for line in output.lines().chain(std::iter::once("")) {
        let Some((field, value)) = line.split_at_checked(1) else {
            continue;
        };
        if field == "p" {
            current.clear();
        }
        if field == "n" {
            if let Some(entry) = lsof_entry(&current, value) {
                entries.push(entry);
            }
        } else if matches!(field.chars().next(), Some('p' | 'c' | 'u')) {
            current.insert(field.chars().next().unwrap(), value.to_string());
        }
        if line.is_empty() {
            current.clear();
        }
    }
    entries.sort_by_key(|entry| (entry.port, entry.address.clone(), entry.process.clone()));
    entries
}

fn lsof_entry(fields: &BTreeMap<char, String>, name: &str) -> Option<RuntimeEntry> {
    let (address, port) = split_endpoint(name)?;
    Some(RuntimeEntry {
        protocol: "TCP".to_string(),
        address,
        port,
        pid: fields.get(&'p').and_then(|pid| pid.parse().ok()),
        process: fields.get(&'c').cloned().unwrap_or_default(),
        user: fields.get(&'u').cloned().unwrap_or_default(),
        command: fields.get(&'c').cloned().unwrap_or_default(),
        working_directory: None,
        owned_by_current_user: false,
        kind: RuntimeProcessKind::Unknown,
        origin: RuntimeOrigin::Unknown,
        origin_reason: String::new(),
        project: None,
        external_project: None,
        tunnel: None,
    })
}

fn parse_ss(output: &str) -> Vec<RuntimeEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 {
            continue;
        }
        let Some((address, port)) = split_endpoint(fields[3]) else {
            continue;
        };
        let (process, pid) = parse_ss_process(line);
        entries.push(RuntimeEntry {
            protocol: "TCP".to_string(),
            address,
            port,
            pid,
            process: process.clone(),
            user: String::new(),
            command: process,
            working_directory: None,
            owned_by_current_user: false,
            kind: RuntimeProcessKind::Unknown,
            origin: RuntimeOrigin::Unknown,
            origin_reason: String::new(),
            project: None,
            external_project: None,
            tunnel: None,
        });
    }
    entries.sort_by_key(|entry| (entry.port, entry.address.clone(), entry.process.clone()));
    entries
}

fn parse_ss_process(line: &str) -> (String, Option<u32>) {
    let Some(users) = line.split("users:((").nth(1) else {
        return (String::new(), None);
    };
    let process = users.split('"').nth(1).unwrap_or_default().to_string();
    let pid = users
        .split("pid=")
        .nth(1)
        .and_then(|value| value.split([',', ')']).next())
        .and_then(|value| value.parse().ok());
    (process, pid)
}

fn split_endpoint(value: &str) -> Option<(String, u16)> {
    let value = value.trim().trim_matches('"');
    let (address, port) = value.rsplit_once(':')?;
    let port = port.parse().ok()?;
    Some((address.trim_matches(['[', ']']).to_string(), port))
}

fn exit_label(code: Option<i32>) -> String {
    code.map_or_else(|| "signal".to_string(), |code| format!("exit {code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ownership_matches_the_process_group_only_on_local() {
        let observations = parse_process_observations(
            "10 1 10 ?? sh -c serve\n11 10 10 ?? server\n12 1 12 ?? server /same/path\n",
        );
        let owner = ProjectOwner {
            key: (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)),
            run_id: 3,
            process_group: 10,
            project_name: "Demo".into(),
            command_name: "Serve".into(),
        };
        for pid in [10, 11] {
            assert_eq!(
                project_owner(
                    &RuntimeTarget::Local,
                    Some(pid),
                    &observations,
                    std::slice::from_ref(&owner)
                ),
                Some(owner.clone())
            );
            assert!(
                project_owner(
                    &RuntimeTarget::Host("fixture".into()),
                    Some(pid),
                    &observations,
                    std::slice::from_ref(&owner)
                )
                .is_none()
            );
        }
        assert!(
            project_owner(
                &RuntimeTarget::Local,
                Some(12),
                &observations,
                std::slice::from_ref(&owner)
            )
            .is_none()
        );
        assert!(project_owner(&RuntimeTarget::Local, Some(11), &observations, &[]).is_none());
    }

    #[test]
    fn origin_distinguishes_shell_agent_desktop_and_unknown_ancestry() {
        let processes = parse_process_observations(
            "10 1 10 ?? /Applications/WeChat.app/Contents/MacOS/WeChat\n\
             11 10 10 ?? /Applications/WeChat.app/Contents/Helpers/network\n\
             20 1 20 ttys002 -zsh\n\
             21 20 21 ?? node server.js\n\
             30 20 30 ?? /usr/local/bin/codex\n\
             31 30 30 ?? /bin/sh -c node server.js\n\
             32 31 30 ?? node server.js\n\
             40 1 40 ?? node orphan.js\n\
             50 51 50 ?? worker\n\
             51 50 50 ?? worker\n",
        );
        assert_eq!(
            classify_origin(Some(11), &processes).0,
            RuntimeOrigin::Desktop
        );
        assert_eq!(
            classify_origin(Some(21), &processes).0,
            RuntimeOrigin::CommandLine
        );
        assert_eq!(
            classify_origin(Some(32), &processes).0,
            RuntimeOrigin::Agent
        );
        assert_eq!(
            classify_origin(Some(40), &processes).0,
            RuntimeOrigin::Unknown
        );
        assert_eq!(
            classify_origin(Some(50), &processes).0,
            RuntimeOrigin::Unknown
        );
        assert_eq!(classify_origin(None, &processes).0, RuntimeOrigin::Unknown);
        assert!(is_app_bundle_command(
            "/Applications/Some App.app/Contents/MacOS/App"
        ));
        assert!(!is_app_bundle_command(
            "/usr/local/bin/node /Applications/Foo.app/Contents/a.js"
        ));
    }

    #[test]
    fn parses_lsof_field_output() {
        let entries = parse_lsof("p123\ncnginx\nuroot\nn127.0.0.1:8080\np7\ncsshd\nn*:22\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].port, 22);
        assert_eq!(entries[0].pid, Some(7));
        assert_eq!(entries[1].address, "127.0.0.1");
    }

    #[test]
    fn parses_ss_output() {
        let entries =
            parse_ss(r#"LISTEN 0 128 0.0.0.0:3306 0.0.0.0:* users:(("mysqld",pid=42,fd=3))"#);
        assert_eq!(entries[0].port, 3306);
        assert_eq!(entries[0].pid, Some(42));
        assert_eq!(entries[0].process, "mysqld");
    }

    #[test]
    fn endpoint_parser_supports_ipv6_and_wildcards() {
        assert_eq!(
            split_endpoint("[::1]:8080"),
            Some(("::1".to_string(), 8080))
        );
        assert_eq!(split_endpoint("*:22"), Some(("*".to_string(), 22)));
    }

    #[test]
    fn classifies_user_commands_as_applications_and_daemons_as_system() {
        let application = RuntimeEntry {
            protocol: "TCP".into(),
            address: "127.0.0.1".into(),
            port: 3000,
            pid: Some(10),
            process: "node".into(),
            user: "dada".into(),
            command: "/Users/dada/project/node_modules/.bin/vite".into(),
            working_directory: Some("/Users/dada/project".into()),
            owned_by_current_user: true,
            kind: RuntimeProcessKind::Unknown,
            origin: RuntimeOrigin::Unknown,
            origin_reason: String::new(),
            project: None,
            external_project: None,
            tunnel: None,
        };
        let daemon = RuntimeEntry {
            protocol: "TCP".into(),
            address: "*".into(),
            port: 22,
            pid: Some(11),
            process: "sshd".into(),
            user: "root".into(),
            command: "/usr/sbin/sshd".into(),
            working_directory: Some("/var/empty".into()),
            owned_by_current_user: false,
            kind: RuntimeProcessKind::Unknown,
            origin: RuntimeOrigin::Unknown,
            origin_reason: String::new(),
            project: None,
            external_project: None,
            tunnel: None,
        };
        assert_eq!(
            classify_process(&application),
            RuntimeProcessKind::Application
        );
        assert_eq!(classify_process(&daemon), RuntimeProcessKind::System);
        let mut root_app = application.clone();
        root_app.user = "0".into();
        root_app.kind = classify_process(&root_app);
        assert_eq!(root_app.kind, RuntimeProcessKind::Application);
        assert!(RuntimeFilter::Mine.matches(&root_app));
        assert!(RuntimeFilter::Applications.matches(&root_app));
        assert!(!RuntimeFilter::System.matches(&root_app));
        root_app.owned_by_current_user = false;
        assert!(!RuntimeFilter::Mine.matches(&root_app));
        let mut unknown = root_app;
        unknown.user.clear();
        assert_eq!(classify_process(&unknown), RuntimeProcessKind::Unknown);
    }
}
