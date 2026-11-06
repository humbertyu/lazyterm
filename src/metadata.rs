use std::{collections::BTreeMap, env, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::hosts::HostCatalog;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum HostView {
    #[default]
    Groups,
    Hosts,
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Metadata {
    pub version: u32,
    pub view: HostView,
    pub tags: BTreeMap<String, Vec<String>>,
    pub local_path: Option<PathBuf>,
    pub remote_paths: BTreeMap<String, String>,
    pub show_hidden_files: bool,
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
        }
    }
}

impl Metadata {
    pub fn load() -> io::Result<Self> {
        Self::read_from(&Self::path()?)
    }

    fn read_from(path: &std::path::Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let metadata: Self = serde_json::from_slice(&bytes)?;
                if metadata.version != 1 {
                    return Err(io::Error::other("Unsupported host metadata version"));
                }
                Ok(metadata)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path()?)
    }

    fn save_to(&self, path: &std::path::Path) -> io::Result<()> {
        crate::hosts::safe_write(path, &serde_json::to_vec_pretty(self)?)
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
        let previous_len = self.tags.len() + self.remote_paths.len();
        self.tags
            .retain(|alias, _| aliases.contains(alias.as_str()));
        self.remote_paths
            .retain(|alias, _| aliases.contains(alias.as_str()));
        self.tags.len() + self.remote_paths.len() != previous_len
    }

    fn path() -> io::Result<PathBuf> {
        let home = env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let base = if cfg!(target_os = "macos") {
            PathBuf::from(home).join("Library/Application Support")
        } else {
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(home).join(".local/share"))
        };
        Ok(base.join("lazyterm/hosts.json"))
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
        let bytes = serde_json::to_vec(&metadata).unwrap();
        let restored: Metadata = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.view, HostView::Hosts);
        assert_eq!(restored.tags["web"], ["prod", "work"]);
        assert_eq!(restored.local_path, Some(PathBuf::from("/tmp/lazyterm")));
        assert_eq!(restored.remote_paths["web"], "projects");
        assert!(restored.show_hidden_files);
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
                managed: false,
            }],
            error: None,
        };
        assert!(metadata.reconcile(&catalog));
        assert!(metadata.tags.is_empty());
    }
}
