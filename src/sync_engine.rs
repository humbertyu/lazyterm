use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    hosts::HostCatalog,
    metadata::Metadata,
    sync::{SyncPlan, SyncSettings, SyncSnapshot, plan_sync},
};

const MANIFEST: &str = "lazyterm-sync.json";
const SOURCE_ROOT: &str = "/lazyterm-sync/.ssh";
const ENTITY_FIELDS: &[&str] = &[
    "projects",
    "commands",
    "links",
    "snippet_packages",
    "snippets",
    "tunnels",
];
const FIELDS: &[&str] = &[
    "tags",
    "host_groups",
    "tunnels",
    "snippet_packages",
    "projects",
    "theme",
    "language",
    "hidden_tabs",
    "show_hidden_files",
];
const SENSITIVE_SSH_VALUE: &str = r"(?i)(?:password|token|secret|api[_-]?key)\s*[:=]\s*\S+|--(?:password|token|secret)\s+\S+|://[^/\s:@]+:[^/\s@]+@|sshpass\s+-p\s+\S+";

fn contains_inline_secret(text: &str) -> bool {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| regex::Regex::new(SENSITIVE_SSH_VALUE).unwrap());
    text.lines().any(|line| pattern.is_match(line))
}

pub struct Capture {
    pub metadata: Metadata,
    pub ssh: BTreeMap<String, String>,
    local_only_includes: BTreeMap<PathBuf, Vec<(usize, String)>>,
    pub snapshot: SyncSnapshot,
    pub home: PathBuf,
    pub metadata_path: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
struct Baseline {
    remote: String,
    branch: String,
    snapshot: SyncSnapshot,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    snapshot: SyncSnapshot,
}

pub struct Prepared {
    pub capture: Capture,
    pub plan: SyncPlan,
    pub conflict_selection: usize,
    settings: SyncSettings,
    checkout: PathBuf,
    remote_revision: Option<String>,
    remote_snapshot: SyncSnapshot,
    _lock: fs::File,
}

pub enum SyncResult {
    Prepared(Box<Prepared>),
    Applied(Box<Metadata>),
}

pub struct Worker<T> {
    pub receiver: Receiver<Result<T, String>>,
}

impl<T: Send + 'static> Worker<T> {
    pub fn start(work: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("lazyterm-sync".into())
            .spawn(move || {
                let result = work()
                    .map_err(|error| crate::diagnostics::redact_sensitive_text(&error.to_string()));
                let _ = sender.send(result);
            })?;
        Ok(Self { receiver })
    }
}

pub fn prepare(
    settings: SyncSettings,
    home: PathBuf,
    metadata_path: PathBuf,
    directory: PathBuf,
) -> io::Result<Worker<SyncResult>> {
    Worker::start(move || {
        let capture = Capture::read(home, metadata_path)?;
        let prepared = Prepared::prepare(settings, capture, &directory)?;
        Ok(SyncResult::Prepared(Box::new(prepared)))
    })
}

