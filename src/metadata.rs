use std::{cell::RefCell, collections::BTreeMap, env, fs, io, io::Write, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::hosts::HostCatalog;

const CURRENT_METADATA_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum TunnelKind {
    #[default]
    Local,
    Remote,
    Dynamic,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum TunnelLifecycle {
    #[default]
    App,
    Session,
    Persistent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TunnelDefinition {
    pub id: crate::projects::EntityId,
    pub label: String,
    pub host_alias: String,
    #[serde(default)]
    pub custom_host: bool,
    pub kind: TunnelKind,
    pub bind_address: String,
    pub local_port: u16,
    pub remote_host: Option<String>,
    pub remote_port: Option<u16>,
    pub lifecycle: TunnelLifecycle,
    pub auto_reconnect: bool,
    #[serde(default = "default_tunnel_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_keepalive_interval")]
    pub keepalive_interval_secs: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnippetDefinition {
    pub id: crate::projects::EntityId,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub dangerous: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnippetPackage {
    pub id: crate::projects::EntityId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub snippets: Vec<SnippetDefinition>,
}

fn default_tunnel_timeout() -> u64 {
    10
}

fn default_keepalive_interval() -> u64 {
    30
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum HostView {
    #[default]
    Groups,
    Hosts,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Metadata {
    pub version: u32,
    pub view: HostView,
    pub tags: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub host_groups: Vec<String>,
    pub local_path: Option<PathBuf>,
    pub remote_paths: BTreeMap<String, String>,
    pub show_hidden_files: bool,
    pub tunnels: Vec<TunnelDefinition>,
    #[serde(default)]
    pub snippet_packages: Vec<SnippetPackage>,
    #[serde(default)]
    pub projects: Vec<crate::projects::ProjectDefinition>,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub hidden_tabs: Vec<String>,
    pub credentials: Vec<crate::credentials::Credential>,
    pub host_passwords: BTreeMap<String, uuid::Uuid>,
    #[serde(default)]
    pub uploaded_keys: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub host_public_key_auth: std::collections::BTreeSet<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub(crate) credential_import_targets: BTreeMap<String, String>,
    #[serde(skip)]
    snapshot: RefCell<Option<Vec<u8>>>,
}

#[derive(Serialize)]
pub struct ExportBundle<'a> {
    pub format_version: u32,
    pub ssh_config_path: Option<String>,
    pub ssh_config: String,
    pub ssh_config_files: std::collections::BTreeMap<String, String>,
    pub hosts: &'a [crate::hosts::Host],
    pub metadata: &'a Metadata,
    pub passwords_included: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Theme {
    #[default]
    #[serde(alias = "Dark", alias = "Light")]
    Forest,
    Ocean,
    Amber,
    Rose,
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Self::Forest => "Forest",
            Self::Ocean => "Ocean",
            Self::Amber => "Amber",
            Self::Rose => "Rose",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Forest => Self::Ocean,
            Self::Ocean => Self::Amber,
            Self::Amber => Self::Rose,
            Self::Rose => Self::Forest,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Language {
    #[default]
    English,
    Chinese,
    Japanese,
}

impl Language {
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
            Self::Japanese => "日本語",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::English => Self::Chinese,
            Self::Chinese => Self::Japanese,
            Self::Japanese => Self::English,
        }
    }
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            version: CURRENT_METADATA_VERSION,
            view: HostView::Groups,
            tags: BTreeMap::new(),
            host_groups: Vec::new(),
            local_path: None,
            remote_paths: BTreeMap::new(),
            show_hidden_files: false,
            tunnels: Vec::new(),
            snippet_packages: Vec::new(),
            projects: Vec::new(),
            theme: Theme::Forest,
            language: Language::English,
            hidden_tabs: Vec::new(),
            credentials: Vec::new(),
            host_passwords: BTreeMap::new(),
            uploaded_keys: BTreeMap::new(),
            host_public_key_auth: std::collections::BTreeSet::new(),
            extra: BTreeMap::new(),
            credential_import_targets: BTreeMap::new(),
            snapshot: RefCell::new(None),
        }
    }
}

impl Metadata {
    pub(crate) fn merged_document(
        &self,
        document: serde_json::Value,
        credential_import_targets: BTreeMap<String, String>,
    ) -> io::Result<Self> {
        let mut merged: Self = serde_json::from_value(document)?;
        merged.version = CURRENT_METADATA_VERSION;
        merged.snapshot = self.snapshot.clone();
        merged.credential_import_targets = credential_import_targets;
        Ok(merged)
    }

    pub fn normalize_host_groups(&mut self) -> bool {
        let mut groups = self
            .host_groups
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for tag in self.tags.values().flatten() {
            groups.insert(tag.clone());
        }
        let groups = groups.into_iter().collect::<Vec<_>>();
        if groups == self.host_groups {
            false
        } else {
            self.host_groups = groups;
            true
        }
    }

    pub fn load() -> io::Result<Self> {
        Self::read_from(&Self::path()?)
    }

    pub(crate) fn read_from(path: &std::path::Path) -> io::Result<Self> {
        Self::read_document(path, true)
    }

    pub(crate) fn read_for_import(path: &std::path::Path) -> io::Result<Self> {
        Self::read_document(path, false)
    }

    fn read_document(path: &std::path::Path, migrate: bool) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let mut document: serde_json::Value = serde_json::from_slice(&bytes)?;
                let version = match document.get("version") {
                    None => 1,
                    Some(value) => value
                        .as_u64()
                        .and_then(|version| u32::try_from(version).ok())
                        .ok_or_else(|| io::Error::other("Invalid host metadata version"))?,
                };
                if version == 0 || version > CURRENT_METADATA_VERSION {
                    return Err(io::Error::other("Unsupported host metadata version"));
                }
                if version < CURRENT_METADATA_VERSION {
                    let legacy_runner_ids = migrate_entity_ids_inner(&mut document)?;
                    if !legacy_runner_ids.is_empty() && migrate {
                        crate::project_runner::save_legacy_ids(
                            path.parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .join("project-logs"),
                            &legacy_runner_ids,
                        )?;
                    }
                }
                let mut metadata: Self = serde_json::from_value(document)?;
                metadata.version = version;
                *metadata.snapshot.borrow_mut() = Some(bytes);
                if migrate && version < CURRENT_METADATA_VERSION {
                    metadata.version = CURRENT_METADATA_VERSION;
                    metadata.save_to(path)?;
                }
                Ok(metadata)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(test))]
    #[track_caller]
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path()?)
    }

    #[track_caller]
    pub(crate) fn save_to(&self, path: &std::path::Path) -> io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        fs::create_dir_all(parent)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("Missing metadata filename"))?
            .to_string_lossy();
        let open_private = |destination: &std::path::Path, append: bool| {
            let mut options = fs::OpenOptions::new();
            options.create(true).read(true).write(true).append(append);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(destination)
        };
        let lock = open_private(&parent.join(format!("{name}.lock")), false)?;
        lock.lock()?;
        let current = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let caller = std::panic::Location::caller();
        let mut journal = open_private(&parent.join(format!("{name}.writes.jsonl")), true)?;
        let mut log = |outcome: &str| -> io::Result<()> {
            serde_json::to_writer(
                &mut journal,
                &serde_json::json!({
                    "unix_nanos": timestamp.to_string(),
                    "pid": std::process::id(),
                    "outcome": outcome,
                    "caller": format!("{}:{}", caller.file(), caller.line()),
                    "projects": self.projects.len(),
                    "project_commands": self.projects.iter().map(|project| project.commands.len()).sum::<usize>(),
                    "project_links": self.projects.iter().map(|project| project.links.len()).sum::<usize>(),
                    "credentials": self.credentials.len(),
                }),
            )?;
            journal.write_all(b"\n")?;
            journal.sync_all()
        };
        if current.as_ref() != self.snapshot.borrow().as_ref() {
            log("conflict")?;
            return Err(io::Error::other(
                "Configuration changed on disk; save blocked to preserve other changes. Close older LazyTerm instances, then press r in Hosts to reload before retrying.",
            ));
        }
        if let Some(bytes) = &current {
            let backups = parent.join(format!("{name}.backups"));
            fs::create_dir_all(&backups)?;
            let backup = backups.join(format!("{timestamp:020}-{}.json", std::process::id()));
            let mut file = open_private(&backup, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            let mut entries = fs::read_dir(&backups)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                })
                .collect::<Vec<_>>();
            entries.sort();
            let expired = entries.len().saturating_sub(30);
            for entry in entries.into_iter().take(expired) {
                fs::remove_file(entry)?;
            }
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        log("attempt")?;
        if let Err(error) = crate::hosts::safe_write(path, &bytes) {
            let _ = log("failed");
            return Err(error);
        }
        *self.snapshot.borrow_mut() = Some(bytes);
        let _ = log("saved");
        Ok(())
    }

    pub fn reconcile(&mut self, catalog: &HostCatalog) -> bool {
        // An incomplete catalog is not evidence that a host was deleted.
        if catalog.error.is_some() {
            return false;
        }
        let aliases = catalog
            .hosts
            .iter()
            .map(|host| host.alias.as_str())
            .collect::<std::collections::HashSet<_>>();
        let previous_len = self.tags.len()
            + self.remote_paths.len()
            + self.tunnels.len()
            + self.host_passwords.len()
            + self.uploaded_keys.len()
            + self.host_public_key_auth.len();
        self.host_passwords.retain(|alias, id| {
            aliases.contains(alias.as_str())
                && self.credentials.iter().any(|credential| {
                    credential.id == *id
                        && credential.kind == crate::credentials::CredentialKind::Password
                })
        });
        self.tags
            .retain(|alias, _| aliases.contains(alias.as_str()));
        self.remote_paths
            .retain(|alias, _| aliases.contains(alias.as_str()));
        self.tunnels
            .retain(|tunnel| tunnel.custom_host || aliases.contains(tunnel.host_alias.as_str()));
        self.uploaded_keys.retain(|alias, fingerprints| {
            aliases.contains(alias.as_str()) && !fingerprints.is_empty()
        });
        self.host_public_key_auth
            .retain(|alias| aliases.contains(alias.as_str()));
        self.tags.len()
            + self.remote_paths.len()
            + self.tunnels.len()
            + self.host_passwords.len()
            + self.uploaded_keys.len()
            + self.host_public_key_auth.len()
            != previous_len
    }

    pub(crate) fn data_dir() -> io::Result<PathBuf> {
        let home = env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let base = if cfg!(target_os = "macos") {
            PathBuf::from(home).join("Library/Application Support")
        } else {
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(home).join(".local/share"))
        };
        Ok(base.join("lazyterm"))
    }

    fn path() -> io::Result<PathBuf> {
        Ok(Self::data_dir()?.join("hosts.json"))
    }
}

