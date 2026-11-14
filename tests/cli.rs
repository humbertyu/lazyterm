use std::{
    fs,
    io::Write,
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
            "{}",
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