pub fn apply(prepared: Prepared) -> io::Result<Worker<SyncResult>> {
    Worker::start(move || {
        prepared
            .apply_and_push()
            .map(|metadata| SyncResult::Applied(Box::new(metadata)))
    })
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

fn secure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl Capture {
    pub fn read(home: PathBuf, metadata_path: PathBuf) -> io::Result<Self> {
        let metadata = Metadata::read_from(&metadata_path)?;
        let ssh_root = home.join(".ssh");
        let ssh = HostCatalog::snapshot_from(&ssh_root.join("config"), &ssh_root)?;
        let mut records = BTreeMap::new();
        let document = serde_json::to_value(&metadata)?;
        for field in FIELDS {
            let value = normalize(document[*field].clone(), field, &home)?;
            records.insert(format!("metadata:{field}"), value);
        }
        let canonical = fs::canonicalize(&ssh_root).unwrap_or(ssh_root.clone());
        let mut local_only_includes = BTreeMap::new();
        for (path, text) in &ssh {
            let path = Path::new(path);
            let Ok(relative) = path.strip_prefix(&canonical) else {
                continue;
            };
            let (text, local_includes) =
                crate::import_export::strip_external_include_directives(text, &ssh_root)?;
            if !local_includes.is_empty() {
                local_only_includes.insert(relative.to_path_buf(), local_includes);
            }
            let text = crate::import_export::rewrite_absolute_includes(
                &text,
                &canonical,
                Path::new(SOURCE_ROOT),
            )?;
            if text.contains("PRIVATE KEY-----") || contains_inline_secret(&text) {
                return Err(invalid(
                    "SSH configuration may contain inline credentials or private key material; sync blocked.",
                ));
            }
            let mut sections = serde_json::Map::new();
            let mut occurrences = BTreeMap::<String, usize>::new();
            for (position, (_, text)) in crate::import_export::ssh_sections(&text)?
                .into_iter()
                .enumerate()
            {
                let header = if position == 0 {
                    "preamble".to_string()
                } else {
                    text.lines().next().unwrap_or_default().trim().to_string()
                };
                let occurrence = occurrences.entry(header.clone()).or_default();
                let key = format!("{header}#{occurrence}");
                *occurrence += 1;
                sections.insert(key, json!({"position": position, "text": text}));
            }
            records.insert(
                format!("ssh:{}", relative.to_string_lossy()),
                Value::Object(sections),
            );
        }
        // Always represent the root so deleting the final Host can propagate.
        records.entry("ssh:config".into()).or_insert(json!({}));
        Ok(Self {
            metadata,
            ssh,
            local_only_includes,
            snapshot: SyncSnapshot { records },
            home,
            metadata_path,
        })
    }
}

fn normalize(mut value: Value, field: &str, home: &Path) -> io::Result<Value> {
    if ENTITY_FIELDS.contains(&field) {
        let items = value
            .as_array()
            .ok_or_else(|| invalid("Invalid entity collection."))?;
        let mut entities = serde_json::Map::new();
        for item in items {
            let id = item["id"]
                .as_str()
                .ok_or_else(|| invalid("Missing sync UUID."))?;
            uuid::Uuid::parse_str(id).map_err(|_| invalid("Invalid sync UUID."))?;
            if entities
                .insert(id.into(), normalize(item.clone(), "", home)?)
                .is_some()
            {
                return Err(invalid("Duplicate sync UUID."));
            }
        }
        return Ok(Value::Object(entities));
    }
    if field == "host_groups" {
        return Ok(Value::Object(
            value
                .as_array()
                .ok_or_else(|| invalid("Invalid Host groups."))?
                .iter()
                .map(|name| {
                    Ok((
                        name.as_str()
                            .ok_or_else(|| invalid("Invalid group name."))?
                            .into(),
                        json!(true),
                    ))
                })
                .collect::<io::Result<_>>()?,
        ));
    }
    if let Some(object) = value.as_object_mut() {
        for secret in ["password", "password_ref", "password_storage"] {
            object.remove(secret);
        }
        for (key, child) in object.iter_mut() {
            if matches!(key.as_str(), "base_path" | "working_directory")
                && let Some(path) = child.as_str()
                && let Ok(relative) = Path::new(path).strip_prefix(home)
            {
                *child = Value::String(format!("~/{}", relative.display()));
            }
            *child = normalize(child.take(), key, home)?;
        }
    } else if let Some(items) = value.as_array_mut() {
        for item in items {
            *item = normalize(item.take(), "", home)?;
        }
    }
    Ok(value)
}

fn expand(value: &Value, field: &str, local: &Value) -> io::Result<Value> {
    if matches!(field, "base_path" | "working_directory")
        && let Some(path) = value.as_str()
        && let Some(relative) = path.strip_prefix("~/")
    {
        let local_home = std::env::var_os("HOME").unwrap_or_default();
        return Ok(Value::String(
            PathBuf::from(local_home)
                .join(relative)
                .display()
                .to_string(),
        ));
    }
    if ENTITY_FIELDS.contains(&field) {
        let entities = value
            .as_object()
            .ok_or_else(|| invalid("Invalid sync entities."))?;
        let mut ids = local
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["id"].as_str())
            .filter(|id| entities.contains_key(*id))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for id in entities.keys() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        return Ok(Value::Array(
            ids.iter()
                .map(|id| {
                    let original = local
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|item| item["id"] == *id)
                        .unwrap_or(&Value::Null);
                    if entities[id]["id"].as_str() != Some(id) {
                        return Err(invalid("Sync entity key does not match its UUID."));
                    }
                    expand(&entities[id], "", original)
                })
                .collect::<io::Result<_>>()?,
        ));
    }
    if field == "host_groups" {
        return Ok(Value::Array(
            value
                .as_object()
                .ok_or_else(|| invalid("Invalid sync groups."))?
                .keys()
                .map(|name| json!(name))
                .collect(),
        ));
    }
    if let Some(object) = value.as_object() {
        let mut result = serde_json::Map::new();
        for (key, child) in object {
            if matches!(
                key.as_str(),
                "password" | "password_ref" | "password_storage"
            ) {
                return Err(invalid("Remote manifest contains credential fields."));
            }
            result.insert(key.clone(), expand(child, key, &local[key])?);
        }
        if object.contains_key("url") {
            crate::projects::validate_link_url(object["url"].as_str().unwrap_or_default())
                .map_err(|_| invalid("Remote manifest contains an invalid Web URL."))?;
            for key in ["password", "password_ref", "password_storage"] {
                if let Some(value) = local.get(key) {
                    result.insert(key.into(), value.clone());
                }
            }
        }
        return Ok(Value::Object(result));
    }
    Ok(value.clone())
}

