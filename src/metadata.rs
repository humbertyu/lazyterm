use std::{cell::RefCell, collections::BTreeMap, env, fs, io, io::Write, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::hosts::HostCatalog;

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
    pub id: u64,
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
    pub id: u64,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub dangerous: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnippetPackage {
    pub id: u64,
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
    pub host_passwords: BTreeMap<String, u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
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
    Dark,
    Light,
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Language {
    #[default]
    English,
    Chinese,
}

impl Language {
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
        }
    }
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            version: 1,
            view: HostView::Groups,
            tags: BTreeMap::new(),
            local_path: None,
            remote_paths: BTreeMap::new(),
            show_hidden_files: false,
            tunnels: Vec::new(),
            snippet_packages: Vec::new(),
            projects: Vec::new(),
            theme: Theme::Dark,
            language: Language::English,
            hidden_tabs: Vec::new(),
            credentials: Vec::new(),
            host_passwords: BTreeMap::new(),
            extra: BTreeMap::new(),
            snapshot: RefCell::new(None),
        }
    }
}

impl Metadata {
    pub fn load() -> io::Result<Self> {
        Self::read_from(&Self::path()?)
    }

    pub(crate) fn read_from(path: &std::path::Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let metadata: Self = serde_json::from_slice(&bytes)?;
                if metadata.version != 1 {
                    return Err(io::Error::other("Unsupported host metadata version"));
                }
                *metadata.snapshot.borrow_mut() = Some(bytes);
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
            + self.host_passwords.len();
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
        self.tags.len() + self.remote_paths.len() + self.tunnels.len() + self.host_passwords.len()
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
            id: 1,
            name: "Home".into(),
            url: "http://localhost:3000/".into(),
            password_storage: crate::projects::PasswordStorage::Config,
            password: Some("sensitive-fixture-value".into()),
            ..Default::default()
        });
        current.projects.push(project);
        current.credentials.push(crate::credentials::Credential {
            id: 1,
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
        stale.theme = Theme::Light;
        let error = stale.save_to(&path).unwrap_err();
        assert!(error.to_string().contains("Configuration changed on disk"));
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert!(Metadata::default().save_to(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);

        let mut reloaded = Metadata::read_from(&path).unwrap();
        reloaded.theme = Theme::Light;
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
            id: 41,
            name: "Demo".into(),
            base_path: fixture.root.clone(),
            commands: vec![ProjectCommand {
                id: 7,
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
                id: 1,
                name: "Preview".into(),
                url: "http://localhost:3000/".into(),
                ..Default::default()
            }],
        };
        metadata.projects.push(project.clone());
        assert!(metadata.reconcile(&HostCatalog::default()));
        assert_eq!(metadata.projects.as_slice(), std::slice::from_ref(&project));
        metadata.save_to(&path).unwrap();
        assert_eq!(
            fs::read(path.with_extension("config.lazyterm.bak")).unwrap(),
            original
        );
        let restored = Metadata::read_from(&path).unwrap();
        assert_eq!(restored.projects, [project]);
        assert_eq!(restored.version, 1);
        assert!(restored.tags.is_empty());
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
            version: 1,
            view: HostView::Hosts,
            ..Metadata::default()
        };
        metadata.tags.insert("web".into(), parse_tags("prod,work"));
        metadata.local_path = Some(PathBuf::from("/tmp/lazyterm"));
        metadata
            .remote_paths
            .insert("web".into(), "projects".into());
        metadata.show_hidden_files = true;
        metadata.tunnels.push(TunnelDefinition {
            id: 7,
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
                    id: 1,
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
                    id: 2,
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
    fn metadata_persists_and_rejects_corrupt_or_future_files() {
        let root = env::temp_dir().join(format!("lazyterm-metadata-{}", std::process::id()));
        let path = root.join("hosts.json");
        assert_eq!(Metadata::read_from(&path).unwrap().version, 1);
        let mut metadata = Metadata {
            view: HostView::Hosts,
            ..Metadata::default()
        };
        metadata.tags.insert("web".into(), parse_tags("prod,work"));
        crate::hosts::safe_write(&path, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        let restored = Metadata::read_from(&path).unwrap();
        assert_eq!(restored.tags["web"], ["prod", "work"]);
        assert_eq!(restored.view, HostView::Hosts);
        fs::write(&path, r#"{"version":2}"#).unwrap();
        assert!(Metadata::read_from(&path).is_err());
        fs::write(&path, "{").unwrap();
        assert!(Metadata::read_from(&path).is_err());
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
