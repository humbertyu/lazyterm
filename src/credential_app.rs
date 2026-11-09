use crate::{
    app::{App, HostGroup, HostPickerItem, NavSection},
    credentials::{self, Credential, CredentialItem, CredentialKind, CredentialSource},
    project_credentials::PreparedPassword,
};
use crossterm::event::KeyCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Default)]
pub struct CredentialState {
    pub open: bool,
    pub selected: usize,
    pub target: Option<String>,
    pub upload_only: bool,
    pub form: Option<CredentialForm>,
    pub deleting: bool,
    pub picker: bool,
    pub picker_upload: bool,
    pub picker_group: Option<HostGroup>,
    pub picker_selected: usize,
    pub confirm_upload: Option<String>,
    pub message: Option<String>,
    pending: Option<Receiver<Result<CredentialJob, String>>>,
    cleanup: Vec<Receiver<Result<String, String>>>,
    upload_cancel: Option<Arc<AtomicBool>>,
    pub(crate) discovered_keys: Vec<Credential>,
}

impl Drop for CredentialState {
    fn drop(&mut self) {
        if let Some(cancel) = &self.upload_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::NavSection,
        hosts::{Host, HostCatalog},
        metadata::Metadata,
        projects::tests::Fixture,
    };
    use std::time::{Duration, Instant};

    fn app(fixture: &Fixture) -> App {
        let mut app = App::with_catalog(
            HostCatalog {
                hosts: vec![Host {
                    alias: "web".into(),
                    hostname: Some("example.com".into()),
                    user: Some("deploy".into()),
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                    managed: false,
                }],
                error: None,
            },
            Ok(Metadata::default()),
        );
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.open_credentials(None, false);
        app
    }

