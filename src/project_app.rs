use crossterm::event::KeyCode;
use std::sync::mpsc::{Receiver, TryRecvError};

use crate::{
    app::{App, NavSection},
    project_monitor::{ExternalRun, ProjectMonitor, stop_external_async},
    projects::{ProjectCommand, ProjectDefinition, TaskKey, working_directory},
};

#[derive(Default)]
pub struct WorkspaceState {
    pub selected_project: usize,
    pub selected_command: usize,
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
}

impl WorkspaceForm {
    pub fn labels(&self) -> &[&str] {
        if self.project_id.is_some() {
            &["Name", "Command", "Working directory (optional)"]
        } else {
            &["Project name", "Base directory"]
        }
    }
}

pub enum WorkspaceDelete {
    Project { id: u64, name: String },
    Command { key: TaskKey, name: String },
}

impl App {
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
            KeyCode::Char('[') | KeyCode::Char(']') if self.workspace.output_open => {
                let count = self
                    .selected_project()
                    .map_or(0, |project| project.commands.len());
                self.workspace.selected_command = if key == KeyCode::Char('[') {
                    self.workspace.selected_command.saturating_sub(1)
                } else {
                    (self.workspace.selected_command + 1).min(count.saturating_sub(1))
                };
                self.workspace.output_scroll = None;
            }
            KeyCode::Up | KeyCode::Down => {
                let (selected, count) = if self.workspace.project_open {
                    let count = self
                        .selected_project()
                        .map_or(0, |project| project.commands.len());
                    (&mut self.workspace.selected_command, count)
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
                    if self.selected_project_command().is_some() {
                        self.workspace.output_open = true;
                        self.workspace.output_scroll = None;
                    }
                } else if self.selected_project().is_some() {
                    self.workspace.project_open = true;
                    self.workspace.selected_command = 0;
                }
            }
            KeyCode::Char('a') => self.open_workspace_form(false),
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
                if let Some(project) = self.selected_project().cloned() {
                    self.queue_project_action(
                        ProjectAction::Start,
                        project
                            .commands
                            .iter()
                            .map(|command| (project.id, command.id))
                            .collect(),
                    );
                }
            }
            KeyCode::Char('X') => {
                if let Some(project) = self.selected_project().cloned() {
                    self.queue_project_action(
                        ProjectAction::Stop,
                        project
                            .commands
                            .iter()
                            .map(|command| (project.id, command.id))
                            .collect(),
                    );
                }
            }
            _ => {}
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
        let form = if self.workspace.project_open {
            let Some(project) = self.selected_project() else {
                return;
            };
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
                    || vec![String::new(); 3],
                    |command| {
                        vec![
                            command.name,
                            command.command,
                            command
                                .working_directory
                                .map_or_else(String::new, |p| p.display().to_string()),
                        ]
                    },
                ),
                selected: 0,
                error: None,
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
                    || vec![String::new(), self.local_files.path.display().to_string()],
                    |project| vec![project.name, project.base_path.display().to_string()],
                ),
                selected: 0,
                error: None,
            }
        };
        self.workspace.form = Some(form);
    }

    fn request_workspace_delete(&mut self) {
        if self.workspace.project_open {
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
                    }
                    if !self.save_metadata() {
                        self.metadata.projects = previous;
                        return;
                    }
                    match target {
                        WorkspaceDelete::Project { id, .. } => {
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
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => self.workspace.delete = None,
                _ => {}
            }
            return;
        }
        let Some(form) = self.workspace.form.as_mut() else {
            return;
        };
        match key {
            KeyCode::Esc => self.workspace.form = None,
            KeyCode::Tab | KeyCode::Down => form.selected = (form.selected + 1) % form.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                form.selected = (form.selected + form.fields.len() - 1) % form.fields.len()
            }
            KeyCode::Backspace => {
                form.fields[form.selected].pop();
            }
            KeyCode::Char(value) if !value.is_control() => form.fields[form.selected].push(value),
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
        if let Some(project_id) = form.project_id {
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
                name,
                command: form.fields[1].trim().to_string(),
                working_directory: (!form.fields[2].trim().is_empty())
                    .then(|| form.fields[2].trim().into()),
            };
            if command.command.is_empty() {
                return Err("Command is required.".into());
            }
            working_directory(project, &command).map_err(|error| error.to_string())?;
            if let Some(existing) = project.commands.iter_mut().find(|c| c.id == command.id) {
                *existing = command;
            } else {
                project.commands.push(command);
                self.workspace.selected_command = project.commands.len() - 1;
            }
        } else {
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
                name,
                base_path: form.fields[1].trim().into(),
                commands: form
                    .editing_id
                    .and_then(|id| self.metadata.projects.iter().find(|p| p.id == id))
                    .map_or_else(Vec::new, |p| p.commands.clone()),
            };
            working_directory(&project, &ProjectCommand::default())
                .map_err(|error| error.to_string())?;
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
        Ok(())
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
        self.workspace.selected_project = index;
        self.workspace.selected_command = self.metadata.projects[index]
            .commands
            .iter()
            .position(|command| command.id == key.1)
            .unwrap_or(0);
        self.workspace.project_open = true;
        self.workspace.output_open = true;
        self.workspace.output_scroll = None;
        self.switch_section(NavSection::Projects);
    }
}
