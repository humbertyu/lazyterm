use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender, TryRecvError},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::hosts::{Host, HostCatalog};
use crate::metadata::{HostView, Metadata, parse_tags};
use crate::sftp::{
    self, SftpListing, SftpResult, TransferDirection, TransferEvent, TransferRequest,
};

pub const LOCAL_BREADCRUMB_SEPARATOR: &str = "  >  ";

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilePane {
    Local,
    Remote,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalEntryKind {
    Parent,
    Directory,
    File,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEntry {
    pub name: String,
    pub kind: LocalEntryKind,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFiles {
    pub path: PathBuf,
    pub entries: Vec<LocalEntry>,
    pub selected: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBreadcrumb {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBreadcrumbPart {
    pub label: String,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferStatus {
    Running,
    Completed,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferTask {
    pub id: u64,
    pub direction: TransferDirection,
    pub name: String,
    pub local_path: PathBuf,
    pub remote_path: String,
    pub transferred: u64,
    pub total: u64,
    pub status: TransferStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostPickerItem {
    Group(HostGroup),
    Host(String),
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
    pub host_context: Option<String>,
    pub files: FilesState,
    pub selected_file: usize,
    pub local_files: LocalFiles,
    pub file_filter: String,
    pub file_filtering: bool,
    pub show_hidden_files: bool,
    pub active_file_pane: FilePane,
    pub host_picker_open: bool,
    pub host_picker_group: Option<HostGroup>,
    pub host_picker_selection: usize,
    pub transfer_tasks: Vec<TransferTask>,
    next_transfer_id: u64,
    sftp_receiver: Option<Receiver<SftpResult>>,
    transfer_sender: Sender<TransferEvent>,
    transfer_receiver: Receiver<TransferEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesState {
    Idle,
    Loading {
        host_alias: String,
        path: String,
    },
    Loaded(SftpListing),
    Failed {
        host_alias: String,
        path: String,
        message: String,
    },
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
    LoadFiles { host_alias: String, path: String },
    OpenNativeSftp,
}

impl App {
    pub fn new() -> Self {
        let mut app = Self::with_catalog(HostCatalog::load(), Metadata::load());
        app.reconcile_metadata();
        app.refresh_local_files();
        app
    }

    pub(crate) fn with_catalog(hosts: HostCatalog, metadata: std::io::Result<Metadata>) -> Self {
        let notice = metadata
            .as_ref()
            .err()
            .map(|error| format!("Could not read host metadata: {error}"));
        let metadata_error = metadata.is_err();
        let metadata = metadata.unwrap_or_default();
        let show_hidden_files = metadata.show_hidden_files;
        let local_path = metadata.local_path.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default()
        });
        let (transfer_sender, transfer_receiver) = std::sync::mpsc::channel();
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
            metadata,
            metadata_error,
            active_group: None,
            selected_group: 0,
            tags_dialog: None,
            host_context: None,
            files: FilesState::Idle,
            selected_file: 0,
            local_files: LocalFiles {
                path: local_path,
                entries: Vec::new(),
                selected: 0,
                error: None,
            },
            file_filter: String::new(),
            file_filtering: false,
            show_hidden_files,
            active_file_pane: FilePane::Remote,
            host_picker_open: false,
            host_picker_group: None,
            host_picker_selection: 0,
            transfer_tasks: Vec::new(),
            next_transfer_id: 1,
            sftp_receiver: None,
            transfer_sender,
            transfer_receiver,
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

    pub fn context_host(&self) -> Option<&Host> {
        self.host_context
            .as_deref()
            .and_then(|alias| self.hosts.hosts.iter().find(|host| host.alias == alias))
    }

    pub fn start_sftp_listing(&mut self, host_alias: String, path: String) {
        self.files = FilesState::Loading {
            host_alias: host_alias.clone(),
            path: path.clone(),
        };
        self.active_file_pane = FilePane::Remote;
        self.notice = Some(format!("Loading remote files from {host_alias}..."));
        self.sftp_receiver = Some(sftp::spawn_list(host_alias, path));
    }

    pub fn refresh_local_files(&mut self) {
        let mut entries = Vec::new();
        let mut error = None;

        match fs::read_dir(&self.local_files.path) {
            Ok(directory) => {
                if self
                    .local_files
                    .path
                    .parent()
                    .is_some_and(|parent| parent != self.local_files.path)
                {
                    entries.push(LocalEntry {
                        name: "..".to_string(),
                        kind: LocalEntryKind::Parent,
                        size: None,
                    });
                }
                for item in directory {
                    let item = match item {
                        Ok(item) => item,
                        Err(read_error) => {
                            error.get_or_insert_with(|| read_error.to_string());
                            continue;
                        }
                    };
                    let metadata = match item.metadata() {
                        Ok(metadata) => metadata,
                        Err(read_error) => {
                            error.get_or_insert_with(|| read_error.to_string());
                            continue;
                        }
                    };
                    let name = item.file_name().to_string_lossy().into_owned();
                    if !self.show_hidden_files && name.starts_with('.') {
                        continue;
                    }
                    let kind = if metadata.is_dir() {
                        LocalEntryKind::Directory
                    } else if metadata.is_file() {
                        LocalEntryKind::File
                    } else {
                        LocalEntryKind::Other
                    };
                    entries.push(LocalEntry {
                        name,
                        kind,
                        size: metadata.is_file().then_some(metadata.len()),
                    });
                }
                let first_child = usize::from(
                    entries
                        .first()
                        .is_some_and(|entry| entry.kind == LocalEntryKind::Parent),
                );
                entries[first_child..].sort_by_key(|entry| {
                    (
                        entry.kind != LocalEntryKind::Directory,
                        entry.name.to_lowercase(),
                    )
                });
            }
            Err(read_error) => error = Some(read_error.to_string()),
        }

        self.local_files.entries = entries;
        self.local_files.error = error;
        self.local_files.selected = self
            .local_files
            .selected
            .min(self.visible_local_entries().len().saturating_sub(1));
    }

    pub fn visible_local_entries(&self) -> Vec<&LocalEntry> {
        let query = self.file_filter.to_lowercase();
        self.local_files
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == LocalEntryKind::Parent
                    || (self.show_hidden_files || !entry.name.starts_with('.'))
                        && (query.is_empty() || entry.name.to_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn visible_remote_entries(&self) -> Vec<&crate::sftp::SftpEntry> {
        let FilesState::Loaded(listing) = &self.files else {
            return Vec::new();
        };
        listing
            .entries
            .iter()
            .filter(|entry| self.show_hidden_files || !entry.name.starts_with('.'))
            .collect()
    }

    pub fn local_breadcrumbs(&self) -> Vec<LocalBreadcrumb> {
        let path = &self.local_files.path;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(home) = home
            && let Ok(relative) = path.strip_prefix(&home)
        {
            let mut breadcrumbs = vec![LocalBreadcrumb {
                label: "~".to_string(),
                path: home.clone(),
            }];
            let mut current = home;
            for component in relative.components() {
                let name = component.as_os_str().to_string_lossy().into_owned();
                current.push(&name);
                breadcrumbs.push(LocalBreadcrumb {
                    label: name,
                    path: current.clone(),
                });
            }
            return breadcrumbs;
        }

        if path.is_absolute() {
            let mut breadcrumbs = vec![LocalBreadcrumb {
                label: "/".to_string(),
                path: PathBuf::from("/"),
            }];
            let mut current = PathBuf::from("/");
            for component in path.components().skip(1) {
                let name = component.as_os_str().to_string_lossy().into_owned();
                current.push(&name);
                breadcrumbs.push(LocalBreadcrumb {
                    label: name,
                    path: current.clone(),
                });
            }
            return breadcrumbs;
        }

        vec![LocalBreadcrumb {
            label: ".".to_string(),
            path: path.clone(),
        }]
    }

    pub fn local_breadcrumb_parts(&self, width: u16) -> Vec<LocalBreadcrumbPart> {
        let breadcrumbs = self.local_breadcrumbs();
        let separator_width = LOCAL_BREADCRUMB_SEPARATOR.chars().count();
        let total_width = breadcrumbs
            .iter()
            .map(|breadcrumb| breadcrumb.label.chars().count())
            .sum::<usize>()
            + separator_width.saturating_mul(breadcrumbs.len().saturating_sub(1));
        if total_width <= usize::from(width) {
            return breadcrumbs
                .into_iter()
                .map(|breadcrumb| LocalBreadcrumbPart {
                    label: breadcrumb.label,
                    path: Some(breadcrumb.path),
                })
                .collect();
        }

        let mut tail = Vec::new();
        let Some(current) = breadcrumbs.last() else {
            return tail;
        };
        let mut used_width = current.label.chars().count();
        tail.push(LocalBreadcrumbPart {
            label: current.label.clone(),
            path: Some(current.path.clone()),
        });
        let mut omitted = false;
        for breadcrumb in breadcrumbs[..breadcrumbs.len() - 1].iter().rev() {
            let candidate_width = used_width + separator_width + breadcrumb.label.chars().count();
            if candidate_width > usize::from(width) {
                omitted = true;
                break;
            }
            used_width = candidate_width;
            tail.push(LocalBreadcrumbPart {
                label: breadcrumb.label.clone(),
                path: Some(breadcrumb.path.clone()),
            });
        }
        tail.reverse();
        if omitted && used_width + separator_width + "...".chars().count() <= usize::from(width) {
            tail.insert(
                0,
                LocalBreadcrumbPart {
                    label: "...".to_string(),
                    path: None,
                },
            );
        }
        tail
    }

    fn local_list_start_row(&self) -> u16 {
        5 + u16::from(self.file_filtering || !self.file_filter.is_empty())
    }

    pub fn transfer_panel_height(&self, area_height: u16) -> u16 {
        if self.transfer_tasks.is_empty() {
            0
        } else {
            (self.transfer_tasks.len().min(4) as u16 + 2).min(area_height.saturating_sub(6))
        }
    }

    fn local_breadcrumb_at(&self, column: u16, panel_x: u16, inner_width: u16) -> Option<PathBuf> {
        let mut cursor = usize::from(panel_x.saturating_add(1));
        let click = usize::from(column);
        let parts = self.local_breadcrumb_parts(inner_width);
        for (index, part) in parts.iter().enumerate() {
            let width = part.label.chars().count();
            if click >= cursor && click < cursor.saturating_add(width) {
                return part.path.clone();
            }
            cursor = cursor.saturating_add(width);
            if index + 1 < parts.len() {
                cursor = cursor.saturating_add(LOCAL_BREADCRUMB_SEPARATOR.chars().count());
            }
        }
        None
    }

    fn navigate_local_to(&mut self, path: PathBuf) {
        self.local_files.path = path;
        self.local_files.selected = 0;
        self.file_filter.clear();
        self.file_filtering = false;
        self.metadata.local_path = Some(self.local_files.path.clone());
        self.save_metadata();
        self.refresh_local_files();
    }

    fn move_local_selection(&mut self, key: KeyCode) {
        let count = self.visible_local_entries().len();
        if key == KeyCode::Down {
            self.local_files.selected =
                (self.local_files.selected + 1).min(count.saturating_sub(1));
        } else {
            self.local_files.selected = self.local_files.selected.saturating_sub(1);
        }
    }

    fn open_local_selection(&mut self) {
        let entries = self.visible_local_entries();
        let Some(entry) = entries.get(self.local_files.selected) else {
            return;
        };
        let path = match entry.kind {
            LocalEntryKind::Parent => self.local_files.path.parent().map(Path::to_path_buf),
            LocalEntryKind::Directory => Some(self.local_files.path.join(&entry.name)),
            LocalEntryKind::File | LocalEntryKind::Other => None,
        };
        if let Some(path) = path {
            self.navigate_local_to(path);
        }
    }

    fn open_remote_selection(&mut self) -> InputAction {
        if self.host_context.is_none() {
            self.open_host_picker();
        } else if let FilesState::Loaded(listing) = &self.files
            && let Some(entry) = self.visible_remote_entries().get(self.selected_file)
            && entry.kind == crate::sftp::SftpEntryKind::Directory
        {
            let path = if listing.path.is_empty() {
                entry.name.clone()
            } else {
                Path::new(&listing.path)
                    .join(&entry.name)
                    .to_string_lossy()
                    .into_owned()
            };
            return self.load_files(path);
        } else if matches!(self.files, FilesState::Failed { .. } | FilesState::Idle) {
            return self.load_files(self.current_files_path());
        }
        InputAction::None
    }

    fn go_to_local_parent(&mut self) {
        if let Some(parent) = self.local_files.path.parent() {
            self.navigate_local_to(parent.to_path_buf());
        }
    }

    fn go_to_remote_parent(&mut self) -> InputAction {
        let path = self.current_files_path();
        if !path.is_empty() {
            let parent = Path::new(&path)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            return self.load_files(parent);
        }
        InputAction::None
    }

    fn remote_child_path(directory: &str, name: &str) -> String {
        if directory.is_empty() {
            name.to_string()
        } else {
            Path::new(directory)
                .join(name)
                .to_string_lossy()
                .into_owned()
        }
    }

    fn queue_transfer(&mut self, request: TransferRequest) {
        let task = TransferTask {
            id: request.task_id,
            direction: request.direction,
            name: request.local_path.file_name().map_or_else(
                || request.remote_path.clone(),
                |name| name.to_string_lossy().into_owned(),
            ),
            local_path: request.local_path.clone(),
            remote_path: request.remote_path.clone(),
            transferred: 0,
            total: 0,
            status: TransferStatus::Running,
        };
        self.notice = Some(match request.direction {
            TransferDirection::Upload => format!("Uploading {}...", task.name),
            TransferDirection::Download => format!("Downloading {}...", task.name),
        });
        self.transfer_tasks.push(task);
        if self.transfer_tasks.len() > 20 {
            self.transfer_tasks.remove(0);
        }
        sftp::spawn_transfer(request, self.transfer_sender.clone());
    }

    fn prepare_upload(&mut self) -> Result<TransferRequest, String> {
        let host_alias = self
            .context_host()
            .map(|host| host.alias.clone())
            .ok_or_else(|| "Select a remote host before uploading.".to_string())?;
        let FilesState::Loaded(listing) = &self.files else {
            return Err("Load a remote directory before uploading.".to_string());
        };
        let entries = self.visible_local_entries();
        let entry = entries
            .get(self.local_files.selected)
            .ok_or_else(|| "Select a local file to upload.".to_string())?;
        if entry.kind != LocalEntryKind::File {
            return Err("Only files can be uploaded right now.".to_string());
        }
        let local_path = self.local_files.path.join(&entry.name);
        Ok(TransferRequest {
            task_id: self.next_transfer_id,
            host_alias,
            local_path,
            remote_path: Self::remote_child_path(&listing.path, &entry.name),
            direction: TransferDirection::Upload,
        })
    }

    fn prepare_download(&mut self) -> Result<TransferRequest, String> {
        let host_alias = self
            .context_host()
            .map(|host| host.alias.clone())
            .ok_or_else(|| "Select a remote host before downloading.".to_string())?;
        let FilesState::Loaded(listing) = &self.files else {
            return Err("Load a remote directory before downloading.".to_string());
        };
        let entries = self.visible_remote_entries();
        let entry = entries
            .get(self.selected_file)
            .ok_or_else(|| "Select a remote file to download.".to_string())?;
        if entry.kind != crate::sftp::SftpEntryKind::File {
            return Err("Only files can be downloaded right now.".to_string());
        }
        Ok(TransferRequest {
            task_id: self.next_transfer_id,
            host_alias,
            local_path: self.local_files.path.join(&entry.name),
            remote_path: Self::remote_child_path(&listing.path, &entry.name),
            direction: TransferDirection::Download,
        })
    }

    fn start_upload(&mut self) {
        match self.prepare_upload() {
            Ok(request) => {
                self.next_transfer_id = self.next_transfer_id.saturating_add(1);
                self.queue_transfer(request);
            }
            Err(message) => self.notice = Some(message),
        }
    }

    fn start_download(&mut self) {
        match self.prepare_download() {
            Ok(request) => {
                self.next_transfer_id = self.next_transfer_id.saturating_add(1);
                self.queue_transfer(request);
            }
            Err(message) => self.notice = Some(message),
        }
    }

    pub fn host_picker_items(&self) -> Vec<HostPickerItem> {
        if let Some(group) = &self.host_picker_group {
            return self
                .hosts
                .hosts
                .iter()
                .filter(|host| self.matches_group(host, group))
                .map(|host| HostPickerItem::Host(host.alias.clone()))
                .collect();
        }

        let mut groups = std::collections::BTreeSet::new();
        for host in &self.hosts.hosts {
            if self.tags(host).is_empty() {
                groups.insert(HostGroup::Untagged);
            } else {
                groups.extend(self.tags(host).iter().cloned().map(HostGroup::Tag));
            }
        }
        let mut items = groups
            .into_iter()
            .map(HostPickerItem::Group)
            .collect::<Vec<_>>();
        items.extend(
            self.hosts
                .hosts
                .iter()
                .map(|host| HostPickerItem::Host(host.alias.clone())),
        );
        items
    }

    pub fn host_group_count(&self, group: &HostGroup) -> usize {
        self.hosts
            .hosts
            .iter()
            .filter(|host| self.matches_group(host, group))
            .count()
    }

    fn open_host_picker(&mut self) {
        self.host_picker_open = true;
        self.host_picker_group = None;
        self.host_picker_selection = 0;
        self.active_file_pane = FilePane::Remote;
    }

    fn move_host_picker_selection(&mut self, key: KeyCode) {
        let count = self.host_picker_items().len();
        if key == KeyCode::Down {
            self.host_picker_selection =
                (self.host_picker_selection + 1).min(count.saturating_sub(1));
        } else {
            self.host_picker_selection = self.host_picker_selection.saturating_sub(1);
        }
    }

    fn choose_file_host(&mut self, alias: String) -> InputAction {
        self.set_host_context(Some(alias.clone()));
        self.host_picker_open = false;
        self.host_picker_group = None;
        self.host_picker_selection = 0;
        let path = self
            .metadata
            .remote_paths
            .get(&alias)
            .cloned()
            .unwrap_or_default();
        self.files = FilesState::Loading {
            host_alias: alias.clone(),
            path: path.clone(),
        };
        self.notice = Some(format!("Loading remote files from {alias}..."));
        InputAction::LoadFiles {
            host_alias: alias,
            path,
        }
    }

    fn handle_files_key(&mut self, key: KeyCode) -> InputAction {
        if self.host_picker_open {
            match key {
                KeyCode::Esc | KeyCode::Backspace => {
                    if self.host_picker_group.is_some() {
                        self.host_picker_group = None;
                        self.host_picker_selection = 0;
                    } else {
                        self.host_picker_open = false;
                    }
                }
                KeyCode::Up | KeyCode::Down => self.move_host_picker_selection(key),
                KeyCode::Enter => {
                    match self
                        .host_picker_items()
                        .get(self.host_picker_selection)
                        .cloned()
                    {
                        Some(HostPickerItem::Group(group)) => {
                            self.host_picker_group = Some(group);
                            self.host_picker_selection = 0;
                        }
                        Some(HostPickerItem::Host(alias)) => {
                            return self.choose_file_host(alias);
                        }
                        None => {}
                    }
                }
                _ => {}
            }
            return InputAction::None;
        }

        if self.active_file_pane == FilePane::Local
            && key == KeyCode::Esc
            && !self.file_filter.is_empty()
        {
            self.file_filter.clear();
            self.local_files.selected = 0;
            return InputAction::None;
        }

        match key {
            KeyCode::Tab | KeyCode::BackTab => {
                self.active_file_pane = match self.active_file_pane {
                    FilePane::Local => FilePane::Remote,
                    FilePane::Remote => FilePane::Local,
                };
            }
            KeyCode::Up | KeyCode::Down => match self.active_file_pane {
                FilePane::Local => self.move_local_selection(key),
                FilePane::Remote => self.move_file_selection(key),
            },
            KeyCode::Left => match self.active_file_pane {
                FilePane::Local => self.go_to_local_parent(),
                FilePane::Remote => return self.go_to_remote_parent(),
            },
            KeyCode::Right => match self.active_file_pane {
                FilePane::Local => self.open_local_selection(),
                FilePane::Remote => return self.open_remote_selection(),
            },
            KeyCode::Enter => match self.active_file_pane {
                FilePane::Local => self.open_local_selection(),
                FilePane::Remote => return self.open_remote_selection(),
            },
            KeyCode::Backspace => match self.active_file_pane {
                FilePane::Local => self.go_to_local_parent(),
                FilePane::Remote => return self.go_to_remote_parent(),
            },
            KeyCode::Char('s') if self.active_file_pane == FilePane::Remote => {
                self.open_host_picker();
            }
            KeyCode::Char('s') => {}
            KeyCode::Char('u') if self.active_file_pane == FilePane::Local => {
                self.start_upload();
            }
            KeyCode::Char('d') if self.active_file_pane == FilePane::Remote => {
                self.start_download();
            }
            KeyCode::Char('/') if self.active_file_pane == FilePane::Local => {
                self.file_filtering = true;
                self.file_filter.clear();
                self.local_files.selected = 0;
            }
            KeyCode::Char('r') => match self.active_file_pane {
                FilePane::Local => self.refresh_local_files(),
                FilePane::Remote if self.context_host().is_some() => {
                    return self.load_files(self.current_files_path());
                }
                FilePane::Remote => self.open_host_picker(),
            },
            KeyCode::Char('h')
                if self.active_file_pane == FilePane::Local
                    || self.active_file_pane == FilePane::Remote =>
            {
                self.show_hidden_files = !self.show_hidden_files;
                self.metadata.show_hidden_files = self.show_hidden_files;
                self.save_metadata();
                self.refresh_local_files();
                self.selected_file = self
                    .selected_file
                    .min(self.visible_remote_entries().len().saturating_sub(1));
            }
            KeyCode::Char('o') if self.active_file_pane == FilePane::Remote => {
                if self.context_host().is_some() {
                    return InputAction::OpenNativeSftp;
                }
                self.open_host_picker();
            }
            _ => self.handle_key(key),
        }
        InputAction::None
    }

    pub fn handle_mouse(
        &mut self,
        column: u16,
        row: u16,
        viewport_width: u16,
        viewport_height: u16,
    ) -> InputAction {
        if self.section != NavSection::Files || row < 3 || row >= viewport_height.saturating_sub(3)
        {
            return InputAction::None;
        }
        let files_end = viewport_height.saturating_sub(3);
        let files_height = files_end.saturating_sub(3);
        let pane_end = files_end.saturating_sub(self.transfer_panel_height(files_height));
        if row >= pane_end {
            return InputAction::None;
        }
        if self.file_filtering {
            return InputAction::None;
        }

        let local_width = viewport_width / 2;
        self.active_file_pane = if column < local_width {
            FilePane::Local
        } else {
            FilePane::Remote
        };
        if row == 3 {
            return InputAction::None;
        }

        if self.active_file_pane == FilePane::Remote && self.host_picker_open {
            self.select_host_picker_row(row);
        } else if self.active_file_pane == FilePane::Local {
            if row == 4 {
                if let Some(path) =
                    self.local_breadcrumb_at(column, 0, local_width.saturating_sub(2))
                {
                    self.navigate_local_to(path);
                }
                return InputAction::None;
            }
            let index = usize::from(row.saturating_sub(self.local_list_start_row()));
            if index < self.visible_local_entries().len() {
                self.local_files.selected = index;
            }
        } else if self.host_context.is_none() {
            self.open_host_picker();
        } else if matches!(self.files, FilesState::Loaded(_)) {
            let index = usize::from(row.saturating_sub(4));
            if index < self.visible_remote_entries().len() {
                self.selected_file = index;
            }
        }
        InputAction::None
    }

    fn select_host_picker_row(&mut self, row: u16) {
        let list_row = usize::from(row.saturating_sub(4));
        let items = self.host_picker_items();
        let group_count = items
            .iter()
            .take_while(|item| matches!(item, HostPickerItem::Group(_)))
            .count();
        let selection = if group_count > 0 {
            if list_row == 0 || list_row == group_count + 1 {
                return;
            }
            if list_row <= group_count {
                list_row - 1
            } else {
                let Some(selection) = list_row.checked_sub(2) else {
                    return;
                };
                selection
            }
        } else {
            let Some(selection) = list_row.checked_sub(1) else {
                return;
            };
            selection
        };
        if selection < items.len() {
            self.host_picker_selection = selection;
        }
    }

    pub fn poll_sftp(&mut self) {
        let Some(receiver) = &self.sftp_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(listing)) if self.host_context.as_deref() == Some(&listing.host_alias) => {
                self.notice = Some(format!(
                    "Loaded {} entries from {}.",
                    listing.entries.len(),
                    listing.host_alias
                ));
                self.files = FilesState::Loaded(listing);
                self.selected_file = 0;
                if let FilesState::Loaded(listing) = &self.files {
                    self.metadata
                        .remote_paths
                        .insert(listing.host_alias.clone(), listing.path.clone());
                    self.save_metadata();
                }
                self.sftp_receiver = None;
            }
            Ok(Ok(_)) => self.sftp_receiver = None,
            Ok(Err(message)) => {
                let (host_alias, path) = match &self.files {
                    FilesState::Loading { host_alias, path } => (host_alias.clone(), path.clone()),
                    _ => (
                        self.host_context
                            .clone()
                            .unwrap_or_else(|| "selected host".to_string()),
                        String::new(),
                    ),
                };
                self.notice = Some(message.clone());
                self.files = FilesState::Failed {
                    host_alias,
                    path,
                    message,
                };
                self.sftp_receiver = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let host_alias = self
                    .host_context
                    .clone()
                    .unwrap_or_else(|| "selected host".to_string());
                let path = self.current_files_path();
                self.files = FilesState::Failed {
                    host_alias,
                    path,
                    message: "SFTP worker stopped unexpectedly.".to_string(),
                };
                self.sftp_receiver = None;
            }
        }
    }

    pub fn poll_transfers(&mut self) {
        loop {
            let event = match self.transfer_receiver.try_recv() {
                Ok(event) => event,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            };
            match event {
                TransferEvent::Progress {
                    task_id,
                    transferred,
                    total,
                } => {
                    if let Some(task) = self
                        .transfer_tasks
                        .iter_mut()
                        .find(|task| task.id == task_id)
                    {
                        task.transferred = transferred;
                        task.total = total;
                    }
                }
                TransferEvent::Finished { task_id } => {
                    let Some(task) = self
                        .transfer_tasks
                        .iter_mut()
                        .find(|task| task.id == task_id)
                    else {
                        continue;
                    };
                    task.status = TransferStatus::Completed;
                    let direction = task.direction;
                    let name = task.name.clone();
                    let remote_path = task.remote_path.clone();
                    let total = task.total;
                    self.notice = Some(format!("Transfer completed: {name}."));
                    if direction == TransferDirection::Download {
                        self.refresh_local_files();
                    } else if let FilesState::Loaded(listing) = &mut self.files {
                        let remote_name = Path::new(&remote_path)
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                        if let Some(entry) = listing
                            .entries
                            .iter_mut()
                            .find(|entry| entry.name == remote_name)
                        {
                            entry.kind = crate::sftp::SftpEntryKind::File;
                            entry.size = Some(total);
                        } else if !remote_name.is_empty() {
                            listing.entries.push(crate::sftp::SftpEntry {
                                name: remote_name,
                                kind: crate::sftp::SftpEntryKind::File,
                                size: Some(total),
                            });
                            listing.entries.sort_by_key(|entry| {
                                (
                                    entry.kind != crate::sftp::SftpEntryKind::Directory,
                                    entry.name.to_lowercase(),
                                )
                            });
                        }
                    }
                }
                TransferEvent::Failed { task_id, message } => {
                    if let Some(task) = self
                        .transfer_tasks
                        .iter_mut()
                        .find(|task| task.id == task_id)
                    {
                        task.status = TransferStatus::Failed(message.clone());
                        self.notice = Some(format!("Transfer failed: {message}"));
                    }
                }
            }
        }
    }

    fn load_files(&mut self, path: String) -> InputAction {
        let Some(host) = self.context_host() else {
            self.notice = Some("Select a host from the Files remote pane first.".to_string());
            return InputAction::None;
        };
        let host_alias = host.alias.clone();
        self.files = FilesState::Loading {
            host_alias: host_alias.clone(),
            path: path.clone(),
        };
        self.notice = Some(format!("Loading remote files from {host_alias}..."));
        InputAction::LoadFiles { host_alias, path }
    }

    fn current_files_path(&self) -> String {
        match &self.files {
            FilesState::Loading { path, .. } | FilesState::Failed { path, .. } => path.clone(),
            FilesState::Loaded(listing) => listing.path.clone(),
            FilesState::Idle => String::new(),
        }
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
        } else if self.file_filtering && !modal {
            self.handle_file_filter_key(key.code);
        } else if modal {
            self.handle_key(key.code);
        } else if self.section == NavSection::Hosts && key.code == KeyCode::Enter {
            if !self.enter_group()
                && let Some(host) = self.selected_host().cloned()
            {
                self.set_host_context(Some(host.alias));
                return InputAction::Connect;
            }
        } else if self.section == NavSection::Files {
            return self.handle_files_key(key.code);
        } else {
            self.handle_key(key.code);
        }
        InputAction::None
    }

    fn move_file_selection(&mut self, key: KeyCode) {
        let count = self.visible_remote_entries().len();
        if key == KeyCode::Down {
            self.selected_file = (self.selected_file + 1).min(count.saturating_sub(1));
        } else {
            self.selected_file = self.selected_file.saturating_sub(1);
        }
    }

    fn switch_section(&mut self, section: NavSection) {
        if section != self.section {
            self.previous_section = self.section;
            self.section = section;
            self.searching = false;
            self.file_filtering = false;
        }
    }

    fn set_host_context(&mut self, alias: Option<String>) {
        if self.host_context != alias {
            self.host_context = alias;
            self.files = FilesState::Idle;
            self.selected_file = 0;
            self.sftp_receiver = None;
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
                            self.metadata.remote_paths.remove(&alias);
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
                            if self.host_context.as_deref() == Some(original) {
                                self.set_host_context(Some(host.alias.clone()));
                            }
                            if let Some(tags) = self.metadata.tags.remove(original) {
                                self.metadata.tags.insert(host.alias.clone(), tags);
                            }
                            if let Some(path) = self.metadata.remote_paths.remove(original) {
                                self.metadata.remote_paths.insert(host.alias.clone(), path);
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

    fn handle_file_filter_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Enter => self.file_filtering = false,
            KeyCode::Up | KeyCode::Down => self.move_local_selection(key),
            KeyCode::Left => {
                self.go_to_local_parent();
            }
            KeyCode::Right => {
                self.file_filtering = false;
                self.open_local_selection();
            }
            KeyCode::Esc => {
                self.file_filtering = false;
                self.file_filter.clear();
                self.local_files.selected = 0;
            }
            KeyCode::Backspace => {
                self.file_filter.pop();
                self.local_files.selected = 0;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.file_filtering = false;
                self.active_file_pane = match self.active_file_pane {
                    FilePane::Local => FilePane::Remote,
                    FilePane::Remote => FilePane::Local,
                };
            }
            KeyCode::Char(character) if !character.is_control() => {
                self.file_filter.push(character);
                self.local_files.selected = 0;
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
        if let Some(alias) = &self.host_context
            && !self.hosts.hosts.iter().any(|host| &host.alias == alias)
            && self.hosts.error.is_none()
        {
            self.set_host_context(None);
            self.notice = Some("The selected host no longer exists.".to_string());
        }
        match Metadata::load() {
            Ok(metadata) => {
                self.show_hidden_files = metadata.show_hidden_files;
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
            && let Some(alias) = &self.host_context
            && !self.hosts.hosts.iter().any(|host| &host.alias == alias)
        {
            self.set_host_context(None);
        }
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
    fn files_open_host_picker_instead_of_using_the_hosts_tab_selection() {
        let mut app = grouped_fixture();
        app.selected_browse_item = 3;
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert!(app.host_context.is_none());
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert!(app.host_picker_open);
        assert_eq!(
            app.host_picker_items()[0],
            HostPickerItem::Group(HostGroup::Tag("prod".into()))
        );
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.host_picker_group, Some(HostGroup::Tag("prod".into())));
        assert_eq!(
            app.host_picker_items(),
            [
                HostPickerItem::Host("web".into()),
                HostPickerItem::Host("db".into())
            ]
        );
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::LoadFiles {
                host_alias: "web".into(),
                path: String::new(),
            }
        );
        assert_eq!(app.host_context.as_deref(), Some("web"));
        assert!(!app.host_picker_open);
        assert!(matches!(app.files, FilesState::Loading { .. }));
        app.hosts.hosts.clear();
        app.hosts.error = None;
        app.clamp_selection();
        assert!(app.host_context.is_none());
    }

    #[test]
    fn files_restore_the_last_remote_path_for_each_host() {
        let mut app = grouped_fixture();
        app.metadata
            .remote_paths
            .insert("web".into(), "projects".into());
        app.section = NavSection::Files;

        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::LoadFiles {
                host_alias: "web".into(),
                path: "projects".into(),
            }
        );
        assert!(matches!(
            app.files,
            FilesState::Loading { ref path, .. } if path == "projects"
        ));
    }

    #[test]
    fn files_hide_dot_entries_by_default_and_toggle_them_with_h() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lazyterm-hidden-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("visible.txt"), "visible").unwrap();
        std::fs::write(root.join(".hidden.txt"), "hidden").unwrap();

        let mut app = app_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Local;
        app.local_files.path = root.clone();
        app.refresh_local_files();
        assert!(
            app.visible_local_entries().iter().all(|entry| {
                entry.kind == LocalEntryKind::Parent || !entry.name.starts_with('.')
            })
        );

        press(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        assert!(
            app.visible_local_entries()
                .iter()
                .any(|entry| entry.name == ".hidden.txt")
        );
        assert!(app.show_hidden_files);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_hidden_entries_follow_the_same_setting() {
        let mut app = grouped_fixture();
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: String::new(),
            entries: vec![
                crate::sftp::SftpEntry {
                    name: ".ssh".into(),
                    kind: crate::sftp::SftpEntryKind::Directory,
                    size: None,
                },
                crate::sftp::SftpEntry {
                    name: "README.md".into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    size: Some(1),
                },
            ],
        });
        assert_eq!(app.visible_remote_entries().len(), 1);
        app.show_hidden_files = true;
        assert_eq!(app.visible_remote_entries().len(), 2);
    }

    #[test]
    fn files_tab_switches_panes_and_host_picker_is_remote_only() {
        let mut app = grouped_fixture();
        app.section = NavSection::Files;
        assert_eq!(app.active_file_pane, FilePane::Remote);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.active_file_pane, FilePane::Local);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(!app.host_picker_open);
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.active_file_pane, FilePane::Remote);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.host_picker_open);
    }

    #[test]
    fn files_mouse_clicks_switch_panes_and_select_entries() {
        let mut app = grouped_fixture();
        app.section = NavSection::Files;
        app.local_files.entries = vec![
            LocalEntry {
                name: "first".into(),
                kind: LocalEntryKind::File,
                size: None,
            },
            LocalEntry {
                name: "second".into(),
                kind: LocalEntryKind::File,
                size: None,
            },
        ];

        app.handle_mouse(5, 4, 100, 24);
        assert_eq!(app.active_file_pane, FilePane::Local);
        assert_eq!(app.local_files.selected, 0);

        app.handle_mouse(5, 6, 100, 24);
        assert_eq!(app.active_file_pane, FilePane::Local);
        assert_eq!(app.local_files.selected, 1);

        app.handle_mouse(75, 5, 100, 24);
        assert_eq!(app.active_file_pane, FilePane::Remote);
        assert!(app.host_picker_open);
    }

    #[test]
    fn local_breadcrumb_click_jumps_to_an_ancestor_directory() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lazyterm-breadcrumb-{}-{unique}",
            std::process::id()
        ));
        let nested = root.join("one").join("two");
        std::fs::create_dir_all(&nested).unwrap();

        let mut app = app_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Local;
        app.local_files.path = nested;
        app.refresh_local_files();

        let parts = app.local_breadcrumb_parts(98);
        let target_index = parts.iter().position(|part| part.label == "one").unwrap();
        let mut column = 1usize;
        for part in parts.iter().take(target_index) {
            column += part.label.chars().count() + LOCAL_BREADCRUMB_SEPARATOR.chars().count();
        }
        column += parts[target_index].label.chars().count() / 2;
        app.handle_mouse(column as u16, 4, 200, 24);

        assert_eq!(app.local_files.path, root.join("one"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_file_filter_matches_names_and_preserves_parent_entry() {
        let mut app = app_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Local;
        app.local_files.entries = vec![
            LocalEntry {
                name: "..".into(),
                kind: LocalEntryKind::Parent,
                size: None,
            },
            LocalEntry {
                name: "documents".into(),
                kind: LocalEntryKind::Directory,
                size: None,
            },
            LocalEntry {
                name: "notes.txt".into(),
                kind: LocalEntryKind::File,
                size: None,
            },
        ];

        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        for character in "note".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert!(app.file_filtering);
        assert_eq!(
            app.visible_local_entries()
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["..", "notes.txt"]
        );

        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.local_files.selected, 1);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.file_filtering);
        assert_eq!(app.file_filter, "note");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.file_filter.is_empty());
    }

    #[test]
    fn files_mouse_click_selects_remote_entry_without_opening_it() {
        let mut app = grouped_fixture();
        app.section = NavSection::Files;
        app.host_context = Some("web".into());
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: String::new(),
            entries: vec![
                crate::sftp::SftpEntry {
                    name: "docs".into(),
                    kind: crate::sftp::SftpEntryKind::Directory,
                    size: None,
                },
                crate::sftp::SftpEntry {
                    name: "README.md".into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    size: Some(12),
                },
            ],
        });

        app.handle_mouse(75, 5, 100, 24);
        assert_eq!(app.active_file_pane, FilePane::Remote);
        assert_eq!(app.selected_file, 1);
        assert!(matches!(
            &app.files,
            FilesState::Loaded(crate::sftp::SftpListing { path, .. }) if path.is_empty()
        ));
    }

    #[test]
    fn files_can_enter_directories_and_return_to_the_parent() {
        let mut app = grouped_fixture();
        app.host_context = Some("web".into());
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Remote;
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: String::new(),
            entries: vec![crate::sftp::SftpEntry {
                name: "docs".into(),
                kind: crate::sftp::SftpEntryKind::Directory,
                size: None,
            }],
        });
        assert_eq!(
            press(&mut app, KeyCode::Right, KeyModifiers::NONE),
            InputAction::LoadFiles {
                host_alias: "web".into(),
                path: "docs".into(),
            }
        );
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: "docs".into(),
            entries: Vec::new(),
        });
        assert_eq!(
            press(&mut app, KeyCode::Left, KeyModifiers::NONE),
            InputAction::LoadFiles {
                host_alias: "web".into(),
                path: String::new(),
            }
        );
    }

    #[test]
    fn sftp_results_update_the_ui_state_without_network_access() {
        let mut app = grouped_fixture();
        app.host_context = Some("web".into());
        app.files = FilesState::Loading {
            host_alias: "web".into(),
            path: String::new(),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        app.sftp_receiver = Some(receiver);
        sender
            .send(Ok(crate::sftp::SftpListing {
                host_alias: "web".into(),
                path: String::new(),
                entries: vec![crate::sftp::SftpEntry {
                    name: "docs".into(),
                    kind: crate::sftp::SftpEntryKind::Directory,
                    size: None,
                }],
            }))
            .unwrap();

        app.poll_sftp();

        let FilesState::Loaded(listing) = &app.files else {
            panic!("listing should be visible after its worker result arrives");
        };
        assert_eq!(listing.entries[0].name, "docs");
        assert!(app.sftp_receiver.is_none());
    }

    #[test]
    fn transfer_requests_use_the_selected_file_and_reject_directories() {
        let mut app = grouped_fixture();
        app.host_context = Some("web".into());
        app.local_files.path = PathBuf::from("/tmp");
        app.local_files.entries = vec![
            LocalEntry {
                name: "folder".into(),
                kind: LocalEntryKind::Directory,
                size: None,
            },
            LocalEntry {
                name: "notes.txt".into(),
                kind: LocalEntryKind::File,
                size: Some(12),
            },
        ];
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: "incoming".into(),
            entries: vec![
                crate::sftp::SftpEntry {
                    name: "remote.txt".into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    size: Some(20),
                },
                crate::sftp::SftpEntry {
                    name: "remote-folder".into(),
                    kind: crate::sftp::SftpEntryKind::Directory,
                    size: None,
                },
            ],
        });

        app.local_files.selected = 1;
        let upload = app.prepare_upload().expect("file should be uploadable");
        assert_eq!(upload.direction, TransferDirection::Upload);
        assert_eq!(upload.local_path, PathBuf::from("/tmp/notes.txt"));
        assert_eq!(upload.remote_path, "incoming/notes.txt");

        app.local_files.selected = 0;
        assert_eq!(
            app.prepare_upload().unwrap_err(),
            "Only files can be uploaded right now."
        );

        app.selected_file = 0;
        let download = app
            .prepare_download()
            .expect("remote file should be downloadable");
        assert_eq!(download.direction, TransferDirection::Download);
        assert_eq!(download.local_path, PathBuf::from("/tmp/remote.txt"));
        assert_eq!(download.remote_path, "incoming/remote.txt");

        app.selected_file = 1;
        assert_eq!(
            app.prepare_download().unwrap_err(),
            "Only files can be downloaded right now."
        );
    }

    #[test]
    fn transfer_events_update_progress_completion_and_failure() {
        let mut app = grouped_fixture();
        app.host_context = Some("web".into());
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: String::new(),
            entries: vec![crate::sftp::SftpEntry {
                name: "same.txt".into(),
                kind: crate::sftp::SftpEntryKind::File,
                size: Some(1),
            }],
        });
        app.transfer_tasks.push(TransferTask {
            id: 7,
            direction: TransferDirection::Upload,
            name: "same.txt".into(),
            local_path: PathBuf::from("/tmp/same.txt"),
            remote_path: "same.txt".into(),
            transferred: 0,
            total: 0,
            status: TransferStatus::Running,
        });
        app.transfer_sender
            .send(TransferEvent::Progress {
                task_id: 7,
                transferred: 4,
                total: 4,
            })
            .unwrap();
        app.transfer_sender
            .send(TransferEvent::Finished { task_id: 7 })
            .unwrap();
        app.poll_transfers();

        assert_eq!(app.transfer_tasks[0].transferred, 4);
        assert_eq!(app.transfer_tasks[0].total, 4);
        assert_eq!(app.transfer_tasks[0].status, TransferStatus::Completed);
        let FilesState::Loaded(listing) = &app.files else {
            panic!("remote listing should remain loaded");
        };
        assert_eq!(listing.entries[0].size, Some(4));

        app.transfer_tasks.push(TransferTask {
            id: 8,
            direction: TransferDirection::Download,
            name: "missing.txt".into(),
            local_path: PathBuf::from("/tmp/missing.txt"),
            remote_path: "missing.txt".into(),
            transferred: 0,
            total: 0,
            status: TransferStatus::Running,
        });
        app.transfer_sender
            .send(TransferEvent::Failed {
                task_id: 8,
                message: "permission denied".into(),
            })
            .unwrap();
        app.poll_transfers();
        assert_eq!(
            app.transfer_tasks[1].status,
            TransferStatus::Failed("permission denied".into())
        );
    }

    #[test]
    fn transfer_panel_mouse_area_does_not_change_file_selection() {
        let mut app = grouped_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Remote;
        app.local_files.selected = 1;
        app.transfer_tasks.push(TransferTask {
            id: 1,
            direction: TransferDirection::Upload,
            name: "notes.txt".into(),
            local_path: PathBuf::from("/tmp/notes.txt"),
            remote_path: "notes.txt".into(),
            transferred: 5,
            total: 10,
            status: TransferStatus::Running,
        });

        app.handle_mouse(5, 19, 100, 24);

        assert_eq!(app.active_file_pane, FilePane::Remote);
        assert_eq!(app.local_files.selected, 1);
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

    #[test]
    fn local_directory_browser_sorts_folders_and_supports_parent_navigation() {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lazyterm-local-{}-{unique}", std::process::id()));
        fs::create_dir_all(root.join("z-folder")).unwrap();
        fs::create_dir_all(root.join("a-folder")).unwrap();
        fs::write(root.join("notes.txt"), b"hello").unwrap();

        let mut app = app_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Local;
        app.local_files.path = root.clone();
        app.refresh_local_files();

        assert_eq!(app.local_files.entries[0].kind, LocalEntryKind::Parent);
        assert_eq!(app.local_files.entries[1].name, "a-folder");
        assert_eq!(app.local_files.entries[2].name, "z-folder");
        let file = app
            .local_files
            .entries
            .iter()
            .find(|entry| entry.name == "notes.txt")
            .unwrap();
        assert_eq!(file.size, Some(5));

        app.local_files.selected = 1;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.local_files.path, root.join("a-folder"));
        assert_eq!(app.local_files.entries[0].kind, LocalEntryKind::Parent);

        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.local_files.path, root);
        fs::remove_dir_all(root).unwrap();
    }
}
