use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::hosts::{Host, HostCatalog};
use crate::metadata::{HostView, Metadata, parse_tags};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum HostGroup {
    Tag(String),
    Untagged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostBrowseItem {
    Group(HostGroup),
    Host(usize),
}

pub fn host_browser_width(width: u16) -> u16 {
    if width < 70 {
        width
    } else {
        (width / 5 * 2).clamp(38, 48)
    }
}

pub fn group_columns(width: u16) -> usize {
    usize::from(width / 18).clamp(1, 2)
}

impl HostGroup {
    pub fn title(&self) -> &str {
        match self {
            Self::Tag(tag) => tag,
            Self::Untagged => "Untagged",
        }
    }
}

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
    pub previous_section: NavSection,
    pub hosts: HostCatalog,
    pub selected_host: usize,
    pub selected_browse_item: usize,
    pub viewport_width: u16,
    pub search_query: String,
    pub searching: bool,
    pub should_quit: bool,
    pub notice: Option<String>,
    pub connection_dialog: Option<ConnectionDialog>,
    pub host_dialog: Option<HostDialog>,
    pub metadata: Metadata,
    pub metadata_error: bool,
    pub active_group: Option<HostGroup>,
    pub selected_group: usize,
    pub tags_dialog: Option<(String, String)>,
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

#[derive(Debug, Eq, PartialEq)]
pub enum InputAction {
    None,
    Connect,
}

impl App {
    pub fn new() -> Self {
        let mut app = Self::with_catalog(HostCatalog::load(), Metadata::load());
        app.reconcile_metadata();
        app
    }

    pub(crate) fn with_catalog(hosts: HostCatalog, metadata: std::io::Result<Metadata>) -> Self {
        let notice = metadata
            .as_ref()
            .err()
            .map(|error| format!("Could not read host metadata: {error}"));
        let metadata_error = metadata.is_err();
        Self {
            section: NavSection::Hosts,
            previous_section: NavSection::Hosts,
            hosts,
            selected_host: 0,
            selected_browse_item: 0,
            viewport_width: 80,
            search_query: String::new(),
            searching: false,
            should_quit: false,
            notice,
            connection_dialog: None,
            host_dialog: None,
            metadata: metadata.unwrap_or_default(),
            metadata_error,
            active_group: None,
            selected_group: 0,
            tags_dialog: None,
        }
    }

    pub fn tags(&self, host: &Host) -> &[String] {
        self.metadata
            .tags
            .get(&host.alias)
            .map_or(&[], Vec::as_slice)
    }

    pub fn showing_groups(&self) -> bool {
        self.active_group.is_none()
    }

    fn matches_group(&self, host: &Host, group: &HostGroup) -> bool {
        match group {
            HostGroup::Tag(tag) => self.tags(host).contains(tag),
            HostGroup::Untagged => self.tags(host).is_empty(),
        }
    }

    pub fn group_hosts(&self, group: &HostGroup) -> Vec<&Host> {
        self.matching_hosts()
            .into_iter()
            .filter(|host| self.matches_group(host, group))
            .collect()
    }

    pub fn visible_groups(&self) -> Vec<HostGroup> {
        let mut groups = std::collections::BTreeSet::new();
        for host in self.matching_hosts() {
            if self.tags(host).is_empty() {
                groups.insert(HostGroup::Untagged);
            } else {
                groups.extend(self.tags(host).iter().cloned().map(HostGroup::Tag));
            }
        }
        groups.into_iter().collect()
    }

    pub fn selected_group(&self) -> Option<HostGroup> {
        match self.browse_items().get(self.selected_browse_item) {
            Some(HostBrowseItem::Group(group)) => Some(group.clone()),
            _ => None,
        }
    }

    pub fn enter_group(&mut self) -> bool {
        if !self.showing_groups() {
            return false;
        }
        if let Some(group) = self.selected_group() {
            self.selected_group = self.selected_browse_item;
            self.active_group = Some(group);
            self.selected_host = 0;
            self.selected_browse_item = 0;
            return true;
        }
        false
    }

    pub fn browse_items(&self) -> Vec<HostBrowseItem> {
        if self.active_group.is_some() {
            return self
                .visible_hosts()
                .iter()
                .enumerate()
                .map(|(index, _)| HostBrowseItem::Host(index))
                .collect();
        }
        let mut items = self
            .visible_groups()
            .into_iter()
            .map(HostBrowseItem::Group)
            .collect::<Vec<_>>();
        items.extend(
            self.matching_hosts()
                .iter()
                .enumerate()
                .map(|(index, _)| HostBrowseItem::Host(index)),
        );
        items
    }

    fn matching_hosts(&self) -> Vec<&Host> {
        let query = self.search_query.to_lowercase();
        self.hosts
            .hosts
            .iter()
            .filter(|host| {
                query.is_empty()
                    || host.alias.to_lowercase().contains(&query)
                    || host.destination().to_lowercase().contains(&query)
                    || self
                        .tags(host)
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn visible_hosts(&self) -> Vec<&Host> {
        self.matching_hosts()
            .into_iter()
            .filter(|host| {
                self.active_group
                    .as_ref()
                    .is_none_or(|group| self.matches_group(host, group))
            })
            .collect()
    }

    pub fn selected_host(&self) -> Option<&Host> {
        let index = match self.browse_items().get(self.selected_browse_item) {
            Some(HostBrowseItem::Host(index)) => *index,
            _ => return None,
        };
        self.visible_hosts().get(index).copied()
    }

    pub fn handle_event(&mut self, key: KeyEvent) -> InputAction {
        let modal = self.host_dialog.is_some()
            || self.tags_dialog.is_some()
            || self.connection_dialog.is_some();
        if !modal {
            match (key.code, key.modifiers) {
                (KeyCode::F(number @ 1..=4), KeyModifiers::NONE) => {
                    self.switch_section(NavSection::ALL[usize::from(number - 1)]);
                    return InputAction::None;
                }
                (KeyCode::Right, KeyModifiers::CONTROL) => {
                    self.next_section();
                    return InputAction::None;
                }
                (KeyCode::Left, KeyModifiers::CONTROL) => {
                    self.previous_section();
                    return InputAction::None;
                }
                _ => {}
            }
        }
        // Modified text must not turn into form input or an unrelated command.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return InputAction::None;
        }
        if self.searching && !modal {
            self.handle_search_key(key.code);
        } else if modal {
            self.handle_key(key.code);
        } else if self.section == NavSection::Hosts && key.code == KeyCode::Enter {
            if !self.enter_group() && self.selected_host().is_some() {
                return InputAction::Connect;
            }
        } else {
            self.handle_key(key.code);
        }
        InputAction::None
    }

    fn switch_section(&mut self, section: NavSection) {
        if section != self.section {
            self.previous_section = self.section;
            self.section = section;
            self.searching = false;
        }
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        if self.tags_dialog.is_some() {
            self.handle_tags_key(key);
            return;
        }
        if self.host_dialog.is_some() {
            self.handle_host_dialog_key(key);
            return;
        }
        if self.connection_dialog.is_some() {
            self.connection_dialog = None;
            return;
        }

        match key {
            KeyCode::Esc | KeyCode::Backspace
                if self.section == NavSection::Hosts && self.active_group.is_some() =>
            {
                self.leave_group();
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Up | KeyCode::Left | KeyCode::Right
                if self.section == NavSection::Hosts =>
            {
                self.move_browser_selection(key)
            }
            KeyCode::Tab => self.next_section(),
            KeyCode::BackTab => self.previous_section(),
            KeyCode::Char('`') => self.switch_section(self.previous_section),
            KeyCode::Char('/') if self.section == NavSection::Hosts => {
                self.searching = true;
                self.search_query.clear();
                self.selected_host = 0;
                self.selected_group = 0;
                self.selected_browse_item = 0;
            }
            KeyCode::Enter if self.section == NavSection::Hosts => {
                self.enter_group();
            }
            KeyCode::Char('v') if self.section == NavSection::Hosts => {
                self.active_group = None;
                self.selected_host = 0;
                self.selected_browse_item = 0;
            }
            KeyCode::Char('g') if self.section == NavSection::Hosts => {
                if let Some(host) = self.selected_host() {
                    self.tags_dialog = Some((host.alias.clone(), self.tags(host).join(", ")));
                }
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
            KeyCode::Char('1') => self.switch_section(NavSection::Hosts),
            KeyCode::Char('2') | KeyCode::Char('f') => self.switch_section(NavSection::Files),
            KeyCode::Char('3') | KeyCode::Char('t') => self.switch_section(NavSection::Tunnels),
            KeyCode::Char('4') | KeyCode::Char('s') => self.switch_section(NavSection::Snippets),
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
                        Ok(()) => {
                            self.metadata.tags.remove(&alias);
                            self.notice = Some(format!("Deleted host {alias}."));
                            self.save_metadata();
                            self.clamp_selection();
                        }
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
                    Ok(()) => {
                        if let Some(original) = dialog
                            .original_alias
                            .as_ref()
                            .filter(|alias| **alias != host.alias)
                        {
                            if let Some(tags) = self.metadata.tags.remove(original) {
                                self.metadata.tags.insert(host.alias.clone(), tags);
                            }
                        } else if !dialog.editing
                            && let Some(HostGroup::Tag(tag)) = &self.active_group
                        {
                            self.metadata
                                .tags
                                .insert(host.alias.clone(), vec![tag.clone()]);
                        }
                        self.notice = Some(format!("Saved host {}.", host.alias));
                        self.save_metadata();
                        self.clamp_selection();
                    }
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
        self.selected_group = 0;
        self.selected_browse_item = 0;
        match key {
            KeyCode::Enter => self.searching = false,
            KeyCode::Esc => {
                self.searching = false;
                self.search_query.clear();
                self.selected_host = 0;
                self.selected_browse_item = 0;
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

    fn leave_group(&mut self) {
        self.active_group = None;
        self.selected_host = 0;
        self.selected_browse_item = self.selected_group;
        self.clamp_selection();
    }

    fn move_browser_selection(&mut self, key: KeyCode) {
        if key == KeyCode::Left && self.active_group.is_some() {
            self.leave_group();
            return;
        }
        let count = self.browse_items().len();
        if count == 0 {
            self.selected_host = 0;
            self.selected_browse_item = 0;
            return;
        }
        let groups = if self.showing_groups() {
            self.visible_groups().len()
        } else {
            0
        };
        let columns = group_columns(host_browser_width(self.viewport_width));
        let selected = self.selected_browse_item.min(count - 1);
        self.selected_browse_item = match key {
            KeyCode::Right
                if selected < groups
                    && selected % columns + 1 < columns
                    && selected + 1 < groups =>
            {
                selected + 1
            }
            KeyCode::Left if selected < groups && !selected.is_multiple_of(columns) => selected - 1,
            KeyCode::Down if selected < groups => {
                if selected / columns + 1 < groups.div_ceil(columns) {
                    (selected + columns).min(groups - 1)
                } else if groups < count {
                    groups
                } else {
                    selected
                }
            }
            KeyCode::Down => (selected + 1).min(count - 1),
            KeyCode::Up if selected < groups => {
                if selected >= columns {
                    selected - columns
                } else {
                    selected
                }
            }
            KeyCode::Up => selected.saturating_sub(1),
            _ => selected,
        };
        if self.selected_browse_item < groups {
            self.selected_group = self.selected_browse_item;
        } else {
            self.selected_host = self.selected_browse_item - groups;
        }
    }

    fn next_section(&mut self) {
        let index = NavSection::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.switch_section(NavSection::ALL[(index + 1) % NavSection::ALL.len()]);
    }

    fn previous_section(&mut self) {
        let index = NavSection::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.switch_section(
            NavSection::ALL[(index + NavSection::ALL.len() - 1) % NavSection::ALL.len()],
        );
    }

    fn refresh_hosts(&mut self) {
        self.hosts = HostCatalog::load();
        match Metadata::load() {
            Ok(metadata) => {
                self.metadata = metadata;
                self.metadata_error = false;
                if self.metadata.view == HostView::Hosts {
                    self.active_group = None;
                }
            }
            Err(error) => {
                self.metadata_error = true;
                self.notice = Some(format!("Could not read host metadata: {error}"));
                self.clamp_selection();
                return;
            }
        }
        self.selected_host = 0;
        self.selected_browse_item = 0;
        self.clamp_selection();
        self.notice = self
            .hosts
            .error
            .clone()
            .or_else(|| Some("OpenSSH host list refreshed.".to_string()));
        self.reconcile_metadata();
    }

    fn clamp_selection(&mut self) {
        if self.hosts.error.is_none()
            && let Some(group) = &self.active_group
            && !self
                .hosts
                .hosts
                .iter()
                .any(|host| self.matches_group(host, group))
        {
            self.active_group = None;
            self.selected_group = 0;
        }
        self.selected_host = self
            .selected_host
            .min(self.visible_hosts().len().saturating_sub(1));
        self.selected_browse_item = self
            .selected_browse_item
            .min(self.browse_items().len().saturating_sub(1));
        self.selected_group = self
            .selected_group
            .min(self.visible_groups().len().saturating_sub(1));
    }

    fn save_metadata(&mut self) {
        if self.metadata_error {
            self.notice = Some(
                "Metadata unavailable; changes cannot be saved. Reload after fixing hosts.json."
                    .into(),
            );
        } else {
            self.metadata.reconcile(&self.hosts);
            if let Err(error) = self.metadata.save() {
                self.notice = Some(format!("Could not save host metadata: {error}"));
            }
        }
    }

    fn reconcile_metadata(&mut self) {
        if !self.metadata_error && self.metadata.reconcile(&self.hosts) {
            self.save_metadata();
        }
        self.clamp_selection();
    }

    fn handle_tags_key(&mut self, key: KeyCode) {
        let (_, value) = self.tags_dialog.as_mut().expect("tags dialog exists");
        match key {
            KeyCode::Esc => self.tags_dialog = None,
            KeyCode::Backspace => {
                value.pop();
            }
            KeyCode::Char(character) if !character.is_control() => value.push(character),
            KeyCode::Enter => {
                let (alias, value) = self.tags_dialog.take().expect("tags dialog exists");
                let tags = parse_tags(&value);
                if tags.is_empty() {
                    self.metadata.tags.remove(&alias);
                } else {
                    self.metadata.tags.insert(alias.clone(), tags);
                }
                self.notice = Some(format!("Updated tags for {alias}."));
                self.save_metadata();
                self.clamp_selection();
            }
            _ => {}
        }
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_fixture() -> App {
        let mut app = App::with_catalog(HostCatalog::default(), Ok(Metadata::default()));
        // Keyboard tests must never write to the developer's metadata.
        app.metadata_error = true;
        app.viewport_width = 32;
        app
    }

    #[test]
    fn navigation_wraps_and_host_selection_moves() {
        let mut app = app_fixture();
        app.metadata.view = HostView::Hosts;
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
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.section, NavSection::Hosts);
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_browse_item, 2);
        assert_eq!(app.selected_host().unwrap().alias, "two");
    }

    #[test]
    fn search_can_be_entered_and_cancelled() {
        let mut app = app_fixture();
        app.handle_key(KeyCode::Char('/'));
        app.handle_search_key(KeyCode::Char('d'));
        assert_eq!(app.search_query, "d");
        app.handle_search_key(KeyCode::Esc);
        assert!(app.search_query.is_empty());
        assert!(!app.searching);
    }

    fn grouped_fixture() -> App {
        let mut app = app_fixture();
        app.hosts.hosts = ["web", "db", "local"]
            .into_iter()
            .map(|alias| Host {
                alias: alias.into(),
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                managed: false,
            })
            .collect();
        app.metadata
            .tags
            .insert("web".into(), parse_tags("prod,work"));
        app.metadata.tags.insert("db".into(), parse_tags("prod"));
        app
    }

    #[test]
    fn groups_include_multi_tag_hosts_and_untagged_hosts() {
        let app = grouped_fixture();
        assert_eq!(
            app.visible_groups(),
            [
                HostGroup::Tag("prod".into()),
                HostGroup::Tag("work".into()),
                HostGroup::Untagged,
            ]
        );
        assert_eq!(app.group_hosts(&HostGroup::Tag("prod".into())).len(), 2);
        assert_eq!(app.group_hosts(&HostGroup::Untagged)[0].alias, "local");
        assert!(app.selected_host().is_none());
    }

    #[test]
    fn opening_and_leaving_groups_preserves_group_selection() {
        let mut app = grouped_fixture();
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_group, 1);
        assert!(app.enter_group());
        assert_eq!(app.selected_host().unwrap().alias, "web");
        assert!(!app.enter_group());
        app.handle_key(KeyCode::Esc);
        assert!(app.showing_groups());
        assert!(!app.should_quit);
        assert_eq!(app.selected_group, 1);
        app.handle_key(KeyCode::Char('v'));
        assert!(app.showing_groups());
        assert_eq!(app.visible_hosts().len(), 3);
        assert!(app.active_group.is_none());
    }

    #[test]
    fn search_matches_tags_and_stays_within_the_active_group() {
        let mut app = grouped_fixture();
        app.search_query = "work".into();
        assert_eq!(
            app.visible_groups(),
            [HostGroup::Tag("prod".into()), HostGroup::Tag("work".into())]
        );
        assert_eq!(app.visible_hosts().len(), 1);
        app.active_group = Some(HostGroup::Untagged);
        assert!(app.visible_hosts().is_empty());
        app.handle_search_key(KeyCode::Esc);
        assert_eq!(app.visible_hosts()[0].alias, "local");
    }

    #[test]
    fn tags_can_be_changed_on_unmanaged_hosts_without_editing_ssh_config() {
        let mut app = grouped_fixture();
        app.enter_group();
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host().unwrap().alias, "db");
        app.handle_key(KeyCode::Char('g'));
        app.tags_dialog.as_mut().unwrap().1 = "work".into();
        app.handle_key(KeyCode::Enter);
        assert_eq!(app.metadata.tags["db"], ["work"]);
        assert_eq!(app.selected_host().unwrap().alias, "web");
        assert_eq!(app.selected_host, 0);
        assert!(app.tags_dialog.is_none());
    }

    #[test]
    fn empty_group_navigation_never_connects_or_panics() {
        let mut app = app_fixture();
        assert!(!app.enter_group());
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_browse_item, 0);
        assert!(app.selected_host().is_none());
    }

    #[test]
    fn deleted_group_returns_to_group_list_but_search_does_not() {
        let mut app = grouped_fixture();
        app.enter_group();
        app.search_query = "not-found".into();
        app.clamp_selection();
        assert!(app.active_group.is_some());
        app.hosts.hosts.clear();
        app.clamp_selection();
        assert!(app.showing_groups());
        assert_eq!(app.selected_host, 0);
        assert_eq!(app.selected_group, 0);
    }

    #[test]
    fn metadata_load_failure_blocks_reconciliation() {
        let mut app = grouped_fixture();
        app.hosts.hosts.clear();
        app.reconcile_metadata();
        assert_eq!(app.metadata.tags.len(), 2);
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> InputAction {
        app.handle_event(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn direct_and_cyclic_tab_shortcuts_work_from_every_section() {
        let mut app = app_fixture();
        for section in NavSection::ALL {
            app.section = section;
            for number in 1..=4 {
                assert_eq!(
                    press(&mut app, KeyCode::F(number), KeyModifiers::NONE),
                    InputAction::None
                );
                assert_eq!(app.section, NavSection::ALL[usize::from(number - 1)]);
                press(
                    &mut app,
                    KeyCode::Char(char::from(b'0' + number)),
                    KeyModifiers::NONE,
                );
                assert_eq!(app.section, NavSection::ALL[usize::from(number - 1)]);
            }
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert_eq!(app.section, NavSection::Hosts);
            press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
            assert_eq!(app.section, NavSection::Snippets);
            press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
            assert_eq!(app.section, NavSection::Hosts);
            press(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
            assert_eq!(app.section, NavSection::Snippets);
        }
    }

    #[test]
    fn last_tab_shortcut_preserves_host_context() {
        let mut app = grouped_fixture();
        app.enter_group();
        app.selected_host = 1;
        app.search_query = "prod".into();
        let group = app.active_group.clone();
        press(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('`'), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Hosts);
        assert_eq!(app.active_group, group);
        assert_eq!(app.selected_host, 1);
        assert_eq!(app.search_query, "prod");
        press(&mut app, KeyCode::Char('`'), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Tunnels);
    }

    #[test]
    fn search_owns_characters_but_explicit_tab_shortcuts_still_work() {
        let mut app = grouped_fixture();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        for character in ['2', 'f', 't', '`', 'S'] {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(app.search_query, "2ft`S");
        assert_eq!(app.section, NavSection::Hosts);
        press(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Files);
        assert!(!app.searching);
        press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(app.search_query, "2ft`S");
        app.searching = true;
        press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(app.section, NavSection::Files);
        assert!(!app.searching);
    }

    #[test]
    fn form_and_tag_editor_own_input_and_block_tab_switching() {
        let mut app = grouped_fixture();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Hosts);
        assert_eq!(app.host_dialog.as_ref().unwrap().fields[0], "2");
        assert_eq!(app.host_dialog.as_ref().unwrap().selected_field, 1);
        assert!(!app.should_quit);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.enter_group();
        press(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(app.section, NavSection::Hosts);
        assert!(app.tags_dialog.as_ref().unwrap().1.ends_with('2'));
    }

    #[test]
    fn enter_only_requests_connection_for_a_selected_host() {
        let mut app = grouped_fixture();
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert!(app.active_group.is_some());
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::Connect
        );
        app.searching = true;
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert!(!app.searching);
        app.connection_dialog = Some(ConnectionDialog {
            title: "Error".into(),
            message: "Failed".into(),
            pending: false,
        });
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert!(app.connection_dialog.is_none());
        app.search_query = "missing".into();
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
    }

    #[test]
    fn arrow_keys_follow_group_grid_then_host_list_without_changing_tabs() {
        let mut app = grouped_fixture();
        app.viewport_width = 120;
        assert_eq!(group_columns(host_browser_width(app.viewport_width)), 2);
        for (key, expected) in [
            (KeyCode::Up, 0),
            (KeyCode::Left, 0),
            (KeyCode::Right, 1),
            (KeyCode::Up, 1),
            (KeyCode::Right, 1),
            (KeyCode::Down, 2),
            (KeyCode::Right, 2),
            (KeyCode::Left, 2),
            (KeyCode::Down, 3),
            (KeyCode::Left, 3),
            (KeyCode::Right, 3),
            (KeyCode::Down, 4),
            (KeyCode::Down, 5),
            (KeyCode::Down, 5),
            (KeyCode::Up, 4),
            (KeyCode::Up, 3),
            (KeyCode::Up, 2),
            (KeyCode::Up, 0),
        ] {
            press(&mut app, key, KeyModifiers::NONE);
            assert_eq!(app.selected_browse_item, expected, "{key:?}");
            assert_eq!(app.section, NavSection::Hosts);
        }
    }

    #[test]
    fn group_return_and_resize_keep_grid_navigation_consistent() {
        let mut app = grouped_fixture();
        app.viewport_width = 120;
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.selected_group(), Some(HostGroup::Tag("work".into())));
        app.enter_group();
        assert_eq!(app.selected_host().unwrap().alias, "web");
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Hosts);
        assert!(app.active_group.is_some());
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.selected_browse_item, 1);
        assert_eq!(app.selected_group(), Some(HostGroup::Tag("work".into())));
        app.viewport_width = 32;
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.selected_browse_item, 2);
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.selected_browse_item, 1);
        app.searching = true;
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.selected_browse_item, 0);
        assert_eq!(app.section, NavSection::Hosts);
        press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(app.selected_browse_item, 0);
    }
}
