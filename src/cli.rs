use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::PathBuf,
};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    hosts::{Host, HostCatalog},
    metadata::Metadata,
    projects::{ProjectDefinition, ProjectLifecycle, ProjectTarget, execution_target},
};

const INPUT_LIMIT: u64 = 1024 * 1024;
const HELP: &str = "\
LazyTerm configuration and runtime CLI

Usage: lazyterm <resource> <action> [id] [command-id] [options]
       lazyterm schema [--json]

Resources:
  hosts      list | get ALIAS | create | update ALIAS | tag ALIAS
  groups     list | create | update NAME
  snippets   list | get PACKAGE-UUID | create | update PACKAGE-UUID
  projects   list | get UUID | create | update UUID | status | output UUID COMMAND-UUID
             start UUID COMMAND-UUID --yes
             stop UUID COMMAND-UUID --run-id RUN-ID --yes
  tunnels    list | get UUID | create | update UUID
  runtime    list | stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes

Options:
  --version, -V          Application version
  --diagnostics          Redacted platform support report
  --json                 Versioned JSON (also the default for CLI results)
  --input FILE|-         JSON object; use - to read stdin. Required for configuration writes.
  --yes                  Authorize one start/stop action; does not authorize other actions.
  --run-id RUN-ID        Exact run identity returned by projects status/runtime list.

Updates are partial objects. Nested commands, links and snippets merge by UUID;
omit an ID to add a nested item. Omitted fields/items are preserved.
hosts tag accepts {\"groups\":[\"development\"]}; groups create accepts {\"name\":\"development\"}.
Run lazyterm schema --json for writable field names and examples.
No SSH login, Host exec, SFTP, password or private-key endpoints are provided.
Project start currently supports Local commands with Persistent lifecycle.
Run without arguments to open the TUI.
";

#[derive(Debug)]
struct Failure {
    code: &'static str,
    message: String,
}

type Result<T> = std::result::Result<T, Failure>;

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        let code = if error.kind() == io::ErrorKind::AlreadyExists
            || error.to_string().contains("changed")
        {
            "conflict"
        } else if error.kind() == io::ErrorKind::InvalidInput {
            "invalid_input"
        } else {
            "operation_failed"
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

fn invalid(message: impl Into<String>) -> Failure {
    fail("invalid_input", message)
}

#[derive(Default)]
struct Arguments {
    positional: Vec<String>,
    input: Option<String>,
    yes: bool,
    run_id: Option<u64>,
}

impl Arguments {
    fn parse(args: &[String]) -> Result<Self> {
        let mut parsed = Self::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--json" => {}
                "--yes" if !parsed.yes => parsed.yes = true,
                "--input" if parsed.input.is_none() => {
                    parsed.input = Some(
                        iter.next()
                            .ok_or_else(|| invalid("Missing --input value."))?
                            .clone(),
                    );
                }
                "--run-id" if parsed.run_id.is_none() => {
                    parsed.run_id = Some(
                        iter.next()
                            .ok_or_else(|| invalid("Missing --run-id value."))?
                            .parse()
                            .map_err(|_| invalid("Invalid --run-id."))?,
                    );
                }
                value if value.starts_with('-') => {
                    return Err(invalid(format!("Unknown or duplicate option: {value}")));
                }
                value => parsed.positional.push(value.into()),
            }
        }
        Ok(parsed)
    }

    fn id(&self, index: usize) -> Result<&str> {
        self.positional
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| invalid("Missing resource/action/identifier. See --help."))
    }

    fn uuid(&self, index: usize) -> Result<Uuid> {
        Uuid::parse_str(self.id(index)?).map_err(|_| invalid("Expected an entity UUID."))
    }

    fn payload(&self) -> Result<Value> {
        let path = self
            .input
            .as_deref()
            .ok_or_else(|| invalid("Specify --input FILE or --input - for JSON stdin."))?;
        let reader: Box<dyn Read> = if path == "-" {
            Box::new(io::stdin())
        } else {
            Box::new(fs::File::open(path)?)
        };
        let mut bytes = Vec::new();
        reader.take(INPUT_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > INPUT_LIMIT {
            return Err(invalid("JSON input exceeds 1 MiB."));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid JSON input."))?;
        if !value.is_object() {
            return Err(invalid("Expected a JSON object."));
        }
        reject_credentials(&value)?;
        Ok(value)
    }
}

pub(crate) fn run_if_requested() -> Option<i32> {
    if std::env::var_os("LAZYTERM_AUTH_SOCKET").is_some()
        || std::env::var_os("LAZYTERM_ASKPASS_REFERENCE").is_some()
    {
        return None;
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let first = args.first()?;
    if matches!(
        first.as_str(),
        "--version" | "-V" | "--diagnostics" | "--process-guard" | "--project-runner"
    ) {
        return None;
    }
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        print!("{HELP}");
        return Some(0);
    }
    let result = Arguments::parse(&args).and_then(|args| {
        let context = Context::current()?;
        dispatch(&context, &args)
    });
    match result {
        Ok(data) => {
            println!(
                "{}",
                json!({"schema_version":1, "ok":true, "data": redact(data)})
            );
            Some(0)
        }
        Err(error) => {
            let message = crate::diagnostics::redact_sensitive_text(&error.message);
            println!(
                "{}",
                json!({"schema_version":1, "ok":false, "error":{"code":error.code,"message":message}})
            );
            eprintln!("LazyTerm: {message}");
            Some(match error.code {
                "invalid_input" => 2,
                "not_found" => 3,
                "approval_required" => 4,
                "conflict" => 5,
                _ => 1,
            })
        }
    }
}

struct Context {
    home: PathBuf,
    data: PathBuf,
}

impl Context {
    fn current() -> Result<Self> {
        Ok(Self {
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| invalid("HOME is not set."))?,
            data: Metadata::data_dir()?,
        })
    }

    fn metadata(&self) -> Result<Metadata> {
        // Do not run migration/reconciliation as a side effect of a query.
        let metadata = Metadata::read_for_import(&self.data.join("hosts.json"))?;
        if metadata.version != 3 {
            return Err(invalid(
                "Open the TUI once to migrate the configuration before using the CLI.",
            ));
        }
        Ok(metadata)
    }

    fn catalog(&self) -> Result<HostCatalog> {
        let root = self.home.join(".ssh");
        let catalog = HostCatalog::load_from(&root.join("config"), &root);
        if let Some(error) = &catalog.error {
            return Err(fail("configuration_invalid", error.clone()));
        }
        Ok(catalog)
    }
}

