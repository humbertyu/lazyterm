use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};

use crate::{
    app::{
        App, ConnectionDialog, FileDialog, FilePane, FilesState, HostPickerItem,
        LOCAL_BREADCRUMB_SEPARATOR, LocalEntryKind, NavSection, TransferStatus, TunnelStatus,
        group_columns, host_browser_width,
    },
    sftp::SftpEntryKind,
};

const BG: Color = Color::Rgb(13, 16, 17);
const PANEL: Color = Color::Rgb(18, 23, 23);
const BORDER: Color = Color::Rgb(49, 65, 61);
const GREEN: Color = Color::Rgb(133, 196, 159);
const TEXT: Color = Color::Rgb(215, 228, 218);
const MUTED: Color = Color::Rgb(107, 126, 116);
const DISABLED: Color = Color::Rgb(76, 91, 82);
const LIGHT_BG: Color = Color::Rgb(238, 242, 239);

fn runtime_origin_color(origin: crate::runtime::RuntimeOrigin) -> Color {
    use crate::runtime::RuntimeOrigin;
    match origin {
        RuntimeOrigin::Project => GREEN,
        RuntimeOrigin::ExternalProject => Color::Rgb(103, 211, 211),
        RuntimeOrigin::Tunnel => Color::Rgb(190, 145, 255),
        RuntimeOrigin::Agent => Color::Rgb(239, 192, 101),
        RuntimeOrigin::CommandLine => Color::Rgb(103, 211, 211),
        RuntimeOrigin::Desktop => Color::Rgb(160, 171, 190),
        RuntimeOrigin::Unknown => MUTED,
    }
}

pub fn render(frame: &mut Frame, app: &App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(
            if app.metadata.theme == crate::metadata::Theme::Light {
                LIGHT_BG
            } else {
                BG
            },
        )),
        frame.area(),
    );
    let page = page_layout().split(frame.area());

    render_header(frame, page[0], app);
    render_navigation(frame, page[1], app);
    match app.section {
        NavSection::Hosts => render_hosts(frame, page[2], app),
        NavSection::Files => render_files(frame, page[2], app),
        NavSection::Tunnels => render_tunnels(frame, page[2], app),
        NavSection::Snippets => render_snippets(frame, page[2], app),
        NavSection::Runtime => render_runtime(frame, app),
        NavSection::Projects => render_projects(frame, page[2], app),
        NavSection::Credentials => crate::credential_ui::render_page(frame, page[2], app),
        NavSection::Settings => render_settings(frame, page[2], app),
    }
    render_footer(frame, page[3], app);
    render_dialogs(frame, app);
}

fn page_layout() -> Layout {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
}

fn render_dialogs(frame: &mut Frame, app: &App) {
    if app.searching {
        render_search(frame, app);
    }
    if let Some(dialog) = &app.connection_dialog {
        render_connection_dialog(frame, dialog);
    }
    if let Some(dialog) = &app.host_dialog {
        render_host_dialog(frame, dialog);
    }
    if let Some(dialog) = &app.tunnel_dialog {
        render_tunnel_dialog(frame, dialog);
    }
    if app.tunnel_host_picker_open {
        render_tunnel_host_picker(frame, app);
    }
    if app.snippet_host_picker_open {
        render_snippet_host_picker(frame, app);
    }
    if let Some(dialog) = &app.snippet_dialog {
        render_snippet_dialog(frame, dialog, app);
    }
    if let Some(dialog) = &app.package_dialog {
        render_package_dialog(frame, dialog);
    }
    if let Some(dialog) = &app.credential_export_dialog {
        render_credential_export_dialog(frame, dialog);
    }
    if let Some(pending) = &app.snippet_confirmation {
        render_snippet_confirmation(frame, pending);
    }
    if let Some(target) = &app.snippet_delete_confirmation {
        render_snippet_delete_confirmation(frame, target);
    }
    if app.runtime_host_picker_open {
        render_runtime_host_picker(frame, app);
    }
    if let Some(entry) = &app.runtime_kill_confirmation {
        render_runtime_kill_confirmation(frame, entry, &app.runtime_target, false);
    }
    if let Some(entry) = &app.runtime_force_kill_confirmation {
        render_runtime_kill_confirmation(frame, entry, &app.runtime_target, true);
    }
    if let Some((alias, tags)) = &app.tags_dialog {
        let area = centered_rect(68, 7, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!("{tags}_")),
                Line::from(""),
                Line::from(Span::styled(
                    "Comma-separated tags   Enter save   Esc cancel",
                    Style::default().fg(MUTED),
                )),
            ])
            .style(Style::default().fg(TEXT))
            .block(panel(&format!(" TAGS / {alias} ")))
            .wrap(Wrap { trim: true }),
            area,
        );
    }
    if let Some(dialog) = &app.file_dialog {
        render_file_dialog(frame, dialog);
    }
    render_workspace_dialog(frame, app);
    crate::credential_ui::render(frame, app);
}

fn project_status(app: &App, key: crate::projects::TaskKey) -> (String, Color) {
    use crate::projects::TaskStatus;
    if !app.project_service.active(key) {
        let external = app.external_project_runs(key);
        if let Some(run) = external.first() {
            return (
                format!(
                    "{}{}",
                    run.label(),
                    if external.len() > 1 {
                        format!(" / {} instances", external.len())
                    } else {
                        String::new()
                    }
                ),
                GREEN,
            );
        }
        if app.workspace.monitor.matches(&app.metadata.projects)
            && app.workspace.monitor.error.is_some()
        {
            return ("unknown / inspection failed".into(), Color::Yellow);
        }
        if !app.workspace.monitor.matches(&app.metadata.projects) {
            return ("checking...".into(), MUTED);
        }
        return (
            if app.project_service.tasks.contains_key(&key) {
                "stopped"
            } else {
                "not started"
            }
            .into(),
            MUTED,
        );
    }
    app.project_service.tasks.get(&key).map_or_else(
        || ("not started".into(), MUTED),
        |task| {
            let color = match task.status {
                TaskStatus::Running => GREEN,
                TaskStatus::Stopping => Color::Yellow,
                TaskStatus::Exited(0) | TaskStatus::Stopped => MUTED,
                TaskStatus::Exited(_) | TaskStatus::Failed(_) => Color::Red,
            };
            (
                format!(
                    "{}{}",
                    task.status.label(),
                    if task.lifecycle == crate::projects::ProjectLifecycle::Persistent {
                        " / background"
                    } else {
                        ""
                    }
                ),
                color,
            )
        },
    )
}

fn project_last_result(app: &App, key: crate::projects::TaskKey) -> Option<String> {
    app.project_service
        .tasks
        .get(&key)
        .filter(|task| !task.active())
        .map(|task| task.status.label())
}

fn render_projects(frame: &mut Frame, area: Rect, app: &App) {
    let Some(project) = app.selected_project() else {
        frame.render_widget(
            Paragraph::new("No projects.")
                .style(Style::default().fg(MUTED))
                .block(panel(" PROJECTS ")),
            area,
        );
        return;
    };
    if app.workspace.output_open {
        let Some(command) = app.selected_project_command() else {
            return;
        };
        let key = (project.id, command.id);
        let (status, color) = project_status(app, key);
        let header_height = if area.height < 12 { 2 } else { 4 };
        let sections =
            Layout::vertical([Constraint::Length(header_height), Constraint::Min(1)]).split(area);
        let directory = crate::projects::working_directory(project, command).map_or_else(
            |_| project.base_path.display().to_string(),
            |path| path.display().to_string(),
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!(" {} / {}   ", project.name, command.name),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(status, Style::default().fg(color))),
                Line::from(Span::styled(
                    format!(" {directory}"),
                    Style::default().fg(MUTED),
                )),
                Line::from(Span::styled(
                    project_last_result(app, key)
                        .map_or_else(String::new, |result| format!(" Last managed run: {result}")),
                    Style::default().fg(MUTED),
                )),
            ]),
            sections[0],
        );
        let task = app.project_service.tasks.get(&key);
        let external = !app.external_project_runs(key).is_empty();
        let block = panel(if external {
            " OUTPUT / LAST MANAGED RUN "
        } else if app.workspace.output_scroll.is_some() {
            " OUTPUT / SCROLLED "
        } else {
            " OUTPUT / LIVE "
        });
        let inner = block.inner(sections[1]);
        frame.render_widget(block, sections[1]);
        let text = task.map_or(
            if external {
                "No captured output for the external process."
            } else {
                "No output."
            },
            |task| task.output.as_str(),
        );
        let paragraph = Paragraph::new(text)
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: false });
        let end = paragraph
            .line_count(inner.width)
            .saturating_sub(usize::from(inner.height));
        let scroll = app.workspace.output_scroll.unwrap_or(end).min(end);
        frame.render_widget(
            paragraph.scroll((scroll.min(u16::MAX as usize) as u16, 0)),
            inner,
        );
        return;
    }

    let wide = area.width >= 75;
    let columns = Layout::horizontal([
        Constraint::Percentage(if wide { 40 } else { 100 }),
        Constraint::Percentage(if wide { 60 } else { 0 }),
    ])
    .split(area);
    let title = if app.workspace.project_open {
        format!(
            " {} / {} ",
            project.name,
            if app.workspace.flat {
                "All items"
            } else {
                "Workspace"
            }
        )
    } else {
        " PROJECTS ".into()
    };
    let mut state = ListState::default();
    let rows: Vec<ListItem> = if app.workspace.project_open {
        state.select(Some(app.workspace.selected_item));
        app.project_items()
            .iter()
            .map(|item| {
                let command = match item {
                    crate::project_app::WorkspaceItem::Links => {
                        return ListItem::new(Line::from(Span::styled(
                            format!(
                                "{} Web links  {}",
                                if app.project_links_collapsed() {
                                    "▸"
                                } else {
                                    "▾"
                                },
                                project.links.len()
                            ),
                            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                        )));
                    }
                    crate::project_app::WorkspaceItem::Link(index) => {
                        let link = &project.links[*index];
                        let indent = if app.workspace.flat { "" } else { "  " };
                        return ListItem::new(vec![
                            Line::from(Span::styled(
                                format!("{indent}{}", link.name),
                                Style::default().fg(TEXT),
                            )),
                            Line::from(vec![
                                Span::styled(format!("{indent}  "), Style::default()),
                                Span::styled(link.url.as_str(), Style::default().fg(MUTED)),
                            ]),
                        ]);
                    }
                    crate::project_app::WorkspaceItem::Group(group) => {
                        let commands = project
                            .commands
                            .iter()
                            .filter(|command| command.group.as_ref() == Some(group))
                            .collect::<Vec<_>>();
                        let running = commands
                            .iter()
                            .filter(|command| app.project_is_running((project.id, command.id)))
                            .count();
                        return ListItem::new(Line::from(Span::styled(
                            format!(
                                "{} {group}  {} / {running} active",
                                if app.project_group_collapsed(group) {
                                    "▸"
                                } else {
                                    "▾"
                                },
                                commands.len()
                            ),
                            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                        )));
                    }
                    crate::project_app::WorkspaceItem::Command(index) => &project.commands[*index],
                };
                let indent = if !app.workspace.flat && command.group.is_some() {
                    "  "
                } else {
                    ""
                };
                let (status, color) = project_status(app, (project.id, command.id));
                let mut lines = vec![
                    Line::from(Span::styled(
                        format!("{indent}{}", command.name),
                        Style::default().fg(TEXT),
                    )),
                    Line::from(Span::styled(
                        format!(
                            "{indent}  {} / {}",
                            command.kind.label(),
                            crate::projects::execution_target(project, command).label()
                        ),
                        Style::default().fg(MUTED),
                    )),
                    Line::from(Span::styled(
                        format!("{indent}  {status}"),
                        Style::default().fg(color),
                    )),
                ];
                if let Some(result) = project_last_result(app, (project.id, command.id)) {
                    lines.push(Line::from(Span::styled(
                        format!("{indent}  last run: {result}"),
                        Style::default().fg(MUTED),
                    )));
                }
                if !wide {
                    lines.push(Line::from(Span::styled(
                        format!("{indent}  {}", command.command),
                        Style::default().fg(MUTED),
                    )));
                }
                ListItem::new(lines)
            })
            .collect()
    } else {
        state.select(Some(app.workspace.selected_project));
        app.metadata
            .projects
            .iter()
            .map(|project| {
                let running = project
                    .commands
                    .iter()
                    .filter(|command| app.project_is_running((project.id, command.id)))
                    .count();
                ListItem::new(vec![
                    Line::from(Span::styled(
                        project.name.as_str(),
                        Style::default().fg(TEXT),
                    )),
                    Line::from(Span::styled(
                        format!("  {}", project.base_path.display()),
                        Style::default().fg(MUTED),
                    )),
                    Line::from(Span::styled(
                        format!(
                            "  {} commands / {} links / {running} active",
                            project.commands.len(),
                            project.links.len()
                        ),
                        Style::default().fg(MUTED),
                    )),
                ])
            })
            .collect()
    };
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No commands or links.")
                .style(Style::default().fg(MUTED))
                .block(panel(&title)),
            columns[0],
        );
    } else {
        frame.render_stateful_widget(
            List::new(rows)
                .block(panel(&title))
                .highlight_symbol("> ")
                .highlight_style(
                    Style::default()
                        .bg(Color::Rgb(47, 78, 61))
                        .add_modifier(Modifier::BOLD),
                ),
            columns[0],
            &mut state,
        );
    }
    if !wide {
        return;
    }
    let mut lines = vec![
        detail_line("Project", &project.name),
        detail_line("Base directory", &project.base_path.display().to_string()),
        detail_line("Default target", &project.target.label()),
    ];
    if app.project_links_header_selected() {
        lines.push(detail_line("Group", "Web links"));
        lines.push(detail_line("Links", &project.links.len().to_string()));
        for link in &project.links {
            lines.push(detail_line(&link.name, &link.url));
        }
    } else if let Some(link) = app.selected_project_link() {
        lines.extend([
            detail_line("Link", &link.name),
            detail_line("URL", &link.url),
            detail_line(
                "Username",
                if link.username.is_empty() {
                    "(none)"
                } else {
                    &link.username
                },
            ),
            detail_line(
                "Password",
                if link.password_ref.is_some() {
                    "******** (keychain)"
                } else if link.password.is_some() {
                    "******** (config file)"
                } else {
                    "(none)"
                },
            ),
        ]);
    } else if app.workspace.project_open
        && let Some(command) = app.selected_project_command()
    {
        let key = (project.id, command.id);
        lines.extend([
            detail_line("Command name", &command.name),
            detail_line("Group", command.group.as_deref().unwrap_or("Ungrouped")),
            detail_line("Type", command.kind.label()),
            detail_line("On exit", command.lifecycle.label()),
            detail_line(
                "Target",
                &crate::projects::execution_target(project, command).label(),
            ),
            detail_line("Command", &command.command),
            detail_line(
                "Working directory",
                &command.working_directory.as_ref().map_or_else(
                    || "(project base)".into(),
                    |path| path.display().to_string(),
                ),
            ),
            detail_line("Status", &project_status(app, key).0),
        ]);
        if let Some(result) = project_last_result(app, key) {
            lines.push(detail_line("Last managed run", &result));
        }
        for run in app.external_project_runs(key) {
            lines.extend([
                detail_line("Ownership", "External"),
                detail_line("External PID", &run.root.pid.to_string()),
                detail_line("External command", &run.root.command),
                detail_line("External directory", &run.directory.display().to_string()),
                detail_line(
                    "Ports",
                    &run.ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ]);
        }
        if let Some(error) = &app.workspace.monitor.error {
            lines.push(detail_line("Inspection", error));
        }
        if let Some(task) = app.project_service.tasks.get(&key) {
            lines.extend([
                detail_line("Process group", &task.owner.process_group.to_string()),
                detail_line("Executed command", &task.command),
                detail_line("Executed directory", &task.directory.display().to_string()),
                detail_line("Stdout log", &task.stdout_log.display().to_string()),
                detail_line("Stderr log", &task.stderr_log.display().to_string()),
            ]);
        }
    } else if app.workspace.project_open
        && let Some(crate::project_app::WorkspaceItem::Group(group)) =
            app.project_items().get(app.workspace.selected_item)
    {
        let commands = project
            .commands
            .iter()
            .filter(|command| command.group.as_ref() == Some(group))
            .collect::<Vec<_>>();
        lines.push(detail_line("Group", group));
        lines.push(detail_line("Commands", &commands.len().to_string()));
        lines.push(detail_line(
            "Services",
            &commands
                .iter()
                .filter(|command| command.kind == crate::projects::CommandKind::Service)
                .count()
                .to_string(),
        ));
    } else {
        lines.push(detail_line("Commands", &project.commands.len().to_string()));
        lines.push(detail_line("Web links", &project.links.len().to_string()));
        for link in &project.links {
            lines.push(detail_line(&link.name, &link.url));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(" DETAILS "))
            .wrap(Wrap { trim: false }),
        columns[1],
    );
}

