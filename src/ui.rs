use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};

use crate::app::{App, ConnectionDialog, NavSection, group_columns, host_browser_width};

const BG: Color = Color::Rgb(13, 16, 17);
const PANEL: Color = Color::Rgb(18, 23, 23);
const BORDER: Color = Color::Rgb(49, 65, 61);
const GREEN: Color = Color::Rgb(133, 196, 159);
const TEXT: Color = Color::Rgb(215, 228, 218);
const MUTED: Color = Color::Rgb(107, 126, 116);
const DISABLED: Color = Color::Rgb(76, 91, 82);

pub fn render(frame: &mut Frame, app: &App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG)),
        frame.area(),
    );
    let page = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());

    render_header(frame, page[0], app);
    render_navigation(frame, page[1], app);
    match app.section {
        NavSection::Hosts => render_hosts(frame, page[2], app),
        NavSection::Files => {
            let description = app.selected_host().map_or_else(
                || "Select a host in Hosts to choose the remote file context.".to_string(),
                |host| format!("SFTP browser for {} will appear here.", host.alias),
            );
            render_placeholder(frame, page[2], "Files", "SFTP workspace", &description);
        }
        NavSection::Tunnels => {
            let description = app.selected_host().map_or_else(
                || "Create local, remote, or SOCKS port forwards.".to_string(),
                |host| format!("Tunnel manager for {} will appear here.", host.alias),
            );
            render_placeholder(frame, page[2], "Tunnels", "Port forwarding", &description);
        }
        NavSection::Snippets => render_placeholder(
            frame,
            page[2],
            "Snippets",
            "Command library",
            "Save commands and run them against one or more hosts.",
        ),
    }
    render_footer(frame, page[3], app);
    if app.searching {
        render_search(frame, app);
    }
    if let Some(dialog) = &app.connection_dialog {
        render_connection_dialog(frame, dialog);
    }
    if let Some(dialog) = &app.host_dialog {
        render_host_dialog(frame, dialog);
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
    ]);
    frame.render_widget(Paragraph::new(content), area);
}

fn render_navigation(frame: &mut Frame, area: Rect, app: &App) {
    let titles = NavSection::ALL.iter().enumerate().map(|(index, section)| {
        let title = if area.width < 34 {
            format!("{} {}", index + 1, &section.title()[..1])
        } else if area.width < 42 {
            section.title().to_string()
        } else {
            format!("{} {}", index + 1, section.title())
        };
        Line::from(title)
    });
    let selected = NavSection::ALL
        .iter()
        .position(|section| *section == app.section)
        .unwrap_or_default();
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .padding("", "")
            .divider(" | ")
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
            detail_line("Auth", "ssh-agent, key, or password prompt"),
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
            detail_line("Identity", "IdentityFile / ssh-agent"),
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
        Span::styled(format!("{label:<11}"), Style::default().fg(MUTED)),
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

fn render_placeholder(
    frame: &mut Frame,
    area: Rect,
    section: &str,
    title: &str,
    description: &str,
) {
    let block = panel(section);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let content = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(38),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(title).style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
        content[1],
    );
    frame.render_widget(
        Paragraph::new(description)
            .style(Style::default().fg(MUTED))
            .wrap(Wrap { trim: true }),
        content[2],
    );
    frame.render_widget(
        Paragraph::new("Planned").style(Style::default().fg(GREEN).add_modifier(Modifier::BOLD)),
        content[3],
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let navigation = if area.width < 75 {
        "1-4 tabs  Tab next  Shift+Tab prev  ` last"
    } else {
        "1-4 / F1-F4 tabs  Tab / Shift+Tab cycle  Ctrl+Left/Right cycle  ` last"
    };
    let hint = if app.host_dialog.is_some() {
        "Tab / Shift+Tab field  Enter next/save  Esc cancel"
    } else if app.tags_dialog.is_some() {
        "Enter save tags  Esc cancel"
    } else if app.searching {
        "Type to filter   Enter apply   Esc clear"
    } else if app.section == NavSection::Hosts {
        if area.width < 75 && app.showing_groups() {
            "Arrows select  Enter open/SSH  / find  q quit"
        } else if area.width < 75 {
            "↑↓ select  Enter SSH  g tags  Left/Esc back"
        } else if app.showing_groups() {
            "Arrows select  Enter open/SSH  g tags  / find  a add  r reload  q quit"
        } else if area.width < 95 {
            "↑↓ move  Enter SSH  v view  g tags  / find  Esc back  q quit"
        } else {
            "↑↓ select  Enter SSH  v view  g tags  Esc back  a add  e edit  d delete  / find  r reload  q quit"
        }
    } else {
        "q quit"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(if app.host_dialog.is_some() || app.tags_dialog.is_some() {
                ""
            } else if app.searching {
                "F1-F4 / Ctrl+Left/Right tabs"
            } else {
                navigation
            }),
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
    use crate::{app::App, hosts::HostCatalog};

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
            }
        }
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
                    assert!(text.contains(&format!("{} {}", number + 1, tab.title())));
                }
                let selected = text
                    .find(&format!("{} {}", index + 1, section.title()))
                    .unwrap();
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
