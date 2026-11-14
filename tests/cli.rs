use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use uuid::Uuid;

struct Client {
    root: PathBuf,
    runs: Vec<(String, String, String)>,
}

impl Client {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("lazyterm-cli-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join(".ssh")).unwrap();
        Self { root, runs: vec![] }
    }

    fn data(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.root.join("Library/Application Support/lazyterm")
        } else {
            self.root.join("data/lazyterm")
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lazyterm"));
        command
            .env("HOME", &self.root)
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env_remove("LAZYTERM_AUTH_SOCKET")
            .env_remove("LAZYTERM_ASKPASS_REFERENCE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn call(&self, args: &[&str], payload: Option<Value>) -> Output {
        let mut command = self.command();
        command.args(args);
        if let Some(payload) = payload {
            command.args(["--input", "-"]).stdin(Stdio::piped());
            let mut child = command.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(payload.to_string().as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        } else {
            command.output().unwrap()
        }
    }

    fn ok(&self, args: &[&str], payload: Option<Value>) -> Value {
        let output = self.call(args, payload);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["ok"], true);
        envelope["data"].clone()
    }

    fn error(&self, args: &[&str], payload: Option<Value>, exit: i32, code: &str) {
        let output = self.call(args, payload);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], code);
    }

    fn seed(&self, metadata: Value) {
        fs::create_dir_all(self.data()).unwrap();
        fs::write(self.data().join("hosts.json"), metadata.to_string()).unwrap();
    }

    fn metadata(&self) -> Value {
        serde_json::from_slice(&fs::read(self.data().join("hosts.json")).unwrap()).unwrap()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        for (project, command, run) in &self.runs {
            let _ = self.call(
                &[
                    "projects", "stop", project, command, "--run-id", run, "--yes",
                ],
                None,
            );
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn discovery_is_read_only_and_independent_of_tui_lock() {
    let client = Client::new();
    fs::write(
        client.root.join(".ssh/config"),
        "Host fixture\n HostName 192.0.2.1\n",
    )
    .unwrap();
    for args in [
        vec!["schema", "--json"],
        vec!["hosts", "list"],
        vec!["groups", "list"],
        vec!["projects", "list"],
        vec!["snippets", "list"],
        vec!["tunnels", "list"],
        vec!["projects", "status"],
    ] {
        client.ok(&args, None);
    }
    assert!(
        !client.data().exists(),
        "queries must not create config or lock files"
    );
    client.seed(json!({"version":3}));
    let before = fs::read(client.data().join("hosts.json")).unwrap();
    let lock = fs::File::create(client.data().join("tui.instance.lock")).unwrap();
    lock.lock().unwrap();
    assert_eq!(
        client.ok(&["hosts", "get", "fixture"], None)["hostname"],
        "192.0.2.1"
    );
    client.ok(&["groups", "create"], Some(json!({"name":"test"})));
    assert_ne!(before, fs::read(client.data().join("hosts.json")).unwrap());
    assert!(client.call(&["--help"], None).status.success());
    client.error(&["hosts", "connect", "fixture"], None, 2, "invalid_input");
    client.error(&["hosts", "get", "absent"], None, 3, "not_found");
    client.error(&["hosts", "list", "extra"], None, 2, "invalid_input");
    client.error(
        &["schema", "--input", "-"],
        Some(json!({})),
        2,
        "invalid_input",
    );
}

#[test]
fn host_writes_preserve_custom_directives_and_other_metadata() {
    let client = Client::new();
    let config = client.root.join(".ssh/config");
    fs::write(&config, "# keep comment\nHost existing\n HostName 192.0.2.1\n User root\n ServerAliveInterval 42\n\nHost other\n HostName 192.0.2.2\n").unwrap();
    client.seed(json!({"version":3, "future_field":{"preserve":true}}));
    client.ok(
        &["hosts", "update", "existing"],
        Some(json!({"user":"deploy"})),
    );
    let contents = fs::read_to_string(&config).unwrap();
    assert!(contents.contains("# keep comment"));
    assert!(contents.contains("ServerAliveInterval 42"));
    assert!(contents.contains("Host other"));
    assert_eq!(
        client.ok(&["hosts", "get", "existing"], None)["user"],
        "deploy"
    );
    client.ok(
        &["hosts", "create"],
        Some(json!({"alias":"new","hostname":"192.0.2.3","port":2222})),
    );
    client.ok(
        &["hosts", "tag", "new"],
        Some(json!({"groups":["Development"]})),
    );
    client.ok(
        &["groups", "update", "Development"],
        Some(json!({"name":"Dev"})),
    );
    assert_eq!(
        client.ok(&["hosts", "get", "new"], None)["groups"],
        json!(["Dev"])
    );
    assert_eq!(client.metadata()["future_field"]["preserve"], true);
    let before = fs::read(&config).unwrap();
    client.error(
        &["hosts", "update", "new"],
        Some(json!({"port":0})),
        2,
        "invalid_input",
    );
    client.error(
        &["hosts", "update", "new"],
        Some(json!({"alias":"renamed"})),
        2,
        "invalid_input",
    );
    client.error(
        &["hosts", "create"],
        Some(json!({"alias":"bad","password":"hidden"})),
        2,
        "invalid_input",
    );
    assert_eq!(before, fs::read(&config).unwrap());
}

#[test]
fn nested_uuid_updates_preserve_siblings_and_secrets_without_returning_them() {
    let client = Client::new();
    let project = Uuid::new_v4().to_string();
    let first = Uuid::new_v4().to_string();
    let second = Uuid::new_v4().to_string();
    let link = Uuid::new_v4().to_string();
    client.seed(json!({"version":3, "projects":[{
        "id":project,"name":"fixture","base_path":client.root,
        "commands":[{"id":first,"name":"one","command":"echo one"},{"id":second,"name":"two","command":"echo two"}],
        "links":[{"id":link,"name":"Home","url":"http://localhost:3000/","password_storage":"Config","password":"opaque-test-secret","password_ref":"secret-ref"}]
    }]}));
    client.ok(&["projects","update",&project], Some(json!({"commands":[{"id":first,"name":"updated"}],"links":[{"id":link,"name":"Start"}]})));
    let saved = client.metadata();
    assert_eq!(
        saved["projects"][0]["commands"].as_array().unwrap().len(),
        2
    );
    assert_eq!(saved["projects"][0]["commands"][0]["command"], "echo one");
    assert_eq!(
        saved["projects"][0]["links"][0]["password"],
        "opaque-test-secret"
    );
    let exposed = client.ok(&["projects", "get", &project], None).to_string();
    assert!(!exposed.contains("opaque-test-secret"));
    assert!(!exposed.contains("secret-ref"));
    assert!(!exposed.contains("password_storage"));
    client.error(
        &["projects", "update", &project],
        Some(json!({"links":[{"id":link,"password":"replacement"}]})),
        2,
        "invalid_input",
    );
    client.error(
        &["projects", "update", &project],
        Some(json!({"commands":[{"id":first,"name":"a"},{"id":first,"name":"b"}]})),
        2,
        "invalid_input",
    );
    assert_eq!(saved, client.metadata());
    // A deleted Host may leave an intentionally retained Project definition.
    // Editing another Project must not discard or require repairing that record.
    let mut with_orphan = saved.clone();
    with_orphan["projects"].as_array_mut().unwrap().push(json!({
        "id":Uuid::new_v4(),"name":"retained remote","base_path":"/tmp","target":{"Host":"removed-host"}
    }));
    client.seed(with_orphan);
    client.ok(
        &["projects", "update", &project],
        Some(json!({"name":"still editable"})),
    );
    assert_eq!(client.metadata()["projects"].as_array().unwrap().len(), 2);
    let created = client.ok(
        &["snippets", "create"],
        Some(json!({"name":"Ops","snippets":[{"name":"Health","command":"uptime"}]})),
    );
    let package = created["id"].as_str().unwrap();
    client.ok(
        &["snippets", "update", package],
        Some(json!({"snippets":[{"name":"Disk","command":"df -h"}]})),
    );
    assert_eq!(
        client.ok(&["snippets", "get", package], None)["snippets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    client.error(
        &["projects", "create"],
        Some(json!({"name":"remote","base_path":"/tmp","target":{"Host":"absent"}})),
        2,
        "invalid_input",
    );
    client.error(&["snippets","create"], Some(json!({"name":"bad","snippets":[{"name":"key","command":"-----BEGIN OPENSSH PRIVATE KEY-----"}]})), 2, "invalid_input");
}

#[test]
fn invalid_configuration_and_payloads_do_not_fall_through_to_tui() {
    let client = Client::new();
    fs::create_dir_all(client.data()).unwrap();
    fs::write(client.data().join("hosts.json"), b"{broken").unwrap();
    client.error(&["projects", "list"], None, 1, "operation_failed");
    assert_eq!(
        fs::read(client.data().join("hosts.json")).unwrap(),
        b"{broken"
    );
    client.seed(json!({"version":1}));
    client.error(&["projects", "list"], None, 2, "invalid_input");
    assert_eq!(client.metadata()["version"], 1);
    client.seed(json!({"version":3}));
    client.error(
        &["projects", "start", "not-a-uuid", "also-invalid"],
        None,
        4,
        "approval_required",
    );
    client.error(
        &["projects", "create"],
        Some(json!({"name":"no base path"})),
        2,
        "invalid_input",
    );
    assert_eq!(client.metadata(), json!({"version":3}));
    let mut child = client
        .command()
        .args(["groups", "create", "--input", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{bad json").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn live_tui_refreshes_cli_configuration_without_replacing_an_open_form() {
    let client = Client::new();
    client.seed(json!({"version":3}));
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = libc::winsize {
        ws_row: 32,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert_ne!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        -1
    );
    let mut tui = client
        .command()
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    struct Tui<'a>(&'a mut std::process::Child);
    impl Drop for Tui<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let tui = Tui(&mut tui);
    fn wait_text(master: &mut fs::File, child: &mut std::process::Child, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut seen = String::new();
        while !seen.contains(expected) {
            let mut buffer = [0; 32768];
            match master.read(&mut buffer) {
                Ok(n) => seen.push_str(&String::from_utf8_lossy(&buffer[..n])),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("PTY read: {error}; {seen}"),
            }
            assert!(child.try_wait().unwrap().is_none(), "TUI exited: {seen}");
            assert!(Instant::now() < deadline, "Missing {expected}: {seen}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    wait_text(&mut master, tui.0, "Hosts");
    master.write_all(b"6").unwrap();
    wait_text(&mut master, tui.0, "PROJECTS");
    let created = client.ok(
        &["projects", "create"],
        Some(json!({
            "name":"cli-created-workspace","base_path":client.root
        })),
    );
    wait_text(&mut master, tui.0, "cli-created-workspace");
    master.write_all(b"e").unwrap();
    wait_text(&mut master, tui.0, "EDIT PROJECT");
    let id = created["id"].as_str().unwrap();
    client.ok(
        &["projects", "update", id],
        Some(json!({"name":"refreshed-after-form"})),
    );
    master.write_all(b"\r").unwrap();
    wait_text(&mut master, tui.0, "changed on disk");
    assert_eq!(
        client.metadata()["projects"][0]["name"],
        "refreshed-after-form"
    );
    // Return to the list; the deferred update must become visible without restarting.
    master.write_all(b"\x1b").unwrap();
    wait_text(&mut master, tui.0, "refreshed-after-form");
    assert_eq!(
        client.metadata()["projects"][0]["name"],
        "refreshed-after-form"
    );
    master.write_all(b"q").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while tui.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "TUI did not quit");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn external_project_status_prevents_duplicates_and_stop_revalidates_identity() {
    let client = Client::new();
    let executable = std::env::current_exe().unwrap();
    let text = format!(
        "{} --ignored --exact disposable_listener --nocapture",
        shell_words::quote(executable.to_str().unwrap())
    );
    let created = client.ok(
        &["projects", "create"],
        Some(json!({
            "name":"external","base_path":client.root,
            "commands":[{"name":"serve","command":text}]
        })),
    );
    let project = created["id"].as_str().unwrap();
    let definition = client.ok(&["projects", "get", project], None);
    let command = definition["commands"][0]["id"].as_str().unwrap();
    let mut child = Command::new(executable)
        .args(["--ignored", "--exact", "disposable_listener", "--nocapture"])
        .current_dir(&client.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Cleanup<'a>(&'a mut std::process::Child);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let cleanup = Cleanup(&mut child);
    let before = fs::read(client.data().join("hosts.json")).unwrap();
    let status = client.ok(&["projects", "status"], None);
    assert_eq!(status[0]["run"]["source"], "external", "{status}");
    let run_id = status[0]["run"]["run_id"].as_str().unwrap();
    client.error(
        &["projects", "start", project, command, "--yes"],
        None,
        5,
        "conflict",
    );
    client.error(
        &[
            "projects",
            "stop",
            project,
            command,
            "--run-id",
            "external:stale",
            "--yes",
        ],
        None,
        5,
        "conflict",
    );
    assert!(cleanup.0.try_wait().unwrap().is_none());
    client.error(
        &["projects", "stop", project, command, "--run-id", run_id],
        None,
        4,
        "approval_required",
    );
    let stopped = client.ok(
        &[
            "projects", "stop", project, command, "--run-id", run_id, "--yes",
        ],
        None,
    );
    assert_eq!(stopped["active"], false);
    assert_eq!(before, fs::read(client.data().join("hosts.json")).unwrap());
}

#[test]
fn disposable_ssh_runtime_and_remote_project_cli() {
    if std::env::var("LAZYTERM_SFTP_FIXTURE").as_deref() != Ok("1") {
        eprintln!("Set LAZYTERM_SFTP_FIXTURE=1 for real SSH CLI acceptance.");
        return;
    }
    let mut client = Client::new();
    fn success(command: &mut Command) -> Output {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    for name in ["client", "server"] {
        success(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(client.root.join(name)),
        );
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let user = String::from_utf8(success(Command::new("id").arg("-un")).stdout).unwrap();
    let config = client.root.join("sshd_config");
    fs::write(&config, format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nAllowUsers {}\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n",
        client.root.join("server").display(),client.root.join("sshd.pid").display(),
        client.root.join("client.pub").display(),user.trim())).unwrap();
    let mut server = Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f"])
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    struct Server<'a>(&'a mut std::process::Child);
    impl Drop for Server<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _server = Server(&mut server);
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "fixture sshd failed to start");
        std::thread::sleep(Duration::from_millis(30));
    }
    let scan = success(Command::new("ssh-keyscan").args(["-p", &port.to_string(), "127.0.0.1"]));
    let known_hosts = client.root.join(".ssh/known_hosts");
    fs::write(&known_hosts, scan.stdout).unwrap();
    fs::write(client.root.join(".ssh/config"),format!(
        "Host fixture\n HostName 127.0.0.1\n Port {port}\n User {}\n IdentityFile {}\n IdentitiesOnly yes\n UserKnownHostsFile {}\n StrictHostKeyChecking yes\n",
        user.trim(),client.root.join("client").display(),known_hosts.display())).unwrap();
    client.seed(json!({"version":3}));
    client.error(
        &["runtime", "list", "--host", "missing"],
        None,
        3,
        "not_found",
    );
    let runtime = client.ok(&["runtime", "list", "--host", "fixture"], None);
    assert_eq!(runtime["target"], "fixture");
    assert!(
        runtime["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["port"] == port)
    );

    let ready = client.root.join("remote-ready");
    let script = format!(
        "import socket,os,time; s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); open({},'w').write(str(os.getpid())+':'+str(s.getsockname()[1])); print('remote-ready',flush=True); time.sleep(60)",
        serde_json::to_string(ready.to_str().unwrap()).unwrap()
    );
    let created = client.ok(&["projects","create"],Some(json!({
        "name":"remote","base_path":client.root,"target":{"Host":"fixture"},
        "commands":[{"name":"serve","command":format!("python3 -u -c {}",shell_words::quote(&script)),
        "lifecycle":"App","kind":"Service"},
        {"name":"once","command":"printf remote-task-completed; exit 0",
        "lifecycle":"App","kind":"Task"}]
    })));
    let project = created["id"].as_str().unwrap().to_string();
    let definition = client.ok(&["projects", "get", &project], None);
    let command = definition["commands"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let before = fs::read(client.data().join("hosts.json")).unwrap();
    let once = definition["commands"][1]["id"].as_str().unwrap();
    let task = client.ok(&["projects", "start", &project, once, "--yes"], None);
    client.runs.push((
        project.clone(),
        once.into(),
        task["run_id"].as_str().unwrap().into(),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = client.ok(&["projects", "status"], None);
        if status
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["command_id"] == once && run["run"]["active"] == false)
        {
            let output = client.ok(&["projects", "output", &project, once], None);
            assert_eq!(output["run"]["status"], "exit 0", "{output}");
            assert!(
                output["output"]
                    .as_str()
                    .unwrap()
                    .contains("remote-task-completed")
            );
            break;
        }
        assert!(Instant::now() < deadline, "remote Task did not finish");
        std::thread::sleep(Duration::from_millis(30));
    }
    let run = client.ok(&["projects", "start", &project, &command, "--yes"], None);
    let run_id = run["run_id"].as_str().unwrap().to_string();
    client
        .runs
        .push((project.clone(), command.clone(), run_id.clone()));
    assert_eq!(run["target"], json!({"Host":"fixture"}));
    let deadline = Instant::now() + Duration::from_secs(10);
    let remote_port = loop {
        if let Ok(contents) = fs::read_to_string(&ready)
            && let Some((_, port)) = contents.split_once(':')
            && let Ok(port) = port.parse::<u16>()
        {
            break port;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            client.ok(&["projects", "output", &project, &command], None)
        );
        std::thread::sleep(Duration::from_millis(30));
    };
    assert!(std::net::TcpStream::connect(("127.0.0.1", remote_port)).is_ok());
    let output = client.ok(&["projects", "output", &project, &command], None);
    assert!(output["output"].as_str().unwrap().contains("remote-ready"));
    client.error(
        &[
            "projects", "stop", &project, &command, "--run-id", "1", "--yes",
        ],
        None,
        5,
        "conflict",
    );
    client.ok(
        &[
            "projects", "stop", &project, &command, "--run-id", &run_id, "--yes",
        ],
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", remote_port)).is_ok() {
        assert!(Instant::now() < deadline, "remote child survived stop");
        std::thread::sleep(Duration::from_millis(30));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.ok(&["projects", "status"], None)[0]["run"]["active"] != false {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    }
    fs::remove_file(&ready).unwrap();
    let restarted = client.ok(&["projects", "start", &project, &command, "--yes"], None);
    client.runs.push((
        project.clone(),
        command.clone(),
        restarted["run_id"].as_str().unwrap().into(),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let port_after_restart = loop {
        if let Ok(contents) = fs::read_to_string(&ready)
            && let Some((_, port)) = contents.split_once(':')
            && let Ok(port) = port.parse::<u16>()
        {
            break port;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    };
    let record: Value = serde_json::from_slice(
        &fs::read(
            client
                .data()
                .join("project-logs")
                .join(format!("persistent-{project}-{command}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    let socket = PathBuf::from(record["socket"].as_str().unwrap());
    let runner_pid: i32 = socket
        .parent()
        .unwrap()
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
    let deadline = Instant::now() + Duration::from_secs(25);
    while std::net::TcpStream::connect(("127.0.0.1", port_after_restart)).is_ok() {
        assert!(
            Instant::now() < deadline,
            "remote child survived runner SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(before, fs::read(client.data().join("hosts.json")).unwrap());
    client.runs.clear();
}

#[test]
fn persistent_project_cli_controls_only_the_observed_run() {
    project_cli_controls_only_the_observed_run("Persistent", "Service");
}

#[test]
fn app_task_cli_start_preserves_definition_and_controls_background_run() {
    project_cli_controls_only_the_observed_run("App", "Task");
}

fn project_cli_controls_only_the_observed_run(lifecycle: &str, kind: &str) {
    let mut client = Client::new();
    let helper =
        shell_words::quote(std::env::current_exe().unwrap().to_str().unwrap()).into_owned();
    let unrelated = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unrelated_port = unrelated.local_addr().unwrap().port();
    let created = client.ok(&["projects","create"], Some(json!({
        "name":"disposable","base_path":client.root,"commands":[{
            "name":"wait","command":format!("printf 'password=hidden-cli-secret\\n'; exec {helper} --ignored --exact disposable_listener --nocapture"),
            "kind":kind,"lifecycle":lifecycle
        }]
    })));
    let project = created["id"].as_str().unwrap().to_string();
    let config = client.ok(&["projects", "get", &project], None);
    let before = fs::read(client.data().join("hosts.json")).unwrap();
    let command = config["commands"][0]["id"].as_str().unwrap().to_string();
    client.error(
        &["projects", "start", &project, &command],
        None,
        4,
        "approval_required",
    );
    let run = client.ok(&["projects", "start", &project, &command, "--yes"], None);
    let run_id = run["run_id"].as_str().unwrap().to_string();
    client
        .runs
        .push((project.clone(), command.clone(), run_id.clone()));
    assert_eq!(run["execution_lifecycle"], "Persistent");
    assert_eq!(before, fs::read(client.data().join("hosts.json")).unwrap());
    assert_eq!(client.ok(&["projects", "get", &project], None), config);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client.ok(&["projects", "status"], None);
        let output = client.ok(&["projects", "output", &project, &command], None);
        if status[0]["run"]["active"] == true
            && output["output"]
                .as_str()
                .unwrap()
                .contains("listener-ready:")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "runner did not become active: {status}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    client.error(
        &[
            "projects", "stop", &project, &command, "--run-id", "1", "--yes",
        ],
        None,
        5,
        "conflict",
    );
    client.error(
        &["projects", "update", &project],
        Some(json!({"commands":[{"id":command,"command":"echo changed"}]})),
        5,
        "conflict",
    );
    client.error(
        &["projects", "start", &project, &command, "--yes"],
        None,
        1,
        "operation_failed",
    );
    let output = client
        .ok(&["projects", "output", &project, &command], None)
        .to_string();
    assert!(!output.contains("hidden-cli-secret"));
    assert!(!output.contains("\"token\""));
    assert!(!output.contains(".sock"));
    let runtime = client.ok(&["runtime", "list"], None);
    assert_eq!(runtime["target"], "Local");
    assert!(
        runtime["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["project"]["project_id"] == project)
    );
    client.ok(
        &[
            "runtime", "stop", &project, &command, "--run-id", &run_id, "--yes",
        ],
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client.ok(&["projects", "status"], None);
        if status[0]["run"]["active"] == false {
            break;
        }
        assert!(Instant::now() < deadline, "runner did not stop: {status}");
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", unrelated_port)).is_err(),
        "stopping a Project must preserve an unrelated listener"
    );
    client.runs.clear();
    assert_eq!(before, fs::read(client.data().join("hosts.json")).unwrap());
}

#[test]
#[ignore = "subprocess fixture launched by persistent_project_cli_controls_only_the_observed_run"]
fn disposable_listener() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    println!("listener-ready:{}", listener.local_addr().unwrap().port());
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(60));
    drop(listener);
}
