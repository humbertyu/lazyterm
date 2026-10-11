use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Section {
    Hosts,
    Tunnels,
    Snippets,
}

impl Section {
    const ALL: [Self; 3] = [Self::Hosts, Self::Tunnels, Self::Snippets];

    fn title(self) -> &'static str {
        match self {
            Self::Hosts => "Hosts",
            Self::Tunnels => "Tunnels",
            Self::Snippets => "Snippets",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Hosts => "SSH hosts will appear here.",
            Self::Tunnels => "No tunnels configured.",
            Self::Snippets => "No snippets configured.",
        }
    }
}

struct App {
    section: Section,
    should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            section: Section::Hosts,
            should_quit: false,
        }
    }
}

impl App {
    fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab | KeyCode::Right => self.next_section(),
            KeyCode::BackTab | KeyCode::Left => self.previous_section(),
            KeyCode::Char('1') => self.section = Section::Hosts,
            KeyCode::Char('2') => self.section = Section::Tunnels,
            KeyCode::Char('3') => self.section = Section::Snippets,
            _ => {}
        }
    }

    fn next_section(&mut self) {
        let index = Section::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.section = Section::ALL[(index + 1) % Section::ALL.len()];
    }

    fn previous_section(&mut self) {
        let index = Section::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.section = Section::ALL[(index + Section::ALL.len() - 1) % Section::ALL.len()];
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, App::default());
    restore_terminal();
    result
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| render(frame, &app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key(key.code);
        }
    }
    Ok(())
}

fn render(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " LAZYTERM ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "lightweight remote workspace",
            Style::default().fg(Color::Gray),
        ),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(title, areas[0]);

    let selected = Section::ALL
        .iter()
        .position(|section| *section == app.section)
        .unwrap_or_default();
    let tabs = Tabs::new(
        Section::ALL
            .iter()
            .map(|section| Line::from(format!(" {} ", section.title())))
            .collect::<Vec<_>>(),
    )
    .select(selected)
    .highlight_style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .divider("  ");
    frame.render_widget(tabs, areas[1]);

    let footer = Paragraph::new("  1-3 switch view   Tab next   q quit")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, areas[2]);

    let content_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(areas[1])[1];
    let content = Paragraph::new(app.section.empty_message())
        .style(Style::default().fg(Color::Gray))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", app.section.title())),
        );
    frame.render_widget(content, content_area);
}

fn setup_terminal() -> Result<DefaultTerminal> {
    Ok(ratatui::init())
}

fn restore_terminal() {
    ratatui::restore();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_shortcuts_select_sections() {
        let mut app = App::default();
        app.handle_key(KeyCode::Char('2'));
        assert_eq!(app.section, Section::Tunnels);
        app.handle_key(KeyCode::Char('3'));
        assert_eq!(app.section, Section::Snippets);
    }

    #[test]
    fn section_navigation_wraps() {
        let mut app = App::default();
        app.handle_key(KeyCode::Left);
        assert_eq!(app.section, Section::Snippets);
        app.handle_key(KeyCode::Right);
        assert_eq!(app.section, Section::Hosts);
    }
}