fn dispatch(context: &Context, args: &Arguments) -> Result<Value> {
    let resource = args.id(0)?;
    if resource == "schema"
        && args.positional.len() == 1
        && args.input.is_none()
        && !args.yes
        && args.run_id.is_none()
    {
        return Ok(schema());
    }
    let action = args.id(1)?;
    let expected = match (resource, action) {
        ("hosts", "list" | "create")
        | ("groups", "list" | "create")
        | ("snippets" | "projects" | "tunnels", "list" | "create")
        | ("projects", "status")
        | ("runtime", "list") => 2,
        ("hosts", "get" | "update" | "tag")
        | ("groups", "update")
        | ("snippets" | "projects" | "tunnels", "get" | "update") => 3,
        ("projects", "start" | "stop" | "output") | ("runtime", "stop") => 4,
        _ => return Err(invalid("Unknown command. See lazyterm --help.")),
    };
    if args.positional.len() != expected {
        return Err(invalid("Unexpected positional arguments."));
    }
    let write = matches!(action, "create" | "update" | "tag");
    let control = matches!(action, "start" | "stop");
    if (args.input.is_some() && !write)
        || (args.yes && !control)
        || (args.run_id.is_some() && action != "stop")
    {
        return Err(invalid("An option is not valid for this action."));
    }
    if control && !args.yes {
        return Err(fail(
            "approval_required",
            "Start/stop requires explicit user authorization and --yes.",
        ));
    }
    match resource {
        "hosts" => hosts(context, args, action),
        "groups" => groups(context, args, action),
        "snippets" | "projects" | "tunnels" if write => {
            write_entity(context, args, resource, action)
        }
        "projects" if matches!(action, "status" | "output" | "start" | "stop") => {
            project_operation(context, args, action)
        }
        "runtime" if action == "stop" => project_operation(context, args, "stop"),
        "runtime" => runtime(context),
        _ => {
            let metadata = context.metadata()?;
            let value = metadata_collection(&metadata, resource)?;
            if action == "get" {
                let id = args.uuid(2)?.to_string();
                value
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|value| value["id"] == id)
                    .cloned()
                    .ok_or_else(|| fail("not_found", "Entity UUID not found."))
            } else {
                Ok(value)
            }
        }
    }
}