impl Prepared {
    pub fn prepare(settings: SyncSettings, capture: Capture, directory: &Path) -> io::Result<Self> {
        settings.validate().map_err(|error| invalid(&error))?;
        secure_dir(directory)?;
        let lock_path = directory.join("sync.lock");
        let mut options = fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(lock_path)?;
        lock.try_lock()
            .map_err(|_| invalid("Another synchronization is already running."))?;
        let checkout = directory.join("repository");
        secure_dir(&checkout)?;
        git(&checkout, &["init", "--quiet"], None)?;
        let reference = format!("refs/heads/{}", settings.branch.trim());
        git(&checkout, &["check-ref-format", &reference], None)?;
        let remote = settings.remote.trim();
        let refs = git(
            &checkout,
            &["ls-remote", "--heads", "--", remote, &reference],
            None,
        )?;
        let remote_revision = if refs.is_empty() {
            None
        } else {
            git(
                &checkout,
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--",
                    remote,
                    &reference,
                ],
                None,
            )?;
            Some(
                git(&checkout, &["rev-parse", "FETCH_HEAD"], None)?
                    .trim()
                    .to_string(),
            )
        };
        let remote_snapshot = if let Some(revision) = &remote_revision {
            let files = git(&checkout, &["ls-tree", "--name-only", revision], None)?;
            if files.lines().any(|name| name != MANIFEST) {
                return Err(invalid(
                    "The selected branch contains files other than the LazyTerm manifest. Use a dedicated repository or branch.",
                ));
            }
            if files.is_empty() {
                SyncSnapshot::default()
            } else {
                let raw = git(
                    &checkout,
                    &["show", &format!("{revision}:{MANIFEST}")],
                    None,
                )?;
                if raw.len() > 16 * 1024 * 1024 {
                    return Err(invalid("Remote sync manifest is too large."));
                }
                let manifest: Manifest = serde_json::from_str(&raw)?;
                if manifest.format_version != 1 {
                    return Err(invalid("Unsupported sync manifest version."));
                }
                reject_sensitive_content(&manifest.snapshot)?;
                manifest.snapshot
            }
        } else {
            SyncSnapshot::default()
        };
        let baseline_path = directory.join("baseline.json");
        let baseline = match fs::read(&baseline_path) {
            Ok(bytes) => serde_json::from_slice::<Baseline>(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Baseline::default(),
            Err(error) => return Err(error),
        };
        let base = if baseline.remote == settings.remote && baseline.branch == settings.branch {
            baseline.snapshot
        } else {
            SyncSnapshot::default()
        };
        let plan = plan_sync(&base, &capture.snapshot, &remote_snapshot);
        let prepared = Self {
            capture,
            plan,
            conflict_selection: 0,
            settings,
            checkout,
            remote_revision,
            remote_snapshot,
            _lock: lock,
        };
        prepared.validate_snapshot(&prepared.plan.merged)?;
        Ok(prepared)
    }

