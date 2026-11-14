use crossterm::event::KeyCode;
use std::sync::mpsc::{Receiver, TryRecvError};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    app::{App, NavSection},
    project_monitor::{ExternalRun, ProjectMonitor, stop_external_async},
    projects::{
        CommandKind, PasswordStorage, ProjectCommand, ProjectDefinition, ProjectLifecycle,
        ProjectLink, ProjectTarget, TaskKey, execution_target, working_directory,
    },
};

#[derive(Default)]
pub struct WorkspaceState {
    pub selected_project: usize,
    pub selected_command: usize,
    pub selected_item: usize,
    collapsed_groups: std::collections::HashSet<(uuid::Uuid, String)>,
    collapsed_links: std::collections::HashSet<uuid::Uuid>,
    pub flat: bool,
    pub project_open: bool,
    pub output_open: bool,
    pub output_scroll: Option<usize>,
    pub form: Option<WorkspaceForm>,
    pub delete: Option<WorkspaceDelete>,
    pub monitor: ProjectMonitor,
    pub confirmation: Option<ExternalConfirmation>,
    pending: Option<PendingAction>,
    stopping: Option<ExternalStop>,
    opening_link: Option<Receiver<Result<(), String>>>,
    saving_password: Option<Receiver<Result<crate::project_credentials::PreparedPassword, String>>>,
    credential_jobs: Vec<Receiver<Result<String, String>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectAction {
    Start,
    Stop,
    Restart,
}

pub struct ExternalConfirmation {
    pub action: ProjectAction,
    pub keys: Vec<TaskKey>,
    pub runs: Vec<ExternalRun>,
}

struct PendingAction {
    action: ProjectAction,
    keys: Vec<TaskKey>,
    minimum_generation: u64,
    definitions: Vec<ProjectDefinition>,
}

struct ExternalStop {
    action: ProjectAction,
    keys: Vec<TaskKey>,
    receiver: Receiver<Result<(), String>>,
}

pub struct WorkspaceForm {
    pub project_id: Option<uuid::Uuid>,
    pub editing_id: Option<uuid::Uuid>,
    pub fields: Vec<String>,
    pub selected: usize,
    pub error: Option<String>,
    pub group_form: bool,
    pub link_form: bool,
    pub original_group: Option<String>,
}

impl WorkspaceForm {
    pub fn labels(&self) -> &[&str] {
        if self.group_form {
            &["Group name"]
        } else if self.link_form {
            &[
                "Link name",
                "URL",
                "Username (optional)",
                "Password (blank keeps saved)",
                "Password storage",
                "Remove saved password",
            ]
        } else if self.project_id.is_some() {
            &[
                "Name",
                "Command",
                "Working directory (optional)",
                "Group (optional)",
                "Type",
                "Target",
                "On exit",
            ]
        } else {
            &["Project name", "Base directory", "Default target"]
        }
    }
}

impl Drop for WorkspaceForm {
    fn drop(&mut self) {
        if self.link_form
            && let Some(password) = self.fields.get_mut(3)
        {
            password.zeroize();
        }
    }
}

pub enum WorkspaceDelete {
    Project {
        id: uuid::Uuid,
        name: String,
    },
    Command {
        key: TaskKey,
        name: String,
    },
    Group {
        project_id: uuid::Uuid,
        name: String,
    },
    Link {
        project_id: uuid::Uuid,
        id: uuid::Uuid,
        name: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceItem {
    Links,
    Group(String),
    Command(usize),
    Link(usize),
}

impl App {
    pub fn project_groups(&self) -> Vec<String> {
        let Some(project) = self.selected_project() else {
            return Vec::new();
        };
        let mut groups = project.groups.clone();
        for group in project
            .commands
            .iter()
            .filter_map(|command| command.group.as_ref())
        {
            if !groups.contains(group) {
                groups.push(group.clone());
            }
        }
        groups
    }

    pub fn project_items(&self) -> Vec<WorkspaceItem> {
        let Some(project) = self.selected_project() else {
            return Vec::new();
        };
        let mut items = Vec::new();
        if !project.links.is_empty() {
            if !self.workspace.flat {
                items.push(WorkspaceItem::Links);
            }
            if self.workspace.flat || !self.project_links_collapsed() {
                items.extend((0..project.links.len()).map(WorkspaceItem::Link));
            }
        }
        if self.workspace.flat {
            items.extend((0..project.commands.len()).map(WorkspaceItem::Command));
            return items;
        }
        for group in self.project_groups() {
            items.push(WorkspaceItem::Group(group.clone()));
            if !self.project_group_collapsed(&group) {
                items.extend(
                    project
                        .commands
                        .iter()
                        .enumerate()
                        .filter(|(_, command)| command.group.as_ref() == Some(&group))
                        .map(|(index, _)| WorkspaceItem::Command(index)),
                );
            }
        }
        items.extend(
            project
                .commands
                .iter()
                .enumerate()
                .filter(|(_, command)| command.group.is_none())
                .map(|(index, _)| WorkspaceItem::Command(index)),
        );
        items
    }

    pub fn project_group_collapsed(&self, group: &str) -> bool {
        self.selected_project().is_some_and(|project| {
            self.workspace
                .collapsed_groups
                .contains(&(project.id, group.to_string()))
        })
    }

    pub fn project_links_collapsed(&self) -> bool {
        self.selected_project()
            .is_some_and(|project| self.workspace.collapsed_links.contains(&project.id))
    }

    pub fn project_links_header_selected(&self) -> bool {
        self.workspace.project_open
            && !self.workspace.output_open
            && self.project_items().get(self.workspace.selected_item) == Some(&WorkspaceItem::Links)
    }

    fn toggle_project_links(&mut self, key: KeyCode) {
        let Some(project) = self.selected_project() else {
            return;
        };
        let id = project.id;
        if key == KeyCode::Right {
            self.workspace.collapsed_links.remove(&id);
        } else if key == KeyCode::Left || !self.workspace.collapsed_links.remove(&id) {
            self.workspace.collapsed_links.insert(id);
        }
        self.workspace.selected_item = 0;
    }

    fn set_project_group_expanded(&mut self, group: &str, expanded: bool) {
        let Some(project) = self.selected_project() else {
            return;
        };
        let key = (project.id, group.to_string());
        if expanded {
            self.workspace.collapsed_groups.remove(&key);
        } else {
            self.workspace.collapsed_groups.insert(key);
        }
    }

    fn selected_project_group(&self) -> Option<String> {
        if !self.workspace.output_open
            && let Some(WorkspaceItem::Group(group)) =
                self.project_items().get(self.workspace.selected_item)
        {
            return Some(group.clone());
        }
        self.selected_project_command()
            .and_then(|command| command.group.clone())
    }

    fn handle_project_group_key(&mut self, key: KeyCode) {
        if self.project_links_header_selected() {
            self.toggle_project_links(key);
            return;
        }
        if self.selected_project_link().is_some() {
            if key == KeyCode::Left {
                self.workspace.selected_item = 0;
            }
            return;
        }
        let Some(group) = self.selected_project_group() else {
            return;
        };
        let header = matches!(
            self.project_items().get(self.workspace.selected_item),
            Some(WorkspaceItem::Group(_))
        );
        if header {
            let expanded = match key {
                KeyCode::Right => true,
                KeyCode::Left => false,
                _ => self.project_group_collapsed(&group),
            };
            self.set_project_group_expanded(&group, expanded);
        } else if key != KeyCode::Left {
            return;
        }
        self.workspace.selected_item = self
            .project_items()
            .iter()
            .position(|item| *item == WorkspaceItem::Group(group.clone()))
            .unwrap_or(0);
    }

    fn sync_project_selection(&mut self) {
        let items = self.project_items();
        self.workspace.selected_item = self
            .workspace
            .selected_item
            .min(items.len().saturating_sub(1));
        if let Some(WorkspaceItem::Command(index)) = items.get(self.workspace.selected_item) {
            self.workspace.selected_command = *index;
        }
    }

    fn project_service_keys(&self) -> Vec<TaskKey> {
        if self.selected_project_link().is_some() || self.project_links_header_selected() {
            return Vec::new();
        }
        let Some(project) = self.selected_project() else {
            return Vec::new();
        };
        let group = if self.workspace.project_open && !self.workspace.flat {
            self.selected_project_group()
        } else {
            None
        };
        project
            .commands
            .iter()
            .filter(|command| command.kind == CommandKind::Service)
            .filter(|command| {
                group
                    .as_ref()
                    .is_none_or(|group| command.group.as_ref() == Some(group))
            })
            .map(|command| (project.id, command.id))
            .collect()
    }

    pub fn external_project_runs(&self, key: TaskKey) -> &[ExternalRun] {
        if !self.workspace.monitor.matches(&self.metadata.projects) {
            return &[];
        }
        self.workspace
            .monitor
            .runs
            .get(&key)
            .map_or(&[], Vec::as_slice)
    }

    pub fn project_is_running(&self, key: TaskKey) -> bool {
        self.project_service.active(key) || !self.external_project_runs(key).is_empty()
    }

    pub fn refresh_projects(&mut self) {
        self.workspace
            .monitor
            .refresh(&self.metadata.projects, self.project_service.owners());
    }

    fn poll_project_credentials(&mut self) {
        if let Some(receiver) = &self.workspace.saving_password {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Keychain worker stopped.".into())),
            };
            if let Some(result) = result {
                self.workspace.saving_password = None;
                if let Some(mut form) = self.workspace.form.take() {
                    let result = result.and_then(|mut prepared| {
                        self.save_workspace_form(&form, Some(&prepared.reference), None)?;
                        prepared.commit();
                        Ok(())
                    });
                    if let Err(error) = result {
                        form.error = Some(format!("{error} Re-enter the new password to retry."));
                        self.workspace.form = Some(form);
                    }
                }
            }
        }
        self.workspace
            .credential_jobs
            .retain(|receiver| match receiver.try_recv() {
                Ok(Ok(message)) => {
                    if !message.is_empty() {
                        self.notice = Some(message);
                    }
                    false
                }
                Ok(Err(error)) => {
                    self.notice = Some(error);
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => {
                    self.notice = Some("Account worker stopped.".into());
                    false
                }
            });
    }

    fn cleanup_link_passwords(&mut self, previous: &[ProjectDefinition]) {
        let used: std::collections::HashSet<_> = self
            .metadata
            .projects
            .iter()
            .flat_map(|project| &project.links)
            .filter_map(|link| link.password_ref.as_ref())
            .chain(
                self.metadata
                    .credentials
                    .iter()
                    .filter_map(|item| item.password_ref.as_ref()),
            )
            .collect();
        let unused: std::collections::HashSet<_> = previous
            .iter()
            .flat_map(|project| &project.links)
            .filter_map(|link| link.password_ref.as_ref())
            .filter(|reference| !used.contains(reference))
            .cloned()
            .collect();
        if !unused.is_empty() {
            self.workspace
                .credential_jobs
                .push(crate::project_credentials::remove_unused(
                    unused.into_iter().collect(),
                ));
        }
    }

    pub fn poll_projects(&mut self) {
        self.poll_project_credentials();
        self.project_service.restore(&self.metadata.projects);
        if let Some(receiver) = &self.workspace.opening_link {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Browser worker disconnected.".into())),
            };
            if let Some(result) = result {
                self.workspace.opening_link = None;
                self.notice = Some(match result {
                    Ok(()) => "Opened project link in browser.".into(),
                    Err(error) => format!("Could not open project link: {error}"),
                });
            }
        }
        self.project_service.poll();
        if self
            .workspace
            .monitor
            .poll(&self.metadata.projects, &self.project_service.owners())
        {
            self.update_external_runtime_links();
        }
        let stopped =
            self.workspace
                .stopping
                .as_ref()
                .and_then(|stop| match stop.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        Some(Err("External stop worker disconnected.".into()))
                    }
                });
        if let Some(result) = stopped {
            let stop = self.workspace.stopping.take().expect("stopping");
            match result {
                Ok(()) => {
                    self.notice = Some("External command stopped.".into());
                    // A new inspection must precede restart: another process may have appeared.
                    self.queue_project_action(stop.action, stop.keys);
                }
                Err(error) => {
                    self.notice = Some(error);
                    self.refresh_projects();
                }
            }
        }
        if self.workspace.pending.as_ref().is_some_and(|pending| {
            self.workspace.monitor.completed >= pending.minimum_generation
                && !self.workspace.monitor.checking
        }) {
            let pending = self.workspace.pending.take().expect("pending");
            if pending.definitions != self.metadata.projects {
                self.notice = Some("Project definition changed; repeat the action.".into());
            } else if let Some(error) = &self.workspace.monitor.error {
                self.notice = Some(format!(
                    "Project inspection failed; action cancelled: {error}"
                ));
            } else {
                self.apply_project_action(pending.action, pending.keys);
            }
        }
        if !self.workspace.monitor.checking
            && (self.workspace.pending.is_some()
                || ((!self.metadata.projects.is_empty()
                    || !self.workspace.monitor.runs.is_empty())
                    && self.workspace.monitor.due(&self.metadata.projects)))
        {
            self.refresh_projects();
        }
    }