pub(crate) fn project_output_end(app: &App) -> usize {
    let page = page_layout().split(Rect::new(0, 0, app.viewport_width, app.viewport_height));
    let header_height = if page[2].height < 12 { 2 } else { 4 };
    let sections =
        Layout::vertical([Constraint::Length(header_height), Constraint::Min(1)]).split(page[2]);
    let inner = panel(" OUTPUT ").inner(sections[1]);
    app.selected_project_key()
        .and_then(|key| app.project_service.tasks.get(&key))
        .map_or(0, |task| {
            Paragraph::new(task.output.as_str())
                .wrap(Wrap { trim: false })
                .line_count(inner.width)
                .saturating_sub(usize::from(inner.height))
        })
}

fn render_workspace_dialog(frame: &mut Frame, app: &App) {
    if let Some(confirmation) = &app.workspace.confirmation {
        let area = centered_rect(86, 18, frame.area());
        frame.render_widget(Clear, area);
        let mut lines = vec![
            Line::from(
                if confirmation.action == crate::project_app::ProjectAction::Restart {
                    "Stop external commands and restart under LazyTerm?"
                } else {
                    "Stop external commands and their child processes?"
                },
            ),
            Line::from(""),
        ];
        for run in &confirmation.runs {
            lines.push(Line::from(format!(
                "{} / {} / PID {}",
                run.project_name, run.command_name, run.root.pid
            )));
            lines.push(Line::from(run.root.command.clone()));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("y / Enter confirm    n / Esc cancel"));
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel(" EXTERNAL PROCESS "))
                .style(Style::default().fg(TEXT))
                .wrap(Wrap { trim: false }),
            area,
        );
    } else if let Some(form) = &app.workspace.form {
        let area = centered_rect(86, (form.fields.len() * 3 + 5) as u16, frame.area());
        frame.render_widget(Clear, area);
        let title = format!(
            " {} {} ",
            if form.editing_id.is_some() || form.original_group.is_some() {
                "EDIT"
            } else {
                "ADD"
            },
            if form.group_form {
                "GROUP"
            } else if form.link_form {
                "WEB LINK"
            } else if form.project_id.is_some() {
                "COMMAND"
            } else {
                "PROJECT"
            }
        );
        let block = panel(&title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let compact = area.height < (form.fields.len() * 3 + 4) as u16;
        let field_height = if compact { 2 } else { 3 };
        let visible_count = (usize::from(inner.height.saturating_sub(2)) / field_height)
            .max(1)
            .min(form.fields.len());
        let start = form.selected.saturating_sub(visible_count - 1);
        let mut fields = vec![Constraint::Length(field_height as u16); visible_count];
        fields.push(Constraint::Min(1));
        let rows = Layout::vertical(fields).split(inner);
        for (row_index, (index, value)) in form
            .fields
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_count)
            .enumerate()
        {
            let active = index == form.selected;
            let inner =
                if compact {
                    let fields = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
                        .split(rows[row_index]);
                    frame.render_widget(
                        Paragraph::new(form.labels()[index])
                            .style(Style::default().fg(if active { GREEN } else { MUTED })),
                        fields[0],
                    );
                    fields[1]
                } else {
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .title(form.labels()[index])
                        .border_style(Style::default().fg(if active { GREEN } else { BORDER }));
                    let inner = block.inner(rows[row_index]);
                    frame.render_widget(block, rows[row_index]);
                    inner
                };
            let choice = (form.link_form && (index == 4 || index == 5))
                || (!form.group_form
                    && !form.link_form
                    && (if form.project_id.is_some() {
                        index >= 4
                    } else {
                        index == 2
                    }));
            let value = if form.link_form && index == 3 {
                let display = if active && !value.is_empty() {
                    value.as_str()
                } else if !value.is_empty() {
                    "********"
                } else if form.editing_id.is_some()
                    && app
                        .selected_project_link()
                        .is_some_and(|link| link.password_ref.is_some() || link.password.is_some())
                {
                    "(saved; blank keeps it)"
                } else {
                    ""
                };
                format!("{display}{}", if active { "_" } else { "" })
            } else if choice {
                format!("‹ {value} ›")
            } else {
                format!("{value}{}", if active { "_" } else { "" })
            };
            let width = Line::from(value.as_str()).width();
            frame.render_widget(
                Paragraph::new(value)
                    .style(Style::default().fg(TEXT))
                    .scroll((
                        0,
                        width
                            .saturating_sub(inner.width as usize)
                            .min(u16::MAX as usize) as u16,
                    )),
                inner,
            );
        }
        if let Some(error) = &form.error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: false }),
                rows[visible_count],
            );
        }
    } else if let Some(target) = &app.workspace.delete {
        let message = match target {
            crate::project_app::WorkspaceDelete::Project { name, .. } => {
                format!("Delete project {name} and its command definitions?")
            }
            crate::project_app::WorkspaceDelete::Command { name, .. } => {
                format!("Delete command {name}?")
            }
            crate::project_app::WorkspaceDelete::Group { name, .. } => {
                format!("Remove group {name}? Its commands will become ungrouped.")
            }
            crate::project_app::WorkspaceDelete::Link { name, .. } => {
                format!("Delete web link {name}?")
            }
        };
        let area = centered_rect(72, 7, frame.area());
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(format!("{message}\n\ny / Enter confirm    n / Esc cancel"))
                .block(panel(" DELETE "))
                .style(Style::default().fg(TEXT))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

fn render_tunnels(frame: &mut Frame, area: Rect, app: &App) {
    let columns =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).split(area);
    let list_block = Block::default()
        .title(" PORT FORWARDING ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL));
    let list_inner = list_block.inner(columns[0]);
    frame.render_widget(list_block, columns[0]);

    let mut rows = vec![ListItem::new(Line::from(Span::styled(
        "  + New forwarding    a add",
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
    )))];
    rows.extend(app.metadata.tunnels.iter().map(|tunnel| {
        let selected = app
            .selected_tunnel_definition()
            .is_some_and(|selected| selected.id == tunnel.id);
        let status = app
            .tunnel_statuses
            .get(&tunnel.id)
            .cloned()
            .unwrap_or(TunnelStatus::Stopped);
        let (status_label, status_color) = tunnel_status_display(&status);
        let route = match tunnel.kind {
            crate::metadata::TunnelKind::Dynamic => {
                format!("{}:{}  SOCKS", tunnel.bind_address, tunnel.local_port)
            }
            _ => format!(
                "{}:{}  ->  {}:{}",
                tunnel.bind_address,
                tunnel.local_port,
                tunnel.remote_host.as_deref().unwrap_or("-"),
                tunnel.remote_port.unwrap_or_default()
            ),
        };
        ListItem::new(Line::from(vec![
            Span::styled(
                if selected { " > " } else { "   " },
                Style::default().fg(if selected { GREEN } else { MUTED }),
            ),
            Span::styled(
                format!("{} ", tunnel_kind_label(tunnel.kind)),
                Style::default().fg(if selected { GREEN } else { MUTED }),
            ),
            Span::styled(
                format!("{status_label:<9}"),
                Style::default().fg(status_color),
            ),
            Span::styled(
                format!("{:<18}", tunnel.label),
                Style::default()
                    .fg(if selected { TEXT } else { MUTED })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(route, Style::default().fg(MUTED)),
        ]))
    }));
    frame.render_widget(List::new(rows), list_inner);

    let detail_block = Block::default()
        .title(" EDIT PORT FORWARDING ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    let Some(tunnel) = app.selected_tunnel_definition() else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No forwarding selected.",
                    Style::default().fg(TEXT),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press a to create a local forward.",
                    Style::default().fg(MUTED),
                )),
            ])
            .wrap(Wrap { trim: true }),
            detail_inner,
        );
        return;
    };
    let status = app
        .tunnel_statuses
        .get(&tunnel.id)
        .cloned()
        .unwrap_or(crate::app::TunnelStatus::Stopped);
    let (status_label, status_color) = tunnel_status_display(&status);
    let status_line = match &status {
        TunnelStatus::Failed(message) => format!("Status                {status_label}: {message}"),
        _ => format!("Status                {status_label}"),
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("●  {}", tunnel.label),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!(
            "SSH host             {}{}",
            tunnel.host_alias,
            if tunnel.custom_host { " (custom)" } else { "" }
        )),
        if let Some(proxy_jump) = app
            .hosts
            .hosts
            .iter()
            .find(|host| host.alias == tunnel.host_alias)
            .and_then(|host| host.proxy_jump.as_deref())
        {
            Line::from(format!("ProxyJump            {proxy_jump}"))
        } else {
            Line::from("ProxyJump            none")
        },
        Line::from(format!(
            "{}                 {}:{}",
            if tunnel.kind == crate::metadata::TunnelKind::Remote {
                "Remote bind"
            } else {
                "Local bind"
            },
            tunnel.bind_address,
            tunnel.local_port
        )),
        if tunnel.kind == crate::metadata::TunnelKind::Dynamic {
            Line::from("SOCKS destination    dynamic")
        } else {
            Line::from(format!(
                "{}           {}:{}",
                if tunnel.kind == crate::metadata::TunnelKind::Remote {
                    "Local destination"
                } else {
                    "Destination"
                },
                tunnel.remote_host.as_deref().unwrap_or("-"),
                tunnel.remote_port.unwrap_or_default()
            ))
        },
        Line::from(format!(
            "Mode                 {}",
            tunnel_kind_label(tunnel.kind)
        )),
        Line::from(format!(
            "Reconnect            {}",
            if tunnel.auto_reconnect {
                "enabled"
            } else {
                "manual"
            }
        )),
        Line::from(format!(
            "Timeout / KeepAlive  {}s / {}s",
            tunnel.connect_timeout_secs, tunnel.keepalive_interval_secs
        )),
        Line::from(format!("Lifecycle             {:?}", tunnel.lifecycle)),
        Line::from(Span::styled(status_line, Style::default().fg(status_color))),
        Line::from(""),
        Line::from(Span::styled(
            "s/Space start/stop   r restart   A auto-reconnect   e edit   d delete",
            Style::default().fg(MUTED),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        detail_inner,
    );
}

pub(crate) struct RuntimeLayout {
    pub local: Rect,
    pub hosts: Rect,
    pub target: Rect,
    pub filters: Rect,
    pub listeners: Rect,
    pub details: Rect,
}