    pub fn resolve_selected(&mut self, use_remote: bool) -> io::Result<()> {
        if self.plan.conflicts.is_empty() {
            return Ok(());
        }
        let conflict = &self.plan.conflicts[self.conflict_selection];
        let value = if use_remote {
            &conflict.remote
        } else {
            &conflict.local
        };
        let mut merged = self.plan.merged.clone();
        let mut parts = conflict
            .path
            .split('/')
            .skip(1)
            .map(|part| part.replace("~1", "/").replace("~0", "~"))
            .collect::<Vec<_>>();
        let record = parts.remove(0);
        if parts.is_empty() {
            match value {
                Some(value) => {
                    merged.insert(record, value.clone());
                }
                None => {
                    merged.remove(&record);
                }
            }
        } else {
            let mut current = merged
                .get_mut(&record)
                .ok_or_else(|| invalid("Invalid conflict path."))?;
            let last = parts.pop().unwrap();
            for part in parts {
                current = current
                    .get_mut(&part)
                    .ok_or_else(|| invalid("Invalid conflict path."))?;
            }
            let object = current
                .as_object_mut()
                .ok_or_else(|| invalid("Invalid conflict value."))?;
            match value {
                Some(value) => {
                    object.insert(last, value.clone());
                }
                None => {
                    object.remove(&last);
                }
            }
        }
        if self.plan.conflicts.len() == 1 {
            self.validate_snapshot(&merged)?;
        }
        let index = self.conflict_selection;
        self.plan.merged = merged;
        self.plan.conflicts.remove(index);
        self.conflict_selection = index.min(self.plan.conflicts.len().saturating_sub(1));
        Ok(())
    }