    fn finish(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.credentials_busy() || !app.credential_manager.cleanup.is_empty() {
            app.poll_credentials();
            assert!(Instant::now() < deadline, "credential worker timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn save_password(app: &mut App, secret: &str) {
        app.handle_credentials_key(KeyCode::Char('a'));
        let form = app.credential_manager.form.as_mut().unwrap();
        form.fields = vec!["Production".into(), "deploy".into(), secret.into()];
        form.selected = 2;
        app.handle_credentials_key(KeyCode::Enter);
        finish(app);
    }

    #[test]
    fn password_creation_assignment_replacement_unlink_and_cleanup() {
        let fixture = Fixture::new("credentials");
        let mut app = app(&fixture);
        save_password(&mut app, "first secret");
        let id = app.metadata.credentials[0].id;
        let old_ref = app.metadata.credentials[0].password_ref.clone().unwrap();
        assert!(old_ref.starts_with("credential-"));
        assert!(
            !std::fs::read_to_string(fixture.root.join("hosts.json"))
                .unwrap()
                .contains("first secret")
        );
        app.handle_credentials_key(KeyCode::Enter);
        assert!(app.credential_manager.picker);
        app.handle_credentials_key(KeyCode::Enter);
        assert!(app.credential_manager.picker_group.is_some());
        app.handle_credentials_key(KeyCode::Enter);
        assert_eq!(app.metadata.host_passwords.get("web"), Some(&id));
        assert_eq!(
            app.credential_hosts(&app.metadata.credentials[0]),
            vec!["web"]
        );
        app.handle_credentials_key(KeyCode::Char('e'));
        let form = app.credential_manager.form.as_mut().unwrap();
        form.fields[2] = "replacement".into();
        form.selected = 2;
        app.handle_credentials_key(KeyCode::Enter);
        finish(&mut app);
        assert!(!crate::project_credentials::tests::password_exists(
            &old_ref
        ));
        let replacement = app.host_password_reference("web").unwrap().to_string();
        assert_eq!(
            &**crate::project_credentials::read_password(&replacement).unwrap(),
            "replacement"
        );
        app.handle_credentials_key(KeyCode::Char('d'));
        app.handle_credentials_key(KeyCode::Enter);
        assert_eq!(app.metadata.credentials.len(), 1);
        app.open_credentials(Some("web".into()), false);
        app.handle_credentials_key(KeyCode::Char('x'));
        app.handle_credentials_key(KeyCode::Char('d'));
        app.handle_credentials_key(KeyCode::Enter);
        finish(&mut app);
        assert!(app.metadata.credentials.is_empty());
        assert!(!crate::project_credentials::tests::password_exists(
            &replacement
        ));
    }

    #[test]
    fn failed_save_keeps_old_password_and_association() {
        let fixture = Fixture::new("credential-rollback");
        let mut app = app(&fixture);
        save_password(&mut app, "old secret");
        let reference = app.metadata.credentials[0].password_ref.clone().unwrap();
        app.metadata_save_path = Some(fixture.root.clone());
        app.open_credentials(Some("web".into()), false);
        app.handle_credentials_key(KeyCode::Enter);
        assert!(app.metadata.host_passwords.is_empty());
        app.handle_credentials_key(KeyCode::Char('e'));
        let form = app.credential_manager.form.as_mut().unwrap();
        form.fields[2] = "replacement".into();
        form.selected = 2;
        app.handle_credentials_key(KeyCode::Enter);
        finish(&mut app);
        assert_eq!(
            app.metadata.credentials[0].password_ref.as_deref(),
            Some(reference.as_str())
        );
        assert_eq!(
            &**crate::project_credentials::read_password(&reference).unwrap(),
            "old secret"
        );
        assert!(app.credential_manager.form.is_some());
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.credential_manager.form = None;
        app.handle_credentials_key(KeyCode::Char('d'));
        app.handle_credentials_key(KeyCode::Enter);
        finish(&mut app);
    }

    #[test]
    fn project_keychain_credentials_are_listed_deduplicated_and_assignable() {
        let fixture = Fixture::new("project-keychain-list");
        let mut app = app(&fixture);
        let mut project = crate::projects::ProjectDefinition {
            id: 17,
            name: "Dashboard".into(),
            ..Default::default()
        };
        project.links = vec![
            crate::projects::ProjectLink {
                id: 1,
                name: "Home".into(),
                url: "https://example.com/".into(),
                username: "admin".into(),
                password_ref: Some("project-password-ref".into()),
                password_storage: crate::projects::PasswordStorage::Keychain,
                password: None,
            },
            crate::projects::ProjectLink {
                id: 2,
                name: "Admin".into(),
                url: "https://example.com/admin".into(),
                username: "admin".into(),
                password_ref: Some("project-password-ref".into()),
                password_storage: crate::projects::PasswordStorage::Keychain,
                password: None,
            },
        ];
        app.metadata.projects.push(project);

        let listed = app.visible_credentials();
        let project_items = listed
            .iter()
            .filter(|item| matches!(item.source, CredentialSource::ProjectLink { .. }))
            .collect::<Vec<_>>();
        assert_eq!(project_items.len(), 1);
        assert!(matches!(
            project_items[0].source,
            CredentialSource::ProjectLink {
                project_id: 17,
                link_id: 1
            }
        ));
        assert_eq!(
            project_items[0].credential.password_ref.as_deref(),
            Some("project-password-ref")
        );

        app.metadata_save_path = Some(fixture.root.clone());
        app.assign_credential("web".into());
        assert!(app.metadata.credentials.is_empty());
        assert!(app.metadata.host_passwords.is_empty());
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.assign_credential("web".into());
        let host_credential_id = app.metadata.host_passwords["web"];
        assert_eq!(
            app.host_password_reference("web"),
            Some("project-password-ref")
        );
        assert!(app.metadata.credentials.iter().any(|credential| {
            credential.id == host_credential_id
                && credential.password_ref.as_deref() == Some("project-password-ref")
        }));
    }

    #[test]
    fn scanned_keys_deduplicate_registered_paths_and_refresh_on_entry() {
        let fixture = Fixture::new("key-refresh");
        let mut app = app(&fixture);
        let mut key =
            credentials::generate_key(&fixture.root.join("key"), 42, "Saved".into()).unwrap();
        app.metadata.credentials.push(key.clone());
        key.id = 0;
        key.public_key = "updated public key".into();
        key.fingerprint = "updated fingerprint".into();
        app.credential_manager.discovered_keys = vec![key];
        let items = app.visible_credentials();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].credential.id, 42);
        assert_eq!(items[0].credential.public_key, "updated public key");

        // A new entry replaces the scan cache instead of retaining deleted discoveries.
        app.metadata.credentials.clear();
        app.credential_manager.open = false;
        app.switch_section(NavSection::Credentials);
        assert!(
            !app.visible_credentials()
                .iter()
                .any(|item| item.credential.fingerprint == "updated fingerprint")
        );
        app.credential_manager.discovered_keys.clear();
        app.open_credentials(Some("web".into()), false);
        assert_eq!(
            app.credential_manager.discovered_keys,
            credentials::discover_local_keys()
        );
    }