pub(crate) fn migrate_entity_ids(document: &mut serde_json::Value) -> io::Result<()> {
    migrate_entity_ids_inner(document).map(|_| ())
}

fn migrate_entity_ids_inner(
    document: &mut serde_json::Value,
) -> io::Result<Vec<crate::project_runner::LegacyTaskId>> {
    let mut legacy_runner_ids = Vec::new();
    let mut credential_ids = BTreeMap::new();
    if let Some(credentials) = document
        .get_mut("credentials")
        .and_then(serde_json::Value::as_array_mut)
    {
        for item in credentials {
            let previous = item["id"].to_string();
            let id = uuid::Uuid::new_v4().to_string();
            item["id"] = id.clone().into();
            credential_ids.insert(previous, id);
        }
    }
    if let Some(associations) = document
        .get_mut("host_passwords")
        .and_then(serde_json::Value::as_object_mut)
    {
        for id in associations.values_mut() {
            if let Some(mapped) = credential_ids.get(&id.to_string()) {
                *id = mapped.clone().into();
            }
        }
    }

    let mut link_references = BTreeMap::new();
    if let Some(projects) = document
        .get_mut("projects")
        .and_then(serde_json::Value::as_array_mut)
    {
        for project in projects {
            let old_project_id = project["id"].as_u64();
            let old_project = project["id"].to_string();
            let new_project = uuid::Uuid::new_v4().to_string();
            project["id"] = new_project.clone().into();
            if let Some(commands) = project
                .get_mut("commands")
                .and_then(serde_json::Value::as_array_mut)
            {
                for command in commands {
                    let old_command_id = command["id"].as_u64();
                    let new_command = uuid::Uuid::new_v4();
                    command["id"] = new_command.to_string().into();
                    if let (Some(project_id), Some(command_id)) = (old_project_id, old_command_id) {
                        legacy_runner_ids.push(crate::project_runner::LegacyTaskId {
                            project_id,
                            command_id,
                            project_uuid: new_project.parse().map_err(io::Error::other)?,
                            command_uuid: new_command,
                        });
                    }
                }
            }
            if let Some(links) = project
                .get_mut("links")
                .and_then(serde_json::Value::as_array_mut)
            {
                for link in links {
                    let old_link = link["id"].to_string();
                    let new_link = uuid::Uuid::new_v4().to_string();
                    link["id"] = new_link.clone().into();
                    link_references.insert(
                        format!("config:{old_project}:{}", old_link.trim_matches('"')),
                        format!("config:{new_project}:{}", new_link),
                    );
                }
            }
        }
    }
    if let Some(projects) = document
        .get_mut("projects")
        .and_then(serde_json::Value::as_array_mut)
    {
        for project in projects {
            if let Some(links) = project
                .get_mut("links")
                .and_then(serde_json::Value::as_array_mut)
            {
                for link in links {
                    if let Some(reference) = link["password_ref"].as_str()
                        && let Some(mapped) = link_references.get(reference)
                    {
                        link["password_ref"] = mapped.clone().into();
                    }
                }
            }
        }
    }
    for field in ["tunnels", "snippet_packages"] {
        if let Some(items) = document
            .get_mut(field)
            .and_then(serde_json::Value::as_array_mut)
        {
            for item in items {
                item["id"] = uuid::Uuid::new_v4().to_string().into();
                if field == "snippet_packages"
                    && let Some(snippets) = item
                        .get_mut("snippets")
                        .and_then(serde_json::Value::as_array_mut)
                {
                    for snippet in snippets {
                        snippet["id"] = uuid::Uuid::new_v4().to_string().into();
                    }
                }
            }
        }
    }
    Ok(legacy_runner_ids)
}

