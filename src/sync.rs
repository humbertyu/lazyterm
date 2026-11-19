// Keep the staged sync planner available while the manifest and Git adapter are built.
#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SyncSettings {
    pub(crate) remote: String,
    pub(crate) branch: String,
}

pub(crate) struct ConnectionTest {
    pub(crate) receiver: Receiver<Result<(), String>>,
}

impl SyncSettings {
    pub(crate) fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let path = Self::path()?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Sync settings path has no parent"))?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        crate::hosts::safe_write(&path, &bytes)
    }

    fn path() -> io::Result<PathBuf> {
        crate::metadata::Metadata::data_dir().map(|directory| directory.join("sync.json"))
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let remote = self.remote.trim();
        let branch = self.branch.trim();
        if remote.is_empty() || branch.is_empty() {
            return Err("Enter both a Git remote and branch.".into());
        }
        if remote.chars().any(char::is_whitespace)
            || remote.starts_with('-')
            || branch.starts_with('-')
            || branch.chars().any(char::is_whitespace)
            || branch.contains("..")
            || branch.contains("//")
            || branch.ends_with('/')
            || branch.ends_with('.')
            || branch.contains("@{")
            || branch.contains('\\')
        {
            return Err("Remote or branch contains an invalid value.".into());
        }
        if let Ok(url) = url::Url::parse(remote) {
            if url.password().is_some() {
                return Err("Do not embed credentials in the remote URL; use Git credential helper or SSH agent.".into());
            }
            if !matches!(url.scheme(), "https" | "ssh" | "file") {
                return Err("Use an HTTPS, SSH, or local file remote.".into());
            }
        } else if !(Path::new(remote).is_absolute()
            || (remote.contains(':') && !remote.starts_with(':')))
        {
            return Err("Enter a valid HTTPS/SSH remote or local repository path.".into());
        }
        let reference = format!("refs/heads/{branch}");
        let output = std::process::Command::new("git")
            .args(["check-ref-format", &reference])
            .output()
            .map_err(|_| "Could not start Git. Check that Git is installed.".to_string())?;
        if !output.status.success() {
            return Err("Enter a valid Git branch name.".into());
        }
        Ok(())
    }
}

impl ConnectionTest {
    pub(crate) fn start(settings: SyncSettings) -> Result<Self, String> {
        settings.validate()?;
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("lazyterm-git-connection-test".into())
            .spawn(move || {
                let result = (|| {
                    let mut child = Command::new("git")
                    .args([
                        "-c",
                        "core.sshCommand=ssh -oBatchMode=yes -oStrictHostKeyChecking=yes",
                        "ls-remote",
                        "--heads",
                        "--",
                    ])
                    .arg(settings.remote.trim())
                    .arg(settings.branch.trim())
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .env("GCM_INTERACTIVE", "Never")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| "Could not start Git. Check that Git is installed.".to_string())?;
                    let start = Instant::now();
                    loop {
                        match child.try_wait() {
                            Ok(Some(status)) if status.success() => return Ok(()),
                            Ok(Some(_)) => return Err("Git could not access that remote/branch. Check the address, permissions, and authentication.".into()),
                            Ok(None) if start.elapsed() < Duration::from_secs(45) => {
                                thread::sleep(Duration::from_millis(20));
                            }
                            Ok(None) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Err("Git connection test timed out after 45 seconds.".into());
                            }
                            Err(_) => return Err("Could not check Git connection status.".into()),
                        }
                    }
                })();
                let _ = sender.send(result);
            })
            .map_err(|_| "Could not start the Git connection test.".to_string())?;
        Ok(Self { receiver })
    }

    pub(crate) fn try_result(&self) -> Result<Option<Result<(), String>>, TryRecvError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// A sync record key must be stable across devices, such as `host:<alias>` or
/// `project:<uuid>`. Machine-local state must not be included in record values.
#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct SyncSnapshot {
    pub(crate) records: BTreeMap<String, Value>,
}

pub(crate) struct SyncPlan {
    /// Contains local values at conflicted paths as placeholders; never apply
    /// this map while `conflicts` is non-empty.
    pub(crate) merged: BTreeMap<String, Value>,
    pub(crate) conflicts: Vec<SyncConflict>,
}

pub(crate) struct SyncConflict {
    pub(crate) path: String,
    base: Option<Value>,
    pub(crate) local: Option<Value>,
    pub(crate) remote: Option<Value>,
}

impl SyncConflict {
    #[cfg(test)]
    fn values(&self) -> (&Option<Value>, &Option<Value>, &Option<Value>) {
        (&self.base, &self.local, &self.remote)
    }
}

