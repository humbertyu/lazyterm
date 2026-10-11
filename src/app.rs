use crossterm::event::KeyCode;

use crate::hosts::{Host, HostCatalog};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavSection {
    Hosts,
    Files,
    Tunnels,
    Snippets,
}

impl NavSection {
    pub const ALL: [Self; 4] = [Self::Hosts, Self::Files, Self::Tunnels, Self::Snippets];

    pub fn title(self) -> &'static str {
        match self {
            Self::Hosts => "Hosts",
            Self::Files => "Files",
            Self::Tunnels => "Tunnels",
            Self::Snippets => "Snippets",
        }
    }
}

pub struct App {
    pub section: NavSection,
    pub hosts: HostCatalog,
    pub selected_host: usize,
    pub search_query: String,
    pub searching: bool,
    pub should_quit: bool,
    pub notice: Option<String>,
    pub connection_dialog: Option<ConnectionDialog>,
}

pub struct ConnectionDialog {
    pub title: String,
    pub message: String,
    pub pending: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            section: NavSection::Hosts,
            hosts: HostCatalog::load(),
            selected_host: 0,
            search_query: String::new(),
            searching: false,
            should_quit: false,
            notice: None,
            connection_dialog: None,
        }
    }

    pub fn visible_hosts(&self) -> Vec<&Host> {
        let query = self.search_query.to_lowercase();
        self.hosts
            .hosts
            .iter()
            .filter(|host| {
                query.is_empty()
                    || host.alias.to_lowercase().contains(&query)
                    || host.destination().to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn selected_host(&self) -> Option<&Host> {
        self.visible_hosts().get(self.selected_host).copied()
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        if self.connection_dialog.is_some() {
            self.connection_dialog = None;
            return;
        }

        match key {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down => self.move_selection(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Tab | KeyCode::Right => self.next_section(),
            KeyCode::BackTab | KeyCode::Left => self.previous_section(),
            KeyCode::Char('/') if self.section == NavSection::Hosts => {
                self.searching = true;
                self.search_query.clear();
                self.selected_host = 0;
            }
            KeyCode::Char('r') if self.section == NavSection::Hosts => self.refresh_hosts(),
            KeyCode::Char('1') => self.section = NavSection::Hosts,
            KeyCode::Char('2') => self.section = NavSection::Files,
            KeyCode::Char('3') => self.section = NavSection::Tunnels,
            KeyCode::Char('4') => self.section = NavSection::Snippets,
            KeyCode::Char('f') => self.section = NavSection::Files,
            KeyCode::Char('t') => self.section = NavSection::Tunnels,
            KeyCode::Char('s') => self.section = NavSection::Snippets,
            _ => {}
        }
    }

    pub fn handle_search_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Enter => self.searching = false,
            KeyCode::Esc => {
                self.searching = false;
                self.search_query.clear();
                self.selected_host = 0;
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.selected_host = 0;
            }
            KeyCode::Char(character) if !character.is_control() => {
                self.search_query.push(character);
                self.selected_host = 0;
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, offset: isize) {
        let count = self.visible_hosts().len();
        if count == 0 {
            self.selected_host = 0;
            return;
        }
        self.selected_host =
            (self.selected_host as isize + offset).rem_euclid(count as isize) as usize;
    }

    fn next_section(&mut self) {
        let index = NavSection::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.section = NavSection::ALL[(index + 1) % NavSection::ALL.len()];
    }

    fn previous_section(&mut self) {
        let index = NavSection::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.section = NavSection::ALL[(index + NavSection::ALL.len() - 1) % NavSection::ALL.len()];
    }

    fn refresh_hosts(&mut self) {
        self.hosts = HostCatalog::load();
        self.selected_host = 0;
        self.notice = self
            .hosts
            .error
            .clone()
            .or_else(|| Some("OpenSSH host list refreshed.".to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_wraps_and_host_selection_moves() {
        let mut app = App::new();
        app.hosts.hosts = vec![
            Host {
                alias: "one".to_string(),
            },
            Host {
                alias: "two".to_string(),
            },
        ];
        app.section = NavSection::Snippets;
        app.handle_key(KeyCode::Right);
        assert_eq!(app.section, NavSection::Hosts);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 1);
    }

    #[test]
    fn search_can_be_entered_and_cancelled() {
        let mut app = App::new();
        app.handle_key(KeyCode::Char('/'));
        app.handle_search_key(KeyCode::Char('d'));
        assert_eq!(app.search_query, "d");
        app.handle_search_key(KeyCode::Esc);
        assert!(app.search_query.is_empty());
        assert!(!app.searching);
    }
}