pub fn parse_tags(value: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for tag in value
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        if !tags.iter().any(|existing: &String| existing == tag) {
            tags.push(tag.to_string());
        }
    }
    tags.sort();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_theme_values_load_as_forest_without_dropping_projects() {
        let fixture = crate::projects::tests::Fixture::new("legacy-theme");
        for legacy_theme in ["Dark", "Light"] {
            let mut metadata = Metadata::default();
            metadata.projects.push(fixture.project.clone());
            let mut value = serde_json::to_value(metadata).unwrap();
            value["theme"] = serde_json::Value::String(legacy_theme.into());

            let restored: Metadata = serde_json::from_value(value).unwrap();
            assert_eq!(restored.theme, Theme::Forest);
            assert_eq!(
                restored.projects.as_slice(),
                std::slice::from_ref(&fixture.project)
            );
        }
    }

    #[test]
    fn stale_writer_cannot_erase_projects_credentials_or_unknown_fields() {
        let fixture = crate::projects::tests::Fixture::new("stale-metadata-writer");
        let path = fixture.root.join("hosts.json");
        let initial = Metadata::default();
        initial.save_to(&path).unwrap();
        let mut stale = Metadata::read_from(&path).unwrap();
        let mut current = Metadata::read_from(&path).unwrap();
        let mut project = fixture.project.clone();
        project.commands.push(fixture.command("printf ready"));
        project.links.push(crate::projects::ProjectLink {
            id: uuid::Uuid::from_u128(1),
            name: "Home".into(),
            url: "http://localhost:3000/".into(),
            password_storage: crate::projects::PasswordStorage::Config,
            password: Some("sensitive-fixture-value".into()),
            ..Default::default()
        });
        current.projects.push(project);
        current.credentials.push(crate::credentials::Credential {
            id: uuid::Uuid::from_u128(1),
            name: "Fixture password".into(),
            kind: crate::credentials::CredentialKind::Password,
            username: String::new(),
            password_ref: Some("credential-reference".into()),
            private_key_path: None,
            public_key: String::new(),
            fingerprint: String::new(),
        });
        current
            .extra
            .insert("future_field".into(), serde_json::json!({"keep": true}));
        current.save_to(&path).unwrap();
        let saved = fs::read(&path).unwrap();
        stale.theme = Theme::Ocean;
        let error = stale.save_to(&path).unwrap_err();
        assert!(error.to_string().contains("Configuration changed on disk"));
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert!(Metadata::default().save_to(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);

        let mut reloaded = Metadata::read_from(&path).unwrap();
        reloaded.theme = Theme::Amber;
        reloaded.save_to(&path).unwrap();
        reloaded.language = Language::Chinese;
        reloaded.save_to(&path).unwrap();
        let restored = Metadata::read_from(&path).unwrap();
        assert_eq!(restored.projects, current.projects);
        assert_eq!(restored.credentials, current.credentials);
        assert_eq!(restored.extra, current.extra);
        let journal = fs::read_to_string(fixture.root.join("hosts.json.writes.jsonl")).unwrap();
        assert!(journal.contains("\"outcome\":\"conflict\""));
        assert!(journal.contains("\"caller\":"));
        assert!(!journal.contains("sensitive-fixture-value"));
        assert!(!journal.contains("credential-reference"));
        assert!(
            fs::read_dir(fixture.root.join("hosts.json.backups"))
                .unwrap()
                .count()
                >= 3
        );
    }

    #[test]
    fn legacy_unconditional_save_reproduces_project_loss_in_a_temporary_file() {
        let fixture = crate::projects::tests::Fixture::new("legacy-overwrite-reproduction");
        let path = fixture.root.join("hosts.json");
        let stale = Metadata::default();
        let mut updated = stale.clone();
        updated.projects.push(fixture.project.clone());
        updated.save_to(&path).unwrap();
        assert_eq!(Metadata::read_from(&path).unwrap().projects.len(), 1);

        // This is the previous save implementation, exercised only against a fixture.
        crate::hosts::safe_write(&path, &serde_json::to_vec_pretty(&stale).unwrap()).unwrap();
        assert!(Metadata::read_from(&path).unwrap().projects.is_empty());
        assert_eq!(
            Metadata::read_from(&path.with_extension("config.lazyterm.bak"))
                .unwrap()
                .projects
                .len(),
            1
        );
        crate::hosts::safe_write(&path, &serde_json::to_vec_pretty(&stale).unwrap()).unwrap();
        assert!(
            Metadata::read_from(&path.with_extension("config.lazyterm.bak"))
                .unwrap()
                .projects
                .is_empty()
        );
    }

    #[test]
    fn simultaneous_metadata_writers_allow_only_one_snapshot_to_commit() {
        let fixture = crate::projects::tests::Fixture::new("concurrent-metadata");
        let path = fixture.root.join("hosts.json");
        Metadata::default().save_to(&path).unwrap();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writers = (0..2)
            .map(|index| {
                let path = path.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let mut metadata = Metadata::read_from(&path).unwrap();
                    metadata.extra.insert("writer".into(), index.into());
                    gate.wait();
                    metadata.save_to(&path).is_ok()
                })
            })
            .collect::<Vec<_>>();
        let saved = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .filter(|saved| *saved)
            .count();
        assert_eq!(saved, 1);
        assert!(
            Metadata::read_from(&path)
                .unwrap()
                .extra
                .contains_key("writer")
        );
    }

    #[test]
    fn deleted_or_corrupt_metadata_is_never_overwritten_by_an_existing_reader() {
        let fixture = crate::projects::tests::Fixture::new("external-metadata-removal");
        let path = fixture.root.join("hosts.json");
        Metadata::default().save_to(&path).unwrap();
        let metadata = Metadata::read_from(&path).unwrap();
        fs::write(&path, "{broken").unwrap();
        assert!(metadata.save_to(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
        fs::remove_file(&path).unwrap();
        assert!(metadata.save_to(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn projects_upgrade_additively_persist_backup_and_survive_host_reconciliation() {
        use crate::projects::{
            CommandKind, ProjectCommand, ProjectDefinition, ProjectLink, ProjectTarget,
            tests::Fixture,
        };
        let fixture = Fixture::new("metadata");
        let path = fixture.root.join("hosts.json");
        fs::write(&path, r#"{"version":1,"tags":{"gone":["work"]}}"#).unwrap();
        let original = fs::read(&path).unwrap();
        let mut metadata = Metadata::read_from(&path).unwrap();
        assert!(metadata.projects.is_empty());
        let project = ProjectDefinition {
            id: uuid::Uuid::from_u128(41),
            name: "Demo".into(),
            base_path: fixture.root.clone(),
            commands: vec![ProjectCommand {
                id: uuid::Uuid::from_u128(7),
                name: "Tests".into(),
                command: "cargo test".into(),
                working_directory: Some("sub directory".into()),
                group: Some("Development".into()),
                kind: CommandKind::Service,
                target: Some(ProjectTarget::Local),
                lifecycle: crate::projects::ProjectLifecycle::Persistent,
            }],
            groups: vec!["Development".into(), "Empty".into()],
            target: ProjectTarget::Host("fixture".into()),
            links: vec![ProjectLink {
                id: uuid::Uuid::from_u128(1),
                name: "Preview".into(),
                url: "http://localhost:3000/".into(),
                ..Default::default()
            }],
        };
        metadata.projects.push(project.clone());
        assert!(metadata.reconcile(&HostCatalog::default()));
        assert_eq!(metadata.projects.as_slice(), std::slice::from_ref(&project));
        metadata.save_to(&path).unwrap();
        let dated_backups = path.with_file_name("hosts.json.backups");
        assert!(
            fs::read_dir(&dated_backups)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| fs::read(entry.path()).is_ok_and(|backup| backup == original))
        );
        let restored = Metadata::read_from(&path).unwrap();
        assert_eq!(restored.projects, [project]);
        assert_eq!(restored.version, CURRENT_METADATA_VERSION);
        assert!(restored.tags.is_empty());
        let backups = fs::read_dir(path.with_file_name("hosts.json.backups"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| fs::read(entry.path()).unwrap())
            .collect::<Vec<_>>();
        assert!(backups.iter().any(|backup| backup == &original));
    }

    #[test]
    fn tags_are_trimmed_sorted_and_deduplicated() {
        assert_eq!(
            parse_tags(" work,prod, work, ,数据库 "),
            ["prod", "work", "数据库"]
        );
    }

    #[test]
    fn metadata_round_trips_tags_and_view() {
        let mut metadata = Metadata {
            version: CURRENT_METADATA_VERSION,
            view: HostView::Hosts,
            ..Metadata::default()
        };
        metadata.tags.insert("web".into(), parse_tags("prod,work"));
        metadata.local_path = Some(PathBuf::from("/tmp/lazyterm"));
        metadata
            .remote_paths
            .insert("web".into(), "projects".into());
        metadata
            .uploaded_keys
            .insert("web".into(), vec!["SHA256:fixture".into()]);
        metadata.host_public_key_auth.insert("web".into());
        metadata.show_hidden_files = true;
        metadata.tunnels.push(TunnelDefinition {
            id: uuid::Uuid::from_u128(7),
            label: "web-db".into(),
            host_alias: "web".into(),
            custom_host: false,
            kind: TunnelKind::Local,
            bind_address: "127.0.0.1".into(),
            local_port: 5432,
            remote_host: Some("db.internal".into()),
            remote_port: Some(5432),
            lifecycle: TunnelLifecycle::App,
            auto_reconnect: false,
            connect_timeout_secs: 10,
            keepalive_interval_secs: 30,
        });
        let bytes = serde_json::to_vec(&metadata).unwrap();
        let restored: Metadata = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.view, HostView::Hosts);
        assert_eq!(restored.tags["web"], ["prod", "work"]);
        assert_eq!(restored.local_path, Some(PathBuf::from("/tmp/lazyterm")));
        assert_eq!(restored.remote_paths["web"], "projects");
        assert_eq!(restored.uploaded_keys["web"], ["SHA256:fixture"]);
        assert!(restored.host_public_key_auth.contains("web"));
        assert!(restored.show_hidden_files);
        assert_eq!(restored.tunnels[0].label, "web-db");
        assert_eq!(restored.tunnels[0].local_port, 5432);
    }

    #[test]
    fn reconcile_keeps_custom_tunnel_targets() {
        let catalog = HostCatalog {
            hosts: vec![crate::hosts::Host {
                alias: "web".into(),
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                proxy_jump: None,
                managed: false,
            }],
            error: None,
        };
        let mut metadata = Metadata {
            tunnels: vec![
                TunnelDefinition {
                    id: uuid::Uuid::from_u128(1),
                    label: "saved".into(),
                    host_alias: "web".into(),
                    custom_host: false,
                    kind: TunnelKind::Local,
                    bind_address: "127.0.0.1".into(),
                    local_port: 10001,
                    remote_host: Some("db".into()),
                    remote_port: Some(5432),
                    lifecycle: TunnelLifecycle::App,
                    auto_reconnect: false,
                    connect_timeout_secs: 10,
                    keepalive_interval_secs: 30,
                },
                TunnelDefinition {
                    id: uuid::Uuid::from_u128(2),
                    label: "custom".into(),
                    host_alias: "192.0.2.10".into(),
                    custom_host: true,
                    kind: TunnelKind::Local,
                    bind_address: "127.0.0.1".into(),
                    local_port: 10002,
                    remote_host: Some("db".into()),
                    remote_port: Some(5432),
                    lifecycle: TunnelLifecycle::App,
                    auto_reconnect: false,
                    connect_timeout_secs: 10,
                    keepalive_interval_secs: 30,
                },
            ],
            ..Metadata::default()
        };

        assert!(!metadata.reconcile(&catalog));
        assert_eq!(metadata.tunnels.len(), 2);
    }

    #[test]
    fn reconcile_removes_uploaded_key_history_for_deleted_hosts() {
        let catalog = HostCatalog {
            hosts: vec![crate::hosts::Host {
                alias: "web".into(),
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                proxy_jump: None,
                managed: false,
            }],
            error: None,
        };
        let mut metadata = Metadata::default();
        metadata
            .uploaded_keys
            .insert("web".into(), vec!["SHA256:still-here".into()]);
        metadata
            .uploaded_keys
            .insert("deleted".into(), vec!["SHA256:stale".into()]);
        metadata.host_public_key_auth.insert("web".into());
        metadata.host_public_key_auth.insert("deleted".into());

        assert!(metadata.reconcile(&catalog));
        assert!(metadata.uploaded_keys.contains_key("web"));
        assert!(!metadata.uploaded_keys.contains_key("deleted"));
        assert!(metadata.host_public_key_auth.contains("web"));
        assert!(!metadata.host_public_key_auth.contains("deleted"));
    }

    #[test]
    fn metadata_persists_and_rejects_corrupt_or_future_files() {
        let root = env::temp_dir().join(format!("lazyterm-metadata-{}", std::process::id()));
        let path = root.join("hosts.json");
        assert_eq!(
            Metadata::read_from(&path).unwrap().version,
            CURRENT_METADATA_VERSION
        );
        let mut metadata = Metadata {
            view: HostView::Hosts,
            ..Metadata::default()
        };
        metadata.tags.insert("web".into(), parse_tags("prod,work"));
        crate::hosts::safe_write(&path, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        let restored = Metadata::read_from(&path).unwrap();
        assert_eq!(restored.tags["web"], ["prod", "work"]);
        assert_eq!(restored.view, HostView::Hosts);
        fs::write(&path, r#"{"version":4}"#).unwrap();
        assert!(Metadata::read_from(&path).is_err());
        fs::write(&path, r#"{"version":"old"}"#).unwrap();
        assert!(Metadata::read_from(&path).is_err());
        fs::write(&path, "{").unwrap();
        assert!(Metadata::read_from(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v2_numeric_entity_ids_migrate_to_uuids_and_rewrite_references() {
        let root = env::temp_dir().join(format!(
            "lazyterm-uuid-migration-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("hosts.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": 2,
                "credentials": [{
                    "id": 17, "name": "prod password", "kind": "Password"
                }],
                "host_passwords": {"prod": 17},
                "projects": [{
                    "id": 1, "name": "Demo", "base_path": "/tmp",
                    "commands": [{"id": 2, "name": "start", "command": "true"}],
                    "links": [{"id": 3, "name": "Home", "url": "https://example.com",
                        "password_ref": "config:1:3"}]
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let metadata = Metadata::read_from(&path).unwrap();
        assert_eq!(metadata.version, CURRENT_METADATA_VERSION);
        assert_ne!(metadata.credentials[0].id, uuid::Uuid::from_u128(17));
        assert_eq!(metadata.host_passwords["prod"], metadata.credentials[0].id);
        let project = &metadata.projects[0];
        assert_ne!(project.id, uuid::Uuid::from_u128(1));
        assert_ne!(project.commands[0].id, uuid::Uuid::from_u128(2));
        assert_ne!(project.links[0].id, uuid::Uuid::from_u128(3));
        assert_eq!(
            project.links[0].password_ref.as_deref(),
            Some(format!("config:{}:{}", project.id, project.links[0].id).as_str())
        );
        let runner_ids: Vec<crate::project_runner::LegacyTaskId> = serde_json::from_slice(
            &fs::read(root.join("project-logs/legacy-runner-ids.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(runner_ids.len(), 1);
        assert_eq!(runner_ids[0].project_id, 1);
        assert_eq!(runner_ids[0].command_id, 2);
        assert_eq!(runner_ids[0].project_uuid, project.id);
        assert_eq!(runner_ids[0].command_uuid, project.commands[0].id);
        assert_eq!(
            fs::read_dir(root.join("hosts.json.backups"))
                .unwrap()
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reconciles_external_deletions_and_includes_and_persists_cleanup() {
        let root = env::temp_dir().join(format!("lazyterm-reconcile-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config");
        let included = root.join("work.conf");
        let metadata_path = root.join("hosts.json");
        fs::write(&config, "Host web\nHost removed\nInclude work.conf\n").unwrap();
        fs::write(&included, "Host worker\n").unwrap();
        let mut metadata = Metadata {
            view: HostView::Hosts,
            ..Metadata::default()
        };
        for alias in ["web", "removed", "worker"] {
            metadata.tags.insert(alias.into(), parse_tags("work"));
        }
        metadata.save_to(&metadata_path).unwrap();

        let catalog = HostCatalog::load_from(&config, &root);
        assert!(!metadata.reconcile(&catalog));
        fs::write(&config, "Host web\nInclude work.conf\n").unwrap();
        let catalog = HostCatalog::load_from(&config, &root);
        assert!(catalog.error.is_none());
        assert!(metadata.reconcile(&catalog));
        metadata.save_to(&metadata_path).unwrap();
        let mut restored = Metadata::read_from(&metadata_path).unwrap();
        assert_eq!(
            restored.tags.keys().map(String::as_str).collect::<Vec<_>>(),
            ["web", "worker"]
        );
        assert_eq!(restored.view, HostView::Hosts);
        assert!(!restored.reconcile(&catalog));
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "Host web\nInclude work.conf\n"
        );

        fs::remove_file(&included).unwrap();
        assert!(restored.reconcile(&HostCatalog::load_from(&config, &root)));
        assert!(restored.tags.contains_key("web"));
        assert!(!restored.tags.contains_key("worker"));
        fs::remove_file(&config).unwrap();
        assert!(restored.reconcile(&HostCatalog::load_from(&config, &root)));
        assert!(restored.tags.is_empty());
        assert_eq!(restored.view, HostView::Hosts);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incomplete_catalogs_never_remove_metadata() {
        let root =
            env::temp_dir().join(format!("lazyterm-reconcile-errors-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config");
        let mut metadata = Metadata::default();
        metadata.tags.insert("web".into(), parse_tags("prod"));
        for contents in [
            "Host \"unterminated\n",
            "Host web\nInclude ${CONFIG_DIR}/*\n",
            "Host web\nInclude [invalid\n",
            "Host web\nInclude invalid.conf\n",
        ] {
            fs::write(&config, contents).unwrap();
            fs::write(root.join("invalid.conf"), "Host \"unterminated\n").unwrap();
            let catalog = HostCatalog::load_from(&config, &root);
            assert!(catalog.error.is_some(), "{contents}");
            assert!(!metadata.reconcile(&catalog));
            assert_eq!(metadata.tags["web"], ["prod"]);
        }
        fs::remove_file(&config).unwrap();
        fs::create_dir(&config).unwrap();
        let catalog = HostCatalog::load_from(&config, &root);
        assert!(catalog.error.is_some());
        assert!(!metadata.reconcile(&catalog));
        assert_eq!(metadata.tags["web"], ["prod"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_rename_does_not_guess_metadata_identity() {
        let mut metadata = Metadata::default();
        metadata.tags.insert("old".into(), parse_tags("prod"));
        let catalog = HostCatalog {
            hosts: vec![crate::hosts::Host {
                alias: "new".into(),
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                proxy_jump: None,
                managed: false,
            }],
            error: None,
        };
        assert!(metadata.reconcile(&catalog));
        assert!(metadata.tags.is_empty());
    }
}