fn safe_link(link: &crate::projects::ProjectLink) -> Value {
    json!({"id":link.id,"name":link.name,"url":link.url,"username":link.username})
}

fn safe_project(project: &ProjectDefinition) -> Value {
    json!({
        "id":project.id,"name":project.name,"base_path":project.base_path,
        "target":project.target,"groups":project.groups,"commands":project.commands,
        "links":project.links.iter().map(safe_link).collect::<Vec<_>>(),
    })
}

fn metadata_collection(metadata: &Metadata, resource: &str) -> Result<Value> {
    Ok(match resource {
        "projects" => Value::Array(metadata.projects.iter().map(safe_project).collect()),
        "snippets" => json!(metadata.snippet_packages),
        "tunnels" => json!(metadata.tunnels),
        _ => return Err(invalid("Unknown collection.")),
    })
}

fn hosts(context: &Context, args: &Arguments, action: &str) -> Result<Value> {
    let mut catalog = context.catalog()?;
    let mut metadata = context.metadata()?;
    if matches!(action, "list" | "get") {
        let values = catalog
            .hosts
            .iter()
            .filter(|host| action == "list" || args.positional[2] == host.alias)
            .map(|host| {
                let identity = host
                    .identity_file
                    .as_deref()
                    .map(crate::credentials::expand_path);
                json!({
                    "alias":host.alias,"hostname":host.hostname,"user":host.user,"port":host.port,
                    "identity_file":host.identity_file,"proxy_jump":host.proxy_jump,
                    "configured_key_exists":identity.map(|path| path.is_file()),
                    "groups":metadata.tags.get(&host.alias).cloned().unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();
        return if action == "get" {
            values
                .into_iter()
                .next()
                .ok_or_else(|| fail("not_found", "Host alias not found."))
        } else {
            Ok(json!(values))
        };
    }
    let payload = args.payload()?;
    if action == "tag" {
        check_fields(&payload, &["groups"])?;
        if !catalog
            .hosts
            .iter()
            .any(|host| host.alias == args.positional[2])
        {
            return Err(fail("not_found", "Host alias not found."));
        }
        let names = string_list(
            payload
                .get("groups")
                .ok_or_else(|| invalid("groups is required."))?,
        )?;
        metadata.tags.insert(args.positional[2].clone(), names);
        metadata.normalize_host_groups();
        metadata.save_to(&context.data.join("hosts.json"))?;
        return Ok(json!({"alias":args.positional[2],"groups":metadata.tags[&args.positional[2]]}));
    }
    check_fields(
        &payload,
        &[
            "alias",
            "hostname",
            "user",
            "port",
            "identity_file",
            "proxy_jump",
        ],
    )?;
    let original = if action == "update" {
        Some(args.id(2)?)
    } else {
        None
    };
    let mut value = if let Some(alias) = original {
        let host = catalog
            .hosts
            .iter()
            .find(|host| host.alias == alias)
            .ok_or_else(|| fail("not_found", "Host alias not found."))?;
        json!(host)
    } else {
        json!({"alias":"","hostname":null,"user":null,"port":null,"identity_file":null,"proxy_jump":null,"managed":true})
    };
    merge_patch(&mut value, payload)?;
    let host: Host = serde_json::from_value(value).map_err(|_| invalid("Invalid Host fields."))?;
    if host.port == Some(0) {
        return Err(invalid("Host port must be between 1 and 65535."));
    }
    if original.is_some_and(|alias| host.alias != alias) {
        return Err(invalid(
            "Host alias rename is not supported by this CLI yet; use the TUI to migrate references.",
        ));
    }
    catalog.upsert_from(&host, original)?;
    Ok(json!({"alias":host.alias,"saved":true}))
}

fn groups(context: &Context, args: &Arguments, action: &str) -> Result<Value> {
    let mut metadata = context.metadata()?;
    if action == "list" {
        let mut names = metadata.host_groups.clone();
        names.extend(metadata.tags.values().flatten().cloned());
        names.sort();
        names.dedup();
        return Ok(json!(names));
    }
    let payload = args.payload()?;
    check_fields(&payload, &["name"])?;
    let name = required_string(&payload, "name")?.to_string();
    if action == "update" {
        let old = args.id(2)?;
        if !metadata.host_groups.iter().any(|group| group == old)
            && !metadata.tags.values().flatten().any(|group| group == old)
        {
            return Err(fail("not_found", "Group not found."));
        }
        if name != old && metadata.host_groups.contains(&name) {
            return Err(fail("conflict", "The new group name already exists."));
        }
        for group in metadata
            .host_groups
            .iter_mut()
            .chain(metadata.tags.values_mut().flatten())
        {
            if group == old {
                *group = name.clone();
            }
        }
    } else if metadata.host_groups.contains(&name) {
        return Err(fail("conflict", "Group already exists."));
    } else {
        metadata.host_groups.push(name.clone());
    }
    metadata.normalize_host_groups();
    metadata.save_to(&context.data.join("hosts.json"))?;
    Ok(json!({"name":name,"saved":true}))
}

fn write_entity(
    context: &Context,
    args: &Arguments,
    resource: &str,
    action: &str,
) -> Result<Value> {
    let mut metadata = context.metadata()?;
    let original_projects = metadata.projects.clone();
    let mut payload = args.payload()?;
    validate_payload(&payload, resource)?;
    let existing_id = if action == "update" {
        Some(args.uuid(2)?)
    } else {
        None
    };
    let mut collection = match resource {
        "projects" => json!(metadata.projects),
        "snippets" => json!(metadata.snippet_packages),
        _ => json!(metadata.tunnels),
    };
    let items = collection.as_array_mut().unwrap();
    let id = existing_id.unwrap_or_else(Uuid::new_v4);
    if let Some(supplied) = payload.get("id")
        && (existing_id.is_none() || supplied.as_str() != Some(id.to_string().as_str()))
    {
        return Err(invalid(
            "Top-level IDs are assigned on create and cannot change on update.",
        ));
    }
    payload["id"] = json!(id);
    assign_child_ids(&mut payload)?;
    if let Some(existing) = existing_id {
        let item = items
            .iter_mut()
            .find(|item| item["id"] == existing.to_string())
            .ok_or_else(|| fail("not_found", "Entity UUID not found."))?;
        merge_patch(item, payload)?;
    } else {
        items.push(payload);
    }
    match resource {
        "projects" => {
            metadata.projects = serde_json::from_value(collection)
                .map_err(|_| invalid("Invalid Project definition; see schema."))?;
            let changed = metadata
                .projects
                .iter()
                .find(|project| project.id == id)
                .unwrap();
            validate_projects(std::slice::from_ref(changed), &context.catalog()?)?;
            for previous in &original_projects {
                if let Some(updated) = metadata
                    .projects
                    .iter()
                    .find(|project| project.id == previous.id)
                    && updated != previous
                {
                    for command in &previous.commands {
                        let affects_run = updated.base_path != previous.base_path
                            || updated.target != previous.target
                            || updated.commands.iter().find(|item| item.id == command.id)
                                != Some(command);
                        if affects_run
                            && crate::project_runner::inspect(
                                &context.data.join("project-logs"),
                                (previous.id, command.id),
                            )?
                            .is_some_and(|snapshot| snapshot.active())
                        {
                            return Err(fail(
                                "conflict",
                                "Stop the active command before changing its definition.",
                            ));
                        }
                    }
                }
            }
        }
        "snippets" => {
            metadata.snippet_packages = serde_json::from_value(collection)
                .map_err(|_| invalid("Invalid Snippet package; see schema."))?;
            for package in metadata
                .snippet_packages
                .iter()
                .filter(|package| package.id == id)
            {
                nonempty(&package.name)?;
                for snippet in &package.snippets {
                    nonempty(&snippet.name)?;
                    nonempty(&snippet.command)?;
                }
            }
        }
        _ => {
            metadata.tunnels = serde_json::from_value(collection)
                .map_err(|_| invalid("Invalid Tunnel definition; see schema."))?;
            let catalog = context.catalog()?;
            for tunnel in metadata.tunnels.iter().filter(|tunnel| tunnel.id == id) {
                validate_tunnel(tunnel, &catalog)?;
            }
        }
    }
    metadata.save_to(&context.data.join("hosts.json"))?;
    Ok(json!({"id":id,"saved":true}))
}

fn check_fields(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("Expected an object."))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(
            "Unknown or unsupported input field. See lazyterm schema.",
        ));
    }
    Ok(())
}

fn validate_payload(value: &Value, kind: &str) -> Result<()> {
    let allowed: &[&str] = match kind {
        "projects" => &[
            "id",
            "name",
            "base_path",
            "target",
            "groups",
            "commands",
            "links",
        ],
        "commands" => &[
            "id",
            "name",
            "command",
            "working_directory",
            "group",
            "kind",
            "target",
            "lifecycle",
        ],
        "links" => &["id", "name", "url", "username"],
        "snippets" => &["id", "name", "description", "snippets"],
        "snippet" => &["id", "name", "description", "command", "dangerous"],
        "tunnels" => &[
            "id",
            "label",
            "host_alias",
            "custom_host",
            "kind",
            "bind_address",
            "local_port",
            "remote_host",
            "remote_port",
            "lifecycle",
            "auto_reconnect",
            "connect_timeout_secs",
            "keepalive_interval_secs",
        ],
        _ => return Err(invalid("Unknown definition type.")),
    };
    check_fields(value, allowed)?;
    for field in ["commands", "links", "snippets"] {
        if let Some(items) = value.get(field) {
            for item in items
                .as_array()
                .ok_or_else(|| invalid("Nested items must be arrays."))?
            {
                validate_payload(
                    item,
                    if field == "snippets" {
                        "snippet"
                    } else {
                        field
                    },
                )?;
            }
        }
    }
    Ok(())
}

fn assign_child_ids(value: &mut Value) -> Result<()> {
    for field in ["commands", "links", "snippets"] {
        if let Some(items) = value.get_mut(field).and_then(Value::as_array_mut) {
            let mut ids = BTreeSet::new();
            for item in items {
                let id = match item.get("id") {
                    Some(id) => {
                        Uuid::parse_str(id.as_str().ok_or_else(|| invalid("Invalid UUID."))?)
                            .map_err(|_| invalid("Invalid UUID."))?
                    }
                    None => Uuid::new_v4(),
                };
                if id.is_nil() || !ids.insert(id) {
                    return Err(invalid("Duplicate or nil child UUID."));
                }
                item["id"] = json!(id);
            }
        }
    }
    Ok(())
}

// Child arrays merge by UUID so recording one command never erases siblings or credentials.
fn merge_patch(existing: &mut Value, incoming: Value) -> Result<()> {
    let object = incoming
        .as_object()
        .ok_or_else(|| invalid("Expected an object."))?;
    for (key, value) in object {
        if matches!(key.as_str(), "commands" | "links" | "snippets") {
            if existing.get(key).is_none() {
                existing[key] = json!([]);
            }
            let items = existing[key]
                .as_array_mut()
                .ok_or_else(|| invalid("Invalid existing collection."))?;
            for incoming in value
                .as_array()
                .ok_or_else(|| invalid("Expected an array."))?
            {
                if let Some(item) = items.iter_mut().find(|item| item["id"] == incoming["id"]) {
                    merge_patch(item, incoming.clone())?;
                } else {
                    items.push(incoming.clone());
                }
            }
        } else {
            existing[key] = value.clone();
        }
    }
    Ok(())
}

fn nonempty(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.contains('\0') {
        Err(invalid("Required text cannot be blank or contain NUL."))
    } else {
        Ok(())
    }
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{key} must be a string.")))?;
    nonempty(text)?;
    Ok(text)
}

fn string_list(value: &Value) -> Result<Vec<String>> {
    let names: Vec<String> =
        serde_json::from_value(value.clone()).map_err(|_| invalid("Expected a string array."))?;
    for name in &names {
        nonempty(name)?;
    }
    Ok(names
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn validate_target(target: &ProjectTarget, catalog: &HostCatalog) -> Result<()> {
    if let ProjectTarget::Host(alias) = target
        && !catalog.hosts.iter().any(|host| host.alias == *alias)
    {
        return Err(invalid("Project target Host alias does not exist."));
    }
    Ok(())
}

fn validate_projects(projects: &[ProjectDefinition], catalog: &HostCatalog) -> Result<()> {
    for project in projects {
        nonempty(&project.name)?;
        nonempty(&project.base_path.to_string_lossy())?;
        if !project.base_path.is_absolute() && !project.base_path.starts_with("~") {
            return Err(invalid(
                "Project base_path must be absolute or start with ~/.",
            ));
        }
        validate_target(&project.target, catalog)?;
        for command in &project.commands {
            nonempty(&command.name)?;
            nonempty(&command.command)?;
            validate_target(execution_target(project, command), catalog)?;
            if command.lifecycle == ProjectLifecycle::Persistent
                && *execution_target(project, command) != ProjectTarget::Local
            {
                return Err(invalid(
                    "Persistent commands currently require a Local target.",
                ));
            }
        }
        for link in &project.links {
            nonempty(&link.name)?;
            crate::projects::validate_link_url(&link.url).map_err(invalid)?;
        }
    }
    Ok(())
}

fn validate_tunnel(
    tunnel: &crate::metadata::TunnelDefinition,
    catalog: &HostCatalog,
) -> Result<()> {
    use crate::metadata::TunnelKind;
    nonempty(&tunnel.label)?;
    nonempty(&tunnel.host_alias)?;
    if !tunnel.custom_host
        && !catalog
            .hosts
            .iter()
            .any(|host| host.alias == tunnel.host_alias)
    {
        return Err(invalid("Tunnel Host alias does not exist."));
    }
    if tunnel.host_alias.starts_with('-')
        || tunnel.host_alias.chars().any(char::is_whitespace)
        || tunnel.local_port == 0
        || tunnel.bind_address.parse::<std::net::IpAddr>().is_err()
        || tunnel.connect_timeout_secs == 0
    {
        return Err(invalid("Invalid Tunnel address, port or timeout."));
    }
    if tunnel.kind != TunnelKind::Dynamic
        && (tunnel.remote_port.is_none_or(|port| port == 0)
            || tunnel.remote_host.as_deref().is_none_or(|host| {
                host.trim().is_empty()
                    || host.starts_with('-')
                    || host.chars().any(char::is_whitespace)
            }))
    {
        return Err(invalid(
            "Local/Remote Tunnel requires a valid destination host and port.",
        ));
    }
    Ok(())
}

fn snapshot_value(snapshot: &crate::project_runner::Snapshot) -> Value {
    json!({"project_id":snapshot.owner.key.0,"command_id":snapshot.owner.key.1,
        "run_id":snapshot.owner.run_id.to_string(),"process_group":snapshot.owner.process_group,
        "active":snapshot.active(),"status":snapshot.status.label(),"directory":snapshot.directory})
}

fn project_operation(context: &Context, args: &Arguments, action: &str) -> Result<Value> {
    let metadata = context.metadata()?;
    let root = context.data.join("project-logs");
    if action == "status" {
        let mut result = Vec::new();
        for project in &metadata.projects {
            for command in &project.commands {
                let key = (project.id, command.id);
                let observed = crate::project_runner::inspect(&root, key);
                let state = match observed {
                    Ok(Some(snapshot)) => snapshot_value(&snapshot),
                    Ok(None) => json!({"status":"unobserved","active":null}),
                    Err(_) => {
                        json!({"status":"unavailable","active":null,"error":"Runner unavailable; use the TUI recovery workflow."})
                    }
                };
                result.push(json!({"project_id":project.id,"command_id":command.id,
                    "project":project.name,"command":command.name,
                    "target":execution_target(project, command),"run":state}));
            }
        }
        return Ok(json!(result));
    }
    let key = (args.uuid(2)?, args.uuid(3)?);
    let project = metadata
        .projects
        .iter()
        .find(|project| project.id == key.0)
        .ok_or_else(|| fail("not_found", "Project UUID not found."))?;
    let command = project
        .commands
        .iter()
        .find(|command| command.id == key.1)
        .ok_or_else(|| fail("not_found", "Command UUID not found."))?;
    if *execution_target(project, command) != ProjectTarget::Local {
        return Err(fail(
            "unsupported",
            "CLI lifecycle/output currently supports Local runner commands only; use the TUI for Host-targeted Projects.",
        ));
    }
    match action {
        "start" => {
            if command.lifecycle != ProjectLifecycle::Persistent {
                return Err(fail(
                    "unsupported",
                    "CLI start requires Persistent lifecycle so the command survives CLI exit.",
                ));
            }
            nonempty(&command.command)?;
            crate::projects::working_directory(project, command)?;
            let handle = crate::project_runner::start(&root, project, command)?;
            Ok(snapshot_value(&handle.snapshot))
        }
        "stop" => {
            let run_id = args
                .run_id
                .ok_or_else(|| invalid("--run-id is required; query status first."))?;
            Ok(snapshot_value(&crate::project_runner::stop_observed(
                &root, key, run_id,
            )?))
        }
        "output" => {
            let snapshot = crate::project_runner::inspect(&root, key)?
                .ok_or_else(|| fail("not_found", "No runner output exists."))?;
            let text = crate::diagnostics::redact_sensitive_text(&snapshot.output);
            let chars = text.chars().collect::<Vec<_>>();
            let start = chars.len().saturating_sub(16 * 1024);
            Ok(
                json!({"run":snapshot_value(&snapshot),"output":chars[start..].iter().collect::<String>(),"truncated":start > 0}),
            )
        }
        _ => Err(invalid("Unknown Project action.")),
    }
}

fn runtime(context: &Context) -> Result<Value> {
    let metadata = context.metadata()?;
    let mut owners = Vec::new();
    for project in &metadata.projects {
        for command in &project.commands {
            if *execution_target(project, command) == ProjectTarget::Local
                && let Ok(Some(snapshot)) = crate::project_runner::inspect(
                    &context.data.join("project-logs"),
                    (project.id, command.id),
                )
                && snapshot.active()
            {
                owners.push(snapshot.owner);
            }
        }
    }
    let entries = crate::runtime::inspect(&crate::runtime::RuntimeTarget::Local, &owners, &[])
        .map_err(|message| fail("inspection_failed", message))?;
    Ok(
        json!({"target":"Local","entries":entries.iter().map(|entry| json!({
        "protocol":entry.protocol,"address":entry.address,"port":entry.port,"pid":entry.pid,
        "process":entry.process,"user":entry.user,"command":entry.command,
        "working_directory":entry.working_directory,"origin":entry.origin.label(),
        "origin_reason":entry.origin_reason,"kind":entry.kind.label(),
        "project":entry.project.as_ref().map(|owner| json!({"project_id":owner.key.0,"command_id":owner.key.1,"run_id":owner.run_id.to_string()})),
    })).collect::<Vec<_>>()}),
    )
}

fn reject_credentials(value: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "password"
                        | "password_ref"
                        | "password_storage"
                        | "private_key"
                        | "token"
                        | "credentials"
                ) {
                    return Err(invalid(
                        "Credential fields are not accepted by the Agent CLI; use the TUI.",
                    ));
                }
                reject_credentials(value)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_credentials(item)?;
            }
        }
        Value::String(text) if text.contains("PRIVATE KEY-----") => {
            return Err(invalid("Private key material is not accepted."));
        }
        _ => {}
    }
    Ok(())
}

