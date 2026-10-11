use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{App, ConnectionDialog, NavSection};

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
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, page[0], app);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(17), Constraint::Min(1)])
        .split(page[1]);
    render_navigation(frame, columns[0], app);
    match app.section {
        NavSection::Hosts => render_hosts(frame, columns[1], app),
        NavSection::Files => {
            let description = app.selected_host().map_or_else(
                || "Select a host in Hosts to choose the remote file context.".to_string(),
                |host| format!("SFTP browser for {} will appear here.", host.alias),
            );
            render_placeholder(frame, columns[1], "Files", "SFTP workspace", &description);
        }
        NavSection::Tunnels => {
            let description = app.selected_host().map_or_else(
                || "Create local, remote, or SOCKS port forwards.".to_string(),
                |host| format!("Tunnel manager for {} will appear here.", host.alias),
            );
            render_placeholder(
                frame,
                columns[1],
                "Tunnels",
                "Port forwarding",
                &description,
            );
        }
        NavSection::Snippets => render_placeholder(
            frame,
            columns[1],
            "Snippets",
            "Command library",
            "Save commands and run them against one or more hosts.",
        ),
    }
    render_footer(frame, page[2], app);
    if app.searching {
        render_search(frame, app);
    }
    if let Some(dialog) = &app.connection_dialog {
        render_connection_dialog(frame, dialog);
    }
    if let Some(dialog) = &app.host_dialog {
        render_host_dialog(frame, dialog);
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
    frame.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(BORDER)),
        ),
        area,
    );
}

fn render_navigation(frame: &mut Frame, area: Rect, app: &App) {
    let items = [
        (NavSection::Hosts, "1", false),
        (NavSection::Files, "2", true),
        (NavSection::Tunnels, "3", true),
        (NavSection::Snippets, "4", true),
    ]
    .into_iter()
    .map(|(section, key, planned)| {
        let selected = app.section == section;
        let marker = if selected { ">" } else { " " };
        let label = if planned {
            format!("{marker} {key}  {}  ·", section.title())
        } else {
            format!("{marker} {key}  {}", section.title())
        };
        let style = if selected {
            Style::default()
                .fg(TEXT)
                .bg(Color::Rgb(34, 54, 47))
                .add_modifier(Modifier::BOLD)
        } else if planned {
            Style::default().fg(DISABLED)
        } else {
            Style::default().fg(MUTED)
        };
        ListItem::new(label).style(style)
    });

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" WORKSPACE ")
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(BORDER)),
        ),
        area,
    );
}

fn render_hosts(frame: &mut Frame, area: Rect, app: &App) {
    if area.width < 60 {
        render_compact_hosts(frame, area, app);
        return;
    }
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(area);
    render_host_list(frame, panes[0], app);
    render_host_details(frame, panes[1], app);
}

fn render_compact_hosts(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);
    render_host_list(frame, rows[0], app);
    render_host_details(frame, rows[1], app);
}

fn render_host_list(frame: &mut Frame, area: Rect, app: &App) {
    let hosts = app.visible_hosts();
    let items = hosts
        .iter()
        .map(|host| ListItem::new(format!("  {}", host.alias)).style(Style::default().fg(TEXT)))
        .collect::<Vec<_>>();

    let title = if app.search_query.is_empty() {
        format!(" HOSTS  {} ", hosts.len())
    } else {
        format!(" HOSTS  {} matching ", hosts.len())
    };
    let mut state = ListState::default();
    if !hosts.is_empty() {
        state.select(Some(app.selected_host.min(hosts.len() - 1)));
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
    if hosts.is_empty() {
        let message = if app.hosts.error.is_some() {
            "SSH config could not be read."
        } else if !app.search_query.is_empty() {
            "No matching hosts."
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
            detail_line("Target", "Resolved by OpenSSH"),
            detail_line("User / port", "Resolved by OpenSSH"),
            detail_line("Auth", "ssh-agent, key, or password prompt"),
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

fn detail_line<'a>(label: &'a str, value: &'a str) -> Line<'a> {
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
    let hint = if app.searching {
        "Type to filter   Enter apply   Esc clear"
    } else if app.section == NavSection::Hosts {
        if area.width < 95 {
            "↑↓ move  Enter SSH  a add  e edit  d delete  / find  r reload  q quit"
        } else {
            "↑↓ select   Enter connect   a add   e edit   d delete   / search   r refresh   1-4 nav   q quit"
        }
    } else {
        "1-4 nav   Hosts: ↑↓ select, Enter connect   q quit"
    };
    frame.render_widget(Paragraph::new(hint).style(Style::default().fg(MUTED)), area);
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
        for (width, height) in [(120, 40), (80, 24), (54, 18)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("create test terminal");
            let mut app = App::new();
            app.hosts = HostCatalog::default();

            for section in NavSection::ALL {
                app.section = section;
                terminal
                    .draw(|frame| render(frame, &app))
                    .expect("render section");
            }
        }
    }
}
