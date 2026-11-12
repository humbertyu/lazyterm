use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

use crate::{
    projects::{ProjectDefinition, ProjectOwner, TaskKey, working_directory},
    runtime::{self, RuntimeTarget},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    uid: u32,
    started: String,
    pub command: String,
}

impl ProcessIdentity {
    fn same_process(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.uid == other.uid
            && self.started == other.started
            && self.command == other.command
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalRun {
    pub key: TaskKey,
    pub project_name: String,
    pub command_name: String,
    pub root: ProcessIdentity,
    pub directory: PathBuf,
    pub processes: Vec<ProcessIdentity>,
    pub ports: Vec<u16>,
}

impl ExternalRun {
    pub fn label(&self) -> String {
        let ports = self
            .ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "external running / {}",
            if ports.is_empty() {
                format!("PID {}", self.root.pid)
            } else {
                format!(":{ports}")
            }
        )
    }
}

struct Inspection {
    generation: u64,
    definitions: Vec<ProjectDefinition>,
    owners: Vec<ProjectOwner>,
    result: Result<BTreeMap<TaskKey, Vec<ExternalRun>>, String>,
}

pub struct ProjectMonitor {
    pub runs: BTreeMap<TaskKey, Vec<ExternalRun>>,
    pub checking: bool,
    pub error: Option<String>,
    pub generation: u64,
    pub completed: u64,
    definitions: Vec<ProjectDefinition>,
    checked_at: Option<Instant>,
    sender: Sender<Inspection>,
    receiver: Receiver<Inspection>,
}

impl Default for ProjectMonitor {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            runs: BTreeMap::new(),
            checking: false,
            error: None,
            generation: 0,
            completed: 0,
            definitions: Vec::new(),
            checked_at: None,
            sender,
            receiver,
        }
    }
}

impl ProjectMonitor {
    pub fn matches(&self, definitions: &[ProjectDefinition]) -> bool {
        self.completed > 0 && self.definitions == definitions
    }

    pub fn due(&self, definitions: &[ProjectDefinition]) -> bool {
        !self.checking
            && (!self.matches(definitions)
                || self
                    .checked_at
                    .is_none_or(|time| time.elapsed() >= Duration::from_secs(3)))
    }

