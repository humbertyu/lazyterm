use crossterm::event::KeyCode;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::{
    app::{App, NavSection},
    project_monitor::{ExternalRun, ProjectMonitor, stop_external_async},
    projects::{
        CommandKind, ProjectCommand, ProjectDefinition, ProjectTarget, TaskKey, execution_target,
        working_directory,
    },
};

#[derive(Default)]
pub struct WorkspaceState {
    pub selected_project: usize,
    pub selected_command: usize,
    pub selected_item: usize,
    collapsed_groups: std::collections::HashSet<(u64, String)>,
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
    pub project_id: Option<u64>,
    pub editing_id: Option<u64>,
    pub fields: Vec<String>,
    pub selected: usize,
    pub error: Option<String>,
    pub group_form: bool,
    pub original_group: Option<String>,
}

impl WorkspaceForm {
    pub fn labels(&self) -> &[&str] {
        if self.group_form {
            &["Group name"]
        } else if self.project_id.is_some() {
            &[
                "Name",
                "Command",
                "Working directory (optional)",
                "Group (optional)",
                "Type",
                "Target",
            ]
        } else {
            &["Project name", "Base directory", "Default target"]
        }
    }
}

pub enum WorkspaceDelete {
    Project { id: u64, name: String },
    Command { key: TaskKey, name: String },
    Group { project_id: u64, name: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceItem {
    Group(String),
    Command(usize),
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
        if self.workspace.flat {
            return (0..project.commands.len())
                .map(WorkspaceItem::Command)
                .collect();
        }
        let mut items = Vec::new();
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

    pub fn poll_projects(&mut self) {
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
            if let Some(project) = self
                .metadata
                .projects
                .iter()
                .find(|project| project.id == key.0)
                && let Some(command) = project.commands.iter().find(|command| command.id == key.1)
                && *execution_target(project, command) != ProjectTarget::Local
            {
                self.notice = Some(format!(
                    "{} targets {}; remote Project execution is not available yet.",
                    command.name,
                    execution_target(project, command).label()
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
                ProjectAction::Start => self.project_service.start(&project, command),
                ProjectAction::Stop => self.project_service.stop(key),
                ProjectAction::Restart => self.project_service.restart(&project, command),
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
                        ]
                    },
                ),
                selected: 0,
                error: None,
                group_form: false,
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
                    .owners()
                    .iter()
                    .any(|owner| owner.key.0 == project.id)
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
            original_group,
        });
    }

