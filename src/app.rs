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
    pub host_dialog: Option<HostDialog>,
}

pub struct HostDialog {
    pub fields: [String; 5],
    pub original_alias: Option<String>,
    pub selected_field: usize,
    pub editing: bool,
    pub deleting: bool,
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
            host_dialog: None,
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
        if self.host_dialog.is_some() {
            self.handle_host_dialog_key(key);
            return;
        }
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
            KeyCode::Char('a') if self.section == NavSection::Hosts => self.open_host_form(None),
            KeyCode::Char('e') if self.section == NavSection::Hosts => {
                if let Some(host) = self.selected_host().filter(|host| host.managed).cloned() {
                    self.open_host_form(Some(host));
                } else {
                    self.notice = Some("Only LazyTerm-managed hosts can be edited.".to_string());
                }
            }
            KeyCode::Char('d') if self.section == NavSection::Hosts => {
                if let Some(host) = self.selected_host().filter(|host| host.managed) {
                    self.host_dialog = Some(HostDialog {
                        fields: [
                            host.alias.clone(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                        ],
                        original_alias: None,
                        selected_field: 0,
                        editing: false,
                        deleting: true,
                    });
                } else {
                    self.notice = Some("Only LazyTerm-managed hosts can be deleted.".to_string());
                }
            }
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

    fn open_host_form(&mut self, host: Option<Host>) {
        let editing = host.is_some();
        let original_alias = host.as_ref().map(|host| host.alias.clone());
        self.host_dialog = Some(HostDialog {
            fields: host.map_or_else(
                || std::array::from_fn(|_| String::new()),
                |host| {
                    [
                        host.alias,
                        host.hostname.unwrap_or_default(),
                        host.user.unwrap_or_default(),
                        host.port.map_or_else(String::new, |port| port.to_string()),
                        host.identity_file.unwrap_or_default(),
                    ]
                },
            ),
            original_alias,
            selected_field: 0,
            editing,
            deleting: false,
        });
    }

    fn handle_host_dialog_key(&mut self, key: KeyCode) {
        let dialog = self.host_dialog.as_mut().expect("dialog exists");
        if dialog.deleting {
            match key {
                KeyCode::Char('y') => {
                    let alias = dialog.fields[0].clone();
                    self.host_dialog = None;
                    match self.hosts.delete_managed(&alias) {
                        Ok(()) => self.notice = Some(format!("Deleted host {alias}.")),
                        Err(error) => self.notice = Some(format!("Could not delete host: {error}")),
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => self.host_dialog = None,
                _ => {}
            }
            return;
        }
        match key {
            KeyCode::Esc => self.host_dialog = None,
            KeyCode::Tab | KeyCode::Down => dialog.selected_field = (dialog.selected_field + 1) % 5,
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field = (dialog.selected_field + 4) % 5
            }
            KeyCode::Backspace => {
                dialog.fields[dialog.selected_field].pop();
            }
            KeyCode::Enter => {
                if dialog.selected_field < 4 {
                    dialog.selected_field += 1;
                    return;
                }
                let Some(dialog) = self.host_dialog.take() else {
                    return;
                };
                let port = if dialog.fields[3].is_empty() {
                    None
                } else {
                    match dialog.fields[3].parse::<u16>() {
                        Ok(port) if port > 0 => Some(port),
                        _ => {
                            self.notice =
                                Some("Port must be an integer from 1 to 65535.".to_string());
                            self.host_dialog = Some(dialog);
                            return;
                        }
                    }
                };
                if dialog.fields[0].trim().is_empty()
                    || dialog.fields[0].chars().any(char::is_whitespace)
                {
                    self.notice =
                        Some("Host alias is required and cannot contain whitespace.".to_string());
                    self.host_dialog = Some(dialog);
                    return;
                }
                let host = Host {
                    alias: dialog.fields[0].trim().to_string(),
                    hostname: nonempty(&dialog.fields[1]),
                    user: nonempty(&dialog.fields[2]),
                    port,
                    identity_file: nonempty(&dialog.fields[4]),
                    managed: true,
                };
                match self
                    .hosts
                    .upsert_managed_from(&host, dialog.original_alias.as_deref())
                {
                    Ok(()) => self.notice = Some(format!("Saved host {}.", host.alias)),
                    Err(error) => self.notice = Some(format!("Could not save host: {error}")),
                }
            }
            KeyCode::Char(value) if !value.is_control() => {
                dialog.fields[dialog.selected_field].push(value)
            }
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

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
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
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                managed: false,
            },
            Host {
                alias: "two".to_string(),
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                managed: false,
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