    pub fn refresh(&mut self, definitions: &[ProjectDefinition], owners: Vec<ProjectOwner>) {
        if self.checking {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.checking = true;
        let sender = self.sender.clone();
        let definitions = definitions.to_vec();
        std::thread::spawn(move || {
            let result = inspect(&definitions, &owners);
            let _ = sender.send(Inspection {
                generation,
                definitions,
                owners,
                result,
            });
        });
    }

    pub fn poll(&mut self, definitions: &[ProjectDefinition], owners: &[ProjectOwner]) -> bool {
        let Ok(inspection) = self.receiver.try_recv() else {
            return false;
        };
        self.checking = false;
        if inspection.definitions != definitions || inspection.owners != owners {
            self.checked_at = None;
            return false;
        }
        self.completed = inspection.generation;
        self.definitions = inspection.definitions;
        self.checked_at = Some(Instant::now());
        match inspection.result {
            Ok(runs) => {
                self.runs = runs;
                self.error = None;
            }
            Err(error) => {
                self.runs.clear();
                self.error = Some(error);
            }
        }
        true
    }
}

fn inspect(
    definitions: &[ProjectDefinition],
    owners: &[ProjectOwner],
) -> Result<BTreeMap<TaskKey, Vec<ExternalRun>>, String> {
    if definitions.is_empty() {
        return Ok(BTreeMap::new());
    }
    let processes = process_snapshot()?;
    let listeners = runtime::inspect_listeners(&RuntimeTarget::Local)?;
    let mut directories = HashMap::new();
    let mut ports = HashMap::<u32, Vec<u16>>::new();
    for entry in listeners {
        if let Some(pid) = entry.pid {
            ports.entry(pid).or_default().push(entry.port);
        }
    }
    let mut ancestors = ancestors_of(std::process::id(), &processes);
    ancestors.insert(std::process::id());
    let uid = unsafe { libc::geteuid() };
    for process in processes.values() {
        if process.uid != uid
            || ancestors.contains(&process.pid)
            || owners
                .iter()
                .any(|owner| owner.process_group == process.group)
        {
            continue;
        }
        if definitions.iter().any(|project| {
            project
                .commands
                .iter()
                .any(|command| command_matches(&command.command, &process.command))
        }) {
            directories.insert(
                process.pid,
                runtime::process_working_directory(&RuntimeTarget::Local, process.pid)
                    .and_then(|path| PathBuf::from(path).canonicalize().ok()),
            );
        }
    }
    Ok(associate(
        definitions,
        owners,
        &processes,
        &directories,
        &ports,
        uid,
        &ancestors,
    ))
}

fn associate(
    definitions: &[ProjectDefinition],
    owners: &[ProjectOwner],
    processes: &HashMap<u32, ProcessIdentity>,
    directories: &HashMap<u32, Option<PathBuf>>,
    ports: &HashMap<u32, Vec<u16>>,
    uid: u32,
    excluded: &HashSet<u32>,
) -> BTreeMap<TaskKey, Vec<ExternalRun>> {
    let mut matches = HashMap::<u32, Vec<ExternalRun>>::new();
    for project in definitions {
        for command in &project.commands {
            let Ok(directory) = working_directory(project, command) else {
                continue;
            };
            for process in processes.values() {
                if process.uid != uid
                    || excluded.contains(&process.pid)
                    || owners
                        .iter()
                        .any(|owner| owner.process_group == process.group)
                    || !command_matches(&command.command, &process.command)
                    || directories.get(&process.pid).and_then(Option::as_ref) != Some(&directory)
                {
                    continue;
                }
                let mut members = descendants(process.pid, processes);
                members.sort_by_key(|process| process.pid);
                let mut listening = members
                    .iter()
                    .filter_map(|process| ports.get(&process.pid))
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>();
                listening.sort_unstable();
                listening.dedup();
                matches.entry(process.pid).or_default().push(ExternalRun {
                    key: (project.id, command.id),
                    project_name: project.name.clone(),
                    command_name: command.name.clone(),
                    root: process.clone(),
                    directory: directory.clone(),
                    processes: members,
                    ports: listening,
                });
            }
        }
    }
    let mut result = BTreeMap::<TaskKey, Vec<ExternalRun>>::new();
    for mut candidates in matches
        .into_values()
        .filter(|candidates| candidates.len() == 1)
    {
        let run = candidates.pop().expect("one candidate");
        // A wrapper and its child may expose the same command; retain its outer root.
        if ancestors_of(run.root.pid, processes).iter().any(|pid| {
            result
                .get(&run.key)
                .is_some_and(|runs| runs.iter().any(|candidate| candidate.root.pid == *pid))
        }) {
            continue;
        }
        let runs = result.entry(run.key).or_default();
        runs.retain(|candidate| {
            !ancestors_of(candidate.root.pid, processes).contains(&run.root.pid)
        });
        runs.push(run);
        runs.sort_by_key(|run| run.root.pid);
    }
    result
}

fn command_matches(expected: &str, observed: &str) -> bool {
    let Some(expected) = normalized_command(expected) else {
        return false;
    };
    normalized_command(observed).is_some_and(|observed| observed == expected)
}

fn normalized_command(command: &str) -> Option<Vec<String>> {
    let mut words = shell_words::split(command).ok()?;
    if words.is_empty() {
        return None;
    }
    if words[0] == "env" {
        words.remove(0);
    }
    while words.first().is_some_and(|word| {
        word.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                })
        })
    }) {
        words.remove(0);
    }
    if words.is_empty() {
        return None;
    }
    words[0] = words[0].rsplit('/').next()?.to_string();
    if matches!(words[0].as_str(), "sh" | "bash" | "zsh")
        && words.get(1).map(String::as_str) == Some("-c")
    {
        if words.len() < 3 {
            return None;
        }
        if words.len() == 3 {
            return normalized_command(&words[2]);
        }
        words.drain(..2);
        words[0] = words[0].rsplit('/').next()?.to_string();
    }
    if matches!(words[0].as_str(), "npm" | "pnpm" | "yarn") {
        words.retain(|word| word != "--");
        if words.get(1).map(String::as_str) == Some("run-script") {
            words[1] = "run".into();
        }
        if words[0] == "npm" && words.get(1).map(String::as_str) == Some("start") {
            words.insert(1, "run".into());
        }
    }
    Some(words)
}