    pub(crate) fn update_external_runtime_links(&mut self) {
        let runs = if self.workspace.monitor.matches(&self.metadata.projects) {
            self.workspace
                .monitor
                .runs
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for entry in &mut self.runtime_entries {
            let previous = entry.external_project.take();
            if self.runtime_target == crate::runtime::RuntimeTarget::Local
                && entry.project.is_none()
            {
                entry.external_project = runs
                    .iter()
                    .find(|run| {
                        entry.pid.is_some_and(|pid| {
                            run.processes.iter().any(|process| {
                                process.pid == pid && process.command == entry.command
                            })
                        })
                    })
                    .cloned();
            }
            if let Some(run) = &entry.external_project {
                entry.origin = crate::runtime::RuntimeOrigin::ExternalProject;
                entry.origin_reason = format!(
                    "External command: {} / {}; matching command ancestry and directory.",
                    run.project_name, run.command_name
                );
                entry.kind = crate::runtime::RuntimeProcessKind::Application;
                entry.owned_by_current_user = true;
            } else if previous.is_some() {
                entry.origin = crate::runtime::RuntimeOrigin::Unknown;
                entry.origin_reason =
                    "External project observation changed; refresh Runtime.".into();
                entry.pid = None;
            }
        }
    }

    fn queue_project_action(&mut self, action: ProjectAction, keys: Vec<TaskKey>) {
        if keys.is_empty() {
            return;
        }
        for key in &keys {
            let Some(project) = self
                .metadata
                .projects
                .iter()
                .find(|project| project.id == key.0)
            else {
                continue;
            };
            let Some(command) = project.commands.iter().find(|command| command.id == key.1) else {
                continue;
            };
            if let ProjectTarget::Host(alias) = execution_target(project, command)
                && !self.hosts.hosts.iter().any(|host| host.alias == *alias)
            {
                self.notice = Some(format!(
                    "{} targets missing Host {}; choose another target first.",
                    command.name, alias
                ));
                return;
            }
        }
        if self.workspace.pending.is_some()
            || self.workspace.stopping.is_some()
            || self.workspace.confirmation.is_some()
        {
            self.notice = Some("A project action is already in progress.".into());
            return;
        }
        self.workspace.pending = Some(PendingAction {
            action,
            keys,
            minimum_generation: self.workspace.monitor.generation + 1,
            definitions: self.metadata.projects.clone(),
        });
        self.notice = Some("Checking existing project processes...".into());
        self.refresh_projects();
    }

    fn apply_project_action(&mut self, action: ProjectAction, keys: Vec<TaskKey>) {
        let external = keys
            .iter()
            .flat_map(|key| self.external_project_runs(*key).iter().cloned())
            .collect::<Vec<_>>();
        if action != ProjectAction::Start && !external.is_empty() {
            self.workspace.confirmation = Some(ExternalConfirmation {
                action,
                keys,
                runs: external,
            });
            return;
        }
        let mut messages = Vec::new();
        for key in keys {
            let Some(project) = self
                .metadata
                .projects
                .iter()
                .find(|project| project.id == key.0)
                .cloned()
            else {
                continue;
            };
            let Some(command) = project.commands.iter().find(|command| command.id == key.1) else {
                continue;
            };
            if action == ProjectAction::Start && !self.external_project_runs(key).is_empty() {
                messages.push(format!(
                    "{} already running externally; no duplicate started.",
                    command.name
                ));
                continue;
            }
            let result = match action {
                ProjectAction::Start if self.project_service.active(key) => {
                    messages.push(format!("{} already running.", command.name));
                    continue;
                }
                ProjectAction::Start => match execution_target(&project, command) {
                    crate::projects::ProjectTarget::Host(alias) => self
                        .project_service
                        .start_with_auth(&project, command, &self.host_password_auth(alias)),
                    crate::projects::ProjectTarget::Local => {
                        self.project_service.start(&project, command)
                    }
                },
                ProjectAction::Stop => self.project_service.stop(key),
                ProjectAction::Restart => match execution_target(&project, command) {
                    crate::projects::ProjectTarget::Host(alias) => self
                        .project_service
                        .restart_with_auth(&project, command, &self.host_password_auth(alias)),
                    crate::projects::ProjectTarget::Local => {
                        self.project_service.restart(&project, command)
                    }
                },
            };
            messages.push(match result {
                Ok(()) => format!(
                    "{}: {}.",
                    command.name,
                    match action {
                        ProjectAction::Start => "started",
                        ProjectAction::Stop => "stopping",
                        ProjectAction::Restart => "restarting",
                    }
                ),
                Err(error) => format!("{}: {error}", command.name),
            });
        }
        self.notice = Some(messages.join(" "));
        self.workspace.output_scroll = None;
    }

    pub(crate) fn start_confirmed_external_action(&mut self, confirmation: ExternalConfirmation) {
        if self.workspace.stopping.is_some() {
            self.notice = Some("An external stop is already in progress.".into());
            return;
        }
        self.workspace.stopping = Some(ExternalStop {
            action: confirmation.action,
            keys: confirmation.keys,
            receiver: stop_external_async(confirmation.runs),
        });
        self.notice = Some("Stopping confirmed external command processes...".into());
    }

    pub fn selected_project(&self) -> Option<&ProjectDefinition> {
        self.metadata.projects.get(self.workspace.selected_project)
    }

    pub fn selected_project_link(&self) -> Option<&ProjectLink> {
        if !self.workspace.project_open || self.workspace.output_open {
            return None;
        }
        let WorkspaceItem::Link(index) = *self.project_items().get(self.workspace.selected_item)?
        else {
            return None;
        };
        self.selected_project()?.links.get(index)
    }

    pub fn selected_project_command(&self) -> Option<&ProjectCommand> {
        if self.workspace.project_open && !self.workspace.output_open {
            let WorkspaceItem::Command(index) =
                *self.project_items().get(self.workspace.selected_item)?
            else {
                return None;
            };
            return self.selected_project()?.commands.get(index);
        }
        self.selected_project()?
            .commands
            .get(self.workspace.selected_command)
    }

    pub fn selected_project_key(&self) -> Option<TaskKey> {
        Some((
            self.selected_project()?.id,
            self.selected_project_command()?.id,
        ))
    }

    pub fn handle_projects_key(&mut self, key: KeyCode) {
        if self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
        {
            self.handle_workspace_dialog(key);
            return;
        }
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.next_section(),
            KeyCode::BackTab => self.previous_section(),
            KeyCode::Char('`') => self.switch_section(self.previous_section),
            KeyCode::Esc | KeyCode::Backspace => {
                if self.workspace.output_open {
                    self.workspace.output_open = false;
                } else {
                    self.workspace.project_open = false;
                }
            }
            KeyCode::Up | KeyCode::Down if self.workspace.output_open => {
                let end = crate::ui::project_output_end(self);
                let current = self.workspace.output_scroll.unwrap_or(end).min(end);
                self.workspace.output_scroll = Some(if key == KeyCode::Up {
                    current.saturating_sub(1)
                } else {
                    (current + 1).min(end)
                });
            }
            KeyCode::Home if self.workspace.output_open => self.workspace.output_scroll = Some(0),
            KeyCode::End if self.workspace.output_open => self.workspace.output_scroll = None,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.workspace.project_open
                    && !self.workspace.output_open
                    && !self.workspace.flat =>
            {
                self.handle_project_group_key(key);
            }
            KeyCode::Char('[') | KeyCode::Char(']') if self.workspace.output_open => {
                let commands = self
                    .project_items()
                    .into_iter()
                    .filter_map(|item| match item {
                        WorkspaceItem::Command(index) => Some(index),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let current = commands
                    .iter()
                    .position(|index| *index == self.workspace.selected_command)
                    .unwrap_or(0);
                let next = if key == KeyCode::Char('[') {
                    current.saturating_sub(1)
                } else {
                    (current + 1).min(commands.len().saturating_sub(1))
                };
                if let Some(index) = commands.get(next) {
                    self.workspace.selected_command = *index;
                    self.workspace.selected_item = self
                        .project_items()
                        .iter()
                        .position(|item| *item == WorkspaceItem::Command(*index))
                        .unwrap_or(0);
                }
                self.workspace.output_scroll = None;
            }
            KeyCode::Up | KeyCode::Down => {
                let (selected, count) = if self.workspace.project_open {
                    let count = self.project_items().len();
                    (&mut self.workspace.selected_item, count)
                } else {
                    (
                        &mut self.workspace.selected_project,
                        self.metadata.projects.len(),
                    )
                };
                *selected = if key == KeyCode::Up {
                    selected.saturating_sub(1)
                } else {
                    (*selected + 1).min(count.saturating_sub(1))
                };
            }
            KeyCode::Enter => {
                if self.workspace.project_open {
                    match self.project_items().get(self.workspace.selected_item) {
                        Some(WorkspaceItem::Links) => self.toggle_project_links(KeyCode::Enter),
                        Some(WorkspaceItem::Group(group)) => {
                            let group = group.clone();
                            self.set_project_group_expanded(
                                &group,
                                self.project_group_collapsed(&group),
                            );
                        }
                        Some(WorkspaceItem::Command(index)) => {
                            self.workspace.selected_command = *index;
                            self.workspace.output_open = true;
                            self.workspace.output_scroll = None;
                        }
                        Some(WorkspaceItem::Link(_)) => {
                            if self.workspace.opening_link.is_some() {
                                self.notice =
                                    Some("A browser request is already in progress.".into());
                            } else if let Some(link) = self.selected_project_link() {
                                self.workspace.opening_link =
                                    Some(crate::projects::open_link_async(link.url.clone()));
                                self.notice = Some("Opening project link...".into());
                            }
                        }
                        None => {}
                    }
                } else if self.selected_project().is_some() {
                    self.workspace.project_open = true;
                    self.workspace.selected_command = 0;
                    self.workspace.selected_item = 0;
                    self.workspace.flat = false;
                }
            }
            KeyCode::Char('a') => self.open_workspace_form(false),
            KeyCode::Char('L') if self.workspace.project_open && !self.workspace.output_open => {
                self.open_link_form(false)
            }
            KeyCode::Char('u') if self.selected_project_link().is_some() => {
                let username = self.selected_project_link().unwrap().username.clone();
                if username.is_empty() {
                    self.notice = Some("No username saved for this link.".into());
                } else {
                    self.workspace
                        .credential_jobs
                        .push(crate::project_credentials::copy_username(username));
                }
            }
            KeyCode::Char('p') if self.selected_project_link().is_some() => {
                if let Some(reference) = self.selected_project_link().unwrap().password_ref.clone()
                {
                    self.workspace
                        .credential_jobs
                        .push(crate::project_credentials::copy_password(reference));
                    self.notice = Some("Reading password from system keychain...".into());
                } else if let Some(password) =
                    self.selected_project_link().unwrap().password.clone()
                {
                    self.workspace
                        .credential_jobs
                        .push(crate::project_credentials::copy_password_value(password));
                    self.notice = Some("Reading password from configuration...".into());
                } else {
                    self.notice = Some("No password saved for this link.".into());
                }
            }
            KeyCode::Char('G') if self.workspace.project_open && !self.workspace.output_open => {
                self.open_group_form(None)
            }
            KeyCode::Char('v') if self.workspace.project_open && !self.workspace.output_open => {
                self.workspace.flat = !self.workspace.flat;
                self.workspace.selected_item = 0;
            }
            KeyCode::Char('e') => self.open_workspace_form(true),
            KeyCode::Char('d') => self.request_workspace_delete(),
            KeyCode::Char('s') if self.workspace.project_open => self.run_project_command(false),
            KeyCode::Char('r') if self.workspace.project_open => self.run_project_command(true),
            KeyCode::Char('x') if self.workspace.project_open => {
                if let Some(key) = self.selected_project_key() {
                    if self.project_service.active(key) {
                        if let Err(error) = self.project_service.stop(key) {
                            self.notice = Some(format!("Could not stop command: {error}"));
                        }
                    } else {
                        self.queue_project_action(ProjectAction::Stop, vec![key]);
                    }
                }
            }
            KeyCode::Char('A') => {
                let keys = self.project_service_keys();
                if keys.is_empty() {
                    self.notice = Some("No services in this scope.".into());
                } else {
                    self.queue_project_action(ProjectAction::Start, keys);
                }
            }
            KeyCode::Char('X') => {
                let keys = self.project_service_keys();
                if keys.is_empty() {
                    self.notice = Some("No services in this scope.".into());
                } else {
                    self.queue_project_action(ProjectAction::Stop, keys);
                }
            }
            _ => {}
        }
        if !self.workspace.output_open {
            self.sync_project_selection();
        }
    }

    fn run_project_command(&mut self, restart: bool) {
        let Some(key) = self.selected_project_key() else {
            return;
        };
        if !restart && self.project_service.active(key) {
            self.apply_project_action(
                if restart {
                    ProjectAction::Restart
                } else {
                    ProjectAction::Start
                },
                vec![key],
            );
        } else {
            self.queue_project_action(
                if restart {
                    ProjectAction::Restart
                } else {
                    ProjectAction::Start
                },
                vec![key],
            );
        }
    }

    fn open_workspace_form(&mut self, editing: bool) {
        if editing && self.selected_project_link().is_some() {
            self.open_link_form(true);
            return;
        }
        if editing
            && self.workspace.project_open
            && !self.workspace.output_open
            && let Some(WorkspaceItem::Group(group)) =
                self.project_items().get(self.workspace.selected_item)
        {
            self.open_group_form(Some(group.clone()));
            return;
        }
        let form = if self.workspace.project_open {
            let Some(project) = self.selected_project() else {
                return;
            };
            let new_group = self.selected_project_group().unwrap_or_default();
            let command = if editing {
                self.selected_project_command().cloned()
            } else {
                None
            };
            if editing && command.is_none() {
                return;
            }
            if let Some(command) = &command
                && self.project_service.active((project.id, command.id))
            {
                self.notice = Some("Stop the command before editing it.".into());
                return;
            }
            WorkspaceForm {
                project_id: Some(project.id),
                editing_id: command.as_ref().map(|command| command.id),
                fields: command.map_or_else(
                    || {
                        vec![
                            String::new(),
                            String::new(),
                            String::new(),
                            new_group,
                            "Task".into(),
                            "Inherit".into(),
                            ProjectLifecycle::App.label().into(),
                        ]
                    },
                    |command| {
                        vec![
                            command.name,
                            command.command,
                            command
                                .working_directory
                                .map_or_else(String::new, |p| p.display().to_string()),
                            command.group.unwrap_or_default(),
                            command.kind.label().into(),
                            command
                                .target
                                .map_or_else(|| "Inherit".into(), |target| target.label()),
                            command.lifecycle.label().into(),
                        ]
                    },
                ),
                selected: 0,
                error: None,
                group_form: false,
                link_form: false,
                original_group: None,
            }
        } else {
            let project = if editing {
                self.selected_project().cloned()
            } else {
                None
            };
            if editing && project.is_none() {
                return;
            }
            if let Some(project) = &project
                && self
                    .project_service
                    .tasks
                    .values()
                    .any(|task| task.active() && task.owner.key.0 == project.id)
            {
                self.notice = Some("Stop project commands before editing the project.".into());
                return;
            }
            WorkspaceForm {
                project_id: None,
                editing_id: project.as_ref().map(|project| project.id),
                fields: project.map_or_else(
                    || {
                        vec![
                            String::new(),
                            self.local_files.path.display().to_string(),
                            "Local".into(),
                        ]
                    },
                    |project| {
                        vec![
                            project.name,
                            project.base_path.display().to_string(),
                            project.target.label(),
                        ]
                    },
                ),
                selected: 0,
                error: None,
                group_form: false,
                link_form: false,
                original_group: None,
            }
        };
        self.workspace.form = Some(form);
    }

    fn open_group_form(&mut self, original_group: Option<String>) {
        let Some(project) = self.selected_project() else {
            return;
        };
        self.workspace.form = Some(WorkspaceForm {
            project_id: Some(project.id),
            editing_id: None,
            fields: vec![original_group.clone().unwrap_or_default()],
            selected: 0,
            error: None,
            group_form: true,
            link_form: false,
            original_group,
        });
    }

    fn open_link_form(&mut self, editing: bool) {
        let Some(project) = self.selected_project() else {
            return;
        };
        let link = editing.then(|| self.selected_project_link()).flatten();
        if editing && link.is_none() {
            return;
        }
        self.workspace.form = Some(WorkspaceForm {
            project_id: Some(project.id),
            editing_id: link.map(|link| link.id),
            fields: link.map_or_else(
                || {
                    vec![
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        PasswordStorage::Keychain.label().into(),
                        "No".into(),
                    ]
                },
                |link| {
                    vec![
                        link.name.clone(),
                        link.url.clone(),
                        link.username.clone(),
                        String::new(),
                        link.password_storage.label().into(),
                        "No".into(),
                    ]
                },
            ),
            selected: 0,
            error: None,
            group_form: false,
            link_form: true,
            original_group: None,
        });
    }

    fn request_workspace_delete(&mut self) {
        if self.project_links_header_selected() {
            self.notice = Some("Select a web link to delete.".into());
            return;
        }
        if let Some(link) = self.selected_project_link() {
            self.workspace.delete = Some(WorkspaceDelete::Link {
                project_id: self.selected_project().expect("project").id,
                id: link.id,
                name: link.name.clone(),
            });
            return;
        }
        if self.workspace.project_open {
            if !self.workspace.output_open
                && let Some(WorkspaceItem::Group(group)) =
                    self.project_items().get(self.workspace.selected_item)
            {
                self.workspace.delete = Some(WorkspaceDelete::Group {
                    project_id: self.selected_project().expect("project").id,
                    name: group.clone(),
                });
                return;
            }
            if let (Some(key), Some(command)) =
                (self.selected_project_key(), self.selected_project_command())
            {
                if self.project_service.active(key) {
                    self.notice = Some("Stop the command before deleting it.".into());
                } else {
                    self.workspace.delete = Some(WorkspaceDelete::Command {
                        key,
                        name: command.name.clone(),
                    });
                }
            }
        } else if let Some(project) = self.selected_project() {
            if self
                .project_service
                .tasks
                .values()
                .any(|task| task.active() && task.owner.key.0 == project.id)
            {
                self.notice = Some("Stop project commands before deleting the project.".into());
            } else {
                self.workspace.delete = Some(WorkspaceDelete::Project {
                    id: project.id,
                    name: project.name.clone(),
                });
            }
        }
    }

    pub fn handle_workspace_dialog(&mut self, key: KeyCode) {
        if self.workspace.saving_password.is_some() {
            if key == KeyCode::Esc {
                self.workspace.saving_password = None;
                self.workspace.form = None;
            }
            return;
        }
        if self.workspace.confirmation.is_some() {
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let confirmation = self.workspace.confirmation.take().expect("confirmation");
                    self.start_confirmed_external_action(confirmation);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.workspace.confirmation = None,
                _ => {}
            }
            return;
        }
        if self.workspace.delete.is_some() {
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let previous = self.metadata.projects.clone();
                    let target = self.workspace.delete.take().expect("checked");
                    match &target {
                        WorkspaceDelete::Project { id, .. } => {
                            self.metadata.projects.retain(|project| project.id != *id);
                        }
                        WorkspaceDelete::Command { key, .. } => {
                            if let Some(project) = self
                                .metadata
                                .projects
                                .iter_mut()
                                .find(|project| project.id == key.0)
                            {
                                project.commands.retain(|command| command.id != key.1);
                            }
                        }
                        WorkspaceDelete::Link { project_id, id, .. } => {
                            if let Some(project) = self
                                .metadata
                                .projects
                                .iter_mut()
                                .find(|project| project.id == *project_id)
                            {
                                project.links.retain(|link| link.id != *id);
                            }
                        }
                        WorkspaceDelete::Group { project_id, name } => {
                            if let Some(project) = self
                                .metadata
                                .projects
                                .iter_mut()
                                .find(|project| project.id == *project_id)
                            {
                                project.groups.retain(|group| group != name);
                                for command in &mut project.commands {
                                    if command.group.as_ref() == Some(name) {
                                        command.group = None;
                                    }
                                }
                            }
                        }
                    }
                    if !self.save_metadata() {
                        self.metadata.projects = previous;
                        return;
                    }
                    self.cleanup_link_passwords(&previous);
                    match target {
                        WorkspaceDelete::Project { id, .. } => {
                            if let Some(project) = previous.iter().find(|project| project.id == id)
                            {
                                for command in &project.commands {
                                    if let Err(error) = self
                                        .project_service
                                        .forget_persistent_record((id, command.id))
                                    {
                                        self.notice = Some(format!(
                                            "Definition deleted; background record retained: {error}"
                                        ));
                                    }
                                }
                            }
                            self.workspace
                                .collapsed_groups
                                .retain(|(project_id, _)| *project_id != id);
                            self.workspace.collapsed_links.remove(&id);
                            self.project_service
                                .tasks
                                .retain(|(project_id, _), _| *project_id != id);
                            self.workspace.selected_project = self
                                .workspace
                                .selected_project
                                .min(self.metadata.projects.len().saturating_sub(1));
                        }
                        WorkspaceDelete::Command { key, .. } => {
                            if let Err(error) = self.project_service.forget_persistent_record(key) {
                                self.notice = Some(format!(
                                    "Definition deleted; background record retained: {error}"
                                ));
                            }
                            self.project_service.tasks.remove(&key);
                            let count = self
                                .selected_project()
                                .map_or(0, |project| project.commands.len());
                            self.workspace.selected_command =
                                self.workspace.selected_command.min(count.saturating_sub(1));
                            self.workspace.output_open = false;
                        }
                        WorkspaceDelete::Group { project_id, name } => {
                            self.workspace.collapsed_groups.remove(&(project_id, name));
                            self.workspace.selected_item = 0;
                        }
                        WorkspaceDelete::Link { .. } => {}
                    }
                    self.sync_project_selection();
                }
                KeyCode::Char('n') | KeyCode::Esc => self.workspace.delete = None,
                _ => {}
            }
            return;
        }
        let Some(form) = self.workspace.form.as_ref() else {
            return;
        };
        let type_field =
            !form.group_form && !form.link_form && form.project_id.is_some() && form.selected == 4;
        let target_field = !form.group_form
            && !form.link_form
            && form.selected == if form.project_id.is_some() { 5 } else { 2 };
        let lifecycle_field =
            !form.group_form && !form.link_form && form.project_id.is_some() && form.selected == 6;
        let choices = if form.link_form && form.selected == 4 {
            vec![
                PasswordStorage::Keychain.label().into(),
                PasswordStorage::Config.label().into(),
            ]
        } else if form.link_form && form.selected == 5 {
            vec!["No".into(), "Yes".into()]
        } else if type_field {
            vec!["Task".to_string(), "Service".into()]
        } else if lifecycle_field {
            vec![
                ProjectLifecycle::App.label().into(),
                ProjectLifecycle::Persistent.label().into(),
            ]
        } else if target_field {
            let mut choices = vec!["Local".to_string()];
            if form.project_id.is_some() {
                choices.insert(0, "Inherit".into());
            }
            choices.extend(
                self.hosts
                    .hosts
                    .iter()
                    .map(|host| format!("Host: {}", host.alias)),
            );
            choices
        } else {
            Vec::new()
        };
        let form = self.workspace.form.as_mut().expect("form");
        if !choices.is_empty() && matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char(' '))
        {
            let current = choices
                .iter()
                .position(|value| *value == form.fields[form.selected])
                .unwrap_or(0);
            let next = if key == KeyCode::Left {
                (current + choices.len() - 1) % choices.len()
            } else {
                (current + 1) % choices.len()
            };
            let next_value = choices[next].clone();
            let config_warning = form.link_form
                && form.selected == 4
                && next_value == PasswordStorage::Config.label();
            form.fields[form.selected] = next_value;
            if config_warning {
                self.notice = Some(
                    "Warning: this password will be stored as plaintext in hosts.json and its backups."
                        .into(),
                );
            }
            return;
        }
        match key {
            KeyCode::Esc => self.workspace.form = None,
            KeyCode::Tab | KeyCode::Down => form.selected = (form.selected + 1) % form.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                form.selected = (form.selected + form.fields.len() - 1) % form.fields.len()
            }
            KeyCode::Backspace if choices.is_empty() => {
                form.fields[form.selected].pop();
            }
            KeyCode::Char(value) if !value.is_control() && choices.is_empty() => {
                form.fields[form.selected].push(value)
            }
            KeyCode::Enter => {
                let mut form = self.workspace.form.take().expect("checked");
                if form.link_form && !form.fields.get(3).is_none_or(String::is_empty) {
                    let validation = crate::projects::validate_link_url(form.fields[1].trim())
                        .and_then(|()| {
                            if form.fields[0].trim().is_empty() {
                                Err("Name is required.".into())
                            } else if form.fields.get(5).is_some_and(|value| value == "Yes") {
                                Err("Choose either a new password or removal.".into())
                            } else {
                                Ok(())
                            }
                        });
                    if let Err(error) = validation {
                        form.error = Some(error);
                    } else {
                        let password = Zeroizing::new(std::mem::take(&mut form.fields[3]));
                        if form.fields.get(4).map(String::as_str)
                            == Some(PasswordStorage::Config.label())
                        {
                            if let Err(error) =
                                self.save_workspace_form(&form, None, Some(password))
                            {
                                form.error = Some(error);
                            } else {
                                form.error = None;
                            }
                        } else {
                            self.workspace.saving_password =
                                Some(crate::project_credentials::prepare(password));
                            form.error = None;
                            self.notice = Some("Saving password to system keychain...".into());
                        }
                    }
                    self.workspace.form = Some(form);
                    return;
                }
                if let Err(error) = self.save_workspace_form(&form, None, None) {
                    form.error = Some(error);
                    self.workspace.form = Some(form);
                }
            }
            _ => {}
        }
    }

    fn save_workspace_form(
        &mut self,
        form: &WorkspaceForm,
        password_reference: Option<&str>,
        password_value: Option<Zeroizing<String>>,
    ) -> Result<(), String> {
        let previous = self.metadata.projects.clone();
        let name = form.fields[0].trim().to_string();
        if name.is_empty() {
            return Err("Name is required.".into());
        }
        if form.link_form {
            let value = form.fields[1].trim().to_string();
            crate::projects::validate_link_url(&value)?;
            let project = self
                .metadata
                .projects
                .iter_mut()
                .find(|project| Some(project.id) == form.project_id)
                .ok_or("Project no longer exists.")?;
            let storage = PasswordStorage::from_label(
                form.fields
                    .get(4)
                    .map(String::as_str)
                    .unwrap_or(PasswordStorage::Keychain.label()),
            )
            .ok_or("Choose a password storage method.")?;
            let remove_password = form.fields.get(5).is_some_and(|value| value == "Yes");
            let existing = form
                .editing_id
                .and_then(|id| project.links.iter().find(|link| link.id == id));
            if password_value.is_none()
                && password_reference.is_none()
                && !remove_password
                && existing.is_some_and(|link| link.password_storage != storage)
            {
                return Err("Enter the password when changing its storage.".into());
            }
            let link = ProjectLink {
                id: form.editing_id.unwrap_or_else(uuid::Uuid::new_v4),
                name: name.clone(),
                url: value,
                username: form.fields.get(2).map_or("", String::as_str).trim().into(),
                password_ref: if remove_password || storage == PasswordStorage::Config {
                    None
                } else {
                    password_reference.map(str::to_string).or_else(|| {
                        project
                            .links
                            .iter()
                            .find(|link| Some(link.id) == form.editing_id)
                            .and_then(|link| link.password_ref.clone())
                    })
                },
                password_storage: storage,
                password: if remove_password || storage == PasswordStorage::Keychain {
                    None
                } else if let Some(password) = password_value {
                    Some(password.to_string())
                } else {
                    existing.and_then(|link| link.password.clone())
                },
            };
            if let Some(id) = form.editing_id {
                let existing = project
                    .links
                    .iter_mut()
                    .find(|link| link.id == id)
                    .ok_or("Link no longer exists.")?;
                *existing = link;
            } else {
                project.links.push(link);
            }
        } else if form.group_form {
            let project = self
                .metadata
                .projects
                .iter_mut()
                .find(|project| Some(project.id) == form.project_id)
                .ok_or("Project no longer exists.")?;
            if form.original_group.as_ref() != Some(&name)
                && (project.groups.contains(&name)
                    || project
                        .commands
                        .iter()
                        .any(|command| command.group.as_ref() == Some(&name)))
            {
                return Err("A group with this name already exists.".into());
            }
            if let Some(old) = &form.original_group {
                for group in &mut project.groups {
                    if group == old {
                        *group = name.clone();
                    }
                }
                for command in &mut project.commands {
                    if command.group.as_ref() == Some(old) {
                        command.group = Some(name.clone());
                    }
                }
            }
            if !project.groups.contains(&name) {
                project.groups.push(name.clone());
            }
        } else if let Some(project_id) = form.project_id {
            let target = self.parse_project_target(
                form.fields.get(5).map(String::as_str).unwrap_or("Inherit"),
            )?;
            let project = self
                .metadata
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .ok_or("Project no longer exists.")?;
            let command = ProjectCommand {
                id: form.editing_id.unwrap_or_else(uuid::Uuid::new_v4),
                name: name.clone(),
                command: form.fields[1].trim().to_string(),
                working_directory: (!form.fields[2].trim().is_empty())
                    .then(|| form.fields[2].trim().into()),
                group: form.fields.get(3).and_then(|value| {
                    let value = value.trim();
                    (!value.is_empty()).then(|| value.to_string())
                }),
                kind: match form.fields.get(4).map(String::as_str).unwrap_or("Task") {
                    "Service" => CommandKind::Service,
                    "Task" => CommandKind::Task,
                    _ => return Err("Select Service or Task.".into()),
                },
                target,
                lifecycle: match form
                    .fields
                    .get(6)
                    .map(String::as_str)
                    .unwrap_or("Stop on exit")
                {
                    "Stop on exit" => ProjectLifecycle::App,
                    "Keep running" => ProjectLifecycle::Persistent,
                    _ => return Err("Select Stop on exit or Keep running.".into()),
                },
            };
            if command.command.is_empty() {
                return Err("Command is required.".into());
            }
            if *execution_target(project, &command) == ProjectTarget::Local {
                working_directory(project, &command).map_err(|error| error.to_string())?;
            } else {
                validate_remote_directory(&project.base_path)?;
            }
            let command_id = command.id;
            if let Some(group) = &command.group
                && !project.groups.contains(group)
            {
                project.groups.push(group.clone());
            }
            if let Some(existing) = project.commands.iter_mut().find(|c| c.id == command.id) {
                *existing = command;
            } else {
                project.commands.push(command);
                self.workspace.selected_command = project.commands.len() - 1;
            }
            self.workspace.selected_command = project
                .commands
                .iter()
                .position(|command| command.id == command_id)
                .expect("saved command");
        } else {
            let target = self
                .parse_project_target(form.fields.get(2).map(String::as_str).unwrap_or("Local"))?
                .unwrap_or_default();
            let project = ProjectDefinition {
                id: form.editing_id.unwrap_or_else(uuid::Uuid::new_v4),
                name: name.clone(),
                base_path: form.fields[1].trim().into(),
                commands: form
                    .editing_id
                    .and_then(|id| self.metadata.projects.iter().find(|p| p.id == id))
                    .map_or_else(Vec::new, |p| p.commands.clone()),
                groups: form
                    .editing_id
                    .and_then(|id| self.metadata.projects.iter().find(|p| p.id == id))
                    .map_or_else(Vec::new, |project| project.groups.clone()),
                target,
                links: form
                    .editing_id
                    .and_then(|id| {
                        self.metadata
                            .projects
                            .iter()
                            .find(|project| project.id == id)
                    })
                    .map_or_else(Vec::new, |project| project.links.clone()),
            };
            if project.target == ProjectTarget::Local {
                working_directory(&project, &ProjectCommand::default())
                    .map_err(|error| error.to_string())?;
            } else {
                validate_remote_directory(&project.base_path)?;
            }
            if let Some(existing) = self
                .metadata
                .projects
                .iter_mut()
                .find(|p| p.id == project.id)
            {
                *existing = project;
            } else {
                self.metadata.projects.push(project);
                self.workspace.selected_project = self.metadata.projects.len() - 1;
            }
        }
        if !self.save_metadata() {
            self.metadata.projects = previous;
            self.workspace.selected_project = self
                .workspace
                .selected_project
                .min(self.metadata.projects.len().saturating_sub(1));
            self.workspace.selected_command = self.workspace.selected_command.min(
                self.selected_project()
                    .map_or(0, |project| project.commands.len())
                    .saturating_sub(1),
            );
            return Err(self
                .notice
                .clone()
                .unwrap_or_else(|| "Could not save project.".into()));
        }
        self.cleanup_link_passwords(&previous);
        self.notice = Some("Saved project definition.".into());
        if form.link_form {
            if let Some(project_id) = form.project_id {
                self.workspace.collapsed_links.remove(&project_id);
            }
            let index = self
                .selected_project()
                .and_then(|project| {
                    project.links.iter().rposition(|link| {
                        form.editing_id
                            .map_or(link.name == name, |id| link.id == id)
                    })
                })
                .unwrap_or(0);
            self.workspace.selected_item = self
                .project_items()
                .iter()
                .position(|item| *item == WorkspaceItem::Link(index))
                .unwrap_or(0);
        } else if form.group_form {
            self.workspace.flat = false;
            if let Some(project_id) = form.project_id
                && let Some(old) = &form.original_group
                && self
                    .workspace
                    .collapsed_groups
                    .remove(&(project_id, old.clone()))
            {
                self.workspace
                    .collapsed_groups
                    .insert((project_id, name.clone()));
            }
            self.workspace.selected_item = self
                .project_items()
                .iter()
                .position(|item| *item == WorkspaceItem::Group(name.clone()))
                .unwrap_or(0);
        } else if form.project_id.is_some() {
            let group = self
                .selected_project()
                .and_then(|project| project.commands.get(self.workspace.selected_command))
                .and_then(|command| command.group.clone());
            if let Some(group) = group {
                self.set_project_group_expanded(&group, true);
            }
            self.workspace.selected_item = self
                .project_items()
                .iter()
                .position(|item| *item == WorkspaceItem::Command(self.workspace.selected_command))
                .unwrap_or(0);
        }
        self.sync_project_selection();
        Ok(())
    }

    fn parse_project_target(&self, label: &str) -> Result<Option<ProjectTarget>, String> {
        match label {
            "Inherit" => Ok(None),
            "Local" => Ok(Some(ProjectTarget::Local)),
            _ => {
                let alias = label
                    .strip_prefix("Host: ")
                    .ok_or("Select Local or a Host.")?;
                if !self.hosts.hosts.iter().any(|host| host.alias == alias) {
                    return Err(format!("Host {alias} is no longer in the Hosts catalog."));
                }
                Ok(Some(ProjectTarget::Host(alias.into())))
            }
        }
    }

    pub fn open_project_from_runtime(&mut self) {
        let Some(key) = self.selected_runtime_entry().and_then(|entry| {
            entry
                .project
                .as_ref()
                .map(|owner| owner.key)
                .or_else(|| entry.external_project.as_ref().map(|run| run.key))
        }) else {
            return;
        };
        let Some(index) = self
            .metadata
            .projects
            .iter()
            .position(|project| project.id == key.0)
        else {
            return;
        };
        let Some(command_index) = self.metadata.projects[index]
            .commands
            .iter()
            .position(|command| command.id == key.1)
        else {
            return;
        };
        self.workspace.selected_project = index;
        self.workspace.selected_command = command_index;
        self.workspace.project_open = true;
        self.workspace.flat = false;
        if let Some(group) = self.metadata.projects[index].commands[command_index]
            .group
            .clone()
        {
            self.set_project_group_expanded(&group, true);
        }
        self.workspace.selected_item = self
            .project_items()
            .iter()
            .position(|item| *item == WorkspaceItem::Command(self.workspace.selected_command))
            .unwrap_or(0);
        self.workspace.output_open = true;
        self.workspace.output_scroll = None;
        self.switch_section(NavSection::Projects);
    }
}