pub(crate) fn runtime_layout(viewport: Rect) -> RuntimeLayout {
    let area = page_layout().split(viewport)[2];
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .split(area);
    let targets = Layout::horizontal([
        Constraint::Length(9),
        Constraint::Length(11),
        Constraint::Min(0),
    ])
    .split(sections[0]);
    let columns = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(sections[2]);
    RuntimeLayout {
        local: targets[0],
        hosts: targets[1],
        target: targets[2],
        filters: sections[1],
        listeners: columns[0],
        details: columns[1],
    }
}

pub(crate) fn runtime_filter_areas(area: Rect, app: &App) -> Vec<Rect> {
    let mut x = area.x;
    let mut y = area.y;
    crate::runtime::RuntimeFilter::ALL
        .iter()
        .map(|filter| {
            let count = app
                .runtime_entries
                .iter()
                .filter(|entry| filter.matches(entry))
                .count();
            let width = (filter.title().len() + count.to_string().len() + 3) as u16;
            if x > area.x && x.saturating_add(width) > area.right() && y + 1 < area.bottom() {
                x = area.x;
                y += 1;
            }
            let rect = Rect::new(x, y, width.min(area.right().saturating_sub(x)), 1);
            x = x.saturating_add(width + 1).min(area.right());
            rect
        })
        .collect()
}

fn render_runtime(frame: &mut Frame, app: &App) {
    let layout = runtime_layout(frame.area());
    let active = Style::default()
        .fg(GREEN)
        .bg(Color::Rgb(47, 78, 61))
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(MUTED);
    let local = app.runtime_target == crate::runtime::RuntimeTarget::Local;
    frame.render_widget(
        Paragraph::new(" Local ").style(if local { active } else { inactive }),
        layout.local,
    );
    frame.render_widget(
        Paragraph::new(" Hosts ▾ ").style(if local { inactive } else { active }),
        layout.hosts,
    );
    frame.render_widget(
        Paragraph::new(app.runtime_target.label()).style(Style::default().fg(TEXT)),
        layout.target,
    );
    for (filter, area) in crate::runtime::RuntimeFilter::ALL
        .iter()
        .zip(runtime_filter_areas(layout.filters, app))
    {
        let count = app
            .runtime_entries
            .iter()
            .filter(|entry| filter.matches(entry))
            .count();
        frame.render_widget(
            Paragraph::new(format!(" {} {count} ", filter.title())).style(
                if *filter == app.runtime_filter {
                    active
                } else {
                    inactive
                },
            ),
            area,
        );
    }
    let entries = app.visible_runtime_entries();
    let target = app.runtime_target.label();
    let title = if app.runtime_loading {
        format!(" RUNTIME / {target}  loading... ")
    } else {
        format!(" RUNTIME / {target} ")
    };
    let block = panel(&title);
    let inner = block.inner(layout.listeners);
    frame.render_widget(block, layout.listeners);

    if let Some(error) = &app.runtime_error {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Port inspection failed",
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                )),
                Line::from(error.as_str()),
                Line::from("Press r to retry, l for Local, or s to choose a Host."),
            ])
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: false }),
            inner,
        );
    } else if entries.is_empty() && !app.runtime_loading {
        frame.render_widget(
            Paragraph::new("No listeners in this category.")
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
            inner,
        );
    } else {
        let rows = entries.iter().enumerate().map(|(index, entry)| {
            let selected = index == app.selected_runtime;
            let pid = entry
                .pid
                .map_or_else(|| "-".to_string(), |pid| pid.to_string());
            let row_background = if selected {
                Color::Rgb(47, 78, 61)
            } else {
                PANEL
            };
            let row_style = Style::default().bg(row_background);
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        if selected { ">> " } else { "   " },
                        row_style.fg(if selected { GREEN } else { MUTED }),
                    ),
                    Span::styled(
                        format!(
                            "{:<5} {:<5} {}  {}",
                            entry.port,
                            entry.protocol,
                            entry.address,
                            entry.kind.label()
                        ),
                        row_style
                            .fg(if selected { GREEN } else { TEXT })
                            .add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                    Span::styled(
                        format!("  {}", entry.origin.label()),
                        row_style
                            .fg(runtime_origin_color(entry.origin))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "         {:<18} pid {:<8} {}",
                        entry.process,
                        pid,
                        entry.project.as_ref().map_or_else(
                            || entry.external_project.as_ref().map_or_else(
                                || entry.user.clone(),
                                |run| format!(
                                    "{} / {} (external)",
                                    run.project_name, run.command_name
                                )
                            ),
                            |owner| format!("{} / {}", owner.project_name, owner.command_name)
                        )
                    ),
                    row_style.fg(runtime_origin_color(entry.origin)),
                )),
            ])
            .style(row_style)
        });
        let mut state =
            ListState::default().with_offset(app.runtime_scroll_offset(frame.area().height));
        if !entries.is_empty() {
            state.select(Some(app.selected_runtime));
        }
        frame.render_stateful_widget(
            List::new(rows).block(panel(" LISTENERS ")),
            inner,
            &mut state,
        );
    }

    let detail = app.selected_runtime_entry();
    let detail_lines = detail.map_or_else(
        || {
            vec![Line::from(Span::styled(
                "Select a listener to inspect it.",
                Style::default().fg(MUTED),
            ))]
        },
        |entry| {
            let mut lines = vec![
                detail_line("Machine", &app.runtime_target.label()),
                detail_line("Protocol", &entry.protocol),
                detail_line("Address", &entry.address),
                detail_line("Port", &entry.port.to_string()),
                detail_line(
                    "PID",
                    &entry
                        .pid
                        .map_or_else(|| "Unavailable".to_string(), |pid| pid.to_string()),
                ),
                detail_line("Process", &entry.process),
                detail_line(
                    "User",
                    if entry.user.is_empty() {
                        "Unavailable"
                    } else {
                        &entry.user
                    },
                ),
                detail_line("Class", entry.kind.label()),
                Line::from(vec![
                    Span::styled("Origin              ", Style::default().fg(MUTED)),
                    Span::styled(
                        entry.origin.label(),
                        Style::default().fg(runtime_origin_color(entry.origin)),
                    ),
                ]),
                detail_line(
                    "Working dir",
                    entry.working_directory.as_deref().unwrap_or("Unavailable"),
                ),
                detail_line("Command", &entry.command),
                detail_line("Evidence", &entry.origin_reason),
            ];
            if let Some(owner) = &entry.project {
                lines.push(detail_line("Project", &owner.project_name));
                lines.push(detail_line("Project command", &owner.command_name));
            }
            if let Some(run) = &entry.external_project {
                lines.push(detail_line("Project", &run.project_name));
                lines.push(detail_line("Project command", &run.command_name));
                lines.push(detail_line("Ownership", "External"));
            }
            lines
        },
    );
    frame.render_widget(
        Paragraph::new(detail_lines)
            .block(panel(" PROCESS "))
            .wrap(Wrap { trim: false }),
        layout.details,
    );
}

fn render_runtime_host_picker(frame: &mut Frame, app: &App) {
    let area = centered_rect(76, 22, frame.area());
    frame.render_widget(Clear, area);
    let title = app.runtime_host_picker_group.as_ref().map_or_else(
        || " SELECT RUNTIME HOST ".to_string(),
        |group| format!(" SELECT HOST / {} ", group.title()),
    );
    let items = app.runtime_host_picker_items_for_ui();
    let rows = items.iter().map(|item| match item {
        crate::app::HostPickerItem::Group(group) => {
            ListItem::new(format!("> {}", group.title())).style(Style::default().fg(GREEN))
        }
        crate::app::HostPickerItem::Host(alias) => {
            ListItem::new(format!("  {alias}")).style(Style::default().fg(TEXT))
        }
    });
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.runtime_host_picker_selection.min(items.len() - 1)));
    }
    frame.render_stateful_widget(
        List::new(rows)
            .block(panel(&title))
            .highlight_symbol("> ")
            .highlight_style(
                Style::default()
                    .fg(TEXT)
                    .bg(Color::Rgb(34, 54, 47))
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

fn render_runtime_kill_confirmation(
    frame: &mut Frame,
    entry: &crate::runtime::RuntimeEntry,
    target: &crate::runtime::RuntimeTarget,
    force: bool,
) {
    let area = centered_rect(76, 12, frame.area());
    frame.render_widget(Clear, area);
    let pid = entry
        .pid
        .map_or_else(|| "?".to_string(), |pid| pid.to_string());
    let block = panel(if force {
        " FORCE KILL PROCESS "
    } else {
        " TERMINATE PROCESS "
    });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                match target {
                    crate::runtime::RuntimeTarget::Local => "Machine: Local".to_string(),
                    crate::runtime::RuntimeTarget::Host(alias) => {
                        format!("Machine: Host / {alias}")
                    }
                },
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(if force {
                format!(
                    "Send KILL to {} (PID {pid}) listening on {}:{}?",
                    entry.process, entry.address, entry.port
                )
            } else {
                entry.project.as_ref().map_or_else(
                    || {
                        if let Some(run) = &entry.external_project {
                            return format!(
                                "Stop external {} / {} (PID {}) and its child processes?",
                                run.project_name, run.command_name, run.root.pid
                            );
                        }
                        format!(
                            "Send TERM to {} (PID {pid}) listening on {}:{}?",
                            entry.process, entry.address, entry.port
                        )
                    },
                    |owner| {
                        format!(
                            "Stop {} / {} and all its child processes?",
                            owner.project_name, owner.command_name
                        )
                    },
                )
            }),
            Line::from(Span::styled(
                "y / Enter confirm    n / Esc cancel",
                Style::default().fg(MUTED),
            )),
        ])
        .style(Style::default().fg(TEXT))
        .wrap(Wrap { trim: false }),
        inner,
    );
}

fn tunnel_status_display(status: &TunnelStatus) -> (&'static str, Color) {
    match status {
        TunnelStatus::Stopped => ("Stopped", MUTED),
        TunnelStatus::Starting => ("Starting", Color::Yellow),
        TunnelStatus::Running => ("Running", GREEN),
        TunnelStatus::Failed(_) => ("Failed", Color::Red),
    }
}

fn tunnel_kind_label(kind: crate::metadata::TunnelKind) -> &'static str {
    match kind {
        crate::metadata::TunnelKind::Local => "L",
        crate::metadata::TunnelKind::Remote => "R",
        crate::metadata::TunnelKind::Dynamic => "D",
    }
}

fn render_files(frame: &mut Frame, area: Rect, app: &App) {
    let task_height = app.transfer_panel_height(area.height);
    let sections = if task_height == 0 {
        Layout::vertical([Constraint::Min(1)]).split(area)
    } else {
        Layout::vertical([Constraint::Min(6), Constraint::Length(task_height)]).split(area)
    };
    let panes = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[0]);
    render_local_files(frame, panes[0], app);
    if app.host_picker_open {
        render_host_picker(frame, panes[1], app);
    } else {
        render_remote_files(frame, panes[1], app);
    }
    if task_height > 0 {
        render_transfer_tasks(frame, sections[1], app);
    }
}

fn render_transfer_tasks(frame: &mut Frame, area: Rect, app: &App) {
    let tasks = app.transfer_tasks.iter().rev().take(4).collect::<Vec<_>>();
    let task_area = Block::default()
        .title(" TRANSFERS ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL));
    let inner = task_area.inner(area);
    frame.render_widget(task_area, area);
    for (index, task) in tasks.into_iter().enumerate() {
        let direction = match task.direction {
            crate::sftp::TransferDirection::Upload => "↑",
            crate::sftp::TransferDirection::Download => "↓",
        };
        let ratio = if task.total > 0 {
            (task.transferred as f64 / task.total as f64).clamp(0.0, 1.0)
        } else if task.status == TransferStatus::Completed {
            1.0
        } else {
            0.0
        };
        let label = match &task.status {
            TransferStatus::Queued => format!("{direction} {}  queued", task.name),
            TransferStatus::Running if task.total > 0 => {
                format!("{direction} {}  {:.0}%", task.name, ratio * 100.0)
            }
            TransferStatus::Running => {
                format!(
                    "{direction} {}  {}",
                    task.name,
                    format_bytes(task.transferred)
                )
            }
            TransferStatus::Completed => format!("{direction} {}  100% done", task.name),
            TransferStatus::Cancelled => format!("{direction} {}  cancelled", task.name),
            TransferStatus::Failed(message) => {
                format!("{direction} {}  failed: {message}", task.name)
            }
        };
        let selected = app
            .selected_transfer_task_id()
            .is_some_and(|task_id| task_id == task.id);
        let gauge = Gauge::default()
            .gauge_style(
                Style::default().fg(if task.status == TransferStatus::Completed {
                    GREEN
                } else {
                    Color::Rgb(93, 159, 122)
                }),
            )
            .label(if selected {
                format!("> {label}")
            } else {
                label
            })
            .ratio(if task.status == TransferStatus::Queued {
                0.0
            } else {
                ratio
            });
        if let Some(row) = inner.rows().nth(index) {
            frame.render_widget(gauge, row);
        }
    }
}

fn render_local_files(frame: &mut Frame, area: Rect, app: &App) {
    let active = app.active_file_pane == FilePane::Local;
    let title = if active {
        format!(
            " LOCAL  [FOCUS]  hidden:{} ",
            if app.show_hidden_files { "on" } else { "off" }
        )
    } else {
        format!(
            " LOCAL  hidden:{} ",
            if app.show_hidden_files { "on" } else { "off" }
        )
    };
    let block = file_panel(&title, active);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let filter_visible = app.file_filtering || !app.file_filter.is_empty();
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(filter_visible)),
        Constraint::Min(1),
    ])
    .split(inner);
    let breadcrumbs = app.local_breadcrumb_parts(rows[0].width);
    let mut path_line = Vec::new();
    for (index, breadcrumb) in breadcrumbs.iter().enumerate() {
        if index > 0 {
            path_line.push(Span::styled(
                LOCAL_BREADCRUMB_SEPARATOR,
                Style::default().fg(MUTED),
            ));
        }
        path_line.push(Span::styled(
            breadcrumb.label.as_str(),
            Style::default()
                .fg(if active {
                    if index + 1 == breadcrumbs.len() {
                        TEXT
                    } else {
                        GREEN
                    }
                } else {
                    MUTED
                })
                .add_modifier(if index + 1 == breadcrumbs.len() {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(path_line))
            .style(Style::default().fg(TEXT))
            .scroll((0, 0)),
        rows[0],
    );

    if filter_visible {
        let filter_text = if app.file_filtering {
            format!("Filter: {}_", app.file_filter)
        } else {
            format!("Filter: {}", app.file_filter)
        };
        frame.render_widget(
            Paragraph::new(filter_text).style(Style::default().fg(if active {
                if app.file_filter.is_empty() {
                    MUTED
                } else {
                    GREEN
                }
            } else {
                DISABLED
            })),
            rows[1],
        );
    }

    if let Some(error) = &app.local_files.error {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Could not read this directory.",
                    Style::default().fg(if active { TEXT } else { MUTED }),
                )),
                Line::from(Span::styled(error.as_str(), Style::default().fg(MUTED))),
                Line::from(""),
                Line::from(Span::styled(
                    "Press r to retry.",
                    Style::default().fg(GREEN),
                )),
            ])
            .wrap(Wrap { trim: true }),
            rows[2],
        );
        return;
    }

    let visible_entries = app.visible_local_entries();
    let items = visible_entries.iter().map(|entry| {
        let (name, kind) = match entry.kind {
            LocalEntryKind::Parent => ("..".to_string(), "parent"),
            LocalEntryKind::Directory => (format!("{}/", entry.name), "folder"),
            LocalEntryKind::File => (entry.name.clone(), "file"),
            LocalEntryKind::Other => (entry.name.clone(), "other"),
        };
        let size = entry
            .size
            .map_or_else(String::new, |size| format!("  {}", format_bytes(size)));
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("  {name}"),
                Style::default().fg(if active && entry.kind == LocalEntryKind::Directory {
                    GREEN
                } else if active {
                    TEXT
                } else {
                    MUTED
                }),
            ),
            Span::styled(
                format!("  {kind}{size}"),
                Style::default().fg(if active { MUTED } else { DISABLED }),
            ),
        ]))
    });
    let mut state = ListState::default();
    if !visible_entries.is_empty() {
        state.select(Some(
            app.local_files.selected.min(visible_entries.len() - 1),
        ));
    }
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("> ")
            .highlight_style(file_selection_style(active)),
        rows[2],
        &mut state,
    );
    if visible_entries.is_empty() && !app.file_filter.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching local files or directories.")
                .style(Style::default().fg(MUTED)),
            rows[2],
        );
    }
}