pub(crate) fn plan_sync(
    base: &SyncSnapshot,
    local: &SyncSnapshot,
    remote: &SyncSnapshot,
) -> SyncPlan {
    let mut conflicts = Vec::new();
    let keys = base
        .records
        .keys()
        .chain(local.records.keys())
        .chain(remote.records.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut merged = BTreeMap::new();

    for key in keys {
        let value = merge_value(
            base.records.get(&key),
            local.records.get(&key),
            remote.records.get(&key),
            &format!("/{}", escape_pointer(&key)),
            &mut conflicts,
        );
        if let Some(value) = value {
            merged.insert(key, value);
        }
    }

    SyncPlan { merged, conflicts }
}

fn merge_value(
    base: Option<&Value>,
    local: Option<&Value>,
    remote: Option<&Value>,
    path: &str,
    conflicts: &mut Vec<SyncConflict>,
) -> Option<Value> {
    if local == remote {
        return local.cloned();
    }
    if local == base {
        return remote.cloned();
    }
    if remote == base {
        return local.cloned();
    }

    if let (Some(Value::Object(base)), Some(Value::Object(local)), Some(Value::Object(remote))) =
        (base, local, remote)
    {
        let keys = base
            .keys()
            .chain(local.keys())
            .chain(remote.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut merged = serde_json::Map::new();
        for key in keys {
            let child = merge_value(
                base.get(&key),
                local.get(&key),
                remote.get(&key),
                &format!("{path}/{}", escape_pointer(&key)),
                conflicts,
            );
            if let Some(child) = child {
                merged.insert(key, child);
            }
        }
        return Some(Value::Object(merged));
    }

    conflicts.push(SyncConflict {
        path: path.to_owned(),
        base: base.cloned(),
        local: local.cloned(),
        remote: remote.cloned(),
    });
    local.cloned()
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(records: &[(&str, Value)]) -> SyncSnapshot {
        SyncSnapshot {
            records: records
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
        }
    }

    #[test]
    fn merges_independent_records_and_object_fields() {
        let base = snapshot(&[("host:web", json!({"user":"deploy","port":22}))]);
        let local = snapshot(&[
            ("host:web", json!({"user":"admin","port":22})),
            ("project:one", json!({"name":"one"})),
        ]);
        let remote = snapshot(&[
            ("host:web", json!({"user":"deploy","port":2222})),
            ("tunnel:db", json!({"port":3306})),
        ]);

        let plan = plan_sync(&base, &local, &remote);

        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.merged["host:web"], json!({"user":"admin","port":2222}));
        assert_eq!(plan.merged["project:one"], json!({"name":"one"}));
        assert_eq!(plan.merged["tunnel:db"], json!({"port":3306}));
    }

    #[test]
    fn detects_divergent_edits_and_keeps_conflict_values_internal() {
        let base = snapshot(&[("project:one", json!({"name":"one","command":"run"}))]);
        let local = snapshot(&[(
            "project:one",
            json!({"name":"local name","command":"run-local"}),
        )]);
        let remote = snapshot(&[("project:one", json!({"name":"one","command":"run-remote"}))]);

        let plan = plan_sync(&base, &local, &remote);

        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].path, "/project:one/command");
        assert_eq!(
            plan.conflicts[0].values(),
            (
                &Some(json!("run")),
                &Some(json!("run-local")),
                &Some(json!("run-remote"))
            )
        );
        assert_eq!(
            plan.merged["project:one"],
            json!({"name":"local name","command":"run-local"})
        );
    }

    #[test]
    fn detects_delete_modify_conflicts_without_silently_deleting() {
        let base = snapshot(&[("host:web", json!({"host_name":"web.internal"}))]);
        let local = snapshot(&[]);
        let remote = snapshot(&[("host:web", json!({"host_name":"web.prod"}))]);

        let plan = plan_sync(&base, &local, &remote);

        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].path, "/host:web");
        assert_eq!(
            plan.conflicts[0].values(),
            (
                &Some(json!({"host_name":"web.internal"})),
                &None,
                &Some(json!({"host_name":"web.prod"}))
            )
        );
        assert!(plan.merged.is_empty());
    }

    #[test]
    fn applies_uncontested_deletions_and_same_value_edits() {
        let base = snapshot(&[
            ("project:gone", json!({"name":"gone"})),
            ("host:web", json!({"user":"deploy"})),
        ]);
        let local = snapshot(&[("host:web", json!({"user":"admin"}))]);
        let remote = snapshot(&[
            ("project:gone", json!({"name":"gone"})),
            ("host:web", json!({"user":"admin"})),
        ]);

        let plan = plan_sync(&base, &local, &remote);

        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.merged, local.records);
    }

    #[test]
    fn sync_remote_requires_remote_and_branch_and_rejects_embedded_credentials() {
        let mut settings = SyncSettings::default();
        assert!(settings.validate().is_err());

        settings.remote = "https://user:secret@example.com/team/config.git".into();
        settings.branch = "main".into();
        assert!(settings.validate().unwrap_err().contains("credentials"));

        settings.remote = "git@example.com:team/config.git".into();
        assert!(settings.validate().is_ok());
    }
}