fn ancestors_of(pid: u32, processes: &HashMap<u32, ProcessIdentity>) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut current = pid;
    for _ in 0..64 {
        let Some(process) = processes.get(&current) else {
            break;
        };
        if process.parent <= 1 || !seen.insert(process.parent) {
            break;
        }
        current = process.parent;
    }
    seen
}

fn descendants(root: u32, processes: &HashMap<u32, ProcessIdentity>) -> Vec<ProcessIdentity> {
    processes
        .values()
        .filter(|process| {
            process.pid == root || ancestors_of(process.pid, processes).contains(&root)
        })
        .cloned()
        .collect()
}

fn process_snapshot() -> Result<HashMap<u32, ProcessIdentity>, String> {
    let output = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-ww", "-axo", "pid=,ppid=,pgid=,uid=,lstart=,command="])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not inspect project processes: {error}"))?;
    if !output.status.success() {
        return Err("Could not inspect project processes with ps.".into());
    }
    Ok(parse_process_snapshot(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_process_snapshot(text: &str) -> HashMap<u32, ProcessIdentity> {
    text.lines()
        .filter_map(|line| {
            let mut remaining = line.trim();
            let mut field = || {
                let (value, rest) = remaining.split_once(char::is_whitespace)?;
                remaining = rest.trim_start();
                Some(value)
            };
            let pid = field()?.parse().ok()?;
            let parent = field()?.parse().ok()?;
            let group = field()?.parse().ok()?;
            let uid = field()?.parse().ok()?;
            let started = (0..5)
                .map(|_| field())
                .collect::<Option<Vec<_>>>()?
                .join(" ");
            Some((
                pid,
                ProcessIdentity {
                    pid,
                    parent,
                    group,
                    uid,
                    started,
                    command: remaining.trim_end().into(),
                },
            ))
        })
        .collect()
}

pub fn stop_external_async(runs: Vec<ExternalRun>) -> Receiver<Result<(), String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(stop_external(&runs));
    });
    receiver
}