fn render_remote_files(frame: &mut Frame, area: Rect, app: &App) {
    let active = app.active_file_pane == FilePane::Remote;
    let Some(host) = app.context_host() else {
        let block = file_panel(" REMOTE  SELECT HOST ", active);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let content = Layout::vertical([
            Constraint::Percentage(30),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new("[ SFTP ]")
                .alignment(Alignment::Center)
                .style(
                    Style::default()
                        .fg(if active { GREEN } else { MUTED })
                        .add_modifier(Modifier::BOLD),
                ),
            content[0],
        );
        frame.render_widget(
            Paragraph::new("Connect to host")
                .alignment(Alignment::Center)
                .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
            content[1],
        );
        frame.render_widget(
            Paragraph::new("Choose a saved SSH host to manage its files.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
            content[2],
        );
        let select_label = if area.width < 36 {
            "[Enter] Select host"
        } else {
            "[Enter / s]  Select host"
        };
        frame.render_widget(
            Paragraph::new(select_label)
                .alignment(Alignment::Center)
                .style(
                    Style::default()
                        .fg(if active { GREEN } else { MUTED })
                        .add_modifier(Modifier::BOLD),
                ),
            content[3],
        );
        return;
    };

    match &app.files {
        FilesState::Idle => {
            let title = format!(" REMOTE  {}  ~ ", host.alias);
            let block = file_panel(&title, active);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        "Remote host selected.",
                        Style::default()
                            .fg(if active { TEXT } else { MUTED })
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        "Press Enter to load its home directory.",
                        Style::default().fg(if active { MUTED } else { DISABLED }),
                    )),
                ])
                .alignment(Alignment::Center),
                inner,
            );
        }
        FilesState::Loading { host_alias, path } => {
            render_remote_status(
                frame,
                area,
                active,
                &format!(
                    " REMOTE  {}  {} ",
                    host_alias,
                    if path.is_empty() { "~" } else { path }
                ),
                "Connecting to remote directory...",
                "Please wait",
            );
        }
        FilesState::Failed {
            host_alias,
            path,
            message,
        } => {
            render_remote_status(
                frame,
                area,
                active,
                &format!(
                    " REMOTE  {}  {} ",
                    host_alias,
                    if path.is_empty() { "~" } else { path }
                ),
                message,
                "Enter retry  s choose another host",
            );
        }
        FilesState::Loaded(listing) => {
            let title = format!(
                " REMOTE  {}  {}  hidden:{} ",
                listing.host_alias,
                if listing.path.is_empty() {
                    "~"
                } else {
                    listing.path.as_str()
                },
                if app.show_hidden_files { "on" } else { "off" }
            );
            let block = file_panel(&title, active);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let entries = app.visible_remote_entries();
            let filter_visible = app.file_filtering || !app.file_filter.is_empty();
            let rows = Layout::vertical([
                Constraint::Length(u16::from(filter_visible)),
                Constraint::Min(1),
            ])
            .split(inner);
            if filter_visible {
                let filter_text = if app.file_filtering {
                    format!("Filter: {}_", app.file_filter)
                } else {
                    format!("Filter: {}", app.file_filter)
                };
                frame.render_widget(
                    Paragraph::new(filter_text).style(Style::default().fg(if active {
                        if app.file_filter.is_empty() {
                            MUTED
                        } else {
                            GREEN
                        }
                    } else {
                        DISABLED
                    })),
                    rows[0],
                );
            }
            if entries.is_empty() {
                frame.render_widget(
                    Paragraph::new(if app.file_filter.is_empty() {
                        "This directory is empty."
                    } else {
                        "No matching remote files or directories."
                    })
                    .style(Style::default().fg(MUTED)),
                    rows[1],
                );
                return;
            }
            let items = entries.iter().map(|entry| {
                let marker = if entry.kind == SftpEntryKind::Directory {
                    "/"
                } else {
                    ""
                };
                let size = entry
                    .size
                    .map_or_else(String::new, |size| format!("  {}", format_bytes(size)));
                let metadata = format_remote_metadata(entry);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("  {}{}", entry.name, marker),
                        Style::default().fg(if active && entry.kind == SftpEntryKind::Directory {
                            GREEN
                        } else if active {
                            TEXT
                        } else {
                            MUTED
                        }),
                    ),
                    Span::styled(
                        format!("{size}{metadata}"),
                        Style::default().fg(if active { MUTED } else { DISABLED }),
                    ),
                ]))
            });
            let mut state = ListState::default();
            state.select(Some(app.selected_file.min(entries.len() - 1)));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_symbol("> ")
                    .highlight_style(file_selection_style(active)),
                rows[1],
                &mut state,
            );
        }
    }
}

fn format_remote_metadata(entry: &crate::sftp::SftpEntry) -> String {
    let mut parts = Vec::new();
    if let Some(mode) = entry.permissions {
        parts.push(format!("{mode:04o}"));
    }
    if let (Some(uid), Some(gid)) = (entry.uid, entry.gid) {
        parts.push(format!("{uid}:{gid}"));
    }
    if let Some(modified) = entry.modified {
        parts.push(format!("mtime:{}", format_unix_timestamp(modified)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" "))
    }
}

fn format_unix_timestamp(timestamp: u64) -> String {
    const SECONDS_PER_MINUTE: u64 = 60;
    const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
    const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

    let days = timestamp / SECONDS_PER_DAY;
    let seconds = timestamp % SECONDS_PER_DAY;
    let hour = seconds / SECONDS_PER_HOUR;
    let minute = (seconds % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE;
    let (year, month, day) = civil_date_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

// Howard Hinnant's proleptic Gregorian calendar conversion, with Unix epoch
// days as input.
fn civil_date_from_days(days_since_epoch: i64) -> (i64, u8, u8) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year, month as u8, day as u8)
}

fn render_remote_status(
    frame: &mut Frame,
    area: Rect,
    active: bool,
    title: &str,
    message: &str,
    hint: &str,
) {
    let block = file_panel(title, active);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                message,
                Style::default().fg(if active { TEXT } else { MUTED }),
            )),
            Line::from(""),
            Line::from(Span::styled(
                hint,
                Style::default().fg(if active { GREEN } else { MUTED }),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_host_picker(frame: &mut Frame, area: Rect, app: &App) {
    let title = app
        .host_picker_group
        .as_ref()
        .map_or(" SELECT HOST  /  GROUPS + HOSTS ".to_string(), |group| {
            format!(" SELECT HOST  /  {} ", group.title())
        });
    let block = file_panel(&title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items = app.host_picker_items();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No SSH hosts found in ~/.ssh/config.")
                .style(Style::default().fg(MUTED)),
            inner,
        );
        return;
    }
    let group_count = items
        .iter()
        .take_while(|item| matches!(item, HostPickerItem::Group(_)))
        .count();
    let mut list_items = Vec::with_capacity(items.len() + 2);
    if group_count > 0 {
        list_items.push(ListItem::new(Line::from(Span::styled(
            " Groups",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))));
        list_items.extend(items[..group_count].iter().map(|item| {
            let HostPickerItem::Group(group) = item else {
                unreachable!("group section contains only groups")
            };
            ListItem::new(Line::from(vec![
                Span::styled("  [G] ", Style::default().fg(GREEN)),
                Span::styled(
                    group.title(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {} hosts", app.host_group_count(group)),
                    Style::default().fg(MUTED),
                ),
            ]))
        }));
    }
    list_items.push(ListItem::new(Line::from(Span::styled(
        if let Some(group) = &app.host_picker_group {
            format!(" Hosts / {}", group.title())
        } else {
            " Hosts".to_string()
        },
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
    ))));
    let host_items = &items[group_count..];
    list_items.extend(host_items.iter().map(|item| {
        let HostPickerItem::Host(alias) = item else {
            unreachable!("host section contains only hosts")
        };
        let target = app
            .hosts
            .hosts
            .iter()
            .find(|host| host.alias == *alias)
            .map_or(alias.as_str(), |host| host.destination());
        ListItem::new(Line::from(vec![
            Span::styled("  [H] ", Style::default().fg(MUTED)),
            Span::styled(alias, Style::default().fg(TEXT)),
            Span::styled(format!("  {target}"), Style::default().fg(MUTED)),
        ]))
    }));
    let selected_row = if group_count > 0 && app.host_picker_group.is_none() {
        if app.host_picker_selection < group_count {
            app.host_picker_selection + 1
        } else {
            app.host_picker_selection + 2
        }
    } else {
        app.host_picker_selection + 1
    };
    let mut state = ListState::default();
    state.select(Some(selected_row.min(list_items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(list_items)
            .highlight_symbol("> ")
            .highlight_style(file_selection_style(true)),
        inner,
        &mut state,
    );
}

fn file_panel(title: &str, active: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .title_style(Style::default().fg(if active { GREEN } else { MUTED }))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if active { GREEN } else { BORDER }))
        .style(Style::default().bg(PANEL))
}

fn file_selection_style(active: bool) -> Style {
    Style::default()
        .fg(if active { TEXT } else { MUTED })
        .bg(if active {
            Color::Rgb(34, 54, 47)
        } else {
            PANEL
        })
        .add_modifier(if active {
            Modifier::BOLD
        } else {
            Modifier::empty()
        })
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = "B";
    for next_unit in UNITS {
        value /= 1024.0;
        unit = next_unit;
        if value < 1024.0 || next_unit == "TB" {
            break;
        }
    }
    let formatted = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    let formatted = formatted.strip_suffix(".0").unwrap_or(&formatted);
    format!("{formatted} {unit}")
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let hosts = app.hosts.hosts.len();
    let content = Line::from(vec![
        Span::styled(
            " LAZYTERM ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  remote workspace", Style::default().fg(MUTED)),
        Span::styled(
            format!("    SSH CONFIG  {hosts}"),
            Style::default().fg(TEXT),
        ),
        Span::styled("    ~/.ssh/config", Style::default().fg(MUTED)),
        Span::styled("    [,] Settings", Style::default().fg(GREEN)),
    ]);
    frame.render_widget(Paragraph::new(content), area);
}

fn render_navigation(frame: &mut Frame, area: Rect, app: &App) {
    let visible = app.visible_sections();
    let titles = visible.iter().enumerate().map(|(index, section)| {
        let title = if area.width < 70 {
            format!("{} {}", index + 1, &section.title()[..1])
        } else if area.width < 100 {
            section.title().to_string()
        } else {
            format!("{} {}", index + 1, section.title())
        };
        Line::from(title)
    });
    let selected = visible
        .iter()
        .position(|section| *section == app.section)
        .unwrap_or(visible.len());
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .padding("", "")
            .divider(if area.width < 70 { " " } else { " | " })
            .style(Style::default().fg(MUTED))
            .highlight_style(
                Style::default()
                    .fg(GREEN)
                    .bg(Color::Rgb(34, 54, 47))
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
        area,
    );
}

fn render_settings(frame: &mut Frame, area: Rect, app: &App) {
    let rows = [
        ("Theme", app.metadata.theme.label().to_string()),
        ("Language", app.metadata.language.label().to_string()),
        (
            "Show Hosts tab",
            (!app.metadata.hidden_tabs.iter().any(|tab| tab == "Hosts")).to_string(),
        ),
        (
            "Show Files tab",
            (!app.metadata.hidden_tabs.iter().any(|tab| tab == "Files")).to_string(),
        ),
        (
            "Show Tunnels tab",
            (!app.metadata.hidden_tabs.iter().any(|tab| tab == "Tunnels")).to_string(),
        ),
        (
            "Show Snippets tab",
            (!app.metadata.hidden_tabs.iter().any(|tab| tab == "Snippets")).to_string(),
        ),
        (
            "Show Runtime tab",
            (!app.metadata.hidden_tabs.iter().any(|tab| tab == "Runtime")).to_string(),
        ),
        (
            "Show Projects tab",
            (!app.metadata.hidden_tabs.iter().any(|tab| tab == "Projects")).to_string(),
        ),
        (
            "Show Credentials tab",
            (!app
                .metadata
                .hidden_tabs
                .iter()
                .any(|tab| tab == "Credentials"))
            .to_string(),
        ),
        ("Export configuration", "Press Enter".into()),
        ("Export credentials", "Encrypted .age file".into()),
    ];
    let items = rows.iter().enumerate().map(|(index, (label, value))| {
        let selected = index == app.selected_setting;
        let style = if selected {
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT)
        };
        ListItem::new(Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, style),
            Span::styled(format!("{label:<24}"), style),
            Span::styled(
                value,
                Style::default().fg(if selected { GREEN } else { MUTED }),
            ),
        ]))
    });
    let mut state = ListState::default().with_selected(Some(app.selected_setting));
    frame.render_stateful_widget(
        List::new(items).block(panel(" SETTINGS ")),
        area,
        &mut state,
    );
}