fn validate_remote_directory(path: &std::path::Path) -> Result<(), String> {
    if path.is_absolute() || path.starts_with("~") {
        Ok(())
    } else {
        Err("Remote base directory must be absolute or start with ~/.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hosts::HostCatalog, metadata::Metadata, projects::tests::Fixture};

    fn grouped_app(fixture: &Fixture) -> App {
        let mut app = App::with_catalog(HostCatalog::default(), Ok(Metadata::default()));
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.project_service = fixture.service();
        let mut project = fixture.project.clone();
        project.groups = vec!["Services".into(), "Checks".into(), "Empty".into()];
        for (id, name, group, kind) in [
            (1, "server", Some("Services"), CommandKind::Service),
            (2, "check", Some("Checks"), CommandKind::Task),
            (3, "other", None, CommandKind::Task),
        ] {
            project.commands.push(ProjectCommand {
                id: uuid::Uuid::from_u128(id),
                name: name.into(),
                command: "printf fixture".into(),
                group: group.map(String::from),
                kind,
                ..Default::default()
            });
        }
        app.metadata.projects.push(project);
        app.section = NavSection::Projects;
        app
    }

    #[test]
    fn groups_flat_view_and_output_navigation_keep_command_identity() {
        let fixture = Fixture::new("group-navigation");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        assert_eq!(
            app.project_items(),
            vec![
                WorkspaceItem::Group("Services".into()),
                WorkspaceItem::Command(0),
                WorkspaceItem::Group("Checks".into()),
                WorkspaceItem::Command(1),
                WorkspaceItem::Group("Empty".into()),
                WorkspaceItem::Command(2),
            ]
        );
        assert!(app.selected_project_command().is_none());
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Enter);
        assert_eq!(
            app.selected_project_key(),
            Some((fixture.project.id, uuid::Uuid::from_u128(1)))
        );
        assert!(app.workspace.output_open);
        app.handle_projects_key(KeyCode::Char(']'));
        assert_eq!(
            app.selected_project_key(),
            Some((fixture.project.id, uuid::Uuid::from_u128(2)))
        );
        assert!(app.project_service.tasks.is_empty());
        app.handle_projects_key(KeyCode::Esc);
        assert!(app.workspace.project_open);
        app.handle_projects_key(KeyCode::Char('v'));
        assert_eq!(
            app.project_items(),
            vec![
                WorkspaceItem::Command(0),
                WorkspaceItem::Command(1),
                WorkspaceItem::Command(2),
            ]
        );
        app.handle_projects_key(KeyCode::Down);
        assert_eq!(
            app.selected_project_key(),
            Some((fixture.project.id, uuid::Uuid::from_u128(2)))
        );
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        assert_eq!(form.fields[3], "Checks");
        form.fields[3] = "Diagnostics".into();
        form.selected = 6;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.is_none());
        assert_eq!(
            app.selected_project_key(),
            Some((fixture.project.id, uuid::Uuid::from_u128(2)))
        );
        assert!(app.project_groups().contains(&"Diagnostics".into()));
    }

    #[test]
    fn group_crud_preserves_commands_and_rolls_back_failed_saves() {
        let fixture = Fixture::new("group-crud");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('G'));
        app.workspace.form.as_mut().unwrap().fields[0] = "New".into();
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.project_groups().contains(&"New".into()));
        app.handle_projects_key(KeyCode::Char('e'));
        app.workspace.form.as_mut().unwrap().fields[0] = "Services".into();
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
        app.handle_workspace_dialog(KeyCode::Esc);
        app.workspace.selected_item = 0;
        app.handle_projects_key(KeyCode::Char('e'));
        app.workspace.form.as_mut().unwrap().fields[0] = "Development".into();
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(
            app.selected_project().unwrap().commands[0].group.as_deref(),
            Some("Development")
        );
        let blocked = fixture.root.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(
            app.selected_project().unwrap().commands[0].group.as_deref(),
            Some("Development")
        );
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(app.selected_project().unwrap().commands.len(), 3);
        assert!(app.selected_project().unwrap().commands[0].group.is_none());
        assert!(!app.project_groups().contains(&"Development".into()));
        app.handle_projects_key(KeyCode::Char('G'));
        app.workspace.form.as_mut().unwrap().fields[0] = "Failed save".into();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(!app.project_groups().contains(&"Failed save".into()));
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn batch_actions_start_only_services_in_the_selected_group() {
        let fixture = Fixture::new("group-batch");
        let mut app = grouped_app(&fixture);
        let mut listener = fixture.listener_command(false);
        listener.id = uuid::Uuid::from_u128(1);
        listener.group = Some("Services".into());
        listener.kind = CommandKind::Service;
        app.metadata.projects[0].commands[0] = listener;
        app.metadata.projects[0].commands[1].kind = CommandKind::Service;
        app.handle_projects_key(KeyCode::Enter);
        assert_eq!(
            app.project_service_keys(),
            vec![(fixture.project.id, uuid::Uuid::from_u128(1))]
        );
        app.handle_projects_key(KeyCode::Char('A'));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while fixture.ready().is_none() {
            app.poll_projects();
            assert!(
                std::time::Instant::now() < deadline,
                "Service did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(app.project_service.tasks.len(), 1);
        assert!(
            app.project_service
                .active((fixture.project.id, uuid::Uuid::from_u128(1)))
        );
        app.handle_projects_key(KeyCode::Char('X'));
        while app
            .project_service
            .active((fixture.project.id, uuid::Uuid::from_u128(1)))
        {
            app.poll_projects();
            assert!(std::time::Instant::now() < deadline, "Service did not stop");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(app.project_service.tasks.len(), 1);
        app.workspace.selected_item = app
            .project_items()
            .iter()
            .position(|item| *item == WorkspaceItem::Group("Empty".into()))
            .unwrap();
        assert!(app.project_service_keys().is_empty());
    }

    #[test]
    fn target_inheritance_runs_remote_commands_through_ssh() {
        let fixture = Fixture::new("group-target");
        let mut app = grouped_app(&fixture);
        app.metadata.projects[0].target = ProjectTarget::Host("web".into());
        let project = app.metadata.projects[0].clone();
        let command = &project.commands[0];
        assert_eq!(
            execution_target(&project, command),
            &ProjectTarget::Host("web".into())
        );
        assert!(app.project_service.start(&project, command).is_ok());
        assert!(
            app.project_service
                .tasks
                .contains_key(&(project.id, command.id))
        );
        app.project_service.stop((project.id, command.id)).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.project_service.active((project.id, command.id)) {
            app.project_service.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('s'));
        assert!(app.workspace.pending.is_none());
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.contains("Host")),
            "{:?}",
            app.notice
        );
        app.metadata.projects[0].commands[0].target = Some(ProjectTarget::Local);
        assert!(
            working_directory(
                &app.metadata.projects[0],
                &app.metadata.projects[0].commands[0]
            )
            .is_ok()
        );
        assert!(app.parse_project_target("Host: deleted").is_err());
        app.metadata.projects[0].target = ProjectTarget::Local;
        app.handle_projects_key(KeyCode::Char('e'));
        app.workspace.form.as_mut().unwrap().selected = 4;
        app.handle_workspace_dialog(KeyCode::Left);
        assert_eq!(app.workspace.form.as_ref().unwrap().fields[4], "Task");
        app.handle_workspace_dialog(KeyCode::Right);
        assert_eq!(app.workspace.form.as_ref().unwrap().fields[4], "Service");
    }

    #[test]
    fn legacy_commands_default_to_ungrouped_tasks_and_local_execution() {
        let project: ProjectDefinition = serde_json::from_str(
            r#"{
            "id": "00000000-0000-0000-0000-000000000001", "name": "Old", "base_path": "/tmp",
            "commands": [{"id": "00000000-0000-0000-0000-000000000001", "name": "check", "command": "true", "working_directory": null}]
        }"#,
        )
        .unwrap();
        assert!(project.groups.is_empty());
        assert!(project.links.is_empty());
        assert_eq!(project.target, ProjectTarget::Local);
        assert_eq!(project.commands[0].kind, CommandKind::Task);
        assert!(project.commands[0].group.is_none());
        assert!(project.commands[0].target.is_none());
        assert_eq!(project.commands[0].lifecycle, ProjectLifecycle::App);
    }

    #[test]
    fn command_exit_policy_uses_choices_and_persists_without_starting() {
        let fixture = Fixture::new("lifecycle-form");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        assert_eq!(form.fields[6], "Stop on exit");
        form.selected = 6;
        app.handle_workspace_dialog(KeyCode::Right);
        assert_eq!(
            app.workspace.form.as_ref().unwrap().fields[6],
            "Keep running"
        );
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.is_none());
        assert_eq!(
            app.selected_project_command().unwrap().lifecycle,
            ProjectLifecycle::Persistent
        );
        assert!(app.project_service.tasks.is_empty());
        let restored: Metadata = serde_json::from_slice(
            &std::fs::read(app.metadata_save_path.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            restored.projects[0].commands[0].lifecycle,
            ProjectLifecycle::Persistent
        );
        app.handle_projects_key(KeyCode::Char('e'));
        app.workspace.form.as_mut().unwrap().selected = 6;
        app.handle_workspace_dialog(KeyCode::Left);
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(
            app.selected_project_command().unwrap().lifecycle,
            ProjectLifecycle::App
        );
    }

    #[test]
    fn deleting_a_command_cleans_its_background_index_only_after_metadata_saves() {
        use crate::projects::tests::{PersistentCleanup, until};
        let fixture = Fixture::new("persistent-delete");
        let mut app = grouped_app(&fixture);
        let mut command = fixture.command("printf 'saved output\\n'");
        command.id = uuid::Uuid::from_u128(1);
        command.group = Some("Services".into());
        command.lifecycle = ProjectLifecycle::Persistent;
        let key = (fixture.project.id, command.id);
        let _cleanup = PersistentCleanup::new(&fixture, key);
        app.metadata.projects[0].commands[0] = command.clone();
        app.project_service
            .start(&app.metadata.projects[0], &command)
            .unwrap();
        until(&mut app.project_service, |service| !service.active(key));
        let log = app.project_service.tasks[&key].stdout_log.clone();
        let record = fixture
            .root
            .join(format!("logs/persistent-{}-{}.json", key.0, key.1));
        let blocked = fixture.root.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(record.is_file());
        assert_eq!(app.metadata.projects[0].commands.len(), 3);
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(!record.exists());
        assert_eq!(app.metadata.projects[0].commands.len(), 2);
        assert!(log.is_file());
    }

    #[test]
    fn link_credentials_save_keep_replace_remove_and_rollback() {
        use crate::project_credentials::tests::password_exists;
        fn finish(app: &mut App) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while app.workspace.saving_password.is_some()
                || !app.workspace.credential_jobs.is_empty()
            {
                app.poll_project_credentials();
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let fixture = Fixture::new("link-credentials");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec![
            "Preview".into(),
            "http://localhost:3000".into(),
            "admin".into(),
            "first secret".into(),
            PasswordStorage::Keychain.label().into(),
            "No".into(),
        ];
        form.selected = 4;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.saving_password.is_some());
        assert!(app.workspace.form.as_ref().unwrap().fields[3].is_empty());
        finish(&mut app);
        assert!(app.workspace.form.is_none());
        let link = app.selected_project_link().unwrap().clone();
        let old = link.password_ref.clone().unwrap();
        assert!(password_exists(&old));
        assert_eq!(link.username, "admin");
        let json = std::fs::read_to_string(app.metadata_save_path.as_ref().unwrap()).unwrap();
        assert!(!json.contains("first secret"));
        assert!(json.contains(&old));
        let restored: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.projects[0].links.as_slice(),
            std::slice::from_ref(&link)
        );

        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        assert!(form.fields[3].is_empty());
        form.fields[0] = "Renamed".into();
        form.selected = 4;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(
            app.selected_project_link().unwrap().password_ref.as_ref(),
            Some(&old)
        );
        assert!(password_exists(&old));

        let blocked = fixture.root.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        let previous = app.metadata.projects.clone();
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields[3] = "replacement secret".into();
        form.selected = 4;
        app.handle_workspace_dialog(KeyCode::Enter);
        let prepared = app
            .workspace
            .saving_password
            .take()
            .unwrap()
            .recv()
            .unwrap()
            .unwrap();
        let rejected = prepared.reference.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(Ok(prepared)).unwrap();
        app.workspace.saving_password = Some(receiver);
        finish(&mut app);
        assert_eq!(app.metadata.projects, previous);
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while password_exists(&rejected) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!password_exists(&rejected));
        assert!(password_exists(&old));

        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.workspace.form.as_mut().unwrap().fields[3] = "replacement secret".into();
        app.handle_workspace_dialog(KeyCode::Enter);
        finish(&mut app);
        let replacement = app
            .selected_project_link()
            .unwrap()
            .password_ref
            .clone()
            .unwrap();
        assert_ne!(replacement, old);
        assert!(!password_exists(&old));
        assert!(password_exists(&replacement));
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        form.selected = 5;
        app.handle_workspace_dialog(KeyCode::Right);
        assert_eq!(app.workspace.form.as_ref().unwrap().fields[5], "Yes");
        app.handle_workspace_dialog(KeyCode::Enter);
        finish(&mut app);
        assert!(app.selected_project_link().unwrap().password_ref.is_none());
        assert!(!password_exists(&replacement));
        assert_eq!(app.selected_project_link().unwrap().username, "admin");
    }

    #[test]
    fn account_shortcuts_report_results_without_exposing_passwords() {
        let fixture = Fixture::new("account-shortcuts");
        let mut app = grouped_app(&fixture);
        app.metadata.projects[0].links.push(ProjectLink {
            id: uuid::Uuid::from_u128(1),
            name: "Preview".into(),
            url: "http://localhost:3000/".into(),
            ..Default::default()
        });
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('u'));
        assert!(app.notice.as_ref().unwrap().contains("No username"));
        app.handle_projects_key(KeyCode::Char('p'));
        assert!(app.notice.as_ref().unwrap().contains("No password"));
        let mut prepared =
            crate::project_credentials::prepare(Zeroizing::new("private-password".into()))
                .recv()
                .unwrap()
                .unwrap();
        let reference = prepared.reference.clone();
        prepared.commit();
        app.metadata.projects[0].links[0].username = "admin".into();
        app.metadata.projects[0].links[0].password_ref = Some(reference.clone());
        for (key, message) in [('u', "Username copied"), ('p', "Password copied")] {
            app.handle_projects_key(KeyCode::Char(key));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !app.workspace.credential_jobs.is_empty() {
                app.poll_project_credentials();
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(app.notice.as_ref().unwrap().contains(message));
            assert!(!app.notice.as_ref().unwrap().contains("private-password"));
            assert!(!app.workspace.output_open);
        }
        crate::project_credentials::remove_unused(vec![reference])
            .recv()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn link_password_rendering_shows_active_input_and_masks_inactive_input() {
        for (width, height) in [(120, 40), (80, 24)] {
            let fixture = Fixture::new("password-render");
            let mut app = grouped_app(&fixture);
            app.metadata.projects[0].links.push(ProjectLink {
                id: uuid::Uuid::from_u128(1),
                name: "Preview".into(),
                url: "http://localhost:3000".into(),
                username: "admin".into(),
                password_ref: Some(format!("web-{}", "a".repeat(64))),
                ..Default::default()
            });
            app.viewport_width = width;
            app.viewport_height = height;
            app.handle_projects_key(KeyCode::Enter);
            app.handle_projects_key(KeyCode::Down);
            app.handle_projects_key(KeyCode::Char('e'));
            {
                let form = app.workspace.form.as_mut().unwrap();
                form.fields[3] = "DO-NOT-RENDER-PASSWORD".into();
                form.selected = 3;
            }
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let text = format!("{}", terminal.backend());
            assert!(
                text.contains("DO-NOT-RENDER-PASSWORD"),
                "{width}x{height}: {text}"
            );
            assert!(!text.contains("********"), "{width}x{height}: {text}");
            app.workspace.form.as_mut().unwrap().selected = 0;
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let text = format!("{}", terminal.backend());
            assert!(
                !text.contains("DO-NOT-RENDER-PASSWORD"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("********"), "{width}x{height}: {text}");
            app.handle_workspace_dialog(KeyCode::Esc);
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            assert!(!format!("{}", terminal.backend()).contains("DO-NOT-RENDER"));
        }
    }

    #[test]
    fn link_password_errors_and_cancellation_leave_definition_unchanged() {
        let fixture = Fixture::new("password-errors");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec![
            "Preview".into(),
            "http://localhost:3000".into(),
            "admin".into(),
            "secret".into(),
            PasswordStorage::Keychain.label().into(),
            "Yes".into(),
        ];
        form.selected = 5;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
        assert!(app.workspace.saving_password.is_none());
        assert!(app.metadata.projects[0].links.is_empty());
        app.workspace.form.as_mut().unwrap().fields[5] = "No".into();
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(Err("Keychain locked.".into())).unwrap();
        app.workspace.saving_password = Some(receiver);
        app.poll_project_credentials();
        assert!(
            app.workspace
                .form
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("Keychain locked")
        );
        assert!(app.metadata.projects[0].links.is_empty());
        let (_sender, receiver) = std::sync::mpsc::channel();
        app.workspace.saving_password = Some(receiver);
        app.handle_workspace_dialog(KeyCode::Esc);
        assert!(app.workspace.form.is_none());
        assert!(app.workspace.saving_password.is_none());
    }

    #[test]
    fn deleting_a_project_cleans_passwords_only_after_successful_save() {
        use crate::project_credentials::{prepare, tests::password_exists};
        let fixture = Fixture::new("password-delete");
        let mut app = grouped_app(&fixture);
        let mut prepared = prepare(Zeroizing::new("secret".into()))
            .recv()
            .unwrap()
            .unwrap();
        let reference = prepared.reference.clone();
        prepared.commit();
        app.metadata.projects[0].links = vec![
            ProjectLink {
                id: uuid::Uuid::from_u128(1),
                name: "Home".into(),
                url: "http://localhost:3000".into(),
                password_ref: Some(reference.clone()),
                ..Default::default()
            },
            ProjectLink {
                id: uuid::Uuid::from_u128(2),
                name: "Buy".into(),
                url: "http://localhost:3000/buy".into(),
                password_ref: Some(reference.clone()),
                ..Default::default()
            },
        ];
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(password_exists(&reference));
        assert!(app.workspace.credential_jobs.is_empty());
        let blocked = fixture.root.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        app.handle_projects_key(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(password_exists(&reference));
        assert_eq!(app.metadata.projects.len(), 1);
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !app.workspace.credential_jobs.is_empty() {
            app.poll_project_credentials();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!password_exists(&reference));
        assert!(app.metadata.projects.is_empty());
    }

    #[test]
    fn project_links_crud_preserves_commands_and_survives_project_edits() {
        let fixture = Fixture::new("project-links");
        let mut app = grouped_app(&fixture);
        let commands = app.metadata.projects[0].commands.clone();
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        assert_eq!(form.labels().len(), 6);
        form.fields = vec!["Preview".into(), "http://localhost:3000/".into()];
        form.selected = 0;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.is_none());
        assert_eq!(app.selected_project_link().unwrap().name, "Preview");
        assert_eq!(app.selected_project_key(), None);
        assert_eq!(app.project_items()[0], WorkspaceItem::Links);
        assert_eq!(app.project_items()[1], WorkspaceItem::Link(0));
        for key in ['s', 'r', 'x', 'A', 'X'] {
            app.handle_projects_key(KeyCode::Char(key));
            assert!(app.workspace.pending.is_none());
            assert!(app.project_service.tasks.is_empty());
        }
        app.handle_projects_key(KeyCode::Char('v'));
        assert_eq!(app.project_items()[0], WorkspaceItem::Link(0));
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        assert!(form.link_form);
        form.fields = vec![
            "Admin".into(),
            "https://example.com/admin?tab=users#list".into(),
        ];
        form.selected = 1;
        app.handle_workspace_dialog(KeyCode::Enter);
        let link = app.selected_project_link().unwrap().clone();
        assert_eq!(link.name, "Admin");
        assert_ne!(link.id, uuid::Uuid::nil());
        app.handle_projects_key(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields[0] = "Renamed project".into();
        form.selected = 2;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.is_none());
        assert_eq!(
            app.metadata.projects[0].links.as_slice(),
            std::slice::from_ref(&link)
        );
        let restored: Metadata = serde_json::from_slice(
            &std::fs::read(app.metadata_save_path.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(restored.projects[0].links, [link]);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('d'));
        assert!(matches!(
            app.workspace.delete,
            Some(WorkspaceDelete::Link { .. })
        ));
        app.handle_workspace_dialog(KeyCode::Esc);
        assert_eq!(app.metadata.projects[0].links.len(), 1);
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.metadata.projects[0].links.is_empty());
        assert_eq!(app.metadata.projects[0].commands, commands);
    }

    #[test]
    fn web_link_enter_saves_from_any_field() {
        let fixture = Fixture::new("project-link-enter-save");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec![
            "Preview".into(),
            "http://localhost:3000/".into(),
            "admin".into(),
            String::new(),
            PasswordStorage::Keychain.label().into(),
            "No".into(),
        ];
        form.selected = 3;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.is_none());
        assert_eq!(app.selected_project_link().unwrap().name, "Preview");
        assert_eq!(
            app.selected_project_link().unwrap().url,
            "http://localhost:3000/"
        );
    }

    #[test]
    fn web_link_can_store_password_in_config_with_explicit_choice() {
        let fixture = Fixture::new("project-link-config-password");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec![
            "Preview".into(),
            "http://localhost:3000/".into(),
            "admin".into(),
            "config secret".into(),
            PasswordStorage::Config.label().into(),
            "No".into(),
        ];
        form.selected = 3;
        app.handle_workspace_dialog(KeyCode::Enter);
        let link = app.selected_project_link().unwrap();
        assert_eq!(link.password_storage, PasswordStorage::Config);
        assert_eq!(link.password.as_deref(), Some("config secret"));
        assert!(link.password_ref.is_none());
        let json = std::fs::read_to_string(app.metadata_save_path.as_ref().unwrap()).unwrap();
        assert!(json.contains("config secret"));
    }

    #[test]
    fn invalid_links_and_failed_saves_preserve_metadata_and_selection() {
        let fixture = Fixture::new("project-links-failure");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec!["Bad".into(), "file:///tmp/index.html".into()];
        form.selected = 1;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.metadata.projects[0].links.is_empty());
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
        app.workspace.form.as_mut().unwrap().fields[1] = "http://localhost:3000".into();
        let blocked = fixture.root.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        let original = app.metadata.projects.clone();
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(app.metadata.projects, original);
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(app.selected_project_link().unwrap().name, "Bad");
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        let original = app.metadata.projects.clone();
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields[0] = "Changed".into();
        form.selected = 1;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(app.metadata.projects, original);
        app.handle_workspace_dialog(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Char('d'));
        app.handle_workspace_dialog(KeyCode::Enter);
        assert_eq!(app.metadata.projects, original);
        assert_eq!(app.selected_project_link().unwrap().name, "Bad");
    }

    #[test]
    fn browser_errors_are_reported_without_opening_command_output() {
        let fixture = Fixture::new("project-browser-error");
        let mut app = grouped_app(&fixture);
        // Imported metadata is validated again before handing it to the browser.
        app.metadata.projects[0].links.push(ProjectLink {
            id: uuid::Uuid::from_u128(1),
            name: "Invalid".into(),
            url: "javascript:alert(1)".into(),
            ..Default::default()
        });
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Enter);
        assert!(!app.workspace.output_open);
        assert!(app.workspace.opening_link.is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.workspace.opening_link.is_some() {
            app.poll_projects();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            app.notice
                .as_ref()
                .unwrap()
                .contains("Could not open project link")
        );
        assert!(app.project_service.tasks.is_empty());
        assert_eq!(app.selected_project_link().unwrap().name, "Invalid");
    }

    #[test]
    fn web_links_group_folds_and_keeps_command_actions_separate() {
        let fixture = Fixture::new("links-group");
        let mut app = grouped_app(&fixture);
        app.metadata.projects[0].links = vec![
            ProjectLink {
                id: uuid::Uuid::from_u128(1),
                name: "Home".into(),
                url: "http://localhost:3000/".into(),
                ..Default::default()
            },
            ProjectLink {
                id: uuid::Uuid::from_u128(2),
                name: "Buy".into(),
                url: "http://localhost:3000/buy".into(),
                ..Default::default()
            },
        ];
        app.handle_projects_key(KeyCode::Enter);
        assert_eq!(
            &app.project_items()[..4],
            &[
                WorkspaceItem::Links,
                WorkspaceItem::Link(0),
                WorkspaceItem::Link(1),
                WorkspaceItem::Group("Services".into()),
            ]
        );
        assert!(app.project_links_header_selected());
        for key in ['s', 'r', 'x', 'A', 'X', 'e', 'd'] {
            app.handle_projects_key(KeyCode::Char(key));
            assert!(app.workspace.pending.is_none());
            assert!(app.workspace.delete.is_none());
            assert!(app.workspace.form.is_none());
        }
        assert!(app.project_service.tasks.is_empty());
        app.handle_projects_key(KeyCode::Enter);
        assert!(app.project_links_collapsed());
        assert!(!app.project_items().contains(&WorkspaceItem::Link(0)));
        assert!(app.workspace.opening_link.is_none());
        app.handle_projects_key(KeyCode::Right);
        assert!(!app.project_links_collapsed());
        app.handle_projects_key(KeyCode::Down);
        assert_eq!(app.selected_project_link().unwrap().name, "Home");
        app.handle_projects_key(KeyCode::Left);
        assert!(app.project_links_header_selected());
        app.handle_projects_key(KeyCode::Left);
        assert!(app.project_links_collapsed());
        app.handle_projects_key(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Enter);
        assert!(app.project_links_collapsed());
        app.handle_projects_key(KeyCode::Char('v'));
        assert_eq!(app.project_items()[0], WorkspaceItem::Link(0));
        assert_eq!(app.project_items()[1], WorkspaceItem::Link(1));
        assert!(!app.project_items().contains(&WorkspaceItem::Links));
        app.handle_projects_key(KeyCode::Char('v'));
        app.handle_projects_key(KeyCode::Char('L'));
        let form = app.workspace.form.as_mut().unwrap();
        form.fields[0] = "Docs".into();
        form.fields[1] = "https://example.com/docs".into();
        form.selected = 4;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(!app.project_links_collapsed());
        assert_eq!(app.selected_project_link().unwrap().name, "Docs");
        assert_eq!(app.workspace.selected_item, 3);
    }

    #[test]
    fn project_link_only_workspaces_render_and_edit_at_compact_sizes() {
        for (width, height) in [(120, 40), (80, 24), (54, 18), (32, 12)] {
            let fixture = Fixture::new("project-link-render");
            let mut app = grouped_app(&fixture);
            app.metadata.projects[0].commands.clear();
            app.metadata.projects[0].groups.clear();
            app.metadata.projects[0].links.push(ProjectLink {
                id: uuid::Uuid::from_u128(1),
                name: "Preview".into(),
                url: "http://localhost:3000/".into(),
                ..Default::default()
            });
            app.viewport_width = width;
            app.viewport_height = height;
            app.handle_projects_key(KeyCode::Enter);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("Web links"), "{width}x{height}: {text}");
            assert!(text.contains("Preview"), "{width}x{height}: {text}");
            assert!(
                text.contains("http://localhost:3000/"),
                "{width}x{height}: {text}"
            );
            app.handle_projects_key(KeyCode::Down);
            app.handle_projects_key(KeyCode::Char('e'));
            app.workspace.form.as_mut().unwrap().selected = 1;
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("URL"), "{width}x{height}: {text}");
        }
    }

    #[test]
    fn grouped_browser_and_last_form_field_render_at_compact_sizes() {
        for (width, height) in [(120, 40), (80, 24), (54, 18), (32, 12)] {
            let fixture = Fixture::new("group-render");
            let mut app = grouped_app(&fixture);
            app.metadata.projects[0].links = vec![
                ProjectLink {
                    id: uuid::Uuid::from_u128(1),
                    name: "Home".into(),
                    url: "http://localhost:3000/".into(),
                    ..Default::default()
                },
                ProjectLink {
                    id: uuid::Uuid::from_u128(2),
                    name: "Buy".into(),
                    url: "http://localhost:3000/buy".into(),
                    ..Default::default()
                },
            ];
            app.viewport_width = width;
            app.viewport_height = height;
            app.handle_projects_key(KeyCode::Enter);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let items = app.project_items();
            for (index, item) in items.iter().enumerate() {
                app.workspace.selected_item = index;
                let name = match item {
                    WorkspaceItem::Links => "Web links",
                    WorkspaceItem::Group(group) => group.as_str(),
                    WorkspaceItem::Command(index) => {
                        app.metadata.projects[0].commands[*index].name.as_str()
                    }
                    WorkspaceItem::Link(index) => {
                        app.metadata.projects[0].links[*index].name.as_str()
                    }
                };
                terminal
                    .draw(|frame| crate::ui::render(frame, &app))
                    .unwrap();
                let text = format!("{}", terminal.backend());
                assert!(text.contains(name), "{width}x{height}: missing {name}");
            }
            app.workspace.selected_item = 0;
            app.handle_projects_key(KeyCode::Enter);
            app.handle_projects_key(KeyCode::Char('a'));
            app.workspace.form.as_mut().unwrap().selected = 6;
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let text = format!("{}", terminal.backend());
            assert!(
                text.contains("On exit"),
                "{width}x{height}: lifecycle hidden"
            );
            assert!(
                text.contains("Stop on exit"),
                "{width}x{height}: lifecycle value hidden"
            );
        }
    }

    #[test]
    fn groups_expand_inline_and_remember_folds_separately_for_each_project() {
        let fixture = Fixture::new("inline-groups");
        let mut app = grouped_app(&fixture);
        app.handle_projects_key(KeyCode::Enter);
        assert!(app.project_items().contains(&WorkspaceItem::Command(0)));
        app.handle_projects_key(KeyCode::Left);
        assert!(app.project_group_collapsed("Services"));
        assert!(!app.project_items().contains(&WorkspaceItem::Command(0)));
        assert!(app.project_items().contains(&WorkspaceItem::Command(1)));
        assert!(app.selected_project_command().is_none());
        app.handle_projects_key(KeyCode::Char('s'));
        assert!(app.workspace.pending.is_none());
        app.handle_projects_key(KeyCode::Right);
        assert!(!app.project_group_collapsed("Services"));
        app.handle_projects_key(KeyCode::Down);
        assert_eq!(
            app.selected_project_key(),
            Some((fixture.project.id, uuid::Uuid::from_u128(1)))
        );
        app.handle_projects_key(KeyCode::Left);
        assert_eq!(app.workspace.selected_item, 0);
        assert!(!app.project_group_collapsed("Services"));
        app.handle_projects_key(KeyCode::Char(' '));
        assert!(app.project_group_collapsed("Services"));
        app.handle_projects_key(KeyCode::Char('v'));
        assert!(app.project_items().contains(&WorkspaceItem::Command(0)));
        app.handle_projects_key(KeyCode::Char('v'));
        assert!(!app.project_items().contains(&WorkspaceItem::Command(0)));
        app.handle_projects_key(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Enter);
        assert!(app.project_group_collapsed("Services"));
        let mut other = app.metadata.projects[0].clone();
        other.id = uuid::Uuid::new_v4();
        app.metadata.projects.push(other);
        app.handle_projects_key(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Enter);
        assert!(!app.project_group_collapsed("Services"));
        assert!(app.project_items().contains(&WorkspaceItem::Command(0)));
        assert!(app.project_service.tasks.is_empty());
    }
}