fn stop_external(runs: &[ExternalRun]) -> Result<(), String> {
    let snapshot = process_snapshot()?;
    let mut protected = ancestors_of(std::process::id(), &snapshot);
    protected.insert(std::process::id());
    let mut targets = BTreeMap::new();
    for run in runs {
        let Some(root) = snapshot.get(&run.root.pid) else {
            return Err("External command ended; refresh before acting.".into());
        };
        let cwd = runtime::process_working_directory(&RuntimeTarget::Local, root.pid)
            .and_then(|path| PathBuf::from(path).canonicalize().ok());
        if !root.same_process(&run.root) || cwd.as_ref() != Some(&run.directory) {
            return Err("External process identity changed; refresh before acting.".into());
        }
        for member in descendants(root.pid, &snapshot) {
            if member.uid != root.uid || protected.contains(&member.pid) || member.pid <= 1 {
                return Err("Refusing to terminate an unrelated project process.".into());
            }
            targets.insert(member.pid, member);
        }
    }
    // Signal individual validated descendants. External process groups can be shared.
    let mut ordered = targets.values().collect::<Vec<_>>();
    ordered.sort_by_key(|process| std::cmp::Reverse(ancestors_of(process.pid, &snapshot).len()));
    for process in ordered {
        let fresh = process_snapshot()?;
        if !fresh
            .get(&process.pid)
            .is_some_and(|current| process.same_process(current))
        {
            continue;
        }
        let pid = i32::try_from(process.pid).map_err(|_| "Invalid external PID.")?;
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(format!("Could not stop PID {pid}: {error}"));
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let fresh = process_snapshot()?;
        if !targets.values().any(|process| {
            fresh
                .get(&process.pid)
                .is_some_and(|current| process.same_process(current))
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "External command did not stop after TERM; no restart or KILL was performed."
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::projects::{ProjectCommand, tests::Fixture};
    use std::{net::TcpStream, process::Child};

    pub struct ExternalFixture {
        pub fixture: Fixture,
        pub child: Child,
        pub command: ProjectCommand,
        pub project: ProjectDefinition,
    }

    impl ExternalFixture {
        pub fn new() -> Self {
            let fixture = Fixture::new("external");
            let exe = std::env::current_exe().unwrap();
            let arguments = [
                "--ignored",
                "--exact",
                "projects::tests::disposable_listener",
                "--nocapture",
            ];
            let command = fixture.command(&format!(
                "LAZYTERM_PROJECT_FIXTURE={} LAZYTERM_PROJECT_IGNORE_TERM=0 {} {}",
                shell_words::quote(fixture.root.to_str().unwrap()),
                shell_words::quote(exe.to_str().unwrap()),
                arguments.join(" ")
            ));
            let child = Command::new(&exe)
                .args(arguments)
                .env("LAZYTERM_PROJECT_FIXTURE", &fixture.root)
                .env("LAZYTERM_PROJECT_IGNORE_TERM", "0")
                .current_dir(&fixture.root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut project = fixture.project.clone();
            project.commands.push(command.clone());
            let external = Self {
                fixture,
                child,
                command,
                project,
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            while external.fixture.ready().is_none() {
                assert!(Instant::now() < deadline, "External fixture did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
            external
        }
    }

    impl Drop for ExternalFixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn command_matching_handles_npm_titles_and_shells_without_loose_substrings() {
        assert!(command_matches(
            "npm run start -- -p 3000",
            "npm run start -p 3000"
        ));
        assert!(command_matches(
            "npm start -- -p 3000",
            "/usr/local/bin/npm run start -p 3000"
        ));
        assert!(command_matches(
            "npm run start -- -p 3000",
            "sh -c npm run start -- -p 3000"
        ));
        assert!(command_matches("npm run start", "sh -c 'npm run start'"));
        assert!(command_matches(
            "NODE_ENV=production npm run start",
            "npm run start"
        ));
        assert!(!command_matches(
            "npm run start -- -p 3000",
            "npm run start -p 3001"
        ));
        assert!(!command_matches("npm run start", "npm run start-other"));
        assert!(!command_matches("npm run start", "next-server (v15.5.18)"));
        assert!(!command_matches(
            "node 'server file.js'",
            "node server file.js"
        ));
        assert!(!command_matches("npm run start", "sh -c"));
    }

    #[test]
    fn association_requires_directory_uid_command_and_unique_definition() {
        let fixture = Fixture::new("association");
        let root = fixture.root.canonicalize().unwrap();
        let mut project = fixture.project.clone();
        project
            .commands
            .push(fixture.command("npm run start -- -p 3000"));
        let processes = parse_process_snapshot(
            "10 1 10 501 Sun Nov 8 12:00:00 2026 npm run start -p 3000\n\
             11 10 10 501 Sun Nov 8 12:00:01 2026 next-server (v15.5.18)\n\
             12 1 12 502 Sun Nov 8 12:00:00 2026 npm run start -p 3000\n\
             13 1 13 501 Sun Nov 8 12:00:00 2026 npm run start -p 3001\n",
        );
        let directories = HashMap::from([
            (10, Some(root.clone())),
            (12, Some(root.clone())),
            (13, Some(root.clone())),
        ]);
        let ports = HashMap::from([(11, vec![3000])]);
        let runs = associate(
            std::slice::from_ref(&project),
            &[],
            &processes,
            &directories,
            &ports,
            501,
            &HashSet::new(),
        );
        let key = (project.id, project.commands[0].id);
        assert_eq!(runs[&key].len(), 1);
        assert_eq!(runs[&key][0].root.pid, 10);
        assert_eq!(runs[&key][0].ports, [3000]);
        assert!(
            runs[&key][0]
                .processes
                .iter()
                .any(|process| process.pid == 11)
        );
        let wrong_cwd = HashMap::from([(10, Some(root.join("sub directory")))]);
        assert!(
            associate(
                std::slice::from_ref(&project),
                &[],
                &processes,
                &wrong_cwd,
                &ports,
                501,
                &HashSet::new()
            )
            .is_empty()
        );
        let owner = ProjectOwner {
            key,
            run_id: 1,
            process_group: 10,
            project_name: project.name.clone(),
            command_name: project.commands[0].name.clone(),
        };
        assert!(
            associate(
                std::slice::from_ref(&project),
                &[owner],
                &processes,
                &directories,
                &ports,
                501,
                &HashSet::new()
            )
            .is_empty()
        );
        let mut ambiguous = project.clone();
        ambiguous.id = uuid::Uuid::new_v4();
        assert!(
            associate(
                &[project, ambiguous],
                &[],
                &processes,
                &directories,
                &ports,
                501,
                &HashSet::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn real_external_detection_does_not_take_ownership_and_validates_before_stop() {
        let mut fixture = ExternalFixture::new();
        let definitions = vec![fixture.project.clone()];
        let runs = inspect(&definitions, &[]).unwrap();
        let key = (fixture.project.id, fixture.command.id);
        let run = &runs[&key][0];
        assert_eq!(run.root.pid, fixture.child.id());
        let (_, port) = fixture.fixture.ready().unwrap();
        assert!(run.ports.contains(&port));
        // The fixture shares its process group with this test. Positive PID signals only.
        assert_eq!(
            run.root.group,
            process_snapshot().unwrap()[&std::process::id()].group
        );
        let mut stale = run.clone();
        stale.root.started.push_str(" stale");
        assert!(stop_external(&[stale]).is_err());
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        let mut monitor = ProjectMonitor::default();
        monitor.refresh(&definitions, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !monitor.poll(&definitions, &[]) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!monitor.runs.is_empty());
        drop(monitor);
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_ok(),
            "Monitor drop must not own external processes"
        );
        stop_external(std::slice::from_ref(run)).unwrap();
        fixture.child.wait().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    #[test]
    fn monitor_rejects_results_for_old_definitions_and_old_managed_generations() {
        let fixture = Fixture::new("stale-scan");
        let mut monitor = ProjectMonitor::default();
        let definitions = vec![fixture.project.clone()];
        let mut changed = definitions.clone();
        changed[0].name.push_str(" changed");
        monitor.checking = true;
        monitor
            .sender
            .send(Inspection {
                generation: 1,
                definitions: definitions.clone(),
                owners: Vec::new(),
                result: Ok(BTreeMap::new()),
            })
            .unwrap();
        assert!(!monitor.poll(&changed, &[]));
        assert!(!monitor.matches(&changed));
        let owner = ProjectOwner {
            key: (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)),
            run_id: 1,
            process_group: 99,
            project_name: "Demo".into(),
            command_name: "Start".into(),
        };
        monitor
            .sender
            .send(Inspection {
                generation: 2,
                definitions: definitions.clone(),
                owners: vec![owner.clone()],
                result: Ok(BTreeMap::new()),
            })
            .unwrap();
        let replacement = ProjectOwner { run_id: 2, ..owner };
        assert!(!monitor.poll(&definitions, &[replacement]));
        assert_eq!(monitor.completed, 0);
    }
}