fn render_hosts(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(area);
    let path = app.active_group.as_ref().map_or_else(
        || "All hosts".to_string(),
        |group| format!("All hosts  /  {}", group.title()),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                path,
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                if app.search_query.is_empty() {
                    String::new()
                } else {
                    format!(" Filter: {}", app.search_query)
                },
                Style::default().fg(MUTED),
            )),
        ]),
        rows[0],
    );
    let area = rows[1];
    if area.width < 70 {
        render_compact_hosts(frame, area, app);
        return;
    }
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(host_browser_width(area.width)),
            Constraint::Min(1),
        ])
        .split(area);
    render_host_browser(frame, panes[0], app);
    render_host_details(frame, panes[1], app);
}

fn render_host_browser(frame: &mut Frame, area: Rect, app: &App) {
    if app.showing_groups() && !app.visible_groups().is_empty() {
        let columns = group_columns(area.width);
        let total_rows = app.visible_groups().len().div_ceil(columns);
        let visible_rows =
            total_rows.min(usize::from(area.height.saturating_sub(8) / 4).clamp(1, 2));
        let height = (1 + visible_rows as u16 * 4).min(area.height.saturating_sub(4));
        let sections =
            Layout::vertical([Constraint::Length(height), Constraint::Min(3)]).split(area);
        render_group_cards(frame, sections[0], app);
        render_host_list(frame, sections[1], app);
    } else {
        render_host_list(frame, area, app);
    }
}

fn render_group_cards(frame: &mut Frame, area: Rect, app: &App) {
    let groups = app.visible_groups();
    let columns = group_columns(area.width);
    let total_rows = groups.len().div_ceil(columns);
    let visible_rows = usize::from(area.height.saturating_sub(1) / 4).max(1);
    let selected = app
        .selected_group()
        .and_then(|group| groups.iter().position(|item| *item == group));
    let first_row = selected
        .map_or(0, |index| {
            (index / columns).saturating_sub(visible_rows - 1)
        })
        .min(total_rows.saturating_sub(visible_rows));
    let heading = if total_rows > visible_rows {
        format!(
            " GROUPS  {}    {}/{}",
            groups.len(),
            first_row + 1,
            total_rows
        )
    } else {
        format!(" GROUPS  {}", groups.len())
    };
    frame.render_widget(
        Paragraph::new(heading).style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
        Rect::new(area.x, area.y, area.width, area.height.min(1)),
    );
    for row in 0..visible_rows {
        let y = area.y.saturating_add(1 + row as u16 * 4);
        if y.saturating_add(4) > area.bottom() {
            break;
        }
        let cells = Layout::horizontal(vec![Constraint::Ratio(1, columns as u32); columns])
            .spacing(1)
            .split(Rect::new(area.x, y, area.width, 4));
        for column in 0..columns {
            let index = (first_row + row) * columns + column;
            let Some(group) = groups.get(index) else {
                break;
            };
            let active = selected == Some(index);
            let count = app.group_hosts(group).len();
            let border = if active { GREEN } else { BORDER };
            let background = if active {
                Color::Rgb(34, 54, 47)
            } else {
                PANEL
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .style(Style::default().bg(background));
            let inner = block.inner(cells[column]);
            frame.render_widget(block, cells[column]);
            let rows =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
            frame.render_widget(
                Paragraph::new(group.title())
                    .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
                    .wrap(Wrap { trim: true }),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "{count} {}",
                    if count == 1 { "host" } else { "hosts" }
                ))
                .style(Style::default().fg(if active { GREEN } else { MUTED })),
                rows[1],
            );
        }
    }
}

fn render_compact_hosts(frame: &mut Frame, area: Rect, app: &App) {
    render_host_browser(frame, area, app);
}

fn render_host_list(frame: &mut Frame, area: Rect, app: &App) {
    let hosts = app.visible_hosts();
    let groups = app.visible_groups();
    let items = hosts
        .iter()
        .map(|host| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!("  {}", host.alias),
                    Style::default().fg(TEXT),
                )),
                Line::from(Span::styled(
                    format!("  {}", host.destination()),
                    Style::default().fg(MUTED),
                )),
            ])
        })
        .collect::<Vec<_>>();

    let title = if app.search_query.is_empty() {
        format!(" HOSTS  {} ", hosts.len())
    } else {
        format!(" HOSTS  {} matching ", hosts.len())
    };
    let mut state = ListState::default();
    let count = items.len();
    if count > 0 {
        if app.showing_groups() {
            if let Some(index) = app.selected_browse_item.checked_sub(groups.len()) {
                state.select(Some(index.min(count - 1)));
            }
        } else {
            state.select(Some(app.selected_browse_item.min(count - 1)));
        }
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(BORDER))
                    .style(Style::default().bg(PANEL)),
            )
            .highlight_symbol("> ")
            .highlight_style(
                Style::default()
                    .fg(TEXT)
                    .bg(Color::Rgb(34, 54, 47))
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
    if count == 0 {
        let message = if app.hosts.error.is_some() {
            "SSH config could not be read."
        } else if !app.search_query.is_empty() {
            "No matching hosts."
        } else if app.active_group.is_some() {
            "No hosts in this group."
        } else {
            "No Host aliases found in ~/.ssh/config."
        };
        let inner = Block::default().borders(Borders::ALL).inner(area);
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
            inner,
        );
    }
}

fn render_host_details(frame: &mut Frame, area: Rect, app: &App) {
    if app.showing_groups()
        && let Some(group) = app.selected_group()
    {
        let hosts = app.group_hosts(&group);
        let rows = Layout::vertical([Constraint::Length(4), Constraint::Min(1)]).split(area);
        frame.render_widget(
            Paragraph::new(vec![
                detail_line("Group", group.title()),
                detail_line("Hosts", &hosts.len().to_string()),
            ])
            .block(panel(" GROUP ")),
            rows[0],
        );
        let items = hosts.iter().map(|host| {
            ListItem::new(vec![
                Line::from(Span::styled(host.alias.as_str(), Style::default().fg(TEXT))),
                Line::from(Span::styled(host.destination(), Style::default().fg(MUTED))),
            ])
        });
        frame.render_widget(List::new(items).block(panel(" HOSTS ")), rows[1]);
        return;
    }
    if let Some(host) = app.selected_host() {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Min(5),
            ])
            .split(area);

        let overview = vec![
            detail_line("Alias", &host.alias),
            detail_line("Target", host.destination()),
            detail_line("User / port", "Resolved by OpenSSH"),
            detail_line(
                "Auth",
                if app.host_password_reference(&host.alias).is_some() {
                    "Saved password / Keychain"
                } else {
                    "ssh-agent, key, or password prompt"
                },
            ),
            detail_line("Tags", &app.tags(host).join(", ")),
        ];
        frame.render_widget(
            Paragraph::new(overview)
                .block(panel(" HOST "))
                .wrap(Wrap { trim: true }),
            vertical[0],
        );

        let routing = vec![
            detail_line("Routing", "ProxyJump / ProxyCommand"),
            detail_line(
                "Identity",
                host.identity_file
                    .as_deref()
                    .unwrap_or("IdentityFile / ssh-agent"),
            ),
            detail_line("Credentials", "k select / manage    U upload public key"),
            detail_line("Source", "~/.ssh/config"),
        ];
        frame.render_widget(
            Paragraph::new(routing)
                .block(panel(" CONNECTION "))
                .wrap(Wrap { trim: true }),
            vertical[1],
        );

        let recent = app.notice.as_deref().unwrap_or("No session history yet.");
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(recent, Style::default().fg(MUTED))),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter connects using the system OpenSSH client.",
                    Style::default().fg(DISABLED),
                )),
            ])
            .block(panel(" RECENT SESSIONS "))
            .wrap(Wrap { trim: true }),
            vertical[2],
        );
    } else {
        let message = app.hosts.error.as_deref().unwrap_or({
            if app.search_query.is_empty() && app.hosts.hosts.is_empty() {
                "Add a Host alias to ~/.ssh/config, then press r to reload."
            } else {
                "No hosts match the current search."
            }
        });
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(MUTED))
                .block(panel(" HOST DETAILS "))
                .wrap(Wrap { trim: true }),
            area,
        );
    }
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<11}  "), Style::default().fg(MUTED)),
        Span::styled(value.to_string(), Style::default().fg(TEXT)),
    ])
}

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL))
}

fn render_snippets(frame: &mut Frame, area: Rect, app: &App) {
    if app.snippet_execution_view {
        render_snippet_execution(frame, area, app);
        return;
    }
    let block = panel(" SNIPPETS ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "Packages",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   →   ", Style::default().fg(MUTED)),
        Span::styled(
            if app.snippet_package_open {
                app.metadata
                    .snippet_packages
                    .get(app.selected_package)
                    .map_or("Package", |package| package.name.as_str())
            } else {
                "Select a package"
            },
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ),
    ]));
    frame.render_widget(title, Rect { height: 1, ..inner });
    let body = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    let sections = Layout::vertical([Constraint::Min(6), Constraint::Length(7)]).split(body);
    let columns = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(sections[0]);

    let package_rows = app
        .metadata
        .snippet_packages
        .iter()
        .enumerate()
        .map(|(index, package)| {
            let selected = !app.snippet_package_open && index == app.selected_package;
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!("▣  {}", package.name),
                    Style::default()
                        .fg(if selected { GREEN } else { TEXT })
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                )),
                Line::from(Span::styled(
                    format!(
                        "   {} snippet{}",
                        package.snippets.len(),
                        if package.snippets.len() == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(MUTED),
                )),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(package_rows).block(panel(" PACKAGES ")),
        columns[0],
    );

    let snippet_rows = app
        .metadata
        .snippet_packages
        .get(app.selected_package)
        .map(|package| {
            package
                .snippets
                .iter()
                .enumerate()
                .map(|(index, snippet)| {
                    let selected = app.snippet_package_open && index == app.selected_snippet;
                    let marker = if selected { ">" } else { " " };
                    ListItem::new(vec![
                        Line::from(Span::styled(
                            format!("{marker}  {{}}  {} {}", snippet.name, snippet.command),
                            Style::default()
                                .fg(if selected { GREEN } else { TEXT })
                                .add_modifier(if selected {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                }),
                        )),
                        Line::from(Span::styled(
                            if snippet.description.is_empty() {
                                "     No description".to_string()
                            } else {
                                format!("     {}", snippet.description)
                            },
                            Style::default().fg(MUTED),
                        )),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    frame.render_widget(
        List::new(snippet_rows).block(panel(" SNIPPETS IN PACKAGE ")),
        columns[1],
    );
    render_snippet_runs(frame, sections[1], app);
}

fn render_snippet_execution(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel(" SNIPPET EXECUTION ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(run) = app.snippet_runs.get(app.selected_snippet_run) else {
        frame.render_widget(
            Paragraph::new("No execution selected.").style(Style::default().fg(MUTED)),
            inner,
        );
        return;
    };
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Snippet: ", Style::default().fg(MUTED)),
            Span::styled(
                run.snippet_name.clone(),
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Command: ", Style::default().fg(MUTED)),
            Span::styled(run.command.clone(), Style::default().fg(TEXT)),
        ]),
    ]);
    frame.render_widget(header, Rect { height: 2, ..inner });
    let body = Rect {
        y: inner.y + 3,
        height: inner.height.saturating_sub(3),
        ..inner
    };
    let columns =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).split(body);
    let target_rows = app.snippet_runs.iter().enumerate().map(|(index, item)| {
        let selected = index == app.selected_snippet_run;
        let status = match &item.status {
            crate::app::SnippetRunStatus::Queued => "queued".to_string(),
            crate::app::SnippetRunStatus::Running => "running".to_string(),
            crate::app::SnippetRunStatus::Completed(code) => format!("exit {code}"),
            crate::app::SnippetRunStatus::ConnectionFailed(error) => {
                format!("connection: {error}")
            }
            crate::app::SnippetRunStatus::Cancelled => "cancelled".to_string(),
        };
        ListItem::new(vec![
            Line::from(Span::styled(
                format!("{} {}", if selected { ">" } else { " " }, item.host_alias),
                Style::default()
                    .fg(if selected { GREEN } else { TEXT })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )),
            Line::from(Span::styled(
                format!("  {status}"),
                Style::default().fg(MUTED),
            )),
        ])
    });
    frame.render_widget(
        List::new(target_rows).block(panel(" TARGET HOSTS ")),
        columns[0],
    );
    let output = if run.output.is_empty() {
        match &run.status {
            crate::app::SnippetRunStatus::Running => "Waiting for output...".to_string(),
            crate::app::SnippetRunStatus::Queued => "Queued...".to_string(),
            _ => "No output.".to_string(),
        }
    } else {
        run.output.clone()
    };
    frame.render_widget(
        Paragraph::new(output)
            .scroll((app.snippet_output_scroll, 0))
            .wrap(Wrap { trim: false })
            .block(panel(" OUTPUT ")),
        columns[1],
    );
}

fn render_snippet_runs(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.snippet_runs.iter().enumerate().map(|(index, run)| {
        let marker = if index == app.selected_snippet_run {
            ">"
        } else {
            " "
        };
        let status = match &run.status {
            crate::app::SnippetRunStatus::Queued => "queued".to_string(),
            crate::app::SnippetRunStatus::Running => "running".to_string(),
            crate::app::SnippetRunStatus::Completed(code) => format!("exit {code}"),
            crate::app::SnippetRunStatus::ConnectionFailed(error) => {
                format!("connection: {error}")
            }
            crate::app::SnippetRunStatus::Cancelled => "cancelled".to_string(),
        };
        let output = run.output.lines().next().unwrap_or("");
        ListItem::new(Line::from(vec![
            Span::styled(
                format!("{marker} {:<18} {:<10} ", run.host_alias, status),
                Style::default().fg(if index == app.selected_snippet_run {
                    GREEN
                } else {
                    TEXT
                }),
            ),
            Span::styled(output, Style::default().fg(MUTED)),
        ]))
    });
    frame.render_widget(List::new(rows).block(panel(" RUNS ")), area);
}

fn render_snippet_dialog(frame: &mut Frame, dialog: &crate::app::SnippetDialog, app: &App) {
    let area = centered_rect(82, 16, frame.area());
    frame.render_widget(Clear, area);
    let block = panel(if dialog.editing_id.is_some() {
        " EDIT SNIPPET "
    } else {
        " NEW SNIPPET "
    });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let labels = ["Name", "Command", "Description", "Dangerous (y/n)"];
    let rows = Layout::vertical([Constraint::Length(1); 4]).split(inner);
    for (index, (label, value)) in labels.iter().zip(dialog.fields.iter()).enumerate() {
        let cursor = if index == dialog.selected_field {
            "_"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{label:<18}"), Style::default().fg(MUTED)),
                Span::styled(format!("{value}{cursor}"), Style::default().fg(TEXT)),
            ])),
            rows[index],
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "Package: {}   Tab / ↑↓ next field   Enter next / save   Esc cancel",
            app.metadata
                .snippet_packages
                .iter()
                .find(|package| package.id == dialog.package_id)
                .map_or("Unknown", |package| package.name.as_str())
        ))
        .style(Style::default().fg(MUTED))
        .wrap(Wrap { trim: true }),
        Rect {
            y: inner.y + 6,
            height: inner.height.saturating_sub(6),
            ..inner
        },
    );
}

