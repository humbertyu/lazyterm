use std::io::Read;
use std::process::{Child, ExitStatus};
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::hosts::{Host, HostCatalog};
use crate::metadata::{
    HostView, Metadata, SnippetDefinition, SnippetPackage, TunnelDefinition, TunnelKind,
    TunnelLifecycle, parse_tags,
};
use crate::runtime::{self, RuntimeEntry, RuntimeEvent, RuntimeFilter, RuntimeTarget};
use crate::sftp::{
    self, Mutation, MutationEvent, MutationRequest, SftpListing, SftpResult, TransferDirection,
    TransferEvent, TransferRequest,
};
use crate::tunnel;

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
pub enum FileDialog {
    Rename {
        pane: FilePane,
        old_name: String,
        value: String,
    },
    Delete {
        pane: FilePane,
        name: String,
        is_dir: bool,
    },
    CreateDir {
        pane: FilePane,
        value: String,
    },
    TransferConflict {
        request: TransferRequest,
    },
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
    Queued,
    Running,
    Completed,
    Cancelled,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferTask {
    pub id: u64,
    pub host_alias: String,
    pub direction: TransferDirection,
    pub name: String,
    pub local_path: PathBuf,
    pub remote_path: String,
    pub is_dir: bool,
    pub transferred: u64,
    pub total: u64,
    pub status: TransferStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelStatus {
    Stopped,
    Starting,
    Running,
    Failed(String),
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
    Runtime,
    Projects,
}

impl NavSection {
    pub const ALL: [Self; 6] = [
        Self::Hosts,
        Self::Files,
        Self::Tunnels,
        Self::Snippets,
        Self::Runtime,
        Self::Projects,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Hosts => "Hosts",
            Self::Files => "Files",
            Self::Tunnels => "Tunnels",
            Self::Snippets => "Snippets",
            Self::Runtime => "Runtime",
            Self::Projects => "Projects",
        }
    }
}

pub struct App {
    pub workspace: crate::project_app::WorkspaceState,
    pub project_service: crate::projects::ProjectService,
    pub section: NavSection,
    pub previous_section: NavSection,
    pub hosts: HostCatalog,
    pub selected_host: usize,
    pub selected_browse_item: usize,
    pub viewport_width: u16,
    pub viewport_height: u16,
    pub search_query: String,
    pub searching: bool,
    pub should_quit: bool,
    pub notice: Option<String>,
    pub connection_dialog: Option<ConnectionDialog>,
    pub host_dialog: Option<HostDialog>,
    pub tunnel_dialog: Option<TunnelDialog>,
    pub snippet_dialog: Option<SnippetDialog>,
    pub package_dialog: Option<PackageDialog>,
    pub tunnel_host_picker_open: bool,
    pub snippet_host_picker_open: bool,
    pub snippet_host_picker_group: Option<HostGroup>,
    pub snippet_host_picker_selection: usize,
    pub snippet_package_open: bool,
    pub selected_package: usize,
    pub selected_snippet: usize,
    pub snippet_concurrency: usize,
    pub selected_snippet_run: usize,
    pub snippet_runs: Vec<SnippetRun>,
    pub snippet_confirmation: Option<PendingSnippetRun>,
    pub snippet_delete_confirmation: Option<SnippetDeleteTarget>,
    pub snippet_execution_view: bool,
    pub snippet_output_scroll: u16,
    pub runtime_target: RuntimeTarget,
    pub runtime_entries: Vec<RuntimeEntry>,
    pub runtime_filter: RuntimeFilter,
    pub selected_runtime: usize,
    pub runtime_list_offset: usize,
    runtime_views: std::collections::HashMap<RuntimeTarget, runtime::RuntimeView>,
    runtime_request: u64,
    pub runtime_loading: bool,
    pub runtime_error: Option<String>,
    pub runtime_host_picker_open: bool,
    pub runtime_host_picker_group: Option<HostGroup>,
    pub runtime_host_picker_selection: usize,
    pub runtime_kill_confirmation: Option<RuntimeEntry>,
    pub selected_tunnel: usize,
    pub tunnel_statuses: std::collections::HashMap<u64, TunnelStatus>,
    pub metadata: Metadata,
    pub metadata_error: bool,
    #[cfg(test)]
    pub(crate) metadata_save_path: Option<PathBuf>,
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
    pub selected_transfer: usize,
    transfer_queue: VecDeque<TransferRequest>,
    active_transfer_id: Option<u64>,
    transfer_cancellation: std::collections::HashMap<u64, Arc<AtomicBool>>,
    pub file_dialog: Option<FileDialog>,
    next_transfer_id: u64,
    next_mutation_id: u64,
    sftp_receiver: Option<Receiver<SftpResult>>,
    transfer_sender: Sender<TransferEvent>,
    transfer_receiver: Receiver<TransferEvent>,
    mutation_sender: Sender<MutationEvent>,
    mutation_receiver: Receiver<MutationEvent>,
    mutation_hosts: std::collections::HashMap<u64, String>,
    tunnel_sender: Sender<tunnel::TunnelEvent>,
    tunnel_receiver: Receiver<tunnel::TunnelEvent>,
    runtime_sender: Sender<RuntimeEvent>,
    runtime_receiver: Receiver<RuntimeEvent>,
    tunnel_processes: std::collections::HashMap<u64, Child>,
    snippet_processes: std::collections::HashMap<u64, SnippetProcess>,
    next_snippet_run_id: u64,
    tunnel_errors: std::collections::HashMap<u64, String>,
    tunnel_retries: std::collections::HashMap<u64, TunnelRetry>,
}

struct TunnelRetry {
    attempts: u8,
    next_attempt: Instant,
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

pub struct TunnelDialog {
    pub fields: [String; 8],
    pub kind: TunnelKind,
    pub editing_id: Option<u64>,
    pub selected_field: usize,
    pub deleting: bool,
}

pub struct SnippetDialog {
    pub fields: [String; 4],
    pub package_id: u64,
    pub editing_id: Option<u64>,
    pub selected_field: usize,
}

pub struct PackageDialog {
    pub fields: [String; 2],
    pub editing_id: Option<u64>,
    pub selected_field: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnippetRunStatus {
    Queued,
    Running,
    Completed(i32),
    ConnectionFailed(String),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnippetRun {
    pub id: u64,
    pub host_alias: String,
    pub snippet_name: String,
    pub command: String,
    pub output: String,
    pub status: SnippetRunStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingSnippetRun {
    pub targets: Vec<String>,
    pub command: String,
    pub snippet_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnippetDeleteTarget {
    Package {
        id: u64,
        name: String,
    },
    Snippet {
        package_id: u64,
        id: u64,
        name: String,
    },
}

enum SnippetProcessEvent {
    Output(String, bool),
    Closed,
}

struct SnippetProcess {
    child: Child,
    receiver: Receiver<SnippetProcessEvent>,
    finished: Option<ExitStatus>,
    closed_readers: u8,
}

fn spawn_snippet_reader<R: Read + Send + 'static>(
    mut reader: R,
    sender: Sender<SnippetProcessEvent>,
    stderr: bool,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = sender.send(SnippetProcessEvent::Closed);
                    break;
                }
                Ok(size) => {
                    let text = String::from_utf8_lossy(&buffer[..size]).into_owned();
                    if sender
                        .send(SnippetProcessEvent::Output(text, stderr))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
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
    LoadFiles {
        host_alias: String,
        path: String,
    },
    OpenNativeSftp,
    RunSnippets {
        host_aliases: Vec<String>,
        command: String,
        snippet_name: String,
    },
    CopySnippet {
        command: String,
    },
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
        let (mutation_sender, mutation_receiver) = std::sync::mpsc::channel();
        let (tunnel_sender, tunnel_receiver) = std::sync::mpsc::channel();
        let (runtime_sender, runtime_receiver) = std::sync::mpsc::channel();
        Self {
            workspace: crate::project_app::WorkspaceState::default(),
            project_service: crate::projects::ProjectService::new(
                Metadata::data_dir()
                    .ok()
                    .map(|path| path.join("project-logs")),
            ),
            section: NavSection::Hosts,
            previous_section: NavSection::Hosts,
            hosts,
            selected_host: 0,
            selected_browse_item: 0,
            viewport_width: 80,
            viewport_height: 24,
            search_query: String::new(),
            searching: false,
            should_quit: false,
            notice,
            connection_dialog: None,
            host_dialog: None,
            tunnel_dialog: None,
            snippet_dialog: None,
            package_dialog: None,
            tunnel_host_picker_open: false,
            snippet_host_picker_open: false,
            snippet_host_picker_group: None,
            snippet_host_picker_selection: 0,
            snippet_package_open: false,
            selected_package: 0,
            selected_snippet: 0,
            snippet_concurrency: 4,
            selected_snippet_run: 0,
            snippet_runs: Vec::new(),
            snippet_confirmation: None,
            snippet_delete_confirmation: None,
            snippet_execution_view: false,
            snippet_output_scroll: 0,
            runtime_target: RuntimeTarget::Local,
            runtime_entries: Vec::new(),
            runtime_filter: RuntimeFilter::Mine,
            runtime_list_offset: 0,
            runtime_views: Default::default(),
            runtime_request: 0,
            selected_runtime: 0,
            runtime_loading: false,
            runtime_error: None,
            runtime_host_picker_open: false,
            runtime_host_picker_group: None,
            runtime_host_picker_selection: 0,
            runtime_kill_confirmation: None,
            selected_tunnel: 0,
            tunnel_statuses: metadata
                .tunnels
                .iter()
                .map(|tunnel| (tunnel.id, TunnelStatus::Stopped))
                .collect(),
            metadata,
            metadata_error,
            #[cfg(test)]
            metadata_save_path: None,
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
            selected_transfer: 0,
            transfer_queue: VecDeque::new(),
            active_transfer_id: None,
            transfer_cancellation: std::collections::HashMap::new(),
            file_dialog: None,
            next_transfer_id: 1,
            next_mutation_id: 1,
            sftp_receiver: None,
            transfer_sender,
            transfer_receiver,
            mutation_sender,
            mutation_receiver,
            mutation_hosts: std::collections::HashMap::new(),
            tunnel_sender,
            tunnel_receiver,
            runtime_sender,
            runtime_receiver,
            tunnel_processes: std::collections::HashMap::new(),
            snippet_processes: std::collections::HashMap::new(),
            next_snippet_run_id: 1,
            tunnel_errors: std::collections::HashMap::new(),
            tunnel_retries: std::collections::HashMap::new(),
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
        let query = self.file_filter.to_lowercase();
        listing
            .entries
            .iter()
            .filter(|entry| {
                (self.show_hidden_files || !entry.name.starts_with('.'))
                    && (query.is_empty() || entry.name.to_lowercase().contains(&query))
            })
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

    fn remote_list_start_row(&self) -> u16 {
        4 + u16::from(self.file_filtering || !self.file_filter.is_empty())
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
            host_alias: request.host_alias.clone(),
            direction: request.direction,
            name: request.local_path.file_name().map_or_else(
                || request.remote_path.clone(),
                |name| name.to_string_lossy().into_owned(),
            ),
            local_path: request.local_path.clone(),
            remote_path: request.remote_path.clone(),
            is_dir: request.is_dir,
            transferred: 0,
            total: 0,
            status: TransferStatus::Queued,
        };
        self.notice = Some(match request.direction {
            TransferDirection::Upload => format!("Uploading {}...", task.name),
            TransferDirection::Download => format!("Downloading {}...", task.name),
        });
        self.transfer_tasks.push(task);
        if self.transfer_tasks.len() > 20 {
            self.transfer_tasks.remove(0);
        }
        self.selected_transfer = self
            .selected_transfer
            .min(self.transfer_tasks.len().saturating_sub(1));
        self.transfer_queue.push_back(request);
        self.start_next_transfer();
    }

    fn start_next_transfer(&mut self) {
        if self.active_transfer_id.is_some() {
            return;
        }
        let Some(request) = self.transfer_queue.pop_front() else {
            return;
        };
        let task_id = request.task_id;
        let cancellation = Arc::new(AtomicBool::new(false));
        if let Some(task) = self
            .transfer_tasks
            .iter_mut()
            .find(|task| task.id == task_id)
        {
            task.status = TransferStatus::Running;
        }
        self.active_transfer_id = Some(task_id);
        self.transfer_cancellation
            .insert(task_id, cancellation.clone());
        sftp::spawn_transfer(request, self.transfer_sender.clone(), cancellation);
    }

    fn cancel_active_transfer(&mut self) {
        let Some(task_id) = self.active_transfer_id else {
            self.notice = Some("No transfer is currently running.".to_string());
            return;
        };
        if let Some(cancellation) = self.transfer_cancellation.get(&task_id) {
            cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
            self.notice = Some("Cancelling transfer...".to_string());
        }
    }

    fn cancel_all_transfers(&mut self) {
        for cancellation in self.transfer_cancellation.values() {
            cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for request in self.transfer_queue.drain(..) {
            if let Some(task) = self
                .transfer_tasks
                .iter_mut()
                .find(|task| task.id == request.task_id)
            {
                task.status = TransferStatus::Cancelled;
            }
        }
        if self.active_transfer_id.is_some() {
            self.notice = Some("Cancelling all transfers...".to_string());
        } else {
            self.notice = Some("Cancelled queued transfers.".to_string());
        }
    }

    pub fn selected_transfer_task_id(&self) -> Option<u64> {
        self.transfer_tasks
            .get(self.selected_transfer)
            .map(|task| task.id)
    }

    fn move_transfer_selection(&mut self, direction: KeyCode) {
        if self.transfer_tasks.is_empty() {
            return;
        }
        if direction == KeyCode::Up {
            self.selected_transfer =
                (self.selected_transfer + 1).min(self.transfer_tasks.len() - 1);
        } else {
            self.selected_transfer = self.selected_transfer.saturating_sub(1);
        }
    }

    fn cancel_selected_transfer(&mut self) {
        let Some(task_id) = self.selected_transfer_task_id() else {
            self.notice = Some("No transfer task is selected.".to_string());
            return;
        };
        if self.active_transfer_id == Some(task_id) {
            self.cancel_active_transfer();
            return;
        }
        if let Some(position) = self
            .transfer_queue
            .iter()
            .position(|request| request.task_id == task_id)
        {
            self.transfer_queue.remove(position);
            if let Some(task) = self
                .transfer_tasks
                .iter_mut()
                .find(|task| task.id == task_id)
            {
                task.status = TransferStatus::Cancelled;
            }
            self.notice = Some("Queued transfer cancelled.".to_string());
        } else {
            self.notice = Some("Only queued or running transfers can be cancelled.".to_string());
        }
    }

    fn retry_selected_transfer(&mut self) {
        let Some(task) = self.transfer_tasks.get(self.selected_transfer).cloned() else {
            self.notice = Some("No transfer task is selected.".to_string());
            return;
        };
        if !matches!(
            task.status,
            TransferStatus::Failed(_) | TransferStatus::Cancelled
        ) {
            self.notice = Some("Only failed or cancelled transfers can be retried.".to_string());
            return;
        }
        let request = TransferRequest {
            task_id: self.next_transfer_id,
            host_alias: task.host_alias,
            local_path: task.local_path,
            remote_path: task.remote_path,
            direction: task.direction,
            is_dir: task.is_dir,
        };
        self.next_transfer_id = self.next_transfer_id.saturating_add(1);
        self.queue_or_confirm_transfer(request);
    }

    fn clear_finished_transfers(&mut self) {
        self.transfer_tasks.retain(|task| {
            matches!(
                task.status,
                TransferStatus::Queued | TransferStatus::Running
            )
        });
        self.selected_transfer = self
            .selected_transfer
            .min(self.transfer_tasks.len().saturating_sub(1));
        self.notice = Some("Cleared finished transfer history.".to_string());
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
        if !matches!(entry.kind, LocalEntryKind::File | LocalEntryKind::Directory) {
            return Err("Only files and directories can be uploaded.".to_string());
        }
        let local_path = self.local_files.path.join(&entry.name);
        Ok(TransferRequest {
            task_id: self.next_transfer_id,
            host_alias,
            local_path,
            remote_path: Self::remote_child_path(&listing.path, &entry.name),
            direction: TransferDirection::Upload,
            is_dir: entry.kind == LocalEntryKind::Directory,
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
        if !matches!(
            entry.kind,
            crate::sftp::SftpEntryKind::File | crate::sftp::SftpEntryKind::Directory
        ) {
            return Err("Only files and directories can be downloaded.".to_string());
        }
        Ok(TransferRequest {
            task_id: self.next_transfer_id,
            host_alias,
            local_path: self.local_files.path.join(&entry.name),
            remote_path: Self::remote_child_path(&listing.path, &entry.name),
            direction: TransferDirection::Download,
            is_dir: entry.kind == crate::sftp::SftpEntryKind::Directory,
        })
    }

    fn start_upload(&mut self) {
        match self.prepare_upload() {
            Ok(request) => {
                self.next_transfer_id = self.next_transfer_id.saturating_add(1);
                self.queue_or_confirm_transfer(request);
            }
            Err(message) => self.notice = Some(message),
        }
    }

    fn start_download(&mut self) {
        match self.prepare_download() {
            Ok(request) => {
                self.next_transfer_id = self.next_transfer_id.saturating_add(1);
                self.queue_or_confirm_transfer(request);
            }
            Err(message) => self.notice = Some(message),
        }
    }

    fn queue_or_confirm_transfer(&mut self, request: TransferRequest) {
        let remote_name = Path::new(&request.remote_path)
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let target_exists = match request.direction {
            TransferDirection::Upload => matches!(
                &self.files,
                FilesState::Loaded(listing)
                    if listing.entries.iter().any(|entry| entry.name == remote_name)
            ),
            TransferDirection::Download => request.local_path.exists(),
        };
        if target_exists {
            self.file_dialog = Some(FileDialog::TransferConflict { request });
        } else {
            self.queue_transfer(request);
        }
    }

    fn selected_file_target(&self) -> Result<(String, bool), String> {
        match self.active_file_pane {
            FilePane::Local => {
                let entry = self
                    .visible_local_entries()
                    .get(self.local_files.selected)
                    .cloned()
                    .ok_or_else(|| "Select a file or directory first.".to_string())?;
                if entry.kind == LocalEntryKind::Parent {
                    return Err("The parent entry cannot be changed.".to_string());
                }
                if entry.kind == LocalEntryKind::Other {
                    return Err("This file type cannot be changed here.".to_string());
                }
                Ok((entry.name.clone(), entry.kind == LocalEntryKind::Directory))
            }
            FilePane::Remote => {
                let entry = self
                    .visible_remote_entries()
                    .get(self.selected_file)
                    .cloned()
                    .ok_or_else(|| "Select a file or directory first.".to_string())?;
                Ok((
                    entry.name.clone(),
                    entry.kind == crate::sftp::SftpEntryKind::Directory,
                ))
            }
        }
    }

    fn start_file_rename(&mut self) {
        match self.selected_file_target() {
            Ok((name, _)) => {
                self.file_dialog = Some(FileDialog::Rename {
                    pane: self.active_file_pane,
                    old_name: name.clone(),
                    value: name,
                });
            }
            Err(message) => self.notice = Some(message),
        }
    }

    fn start_file_delete(&mut self) {
        match self.selected_file_target() {
            Ok((name, is_dir)) => {
                self.file_dialog = Some(FileDialog::Delete {
                    pane: self.active_file_pane,
                    name,
                    is_dir,
                });
            }
            Err(message) => self.notice = Some(message),
        }
    }

    fn start_create_dir(&mut self) {
        self.file_dialog = Some(FileDialog::CreateDir {
            pane: self.active_file_pane,
            value: String::new(),
        });
    }

    fn submit_create_dir(&mut self, pane: FilePane, value: String) {
        let name = value.trim().to_string();
        if name.is_empty() {
            self.notice = Some("A directory name is required.".to_string());
            return;
        }
        if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
            self.notice = Some("Name must be a single directory name.".to_string());
            return;
        }

        match pane {
            FilePane::Local => {
                let path = self.local_files.path.join(&name);
                match fs::create_dir(path) {
                    Ok(()) => {
                        self.file_dialog = None;
                        self.notice = Some(format!("Created directory {name}."));
                        self.refresh_local_files();
                    }
                    Err(error) => {
                        self.notice = Some(format!("Could not create directory: {error}"));
                    }
                }
            }
            FilePane::Remote => {
                let Some(host_alias) = self.context_host().map(|host| host.alias.clone()) else {
                    self.notice = Some("Select a remote host first.".to_string());
                    return;
                };
                let request = MutationRequest {
                    operation_id: self.next_mutation_id,
                    host_alias,
                    mutation: Mutation::CreateDir {
                        path: Self::remote_child_path(&self.current_files_path(), &name),
                    },
                };
                self.next_mutation_id = self.next_mutation_id.saturating_add(1);
                self.mutation_hosts
                    .insert(request.operation_id, request.host_alias.clone());
                self.file_dialog = None;
                self.notice = Some(format!("Creating directory {name}..."));
                sftp::spawn_mutation(request, self.mutation_sender.clone());
            }
        }
    }

    fn submit_file_rename(&mut self, pane: FilePane, old_name: String, value: String) {
        let new_name = value.trim().to_string();
        if new_name.is_empty() {
            self.notice = Some("A new name is required.".to_string());
            return;
        }
        if new_name == "." || new_name == ".." || new_name.contains('/') || new_name.contains('\\')
        {
            self.notice = Some("Name must be a single file or directory name.".to_string());
            return;
        }
        if new_name == old_name {
            self.file_dialog = None;
            return;
        }

        match pane {
            FilePane::Local => {
                let from = self.local_files.path.join(&old_name);
                let to = self.local_files.path.join(&new_name);
                match fs::rename(from, to) {
                    Ok(()) => {
                        self.file_dialog = None;
                        self.notice = Some(format!("Renamed {old_name} to {new_name}."));
                        self.refresh_local_files();
                    }
                    Err(error) => self.notice = Some(format!("Could not rename file: {error}")),
                }
            }
            FilePane::Remote => {
                let Some(host_alias) = self.context_host().map(|host| host.alias.clone()) else {
                    self.notice = Some("Select a remote host first.".to_string());
                    return;
                };
                let path = self.current_files_path();
                let request = MutationRequest {
                    operation_id: self.next_mutation_id,
                    host_alias: host_alias.clone(),
                    mutation: Mutation::Rename {
                        from: Self::remote_child_path(&path, &old_name),
                        to: Self::remote_child_path(&path, &new_name),
                    },
                };
                self.next_mutation_id = self.next_mutation_id.saturating_add(1);
                self.mutation_hosts
                    .insert(request.operation_id, host_alias.clone());
                self.file_dialog = None;
                self.notice = Some(format!("Renaming {old_name}..."));
                sftp::spawn_mutation(request, self.mutation_sender.clone());
            }
        }
    }

    fn submit_file_delete(&mut self, pane: FilePane, name: String, is_dir: bool) {
        match pane {
            FilePane::Local => {
                let path = self.local_files.path.join(&name);
                let result = if is_dir {
                    fs::remove_dir(path)
                } else {
                    fs::remove_file(path)
                };
                match result {
                    Ok(()) => {
                        self.file_dialog = None;
                        self.notice = Some(format!("Deleted {name}."));
                        self.refresh_local_files();
                    }
                    Err(error) => self.notice = Some(format!("Could not delete {name}: {error}")),
                }
            }
            FilePane::Remote => {
                let Some(host_alias) = self.context_host().map(|host| host.alias.clone()) else {
                    self.notice = Some("Select a remote host first.".to_string());
                    return;
                };
                let path = Self::remote_child_path(&self.current_files_path(), &name);
                let request = MutationRequest {
                    operation_id: self.next_mutation_id,
                    host_alias,
                    mutation: Mutation::Delete { path, is_dir },
                };
                self.next_mutation_id = self.next_mutation_id.saturating_add(1);
                self.mutation_hosts
                    .insert(request.operation_id, request.host_alias.clone());
                self.file_dialog = None;
                self.notice = Some(format!("Deleting {name}..."));
                sftp::spawn_mutation(request, self.mutation_sender.clone());
            }
        }
    }

    fn handle_file_dialog_key(&mut self, key: KeyCode) {
        let Some(dialog) = self.file_dialog.clone() else {
            return;
        };
        match dialog {
            FileDialog::Rename {
                pane,
                old_name,
                mut value,
            } => match key {
                KeyCode::Esc => self.file_dialog = None,
                KeyCode::Backspace => {
                    value.pop();
                    self.file_dialog = Some(FileDialog::Rename {
                        pane,
                        old_name,
                        value,
                    });
                }
                KeyCode::Enter => self.submit_file_rename(pane, old_name, value),
                KeyCode::Char(character) if !character.is_control() => {
                    value.push(character);
                    self.file_dialog = Some(FileDialog::Rename {
                        pane,
                        old_name,
                        value,
                    });
                }
                _ => {}
            },
            FileDialog::Delete { pane, name, is_dir } => match key {
                KeyCode::Char('y') | KeyCode::Enter => self.submit_file_delete(pane, name, is_dir),
                KeyCode::Char('n') | KeyCode::Esc => self.file_dialog = None,
                _ => {}
            },
            FileDialog::CreateDir { pane, mut value } => match key {
                KeyCode::Esc => self.file_dialog = None,
                KeyCode::Backspace => {
                    value.pop();
                    self.file_dialog = Some(FileDialog::CreateDir { pane, value });
                }
                KeyCode::Enter => self.submit_create_dir(pane, value),
                KeyCode::Char(character) if !character.is_control() => {
                    value.push(character);
                    self.file_dialog = Some(FileDialog::CreateDir { pane, value });
                }
                _ => {}
            },
            FileDialog::TransferConflict { request } => match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.file_dialog = None;
                    self.queue_transfer(request);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.file_dialog = None,
                _ => {}
            },
        }
    }

    pub fn poll_mutations(&mut self) {
        loop {
            let event = match self.mutation_receiver.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            };
            match event {
                MutationEvent::Finished { operation_id } => {
                    let Some(host_alias) = self.mutation_hosts.remove(&operation_id) else {
                        continue;
                    };
                    if self.context_host().map(|host| host.alias.as_str())
                        != Some(host_alias.as_str())
                    {
                        continue;
                    }
                    self.notice = Some("Remote file operation completed.".to_string());
                    let path = self.current_files_path();
                    self.files = FilesState::Loading {
                        host_alias: host_alias.clone(),
                        path: path.clone(),
                    };
                    self.sftp_receiver = Some(sftp::spawn_list(host_alias, path));
                }
                MutationEvent::Failed {
                    operation_id,
                    message,
                } => {
                    self.mutation_hosts.remove(&operation_id);
                    self.notice = Some(format!("Remote file operation failed: {message}"));
                }
            }
        }
    }

    pub fn poll_tunnels(&mut self) {
        while let Ok(event) = self.tunnel_receiver.try_recv() {
            match event {
                tunnel::TunnelEvent::Stderr { id, message } => {
                    let failed =
                        matches!(self.tunnel_statuses.get(&id), Some(TunnelStatus::Failed(_)));
                    self.tunnel_errors.insert(id, message.clone());
                    if failed {
                        self.tunnel_statuses
                            .insert(id, TunnelStatus::Failed(message.clone()));
                        self.notice = Some(format!("Tunnel failed: {message}"));
                    }
                }
            }
        }

        let due = self
            .tunnel_retries
            .iter()
            .filter(|(_, retry)| retry.next_attempt <= Instant::now())
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in due {
            self.tunnel_retries.remove(&id);
            if let Some(index) = self
                .metadata
                .tunnels
                .iter()
                .position(|tunnel| tunnel.id == id)
            {
                self.selected_tunnel = index;
                self.start_selected_tunnel();
            }
        }

        let ids = self.tunnel_processes.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let result = self.tunnel_processes.get_mut(&id).map(Child::try_wait);
            let Some(result) = result else {
                continue;
            };
            match result {
                Ok(None) => {
                    self.tunnel_statuses.insert(id, TunnelStatus::Running);
                }
                Ok(Some(status)) => {
                    self.tunnel_processes.remove(&id);
                    if status.success() {
                        self.tunnel_errors.remove(&id);
                        self.tunnel_retries.remove(&id);
                        self.tunnel_statuses.insert(id, TunnelStatus::Stopped);
                    } else {
                        let message = self
                            .tunnel_errors
                            .remove(&id)
                            .unwrap_or_else(|| format!("ssh exited with {status}"));
                        self.notice = Some(format!("Tunnel failed: {message}"));
                        self.tunnel_statuses
                            .insert(id, TunnelStatus::Failed(message.clone()));
                        if let Some(tunnel) =
                            self.metadata.tunnels.iter().find(|tunnel| tunnel.id == id)
                            && tunnel.auto_reconnect
                        {
                            let retry = self.tunnel_retries.entry(id).or_insert(TunnelRetry {
                                attempts: 0,
                                next_attempt: Instant::now(),
                            });
                            if retry.attempts < 5 {
                                retry.attempts += 1;
                                let delay = 1_u64 << (retry.attempts - 1);
                                retry.next_attempt = Instant::now() + Duration::from_secs(delay);
                                self.notice = Some(format!(
                                    "Tunnel failed; reconnecting in {delay}s (attempt {}/5).",
                                    retry.attempts
                                ));
                            }
                        }
                    }
                }
                Err(error) => {
                    self.tunnel_processes.remove(&id);
                    let message = format!("Could not check tunnel process: {error}");
                    self.tunnel_errors.insert(id, message.clone());
                    self.tunnel_statuses
                        .insert(id, TunnelStatus::Failed(message.clone()));
                    self.notice = Some(message);
                }
            }
        }
    }

    pub fn poll_runtime(&mut self) {
        while let Ok(event) = self.runtime_receiver.try_recv() {
            match event {
                RuntimeEvent::Listed {
                    target,
                    request,
                    mut entries,
                } => {
                    if target == self.runtime_target && request == self.runtime_request {
                        let selected = self.selected_runtime_entry().map(|entry| {
                            (
                                entry.pid,
                                entry.address.clone(),
                                entry.port,
                                entry.protocol.clone(),
                            )
                        });
                        let owners = self.project_service.owners();
                        for entry in &mut entries {
                            if entry.project.as_ref().is_some_and(|owner| {
                                target != RuntimeTarget::Local || !owners.contains(owner)
                            }) {
                                entry.project = None;
                                entry.pid = None;
                                entry.origin = runtime::RuntimeOrigin::Unknown;
                                entry.origin_reason =
                                    "Project command has ended; refresh inspection.".into();
                            }
                        }
                        self.runtime_entries = entries;
                        self.update_external_runtime_links();
                        if let Some((pid, address, port, protocol)) = selected
                            && let Some(index) =
                                self.visible_runtime_entries().iter().position(|entry| {
                                    entry.pid == pid
                                        && entry.address == address
                                        && entry.port == port
                                        && entry.protocol == protocol
                                })
                        {
                            self.selected_runtime = index;
                        }
                        self.selected_runtime = self
                            .selected_runtime
                            .min(self.visible_runtime_entries().len().saturating_sub(1));
                        self.runtime_loading = false;
                        self.runtime_error = None;
                        self.runtime_list_offset = self.runtime_scroll_offset(self.viewport_height);
                    }
                }
                RuntimeEvent::Failed {
                    target,
                    request,
                    message,
                } => {
                    if target == self.runtime_target && request == self.runtime_request {
                        self.runtime_loading = false;
                        self.runtime_error = Some(message.clone());
                        self.notice = Some(format!("Runtime inspection failed: {message}"));
                    }
                }
                RuntimeEvent::ActionCompleted { target, message } => {
                    if target == self.runtime_target {
                        self.notice = Some(message);
                        self.refresh_runtime();
                    }
                }
                RuntimeEvent::ActionFailed { target, message } => {
                    if target == self.runtime_target {
                        self.notice = Some(format!("Could not terminate process: {message}"));
                    }
                }
            }
        }
    }

    pub fn refresh_runtime(&mut self) {
        self.runtime_request += 1;
        if let RuntimeTarget::Host(alias) = &self.runtime_target
            && !self.hosts.hosts.iter().any(|host| host.alias == *alias)
        {
            self.runtime_loading = false;
            self.runtime_entries.clear();
            self.runtime_error = Some(format!("Host {alias} is no longer in the Hosts catalog."));
            return;
        }
        self.runtime_loading = true;
        self.runtime_error = None;
        runtime::inspect_async(
            self.runtime_sender.clone(),
            self.runtime_target.clone(),
            self.runtime_request,
            self.project_service.owners(),
        );
    }

    pub(crate) fn switch_runtime_target(&mut self, target: RuntimeTarget) {
        if self.runtime_kill_confirmation.is_some() {
            return;
        }
        self.restore_runtime_view(target);
        self.refresh_runtime();
    }

    fn restore_runtime_view(&mut self, target: RuntimeTarget) {
        if self.runtime_target != target {
            let previous = runtime::RuntimeView {
                entries: std::mem::take(&mut self.runtime_entries),
                filter: self.runtime_filter,
                selected: self.selected_runtime,
                offset: self.runtime_list_offset,
                error: self.runtime_error.take(),
            };
            self.runtime_views
                .insert(self.runtime_target.clone(), previous);
            let next = self.runtime_views.remove(&target).unwrap_or_default();
            self.runtime_target = target;
            self.runtime_entries = next.entries;
            self.runtime_filter = next.filter;
            self.selected_runtime = next.selected;
            self.runtime_list_offset = next.offset;
            self.runtime_error = next.error;
        }
    }

    fn open_runtime_host_picker(&mut self) {
        self.runtime_host_picker_open = true;
        self.runtime_host_picker_group = None;
        self.runtime_host_picker_selection = 0;
    }

    pub(crate) fn runtime_scroll_offset(&self, height: u16) -> usize {
        let layout = crate::ui::runtime_layout(ratatui::layout::Rect::new(
            0,
            0,
            self.viewport_width,
            height,
        ));
        let capacity = usize::from(layout.listeners.height.saturating_sub(4) / 2).max(1);
        let count = self.visible_runtime_entries().len();
        let selected = self.selected_runtime.min(count.saturating_sub(1));
        self.runtime_list_offset
            .min(selected)
            .max(selected.saturating_sub(capacity - 1))
            .min(count.saturating_sub(capacity))
    }

    pub fn visible_runtime_entries(&self) -> Vec<&RuntimeEntry> {
        self.runtime_entries
            .iter()
            .filter(|entry| self.runtime_filter.matches(entry))
            .collect()
    }

    pub fn selected_runtime_entry(&self) -> Option<&RuntimeEntry> {
        self.visible_runtime_entries()
            .get(self.selected_runtime)
            .copied()
    }

    pub(crate) fn runtime_host_picker_items_for_ui(&self) -> Vec<HostPickerItem> {
        if let Some(group) = &self.runtime_host_picker_group {
            return self
                .hosts
                .hosts
                .iter()
                .filter(|host| self.matches_group(host, group))
                .map(|host| HostPickerItem::Host(host.alias.clone()))
                .collect();
        }
        self.visible_groups()
            .into_iter()
            .map(HostPickerItem::Group)
            .chain(
                self.hosts
                    .hosts
                    .iter()
                    .map(|host| HostPickerItem::Host(host.alias.clone())),
            )
            .collect()
    }

    fn handle_runtime_host_picker_key(&mut self, key: KeyCode) {
        let items = self.runtime_host_picker_items_for_ui();
        match key {
            KeyCode::Esc => self.runtime_host_picker_open = false,
            KeyCode::Backspace => {
                if self.runtime_host_picker_group.is_some() {
                    self.runtime_host_picker_group = None;
                    self.runtime_host_picker_selection = 0;
                } else {
                    self.runtime_host_picker_open = false;
                }
            }
            KeyCode::Up => {
                self.runtime_host_picker_selection =
                    self.runtime_host_picker_selection.saturating_sub(1);
            }
            KeyCode::Down => {
                self.runtime_host_picker_selection =
                    (self.runtime_host_picker_selection + 1).min(items.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let Some(item) = items.get(self.runtime_host_picker_selection).cloned() else {
                    return;
                };
                match item {
                    HostPickerItem::Group(group) => {
                        self.runtime_host_picker_group = Some(group);
                        self.runtime_host_picker_selection = 0;
                    }
                    HostPickerItem::Host(alias) => {
                        self.runtime_host_picker_open = false;
                        self.runtime_host_picker_group = None;
                        self.switch_runtime_target(RuntimeTarget::Host(alias));
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_runtime_key(&mut self, key: KeyCode) {
        if self.runtime_kill_confirmation.is_some() {
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let Some(entry) = self.runtime_kill_confirmation.take() else {
                        return;
                    };
                    if let Some(owner) = entry.project {
                        if self.runtime_target == RuntimeTarget::Local
                            && self.project_service.owners().contains(&owner)
                        {
                            self.notice = Some(match self.project_service.stop(owner.key) {
                                Ok(()) => format!(
                                    "Stopping {} / {}.",
                                    owner.project_name, owner.command_name
                                ),
                                Err(error) => format!("Could not stop command: {error}"),
                            });
                        } else {
                            self.notice =
                                Some("Project run has changed; refresh before stopping.".into());
                        }
                        self.refresh_runtime();
                        return;
                    }
                    if let Some(run) = entry.external_project {
                        if self.runtime_target == RuntimeTarget::Local {
                            self.start_confirmed_external_action(
                                crate::project_app::ExternalConfirmation {
                                    action: crate::project_app::ProjectAction::Stop,
                                    keys: vec![run.key],
                                    runs: vec![run],
                                },
                            );
                        }
                        return;
                    }
                    let Some(pid) = entry.pid else {
                        self.notice = Some("The selected entry has no visible PID.".to_string());
                        return;
                    };
                    runtime::terminate_async(
                        self.runtime_sender.clone(),
                        self.runtime_target.clone(),
                        pid,
                        entry.process,
                    );
                }
                KeyCode::Char('n') | KeyCode::Esc => self.runtime_kill_confirmation = None,
                _ => {}
            }
            return;
        }
        if self.runtime_host_picker_open {
            self.handle_runtime_host_picker_key(key);
            return;
        }
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.next_section(),
            KeyCode::BackTab => self.previous_section(),
            KeyCode::Char('`') => self.switch_section(self.previous_section),
            KeyCode::Up => {
                self.selected_runtime = self.selected_runtime.saturating_sub(1);
            }
            KeyCode::Down => {
                self.selected_runtime = (self.selected_runtime + 1)
                    .min(self.visible_runtime_entries().len().saturating_sub(1));
            }
            KeyCode::Left | KeyCode::Right => {
                let index = RuntimeFilter::ALL
                    .iter()
                    .position(|filter| *filter == self.runtime_filter)
                    .unwrap_or(0);
                let next = if key == KeyCode::Right {
                    (index + 1) % RuntimeFilter::ALL.len()
                } else {
                    (index + RuntimeFilter::ALL.len() - 1) % RuntimeFilter::ALL.len()
                };
                self.runtime_filter = RuntimeFilter::ALL[next];
                self.selected_runtime = 0;
                self.runtime_list_offset = 0;
            }
            KeyCode::Char('r') => self.refresh_runtime(),
            KeyCode::Char('o') => self.open_project_from_runtime(),
            KeyCode::Char('l') => {
                self.switch_runtime_target(RuntimeTarget::Local);
            }
            KeyCode::Char('s' | 'h') => {
                self.open_runtime_host_picker();
            }
            KeyCode::Char('k') => {
                if self.runtime_loading {
                    self.notice = Some("Wait for the current machine inspection to finish.".into());
                } else if self.runtime_error.is_some() {
                    self.notice = Some(
                        "Refresh this machine successfully before terminating a process.".into(),
                    );
                } else if let Some(entry) = self.selected_runtime_entry()
                    && entry.pid.is_some()
                {
                    self.runtime_kill_confirmation = Some(entry.clone());
                } else {
                    self.notice = Some("The selected entry has no visible PID.".to_string());
                }
            }
            _ => {}
        }
        self.runtime_list_offset = self.runtime_scroll_offset(self.viewport_height);
    }

    pub fn start_selected_tunnel(&mut self) {
        let Some(tunnel) = self.selected_tunnel_definition().cloned() else {
            self.notice = Some("Select a tunnel first.".to_string());
            return;
        };
        if self.tunnel_processes.contains_key(&tunnel.id) {
            self.notice = Some(format!("Tunnel {} is already running.", tunnel.label));
            return;
        }
        self.tunnel_errors.remove(&tunnel.id);
        self.tunnel_retries.remove(&tunnel.id);
        match tunnel::spawn_forward(&tunnel, self.tunnel_sender.clone()) {
            Ok(child) => {
                self.tunnel_processes.insert(tunnel.id, child);
                self.tunnel_statuses
                    .insert(tunnel.id, TunnelStatus::Starting);
                self.notice = Some(format!("Starting tunnel {}.", tunnel.label));
            }
            Err(error) => {
                let message = format!("Could not start tunnel: {error}");
                self.tunnel_statuses
                    .insert(tunnel.id, TunnelStatus::Failed(message.clone()));
                self.tunnel_errors.insert(tunnel.id, message.clone());
                self.notice = Some(message);
            }
        }
    }

    pub fn stop_selected_tunnel(&mut self) {
        let Some(tunnel) = self.selected_tunnel_definition().cloned() else {
            self.notice = Some("Select a tunnel first.".to_string());
            return;
        };
        self.stop_tunnel(tunnel.id);
        self.notice = Some(format!("Stopped tunnel {}.", tunnel.label));
    }

    pub fn toggle_selected_tunnel(&mut self) {
        let Some(tunnel) = self.selected_tunnel_definition() else {
            self.notice = Some("Select a tunnel first.".to_string());
            return;
        };
        if self.tunnel_processes.contains_key(&tunnel.id) {
            self.stop_selected_tunnel();
        } else {
            self.start_selected_tunnel();
        }
    }

    fn stop_tunnel(&mut self, id: u64) {
        if let Some(mut child) = self.tunnel_processes.remove(&id) {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.tunnel_errors.remove(&id);
        self.tunnel_retries.remove(&id);
        self.tunnel_statuses.insert(id, TunnelStatus::Stopped);
    }

    fn stop_orphaned_tunnels(&mut self) {
        let ids = self
            .tunnel_processes
            .keys()
            .copied()
            .filter(|id| !self.metadata.tunnels.iter().any(|tunnel| tunnel.id == *id))
            .collect::<Vec<_>>();
        for id in ids {
            self.stop_tunnel(id);
            self.tunnel_statuses.remove(&id);
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

        if key == KeyCode::Esc && !self.file_filter.is_empty() {
            self.file_filter.clear();
            self.local_files.selected = 0;
            self.selected_file = 0;
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
            KeyCode::Char('c') => self.cancel_selected_transfer(),
            KeyCode::Char('C') => self.cancel_all_transfers(),
            KeyCode::Char('[') => self.move_transfer_selection(KeyCode::Up),
            KeyCode::Char(']') => self.move_transfer_selection(KeyCode::Down),
            KeyCode::Char('R') => self.retry_selected_transfer(),
            KeyCode::Char('K') => self.clear_finished_transfers(),
            KeyCode::Char('m') => self.start_file_rename(),
            KeyCode::Char('x') => self.start_file_delete(),
            KeyCode::Char('n') => self.start_create_dir(),
            KeyCode::Char('/')
                if self.active_file_pane == FilePane::Local
                    || self.active_file_pane == FilePane::Remote =>
            {
                self.file_filtering = true;
                self.file_filter.clear();
                self.local_files.selected = 0;
                self.selected_file = 0;
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
        if self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
        {
            return InputAction::None;
        }
        if self.section == NavSection::Runtime {
            if self.runtime_host_picker_open || self.runtime_kill_confirmation.is_some() {
                return InputAction::None;
            }
            self.viewport_width = viewport_width;
            self.viewport_height = viewport_height;
            let layout = crate::ui::runtime_layout(ratatui::layout::Rect::new(
                0,
                0,
                viewport_width,
                viewport_height,
            ));
            let position = ratatui::layout::Position::new(column, row);
            if layout.local.contains(position) {
                self.switch_runtime_target(RuntimeTarget::Local);
            } else if layout.hosts.contains(position) {
                self.open_runtime_host_picker();
            } else if let Some(index) = crate::ui::runtime_filter_areas(layout.filters, self)
                .iter()
                .position(|area| area.contains(position))
            {
                self.runtime_filter = RuntimeFilter::ALL[index];
                self.selected_runtime = 0;
                self.runtime_list_offset = 0;
            } else {
                let list = layout.listeners.inner(ratatui::layout::Margin::new(2, 2));
                if list.contains(position) {
                    let index =
                        self.runtime_scroll_offset(viewport_height) + usize::from(row - list.y) / 2;
                    if index < self.visible_runtime_entries().len() {
                        self.selected_runtime = index;
                    }
                }
            }
            self.runtime_list_offset = self.runtime_scroll_offset(viewport_height);
            return InputAction::None;
        }
        if self.section == NavSection::Snippets {
            if row < 6 || row >= viewport_height.saturating_sub(3) {
                return InputAction::None;
            }
            let content_width = viewport_width.saturating_sub(2);
            let left_width = content_width * 35 / 100;
            let list_row = usize::from(row.saturating_sub(7)) / 2;
            if column < left_width {
                if list_row < self.metadata.snippet_packages.len() {
                    self.selected_package = list_row;
                    self.selected_snippet = 0;
                    self.snippet_package_open = true;
                }
            } else if let Some(package) = self.metadata.snippet_packages.get(self.selected_package)
                && list_row < package.snippets.len()
            {
                self.snippet_package_open = true;
                self.selected_snippet = list_row;
            }
            return InputAction::None;
        }
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
            let index = usize::from(row.saturating_sub(self.remote_list_start_row()));
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
                        self.active_transfer_id = None;
                        self.transfer_cancellation.remove(&task_id);
                        self.start_next_transfer();
                        continue;
                    };
                    task.status = TransferStatus::Completed;
                    self.active_transfer_id = None;
                    self.transfer_cancellation.remove(&task_id);
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
                                ..Default::default()
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
                TransferEvent::Cancelled { task_id } => {
                    if let Some(task) = self
                        .transfer_tasks
                        .iter_mut()
                        .find(|task| task.id == task_id)
                    {
                        task.status = TransferStatus::Cancelled;
                    }
                    self.active_transfer_id = None;
                    self.transfer_cancellation.remove(&task_id);
                    self.notice = Some("Transfer cancelled.".to_string());
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
                    self.active_transfer_id = None;
                    self.transfer_cancellation.remove(&task_id);
                }
            }
            self.start_next_transfer();
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

    fn selected_snippet(&self) -> Option<&SnippetDefinition> {
        self.metadata
            .snippet_packages
            .get(self.selected_package)?
            .snippets
            .get(self.selected_snippet)
    }

    fn snippet_run_action(&mut self, targets: Vec<String>) -> InputAction {
        let Some(snippet) = self.selected_snippet().cloned() else {
            return InputAction::None;
        };
        if snippet.dangerous {
            self.snippet_confirmation = Some(PendingSnippetRun {
                targets,
                command: snippet.command,
                snippet_name: snippet.name,
            });
            return InputAction::None;
        }
        InputAction::RunSnippets {
            host_aliases: targets,
            command: snippet.command,
            snippet_name: snippet.name,
        }
    }

    pub fn start_snippet_runs(
        &mut self,
        host_aliases: Vec<String>,
        command: String,
        snippet_name: String,
    ) {
        let target_count = host_aliases.len();
        for host_alias in host_aliases {
            let id = self.next_snippet_run_id;
            self.next_snippet_run_id = self.next_snippet_run_id.saturating_add(1);
            self.snippet_runs.push(SnippetRun {
                id,
                host_alias,
                snippet_name: snippet_name.clone(),
                command: command.clone(),
                output: String::new(),
                status: SnippetRunStatus::Queued,
            });
        }
        if !self.snippet_runs.is_empty() {
            self.selected_snippet_run = self.snippet_runs.len().saturating_sub(1);
        }
        self.snippet_execution_view = true;
        self.snippet_output_scroll = 0;
        self.notice = Some(format!(
            "Queued {} for {} host{}.",
            snippet_name,
            target_count,
            if target_count == 1 { "" } else { "s" }
        ));
    }

    pub fn poll_snippet_runs(&mut self) {
        let running = self.snippet_processes.len();
        let available = self.snippet_concurrency.saturating_sub(running);
        let queued = self
            .snippet_runs
            .iter()
            .filter(|run| matches!(run.status, SnippetRunStatus::Queued))
            .take(available)
            .map(|run| run.id)
            .collect::<Vec<_>>();
        for id in queued {
            let Some(run) = self.snippet_runs.iter_mut().find(|run| run.id == id) else {
                continue;
            };
            match std::process::Command::new("ssh")
                .args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "RequestTTY=no",
                    "-o",
                    "ConnectTimeout=10",
                ])
                .arg(&run.host_alias)
                .arg(&run.command)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    let Some(stdout) = child.stdout.take() else {
                        let _ = child.kill();
                        run.status = SnippetRunStatus::ConnectionFailed(
                            "ssh stdout pipe was unavailable".to_string(),
                        );
                        continue;
                    };
                    let Some(stderr) = child.stderr.take() else {
                        let _ = child.kill();
                        run.status = SnippetRunStatus::ConnectionFailed(
                            "ssh stderr pipe was unavailable".to_string(),
                        );
                        continue;
                    };
                    let (sender, receiver) = std::sync::mpsc::channel();
                    spawn_snippet_reader(stdout, sender.clone(), false);
                    spawn_snippet_reader(stderr, sender, true);
                    run.status = SnippetRunStatus::Running;
                    self.snippet_processes.insert(
                        id,
                        SnippetProcess {
                            child,
                            receiver,
                            finished: None,
                            closed_readers: 0,
                        },
                    );
                }
                Err(error) => run.status = SnippetRunStatus::ConnectionFailed(error.to_string()),
            }
        }
        let ids = self.snippet_processes.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let Some(process) = self.snippet_processes.get_mut(&id) else {
                continue;
            };
            let mut events = Vec::new();
            while let Ok(event) = process.receiver.try_recv() {
                events.push(event);
            }
            let run = self.snippet_runs.iter_mut().find(|run| run.id == id);
            if let Some(run) = run {
                for event in events {
                    match event {
                        SnippetProcessEvent::Output(text, stderr) => {
                            if stderr {
                                run.output.push_str("[stderr] ");
                            }
                            run.output.push_str(&text);
                        }
                        SnippetProcessEvent::Closed => {
                            process.closed_readers = process.closed_readers.saturating_add(1);
                        }
                    }
                }
            }
            let closed_readers = process.closed_readers;
            if process.finished.is_none() {
                process.finished = process.child.try_wait().ok().flatten();
            }
            if process.finished.is_some() && closed_readers >= 2 {
                let status = process.finished.expect("checked above");
                let code = status.code().unwrap_or(-1);
                if let Some(run) = self.snippet_runs.iter_mut().find(|run| run.id == id) {
                    run.status = if code == 255 {
                        SnippetRunStatus::ConnectionFailed(
                            "ssh returned 255; connection or authentication failed".to_string(),
                        )
                    } else {
                        SnippetRunStatus::Completed(code)
                    };
                }
                self.snippet_processes.remove(&id);
            }
        }
    }

    pub fn cancel_selected_snippet_run(&mut self) {
        let Some(run) = self.snippet_runs.get_mut(self.selected_snippet_run) else {
            return;
        };
        if let Some(mut process) = self.snippet_processes.remove(&run.id) {
            let _ = process.child.kill();
            let _ = process.child.wait();
            run.status = SnippetRunStatus::Cancelled;
            self.notice = Some(format!(
                "Cancelled {} on {}.",
                run.snippet_name, run.host_alias
            ));
        } else if matches!(run.status, SnippetRunStatus::Queued) {
            run.status = SnippetRunStatus::Cancelled;
        }
    }

    fn handle_snippet_confirmation_key(&mut self, key: KeyCode) -> InputAction {
        match key {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(pending) = self.snippet_confirmation.take() {
                    return InputAction::RunSnippets {
                        host_aliases: pending.targets,
                        command: pending.command,
                        snippet_name: pending.snippet_name,
                    };
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => self.snippet_confirmation = None,
            _ => {}
        }
        InputAction::None
    }

    pub fn handle_event(&mut self, key: KeyEvent) -> InputAction {
        let modal = self.host_dialog.is_some()
            || self.tunnel_dialog.is_some()
            || self.snippet_dialog.is_some()
            || self.package_dialog.is_some()
            || self.snippet_confirmation.is_some()
            || self.snippet_delete_confirmation.is_some()
            || self.runtime_host_picker_open
            || self.runtime_kill_confirmation.is_some()
            || self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
            || self.tags_dialog.is_some()
            || self.connection_dialog.is_some()
            || self.file_dialog.is_some();
        if !modal {
            match (key.code, key.modifiers) {
                (KeyCode::F(number @ 1..=6), KeyModifiers::NONE) => {
                    self.switch_section(NavSection::ALL[usize::from(number - 1)]);
                    return InputAction::None;
                }
                (KeyCode::Char(number @ '1'..='6'), KeyModifiers::NONE)
                    if !self.searching
                        && !self.file_filtering
                        && !self.host_picker_open
                        && !self.snippet_host_picker_open =>
                {
                    self.switch_section(NavSection::ALL[(number as u8 - b'1') as usize]);
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
        if self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
        {
            self.handle_workspace_dialog(key.code);
        } else if self.searching && !modal {
            self.handle_search_key(key.code);
        } else if self.file_filtering && !modal {
            self.handle_file_filter_key(key.code);
        } else if self.file_dialog.is_some() {
            self.handle_file_dialog_key(key.code);
        } else if self.snippet_confirmation.is_some() {
            return self.handle_snippet_confirmation_key(key.code);
        } else if self.snippet_delete_confirmation.is_some() {
            self.handle_snippet_delete_key(key.code);
        } else if self.runtime_host_picker_open || self.runtime_kill_confirmation.is_some() {
            self.handle_runtime_key(key.code);
        } else if modal {
            self.handle_key(key.code);
        } else if self.section == NavSection::Hosts && key.code == KeyCode::Enter {
            if !self.enter_group()
                && let Some(host) = self.selected_host().cloned()
            {
                self.set_host_context(Some(host.alias));
                return InputAction::Connect;
            }
        } else if self.snippet_host_picker_open {
            return self.handle_snippet_host_picker_key(key.code);
        } else if self.section == NavSection::Files {
            return self.handle_files_key(key.code);
        } else if self.section == NavSection::Snippets {
            return self.handle_snippets_key(key.code);
        } else if self.section == NavSection::Runtime {
            self.handle_runtime_key(key.code);
        } else if self.section == NavSection::Projects {
            self.handle_projects_key(key.code);
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

    pub(crate) fn switch_section(&mut self, section: NavSection) {
        if section != self.section {
            self.previous_section = self.section;
            self.section = section;
            self.searching = false;
            self.file_filtering = false;
            if section == NavSection::Runtime {
                self.refresh_runtime();
            }
            if section == NavSection::Projects {
                self.refresh_projects();
            }
        }
    }

    pub(crate) fn set_host_context(&mut self, alias: Option<String>) {
        if self.host_context != alias {
            self.host_context = alias;
            self.files = FilesState::Idle;
            self.selected_file = 0;
            self.sftp_receiver = None;
        }
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        if self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
        {
            self.handle_workspace_dialog(key);
            return;
        }
        if self.tags_dialog.is_some() {
            self.handle_tags_key(key);
            return;
        }
        if self.tunnel_dialog.is_some() {
            self.handle_tunnel_dialog_key(key);
            return;
        }
        if self.snippet_dialog.is_some() {
            self.handle_snippet_dialog_key(key);
            return;
        }
        if self.package_dialog.is_some() {
            self.handle_package_dialog_key(key);
            return;
        }
        if self.host_dialog.is_some() {
            self.handle_host_dialog_key(key);
            return;
        }
        if self.section == NavSection::Runtime {
            self.handle_runtime_key(key);
            return;
        }
        if self.section == NavSection::Projects {
            self.handle_projects_key(key);
            return;
        }
        if self.section == NavSection::Tunnels {
            match key {
                KeyCode::Up => {
                    self.selected_tunnel = self.selected_tunnel.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.selected_tunnel = (self.selected_tunnel + 1)
                        .min(self.metadata.tunnels.len().saturating_sub(1));
                    return;
                }
                KeyCode::Enter | KeyCode::Char('e') => {
                    self.open_tunnel_form(self.selected_tunnel_definition().cloned());
                    return;
                }
                KeyCode::Char('s') | KeyCode::Char(' ') => {
                    self.toggle_selected_tunnel();
                    return;
                }
                KeyCode::Char('r') => {
                    if let Some(tunnel) = self.selected_tunnel_definition().cloned() {
                        if self.tunnel_processes.contains_key(&tunnel.id) {
                            self.stop_tunnel(tunnel.id);
                        }
                        self.start_selected_tunnel();
                    }
                    return;
                }
                KeyCode::Char('A') => {
                    if let Some(tunnel) = self.selected_tunnel_definition().cloned()
                        && let Some(stored) = self
                            .metadata
                            .tunnels
                            .iter_mut()
                            .find(|item| item.id == tunnel.id)
                    {
                        stored.auto_reconnect = !stored.auto_reconnect;
                        let state = if stored.auto_reconnect {
                            "enabled"
                        } else {
                            "disabled"
                        };
                        self.notice = Some(format!("Auto-reconnect {state}."));
                        self.save_metadata();
                    }
                    return;
                }
                _ => {}
            }
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
            KeyCode::Char('a') if self.section == NavSection::Tunnels => {
                self.open_tunnel_form(None)
            }
            KeyCode::Char('l') if self.section == NavSection::Tunnels => {
                self.open_tunnel_form_with_kind(None, TunnelKind::Local)
            }
            KeyCode::Char('R') if self.section == NavSection::Tunnels => {
                self.open_tunnel_form_with_kind(None, TunnelKind::Remote)
            }
            KeyCode::Char('D') if self.section == NavSection::Tunnels => {
                self.open_tunnel_form_with_kind(None, TunnelKind::Dynamic)
            }
            KeyCode::Char('e') if self.section == NavSection::Tunnels => {
                self.open_tunnel_form(self.selected_tunnel_definition().cloned());
            }
            KeyCode::Char('d') if self.section == NavSection::Tunnels => {
                self.open_tunnel_delete();
            }
            KeyCode::Char('1') => self.switch_section(NavSection::Hosts),
            KeyCode::Char('2') | KeyCode::Char('f') => self.switch_section(NavSection::Files),
            KeyCode::Char('3') | KeyCode::Char('t') => self.switch_section(NavSection::Tunnels),
            KeyCode::Char('4') | KeyCode::Char('s') => self.switch_section(NavSection::Snippets),
            KeyCode::Char('5') | KeyCode::Char('p') => self.switch_section(NavSection::Runtime),
            KeyCode::Char('6') => self.switch_section(NavSection::Projects),
            _ => {}
        }
    }

    fn handle_snippets_key(&mut self, key: KeyCode) -> InputAction {
        match key {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return InputAction::None;
            }
            KeyCode::Tab => {
                self.next_section();
                return InputAction::None;
            }
            KeyCode::BackTab => {
                self.previous_section();
                return InputAction::None;
            }
            KeyCode::Char('`') => {
                self.switch_section(self.previous_section);
                return InputAction::None;
            }
            _ => {}
        }
        if self.snippet_execution_view {
            match key {
                KeyCode::Esc | KeyCode::Backspace => {
                    self.snippet_execution_view = false;
                    self.snippet_output_scroll = 0;
                }
                KeyCode::Up => {
                    self.snippet_output_scroll = self.snippet_output_scroll.saturating_sub(1)
                }
                KeyCode::Down => {
                    self.snippet_output_scroll = self.snippet_output_scroll.saturating_add(1)
                }
                KeyCode::Char('[') => {
                    self.selected_snippet_run = self.selected_snippet_run.saturating_sub(1);
                    self.snippet_output_scroll = 0;
                }
                KeyCode::Char(']') => {
                    self.selected_snippet_run = (self.selected_snippet_run + 1)
                        .min(self.snippet_runs.len().saturating_sub(1));
                    self.snippet_output_scroll = 0;
                }
                KeyCode::Char('c') => self.cancel_selected_snippet_run(),
                KeyCode::Char('C') => {
                    self.snippet_runs.retain(|run| {
                        matches!(
                            run.status,
                            SnippetRunStatus::Queued | SnippetRunStatus::Running
                        )
                    });
                    self.selected_snippet_run = self
                        .selected_snippet_run
                        .min(self.snippet_runs.len().saturating_sub(1));
                    self.snippet_output_scroll = 0;
                }
                _ => {}
            }
            return InputAction::None;
        }
        if self.snippet_host_picker_open {
            let _ = self.handle_snippet_host_picker_key(key);
            return InputAction::None;
        }
        let package_count = self.metadata.snippet_packages.len();
        if package_count == 0 {
            if key == KeyCode::Char('a') {
                self.open_package_form(None);
            }
            return InputAction::None;
        }
        if !self.snippet_package_open {
            match key {
                KeyCode::Up => self.selected_package = self.selected_package.saturating_sub(1),
                KeyCode::Down => {
                    self.selected_package =
                        (self.selected_package + 1).min(package_count.saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Right => {
                    self.snippet_package_open = true;
                    self.selected_snippet = 0;
                }
                KeyCode::Char('a') => self.open_package_form(None),
                KeyCode::Char('e') => {
                    if let Some(package) = self
                        .metadata
                        .snippet_packages
                        .get(self.selected_package)
                        .cloned()
                    {
                        self.open_package_form(Some(package));
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(package) = self.metadata.snippet_packages.get(self.selected_package)
                    {
                        self.snippet_delete_confirmation = Some(SnippetDeleteTarget::Package {
                            id: package.id,
                            name: package.name.clone(),
                        });
                    }
                }
                _ => {}
            }
            return InputAction::None;
        }
        let snippets = self.metadata.snippet_packages[self.selected_package]
            .snippets
            .len();
        match key {
            KeyCode::Char('[') => {
                self.selected_snippet_run = self.selected_snippet_run.saturating_sub(1)
            }
            KeyCode::Char(']') => {
                self.selected_snippet_run =
                    (self.selected_snippet_run + 1).min(self.snippet_runs.len().saturating_sub(1))
            }
            KeyCode::Char('c') => self.cancel_selected_snippet_run(),
            KeyCode::Char('C') => {
                self.snippet_runs.retain(|run| {
                    matches!(
                        run.status,
                        SnippetRunStatus::Queued | SnippetRunStatus::Running
                    )
                });
                self.selected_snippet_run = self
                    .selected_snippet_run
                    .min(self.snippet_runs.len().saturating_sub(1));
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.snippet_concurrency = (self.snippet_concurrency + 1).min(16)
            }
            KeyCode::Char('-') => {
                self.snippet_concurrency = self.snippet_concurrency.saturating_sub(1).max(1)
            }
            KeyCode::Char('Y') => {
                if let Some(snippet) = self.selected_snippet().cloned() {
                    return InputAction::CopySnippet {
                        command: snippet.command,
                    };
                }
            }
            KeyCode::Up => self.selected_snippet = self.selected_snippet.saturating_sub(1),
            KeyCode::Down => {
                self.selected_snippet = (self.selected_snippet + 1).min(snippets.saturating_sub(1))
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Esc => self.snippet_package_open = false,
            KeyCode::Enter | KeyCode::Char('r') | KeyCode::Char('x') => {
                if self.metadata.snippet_packages[self.selected_package]
                    .snippets
                    .get(self.selected_snippet)
                    .is_some()
                {
                    self.open_snippet_host_picker();
                }
            }
            KeyCode::Char('e') => {
                if let Some(snippet) = self.metadata.snippet_packages[self.selected_package]
                    .snippets
                    .get(self.selected_snippet)
                    .cloned()
                {
                    let package_id = self.metadata.snippet_packages[self.selected_package].id;
                    self.open_snippet_form(package_id, Some(snippet));
                }
            }
            KeyCode::Char('d') => {
                if let Some(snippet) = self.metadata.snippet_packages[self.selected_package]
                    .snippets
                    .get(self.selected_snippet)
                {
                    self.snippet_delete_confirmation = Some(SnippetDeleteTarget::Snippet {
                        package_id: self.metadata.snippet_packages[self.selected_package].id,
                        id: snippet.id,
                        name: snippet.name.clone(),
                    });
                }
            }
            KeyCode::Char('a') => {
                let package_id = self.metadata.snippet_packages[self.selected_package].id;
                self.open_snippet_form(package_id, None);
            }
            KeyCode::Char('b') => {
                if self.metadata.snippet_packages[self.selected_package]
                    .snippets
                    .get(self.selected_snippet)
                    .is_some()
                {
                    self.open_snippet_host_picker();
                }
            }
            _ => {}
        }
        InputAction::None
    }

    fn open_snippet_form(&mut self, package_id: u64, snippet: Option<SnippetDefinition>) {
        let editing_id = snippet.as_ref().map(|item| item.id);
        self.snippet_dialog = Some(SnippetDialog {
            fields: snippet.map_or_else(
                || std::array::from_fn(|_| String::new()),
                |item| {
                    [
                        item.name,
                        item.command,
                        item.description,
                        item.dangerous.to_string(),
                    ]
                },
            ),
            package_id,
            editing_id,
            selected_field: 0,
        });
    }

    fn open_package_form(&mut self, package: Option<SnippetPackage>) {
        self.package_dialog = Some(PackageDialog {
            fields: package.as_ref().map_or_else(
                || [String::new(), String::new()],
                |item| [item.name.clone(), item.description.clone()],
            ),
            editing_id: package.map(|item| item.id),
            selected_field: 0,
        });
    }

    fn handle_snippet_delete_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('y') | KeyCode::Enter => {
                let Some(target) = self.snippet_delete_confirmation.take() else {
                    return;
                };
                match target {
                    SnippetDeleteTarget::Package { id, name } => {
                        self.metadata
                            .snippet_packages
                            .retain(|package| package.id != id);
                        self.selected_package = self
                            .selected_package
                            .min(self.metadata.snippet_packages.len().saturating_sub(1));
                        self.snippet_package_open = false;
                        self.selected_snippet = 0;
                        self.notice = Some(format!("Deleted package {name}."));
                    }
                    SnippetDeleteTarget::Snippet {
                        package_id,
                        id,
                        name,
                    } => {
                        if let Some(package) = self
                            .metadata
                            .snippet_packages
                            .iter_mut()
                            .find(|package| package.id == package_id)
                        {
                            package.snippets.retain(|snippet| snippet.id != id);
                            self.selected_snippet = self
                                .selected_snippet
                                .min(package.snippets.len().saturating_sub(1));
                        }
                        self.notice = Some(format!("Deleted snippet {name}."));
                    }
                }
                self.save_metadata();
            }
            KeyCode::Char('n') | KeyCode::Esc => self.snippet_delete_confirmation = None,
            _ => {}
        }
    }

    fn handle_package_dialog_key(&mut self, key: KeyCode) {
        let Some(dialog) = self.package_dialog.as_mut() else {
            return;
        };
        match key {
            KeyCode::Esc => self.package_dialog = None,
            KeyCode::Tab | KeyCode::Down => dialog.selected_field = (dialog.selected_field + 1) % 2,
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field = (dialog.selected_field + 1) % 2
            }
            KeyCode::Backspace => {
                dialog.fields[dialog.selected_field].pop();
            }
            KeyCode::Enter => {
                if dialog.selected_field == 0 {
                    dialog.selected_field = 1;
                    return;
                }
                let Some(dialog) = self.package_dialog.take() else {
                    return;
                };
                let name = dialog.fields[0].trim();
                if name.is_empty() {
                    self.notice = Some("Package name is required.".to_string());
                    self.package_dialog = Some(dialog);
                    return;
                }
                let id = dialog.editing_id.unwrap_or_else(|| {
                    self.metadata
                        .snippet_packages
                        .iter()
                        .map(|package| package.id)
                        .max()
                        .unwrap_or_default()
                        .saturating_add(1)
                });
                let package = SnippetPackage {
                    id,
                    name: name.to_string(),
                    description: dialog.fields[1].trim().to_string(),
                    snippets: self
                        .metadata
                        .snippet_packages
                        .iter()
                        .find(|item| item.id == id)
                        .map_or_else(Vec::new, |item| item.snippets.clone()),
                };
                if let Some(existing) = self
                    .metadata
                    .snippet_packages
                    .iter_mut()
                    .find(|item| item.id == id)
                {
                    *existing = package;
                } else {
                    self.metadata.snippet_packages.push(package);
                    self.selected_package = self.metadata.snippet_packages.len() - 1;
                }
                self.notice = Some(format!("Saved package {name}."));
                self.save_metadata();
            }
            KeyCode::Char(value) if !value.is_control() => {
                dialog.fields[dialog.selected_field].push(value)
            }
            _ => {}
        }
    }

    fn handle_snippet_dialog_key(&mut self, key: KeyCode) {
        let Some(dialog) = self.snippet_dialog.as_mut() else {
            return;
        };
        match key {
            KeyCode::Esc => self.snippet_dialog = None,
            KeyCode::Tab | KeyCode::Down => {
                dialog.selected_field = (dialog.selected_field + 1) % dialog.fields.len()
            }
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field =
                    (dialog.selected_field + dialog.fields.len() - 1) % dialog.fields.len()
            }
            KeyCode::Backspace => {
                dialog.fields[dialog.selected_field].pop();
            }
            KeyCode::Enter => {
                if dialog.selected_field + 1 < dialog.fields.len() {
                    dialog.selected_field += 1;
                    return;
                }
                let Some(dialog) = self.snippet_dialog.take() else {
                    return;
                };
                let name = dialog.fields[0].trim();
                let command = dialog.fields[1].trim();
                if name.is_empty() || command.is_empty() {
                    self.notice = Some("Snippet name and command are required.".to_string());
                    self.snippet_dialog = Some(dialog);
                    return;
                }
                let dangerous = matches!(
                    dialog.fields[3].trim().to_ascii_lowercase().as_str(),
                    "y" | "yes" | "true" | "1"
                );
                let id = dialog.editing_id.unwrap_or_else(|| {
                    self.metadata
                        .snippet_packages
                        .iter()
                        .flat_map(|package| package.snippets.iter())
                        .map(|snippet| snippet.id)
                        .max()
                        .unwrap_or_default()
                        .saturating_add(1)
                });
                let snippet = SnippetDefinition {
                    id,
                    name: name.to_string(),
                    command: command.to_string(),
                    description: dialog.fields[2].trim().to_string(),
                    dangerous,
                };
                if let Some(package) = self
                    .metadata
                    .snippet_packages
                    .iter_mut()
                    .find(|package| package.id == dialog.package_id)
                {
                    if let Some(existing) = package.snippets.iter_mut().find(|item| item.id == id) {
                        *existing = snippet;
                    } else {
                        package.snippets.push(snippet);
                    }
                    package.snippets.sort_by_key(|item| item.id);
                    self.notice = Some(format!("Saved snippet {name}."));
                    self.save_metadata();
                }
            }
            KeyCode::Char(value) if !value.is_control() => {
                dialog.fields[dialog.selected_field].push(value)
            }
            _ => {}
        }
    }

    fn snippet_host_picker_items(&self) -> Vec<HostPickerItem> {
        if let Some(group) = &self.snippet_host_picker_group {
            return self
                .hosts
                .hosts
                .iter()
                .filter(|host| self.matches_group(host, group))
                .map(|host| HostPickerItem::Host(host.alias.clone()))
                .collect();
        }
        self.visible_groups()
            .into_iter()
            .map(HostPickerItem::Group)
            .chain(
                self.hosts
                    .hosts
                    .iter()
                    .map(|host| HostPickerItem::Host(host.alias.clone())),
            )
            .collect()
    }

    fn open_snippet_host_picker(&mut self) {
        self.snippet_host_picker_open = true;
        self.snippet_host_picker_group = None;
        self.snippet_host_picker_selection = 0;
    }

    fn handle_snippet_host_picker_key(&mut self, key: KeyCode) -> InputAction {
        let items = self.snippet_host_picker_items();
        match key {
            KeyCode::Char('q') => {
                self.should_quit = true;
                self.snippet_host_picker_open = false;
            }
            KeyCode::Esc => self.snippet_host_picker_open = false,
            KeyCode::Backspace => {
                if self.snippet_host_picker_group.is_some() {
                    self.snippet_host_picker_group = None;
                    self.snippet_host_picker_selection = 0;
                } else {
                    self.snippet_host_picker_open = false;
                }
            }
            KeyCode::Up => {
                self.snippet_host_picker_selection =
                    self.snippet_host_picker_selection.saturating_sub(1)
            }
            KeyCode::Down => {
                self.snippet_host_picker_selection =
                    (self.snippet_host_picker_selection + 1).min(items.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                let Some(item) = items.get(self.snippet_host_picker_selection).cloned() else {
                    return InputAction::None;
                };
                match item {
                    HostPickerItem::Group(group) => {
                        self.snippet_host_picker_group = Some(group);
                        self.snippet_host_picker_selection = 0;
                    }
                    HostPickerItem::Host(alias) => {
                        self.snippet_host_picker_open = false;
                        let Some(package) =
                            self.metadata.snippet_packages.get(self.selected_package)
                        else {
                            return InputAction::None;
                        };
                        let Some(snippet) = package.snippets.get(self.selected_snippet) else {
                            return InputAction::None;
                        };
                        self.notice = Some(format!("Running {} on {alias}...", snippet.name));
                        return self.snippet_run_action(vec![alias]);
                    }
                }
            }
            KeyCode::Char('b') => {
                if let Some(HostPickerItem::Group(group)) =
                    items.get(self.snippet_host_picker_selection).cloned()
                {
                    let aliases = self
                        .hosts
                        .hosts
                        .iter()
                        .filter(|host| self.matches_group(host, &group))
                        .map(|host| host.alias.clone())
                        .collect::<Vec<_>>();
                    self.snippet_host_picker_open = false;
                    return self.snippet_run_action(aliases);
                }
            }
            _ => {}
        }
        InputAction::None
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

    pub fn selected_tunnel_definition(&self) -> Option<&TunnelDefinition> {
        self.metadata.tunnels.get(self.selected_tunnel)
    }

    fn open_tunnel_form(&mut self, tunnel: Option<TunnelDefinition>) {
        self.open_tunnel_form_with_kind(tunnel, TunnelKind::Local);
    }

    fn open_tunnel_form_with_kind(
        &mut self,
        tunnel: Option<TunnelDefinition>,
        default_kind: TunnelKind,
    ) {
        let kind = tunnel.as_ref().map_or(default_kind, |tunnel| tunnel.kind);
        let mut fields = tunnel.as_ref().map_or_else(
            || {
                [
                    String::new(),
                    self.host_context.clone().unwrap_or_default(),
                    "127.0.0.1".to_string(),
                    String::new(),
                    if kind == TunnelKind::Dynamic {
                        String::new()
                    } else {
                        "127.0.0.1".to_string()
                    },
                    String::new(),
                    "10".to_string(),
                    "30".to_string(),
                ]
            },
            |tunnel| {
                [
                    tunnel.label.clone(),
                    tunnel.host_alias.clone(),
                    tunnel.bind_address.clone(),
                    tunnel.local_port.to_string(),
                    tunnel.remote_host.clone().unwrap_or_default(),
                    tunnel
                        .remote_port
                        .map_or_else(String::new, |port| port.to_string()),
                    tunnel.connect_timeout_secs.to_string(),
                    tunnel.keepalive_interval_secs.to_string(),
                ]
            },
        );
        if kind == TunnelKind::Dynamic {
            fields[4].clear();
            fields[5].clear();
        }
        self.tunnel_dialog = Some(TunnelDialog {
            fields,
            kind,
            editing_id: tunnel.map(|tunnel| tunnel.id),
            selected_field: 0,
            deleting: false,
        });
    }

    fn open_tunnel_delete(&mut self) {
        let Some(tunnel) = self.selected_tunnel_definition().cloned() else {
            self.notice = Some("Select a tunnel first.".to_string());
            return;
        };
        self.tunnel_dialog = Some(TunnelDialog {
            fields: [
                tunnel.label,
                tunnel.host_alias,
                tunnel.bind_address,
                tunnel.local_port.to_string(),
                tunnel.remote_host.unwrap_or_default(),
                tunnel
                    .remote_port
                    .map_or_else(String::new, |port| port.to_string()),
                tunnel.connect_timeout_secs.to_string(),
                tunnel.keepalive_interval_secs.to_string(),
            ],
            kind: tunnel.kind,
            editing_id: Some(tunnel.id),
            selected_field: 0,
            deleting: true,
        });
    }

    fn open_tunnel_host_picker(&mut self) {
        if self.hosts.hosts.is_empty() {
            self.notice = Some("No SSH Hosts are available to select.".to_string());
            return;
        }
        self.host_picker_group = None;
        self.host_picker_selection = 0;
        self.tunnel_host_picker_open = true;
    }

    fn handle_tunnel_host_picker_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Backspace => {
                if self.host_picker_group.is_some() {
                    self.host_picker_group = None;
                    self.host_picker_selection = 0;
                } else {
                    self.tunnel_host_picker_open = false;
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
                        if let Some(dialog) = self.tunnel_dialog.as_mut() {
                            dialog.fields[1] = alias;
                        }
                        self.host_picker_group = None;
                        self.host_picker_selection = 0;
                        self.tunnel_host_picker_open = false;
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn handle_tunnel_dialog_key(&mut self, key: KeyCode) {
        if self.tunnel_host_picker_open {
            self.handle_tunnel_host_picker_key(key);
            return;
        }
        match key {
            KeyCode::F(1) => {
                self.set_tunnel_dialog_kind(TunnelKind::Local);
                return;
            }
            KeyCode::F(2) => {
                self.set_tunnel_dialog_kind(TunnelKind::Remote);
                return;
            }
            KeyCode::F(3) => {
                self.set_tunnel_dialog_kind(TunnelKind::Dynamic);
                return;
            }
            _ => {}
        }
        if key == KeyCode::Enter
            && self
                .tunnel_dialog
                .as_ref()
                .is_some_and(|dialog| !dialog.deleting && dialog.selected_field == 1)
        {
            self.open_tunnel_host_picker();
            return;
        }
        let Some(dialog) = self.tunnel_dialog.as_mut() else {
            return;
        };
        if dialog.deleting {
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let Some(id) = dialog.editing_id else {
                        self.tunnel_dialog = None;
                        return;
                    };
                    let label = dialog.fields[0].clone();
                    self.stop_tunnel(id);
                    self.metadata.tunnels.retain(|tunnel| tunnel.id != id);
                    self.tunnel_statuses.remove(&id);
                    self.tunnel_dialog = None;
                    self.selected_tunnel = self
                        .selected_tunnel
                        .min(self.metadata.tunnels.len().saturating_sub(1));
                    self.notice = Some(format!("Deleted tunnel {label}."));
                    self.save_metadata();
                }
                KeyCode::Char('n') | KeyCode::Esc => self.tunnel_dialog = None,
                _ => {}
            }
            return;
        }

        match key {
            KeyCode::Esc => self.tunnel_dialog = None,
            KeyCode::Tab | KeyCode::Down => {
                dialog.selected_field = (dialog.selected_field + 1) % dialog.fields.len()
            }
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field =
                    (dialog.selected_field + dialog.fields.len() - 1) % dialog.fields.len()
            }
            KeyCode::Backspace => {
                dialog.fields[dialog.selected_field].pop();
            }
            KeyCode::Enter => {
                if dialog.selected_field + 1 < dialog.fields.len() {
                    dialog.selected_field += 1;
                    return;
                }
                let Some(dialog) = self.tunnel_dialog.take() else {
                    return;
                };
                let label = dialog.fields[0].trim();
                let host_alias = dialog.fields[1].trim();
                let bind_address = dialog.fields[2].trim();
                let remote_host = dialog.fields[4].trim();
                let local_port = match dialog.fields[3].trim().parse::<u16>() {
                    Ok(port) if port > 0 => port,
                    _ => {
                        self.notice =
                            Some("Local port must be an integer from 1 to 65535.".to_string());
                        self.tunnel_dialog = Some(dialog);
                        return;
                    }
                };
                let remote_port = if dialog.kind == TunnelKind::Dynamic {
                    None
                } else {
                    match dialog.fields[5].trim().parse::<u16>() {
                        Ok(port) if port > 0 => Some(port),
                        _ => {
                            self.notice = Some(
                                "Destination port must be an integer from 1 to 65535.".to_string(),
                            );
                            self.tunnel_dialog = Some(dialog);
                            return;
                        }
                    }
                };
                let connect_timeout_secs = match dialog.fields[6].trim().parse::<u64>() {
                    Ok(value) if value > 0 => value.min(300),
                    _ => {
                        self.notice = Some("Connect timeout must be 1-300 seconds.".to_string());
                        self.tunnel_dialog = Some(dialog);
                        return;
                    }
                };
                let keepalive_interval_secs = match dialog.fields[7].trim().parse::<u64>() {
                    Ok(value) => value.min(3600),
                    _ => {
                        self.notice =
                            Some("KeepAlive interval must be 0-3600 seconds.".to_string());
                        self.tunnel_dialog = Some(dialog);
                        return;
                    }
                };
                if label.is_empty() || host_alias.is_empty() || bind_address.is_empty() {
                    self.notice =
                        Some("Label, SSH host, and Bind address are required.".to_string());
                    self.tunnel_dialog = Some(dialog);
                    return;
                }
                if dialog.kind != TunnelKind::Dynamic && remote_host.is_empty() {
                    self.notice = Some("Destination address is required.".to_string());
                    self.tunnel_dialog = Some(dialog);
                    return;
                }
                let custom_host = !self.hosts.hosts.iter().any(|host| host.alias == host_alias);
                let id = dialog.editing_id.unwrap_or_else(|| {
                    self.metadata
                        .tunnels
                        .iter()
                        .map(|tunnel| tunnel.id)
                        .max()
                        .unwrap_or_default()
                        .saturating_add(1)
                });
                if dialog.editing_id.is_some() {
                    self.stop_tunnel(id);
                }
                let tunnel = TunnelDefinition {
                    id,
                    label: label.to_string(),
                    host_alias: host_alias.to_string(),
                    custom_host,
                    kind: dialog.kind,
                    bind_address: bind_address.to_string(),
                    local_port,
                    remote_host: if dialog.kind == TunnelKind::Dynamic {
                        None
                    } else {
                        Some(remote_host.to_string())
                    },
                    remote_port,
                    lifecycle: TunnelLifecycle::App,
                    auto_reconnect: false,
                    connect_timeout_secs,
                    keepalive_interval_secs,
                };
                if let Some(existing) = self.metadata.tunnels.iter_mut().find(|t| t.id == id) {
                    *existing = tunnel;
                } else {
                    self.metadata.tunnels.push(tunnel);
                }
                self.metadata.tunnels.sort_by_key(|tunnel| tunnel.id);
                self.selected_tunnel = self
                    .metadata
                    .tunnels
                    .iter()
                    .position(|tunnel| tunnel.id == id)
                    .unwrap_or_default();
                self.tunnel_statuses.insert(id, TunnelStatus::Stopped);
                self.notice = Some(format!("Saved tunnel {label}."));
                self.save_metadata();
            }
            KeyCode::Char(value) if !value.is_control() => {
                dialog.fields[dialog.selected_field].push(value)
            }
            _ => {}
        }
    }

    fn set_tunnel_dialog_kind(&mut self, kind: TunnelKind) {
        let Some(dialog) = self.tunnel_dialog.as_mut() else {
            return;
        };
        if dialog.deleting {
            return;
        }
        dialog.kind = kind;
        if kind == TunnelKind::Dynamic {
            dialog.fields[4].clear();
            dialog.fields[5].clear();
        } else if dialog.fields[4].is_empty() {
            dialog.fields[4] = "127.0.0.1".to_string();
        }
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
                            self.metadata
                                .tunnels
                                .retain(|tunnel| tunnel.custom_host || tunnel.host_alias != alias);
                            self.stop_orphaned_tunnels();
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
                    proxy_jump: None,
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
                            for tunnel in &mut self.metadata.tunnels {
                                if !tunnel.custom_host && tunnel.host_alias == *original {
                                    tunnel.host_alias = host.alias.clone();
                                }
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
            KeyCode::Up | KeyCode::Down => match self.active_file_pane {
                FilePane::Local => self.move_local_selection(key),
                FilePane::Remote => self.move_file_selection(key),
            },
            KeyCode::Left => match self.active_file_pane {
                FilePane::Local => self.go_to_local_parent(),
                FilePane::Remote => {
                    let _ = self.go_to_remote_parent();
                }
            },
            KeyCode::Right => {
                self.file_filtering = false;
                match self.active_file_pane {
                    FilePane::Local => self.open_local_selection(),
                    FilePane::Remote => {
                        let _ = self.open_remote_selection();
                    }
                }
            }
            KeyCode::Esc => {
                self.file_filtering = false;
                self.file_filter.clear();
                self.local_files.selected = 0;
            }
            KeyCode::Backspace => {
                self.file_filter.pop();
                self.local_files.selected = 0;
                self.selected_file = 0;
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
                self.selected_file = 0;
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

    pub(crate) fn next_section(&mut self) {
        let index = NavSection::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.switch_section(NavSection::ALL[(index + 1) % NavSection::ALL.len()]);
    }

    pub(crate) fn previous_section(&mut self) {
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

    pub(crate) fn save_metadata(&mut self) -> bool {
        if self.metadata_error {
            self.notice = Some(
                "Metadata unavailable; changes cannot be saved. Reload after fixing hosts.json."
                    .into(),
            );
            false
        } else {
            self.metadata.reconcile(&self.hosts);
            #[cfg(test)]
            let result = self
                .metadata_save_path
                .as_ref()
                .map_or_else(|| self.metadata.save(), |path| self.metadata.save_to(path));
            #[cfg(not(test))]
            let result = self.metadata.save();
            if let Err(error) = result {
                self.notice = Some(format!("Could not save host metadata: {error}"));
                false
            } else {
                true
            }
        }
    }

    fn reconcile_metadata(&mut self) {
        if !self.metadata_error && self.metadata.reconcile(&self.hosts) {
            self.save_metadata();
        }
        self.stop_orphaned_tunnels();
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

impl Drop for App {
    fn drop(&mut self) {
        let ids = self.tunnel_processes.keys().copied().collect::<Vec<_>>();
        for id in ids {
            self.stop_tunnel(id);
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
    fn workspace_forms_navigation_editing_and_delete_share_existing_semantics() {
        let fixture = crate::projects::tests::Fixture::new("forms");
        let mut app = app_fixture();
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        press(&mut app, KeyCode::Char('6'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(app.workspace.form.is_some());
        press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Projects);
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec!["Demo".into(), fixture.root.display().to_string()];
        form.selected = 1;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.selected_project().unwrap().name, "Demo");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.workspace.project_open);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec!["Check".into(), "printf check".into(), String::new()];
        form.selected = 2;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.workspace.output_open);
        assert!(
            app.project_service.tasks.is_empty(),
            "Enter must never launch a command"
        );
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(
            app.workspace.form.as_ref().unwrap().fields[1],
            "printf check"
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(app.selected_project().unwrap().commands.len(), 1);
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(app.selected_project().unwrap().commands.is_empty());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.metadata.projects.is_empty());
    }

    #[test]
    fn workspace_failed_save_keeps_form_and_rolls_back_changes_and_deletion() {
        let fixture = crate::projects::tests::Fixture::new("save-failure");
        let mut app = app_fixture();
        app.section = NavSection::Projects;
        app.metadata_error = false;
        let blocked = fixture.root.join("blocked");
        std::fs::write(&blocked, "not a directory").unwrap();
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        let form = app.workspace.form.as_mut().unwrap();
        form.fields = vec!["Demo".into(), fixture.root.display().to_string()];
        form.selected = 1;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.metadata.projects.is_empty());
        assert!(app.workspace.form.as_ref().unwrap().error.is_some());
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.workspace.form.is_none());
        assert_eq!(app.metadata.projects.len(), 1);
        app.metadata_save_path = Some(blocked.join("hosts.json"));
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.metadata.projects.len(), 1);
        assert!(app.notice.as_ref().unwrap().contains("Could not save"));
    }

    fn wait_for_workspace(app: &mut App, condition: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            app.poll_projects();
            app.poll_runtime();
            if condition(app) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Workspace timed out: {:?}; inspection: {:?}",
                app.notice,
                app.workspace.monitor.error
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn external_project_running_overrides_last_failure_blocks_duplicates_and_restarts_only_after_confirmation()
     {
        use crate::project_monitor::tests::ExternalFixture;
        use crate::projects::tests::until;
        let external = ExternalFixture::new();
        let key = (external.project.id, external.command.id);
        let mut app = app_fixture();
        app.project_service = external.fixture.service();
        app.metadata.projects.push(external.project.clone());
        let failed = crate::projects::ProjectCommand {
            command: "printf 'previous failure\\n'; exit 1".into(),
            ..external.command.clone()
        };
        app.project_service
            .start(&external.project, &failed)
            .unwrap();
        until(&mut app.project_service, |s| !s.active(key));
        let previous_run = app.project_service.tasks[&key].owner.run_id;
        press(&mut app, KeyCode::F(6), KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| !app.external_project_runs(key).is_empty());
        let (_, port) = external.fixture.ready().unwrap();
        assert!(app.project_is_running(key));
        assert!(app.external_project_runs(key)[0].ports.contains(&port));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let text = format!("{}", terminal.backend());
        assert!(text.contains("external running"));
        assert!(text.contains("last run: exit 1"));
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| {
            app.notice
                .as_ref()
                .is_some_and(|notice| notice.contains("already running externally"))
        });
        assert_eq!(app.project_service.tasks[&key].owner.run_id, previous_run);
        assert!(!app.project_service.active(key));
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| !app.runtime_loading);
        app.selected_runtime = app
            .visible_runtime_entries()
            .iter()
            .position(|entry| entry.pid == Some(external.child.id()))
            .expect("external listener visible");
        assert_eq!(
            app.selected_runtime_entry().unwrap().origin,
            runtime::RuntimeOrigin::ExternalProject
        );
        assert_eq!(
            app.selected_runtime_entry()
                .unwrap()
                .external_project
                .as_ref()
                .unwrap()
                .key,
            key
        );
        press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
        assert!(app.workspace.output_open);
        assert_eq!(app.selected_project_key(), Some(key));
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        assert!(format!("{}", terminal.backend()).contains("Last managed run: exit 1"));
        assert!(format!("{}", terminal.backend()).contains("previous failure"));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| app.workspace.confirmation.is_some());
        press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Projects);
        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| app.workspace.confirmation.is_some());
        assert_eq!(
            app.workspace.confirmation.as_ref().unwrap().action,
            crate::project_app::ProjectAction::Restart
        );
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| {
            app.project_service.active(key)
                && external
                    .fixture
                    .ready()
                    .is_some_and(|(pid, _)| pid != external.child.id())
        });
        assert_ne!(app.project_service.tasks[&key].owner.run_id, previous_run);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        until(&mut app.project_service, |s| !s.active(key));
    }

    #[test]
    fn workspace_listener_runtime_output_and_confirmed_stop_form_a_complete_flow() {
        use crate::projects::tests::{Fixture, until};
        let fixture = Fixture::new("runtime-flow");
        let mut app = app_fixture();
        app.project_service = fixture.service();
        let command = fixture.listener_command(false);
        let key = (fixture.project.id, command.id);
        let mut project = fixture.project.clone();
        project.commands.push(command);
        app.metadata.projects.push(project);
        press(&mut app, KeyCode::F(6), KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        let deadline = Instant::now() + Duration::from_secs(10);
        while fixture.ready().is_none() {
            app.poll_projects();
            assert!(
                Instant::now() < deadline,
                "Project start preflight timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let (pid, port) = fixture.ready().unwrap();
        assert!(app.project_service.active(key));
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert!(
            app.workspace.form.is_none(),
            "Live commands must not be edited"
        );
        press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.runtime_loading {
            app.project_service.poll();
            app.poll_runtime();
            assert!(Instant::now() < deadline, "Runtime inspection timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.runtime_error.is_none(), "{:?}", app.runtime_error);
        app.selected_runtime = app
            .visible_runtime_entries()
            .iter()
            .position(|entry| entry.pid == Some(pid) && entry.port == port)
            .expect("fixture listener must be visible in Mine");
        let entry = app.selected_runtime_entry().unwrap();
        assert_eq!(entry.origin, runtime::RuntimeOrigin::Project);
        assert_eq!(entry.project.as_ref().unwrap().key, key);
        press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Projects);
        assert!(app.workspace.output_open);
        assert_eq!(app.selected_project_key(), Some(key));
        // Return without refreshing so the action uses the displayed run identity.
        app.section = NavSection::Runtime;
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(app.runtime_kill_confirmation.is_some());
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        until(&mut app.project_service, |s| !s.active(key));
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
        assert_eq!(
            app.project_service.tasks[&key].status,
            crate::projects::TaskStatus::Stopped
        );
    }

    #[test]
    fn stale_workspace_run_cannot_stop_a_replacement_and_remote_results_cannot_claim_it() {
        use crate::projects::tests::Fixture;
        let fixture = Fixture::new("stale");
        let mut app = app_fixture();
        app.project_service = fixture.service();
        let command = fixture.command("sleep 30");
        app.project_service
            .start(&fixture.project, &command)
            .unwrap();
        let owner = app.project_service.owners().remove(0);
        let mut stale = owner.clone();
        stale.run_id += 1;
        let entry = RuntimeEntry {
            protocol: "TCP".into(),
            address: "127.0.0.1".into(),
            port: 3000,
            pid: Some(owner.process_group),
            process: "fixture".into(),
            user: "0".into(),
            command: "fixture".into(),
            working_directory: None,
            owned_by_current_user: true,
            kind: runtime::RuntimeProcessKind::Application,
            origin: runtime::RuntimeOrigin::Project,
            origin_reason: String::new(),
            project: Some(stale),
            external_project: None,
        };
        app.section = NavSection::Runtime;
        app.runtime_kill_confirmation = Some(entry.clone());
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.project_service.active(owner.key));
        assert_eq!(
            app.project_service.tasks[&owner.key].status,
            crate::projects::TaskStatus::Running
        );
        app.runtime_target = RuntimeTarget::Host("fixture".into());
        app.runtime_sender
            .send(RuntimeEvent::Listed {
                target: app.runtime_target.clone(),
                request: app.runtime_request,
                entries: vec![RuntimeEntry {
                    project: Some(owner),
                    ..entry
                }],
            })
            .unwrap();
        app.poll_runtime();
        assert!(app.runtime_entries[0].project.is_none());
        assert!(app.runtime_entries[0].pid.is_none());
        assert_ne!(
            app.runtime_entries[0].origin,
            runtime::RuntimeOrigin::Project
        );
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
                proxy_jump: None,
                managed: false,
            },
            Host {
                alias: "two".to_string(),
                hostname: None,
                user: None,
                port: None,
                identity_file: None,
                proxy_jump: None,
                managed: false,
            },
        ];
        app.section = NavSection::Projects;
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.section, NavSection::Hosts);
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_browse_item, 2);
        assert_eq!(app.selected_host().unwrap().alias, "two");
    }

    #[test]
    fn runtime_enter_stays_on_listener_and_s_opens_host_picker() {
        let mut app = app_fixture();
        app.section = NavSection::Runtime;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.runtime_host_picker_open);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.runtime_host_picker_open);
    }

    fn runtime_listener(port: u16) -> RuntimeEntry {
        RuntimeEntry {
            protocol: "TCP".into(),
            address: "127.0.0.1".into(),
            port,
            pid: Some(u32::from(port)),
            process: "fixture".into(),
            user: "501".into(),
            command: format!("fixture --port {port}"),
            working_directory: Some("/tmp/project".into()),
            owned_by_current_user: true,
            kind: runtime::RuntimeProcessKind::Application,
            origin: runtime::RuntimeOrigin::CommandLine,
            origin_reason: "Shell ancestor".into(),
            project: None,
            external_project: None,
        }
    }

    #[test]
    fn runtime_project_link_restores_the_commands_group_context() {
        let fixture = crate::projects::tests::Fixture::new("runtime-group-link");
        let mut app = app_fixture();
        let mut project = fixture.project.clone();
        let mut command = fixture.command("printf check");
        command.group = Some("Checks".into());
        project.commands.push(command.clone());
        project.groups = vec!["Services".into(), "Checks".into()];
        app.metadata.projects.push(project.clone());
        let mut entry = runtime_listener(3000);
        entry.project = Some(crate::projects::ProjectOwner {
            key: (project.id, command.id),
            run_id: 1,
            process_group: 123,
            project_name: project.name.clone(),
            command_name: command.name.clone(),
        });
        app.runtime_entries.push(entry);
        app.section = NavSection::Projects;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert!(app.project_group_collapsed("Checks"));
        app.section = NavSection::Runtime;
        app.open_project_from_runtime();
        assert!(!app.project_group_collapsed("Checks"));
        assert!(app.workspace.output_open);
        assert_eq!(app.selected_project_key(), Some((project.id, command.id)));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.selected_project_key(), Some((project.id, command.id)));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.workspace.project_open);
    }

    #[test]
    fn runtime_keeps_independent_views_for_local_and_each_host() {
        let mut app = app_fixture();
        app.runtime_entries = (3000..3020).map(runtime_listener).collect();
        app.runtime_filter = RuntimeFilter::All;
        app.selected_runtime = 14;
        app.runtime_list_offset = app.runtime_scroll_offset(24);
        let local_offset = app.runtime_list_offset;
        assert!(local_offset > 0);
        app.restore_runtime_view(RuntimeTarget::Host("web".into()));
        assert_eq!(app.runtime_filter, RuntimeFilter::Mine);
        assert!(app.runtime_entries.is_empty());
        assert_eq!(app.selected_runtime, 0);
        app.runtime_entries = vec![runtime_listener(8080), runtime_listener(8081)];
        app.runtime_filter = RuntimeFilter::Applications;
        app.selected_runtime = 1;
        app.restore_runtime_view(RuntimeTarget::Host("db".into()));
        app.runtime_entries = vec![runtime_listener(5432)];
        app.runtime_filter = RuntimeFilter::All;
        app.restore_runtime_view(RuntimeTarget::Local);
        assert_eq!(app.runtime_filter, RuntimeFilter::All);
        assert_eq!(app.selected_runtime_entry().unwrap().port, 3014);
        assert_eq!(app.runtime_scroll_offset(24), local_offset);
        app.restore_runtime_view(RuntimeTarget::Host("web".into()));
        assert_eq!(app.runtime_filter, RuntimeFilter::Applications);
        assert_eq!(app.selected_runtime_entry().unwrap().port, 8081);
        app.restore_runtime_view(RuntimeTarget::Host("db".into()));
        assert_eq!(app.selected_runtime_entry().unwrap().port, 5432);
    }

    #[test]
    fn runtime_rejects_old_machine_requests_and_preserves_selected_listener_on_refresh() {
        let mut app = app_fixture();
        app.runtime_entries = vec![runtime_listener(3000), runtime_listener(3001)];
        app.selected_runtime = 1;
        app.runtime_request = 3;
        app.runtime_loading = true;
        for (target, request) in [
            (RuntimeTarget::Host("web".into()), 3),
            (RuntimeTarget::Local, 1),
        ] {
            app.runtime_sender
                .send(RuntimeEvent::Listed {
                    target: target.clone(),
                    request,
                    entries: vec![runtime_listener(9000)],
                })
                .unwrap();
            app.runtime_sender
                .send(RuntimeEvent::Failed {
                    target,
                    request,
                    message: "obsolete".into(),
                })
                .unwrap();
        }
        app.poll_runtime();
        assert!(app.runtime_loading);
        assert!(app.runtime_error.is_none());
        assert_eq!(app.selected_runtime_entry().unwrap().port, 3001);
        app.runtime_sender
            .send(RuntimeEvent::Listed {
                target: RuntimeTarget::Local,
                request: 3,
                entries: vec![
                    runtime_listener(2000),
                    runtime_listener(3000),
                    runtime_listener(3001),
                ],
            })
            .unwrap();
        app.poll_runtime();
        assert!(!app.runtime_loading);
        assert_eq!(app.selected_runtime, 2);
        assert_eq!(app.selected_runtime_entry().unwrap().port, 3001);
    }

    #[test]
    fn runtime_mouse_targets_categories_and_scrolled_rows_obey_confirmation() {
        let mut app = app_fixture();
        app.section = NavSection::Runtime;
        app.runtime_entries = (3000..3020).map(runtime_listener).collect();
        app.selected_runtime = 15;
        let viewport = ratatui::layout::Rect::new(0, 0, 120, 30);
        app.viewport_width = viewport.width;
        app.viewport_height = viewport.height;
        app.runtime_list_offset = app.runtime_scroll_offset(viewport.height);
        let layout = crate::ui::runtime_layout(viewport);
        let expected = app.runtime_list_offset + 1;
        app.handle_mouse(layout.listeners.x + 3, layout.listeners.y + 4, 120, 30);
        assert_eq!(app.selected_runtime, expected);
        let filters = crate::ui::runtime_filter_areas(layout.filters, &app);
        app.handle_mouse(filters[3].x + 1, filters[3].y, 120, 30);
        assert_eq!(app.runtime_filter, RuntimeFilter::All);
        assert_eq!(app.selected_runtime, 0);
        assert_eq!(app.runtime_list_offset, 0);
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(app.runtime_kill_confirmation.is_some());
        app.handle_mouse(layout.hosts.x + 1, layout.hosts.y, 120, 30);
        assert!(!app.runtime_host_picker_open);
        app.switch_runtime_target(RuntimeTarget::Host("other".into()));
        assert_eq!(app.runtime_target, RuntimeTarget::Local);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.handle_mouse(layout.hosts.x + 1, layout.hosts.y, 120, 30);
        assert!(app.runtime_host_picker_open);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.runtime_loading = true;
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(app.runtime_kill_confirmation.is_none());
    }

    #[test]
    fn runtime_categories_keep_selection_and_termination_on_visible_entries() {
        use crate::runtime::RuntimeProcessKind;
        let mut app = app_fixture();
        app.section = NavSection::Runtime;
        let own = RuntimeEntry {
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
        };
        let mut system = own.clone();
        system.pid = Some(456);
        system.kind = RuntimeProcessKind::System;
        system.owned_by_current_user = false;
        app.runtime_entries = vec![system.clone(), own.clone()];
        assert_eq!(app.selected_runtime_entry(), Some(&own));
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.runtime_kill_confirmation, Some(own));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.runtime_filter, RuntimeFilter::Applications);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.selected_runtime_entry(), Some(&system));
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.visible_runtime_entries().len(), 2);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.runtime_filter, RuntimeFilter::Mine);
        assert_eq!(app.selected_runtime, 0);
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
                proxy_jump: None,
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
            for number in 1..=6 {
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
            assert_eq!(app.section, NavSection::Projects);
            press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
            assert_eq!(app.section, NavSection::Hosts);
            press(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
            assert_eq!(app.section, NavSection::Projects);
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
    fn local_files_can_be_renamed_and_deleted_with_confirmation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lazyterm-mutations-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("before.txt"), "content").unwrap();
        std::fs::create_dir(root.join("empty")).unwrap();

        let mut app = app_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Local;
        app.local_files.path = root.clone();
        app.refresh_local_files();

        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(matches!(
            app.file_dialog,
            Some(FileDialog::CreateDir { .. })
        ));
        for character in "created".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(root.join("created").is_dir());

        app.local_files.selected = app
            .visible_local_entries()
            .iter()
            .position(|entry| entry.name == "before.txt")
            .unwrap();

        press(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(matches!(app.file_dialog, Some(FileDialog::Rename { .. })));
        for _ in "before.txt".chars() {
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        }
        for character in "renamed.txt".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(root.join("renamed.txt").is_file());
        assert!(!root.join("before.txt").exists());

        app.refresh_local_files();
        app.local_files.selected = app
            .visible_local_entries()
            .iter()
            .position(|entry| entry.name == "empty")
            .unwrap();
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(matches!(
            app.file_dialog,
            Some(FileDialog::Delete { is_dir: true, .. })
        ));
        press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(root.join("empty").is_dir());
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(!root.join("empty").exists());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_mutation_completion_reloads_the_current_directory() {
        let mut app = grouped_fixture();
        app.host_context = Some("web".into());
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: "projects".into(),
            entries: vec![],
        });
        app.mutation_hosts.insert(1, "web".into());
        app.mutation_sender
            .send(MutationEvent::Finished { operation_id: 1 })
            .unwrap();

        app.poll_mutations();

        assert!(matches!(
            app.files,
            FilesState::Loading {
                ref host_alias,
                ref path
            } if host_alias == "web" && path == "projects"
        ));
        assert!(app.sftp_receiver.is_some());
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
                    ..Default::default()
                },
                crate::sftp::SftpEntry {
                    name: "README.md".into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    size: Some(1),
                    ..Default::default()
                },
            ],
        });
        assert_eq!(app.visible_remote_entries().len(), 1);
        app.show_hidden_files = true;
        assert_eq!(app.visible_remote_entries().len(), 2);
    }

    #[test]
    fn remote_file_filter_matches_names() {
        let mut app = grouped_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Remote;
        app.host_context = Some("web".into());
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: String::new(),
            entries: vec![
                crate::sftp::SftpEntry {
                    name: "logs".into(),
                    kind: crate::sftp::SftpEntryKind::Directory,
                    size: None,
                    ..Default::default()
                },
                crate::sftp::SftpEntry {
                    name: "README.md".into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    size: Some(12),
                    ..Default::default()
                },
            ],
        });

        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        for character in "read".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(
            app.visible_remote_entries()
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["README.md"]
        );
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
                    ..Default::default()
                },
                crate::sftp::SftpEntry {
                    name: "README.md".into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    size: Some(12),
                    ..Default::default()
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
                ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
                },
                crate::sftp::SftpEntry {
                    name: "remote-folder".into(),
                    kind: crate::sftp::SftpEntryKind::Directory,
                    size: None,
                    ..Default::default()
                },
            ],
        });

        app.local_files.selected = 1;
        let upload = app.prepare_upload().expect("file should be uploadable");
        assert_eq!(upload.direction, TransferDirection::Upload);
        assert_eq!(upload.local_path, PathBuf::from("/tmp/notes.txt"));
        assert_eq!(upload.remote_path, "incoming/notes.txt");
        assert!(!upload.is_dir);

        app.local_files.selected = 0;
        assert!(app.prepare_upload().unwrap().is_dir);

        app.selected_file = 0;
        let download = app
            .prepare_download()
            .expect("remote file should be downloadable");
        assert_eq!(download.direction, TransferDirection::Download);
        assert_eq!(download.local_path, PathBuf::from("/tmp/remote.txt"));
        assert_eq!(download.remote_path, "incoming/remote.txt");
        assert!(!download.is_dir);

        app.selected_file = 1;
        assert!(app.prepare_download().unwrap().is_dir);
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
                ..Default::default()
            }],
        });
        app.transfer_tasks.push(TransferTask {
            id: 7,
            host_alias: "web".into(),
            direction: TransferDirection::Upload,
            name: "same.txt".into(),
            local_path: PathBuf::from("/tmp/same.txt"),
            remote_path: "same.txt".into(),
            is_dir: false,
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
            host_alias: "web".into(),
            direction: TransferDirection::Download,
            name: "missing.txt".into(),
            local_path: PathBuf::from("/tmp/missing.txt"),
            remote_path: "missing.txt".into(),
            is_dir: false,
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
            host_alias: "web".into(),
            direction: TransferDirection::Upload,
            name: "notes.txt".into(),
            local_path: PathBuf::from("/tmp/notes.txt"),
            remote_path: "notes.txt".into(),
            is_dir: false,
            transferred: 5,
            total: 10,
            status: TransferStatus::Running,
        });

        app.handle_mouse(5, 19, 100, 24);

        assert_eq!(app.active_file_pane, FilePane::Remote);
        assert_eq!(app.local_files.selected, 1);
    }

    #[test]
    fn transfer_selection_follows_recent_first_visual_order() {
        let mut app = grouped_fixture();
        for id in 1..=3 {
            app.transfer_tasks.push(TransferTask {
                id,
                host_alias: "web".into(),
                direction: TransferDirection::Upload,
                name: format!("file-{id}"),
                local_path: PathBuf::from(format!("/tmp/file-{id}")),
                remote_path: format!("file-{id}"),
                is_dir: false,
                transferred: 0,
                total: 0,
                status: TransferStatus::Completed,
            });
        }

        assert_eq!(app.selected_transfer_task_id(), Some(1));
        app.move_transfer_selection(KeyCode::Up);
        assert_eq!(app.selected_transfer_task_id(), Some(2));
        app.move_transfer_selection(KeyCode::Up);
        assert_eq!(app.selected_transfer_task_id(), Some(3));
        app.move_transfer_selection(KeyCode::Down);
        assert_eq!(app.selected_transfer_task_id(), Some(2));
    }

    #[test]
    fn terminal_transfer_history_can_be_cleared_without_touching_active_work() {
        let mut app = grouped_fixture();
        app.transfer_tasks = vec![
            TransferTask {
                id: 1,
                host_alias: "web".into(),
                direction: TransferDirection::Upload,
                name: "done".into(),
                local_path: PathBuf::from("/tmp/done"),
                remote_path: "done".into(),
                is_dir: false,
                transferred: 1,
                total: 1,
                status: TransferStatus::Completed,
            },
            TransferTask {
                id: 2,
                host_alias: "web".into(),
                direction: TransferDirection::Download,
                name: "running".into(),
                local_path: PathBuf::from("/tmp/running"),
                remote_path: "running".into(),
                is_dir: false,
                transferred: 0,
                total: 10,
                status: TransferStatus::Running,
            },
            TransferTask {
                id: 3,
                host_alias: "web".into(),
                direction: TransferDirection::Upload,
                name: "failed".into(),
                local_path: PathBuf::from("/tmp/failed"),
                remote_path: "failed".into(),
                is_dir: false,
                transferred: 0,
                total: 0,
                status: TransferStatus::Failed("no access".into()),
            },
        ];

        app.clear_finished_transfers();

        assert_eq!(app.transfer_tasks.len(), 1);
        assert_eq!(app.transfer_tasks[0].id, 2);
    }

    #[test]
    fn local_tunnel_form_creates_and_removes_a_definition() {
        let mut app = grouped_fixture();
        app.section = NavSection::Tunnels;
        app.metadata_error = true;
        app.open_tunnel_form(None);
        let dialog = app.tunnel_dialog.as_mut().expect("tunnel form opens");
        dialog.fields = [
            "web-db".into(),
            "web".into(),
            "127.0.0.1".into(),
            "15432".into(),
            "db.internal".into(),
            "5432".into(),
            "10".into(),
            "30".into(),
        ];
        dialog.selected_field = 7;
        app.handle_key(KeyCode::Enter);

        assert_eq!(app.metadata.tunnels.len(), 1);
        assert_eq!(app.metadata.tunnels[0].label, "web-db");
        assert_eq!(app.metadata.tunnels[0].host_alias, "web");

        app.open_tunnel_delete();
        app.handle_key(KeyCode::Char('y'));
        assert!(app.metadata.tunnels.is_empty());
    }

    #[test]
    fn local_tunnel_form_accepts_a_custom_ip_target() {
        let mut app = grouped_fixture();
        app.section = NavSection::Tunnels;
        app.metadata_error = true;
        app.open_tunnel_form(None);
        let dialog = app.tunnel_dialog.as_mut().expect("tunnel form opens");
        dialog.fields = [
            "custom-db".into(),
            "192.0.2.10".into(),
            "127.0.0.1".into(),
            "15433".into(),
            "db.internal".into(),
            "5432".into(),
            "10".into(),
            "30".into(),
        ];
        dialog.selected_field = 7;
        app.handle_key(KeyCode::Enter);

        assert_eq!(app.metadata.tunnels.len(), 1);
        assert_eq!(app.metadata.tunnels[0].host_alias, "192.0.2.10");
        assert!(app.metadata.tunnels[0].custom_host);
        assert!(!app.metadata.reconcile(&app.hosts));
        assert_eq!(app.metadata.tunnels.len(), 1);
    }

    #[test]
    fn tunnel_forms_save_remote_and_dynamic_modes() {
        let mut app = grouped_fixture();
        app.section = NavSection::Tunnels;
        app.metadata_error = true;

        app.open_tunnel_form_with_kind(None, TunnelKind::Remote);
        let dialog = app.tunnel_dialog.as_mut().expect("remote form opens");
        dialog.fields = [
            "remote-web".into(),
            "web".into(),
            "0.0.0.0".into(),
            "18080".into(),
            "127.0.0.1".into(),
            "8080".into(),
            "10".into(),
            "30".into(),
        ];
        dialog.selected_field = 7;
        app.handle_key(KeyCode::Enter);

        assert_eq!(app.metadata.tunnels[0].kind, TunnelKind::Remote);
        assert_eq!(app.metadata.tunnels[0].remote_port, Some(8080));

        app.open_tunnel_form_with_kind(None, TunnelKind::Dynamic);
        let dialog = app.tunnel_dialog.as_mut().expect("dynamic form opens");
        dialog.fields = [
            "socks".into(),
            "web".into(),
            "127.0.0.1".into(),
            "1080".into(),
            String::new(),
            String::new(),
            "10".into(),
            "30".into(),
        ];
        dialog.selected_field = 7;
        app.handle_key(KeyCode::Enter);

        assert_eq!(app.metadata.tunnels[1].kind, TunnelKind::Dynamic);
        assert_eq!(app.metadata.tunnels[1].remote_host, None);
        assert_eq!(app.metadata.tunnels[1].remote_port, None);
    }

    #[test]
    fn tunnel_create_form_switches_mode_with_function_keys() {
        let mut app = grouped_fixture();
        app.section = NavSection::Tunnels;
        app.open_tunnel_form(None);

        app.handle_key(KeyCode::F(2));
        assert_eq!(app.tunnel_dialog.as_ref().unwrap().kind, TunnelKind::Remote);
        app.handle_key(KeyCode::F(3));
        assert_eq!(
            app.tunnel_dialog.as_ref().unwrap().kind,
            TunnelKind::Dynamic
        );
        assert!(app.tunnel_dialog.as_ref().unwrap().fields[4].is_empty());
        assert!(app.tunnel_dialog.as_ref().unwrap().fields[5].is_empty());
        app.handle_key(KeyCode::F(1));
        assert_eq!(app.tunnel_dialog.as_ref().unwrap().kind, TunnelKind::Local);
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

    #[test]
    fn snippets_queue_targets_and_confirm_dangerous_commands() {
        let mut app = app_fixture();
        app.metadata.snippet_packages.push(SnippetPackage {
            id: 1,
            name: "Ops".into(),
            description: String::new(),
            snippets: vec![
                SnippetDefinition {
                    id: 1,
                    name: "status".into(),
                    command: "uptime".into(),
                    description: String::new(),
                    dangerous: false,
                },
                SnippetDefinition {
                    id: 2,
                    name: "restart".into(),
                    command: "sudo reboot".into(),
                    description: String::new(),
                    dangerous: true,
                },
            ],
        });
        app.selected_package = 0;
        app.selected_snippet = 0;
        app.start_snippet_runs(
            vec!["one".into(), "two".into()],
            "uptime".into(),
            "status".into(),
        );
        assert_eq!(app.snippet_runs.len(), 2);
        assert!(
            app.snippet_runs
                .iter()
                .all(|run| matches!(run.status, SnippetRunStatus::Queued))
        );
        assert_eq!(app.snippet_runs[0].command, "uptime");

        app.selected_snippet = 1;
        assert_eq!(
            app.snippet_run_action(vec!["one".into()]),
            InputAction::None
        );
        assert!(app.snippet_confirmation.is_some());
        assert_eq!(
            press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE),
            InputAction::RunSnippets {
                host_aliases: vec!["one".into()],
                command: "sudo reboot".into(),
                snippet_name: "restart".into(),
            }
        );
    }

    #[test]
    fn snippets_can_delete_a_snippet_and_a_package_after_confirmation() {
        let mut app = app_fixture();
        app.metadata.snippet_packages.push(SnippetPackage {
            id: 1,
            name: "Ops".into(),
            description: String::new(),
            snippets: vec![SnippetDefinition {
                id: 1,
                name: "status".into(),
                command: "uptime".into(),
                description: String::new(),
                dangerous: false,
            }],
        });
        app.snippet_package_open = true;
        app.handle_snippets_key(KeyCode::Char('d'));
        assert!(matches!(
            app.snippet_delete_confirmation,
            Some(SnippetDeleteTarget::Snippet { .. })
        ));
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(app.metadata.snippet_packages[0].snippets.is_empty());

        app.snippet_package_open = false;
        app.handle_snippets_key(KeyCode::Char('d'));
        assert!(matches!(
            app.snippet_delete_confirmation,
            Some(SnippetDeleteTarget::Package { .. })
        ));
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(app.metadata.snippet_packages.is_empty());
    }

    #[test]
    fn snippets_q_quits_from_the_section() {
        let mut app = app_fixture();
        app.section = NavSection::Snippets;
        press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);
    }
}