    #[test]
    fn manager_owns_shortcuts_and_host_removal_preserves_saved_records() {
        let fixture = Fixture::new("credential-modal");
        let mut app = app(&fixture);
        save_password(&mut app, "secret");
        app.metadata
            .host_passwords
            .insert("web".into(), app.metadata.credentials[0].id);
        app.handle_event(crossterm::event::KeyEvent::new(
            KeyCode::Char('4'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.section, NavSection::Hosts);
        let unavailable = HostCatalog {
            error: Some("unreadable config".into()),
            ..Default::default()
        };
        assert!(!app.metadata.reconcile(&unavailable));
        assert!(app.host_password_reference("web").is_some());
        assert!(app.metadata.reconcile(&HostCatalog::default()));
        assert!(app.metadata.host_passwords.is_empty());
        assert_eq!(app.metadata.credentials.len(), 1);
        app.handle_credentials_key(KeyCode::Char('d'));
        app.handle_credentials_key(KeyCode::Enter);
        finish(&mut app);
    }

    #[test]
    fn credentials_tab_supports_workspace_navigation_and_owns_only_modal_input() {
        use crossterm::event::{KeyEvent, KeyModifiers};
        let fixture = Fixture::new("credentials-tab");
        let mut app = app(&fixture);
        app.handle_credentials_key(KeyCode::Esc);
        let press = |app: &mut App, code| app.handle_event(KeyEvent::new(code, KeyModifiers::NONE));
        press(&mut app, KeyCode::F(7));
        assert_eq!(app.section, NavSection::Credentials);
        assert!(!app.credentials_modal());
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.section, NavSection::Hosts);
        app.handle_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.section, NavSection::Credentials);
        press(&mut app, KeyCode::Char(','));
        assert_eq!(app.section, NavSection::Settings);
        app.selected_setting = 8;
        press(&mut app, KeyCode::Enter);
        assert!(!app.visible_sections().contains(&NavSection::Credentials));
        press(&mut app, KeyCode::Enter);
        assert!(app.visible_sections().contains(&NavSection::Credentials));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.section, NavSection::Credentials);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('7'));
        press(&mut app, KeyCode::F(1));
        assert_eq!(app.section, NavSection::Credentials);
        assert_eq!(app.credential_manager.form.as_ref().unwrap().fields[0], "7");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }
}

pub enum CredentialJob {
    Password(Credential, PreparedPassword),
    Key(Credential),
    Message(String),
}

#[derive(Clone, Copy, PartialEq)]
pub enum FormKind {
    Password,
    Generate,
    Import,
}

pub struct CredentialForm {
    pub kind: FormKind,
    pub id: Option<u64>,
    pub fields: Vec<String>,
    pub selected: usize,
    pub error: Option<String>,
}

impl Drop for CredentialForm {
    fn drop(&mut self) {
        if self.kind == FormKind::Password {
            self.fields.iter_mut().for_each(String::zeroize);
        }
    }
}

impl CredentialForm {
    pub fn labels(&self) -> &[&str] {
        match self.kind {
            FormKind::Password => &[
                "Name",
                "Username (optional)",
                "Password (blank keeps saved)",
            ],
            FormKind::Generate => &["Name", "New directory (parent must exist)"],
            FormKind::Import => &["Name", "Private key path (.pub required)"],
        }
    }
}

impl App {
    pub fn credentials_modal(&self) -> bool {
        let state = &self.credential_manager;
        state.open
            || (self.section == NavSection::Credentials
                && (state.form.is_some()
                    || state.picker
                    || state.deleting
                    || state.confirm_upload.is_some()))
    }

    pub fn open_credentials(&mut self, target: Option<String>, upload_only: bool) {
        self.credential_manager.open = true;
        self.credential_manager.target = target;
        self.credential_manager.upload_only = upload_only;
        self.credential_manager.selected = 0;
        self.credential_manager.message = None;
        self.refresh_discovered_keys();
    }