fn render_package_dialog(frame: &mut Frame, dialog: &crate::app::PackageDialog) {
    let area = centered_rect(72, 11, frame.area());
    frame.render_widget(Clear, area);
    let block = panel(if dialog.editing_id.is_some() {
        " EDIT PACKAGE "
    } else {
        " NEW PACKAGE "
    });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let labels = ["Name", "Description"];
    let rows = Layout::vertical([Constraint::Length(1); 2]).split(inner);
    for (index, (label, value)) in labels.iter().zip(dialog.fields.iter()).enumerate() {
        let cursor = if index == dialog.selected_field {
            "_"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{label:<14}"), Style::default().fg(MUTED)),
                Span::styled(format!("{value}{cursor}"), Style::default().fg(TEXT)),
            ])),
            rows[index],
        );
    }
    frame.render_widget(
        Paragraph::new("Tab / ↑↓ next field    Enter next / save    Esc cancel")
            .style(Style::default().fg(MUTED)),
        Rect {
            y: inner.y + 4,
            height: inner.height.saturating_sub(4),
            ..inner
        },
    );
}

fn render_snippet_confirmation(frame: &mut Frame, pending: &crate::app::PendingSnippetRun) {
    let area = centered_rect(76, 9, frame.area());
    frame.render_widget(Clear, area);
    let block = panel(" CONFIRM DANGEROUS COMMAND ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "Run {} on {} host(s)?",
                pending.snippet_name,
                pending.targets.len()
            )),
            Line::from(pending.command.clone()),
            Line::from(Span::styled(
                "y / Enter confirm    n / Esc cancel",
                Style::default().fg(MUTED),
            )),
        ])
        .style(Style::default().fg(TEXT))
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_snippet_delete_confirmation(frame: &mut Frame, target: &crate::app::SnippetDeleteTarget) {
    let area = centered_rect(76, 9, frame.area());
    frame.render_widget(Clear, area);
    let (title, message) = match target {
        crate::app::SnippetDeleteTarget::Package { name, .. } => (
            " DELETE PACKAGE ",
            format!("Delete package \"{name}\" and all snippets inside it?"),
        ),
        crate::app::SnippetDeleteTarget::Snippet { name, .. } => {
            (" DELETE SNIPPET ", format!("Delete snippet \"{name}\"?"))
        }
    };
    let block = panel(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(message),
            Line::from(Span::styled(
                "y / Enter confirm    n / Esc cancel",
                Style::default().fg(MUTED),
            )),
        ])
        .style(Style::default().fg(TEXT)),
        inner,
    );
}

fn render_snippet_host_picker(frame: &mut Frame, app: &App) {
    let area = centered_rect(82, 22, frame.area());
    frame.render_widget(Clear, area);
    let title = app.snippet_host_picker_group.as_ref().map_or_else(
        || " EXECUTE SNIPPET ".to_string(),
        |group| match group {
            crate::app::HostGroup::Tag(tag) => format!(" EXECUTE ON GROUP / {tag} "),
            crate::app::HostGroup::Untagged => " EXECUTE ON GROUP / UNTAGGED ".to_string(),
        },
    );
    let block = panel(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(snippet) = app
        .metadata
        .snippet_packages
        .get(app.selected_package)
        .and_then(|package| package.snippets.get(app.selected_snippet))
    {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    snippet.name.clone(),
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("$ {}", snippet.command),
                    Style::default().fg(MUTED),
                )),
            ]),
            Rect { height: 2, ..inner },
        );
    }
    let list_area = Rect {
        y: inner.y + 3,
        height: inner.height.saturating_sub(3),
        ..inner
    };
    let items = if let Some(group) = &app.snippet_host_picker_group {
        app.hosts
            .hosts
            .iter()
            .filter(|host| match group {
                crate::app::HostGroup::Tag(tag) => app.tags(host).iter().any(|item| item == tag),
                crate::app::HostGroup::Untagged => app.tags(host).is_empty(),
            })
            .map(|host| crate::app::HostPickerItem::Host(host.alias.clone()))
            .collect::<Vec<_>>()
    } else {
        app.visible_groups()
            .into_iter()
            .map(crate::app::HostPickerItem::Group)
            .chain(
                app.hosts
                    .hosts
                    .iter()
                    .map(|host| crate::app::HostPickerItem::Host(host.alias.clone())),
            )
            .collect::<Vec<_>>()
    };
    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let selected = index == app.snippet_host_picker_selection;
            let label = match item {
                crate::app::HostPickerItem::Group(group) => format!("▣  {}", group.title()),
                crate::app::HostPickerItem::Host(alias) => format!("   {alias}"),
            };
            ListItem::new(Span::styled(
                label,
                Style::default()
                    .fg(if selected { GREEN } else { TEXT })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(rows)
            .block(panel(" TARGETS "))
            .highlight_style(Style::default().fg(GREEN)),
        list_area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let navigation = if area.width < 75 {
        "1-7 tabs  Tab next  Shift+Tab prev  ` last"
    } else {
        "1-7 / F1-F7 tabs  Tab / Shift+Tab cycle  Ctrl+Left/Right cycle  ` last  [,] settings"
    };
    let hint = if app
        .workspace
        .form
        .as_ref()
        .is_some_and(|form| form.link_form)
    {
        "Tab / ↑↓ field  ←→ choice  Enter save  Esc cancel"
    } else if app.workspace.form.is_some() {
        "Tab / ↑↓ field  ←→ choice  Enter next/save  Esc cancel"
    } else if app.workspace.delete.is_some() || app.workspace.confirmation.is_some() {
        "y / Enter confirm  n / Esc cancel"
    } else if app.section == NavSection::Projects {
        if app.workspace.output_open {
            "↑↓ scroll  Home/End  [ ] command  s start  x stop  r restart  Esc back  q quit"
        } else if app.workspace.project_open {
            if app.project_links_header_selected() {
                "↑↓ select  ←→/Space fold  Enter fold  L add link  v view  Esc back"
            } else if app.selected_project_link().is_some() {
                "↑↓ select  Enter browser  u username  p password  L add link  e edit  d delete  Esc back"
            } else {
                "↑↓ select  ←→/Space fold  Enter open  a command  L link  G group  e edit  d delete  v view  s/x/r  A/X services"
            }
        } else {
            "↑↓ select  Enter open  a add  e edit  d delete  A/X services  q quit"
        }
    } else if app.snippet_execution_view {
        "↑↓ scroll output  [ ] select Host  c cancel  C clear  Esc back  q quit"
    } else if app.snippet_host_picker_open {
        "↑↓ select group/Host  Enter run one  b run group  Backspace back  Esc cancel"
    } else if app.snippet_confirmation.is_some() || app.snippet_delete_confirmation.is_some() {
        "y / Enter confirm    n / Esc cancel"
    } else if app.snippet_dialog.is_some() || app.package_dialog.is_some() {
        "Tab / ↑↓ field  Enter next/save  Esc cancel"
    } else if app.tunnel_host_picker_open {
        "↑↓ select group/Host  Enter open/choose  Backspace back  Esc cancel"
    } else if app.tunnel_dialog.is_some() {
        if app
            .tunnel_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.deleting)
        {
            "y / Enter confirm  n / Esc cancel"
        } else {
            "F1 Local  F2 Remote  F3 Dynamic  Tab / ↑↓ field  Enter next/save  Esc cancel"
        }
    } else if app.host_dialog.is_some() {
        "Tab / Shift+Tab field  Enter next/save  Esc cancel"
    } else if app.tags_dialog.is_some() {
        "Enter save tags  Esc cancel"
    } else if let Some(dialog) = &app.file_dialog {
        match dialog {
            FileDialog::Rename { .. } => "Type new name  Enter confirm  Esc cancel",
            FileDialog::Delete { .. } => "y / Enter confirm  n / Esc cancel",
            FileDialog::CreateDir { .. } => "Type directory name  Enter confirm  Esc cancel",
            FileDialog::TransferConflict { .. } => "y / Enter overwrite  n / Esc cancel",
        }
    } else if app.searching {
        "Type to filter   Enter apply   Esc clear"
    } else if app.file_filtering {
        "Type to filter current pane   ↑↓ move   ← parent   → open   Enter apply   Esc clear"
    } else if app.section == NavSection::Hosts {
        if area.width < 75 && app.showing_groups() {
            "Arrows select  Enter open/SSH  / find  q quit"
        } else if area.width < 75 {
            "↑↓ select  Enter SSH  g tags  Left/Esc back"
        } else if app.showing_groups() {
            "Arrows select  Enter open/SSH  k set SSH key  U upload key  x unlink password  g tags  / find  a add  r reload  q quit"
        } else if area.width < 95 {
            "↑↓ move  Enter SSH  v view  g tags  / find  Esc back  q quit"
        } else {
            "↑↓ select  Enter SSH  k set SSH key  U upload key  x unlink password  g tags  Esc back  a add  e edit  d delete  / find  r reload  q quit"
        }
    } else if app.section == NavSection::Files {
        if app.host_picker_open {
            if area.width < 75 {
                "↑↓ select  Enter open  Backspace/Esc back"
            } else {
                "↑↓ select  Enter group/host  Backspace back  Esc close"
            }
        } else if area.width < 75 {
            match (app.active_file_pane, app.context_host().is_none()) {
                (FilePane::Local, _) => {
                    "n mkdir  m rename  x delete  c cancel  C cancel all  [ ] task  R retry  K clear  / filter  h hidden  ↑↓ select  ← parent  →/Enter open"
                }
                (FilePane::Remote, true) => "Tab local  Enter/s select host",
                (FilePane::Remote, false) => {
                    "n mkdir  m rename  x delete  c cancel  C cancel all  [ ] task  R retry  K clear  Tab local  h hidden  ↑↓ select  ← parent  →/Enter open"
                }
            }
        } else {
            match (app.active_file_pane, app.context_host().is_none()) {
                (FilePane::Local, _) => {
                    "n mkdir  m rename  x delete  c cancel  C cancel all  [ ] task  R retry  K clear  / filter  h hidden  Tab remote  ↑↓ select  ← parent  →/Enter open  u upload  r reload"
                }
                (FilePane::Remote, true) => "Tab local  Enter/s select host",
                (FilePane::Remote, false) => {
                    "n mkdir  m rename  x delete  c cancel  C cancel all  [ ] task  R retry  K clear  Tab local  h hidden  ↑↓ select  ← parent  →/Enter open  d download  s host  o native sftp"
                }
            }
        }
    } else if app.section == NavSection::Tunnels {
        "↑↓ select  l local  R remote  D dynamic  s/Space start/stop  r restart  e edit  d delete"
    } else if app.section == NavSection::Snippets {
        if app.snippet_package_open {
            "↑↓ select  Enter execute  e edit  a add  d delete  b group  Y copy  [ ] runs  c cancel  C clear"
        } else {
            "↑↓ select package  Enter open  e edit  a add  d delete"
        }
    } else if app.section == NavSection::Runtime {
        if app.runtime_host_picker_open {
            "↑↓ select group/Host  Enter choose  Backspace back  Esc cancel"
        } else if app.runtime_kill_confirmation.is_some()
            || app.runtime_force_kill_confirmation.is_some()
        {
            "y / Enter confirm  n / Esc cancel"
        } else {
            "l Local  h/s Hosts  Left/Right category  ↑↓ select  r refresh  k TERM  K KILL  o project  q quit"
        }
    } else if app.section == NavSection::Settings {
        "↑↓ select  Enter / Space change  Tab switch tab  q quit"
    } else if app.section == NavSection::Credentials {
        "↑↓ select  a password  g generate key  i import  Enter assign  u upload key  q quit"
    } else {
        "q quit"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(
                if app.tunnel_dialog.is_some()
                    || app.host_dialog.is_some()
                    || app.tags_dialog.is_some()
                    || app.file_dialog.is_some()
                    || app.snippet_dialog.is_some()
                    || app.package_dialog.is_some()
                    || app.snippet_confirmation.is_some()
                    || app.snippet_delete_confirmation.is_some()
                    || app.runtime_host_picker_open
                    || app.runtime_kill_confirmation.is_some()
                    || app.workspace.form.is_some()
                    || app.workspace.delete.is_some()
                    || app.workspace.confirmation.is_some()
                {
                    ""
                } else if app.searching {
                    "F1-F7 / Ctrl+Left/Right tabs  [,] settings"
                } else {
                    navigation
                },
            ),
            Line::from(hint),
            Line::from(
                app.notice
                    .as_deref()
                    .or(app.hosts.error.as_deref())
                    .unwrap_or(""),
            ),
        ])
        .style(Style::default().fg(MUTED)),
        area,
    );
}