fn redact(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(crate::diagnostics::redact_sensitive_text(&text)),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, redact(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        value => value,
    }
}

fn schema() -> Value {
    json!({
        "version":1,"transport":"cli","output":"JSON envelope: schema_version, ok, data or error",
        "exit_codes":{"0":"success","1":"operation failure","2":"invalid input","3":"not found","4":"approval required","5":"concurrent change/conflict"},
        "input":"--input FILE or --input -; partial updates; nested items merge by UUID; no deletion endpoints",
        "hosts":{"alias":"required on create","hostname":"string|null","user":"string|null","port":"1..65535|null","identity_file":"path|null","proxy_jump":"alias/address|null"},
        "host_tags":{"groups":["development"]},
        "groups":{"name":"development"},
        "projects":{"name":"example","base_path":"~/code/example","target":"Local","groups":["Development"],
            "commands":[{"name":"start","command":"npm run start","kind":"Service","lifecycle":"Persistent","group":"Development"}],
            "links":[{"name":"Home","url":"http://localhost:3000/","username":""}]},
        "snippets":{"name":"Operations","description":"","snippets":[{"name":"Health","command":"uptime","dangerous":false}]},
        "tunnels":{"label":"database","host_alias":"example","custom_host":false,"kind":"Local","bind_address":"127.0.0.1","local_port":13306,
            "remote_host":"127.0.0.1","remote_port":3306,"lifecycle":"App","auto_reconnect":false},
        "targets":["Local",{"Host":"existing-host-alias"}],
        "read_scope":"Hosts, groups, Projects, Snippet templates, Tunnel definitions, Local Runtime and managed runner status/output",
        "execution_scope":"Local Persistent Project start/stop; runtime stop is the same managed-run operation; --yes authorizes one action",
        "excluded":["SSH login","Host exec","SFTP","SSH key deployment","credential access","MCP","arbitrary PID termination"]
    })
}