    pub fn visible_credentials(&self) -> Vec<CredentialItem> {
        let mut credentials = self
            .metadata
            .credentials
            .iter()
            .cloned()
            .map(|mut credential| {
                if credential.kind == CredentialKind::SshKey
                    && let Some(current) =
                        self.credential_manager.discovered_keys.iter().find(|key| {
                            key.private_key_path
                                .as_deref()
                                .zip(credential.private_key_path.as_deref())
                                .is_some_and(|(left, right)| credentials::same_path(left, right))
                        })
                {
                    credential.public_key.clone_from(&current.public_key);
                    credential.fingerprint.clone_from(&current.fingerprint);
                }
                CredentialItem {
                    credential,
                    source: CredentialSource::Managed,
                }
            })
            .collect::<Vec<_>>();
        let mut references = credentials
            .iter()
            .filter_map(|item| item.credential.password_ref.as_deref())
            .map(str::to_string)
            .collect::<std::collections::HashSet<_>>();
        let mut next_id = credentials
            .iter()
            .map(|item| item.credential.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        for project in &self.metadata.projects {
            for link in &project.links {
                if link.password_storage != crate::projects::PasswordStorage::Keychain {
                    continue;
                }
                let Some(reference) = link.password_ref.as_deref() else {
                    continue;
                };
                if !references.insert(reference.to_string()) {
                    continue;
                }
                credentials.push(CredentialItem {
                    credential: Credential {
                        id: next_id,
                        name: format!("{}/{}", project.name, link.name),
                        kind: CredentialKind::Password,
                        username: link.username.clone(),
                        password_ref: Some(reference.to_string()),
                        private_key_path: None,
                        public_key: String::new(),
                        fingerprint: String::new(),
                    },
                    source: CredentialSource::ProjectLink {
                        project_id: project.id,
                        link_id: link.id,
                    },
                });
                next_id = next_id.saturating_add(1);
            }
        }
        for credential in &self.credential_manager.discovered_keys {
            let already_listed = credentials.iter().any(|item| {
                item.credential.kind == CredentialKind::SshKey
                    && item
                        .credential
                        .private_key_path
                        .as_deref()
                        .zip(credential.private_key_path.as_deref())
                        .is_some_and(|(left, right)| credentials::same_path(left, right))
            });
            if !already_listed {
                credentials.push(CredentialItem {
                    credential: credential.clone(),
                    source: CredentialSource::DiscoveredKey,
                });
            }
        }
        credentials.sort_by_key(|item| match item.credential.kind {
            CredentialKind::Password => 0,
            CredentialKind::SshKey => 1,
        });
        credentials.retain(|item| {
            !self.credential_manager.upload_only || item.credential.kind == CredentialKind::SshKey
        });
        credentials
    }

    pub fn selected_credential(&self) -> Option<CredentialItem> {
        self.visible_credentials()
            .get(self.credential_manager.selected)
            .cloned()
    }

    pub fn host_password_reference(&self, alias: &str) -> Option<&str> {
        let id = self.metadata.host_passwords.get(alias)?;
        self.metadata
            .credentials
            .iter()
            .find(|item| item.id == *id && item.kind == CredentialKind::Password)?
            .password_ref
            .as_deref()
    }

    pub fn credential_hosts(&self, credential: &Credential) -> Vec<String> {
        self.hosts
            .hosts
            .iter()
            .filter(|host| match credential.kind {
                CredentialKind::Password => {
                    self.metadata.host_passwords.get(&host.alias) == Some(&credential.id)
                }
                CredentialKind::SshKey => host
                    .identity_file
                    .as_deref()
                    .zip(credential.private_key_path.as_deref())
                    .is_some_and(|(value, path)| {
                        credentials::same_path(
                            &credentials::expand_path(&value.replace("%%", "%")),
                            path,
                        )
                    }),
            })
            .map(|host| host.alias.clone())
            .collect()
    }

    pub fn credential_item_hosts(&self, item: &CredentialItem) -> Vec<String> {
        match item.source {
            CredentialSource::ProjectLink { .. } => self
                .hosts
                .hosts
                .iter()
                .filter(|host| {
                    self.host_password_reference(&host.alias)
                        == item.credential.password_ref.as_deref()
                })
                .map(|host| host.alias.clone())
                .collect(),
            _ => self.credential_hosts(&item.credential),
        }
    }

    pub(crate) fn refresh_discovered_keys(&mut self) {
        self.credential_manager.discovered_keys = credentials::discover_local_keys();
        self.credential_manager.selected = self
            .credential_manager
            .selected
            .min(self.visible_credentials().len().saturating_sub(1));
    }

    pub fn credential_picker_items(&self) -> Vec<HostPickerItem> {
        if let Some(group) = &self.credential_manager.picker_group {
            self.group_hosts(group)
                .into_iter()
                .map(|host| HostPickerItem::Host(host.alias.clone()))
                .collect()
        } else {
            self.visible_groups()
                .into_iter()
                .map(HostPickerItem::Group)
                .chain(
                    self.hosts
                        .hosts
                        .iter()
                        .map(|host| HostPickerItem::Host(host.alias.clone())),
                )
                .collect()
        }
    }

    pub fn credentials_busy(&self) -> bool {
        self.credential_manager.pending.is_some()
    }

    pub fn poll_credentials(&mut self) {
        self.credential_manager
            .cleanup
            .retain(|receiver| match receiver.try_recv() {
                Err(TryRecvError::Empty) => true,
                Ok(Err(error)) => {
                    self.credential_manager.message = Some(error.clone());
                    self.notice = Some(error);
                    false
                }
                Ok(Ok(message)) => {
                    if !message.is_empty() {
                        self.credential_manager.message = Some(message);
                    }
                    false
                }
                _ => false,
            });
        let Some(receiver) = &self.credential_manager.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Credential worker stopped.".into()),
        };
        self.credential_manager.pending = None;
        self.credential_manager.upload_cancel = None;
        match result {
            Ok(CredentialJob::Message(message)) => {
                self.credential_manager.message = Some(message);
            }
            Ok(job) => {
                let (credential, mut prepared) = match job {
                    CredentialJob::Password(value, prepared) => (value, Some(prepared)),
                    CredentialJob::Key(value) => (value, None),
                    _ => unreachable!(),
                };
                let previous = self.metadata.credentials.clone();
                let old_ref = previous
                    .iter()
                    .find(|item| item.id == credential.id)
                    .and_then(|item| item.password_ref.clone());
                let name = credential.name.clone();
                let saved_id = credential.id;
                if let Some(item) = self
                    .metadata
                    .credentials
                    .iter_mut()
                    .find(|item| item.id == credential.id)
                {
                    *item = credential;
                } else {
                    self.metadata.credentials.push(credential);
                }
                if self.save_metadata() {
                    if let Some(prepared) = &mut prepared {
                        prepared.commit();
                    }
                    if let Some(reference) = old_ref {
                        self.cleanup_credential_reference(reference);
                    }
                    self.credential_manager.form = None;
                    self.credential_manager.selected = self
                        .visible_credentials()
                        .iter()
                        .position(|item| {
                            item.credential.id == saved_id
                                && item.source == CredentialSource::Managed
                        })
                        .unwrap_or(0);
                    self.credential_manager.message = Some(format!("Saved {name}."));
                } else {
                    self.metadata.credentials = previous;
                    self.credential_manager.message = self.notice.clone();
                    // Generated key files are retained so a failed metadata save cannot destroy a key.
                    if prepared.is_none()
                        && self
                            .credential_manager
                            .form
                            .as_ref()
                            .is_some_and(|form| form.kind != FormKind::Password)
                    {
                        self.credential_manager.message = Some("Metadata save failed; key files remain at the specified path. Import them after resolving the error.".into());
                    }
                    if let Some(form) = &mut self.credential_manager.form {
                        form.error = self.credential_manager.message.clone();
                    }
                }
            }
            Err(error) => {
                if let Some(form) = &mut self.credential_manager.form {
                    form.error = Some(error.clone());
                }
                self.credential_manager.message = Some(error);
            }
        }
    }