fn render_file_dialog(frame: &mut Frame, dialog: &FileDialog) {
    match dialog {
        FileDialog::Rename {
            old_name, value, ..
        } => {
            let area = centered_rect(68, 7, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("Rename \"{old_name}\"")),
                    Line::from(format!("Name: {value}_")),
                    Line::from(Span::styled(
                        "Enter confirm   Esc cancel",
                        Style::default().fg(MUTED),
                    )),
                ])
                .style(Style::default().fg(TEXT))
                .block(panel(" RENAME FILE "))
                .wrap(Wrap { trim: true }),
                area,
            );
        }
        FileDialog::Delete { name, is_dir, .. } => {
            let area = centered_rect(68, 7, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("Delete \"{name}\"?")),
                    Line::from(if *is_dir {
                        "The directory must be empty."
                    } else {
                        "This action cannot be undone."
                    }),
                    Line::from(Span::styled(
                        "y / Enter confirm   n / Esc cancel",
                        Style::default().fg(MUTED),
                    )),
                ])
                .style(Style::default().fg(TEXT))
                .block(panel(if *is_dir {
                    " DELETE DIRECTORY "
                } else {
                    " DELETE FILE "
                }))
                .wrap(Wrap { trim: true }),
                area,
            );
        }
        FileDialog::CreateDir { value, .. } => {
            let area = centered_rect(68, 7, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("Name: {value}_")),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Enter confirm   Esc cancel",
                        Style::default().fg(MUTED),
                    )),
                ])
                .style(Style::default().fg(TEXT))
                .block(panel(" NEW DIRECTORY "))
                .wrap(Wrap { trim: true }),
                area,
            );
        }
        FileDialog::TransferConflict { request } => {
            let direction = match request.direction {
                crate::sftp::TransferDirection::Upload => "Upload",
                crate::sftp::TransferDirection::Download => "Download",
            };
            let target = match request.direction {
                crate::sftp::TransferDirection::Upload => request.remote_path.clone(),
                crate::sftp::TransferDirection::Download => {
                    request.local_path.to_string_lossy().into_owned()
                }
            };
            let area = centered_rect(72, 7, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(format!("{direction} will overwrite:")),
                    Line::from(target),
                    Line::from(Span::styled(
                        "y / Enter overwrite   n / Esc cancel",
                        Style::default().fg(MUTED),
                    )),
                ])
                .style(Style::default().fg(TEXT))
                .block(panel(" TRANSFER CONFLICT "))
                .wrap(Wrap { trim: true }),
                area,
            );
        }
    }
}

fn render_host_dialog(frame: &mut Frame, dialog: &crate::app::HostDialog) {
    let area = centered_rect(68, 15, frame.area());
    frame.render_widget(Clear, area);
    let block = panel(if dialog.deleting {
        " DELETE HOST "
    } else if dialog.editing {
        " EDIT HOST "
    } else {
        " NEW HOST "
    });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if dialog.deleting {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!(
                    "Delete LazyTerm-managed host \"{}\"?",
                    dialog.fields[0]
                )),
                Line::from(""),
                Line::from("y confirm    n / Esc cancel"),
            ])
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    let labels = ["Alias", "HostName", "User", "Port", "IdentityFile"];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1); 5])
        .split(inner);
    for (index, (label, value)) in labels.iter().zip(dialog.fields.iter()).enumerate() {
        let cursor = if index == dialog.selected_field {
            "_"
        } else {
            ""
        };
        let line = Line::from(vec![
            Span::styled(format!("{label:<13}"), Style::default().fg(MUTED)),
            Span::styled(format!("{value}{cursor}"), Style::default().fg(TEXT)),
        ]);
        frame.render_widget(Paragraph::new(line), rows[index]);
    }
    let help_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(6),
        width: inner.width,
        height: inner.height.saturating_sub(6),
    };
    frame.render_widget(
        Paragraph::new("Tab / ↑↓ next field    Enter next / save    Esc cancel")
            .style(Style::default().fg(MUTED)),
        help_area,
    );
}

fn render_tunnel_dialog(frame: &mut Frame, dialog: &crate::app::TunnelDialog) {
    let area = centered_rect(82, 22, frame.area());
    frame.render_widget(Clear, area);
    let block = panel(if dialog.deleting {
        " DELETE PORT FORWARDING "
    } else if dialog.editing_id.is_some() {
        match dialog.kind {
            crate::metadata::TunnelKind::Local => " EDIT LOCAL FORWARDING ",
            crate::metadata::TunnelKind::Remote => " EDIT REMOTE FORWARDING ",
            crate::metadata::TunnelKind::Dynamic => " EDIT DYNAMIC FORWARDING ",
        }
    } else {
        match dialog.kind {
            crate::metadata::TunnelKind::Local => " NEW LOCAL FORWARDING ",
            crate::metadata::TunnelKind::Remote => " NEW REMOTE FORWARDING ",
            crate::metadata::TunnelKind::Dynamic => " NEW DYNAMIC FORWARDING ",
        }
    });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if dialog.deleting {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!("Delete forwarding \"{}\"?", dialog.fields[0])),
                Line::from(""),
                Line::from("y / Enter confirm    n / Esc cancel"),
            ])
            .style(Style::default().fg(TEXT))
            .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    }
    let labels = [
        "Label",
        "SSH host / IP",
        if dialog.kind == crate::metadata::TunnelKind::Remote {
            "Remote bind"
        } else {
            "Local bind"
        },
        if dialog.kind == crate::metadata::TunnelKind::Remote {
            "Remote port"
        } else {
            "Local port"
        },
        if dialog.kind == crate::metadata::TunnelKind::Remote {
            "Local destination"
        } else {
            "Destination"
        },
        "Destination port",
        "Connect timeout (s)",
        "KeepAlive interval (s)",
    ];
    let mode = match dialog.kind {
        crate::metadata::TunnelKind::Local => "Local",
        crate::metadata::TunnelKind::Remote => "Remote",
        crate::metadata::TunnelKind::Dynamic => "Dynamic",
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Mode  ", Style::default().fg(MUTED)),
            Span::styled(
                "[F1 Local]  ",
                Style::default().fg(if mode == "Local" { GREEN } else { MUTED }),
            ),
            Span::styled(
                "[F2 Remote]  ",
                Style::default().fg(if mode == "Remote" { GREEN } else { MUTED }),
            ),
            Span::styled(
                "[F3 Dynamic]",
                Style::default().fg(if mode == "Dynamic" { GREEN } else { MUTED }),
            ),
        ])),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );
    let rows = Layout::vertical([Constraint::Length(1); 8]).split(Rect {
        x: inner.x,
        y: inner.y.saturating_add(2),
        width: inner.width,
        height: inner.height.saturating_sub(2),
    });
    for (index, (label, value)) in labels.iter().zip(dialog.fields.iter()).enumerate() {
        let cursor = if index == dialog.selected_field {
            "_"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{label:<19}"), Style::default().fg(MUTED)),
                Span::styled(format!("{value}{cursor}"), Style::default().fg(TEXT)),
            ])),
            rows[index],
        );
    }
    let help_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(11),
        width: inner.width,
        height: inner.height.saturating_sub(11),
    };
    frame.render_widget(
        Paragraph::new(
            "Use a Host alias from ~/.ssh/config, or enter an IP/domain    Tab / ↑↓ field    Enter next/save    Esc cancel",
        )
            .style(Style::default().fg(MUTED)),
        help_area,
    );
}

fn render_tunnel_host_picker(frame: &mut Frame, app: &App) {
    let area = centered_rect(64, 20, frame.area());
    frame.render_widget(Clear, area);
    render_host_picker(frame, area, app);
}

fn render_search(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 5, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!("/{}_", app.search_query)).block(
            Block::default()
                .title(" SEARCH HOSTS ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(GREEN))
                .style(Style::default().bg(PANEL)),
        ),
        area,
    );
}