    fn request_workspace_delete(&mut self) {
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
                .owners()
                .iter()
                .any(|owner| owner.key.0 == project.id)
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
                    match target {
                        WorkspaceDelete::Project { id, .. } => {
                            self.workspace
                                .collapsed_groups
                                .retain(|(project_id, _)| *project_id != id);
                            self.project_service
                                .tasks
                                .retain(|(project_id, _), _| *project_id != id);
                            self.workspace.selected_project = self
                                .workspace
                                .selected_project
                                .min(self.metadata.projects.len().saturating_sub(1));
                        }
                        WorkspaceDelete::Command { key, .. } => {
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
        let type_field = !form.group_form && form.project_id.is_some() && form.selected == 4;
        let target_field =
            !form.group_form && form.selected == if form.project_id.is_some() { 5 } else { 2 };
        let choices = if type_field {
            vec!["Task".to_string(), "Service".into()]
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
            form.fields[form.selected] = choices[next].clone();
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
            KeyCode::Enter if form.selected + 1 < form.fields.len() => form.selected += 1,
            KeyCode::Enter => {
                let mut form = self.workspace.form.take().expect("checked");
                if let Err(error) = self.save_workspace_form(&form) {
                    form.error = Some(error);
                    self.workspace.form = Some(form);
                }
            }
            _ => {}
        }
    }

    fn save_workspace_form(&mut self, form: &WorkspaceForm) -> Result<(), String> {
        let previous = self.metadata.projects.clone();
        let name = form.fields[0].trim().to_string();
        if name.is_empty() {
            return Err("Name is required.".into());
        }
        if form.group_form {
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
                id: form.editing_id.unwrap_or_else(|| {
                    project.commands.iter().map(|c| c.id).max().unwrap_or(0) + 1
                }),
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
                id: form.editing_id.unwrap_or_else(|| {
                    self.metadata
                        .projects
                        .iter()
                        .map(|p| p.id)
                        .max()
                        .unwrap_or(0)
                        + 1
                }),
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
        self.notice = Some("Saved project definition.".into());
        if form.group_form {
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
                id,
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
        assert_eq!(app.selected_project_key(), Some((fixture.project.id, 1)));
        assert!(app.workspace.output_open);
        app.handle_projects_key(KeyCode::Char(']'));
        assert_eq!(app.selected_project_key(), Some((fixture.project.id, 2)));
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
        assert_eq!(app.selected_project_key(), Some((fixture.project.id, 2)));
        app.handle_projects_key(KeyCode::Char('e'));
        let form = app.workspace.form.as_mut().unwrap();
        assert_eq!(form.fields[3], "Checks");
        form.fields[3] = "Diagnostics".into();
        form.selected = 5;
        app.handle_workspace_dialog(KeyCode::Enter);
        assert!(app.workspace.form.is_none());
        assert_eq!(app.selected_project_key(), Some((fixture.project.id, 2)));
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
        listener.id = 1;
        listener.group = Some("Services".into());
        listener.kind = CommandKind::Service;
        app.metadata.projects[0].commands[0] = listener;
        app.metadata.projects[0].commands[1].kind = CommandKind::Service;
        app.handle_projects_key(KeyCode::Enter);
        assert_eq!(app.project_service_keys(), vec![(fixture.project.id, 1)]);
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
        assert!(app.project_service.active((fixture.project.id, 1)));
        app.handle_projects_key(KeyCode::Char('X'));
        while app.project_service.active((fixture.project.id, 1)) {
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
    fn target_inheritance_never_executes_remote_commands_locally() {
        let fixture = Fixture::new("group-target");
        let mut app = grouped_app(&fixture);
        app.metadata.projects[0].target = ProjectTarget::Host("web".into());
        let project = app.metadata.projects[0].clone();
        let command = &project.commands[0];
        assert_eq!(
            execution_target(&project, command),
            &ProjectTarget::Host("web".into())
        );
        assert!(app.project_service.start(&project, command).is_err());
        assert!(app.project_service.tasks.is_empty());
        app.handle_projects_key(KeyCode::Enter);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Char('s'));
        assert!(app.workspace.pending.is_none());
        assert!(
            app.notice
                .as_ref()
                .unwrap()
                .contains("remote Project execution")
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
            "id": 1, "name": "Old", "base_path": "/tmp",
            "commands": [{"id": 1, "name": "check", "command": "true", "working_directory": null}]
        }"#,
        )
        .unwrap();
        assert!(project.groups.is_empty());
        assert_eq!(project.target, ProjectTarget::Local);
        assert_eq!(project.commands[0].kind, CommandKind::Task);
        assert!(project.commands[0].group.is_none());
        assert!(project.commands[0].target.is_none());
    }

    #[test]
    fn grouped_browser_and_last_form_field_render_at_compact_sizes() {
        for (width, height) in [(120, 40), (80, 24), (54, 18), (32, 12)] {
            let fixture = Fixture::new("group-render");
            let mut app = grouped_app(&fixture);
            app.viewport_width = width;
            app.viewport_height = height;
            app.handle_projects_key(KeyCode::Enter);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let items = app.project_items();
            for (index, item) in items.iter().enumerate() {
                app.workspace.selected_item = index;
                let name = match item {
                    WorkspaceItem::Group(group) => group.as_str(),
                    WorkspaceItem::Command(index) => {
                        app.metadata.projects[0].commands[*index].name.as_str()
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
            app.workspace.form.as_mut().unwrap().selected = 5;
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("Target"), "{width}x{height}: target hidden");
            assert!(
                text.contains("Inherit"),
                "{width}x{height}: target value hidden"
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
        assert_eq!(app.selected_project_key(), Some((fixture.project.id, 1)));
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
        other.id += 1;
        app.metadata.projects.push(other);
        app.handle_projects_key(KeyCode::Esc);
        app.handle_projects_key(KeyCode::Down);
        app.handle_projects_key(KeyCode::Enter);
        assert!(!app.project_group_collapsed("Services"));
        assert!(app.project_items().contains(&WorkspaceItem::Command(0)));
        assert!(app.project_service.tasks.is_empty());
    }
}