    fn cleanup_credential_reference(&mut self, reference: String) {
        let used = self
            .metadata
            .credentials
            .iter()
            .any(|item| item.password_ref.as_ref() == Some(&reference))
            || self
                .metadata
                .projects
                .iter()
                .flat_map(|project| &project.links)
                .any(|link| link.password_ref.as_ref() == Some(&reference));
        if !used {
            self.credential_manager
                .cleanup
                .push(crate::project_credentials::remove_unused(vec![reference]));
        }
    }

    fn credential_form(&mut self, kind: FormKind, editing: Option<Credential>) {
        let fields = if let Some(item) = &editing {
            if kind == FormKind::Password {
                vec![item.name.clone(), item.username.clone(), String::new()]
            } else {
                vec![
                    item.name.clone(),
                    item.private_key_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                ]
            }
        } else if kind == FormKind::Password {
            vec![String::new(), String::new(), String::new()]
        } else {
            vec![
                String::new(),
                if kind == FormKind::Generate {
                    format!(
                        "~/.ssh/lazyterm-key-{}",
                        self.metadata
                            .credentials
                            .iter()
                            .map(|item| item.id)
                            .max()
                            .unwrap_or(0)
                            + 1
                    )
                } else {
                    "~/.ssh/".into()
                },
            ]
        };
        self.credential_manager.form = Some(CredentialForm {
            kind,
            id: editing.map(|item| item.id),
            fields,
            selected: 0,
            error: None,
        });
    }