    fn archive(&self, records: &BTreeMap<String, Value>) -> io::Result<Value> {
        let mut metadata = serde_json::to_value(&self.capture.metadata)?;
        for field in FIELDS {
            if let Some(value) = records.get(&format!("metadata:{field}")) {
                metadata[*field] = expand(value, field, &metadata[*field])?;
            } else {
                return Err(invalid(
                    "Sync manifest is missing a required metadata collection.",
                ));
            }
        }
        let mut files = BTreeMap::new();
        for (key, value) in records {
            if let Some(relative) = key.strip_prefix("ssh:") {
                let mut parts = value
                    .as_object()
                    .ok_or_else(|| invalid("Invalid SSH sections."))?
                    .iter()
                    .map(|(key, section)| {
                        Ok((
                            section["position"]
                                .as_u64()
                                .ok_or_else(|| invalid("Invalid SSH position."))?,
                            key,
                            section["text"]
                                .as_str()
                                .ok_or_else(|| invalid("Invalid SSH text."))?,
                        ))
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                parts.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
                let text = parts
                    .into_iter()
                    .map(|(_, _, text)| text)
                    .collect::<String>();
                if text.contains("PRIVATE KEY-----") {
                    return Err(invalid(
                        "Private key material is forbidden in a sync manifest.",
                    ));
                }
                files.insert(format!("{SOURCE_ROOT}/{relative}"), text);
            } else if !FIELDS
                .iter()
                .any(|field| key == &format!("metadata:{field}"))
            {
                return Err(invalid("Unsupported record in sync manifest."));
            }
        }
        // Deleted Include files are emptied transactionally; their old stanzas
        // must not survive merely because the file stopped matching an Include.
        for path in self.capture.ssh.keys() {
            let root = fs::canonicalize(self.capture.home.join(".ssh"))
                .unwrap_or(self.capture.home.join(".ssh"));
            let Ok(relative) = Path::new(path).strip_prefix(&root) else {
                continue;
            };
            files
                .entry(format!("{SOURCE_ROOT}/{}", relative.display()))
                .or_default();
        }
        // The parser checks credential leakage in exports; retain local secrets
        // only after validation, in the transaction below.
        for project in metadata["projects"].as_array_mut().into_iter().flatten() {
            for link in project["links"].as_array_mut().into_iter().flatten() {
                link.as_object_mut().unwrap().remove("password");
            }
        }
        Ok(json!({
            "format_version": 1,
            "ssh_config_path": format!("{SOURCE_ROOT}/config"),
            "ssh_config": files.get(&format!("{SOURCE_ROOT}/config")).cloned().unwrap_or_default(),
            "ssh_config_files": files,
            "passwords_included": false,
            "metadata": metadata,
        }))
    }

    fn validate_snapshot(&self, records: &BTreeMap<String, Value>) -> io::Result<()> {
        reject_sensitive_content(&SyncSnapshot {
            records: records.clone(),
        })?;
        let archive = self.archive(records)?;
        crate::import_export::parse_configuration(
            &serde_json::to_vec(&archive)?,
            &self.capture.home,
        )?;
        Ok(())
    }

    pub fn apply_and_push(self) -> io::Result<Metadata> {
        if !self.plan.conflicts.is_empty() {
            return Err(invalid(
                "Resolve synchronization conflicts before applying.",
            ));
        }
        let archive = self.archive(&self.plan.merged)?;
        let mut import = crate::import_export::parse_configuration(
            &serde_json::to_vec(&archive)?,
            &self.capture.home,
        )?;
        // Reattach secrets strictly by UUID, never by label or alias.
        for project in &mut import.metadata.projects {
            for link in &mut project.links {
                if let Some(original) = self
                    .capture
                    .metadata
                    .projects
                    .iter()
                    .find(|item| item.id == project.id)
                    .and_then(|item| item.links.iter().find(|item| item.id == link.id))
                {
                    link.password = original.password.clone();
                    link.password_ref = original.password_ref.clone();
                    link.password_storage = original.password_storage;
                }
            }
        }
        let snapshot = SyncSnapshot {
            records: self.plan.merged,
        };
        if snapshot != self.remote_snapshot || self.remote_revision.is_none() {
            let manifest = serde_json::to_vec_pretty(&Manifest {
                format_version: 1,
                snapshot: snapshot.clone(),
            })?;
            let hash = git(
                &self.checkout,
                &["hash-object", "-w", "--stdin"],
                Some(&manifest),
            )?;
            // Use a temporary index, never checkout files from the remote tree.
            let _ = fs::remove_file(self.checkout.join("sync-index"));
            let empty = "--empty";
            git(
                &self.checkout,
                &[
                    "read-tree",
                    self.remote_revision.as_deref().unwrap_or(empty),
                ],
                None,
            )?;
            git(
                &self.checkout,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    "100644",
                    hash.trim(),
                    MANIFEST,
                ],
                None,
            )?;
            let tree = git(&self.checkout, &["write-tree"], None)?;
            let mut arguments = vec!["commit-tree", tree.trim()];
            if let Some(revision) = &self.remote_revision {
                arguments.extend(["-p", revision]);
            }
            let commit = git(
                &self.checkout,
                &arguments,
                Some(b"Synchronize LazyTerm configuration\n"),
            )?;
            let source = format!(
                "{}:refs/heads/{}",
                commit.trim(),
                self.settings.branch.trim()
            );
            git(&self.checkout, &["push", "--quiet", "--", self.settings.remote.trim(), &source], None)
                .map_err(|_| invalid("Git push failed or another device pushed first. Local files were not changed and the baseline was not advanced."))?;
        }
        let metadata = crate::import_export::apply_sync_configuration(
            import,
            &self.capture.metadata_path,
            self.capture.metadata.clone(),
            &self.capture.ssh,
            &self.capture.local_only_includes,
        )
        .map_err(|error| {
            invalid(&format!(
                "The remote was updated, but local application failed safely: {error}. Retry Sync to apply the remote version."
            ))
        })?;
        let baseline = Baseline {
            remote: self.settings.remote,
            branch: self.settings.branch,
            snapshot,
        };
        crate::hosts::safe_write(
            &self.checkout.parent().unwrap().join("baseline.json"),
            &serde_json::to_vec_pretty(&baseline)?,
        )?;
        Ok(metadata)
    }
}

fn reject_sensitive_content(snapshot: &SyncSnapshot) -> io::Result<()> {
    let content = serde_json::to_string(snapshot)?;
    if content.contains("PRIVATE KEY-----") || contains_inline_secret(&content) {
        return Err(invalid(
            "Sync data may contain inline credentials or private key material; nothing was applied.",
        ));
    }
    Ok(())
}

fn git(directory: &Path, arguments: &[&str], input: Option<&[u8]>) -> io::Result<String> {
    let mut child = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=LazyTerm",
            "-c",
            "user.email=sync@lazyterm.local",
            "-c",
            "core.sshCommand=ssh -oBatchMode=yes -oStrictHostKeyChecking=yes",
        ])
        .args(arguments)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_INDEX_FILE", directory.join("sync-index"))
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(input) = input {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input)?;
    }
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        stdout.take(17 * 1024 * 1024).read_to_end(&mut bytes)?;
        Ok::<_, io::Error>(bytes)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid(
                "Git operation timed out. Check network access and Git authentication.",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let bytes = reader
        .join()
        .map_err(|_| invalid("Git output reader stopped."))??;
    if !status.success() {
        return Err(invalid(
            "Git operation failed. Check remote access, branch, and Git authentication.",
        ));
    }
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(invalid("Git response exceeded the sync size limit."));
    }
    String::from_utf8(bytes).map_err(|_| invalid("Git returned invalid UTF-8."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{PasswordStorage, ProjectCommand, ProjectDefinition, ProjectLink};
    use std::process::Command;

    fn run_git(directory: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn seed_file_branch(worktree: &Path, remote: &Path) {
        fs::create_dir_all(worktree).unwrap();
        run_git(worktree, &["init", "--quiet"]);
        fs::write(worktree.join("README.md"), "unrelated").unwrap();
        run_git(worktree, &["add", "README.md"]);
        run_git(
            worktree,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "--quiet",
                "-m",
                "existing user data",
            ],
        );
        run_git(worktree, &["push", remote.to_str().unwrap(), "HEAD:main"]);
    }

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lazyterm-sync-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn client(root: &Path, project: ProjectDefinition) -> (PathBuf, PathBuf) {
        let home = root.join("home");
        let data = root.join("data");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        secure_dir(&data).unwrap();
        fs::write(
            home.join(".ssh/config"),
            "Host shared\n  HostName shared.internal\n  User deploy\n",
        )
        .unwrap();
        let metadata_path = data.join("hosts.json");
        let mut metadata = Metadata::default();
        metadata.projects.push(project);
        metadata.save_to(&metadata_path).unwrap();
        (home, metadata_path)
    }

    fn sync_once(home: PathBuf, metadata: PathBuf, remote: &Path, state: &Path) -> Prepared {
        let settings = SyncSettings {
            remote: remote.display().to_string(),
            branch: "main".into(),
        };
        let capture = Capture::read(home, metadata).unwrap();
        Prepared::prepare(settings, capture, state).unwrap()
    }

    fn project(
        id: uuid::Uuid,
        command_id: uuid::Uuid,
        link_id: uuid::Uuid,
        command: &str,
        password: &str,
    ) -> ProjectDefinition {
        ProjectDefinition {
            id,
            name: "fixture".into(),
            base_path: PathBuf::from("~/fixture"),
            commands: vec![ProjectCommand {
                id: command_id,
                name: "start".into(),
                command: command.into(),
                working_directory: None,
                group: None,
                kind: Default::default(),
                target: None,
                lifecycle: Default::default(),
            }],
            links: vec![ProjectLink {
                id: link_id,
                name: "private".into(),
                url: "https://example.test".into(),
                username: "user".into(),
                password: Some(password.into()),
                password_storage: PasswordStorage::Config,
                ..ProjectLink::default()
            }],
            ..ProjectDefinition::default()
        }
    }

    #[test]
    fn disposable_bare_remote_syncs_two_clients_and_resolves_field_conflict() {
        let root = fixture("two-clients");
        let remote = root.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare", "--quiet"]);
        let project_id = uuid::Uuid::new_v4();
        let command_id = uuid::Uuid::new_v4();
        let link_id = uuid::Uuid::new_v4();
        let (home_a, metadata_a) = client(
            &root.join("a"),
            project(project_id, command_id, link_id, "run", "secret-a"),
        );
        let (home_b, metadata_b) = client(
            &root.join("b"),
            project(project_id, command_id, link_id, "run", "secret-b"),
        );
        let state_a = root.join("state-a");
        let state_b = root.join("state-b");

        sync_once(home_a.clone(), metadata_a.clone(), &remote, &state_a)
            .apply_and_push()
            .unwrap();
        sync_once(home_b.clone(), metadata_b.clone(), &remote, &state_b)
            .apply_and_push()
            .unwrap();

        let mut local = Metadata::read_from(&metadata_a).unwrap();
        local.projects[0].commands[0].command = "run-local".into();
        local.save_to(&metadata_a).unwrap();
        sync_once(home_a.clone(), metadata_a.clone(), &remote, &state_a)
            .apply_and_push()
            .unwrap();

        let mut other = Metadata::read_from(&metadata_b).unwrap();
        other.projects[0].commands[0].command = "run-device-b".into();
        other.projects.push(project(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "independent",
            "extra-secret",
        ));
        other.save_to(&metadata_b).unwrap();
        let mut conflict = sync_once(home_b.clone(), metadata_b.clone(), &remote, &state_b);
        assert_eq!(conflict.plan.conflicts.len(), 1);
        assert!(conflict.plan.conflicts[0].path.contains(&format!(
            "metadata:projects/{project_id}/commands/{command_id}/command"
        )));
        conflict.resolve_selected(true).unwrap();
        assert!(conflict.plan.conflicts.is_empty());
        conflict.apply_and_push().unwrap();

        let restored = Metadata::read_from(&metadata_b).unwrap();
        assert_eq!(restored.projects[0].commands[0].command, "run-local");
        assert_eq!(restored.projects.len(), 2);
        assert_eq!(
            restored.projects[0].links[0].password.as_deref(),
            Some("secret-b")
        );
        let ssh = fs::read_to_string(home_b.join(".ssh/config")).unwrap();
        assert!(ssh.contains("Host shared"));
        let manifest = Command::new("git")
            .args(["--git-dir"])
            .arg(&remote)
            .args(["show", &format!("refs/heads/main:{MANIFEST}")])
            .output()
            .unwrap();
        assert!(manifest.status.success());
        let manifest = String::from_utf8(manifest.stdout).unwrap();
        assert!(!manifest.contains("secret-a"));
        assert!(!manifest.contains("secret-b"));

        fs::write(home_a.join(".ssh/config"), "").unwrap();
        sync_once(home_a.clone(), metadata_a.clone(), &remote, &state_a)
            .apply_and_push()
            .unwrap();
        sync_once(home_b.clone(), metadata_b.clone(), &remote, &state_b)
            .apply_and_push()
            .unwrap();
        assert!(
            fs::read_to_string(home_b.join(".ssh/config"))
                .unwrap()
                .trim()
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_ssh_includes_stay_local_and_are_excluded_from_shared_manifest() {
        let root = fixture("external-include");
        let remote = root.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare", "--quiet"]);
        let external = root.join("orb/config");
        fs::create_dir_all(external.parent().unwrap()).unwrap();
        fs::write(&external, "Host orb-machine\n  HostName orb.internal\n").unwrap();
        let shared_project = project(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "run",
            "secret",
        );
        let (home_a, metadata_a) = client(&root.join("a"), shared_project.clone());
        let include = format!("Include {}\n", external.display());
        fs::write(
            home_a.join(".ssh/config"),
            format!("{include}Host shared\n  HostName shared.internal\n"),
        )
        .unwrap();
        let state_a = root.join("state-a");
        sync_once(home_a.clone(), metadata_a, &remote, &state_a)
            .apply_and_push()
            .unwrap();
        assert!(
            fs::read_to_string(home_a.join(".ssh/config"))
                .unwrap()
                .starts_with(&include)
        );
        assert_eq!(
            fs::read_to_string(&external).unwrap(),
            "Host orb-machine\n  HostName orb.internal\n"
        );

        let (home_b, metadata_b) = client(&root.join("b"), shared_project);
        let mut prepared = sync_once(home_b.clone(), metadata_b, &remote, &root.join("state-b"));
        while !prepared.plan.conflicts.is_empty() {
            prepared.resolve_selected(true).unwrap();
        }
        prepared.apply_and_push().unwrap();
        assert!(
            !fs::read_to_string(home_b.join(".ssh/config"))
                .unwrap()
                .contains(&external.display().to_string())
        );

        let manifest = Command::new("git")
            .args(["--git-dir"])
            .arg(&remote)
            .args(["show", &format!("refs/heads/main:{MANIFEST}")])
            .output()
            .unwrap();
        assert!(manifest.status.success());
        let manifest = String::from_utf8(manifest.stdout).unwrap();
        assert!(!manifest.contains("orb-machine"));
        assert!(!manifest.contains(&external.display().to_string()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejected_concurrent_push_does_not_modify_local_configuration() {
        let root = fixture("push-race");
        let remote = root.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare", "--quiet"]);
        let project_id = uuid::Uuid::new_v4();
        let command_id = uuid::Uuid::new_v4();
        let link_id = uuid::Uuid::new_v4();
        let (home_a, metadata_a) = client(
            &root.join("a"),
            project(project_id, command_id, link_id, "run-a", "a-secret"),
        );
        let (home_b, metadata_b) = client(
            &root.join("b"),
            project(project_id, command_id, link_id, "run-b", "b-secret"),
        );
        let prepared_a = sync_once(home_a, metadata_a, &remote, &root.join("state-a"));
        let prepared_b = sync_once(
            home_b.clone(),
            metadata_b.clone(),
            &remote,
            &root.join("state-b"),
        );
        prepared_a.apply_and_push().unwrap();
        let before = fs::read(&metadata_b).unwrap();
        let result = prepared_b.apply_and_push();
        assert!(result.is_err());
        assert_eq!(fs::read(&metadata_b).unwrap(), before);
        assert_eq!(
            fs::read_to_string(home_b.join(".ssh/config")).unwrap(),
            "Host shared\n  HostName shared.internal\n  User deploy\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capture_rejects_inline_authentication_material_in_ssh_config() {
        let root = fixture("sensitive");
        let home = root.join("home");
        let data = root.join("data");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        secure_dir(&data).unwrap();
        fs::write(
            home.join(".ssh/config"),
            "Host shared\n  ProxyCommand sshpass -p cleartext ssh relay\n",
        )
        .unwrap();
        let metadata_path = data.join("hosts.json");
        Metadata::default().save_to(&metadata_path).unwrap();
        assert!(Capture::read(home, metadata_path).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_branch_with_other_files_without_changing_local_data() {
        let root = fixture("unrelated-branch");
        let remote = root.join("remote.git");
        fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "--bare", "--quiet"]);
        seed_file_branch(&root.join("seed"), &remote);
        let project_id = uuid::Uuid::new_v4();
        let command_id = uuid::Uuid::new_v4();
        let link_id = uuid::Uuid::new_v4();
        let (home, metadata) = client(
            &root.join("client"),
            project(project_id, command_id, link_id, "run", "local-secret"),
        );
        let before_metadata = fs::read(&metadata).unwrap();
        let before_ssh = fs::read(home.join(".ssh/config")).unwrap();
        let capture = Capture::read(home.clone(), metadata.clone()).unwrap();
        let settings = SyncSettings {
            remote: remote.display().to_string(),
            branch: "main".into(),
        };
        let result = Prepared::prepare(settings, capture, &root.join("state"));
        assert!(result.is_err());
        assert_eq!(fs::read(&metadata).unwrap(), before_metadata);
        assert_eq!(fs::read(home.join(".ssh/config")).unwrap(), before_ssh);
        let _ = fs::remove_dir_all(root);
    }
}