fn render_connection_dialog(frame: &mut Frame, dialog: &ConnectionDialog) {
    let area = centered_rect(72, 9, frame.area());
    let border = if dialog.pending {
        GREEN
    } else {
        Color::Rgb(221, 132, 112)
    };
    let footer = if dialog.pending {
        "Please wait..."
    } else {
        "Press any key to close"
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                dialog.message.as_str(),
                Style::default().fg(TEXT),
            )),
            Line::from(""),
            Line::from(Span::styled(footer, Style::default().fg(MUTED))),
        ])
        .block(
            Block::default()
                .title(format!(" {} ", dialog.title))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .style(Style::default().bg(PANEL)),
        )
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_credential_export_dialog(frame: &mut Frame, dialog: &crate::app::CredentialExportDialog) {
    let area = centered_rect(76, 18, frame.area());
    let mask = |value: &str| "*".repeat(value.chars().count());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(" EXPORT ENCRYPTED CREDENTIALS ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(GREEN))
            .style(Style::default().bg(PANEL)),
        area,
    );
    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GREEN))
        .inner(area);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new("Set a password to protect the exported .age file.")
            .style(Style::default().fg(TEXT)),
        rows[0],
    );
    for (index, label) in ["Export password", "Confirm password"].iter().enumerate() {
        let label_area = Rect {
            x: rows[index + 2].x,
            y: rows[index + 2].y,
            width: rows[index + 2].width,
            height: 1,
        };
        let input_area = Rect {
            x: rows[index + 2].x,
            y: rows[index + 2].y + 1,
            width: rows[index + 2].width,
            height: 3,
        };
        let focused = dialog.selected_field == index;
        frame.render_widget(
            Paragraph::new(*label).style(
                Style::default()
                    .fg(if focused { GREEN } else { TEXT })
                    .add_modifier(Modifier::BOLD),
            ),
            label_area,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{}{}",
                mask(&dialog.fields[index]),
                if focused { "_" } else { "" }
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(if focused { GREEN } else { BORDER })),
            )
            .style(Style::default().fg(if focused { GREEN } else { TEXT })),
            input_area,
        );
    }
    if let Some(message) = dialog.error.as_deref() {
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(Color::Rgb(221, 132, 112))),
            rows[5],
        );
    }
    frame.render_widget(
        Paragraph::new("Tab/↑↓ switch field   Enter export   Esc cancel")
            .style(Style::default().fg(MUTED)),
        rows[6],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{
        app::{App, TransferStatus, TransferTask},
        hosts::{Host, HostCatalog},
    };

    #[test]
    fn file_sizes_use_readable_units() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(1024), "1 KB");
        assert_eq!(format_bytes(12 * 1024), "12 KB");
        assert_eq!(format_bytes(1536 * 1024), "1.5 MB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2 GB");
    }

    #[test]
    fn remote_times_use_readable_utc_values() {
        assert_eq!(format_unix_timestamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_unix_timestamp(1_704_067_200), "2024-01-01 00:00 UTC");
    }

    #[test]
    fn credential_export_fields_render_masked_input_and_cursor() {
        let mut app = App::with_catalog(
            HostCatalog::default(),
            Ok(crate::metadata::Metadata::default()),
        );
        app.credential_export_dialog = Some(crate::app::CredentialExportDialog {
            fields: ["secret".into(), "confirmed".into()],
            selected_field: 0,
            error: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = format!("{}", terminal.backend());
        assert!(text.contains("******_"), "{text}");
        assert!(text.contains("*********"), "{text}");
    }

    #[test]
    fn populated_projects_forms_and_output_render_and_scroll_at_supported_sizes() {
        use crate::projects::tests::{Fixture, until};
        use crossterm::event::KeyCode;
        let mut fixture = Fixture::new("render");
        fixture.project.name = "Demo".into();
        let mut command = fixture.command(
            "i=0; while [ \"$i\" -lt 200 ]; do printf 'line-%03d\\n' \"$i\"; i=$((i + 1)); done",
        );
        command.name = "Server".into();
        fixture.project.commands.push(command.clone());
        let mut app = App::with_catalog(
            HostCatalog::default(),
            Ok(crate::metadata::Metadata::default()),
        );
        app.metadata_error = true;
        app.metadata.projects.push(fixture.project.clone());
        app.project_service = fixture.service();
        app.project_service
            .start(&fixture.project, &command)
            .unwrap();
        until(&mut app.project_service, |s| {
            !s.active((fixture.project.id, command.id))
        });
        for (width, height) in [(120, 40), (80, 24), (54, 18), (42, 18), (32, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            app.section = NavSection::Projects;
            app.viewport_width = width;
            app.viewport_height = height;
            app.workspace.project_open = false;
            app.workspace.output_open = false;
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("Demo"));
            app.handle_projects_key(KeyCode::Enter);
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("Server"));
            assert!(format!("{}", terminal.backend()).contains("exit 0"));
            app.handle_projects_key(KeyCode::Enter);
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(
                format!("{}", terminal.backend()).contains("line-199"),
                "{width}x{height} must follow live tail"
            );
            app.handle_projects_key(KeyCode::Home);
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("line-000"));
            assert!(!format!("{}", terminal.backend()).contains("line-199"));
            app.handle_projects_key(KeyCode::End);
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("line-199"));
            app.handle_projects_key(KeyCode::Char('e'));
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("EDIT COMMAND"));
            assert!(format!("{}", terminal.backend()).contains("Name"));
            assert!(format!("{}", terminal.backend()).contains("Working directory"));
            app.handle_projects_key(KeyCode::Esc);
        }
    }

    #[test]
    fn runtime_project_source_shows_project_and_command_and_group_stop_confirmation() {
        use crate::runtime::{RuntimeEntry, RuntimeOrigin, RuntimeProcessKind};
        let mut app = App::with_catalog(
            HostCatalog::default(),
            Ok(crate::metadata::Metadata::default()),
        );
        app.section = NavSection::Runtime;
        app.runtime_entries.push(RuntimeEntry {
            protocol: "TCP".into(),
            address: "127.0.0.1".into(),
            port: 3000,
            pid: Some(42),
            process: "node".into(),
            user: "501".into(),
            command: "node server.js".into(),
            working_directory: Some("/tmp/demo".into()),
            owned_by_current_user: true,
            kind: RuntimeProcessKind::Application,
            origin: RuntimeOrigin::Project,
            origin_reason: "Managed command: Demo / Dev server".into(),
            project: Some(crate::projects::ProjectOwner {
                key: (1, 2),
                run_id: 3,
                process_group: 42,
                project_name: "Demo".into(),
                command_name: "Dev server".into(),
            }),
            external_project: None,
            tunnel: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = format!("{}", terminal.backend());
        assert!(text.contains("PROJECT"));
        assert!(text.contains("Demo / Dev server"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "P" && cell.fg == GREEN)
        );
        app.runtime_kill_confirmation = app.selected_runtime_entry().cloned();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(format!("{}", terminal.backend()).contains("all its child processes"));
    }

    #[test]
    fn runtime_categories_render_selection_and_filtered_process_details() {
        use crate::runtime::{RuntimeEntry, RuntimeFilter, RuntimeProcessKind};
        for (width, height) in [(120, 30), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut app = App::with_catalog(
                HostCatalog::default(),
                Ok(crate::metadata::Metadata::default()),
            );
            app.section = NavSection::Runtime;
            app.runtime_entries.push(RuntimeEntry {
                protocol: "TCP".into(),
                address: "127.0.0.1".into(),
                port: 3000,
                pid: Some(123),
                process: "node".into(),
                user: "501".into(),
                command: "node server.js".into(),
                working_directory: Some("/tmp/project".into()),
                owned_by_current_user: true,
                kind: RuntimeProcessKind::Application,
                origin: crate::runtime::RuntimeOrigin::CommandLine,
                origin_reason: "Shell ancestor".into(),
                project: None,
                external_project: None,
                tunnel: None,
            });
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("Mine 1"));
            assert!(text.contains("Apps 1"));
            assert!(text.contains("System 0"));
            assert!(text.contains(">> 3000"));
            assert!(text.contains("CLI"));
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "n"
                        && cell.fg
                            == runtime_origin_color(crate::runtime::RuntimeOrigin::CommandLine))
            );
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "3" && cell.bg == Color::Rgb(47, 78, 61))
            );
            app.runtime_filter = RuntimeFilter::System;
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(!format!("{}", terminal.backend()).contains("node"));
            app.runtime_filter = RuntimeFilter::Mine;
            app.runtime_target = crate::runtime::RuntimeTarget::Host("prod-web".into());
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("Local"));
            assert!(text.contains("Hosts ▾"));
            assert!(text.contains("prod-web"));
            let layout = runtime_layout(terminal.get_frame().area());
            assert_eq!(
                terminal.backend().buffer()[(layout.hosts.x + 1, layout.hosts.y)].fg,
                GREEN
            );
            assert_eq!(
                terminal.backend().buffer()[(layout.local.x + 1, layout.local.y)].fg,
                MUTED
            );
            app.runtime_kill_confirmation = app.selected_runtime_entry().cloned();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("Machine: Host / prod-web"));
        }
    }

    #[test]
    fn all_sections_render_at_desktop_and_compact_sizes() {
        for (width, height) in [(120, 40), (80, 24), (54, 18), (42, 18), (32, 12)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("create test terminal");
            let mut app = App::with_catalog(
                HostCatalog::default(),
                Ok(crate::metadata::Metadata::default()),
            );

            for section in NavSection::ALL {
                app.section = section;
                terminal
                    .draw(|frame| render(frame, &app))
                    .expect("render section");
                if section == NavSection::Runtime {
                    let text = format!("{}", terminal.backend());
                    for label in ["Local", "Hosts ▾", "Mine 0", "Apps 0", "System 0", "All 0"] {
                        assert!(text.contains(label), "{width}x{height}: missing {label}");
                    }
                }
            }
        }
    }

    #[test]
    fn files_render_persistent_local_pane_remote_empty_state_and_host_picker() {
        use crate::{
            hosts::Host,
            metadata::{Metadata, parse_tags},
        };

        for (width, height) in [(120, 40), (80, 24), (54, 18)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut app = App::with_catalog(
                HostCatalog {
                    hosts: vec![Host {
                        alias: "web-prod".into(),
                        hostname: Some("web.example.com".into()),
                        user: None,
                        port: None,
                        identity_file: None,
                        proxy_jump: None,
                        managed: false,
                    }],
                    error: None,
                },
                Ok(Metadata::default()),
            );
            app.section = NavSection::Files;
            app.metadata
                .tags
                .insert("web-prod".into(), parse_tags("production"));
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("LOCAL"));
            assert!(text.contains("REMOTE"));
            assert!(text.contains("Connect to host"));
            assert!(text.contains("Select host"));

            app.host_picker_open = true;
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("Groups"));
            assert!(text.contains("Hosts"));
            assert!(text.contains("production"));
            assert!(text.contains("web-prod"));
        }
    }

    #[test]
    fn files_render_transfer_task_list() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut app = App::with_catalog(
            HostCatalog::default(),
            Ok(crate::metadata::Metadata::default()),
        );
        app.section = NavSection::Files;
        app.transfer_tasks.push(TransferTask {
            id: 1,
            host_alias: "web".into(),
            direction: crate::sftp::TransferDirection::Upload,
            name: "notes.txt".into(),
            local_path: std::path::PathBuf::from("/tmp/notes.txt"),
            remote_path: "notes.txt".into(),
            is_dir: false,
            transferred: 10,
            total: 10,
            status: TransferStatus::Completed,
        });

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = format!("{}", terminal.backend());
        assert!(text.contains("TRANSFERS"));
        assert!(text.contains("notes.txt"));
        assert!(text.contains("done"));
    }

    #[test]
    fn tunnels_render_list_and_detail_panel() {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut app = App::with_catalog(
            HostCatalog {
                hosts: vec![Host {
                    alias: "web".into(),
                    hostname: Some("web.example.com".into()),
                    user: None,
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                    managed: false,
                }],
                error: None,
            },
            Ok(crate::metadata::Metadata::default()),
        );
        app.section = NavSection::Tunnels;
        app.metadata
            .tunnels
            .push(crate::metadata::TunnelDefinition {
                id: 1,
                label: "web-db".into(),
                host_alias: "web".into(),
                custom_host: false,
                kind: crate::metadata::TunnelKind::Local,
                bind_address: "127.0.0.1".into(),
                local_port: 15432,
                remote_host: Some("db.internal".into()),
                remote_port: Some(5432),
                lifecycle: crate::metadata::TunnelLifecycle::App,
                auto_reconnect: false,
                connect_timeout_secs: 10,
                keepalive_interval_secs: 30,
            });

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = format!("{}", terminal.backend());
        assert!(text.contains("PORT FORWARDING"));
        assert!(text.contains("web-db"));
        assert!(text.contains("db.internal:5432"));
        assert!(text.contains("EDIT PORT FORWARDING"));
    }

    #[test]
    fn tabs_share_one_top_row_and_content_has_no_sidebar() {
        for (width, height) in [(120, 40), (80, 24), (54, 18), (42, 18)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut app = App::with_catalog(
                HostCatalog::default(),
                Ok(crate::metadata::Metadata::default()),
            );
            for (index, section) in NavSection::ALL.into_iter().enumerate() {
                app.section = section;
                terminal.draw(|frame| render(frame, &app)).unwrap();
                let buffer = terminal.backend().buffer();
                let row = &buffer.content[usize::from(width)..usize::from(width) * 2];
                let text = row.iter().map(|cell| cell.symbol()).collect::<String>();
                for (number, tab) in NavSection::ALL.iter().enumerate() {
                    let label = if width < 70 {
                        format!("{} {}", number + 1, &tab.title()[..1])
                    } else if width < 100 {
                        tab.title().to_string()
                    } else {
                        format!("{} {}", number + 1, tab.title())
                    };
                    assert!(text.contains(&label));
                }
                let selected_label = if width < 70 {
                    format!("{} {}", index + 1, &section.title()[..1])
                } else if width < 100 {
                    section.title().to_string()
                } else {
                    format!("{} {}", index + 1, section.title())
                };
                let selected = text.find(&selected_label).unwrap();
                assert_eq!(row[selected].fg, GREEN);
                assert_eq!(row[selected].bg, Color::Rgb(34, 54, 47));
                assert!(!format!("{}", terminal.backend()).contains("WORKSPACE"));
                if section == NavSection::Hosts {
                    assert!(format!("{}", terminal.backend()).contains("All hosts"));
                }
            }
        }
    }

    #[test]
    fn group_list_host_list_and_tag_editor_render_at_multiple_sizes() {
        use crate::{
            hosts::Host,
            metadata::{Metadata, parse_tags},
        };
        for (width, height) in [(120, 40), (80, 24), (54, 18)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let catalog = HostCatalog {
                hosts: vec![Host {
                    alias: "web".into(),
                    hostname: Some("web.example.com".into()),
                    user: None,
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                    managed: false,
                }],
                error: None,
            };
            let mut metadata = Metadata::default();
            metadata.tags.insert("web".into(), parse_tags("prod,work"));
            let mut app = App::with_catalog(catalog, Ok(metadata));
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("HOSTS"));
            assert!(text.contains("GROUPS"));
            assert!(text.contains("prod"));
            app.enter_group();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(text.contains("web"));
            assert!(text.contains("HOSTS"));
            app.active_group = None;
            terminal.draw(|frame| render(frame, &app)).unwrap();
            if width >= 80 {
                assert!(format!("{}", terminal.backend()).contains("web"));
            }
            app.tags_dialog = Some(("web".into(), "prod,work".into()));
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("TAGS / web"));
        }
    }

    #[test]
    fn group_cards_reflow_and_scroll_to_the_selected_group() {
        use crate::{hosts::Host, metadata::Metadata};
        for (width, height) in [(120, 40), (80, 24), (54, 18), (32, 18)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut metadata = Metadata::default();
            metadata.tags.insert(
                "web".into(),
                (0..12).map(|index| format!("group-{index:02}")).collect(),
            );
            let catalog = HostCatalog {
                hosts: vec![Host {
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
            let mut app = App::with_catalog(catalog, Ok(metadata));
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 6)].symbol(), "┌");
            assert_eq!(buffer[(0, 6)].fg, GREEN);
            let text = format!("{}", terminal.backend());
            assert!(text.contains("group-00"));
            assert!(text.contains("1 host"));
            if width >= 70 {
                let sidebar = host_browser_width(width);
                assert_eq!(buffer[(sidebar - 1, 6)].symbol(), "┐");
                assert_eq!(buffer[(sidebar, 5)].symbol(), "┌");
                assert_eq!(buffer[(sidebar, 6)].symbol(), "│");
                assert_eq!(buffer[(0, 9)].symbol(), "└");
            }
            if width >= 54 {
                let row = (0..width)
                    .map(|x| buffer[(x, 7)].symbol())
                    .collect::<String>();
                assert!(row.contains("group-00") && row.contains("group-01"));
            }
            app.selected_browse_item = 11;
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert!(format!("{}", terminal.backend()).contains("group-11"));
            app.selected_browse_item = 12;
            terminal.draw(|frame| render(frame, &app)).unwrap();
            assert_eq!(app.selected_host().unwrap().alias, "web");
            assert!(format!("{}", terminal.backend()).contains("web"));
            assert_eq!(terminal.backend().buffer()[(0, 6)].fg, BORDER);
            app.selected_browse_item = 0;
            app.enter_group();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = format!("{}", terminal.backend());
            assert!(!text.contains("GROUPS"));
            assert!(text.contains("group-00"));
        }
    }
}