    fn save_credential_form(&mut self) {
        let Some(form) = &mut self.credential_manager.form else {
            return;
        };
        if form.fields[0].trim().is_empty() {
            form.error = Some("Name is required.".into());
            return;
        }
        let id = form.id.unwrap_or_else(|| {
            self.metadata
                .credentials
                .iter()
                .map(|item| item.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
        });
        let name = form.fields[0].trim().to_string();
        let kind = form.kind;
        let path = credentials::expand_path(&form.fields[1]);
        let username = form.fields[1].trim().to_string();
        let password = if kind == FormKind::Password {
            Zeroizing::new(std::mem::take(&mut form.fields[2]))
        } else {
            Zeroizing::new(String::new())
        };
        let existing = self
            .metadata
            .credentials
            .iter()
            .find(|item| item.id == id)
            .cloned();
        if kind == FormKind::Password && password.is_empty() && existing.is_none() {
            form.error = Some("Enter a password.".into());
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.credential_manager.pending = Some(receiver);
        self.credential_manager.message = Some("Working...".into());
        std::thread::spawn(move || {
            let result = match kind {
                FormKind::Password => {
                    if password.is_empty() {
                        let mut value = existing.unwrap();
                        value.name = name;
                        value.username = username;
                        Ok(CredentialJob::Key(value))
                    } else {
                        crate::project_credentials::prepare_credential(password)
                            .recv()
                            .map_err(|_| "Password worker stopped.".to_string())
                            .and_then(|result| result)
                            .map(|prepared| {
                                let value = Credential {
                                    id,
                                    name,
                                    kind: CredentialKind::Password,
                                    username,
                                    password_ref: Some(prepared.reference.clone()),
                                    private_key_path: None,
                                    public_key: String::new(),
                                    fingerprint: String::new(),
                                };
                                CredentialJob::Password(value, prepared)
                            })
                    }
                }
                FormKind::Generate => {
                    credentials::generate_key(&path, id, name).map(CredentialJob::Key)
                }
                FormKind::Import => {
                    credentials::import_key(&path, id, name).map(CredentialJob::Key)
                }
            };
            let _ = sender.send(result);
        });
    }

    fn assign_credential(&mut self, alias: String) {
        let Some(item) = self.selected_credential() else {
            return;
        };
        let credential = item.credential;
        if credential.kind == CredentialKind::SshKey {
            let Some(path) = &credential.private_key_path else {
                return;
            };
            if !path.is_file() {
                self.credential_manager.message = Some("Private key no longer exists.".into());
                return;
            }
            match self.hosts.set_identity(&alias, path) {
                Ok(()) => {
                    self.credential_manager.message = Some(format!(
                        "{} now uses {} (IdentityFile saved to SSH config).",
                        alias, credential.name
                    ))
                }
                Err(error) => {
                    self.credential_manager.message =
                        Some(format!("Could not assign SSH key: {error}"))
                }
            }
        } else {
            let previous_credentials = self.metadata.credentials.clone();
            let id = match item.source {
                CredentialSource::Managed => credential.id,
                CredentialSource::ProjectLink { .. } => {
                    let id = self
                        .metadata
                        .credentials
                        .iter()
                        .map(|item| item.id)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1);
                    let mut linked = credential.clone();
                    linked.id = id;
                    self.metadata.credentials.push(linked);
                    id
                }
                CredentialSource::DiscoveredKey => return,
            };
            let previous = self.metadata.host_passwords.clone();
            self.metadata.host_passwords.insert(alias.clone(), id);
            if self.save_metadata() {
                self.credential_manager.message = Some(format!(
                    "{} uses password {}. SSH username remains configured in ~/.ssh/config.",
                    alias, credential.name
                ));
            } else {
                self.metadata.host_passwords = previous;
                self.metadata.credentials = previous_credentials;
                self.credential_manager.message = self.notice.clone();
            }
        }
    }

    fn start_public_key_upload(&mut self, alias: String) {
        let Some(item) = self.selected_credential() else {
            return;
        };
        let credential = item.credential;
        let password_ref = self.host_password_reference(&alias).map(str::to_string);
        let (sender, receiver) = mpsc::channel();
        self.credential_manager.pending = Some(receiver);
        self.credential_manager.message = Some(format!("Uploading public key to {alias}..."));
        let cancelled = Arc::new(AtomicBool::new(false));
        self.credential_manager.upload_cancel = Some(cancelled.clone());
        std::thread::spawn(move || {
            let _ = sender.send(
                credentials::upload(
                    &credential.public_key,
                    &alias,
                    password_ref.as_deref(),
                    None,
                    &cancelled,
                )
                .map(CredentialJob::Message),
            );
        });
    }

    pub fn handle_credentials_key(&mut self, key: KeyCode) {
        if self.credentials_busy() {
            if key == KeyCode::Esc
                && let Some(cancel) = &self.credential_manager.upload_cancel
            {
                cancel.store(true, Ordering::Relaxed);
                self.credential_manager.message = Some("Cancelling upload...".into());
            }
            return;
        }
        if self.credential_manager.form.is_some() {
            let form = self.credential_manager.form.as_mut().unwrap();
            let count = form.fields.len();
            match key {
                KeyCode::Esc => self.credential_manager.form = None,
                KeyCode::Up | KeyCode::BackTab => {
                    form.selected = (form.selected + count - 1) % count
                }
                KeyCode::Down | KeyCode::Tab => form.selected = (form.selected + 1) % count,
                KeyCode::Enter => {
                    if form.selected + 1 == count {
                        self.save_credential_form();
                    } else {
                        form.selected += 1;
                    }
                }
                KeyCode::Backspace => {
                    form.fields[form.selected].pop();
                }
                KeyCode::Char(c) if !c.is_control() => {
                    form.fields[form.selected].push(c);
                    form.error = None;
                }
                _ => {}
            }
            return;
        }
        if let Some(alias) = self.credential_manager.confirm_upload.clone() {
            match key {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.credential_manager.confirm_upload = None;
                    self.start_public_key_upload(alias);
                }
                KeyCode::Esc | KeyCode::Char('n') => self.credential_manager.confirm_upload = None,
                _ => {}
            }
            return;
        }
        if self.credential_manager.deleting {
            match key {
                KeyCode::Enter | KeyCode::Char('y') => {
                    if let Some(item) = self.selected_credential() {
                        if item.source != CredentialSource::Managed {
                            self.credential_manager.message = Some(match item.source {
                                CredentialSource::ProjectLink { .. } => "This password belongs to a Project Web Link. Edit or remove it from Projects.".into(),
                                CredentialSource::DiscoveredKey => "This key is discovered from ~/.ssh. Remove or rename the files there, then refresh.".into(),
                                CredentialSource::Managed => String::new(),
                            });
                            self.credential_manager.deleting = false;
                            return;
                        }
                        let value = item.credential;
                        let associated = self.credential_hosts(&value);
                        if value.kind == CredentialKind::Password && !associated.is_empty() {
                            self.credential_manager.message = Some(format!(
                                "Still used by {}; unlink it from each Host first (Hosts / k / x).",
                                associated.join(", ")
                            ));
                        } else {
                            let previous = self.metadata.credentials.clone();
                            self.metadata.credentials.retain(|item| item.id != value.id);
                            if self.save_metadata() {
                                if let Some(reference) = value.password_ref {
                                    self.cleanup_credential_reference(reference);
                                }
                                self.credential_manager.message = Some("Removed credential record. SSH key files and IdentityFile entries are retained.".into());
                            } else {
                                self.metadata.credentials = previous;
                                self.credential_manager.message = self.notice.clone();
                            }
                        }
                    }
                    self.credential_manager.deleting = false;
                }
                KeyCode::Esc | KeyCode::Char('n') => self.credential_manager.deleting = false,
                _ => {}
            }
            return;
        }
        if self.credential_manager.picker {
            let items = self.credential_picker_items();
            match key {
                KeyCode::Esc | KeyCode::Backspace => {
                    if self.credential_manager.picker_group.take().is_none() {
                        self.credential_manager.picker = false;
                    }
                    self.credential_manager.picker_selected = 0;
                }
                KeyCode::Down => {
                    self.credential_manager.picker_selected =
                        (self.credential_manager.picker_selected + 1)
                            .min(items.len().saturating_sub(1))
                }
                KeyCode::Up => {
                    self.credential_manager.picker_selected =
                        self.credential_manager.picker_selected.saturating_sub(1)
                }
                KeyCode::Enter => match items.get(self.credential_manager.picker_selected).cloned()
                {
                    Some(HostPickerItem::Group(group)) => {
                        self.credential_manager.picker_group = Some(group);
                        self.credential_manager.picker_selected = 0;
                    }
                    Some(HostPickerItem::Host(alias)) => {
                        self.credential_manager.picker = false;
                        if self.credential_manager.picker_upload {
                            self.credential_manager.confirm_upload = Some(alias);
                        } else {
                            self.assign_credential(alias);
                        }
                    }
                    None => {}
                },
                _ => {}
            }
            return;
        }
        let count = self.visible_credentials().len();
        self.credential_manager.selected = self
            .credential_manager
            .selected
            .min(count.saturating_sub(1));
        match key {
            KeyCode::Esc | KeyCode::Char('q') if self.credential_manager.open => {
                self.credential_manager.open = false
            }
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.switch_section(self.previous_section),
            KeyCode::Tab => self.next_section(),
            KeyCode::BackTab => self.previous_section(),
            KeyCode::Char('`') => self.switch_section(self.previous_section),
            KeyCode::Char('r') => {
                self.refresh_discovered_keys();
                self.credential_manager.message =
                    Some("Credentials refreshed from metadata, Projects, and ~/.ssh.".into());
            }
            KeyCode::Down => {
                self.credential_manager.selected =
                    (self.credential_manager.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Up => {
                self.credential_manager.selected =
                    self.credential_manager.selected.saturating_sub(1)
            }
            KeyCode::Char('a') => self.credential_form(FormKind::Password, None),
            KeyCode::Char('g') => self.credential_form(FormKind::Generate, None),
            KeyCode::Char('i') => self.credential_form(FormKind::Import, None),
            KeyCode::Char('e') => {
                if let Some(item) = self.selected_credential() {
                    if matches!(item.source, CredentialSource::ProjectLink { .. }) {
                        self.credential_manager.message =
                            Some("Edit this linked password from its Project Web Link.".into());
                        return;
                    }
                    let discovered = item.source == CredentialSource::DiscoveredKey;
                    let item = item.credential;
                    if item.kind == CredentialKind::Password {
                        self.credential_form(FormKind::Password, Some(item));
                    } else {
                        self.credential_form(FormKind::Import, Some(item));
                        if discovered && let Some(form) = &mut self.credential_manager.form {
                            form.id = None;
                        }
                    }
                }
            }
            KeyCode::Char('d') => {
                self.credential_manager.deleting = self.selected_credential().is_some()
            }
            KeyCode::Char('x') => {
                if let Some(alias) = self.credential_manager.target.clone() {
                    let previous = self.metadata.host_passwords.clone();
                    self.metadata.host_passwords.remove(&alias);
                    if self.save_metadata() {
                        self.credential_manager.message =
                            Some(format!("Unlinked saved password from {alias}."));
                    } else {
                        self.metadata.host_passwords = previous;
                        self.credential_manager.message = self.notice.clone();
                    }
                }
            }
            KeyCode::Enter
                if self.credential_manager.target.is_some()
                    && !self.credential_manager.upload_only =>
            {
                self.assign_credential(self.credential_manager.target.clone().unwrap())
            }
            KeyCode::Enter
                if !self.credential_manager.upload_only && self.selected_credential().is_some() =>
            {
                self.credential_manager.picker = true;
                self.credential_manager.picker_upload = false;
                self.credential_manager.picker_group = None;
                self.credential_manager.picker_selected = 0;
            }
            KeyCode::Enter | KeyCode::Char('u')
                if self
                    .selected_credential()
                    .is_some_and(|item| item.credential.kind == CredentialKind::SshKey) =>
            {
                if let Some(alias) = self.credential_manager.target.clone() {
                    self.credential_manager.confirm_upload = Some(alias);
                } else {
                    self.credential_manager.picker = true;
                    self.credential_manager.picker_upload = true;
                    self.credential_manager.picker_group = None;
                    self.credential_manager.picker_selected = 0;
                }
            }
            KeyCode::Char('p') => {
                if let Some(item) = self.selected_credential() {
                    let item = item.credential;
                    self.credential_manager.cleanup.push(
                        if let Some(reference) = item.password_ref {
                            crate::project_credentials::copy_password(reference)
                        } else {
                            crate::project_credentials::copy_public_key(item.public_key)
                        },
                    );
                    self.credential_manager.message = Some("Copying to clipboard...".into());
                }
            }
            _ => {}
        }
    }
}
