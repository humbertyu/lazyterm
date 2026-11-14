use std::io::{Read, Write};
use std::process::{Child, ExitStatus};
use std::{
    collections::VecDeque,
    fs,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use age::{Decryptor, Encryptor, secrecy::SecretString};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

use crate::hosts::{Host, HostCatalog};
use crate::metadata::{
    ExportBundle, HostView, Language, Metadata, SnippetDefinition, SnippetPackage, Theme,
    TunnelDefinition, TunnelKind, TunnelLifecycle, parse_tags,
};
use crate::runtime::{self, RuntimeEntry, RuntimeEvent, RuntimeFilter, RuntimeTarget};
use crate::sftp::{
    self, Mutation, MutationEvent, MutationRequest, SftpListing, TransferDirection, TransferEvent,
    TransferRequest,
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
        cursor: usize,
    },
    Delete {
        pane: FilePane,
        name: String,
        is_dir: bool,
    },
    CreateDir {
        pane: FilePane,
        value: String,
        cursor: usize,
    },
    TransferConflict {
        request: TransferRequest,
    },
}

fn edit_file_name(value: &mut String, cursor: &mut usize, key: KeyCode) {
    let previous = || {
        value[..*cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    };
    let next = || {
        value[*cursor..]
            .chars()
            .next()
            .map_or(*cursor, |character| *cursor + character.len_utf8())
    };
    match key {
        KeyCode::Left => *cursor = previous(),
        KeyCode::Right => *cursor = next(),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = value.len(),
        KeyCode::Backspace if *cursor > 0 => {
            let start = previous();
            value.drain(start..*cursor);
            *cursor = start;
        }
        KeyCode::Delete if *cursor < value.len() => {
            let end = next();
            value.drain(*cursor..end);
        }
        KeyCode::Char(character) if !character.is_control() => {
            value.insert(*cursor, character);
            *cursor += character.len_utf8();
        }
        _ => {}
    }
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
        (((u32::from(width) * 2 + 2) / 5) as u16).saturating_add(4)
    }
}

pub fn group_columns(width: u16) -> usize {
    usize::from(width.saturating_add(1) / 19).max(1)
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
    Credentials,
    Settings,
}

impl NavSection {
    pub const ALL: [Self; 7] = [
        Self::Hosts,
        Self::Files,
        Self::Tunnels,
        Self::Snippets,
        Self::Runtime,
        Self::Projects,
        Self::Credentials,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Hosts => "Hosts",
            Self::Files => "Files",
            Self::Tunnels => "Tunnels",
            Self::Snippets => "Snippets",
            Self::Runtime => "Runtime",
            Self::Projects => "Projects",
            Self::Credentials => "Credentials",
            Self::Settings => "Settings",
        }
    }
}

pub struct App {
    pub native_connection: Option<crate::native_connection::Pending>,
    pub authentication: Option<crate::authentication::Service>,
    pub authentication_challenge: Option<crate::authentication::Challenge>,
    authentication_queue: std::collections::VecDeque<crate::authentication::Challenge>,
    authentication_saves: Vec<AuthenticationSave>,
    pub credential_manager: crate::credential_app::CredentialState,
    pub workspace: crate::project_app::WorkspaceState,
    pub project_service: crate::projects::ProjectService,
    pub section: NavSection,
    pub previous_section: NavSection,
    pub hosts: HostCatalog,
    pub selected_host: usize,
    pub selected_setting: usize,
    pub settings_picker: Option<SettingsPicker>,
    pub sync_settings: crate::sync::SyncSettings,
    pub sync_dialog: Option<SyncSettingsDialog>,
    pub sync_test_pending: Option<crate::sync::ConnectionTest>,
    pub sync_worker: Option<Receiver<Result<crate::sync_engine::SyncResult, String>>>,
    pub sync_conflicts: Option<crate::sync_engine::Prepared>,
    pub selected_browse_item: usize,
    pub viewport_width: u16,
    pub viewport_height: u16,
    pub search_query: String,
    pub searching: bool,
    pub should_quit: bool,
    pub notice: Option<String>,
    pub toast: Option<ToastNotice>,
    observed_toast_message: Option<String>,
    observed_toast_from_credentials: bool,
    pub connection_dialog: Option<ConnectionDialog>,
    pub host_password_dialog: Option<HostPasswordDialog>,
    pending_host_password: Option<SubmittedHostPassword>,
    pub host_dialog: Option<HostDialog>,
    pub host_group_dialog: Option<HostGroupDialog>,
    pub host_action_dialog: Option<HostActionDialog>,
    pub tunnel_dialog: Option<TunnelDialog>,
    pub snippet_dialog: Option<SnippetDialog>,
    pub package_dialog: Option<PackageDialog>,
    pub credential_export_dialog: Option<CredentialExportDialog>,
    pub credential_import_dialog: Option<CredentialImportDialog>,
    pub import_confirmation: Option<ImportKind>,
    pub pending_configuration_import: Option<PendingConfigurationImport>,
    pending_credential_import_path: Option<PathBuf>,
    pending_credential_import: Option<PendingCredentialImport>,
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
    #[cfg(test)]
    pub(crate) test_auth_config: Option<PathBuf>,
    #[cfg(test)]
    pub(crate) test_auth_askpass: Option<PathBuf>,
    pub runtime_host_picker_open: bool,
    pub runtime_host_picker_group: Option<HostGroup>,
    pub runtime_host_picker_selection: usize,
    pub runtime_kill_confirmation: Option<RuntimeEntry>,
    pub runtime_force_kill_confirmation: Option<RuntimeEntry>,
    pub selected_tunnel: usize,
    pub tunnel_statuses: std::collections::HashMap<uuid::Uuid, TunnelStatus>,
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
    pub local_file_filter: String,
    pub remote_file_filter: String,
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
    sftp_receiver: Option<sftp::PendingListing>,
    transfer_sender: Sender<TransferEvent>,
    transfer_receiver: Receiver<TransferEvent>,
    mutation_sender: Sender<MutationEvent>,
    mutation_receiver: Receiver<MutationEvent>,
    mutation_hosts: std::collections::HashMap<u64, String>,
    tunnel_sender: Sender<tunnel::TunnelEvent>,
    tunnel_receiver: Receiver<tunnel::TunnelEvent>,
    runtime_sender: Sender<RuntimeEvent>,
    runtime_receiver: Receiver<RuntimeEvent>,
    tunnel_processes: std::collections::HashMap<uuid::Uuid, crate::process_guard::ManagedChild>,
    tunnel_authentication: std::collections::HashMap<uuid::Uuid, crate::authentication::Status>,
    snippet_processes: std::collections::HashMap<u64, SnippetProcess>,
    next_snippet_run_id: u64,
    tunnel_errors: std::collections::HashMap<uuid::Uuid, String>,
    tunnel_retries: std::collections::HashMap<uuid::Uuid, TunnelRetry>,
}

#[derive(Clone, Debug)]
pub struct SyncSettingsDialog {
    pub remote: String,
    pub branch: String,
    pub selected: usize,
    pub testing: bool,
}

type AuthenticationSave = (
    String,
    String,
    Receiver<Result<crate::project_credentials::PreparedPassword, String>>,
);

#[derive(Clone, Copy, Debug)]
pub struct SettingsPicker {
    pub setting: usize,
    pub selected: usize,
    original: usize,
}

fn configuration_export_bytes(
    source: &Metadata,
    hosts: &[Host],
    ssh_config_path: Option<String>,
    ssh_config: String,
    ssh_config_files: std::collections::BTreeMap<String, String>,
) -> io::Result<Vec<u8>> {
    let mut metadata = source.clone();
    for project in &mut metadata.projects {
        for link in &mut project.links {
            link.password = None;
        }
    }
    let bundle = ExportBundle {
        format_version: 1,
        ssh_config_path,
        ssh_config,
        ssh_config_files,
        hosts,
        metadata: &metadata,
        passwords_included: false,
    };
    serde_json::to_vec_pretty(&bundle).map_err(io::Error::other)
}

fn write_private_export(path: &Path, contents: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("Export path has no file name."))?
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = path.with_file_name(format!(".{name}.{}-{nonce}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

type KeychainReplacement = (String, Zeroizing<String>, Option<Zeroizing<String>>);

fn restore_imported_keychain(
    replacements: &[KeychainReplacement],
    changed: &[String],
) -> Result<(), String> {
    let mut first_error = None;
    for reference in changed.iter().rev() {
        let previous = replacements
            .iter()
            .find(|(candidate, _, _)| candidate == reference)
            .and_then(|(_, _, previous)| previous.as_deref());
        let result = match previous {
            Some(password) => crate::project_credentials::replace_password(reference, password),
            None => crate::project_credentials::delete_password(reference),
        };
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub struct ToastNotice {
    pub message: String,
    pub expires_at: Instant,
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
    pub fields: [String; 6],
    pub tags: Vec<String>,
    pub tag_picker_open: bool,
    pub tag_filter: String,
    pub tag_picker_selected: usize,
    pub proxy_jump_picker_open: bool,
    pub proxy_jump_picker_selected: usize,
    pub original_alias: Option<String>,
    pub selected_field: usize,
    pub editing: bool,
    pub deleting: bool,
}

pub struct HostGroupDialog {
    pub name: String,
    pub original_name: Option<String>,
    pub deleting: bool,
}

#[derive(Clone, Debug)]
pub struct HostActionDialog {
    pub title: String,
    pub message: String,
}

pub struct TunnelDialog {
    pub fields: [String; 8],
    pub kind: TunnelKind,
    pub editing_id: Option<uuid::Uuid>,
    pub selected_field: usize,
    pub deleting: bool,
}

pub struct SnippetDialog {
    pub fields: [String; 4],
    pub package_id: uuid::Uuid,
    pub editing_id: Option<uuid::Uuid>,
    pub selected_field: usize,
}

pub struct PackageDialog {
    pub fields: [String; 2],
    pub editing_id: Option<uuid::Uuid>,
    pub selected_field: usize,
}

pub struct CredentialExportDialog {
    pub fields: [String; 2],
    pub selected_field: usize,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportKind {
    Configuration,
    Credentials,
}

pub struct CredentialImportDialog {
    pub password: String,
    pub error: Option<String>,
}

pub struct PendingConfigurationImport {
    pub import: crate::import_export::ConfigurationImport,
    pub source_path: PathBuf,
}

pub struct PendingCredentialImport {
    archive: CredentialImport,
    export_password: Zeroizing<String>,
}

fn read_limited(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Selected import file is too large.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Selected import file is too large.",
        ));
    }
    Ok(bytes)
}

#[derive(Serialize)]
struct CredentialExport {
    format_version: u32,
    credentials: Vec<CredentialRecord>,
}

#[derive(Serialize)]
struct CredentialRecord {
    reference: String,
    password: String,
}

#[derive(serde::Deserialize)]
struct CredentialImport {
    format_version: u32,
    credentials: Vec<ImportedCredentialRecord>,
}

#[derive(serde::Deserialize)]
struct ImportedCredentialRecord {
    reference: String,
    password: String,
}

impl Drop for ImportedCredentialRecord {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

impl Drop for CredentialRecord {
    fn drop(&mut self) {
        self.password.zeroize();
    }
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
        id: uuid::Uuid,
        name: String,
    },
    Snippet {
        package_id: uuid::Uuid,
        id: uuid::Uuid,
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
    reader: R,
    sender: Sender<SnippetProcessEvent>,
    stderr: bool,
) {
    let mut reader: Box<dyn Read + Send> = if stderr {
        Box::new(crate::diagnostics::RedactingReader::new(reader))
    } else {
        Box::new(reader)
    };
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
    pub cancelable: bool,
    pub scroll: u16,
}

#[derive(Clone)]
pub enum HostPasswordPurpose {
    Authentication,
    UploadKey {
        credential: crate::credentials::Credential,
        identity_configured: bool,
    },
}

pub struct HostPasswordDialog {
    pub alias: String,
    pub username: String,
    pub password: Zeroizing<String>,
    pub save_in_keychain: bool,
    pub save_allowed: bool,
    pub selected_field: usize,
    pub error: Option<String>,
    pub purpose: HostPasswordPurpose,
}

impl Drop for HostPasswordDialog {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

pub struct SubmittedHostPassword {
    pub alias: String,
    pub username: String,
    pub password: Zeroizing<String>,
    pub save_in_keychain: bool,
    pub purpose: HostPasswordPurpose,
}

impl Drop for SubmittedHostPassword {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum InputAction {
    None,
    Connect,
    RetryPublicKeyUpload,
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
    pub fn update_toast(&mut self) {
        let credential_message = if self.credential_manager.open {
            self.credential_manager.message.as_deref()
        } else {
            None
        };
        let (message, from_credentials) = credential_message
            .map(|message| (Some(message), true))
            .unwrap_or((self.notice.as_deref(), false));
        match message {
            Some(message)
                if self.observed_toast_message.as_deref() != Some(message)
                    || self.observed_toast_from_credentials != from_credentials =>
            {
                self.observed_toast_message = Some(message.to_string());
                self.observed_toast_from_credentials = from_credentials;
                self.toast = Some(ToastNotice {
                    message: message.to_string(),
                    expires_at: Instant::now() + Duration::from_secs(4),
                });
            }
            None => {
                self.observed_toast_message = None;
                self.observed_toast_from_credentials = false;
                self.toast = None;
            }
            _ => {}
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.expires_at <= Instant::now())
        {
            self.toast = None;
            if !self.observed_toast_from_credentials
                && self.notice.as_deref() == self.observed_toast_message.as_deref()
            {
                self.notice = None;
                self.observed_toast_message = None;
                self.observed_toast_from_credentials = false;
            }
        }
    }

    pub fn new() -> Self {
        let mut app = Self::with_catalog(HostCatalog::load(), Metadata::load());
        match crate::authentication::Service::new() {
            Ok(service) => app.authentication = Some(service),
            Err(error) => {
                app.notice = Some(format!("Interactive authentication unavailable: {error}"))
            }
        }
        app.reconcile_metadata();
        app.refresh_host_key_sources();
        app.refresh_local_files();
        app.project_service.restore(&app.metadata.projects);
        app
    }

    pub(crate) fn with_catalog(hosts: HostCatalog, metadata: std::io::Result<Metadata>) -> Self {
        let notice = metadata
            .as_ref()
            .err()
            .map(|error| format!("Could not read host metadata: {error}"));
        let metadata_error = metadata.is_err();
        let mut metadata = metadata.unwrap_or_default();
        metadata.normalize_host_groups();
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
            native_connection: None,
            authentication: None,
            authentication_challenge: None,
            authentication_queue: Default::default(),
            authentication_saves: Vec::new(),
            credential_manager: Default::default(),
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
            selected_setting: 0,
            settings_picker: None,
            sync_settings: crate::sync::SyncSettings::load(),
            sync_dialog: None,
            sync_test_pending: None,
            sync_worker: None,
            sync_conflicts: None,
            selected_browse_item: 0,
            viewport_width: 80,
            viewport_height: 24,
            search_query: String::new(),
            searching: false,
            should_quit: false,
            notice,
            toast: None,
            observed_toast_message: None,
            observed_toast_from_credentials: false,
            connection_dialog: None,
            host_password_dialog: None,
            pending_host_password: None,
            host_dialog: None,
            host_group_dialog: None,
            host_action_dialog: None,
            tunnel_dialog: None,
            snippet_dialog: None,
            package_dialog: None,
            credential_export_dialog: None,
            credential_import_dialog: None,
            import_confirmation: None,
            pending_configuration_import: None,
            pending_credential_import_path: None,
            pending_credential_import: None,
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
            #[cfg(test)]
            test_auth_config: None,
            #[cfg(test)]
            test_auth_askpass: None,
            runtime_host_picker_open: false,
            runtime_host_picker_group: None,
            runtime_host_picker_selection: 0,
            runtime_kill_confirmation: None,
            runtime_force_kill_confirmation: None,
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
            local_file_filter: String::new(),
            remote_file_filter: String::new(),
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
            tunnel_authentication: std::collections::HashMap::new(),
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
        let query = self.search_query.to_lowercase();
        groups.extend(
            self.metadata
                .host_groups
                .iter()
                .filter(|tag| query.is_empty() || tag.to_lowercase().contains(&query))
                .cloned()
                .map(HostGroup::Tag),
        );
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
        let auth = self.host_password_auth(&host_alias);
        self.sftp_receiver = Some(sftp::spawn_list_with_auth(host_alias, path, auth));
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
        let query = self.local_file_filter.to_lowercase();
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
        let query = self.remote_file_filter.to_lowercase();
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

    pub fn file_filter(&self, pane: FilePane) -> &str {
        match pane {
            FilePane::Local => &self.local_file_filter,
            FilePane::Remote => &self.remote_file_filter,
        }
    }

    fn active_file_filter_mut(&mut self) -> &mut String {
        match self.active_file_pane {
            FilePane::Local => &mut self.local_file_filter,
            FilePane::Remote => &mut self.remote_file_filter,
        }
    }

    pub fn file_filter_visible(&self, pane: FilePane) -> bool {
        (self.file_filtering && self.active_file_pane == pane) || !self.file_filter(pane).is_empty()
    }

    fn reset_active_file_selection(&mut self) {
        match self.active_file_pane {
            FilePane::Local => self.local_files.selected = 0,
            FilePane::Remote => self.selected_file = 0,
        }
    }

    fn local_list_start_row(&self) -> u16 {
        5 + u16::from(self.file_filter_visible(FilePane::Local))
    }

    fn remote_list_start_row(&self) -> u16 {
        4 + u16::from(self.file_filter_visible(FilePane::Remote))
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
        self.local_file_filter.clear();
        if self.active_file_pane == FilePane::Local {
            self.file_filtering = false;
        }
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
                    cursor: name.len(),
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
            cursor: 0,
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
                mut cursor,
            } => match key {
                KeyCode::Esc => self.file_dialog = None,
                KeyCode::Enter => self.submit_file_rename(pane, old_name, value),
                _ => {
                    edit_file_name(&mut value, &mut cursor, key);
                    self.file_dialog = Some(FileDialog::Rename {
                        pane,
                        old_name,
                        value,
                        cursor,
                    });
                }
            },
            FileDialog::Delete { pane, name, is_dir } => match key {
                KeyCode::Char('y') | KeyCode::Enter => self.submit_file_delete(pane, name, is_dir),
                KeyCode::Char('n') | KeyCode::Esc => self.file_dialog = None,
                _ => {}
            },
            FileDialog::CreateDir {
                pane,
                mut value,
                mut cursor,
            } => match key {
                KeyCode::Esc => self.file_dialog = None,
                KeyCode::Enter => self.submit_create_dir(pane, value),
                _ => {
                    edit_file_name(&mut value, &mut cursor, key);
                    self.file_dialog = Some(FileDialog::CreateDir {
                        pane,
                        value,
                        cursor,
                    });
                }
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
                    let auth = self.host_password_auth(&host_alias);
                    self.sftp_receiver = Some(sftp::spawn_list_with_auth(host_alias, path, auth));
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
            if self
                .tunnel_authentication
                .get(&id)
                .is_some_and(|auth| auth.cancelled())
            {
                if let Some(mut child) = self.tunnel_processes.remove(&id) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                self.tunnel_authentication.remove(&id);
                self.tunnel_retries.remove(&id);
                self.tunnel_statuses.insert(id, TunnelStatus::Stopped);
                continue;
            }
            let result = self
                .tunnel_processes
                .get_mut(&id)
                .map(|child| child.try_wait());
            let Some(result) = result else {
                continue;
            };
            match result {
                Ok(None) => {
                    let starting = self
                        .tunnel_authentication
                        .get(&id)
                        .is_some_and(|auth| !auth.authenticated());
                    self.tunnel_statuses.insert(
                        id,
                        if starting {
                            TunnelStatus::Starting
                        } else {
                            TunnelStatus::Running
                        },
                    );
                }
                Ok(Some(status)) => {
                    self.tunnel_processes.remove(&id);
                    let authenticated = self
                        .tunnel_authentication
                        .remove(&id)
                        .is_none_or(|auth| auth.authenticated());
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
                            && authenticated
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
                    self.tunnel_authentication.remove(&id);
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
            self.metadata.tunnels.clone(),
        );
    }

    pub(crate) fn switch_runtime_target(&mut self, target: RuntimeTarget) {
        if self.runtime_kill_confirmation.is_some()
            || self.runtime_force_kill_confirmation.is_some()
        {
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
        if self.runtime_force_kill_confirmation.is_some() {
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let Some(entry) = self.runtime_force_kill_confirmation.take() else {
                        return;
                    };
                    if let Some(pid) = entry.pid {
                        runtime::kill_async(
                            self.runtime_sender.clone(),
                            self.runtime_target.clone(),
                            pid,
                            entry.process,
                        );
                    } else {
                        self.notice = Some("The selected entry has no visible PID.".into());
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => self.runtime_force_kill_confirmation = None,
                _ => {}
            }
            return;
        }
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
            KeyCode::Char('K') => {
                if let Some(entry) = self.selected_runtime_entry()
                    && entry.pid.is_some()
                {
                    self.runtime_force_kill_confirmation = Some(entry.clone());
                } else {
                    self.notice = Some("The selected entry has no visible PID.".into());
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
        let auth = self.host_password_auth(&tunnel.host_alias);
        match tunnel::spawn_forward(&tunnel, self.tunnel_sender.clone(), &auth) {
            Ok((child, authentication)) => {
                if let Some(authentication) = authentication {
                    self.tunnel_authentication.insert(tunnel.id, authentication);
                }
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

    fn stop_tunnel(&mut self, id: uuid::Uuid) {
        if let Some(mut child) = self.tunnel_processes.remove(&id) {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.tunnel_errors.remove(&id);
        self.tunnel_authentication.remove(&id);
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
        groups.extend(
            self.metadata
                .host_groups
                .iter()
                .cloned()
                .map(HostGroup::Tag),
        );
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

        if key == KeyCode::Esc && !self.file_filter(self.active_file_pane).is_empty() {
            self.active_file_filter_mut().clear();
            self.reset_active_file_selection();
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
                self.active_file_filter_mut().clear();
                self.reset_active_file_selection();
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
        if self.settings_picker.is_some()
            || self.sync_dialog.is_some()
            || self.sync_conflicts.is_some()
            || self.credentials_modal()
        {
            return InputAction::None;
        }
        if self.host_dialog.is_some()
            || self.host_group_dialog.is_some()
            || self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
        {
            return InputAction::None;
        }
        if self.section == NavSection::Runtime {
            if self.runtime_host_picker_open
                || self.runtime_kill_confirmation.is_some()
                || self.runtime_force_kill_confirmation.is_some()
            {
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
        let local_width = viewport_width / 2;
        let clicked_pane = if column < local_width {
            FilePane::Local
        } else {
            FilePane::Remote
        };
        if self.file_filtering && clicked_pane == self.active_file_pane {
            return InputAction::None;
        }
        self.file_filtering = false;
        self.active_file_pane = clicked_pane;
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
            let Some(host_alias) = self
                .snippet_runs
                .iter()
                .find(|run| run.id == id)
                .map(|run| run.host_alias.clone())
            else {
                continue;
            };
            let auth = self.host_password_auth(&host_alias);
            let Some(run) = self.snippet_runs.iter_mut().find(|run| run.id == id) else {
                continue;
            };
            let mut command = std::process::Command::new("ssh");
            command.args(["-o", "RequestTTY=no", "-o", "ConnectTimeout=10"]);
            let trace =
                match crate::credentials::configure_host_auth(&mut command, &auth, &host_alias) {
                    Ok(trace) => trace,
                    Err(error) => {
                        run.status = SnippetRunStatus::ConnectionFailed(format!(
                            "Could not prepare Host authentication: {error}"
                        ));
                        continue;
                    }
                };
            match command
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
                    spawn_snippet_reader(
                        crate::authentication::Reader::new(stderr, trace),
                        sender,
                        true,
                    );
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

    pub fn prompt_host_password(
        &mut self,
        alias: String,
        username: String,
        error: Option<String>,
        save_in_keychain: bool,
        purpose: HostPasswordPurpose,
    ) {
        self.connection_dialog = None;
        self.host_password_dialog = Some(HostPasswordDialog {
            alias,
            username,
            password: Zeroizing::new(String::new()),
            save_in_keychain,
            save_allowed: true,
            selected_field: 0,
            error,
            purpose,
        });
    }

    pub fn take_pending_host_password(&mut self) -> Option<SubmittedHostPassword> {
        self.pending_host_password.take()
    }

    pub fn authentication_saving(&self) -> bool {
        !self.authentication_saves.is_empty()
    }

    pub fn poll_authentication(&mut self) {
        let events = self
            .authentication
            .as_ref()
            .map(|service| service.events.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            match event {
                crate::authentication::Event::Challenge(challenge) => {
                    self.authentication_queue.push_back(challenge)
                }
                crate::authentication::Event::PublicKeyAccepted { alias } => {
                    if self.hosts.hosts.iter().any(|host| host.alias == alias)
                        && self.metadata.host_public_key_auth.insert(alias)
                    {
                        self.save_metadata();
                    }
                }
                crate::authentication::Event::Accepted {
                    alias,
                    username,
                    password,
                } => {
                    self.authentication_saves.push((
                        alias,
                        username,
                        crate::project_credentials::prepare_credential(password),
                    ));
                }
            }
        }
        let mut index = 0;
        while index < self.authentication_saves.len() {
            let result = match self.authentication_saves[index].2.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("Keychain worker stopped.".into())
                }
            };
            let (alias, username, _) = self.authentication_saves.remove(index);
            match result {
                Ok(mut prepared) if self.hosts.hosts.iter().any(|host| host.alias == alias) => {
                    if !self.save_host_password_prepared(&alias, &username, &mut prepared, None) {
                        self.notice = Some(format!(
                            "Connected to {alias}, but its password could not be saved."
                        ));
                    }
                }
                Ok(_) => {
                    self.notice = Some(format!("{alias} was removed; its password was not saved."))
                }
                Err(error) => {
                    self.notice = Some(format!(
                        "Connected to {alias}, but password saving failed: {error}"
                    ))
                }
            }
        }
        if self
            .authentication_challenge
            .as_ref()
            .is_some_and(|challenge| !challenge.active())
        {
            self.authentication_challenge = None;
            if self
                .host_password_dialog
                .as_ref()
                .is_some_and(|dialog| matches!(dialog.purpose, HostPasswordPurpose::Authentication))
            {
                self.host_password_dialog = None;
            }
        }
        // Serialize prompts without replacing another modal or another connection's input.
        if self.host_password_dialog.is_none()
            && (!self.credentials_modal() || self.credentials_uploading())
            && self.host_dialog.is_none()
            && self.tunnel_dialog.is_none()
            && self.snippet_dialog.is_none()
            && self.package_dialog.is_none()
            && self.workspace.form.is_none()
            && self.workspace.delete.is_none()
            && self.workspace.confirmation.is_none()
            && self.host_group_dialog.is_none()
            && self.host_action_dialog.is_none()
            && self.tags_dialog.is_none()
            && self.file_dialog.is_none()
            && self.snippet_confirmation.is_none()
            && self.snippet_delete_confirmation.is_none()
            && self.runtime_kill_confirmation.is_none()
            && self.runtime_force_kill_confirmation.is_none()
            && !self.host_picker_open
            && !self.snippet_host_picker_open
            && !self.runtime_host_picker_open
            && self
                .connection_dialog
                .as_ref()
                .is_none_or(|dialog| dialog.pending && self.native_connection.is_some())
            && self.import_confirmation.is_none()
            && self.credential_export_dialog.is_none()
            && self.credential_import_dialog.is_none()
            && self.settings_picker.is_none()
        {
            while let Some(challenge) = self.authentication_queue.pop_front() {
                if !challenge.active() {
                    continue;
                }
                let message = if challenge.rejected {
                    "Password was rejected. Enter the current password."
                } else {
                    "SSH requires a password."
                };
                self.prompt_host_password(
                    challenge.alias.clone(),
                    challenge.username.clone(),
                    Some(message.into()),
                    challenge.can_save,
                    HostPasswordPurpose::Authentication,
                );
                self.host_password_dialog.as_mut().unwrap().selected_field = 1;
                self.host_password_dialog.as_mut().unwrap().save_allowed = challenge.can_save;
                self.authentication_challenge = Some(challenge);
                break;
            }
        }
    }

    fn handle_host_password_key(&mut self, key: KeyCode) -> InputAction {
        let Some(dialog) = self.host_password_dialog.as_mut() else {
            return InputAction::None;
        };
        let shared = matches!(dialog.purpose, HostPasswordPurpose::Authentication);
        match key {
            KeyCode::Esc => {
                if shared && let Some(challenge) = self.authentication_challenge.take() {
                    challenge.respond(None);
                }
                self.host_password_dialog = None;
                self.pending_host_password = None;
            }
            KeyCode::Tab | KeyCode::Down => {
                dialog.selected_field = if shared {
                    if dialog.selected_field == 1 && dialog.save_allowed {
                        2
                    } else {
                        1
                    }
                } else {
                    (dialog.selected_field + 1) % 3
                };
            }
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field = if shared {
                    if dialog.selected_field == 1 && dialog.save_allowed {
                        2
                    } else {
                        1
                    }
                } else {
                    (dialog.selected_field + 2) % 3
                };
            }
            KeyCode::Char(' ') if dialog.selected_field == 2 => {
                if !shared
                    || self
                        .authentication_challenge
                        .as_ref()
                        .is_some_and(|c| c.can_save)
                {
                    dialog.save_in_keychain = !dialog.save_in_keychain;
                }
            }
            KeyCode::Char(' ') if dialog.selected_field == 1 => {
                dialog.password.push(' ');
                dialog.error = None;
            }
            KeyCode::Backspace if dialog.selected_field == 0 => {
                dialog.username.pop();
                dialog.error = None;
            }
            KeyCode::Backspace if dialog.selected_field == 1 => {
                dialog.password.pop();
                dialog.error = None;
            }
            KeyCode::Char(value)
                if dialog.selected_field == 0 && !value.is_control() && !value.is_whitespace() =>
            {
                dialog.username.push(value);
                dialog.error = None;
            }
            KeyCode::Char(value) if dialog.selected_field == 1 && !value.is_control() => {
                dialog.password.push(value);
                dialog.error = None;
            }
            KeyCode::Enter => {
                if dialog.username.trim().is_empty() {
                    dialog.error = Some("Enter the SSH username.".into());
                    return InputAction::None;
                }
                if dialog.password.is_empty() {
                    dialog.error = Some("Enter the Host password.".into());
                    return InputAction::None;
                }
                let Some(dialog) = self.host_password_dialog.take() else {
                    return InputAction::None;
                };
                if shared {
                    if let Some(challenge) = self.authentication_challenge.take() {
                        challenge.respond(Some(crate::authentication::Answer {
                            password: Zeroizing::new(dialog.password.to_string()),
                            save: dialog.save_in_keychain,
                        }));
                    }
                    return InputAction::None;
                }
                let action = match &dialog.purpose {
                    HostPasswordPurpose::Authentication => unreachable!(),
                    HostPasswordPurpose::UploadKey { .. } => InputAction::RetryPublicKeyUpload,
                };
                self.pending_host_password = Some(SubmittedHostPassword {
                    alias: dialog.alias.clone(),
                    username: dialog.username.clone(),
                    password: Zeroizing::new(dialog.password.to_string()),
                    save_in_keychain: dialog.save_in_keychain,
                    purpose: dialog.purpose.clone(),
                });
                return action;
            }
            _ => {}
        }
        InputAction::None
    }

    pub fn handle_event(&mut self, key: KeyEvent) -> InputAction {
        if self.sync_conflicts.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(prepared) = self.sync_conflicts.as_mut() {
                        prepared.conflict_selection = prepared.conflict_selection.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    if let Some(prepared) = self.sync_conflicts.as_mut() {
                        prepared.conflict_selection = (prepared.conflict_selection + 1)
                            .min(prepared.plan.conflicts.len().saturating_sub(1));
                    }
                }
                KeyCode::Char('l') | KeyCode::Char('L') => self.resolve_sync_conflicts(false),
                KeyCode::Char('r') | KeyCode::Char('R') => self.resolve_sync_conflicts(true),
                KeyCode::Esc => {
                    self.sync_conflicts = None;
                    if let Some(dialog) = self.sync_dialog.as_mut() {
                        dialog.testing = false;
                    }
                }
                _ => {}
            }
            return InputAction::None;
        }
        if self.sync_dialog.is_some() {
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                self.handle_sync_settings_key(key.code);
            }
            return InputAction::None;
        }
        if self.settings_picker.is_some() {
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                self.handle_settings_picker_key(key.code);
            }
            return InputAction::None;
        }
        if self.host_password_dialog.is_some() {
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                return self.handle_host_password_key(key.code);
            }
            return InputAction::None;
        }
        if self.credentials_modal() {
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                self.handle_credentials_key(key.code);
            }
            return InputAction::None;
        }
        let modal = self.host_dialog.is_some()
            || self.host_group_dialog.is_some()
            || self.host_action_dialog.is_some()
            || self.tunnel_dialog.is_some()
            || self.snippet_dialog.is_some()
            || self.package_dialog.is_some()
            || self.credential_export_dialog.is_some()
            || self.credential_import_dialog.is_some()
            || self.import_confirmation.is_some()
            || self.snippet_confirmation.is_some()
            || self.snippet_delete_confirmation.is_some()
            || self.runtime_host_picker_open
            || self.runtime_kill_confirmation.is_some()
            || self.runtime_force_kill_confirmation.is_some()
            || self.workspace.form.is_some()
            || self.workspace.delete.is_some()
            || self.workspace.confirmation.is_some()
            || self.tags_dialog.is_some()
            || self.connection_dialog.is_some()
            || self.host_password_dialog.is_some()
            || self.file_dialog.is_some();
        if !modal {
            match (key.code, key.modifiers) {
                (KeyCode::Char(number @ '1'..='7'), KeyModifiers::NONE)
                    if !self.searching
                        && !self.file_filtering
                        && !self.host_picker_open
                        && !self.snippet_host_picker_open =>
                {
                    if let Some(section) =
                        self.visible_sections().get((number as u8 - b'1') as usize)
                    {
                        self.switch_section(*section);
                    }
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
        if !modal && key.code == KeyCode::Char(',') {
            self.switch_section(NavSection::Settings);
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
        } else if self.host_action_dialog.is_some() {
            self.handle_host_action_dialog_key(key.code);
        } else if self.credential_export_dialog.is_some() {
            self.handle_credential_export_key(key.code);
        } else if self.credential_import_dialog.is_some() {
            self.handle_credential_import_key(key.code);
        } else if self.import_confirmation.is_some() {
            self.handle_import_confirmation_key(key.code);
        } else if self.snippet_confirmation.is_some() {
            return self.handle_snippet_confirmation_key(key.code);
        } else if self.snippet_delete_confirmation.is_some() {
            self.handle_snippet_delete_key(key.code);
        } else if self.runtime_host_picker_open
            || self.runtime_kill_confirmation.is_some()
            || self.runtime_force_kill_confirmation.is_some()
        {
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
        } else if self.section == NavSection::Settings {
            self.handle_settings_key(key.code);
        } else if self.section == NavSection::Projects {
            self.handle_projects_key(key.code);
        } else if self.section == NavSection::Credentials {
            self.handle_credentials_key(key.code);
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
            if section == NavSection::Credentials {
                self.credential_manager.open = false;
                self.credential_manager.target = None;
                self.credential_manager.upload_only = false;
                self.refresh_discovered_keys();
            }
        }
    }

    pub fn visible_sections(&self) -> Vec<NavSection> {
        NavSection::ALL
            .into_iter()
            .filter(|section| {
                !self
                    .metadata
                    .hidden_tabs
                    .iter()
                    .any(|hidden| hidden == section.title())
            })
            .collect()
    }

    fn export_metadata(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export LazyTerm configuration")
            .set_file_name("lazyterm-export.json")
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            self.notice = Some("Configuration export cancelled.".into());
            return;
        };
        let ssh_config_path = HostCatalog::config_path();
        let ssh_config_files = match ssh_config_path.as_deref() {
            Some(config_path) => {
                let Some(ssh_root) = config_path.parent() else {
                    self.notice = Some("Could not locate the SSH config directory.".into());
                    return;
                };
                match crate::import_export::portable_ssh_snapshot(config_path, ssh_root) {
                    Ok(files) => files,
                    Err(error) => {
                        self.notice =
                            Some(format!("Could not read SSH config for export: {error}"));
                        return;
                    }
                }
            }
            None => std::collections::BTreeMap::new(),
        };
        let ssh_config = ssh_config_path
            .as_ref()
            .and_then(|config_path| fs::canonicalize(config_path).ok())
            .and_then(|config_path| ssh_config_files.get(&config_path.display().to_string()))
            .cloned()
            .unwrap_or_default();
        let result = configuration_export_bytes(
            &self.metadata,
            &self.hosts.hosts,
            ssh_config_path.map(|path| path.display().to_string()),
            ssh_config,
            ssh_config_files,
        )
        .and_then(|bytes| write_private_export(&path, &bytes));
        self.notice = Some(match result {
            Ok(()) => format!("Configuration exported to {}.", path.display()),
            Err(error) => format!("Could not export configuration: {error}"),
        });
    }

    fn toggle_setting(&mut self) {
        match self.selected_setting {
            0 => self.open_settings_picker(0),
            1 => self.open_settings_picker(1),
            2..=8 => {
                let section = NavSection::ALL[self.selected_setting - 2];
                let title = section.title().to_string();
                if self
                    .metadata
                    .hidden_tabs
                    .iter()
                    .any(|hidden| hidden == &title)
                {
                    self.metadata.hidden_tabs.retain(|hidden| hidden != &title);
                } else {
                    self.metadata.hidden_tabs.push(title);
                }
                self.save_metadata();
            }
            9 => self.export_metadata(),
            10 => {
                self.credential_export_dialog = Some(CredentialExportDialog {
                    fields: [String::new(), String::new()],
                    selected_field: 0,
                    error: None,
                })
            }
            11 => self.import_configuration(),
            12 => self.import_credentials(),
            13 => {
                self.sync_dialog = Some(SyncSettingsDialog {
                    remote: self.sync_settings.remote.clone(),
                    branch: if self.sync_settings.branch.is_empty() {
                        "main".into()
                    } else {
                        self.sync_settings.branch.clone()
                    },
                    selected: 0,
                    testing: false,
                });
            }
            _ => {}
        }
    }

    pub fn poll_sync_test(&mut self) {
        let Some(pending) = &self.sync_test_pending else {
            self.poll_sync_worker();
            return;
        };
        match pending.try_result() {
            Ok(Some(Ok(()))) => {
                self.sync_test_pending = None;
                self.notice =
                    Some("Git remote connection succeeded. Data sync is not enabled yet.".into());
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
            Ok(Some(Err(error))) => {
                self.sync_test_pending = None;
                self.notice = Some(error);
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
            Err(_) => {
                self.sync_test_pending = None;
                self.notice = Some("Git connection test stopped unexpectedly.".into());
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
            Ok(None) => {}
        }
        self.poll_sync_worker();
    }

    fn poll_sync_worker(&mut self) {
        let Some(worker) = &self.sync_worker else {
            return;
        };
        match worker.try_recv() {
            Ok(Ok(crate::sync_engine::SyncResult::Prepared(prepared))) => {
                self.sync_worker = None;
                if prepared.plan.conflicts.is_empty() {
                    self.start_sync_apply(*prepared);
                } else {
                    self.sync_conflicts = Some(*prepared);
                    self.notice = None;
                }
            }
            Ok(Ok(crate::sync_engine::SyncResult::Applied(metadata))) => {
                self.sync_worker = None;
                self.metadata = *metadata;
                self.hosts = HostCatalog::load();
                self.reconcile_metadata();
                self.refresh_host_key_sources();
                self.project_service.restore(&self.metadata.projects);
                self.notice = Some("Configuration sync completed.".into());
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
            Ok(Err(error)) => {
                self.sync_worker = None;
                self.reload_after_sync_attempt();
                self.notice = Some(crate::diagnostics::redact_sensitive_text(&error));
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.sync_worker = None;
                self.reload_after_sync_attempt();
                self.notice = Some("Configuration sync worker stopped unexpectedly.".into());
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn reload_after_sync_attempt(&mut self) {
        self.hosts = HostCatalog::load();
        if let Ok(metadata) = Metadata::load() {
            self.metadata = metadata;
            self.reconcile_metadata();
            self.refresh_host_key_sources();
            self.project_service.restore(&self.metadata.projects);
        }
    }

    fn start_sync_apply(&mut self, prepared: crate::sync_engine::Prepared) {
        match crate::sync_engine::apply(prepared) {
            Ok(worker) => {
                self.sync_worker = Some(worker.receiver);
                self.notice = Some("Applying validated sync and pushing to Git...".into());
            }
            Err(error) => {
                self.notice = Some(format!("Could not start sync apply: {error}"));
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = false;
                }
            }
        }
    }

    fn start_sync_prepare(&mut self, settings: crate::sync::SyncSettings) {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            self.notice = Some("HOME is not set; cannot synchronize SSH configuration.".into());
            return;
        };
        let metadata_path = match self.metadata_path() {
            Ok(path) => path,
            Err(error) => {
                self.notice = Some(format!("Could not locate LazyTerm configuration: {error}"));
                return;
            }
        };
        let directory = match Metadata::data_dir() {
            Ok(path) => path.join("sync"),
            Err(error) => {
                self.notice = Some(format!("Could not locate sync state directory: {error}"));
                return;
            }
        };
        match crate::sync_engine::prepare(settings, home, metadata_path, directory) {
            Ok(worker) => {
                self.sync_worker = Some(worker.receiver);
                self.notice = Some("Comparing local configuration with Git remote...".into());
                if let Some(dialog) = self.sync_dialog.as_mut() {
                    dialog.testing = true;
                }
            }
            Err(error) => self.notice = Some(format!("Could not start sync: {error}")),
        }
    }

    fn resolve_sync_conflicts(&mut self, use_remote: bool) {
        let Some(mut prepared) = self.sync_conflicts.take() else {
            return;
        };
        match prepared.resolve_selected(use_remote) {
            Ok(()) if prepared.plan.conflicts.is_empty() => self.start_sync_apply(prepared),
            Ok(()) => self.sync_conflicts = Some(prepared),
            Err(error) => {
                self.sync_conflicts = Some(prepared);
                self.notice = Some(format!("Could not resolve sync conflicts: {error}"));
            }
        }
    }

    fn handle_sync_settings_key(&mut self, key: KeyCode) {
        let Some(dialog) = self.sync_dialog.as_mut() else {
            return;
        };
        match key {
            KeyCode::Esc if dialog.testing => {}
            KeyCode::Esc => self.sync_dialog = None,
            KeyCode::Tab | KeyCode::Down => dialog.selected = (dialog.selected + 1) % 5,
            KeyCode::BackTab | KeyCode::Up => dialog.selected = dialog.selected.wrapping_add(4) % 5,
            KeyCode::Backspace if dialog.selected == 0 => {
                dialog.remote.pop();
            }
            KeyCode::Backspace if dialog.selected == 1 => {
                dialog.branch.pop();
            }
            KeyCode::Char(character) if dialog.selected == 0 => dialog.remote.push(character),
            KeyCode::Char(character) if dialog.selected == 1 => dialog.branch.push(character),
            KeyCode::Enter if dialog.selected == 2 => {
                let settings = crate::sync::SyncSettings {
                    remote: dialog.remote.trim().to_string(),
                    branch: dialog.branch.trim().to_string(),
                };
                match settings
                    .validate()
                    .and_then(|()| settings.save().map_err(|error| error.to_string()))
                {
                    Ok(()) => {
                        self.sync_settings = settings;
                        self.notice = Some("Git sync settings saved.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            KeyCode::Enter if dialog.selected == 3 && !dialog.testing => {
                let settings = crate::sync::SyncSettings {
                    remote: dialog.remote.trim().to_string(),
                    branch: dialog.branch.trim().to_string(),
                };
                match crate::sync::ConnectionTest::start(settings) {
                    Ok(test) => {
                        dialog.testing = true;
                        self.sync_test_pending = Some(test);
                        self.notice = Some("Testing Git remote connection...".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            KeyCode::Enter if dialog.selected == 4 && !dialog.testing => {
                let settings = crate::sync::SyncSettings {
                    remote: dialog.remote.trim().to_string(),
                    branch: dialog.branch.trim().to_string(),
                };
                match settings
                    .validate()
                    .and_then(|()| settings.save().map_err(|error| error.to_string()))
                {
                    Ok(()) => {
                        self.sync_settings = settings.clone();
                        self.start_sync_prepare(settings);
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            _ => {}
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        let Some(dialog) = self.sync_dialog.as_mut() else {
            return;
        };
        let value = text
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        match dialog.selected {
            0 => dialog.remote.push_str(&value),
            1 => dialog.branch.push_str(&value),
            _ => {}
        }
    }

    fn open_settings_picker(&mut self, setting: usize) {
        let selected = if setting == 0 {
            Theme::ALL
                .iter()
                .position(|theme| *theme == self.metadata.theme)
                .unwrap_or(0)
        } else {
            Language::ALL
                .iter()
                .position(|language| *language == self.metadata.language)
                .unwrap_or(0)
        };
        self.settings_picker = Some(SettingsPicker {
            setting,
            selected,
            original: selected,
        });
    }

    fn handle_settings_picker_key(&mut self, key: KeyCode) {
        let Some(mut picker) = self.settings_picker else {
            return;
        };
        let count = if picker.setting == 0 {
            Theme::ALL.len()
        } else {
            Language::ALL.len()
        };
        match key {
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(count - 1),
            KeyCode::Enter => {
                if picker.setting == 0 {
                    self.metadata.theme = Theme::ALL[picker.selected];
                } else {
                    self.metadata.language = Language::ALL[picker.selected];
                }
                self.settings_picker = None;
                if !self.save_metadata() {
                    if picker.setting == 0 {
                        self.metadata.theme = Theme::ALL[picker.original];
                    } else {
                        self.metadata.language = Language::ALL[picker.original];
                    }
                }
                return;
            }
            KeyCode::Esc => {
                if picker.setting == 0 {
                    self.metadata.theme = Theme::ALL[picker.original];
                } else {
                    self.metadata.language = Language::ALL[picker.original];
                }
                self.settings_picker = None;
                return;
            }
            _ => {}
        }
        if picker.setting == 0 {
            self.metadata.theme = Theme::ALL[picker.selected];
        } else {
            self.metadata.language = Language::ALL[picker.selected];
        }
        self.settings_picker = Some(picker);
    }

    fn handle_credential_export_key(&mut self, key: KeyCode) {
        let Some(dialog) = self.credential_export_dialog.as_mut() else {
            return;
        };
        match key {
            KeyCode::Esc => {
                dialog.fields.iter_mut().for_each(String::zeroize);
                self.credential_export_dialog = None;
            }
            KeyCode::Tab | KeyCode::Down => {
                dialog.selected_field = (dialog.selected_field + 1) % dialog.fields.len();
                dialog.error = None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field =
                    (dialog.selected_field + dialog.fields.len() - 1) % dialog.fields.len();
                dialog.error = None;
            }
            KeyCode::Backspace => {
                dialog.fields[dialog.selected_field].pop();
                dialog.error = None;
            }
            KeyCode::Enter => {
                let Some(mut dialog) = self.credential_export_dialog.take() else {
                    return;
                };
                let selected_field = dialog.selected_field;
                let password = Zeroizing::new(std::mem::take(&mut dialog.fields[0]));
                let confirmation = Zeroizing::new(std::mem::take(&mut dialog.fields[1]));
                if password.len() < 8 {
                    self.credential_export_dialog = Some(CredentialExportDialog {
                        fields: [password.to_string(), confirmation.to_string()],
                        selected_field,
                        error: Some("Use at least 8 characters.".into()),
                    });
                } else if password.as_str() != confirmation.as_str() {
                    self.credential_export_dialog = Some(CredentialExportDialog {
                        fields: [password.to_string(), confirmation.to_string()],
                        selected_field,
                        error: Some("The two passwords do not match.".into()),
                    });
                } else {
                    self.export_credentials(password);
                }
            }
            KeyCode::Char(value) => {
                if !value.is_control() {
                    dialog.fields[dialog.selected_field].push(value);
                    dialog.error = None;
                }
            }
            _ => {}
        }
    }

    fn export_credentials(&mut self, password: Zeroizing<String>) {
        let links = self
            .metadata
            .projects
            .iter()
            .flat_map(|project| project.links.iter().map(move |link| (project.id, link)))
            .collect::<Vec<_>>();
        if !links
            .iter()
            .any(|(_, link)| link.password_ref.is_some() || link.password.is_some())
            && !self
                .metadata
                .credentials
                .iter()
                .any(|item| item.password_ref.is_some())
        {
            self.credential_export_dialog = Some(CredentialExportDialog {
                fields: [String::new(), String::new()],
                selected_field: 0,
                error: Some("No saved credentials are available to export.".into()),
            });
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export encrypted credentials")
            .set_file_name("lazyterm-credentials.age")
            .add_filter("Age encrypted file", &["age"])
            .save_file()
        else {
            self.notice = Some("Credential export cancelled.".into());
            return;
        };
        let mut credentials = Vec::new();
        let mut seen_references = std::collections::BTreeSet::new();
        for item in &self.metadata.credentials {
            if let Some(reference) = item.password_ref.as_deref()
                && seen_references.insert(reference.to_string())
            {
                let secret = match crate::project_credentials::read_password(reference) {
                    Ok(secret) => secret,
                    Err(error) => {
                        self.notice =
                            Some(format!("Could not read credential for export: {error}"));
                        return;
                    }
                };
                credentials.push(CredentialRecord {
                    reference: reference.to_string(),
                    password: secret.to_string(),
                });
            }
        }
        for (project_id, link) in links {
            if let Some(reference) = link.password_ref.as_deref()
                && seen_references.insert(reference.to_string())
            {
                let secret = match crate::project_credentials::read_password(reference) {
                    Ok(secret) => secret,
                    Err(error) => {
                        self.notice =
                            Some(format!("Could not read credential for export: {error}"));
                        return;
                    }
                };
                credentials.push(CredentialRecord {
                    reference: reference.to_string(),
                    password: secret.to_string(),
                });
            } else if let Some(password) = link.password.as_deref() {
                credentials.push(CredentialRecord {
                    reference: format!("config:{project_id}:{}", link.id),
                    password: password.to_string(),
                });
            }
        }
        let payload = CredentialExport {
            format_version: 1,
            credentials,
        };
        let plaintext = match serde_json::to_vec_pretty(&payload) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(error) => {
                self.notice = Some(format!("Could not prepare credential export: {error}"));
                return;
            }
        };
        let encrypted = match Self::encrypt_credentials(&plaintext, password) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.notice = Some(format!("Could not encrypt credentials: {error}"));
                return;
            }
        };
        match write_private_export(&path, &encrypted) {
            Ok(()) => {
                self.notice = Some(format!(
                    "Encrypted credentials exported to {}.",
                    path.display()
                ))
            }
            Err(error) => self.notice = Some(format!("Could not export credentials: {error}")),
        }
    }

    fn encrypt_credentials(
        plaintext: &[u8],
        password: Zeroizing<String>,
    ) -> std::io::Result<Vec<u8>> {
        let encryptor = Encryptor::with_user_passphrase(SecretString::from(password.to_string()));
        let mut output = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut output)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        writer.write_all(plaintext)?;
        writer
            .finish()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(output)
    }

    fn metadata_path(&self) -> io::Result<PathBuf> {
        #[cfg(test)]
        {
            self.metadata_save_path
                .clone()
                .ok_or_else(|| io::Error::other("Tests must set an isolated metadata_save_path"))
        }
        #[cfg(not(test))]
        {
            Metadata::data_dir().map(|directory| directory.join("hosts.json"))
        }
    }

    fn import_configuration(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import LazyTerm configuration")
            .add_filter("LazyTerm configuration", &["json"])
            .pick_file()
        else {
            self.notice = Some("Configuration import cancelled.".into());
            return;
        };
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let parsed = read_limited(&path, 64 * 1024 * 1024)
            .and_then(|bytes| crate::import_export::parse_configuration(&bytes, &home));
        match parsed {
            Ok(import) => {
                self.pending_configuration_import = Some(PendingConfigurationImport {
                    import,
                    source_path: path,
                });
                self.import_confirmation = Some(ImportKind::Configuration);
            }
            Err(error) => {
                self.notice = Some(format!(
                    "Configuration import rejected; no files were changed: {}",
                    crate::diagnostics::redact_sensitive_text(&error.to_string())
                ));
            }
        }
    }

    fn import_credentials(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import encrypted credentials")
            .add_filter("Age encrypted credentials", &["age"])
            .pick_file()
        else {
            self.notice = Some("Credential import cancelled.".into());
            return;
        };
        self.credential_import_dialog = Some(CredentialImportDialog {
            password: String::new(),
            error: None,
        });
        self.pending_credential_import_path = Some(path);
    }

    fn decrypt_credential_archive(
        bytes: &[u8],
        password: Zeroizing<String>,
    ) -> io::Result<CredentialImport> {
        let decryptor = Decryptor::new(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid age file."))?;
        let identity = age::scrypt::Identity::new(SecretString::from(password.to_string()));
        let reader = decryptor
            .decrypt(std::iter::once(&identity as _))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Could not decrypt credentials; check the export password.",
                )
            })?;
        let mut plaintext = Zeroizing::new(Vec::new());
        reader
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut plaintext)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Encrypted credential file is incomplete.",
                )
            })?;
        if plaintext.len() > 32 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Decrypted credential file is too large.",
            ));
        }
        let archive: CredentialImport = serde_json::from_slice(&plaintext)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid credential data."))?;
        if archive.format_version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported credential export version.",
            ));
        }
        let mut references = std::collections::BTreeSet::new();
        for record in &archive.credentials {
            if !references.insert(record.reference.as_str()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Credential file contains duplicate references.",
                ));
            }
            if !crate::project_credentials::valid_password_reference(&record.reference)
                && !record.reference.starts_with("config:")
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Credential file contains an invalid reference.",
                ));
            }
        }
        Ok(archive)
    }

    fn apply_credential_archive(
        &mut self,
        archive: CredentialImport,
        export_password: Zeroizing<String>,
    ) -> Result<usize, String> {
        let imported_count = archive.credentials.len();
        let mut replacements: Vec<KeychainReplacement> = Vec::new();
        let mut updated_metadata = None;
        for mut record in archive.credentials {
            if let Some(target) = self
                .metadata
                .credential_import_targets
                .get(&record.reference)
            {
                record.reference = target.clone();
            }
            if crate::project_credentials::valid_password_reference(&record.reference) {
                let old = crate::project_credentials::read_optional_password(&record.reference)?;
                replacements.push((
                    std::mem::take(&mut record.reference),
                    Zeroizing::new(std::mem::take(&mut record.password)),
                    old,
                ));
                continue;
            }
            let parts = record.reference.split(':').collect::<Vec<_>>();
            let [_, project_id, link_id] = parts.as_slice() else {
                return Err(
                    "Credential file contains an invalid Project password reference.".into(),
                );
            };
            let project_id = project_id
                .parse::<uuid::Uuid>()
                .map_err(|_| "Credential file contains an invalid Project reference.")?;
            let link_id = link_id
                .parse::<uuid::Uuid>()
                .map_err(|_| "Credential file contains an invalid Web Link reference.")?;
            let metadata = updated_metadata.get_or_insert_with(|| self.metadata.clone());
            let Some(link) = metadata
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .and_then(|project| project.links.iter_mut().find(|link| link.id == link_id))
            else {
                return Err(
                    "Import the matching configuration first; a Project Web Link was not found."
                        .into(),
                );
            };
            link.password = Some(std::mem::take(&mut record.password));
            link.password_ref = None;
            link.password_storage = crate::projects::PasswordStorage::Config;
        }

        let previous_credential_records = replacements
            .iter()
            .filter_map(|(reference, _, previous)| {
                previous.as_ref().map(|password| CredentialRecord {
                    reference: reference.clone(),
                    password: password.to_string(),
                })
            })
            .collect::<Vec<_>>();
        if !previous_credential_records.is_empty() {
            let directory = self
                .metadata_path()
                .map_err(|error| error.to_string())?
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("import-backups");
            fs::create_dir_all(&directory)
                .map_err(|_| "Could not create private credential backup directory.".to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                    .map_err(|_| "Could not secure credential backup directory.".to_string())?;
            }
            let backup = CredentialExport {
                format_version: 1,
                credentials: previous_credential_records,
            };
            let plaintext = Zeroizing::new(
                serde_json::to_vec(&backup)
                    .map_err(|_| "Could not prepare credential backup.".to_string())?,
            );
            let encrypted = Self::encrypt_credentials(&plaintext, export_password)
                .map_err(|_| "Could not encrypt credential backup.".to_string())?;
            let path = directory.join(format!(
                "credentials-{}.age",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "System clock is invalid.")?
                    .as_nanos()
            ));
            write_private_export(&path, &encrypted)
                .map_err(|_| "Could not save encrypted credential backup.".to_string())?;
        }

        let mut changed = Vec::new();
        for (reference, password, _) in &replacements {
            if let Err(error) =
                crate::project_credentials::replace_password(reference, password.as_str())
            {
                let rollback = restore_imported_keychain(&replacements, &changed);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(format!(
                        "{error}; credential rollback also failed: {rollback_error}"
                    )),
                };
            }
            changed.push(reference.clone());
        }

        if let Some(metadata) = updated_metadata {
            let previous = std::mem::replace(&mut self.metadata, metadata);
            if !self.save_metadata() {
                let error = self
                    .notice
                    .take()
                    .unwrap_or_else(|| "Could not save imported credentials.".into());
                self.metadata = previous;
                let rollback = restore_imported_keychain(&replacements, &changed);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(format!(
                        "{error}; credential rollback also failed: {rollback_error}"
                    )),
                };
            }
        }
        Ok(imported_count)
    }

    fn handle_credential_import_key(&mut self, key: KeyCode) {
        let Some(dialog) = self.credential_import_dialog.as_mut() else {
            return;
        };
        match key {
            KeyCode::Esc => {
                dialog.password.zeroize();
                self.credential_import_dialog = None;
                self.pending_credential_import_path = None;
            }
            KeyCode::Backspace => {
                dialog.password.pop();
                dialog.error = None;
            }
            KeyCode::Enter => {
                let Some(mut dialog) = self.credential_import_dialog.take() else {
                    return;
                };
                let password = Zeroizing::new(std::mem::take(&mut dialog.password));
                let Some(path) = self.pending_credential_import_path.take() else {
                    self.notice = Some("Credential import was cancelled.".into());
                    return;
                };
                let result = read_limited(&path, 16 * 1024 * 1024)
                    .and_then(|bytes| Self::decrypt_credential_archive(&bytes, password.clone()))
                    .map_err(|error| error.to_string());
                match result {
                    Ok(archive) => {
                        self.pending_credential_import = Some(PendingCredentialImport {
                            archive,
                            export_password: password,
                        });
                        self.import_confirmation = Some(ImportKind::Credentials);
                    }
                    Err(error) => {
                        self.notice = Some(format!(
                            "Credential import rejected: {}",
                            crate::diagnostics::redact_sensitive_text(&error)
                        ));
                    }
                }
            }
            KeyCode::Char(value) if !value.is_control() => {
                dialog.password.push(value);
                dialog.error = None;
            }
            _ => {}
        }
    }

    fn handle_import_confirmation_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.import_confirmation = None;
                self.pending_configuration_import = None;
                self.pending_credential_import = None;
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                let Some(kind) = self.import_confirmation.take() else {
                    return;
                };
                if kind == ImportKind::Configuration {
                    let Some(pending) = self.pending_configuration_import.take() else {
                        return;
                    };
                    let result = self.metadata_path().and_then(|path| {
                        crate::import_export::apply_configuration(pending.import, &path)
                    });
                    match result {
                        Ok(metadata) => {
                            self.metadata = metadata;
                            self.metadata_error = false;
                            self.hosts = HostCatalog::load();
                            self.clamp_selection();
                            self.notice = Some(format!(
                                "Configuration imported from {}. Previous files are backed up beside LazyTerm data.",
                                pending.source_path.display()
                            ));
                        }
                        Err(error) => {
                            self.notice = Some(format!(
                                "Configuration import failed: {}",
                                crate::diagnostics::redact_sensitive_text(&error.to_string())
                            ));
                        }
                    }
                } else if let Some(pending) = self.pending_credential_import.take() {
                    match self.apply_credential_archive(pending.archive, pending.export_password) {
                        Ok(count) => {
                            self.notice =
                                Some(format!("Imported {count} encrypted credential entries."));
                        }
                        Err(error) => {
                            self.notice = Some(format!(
                                "Credential import failed: {}",
                                crate::diagnostics::redact_sensitive_text(&error)
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_settings_key(&mut self, key: KeyCode) {
        if self.sync_dialog.is_some() {
            self.handle_sync_settings_key(key);
            return;
        }
        if self.settings_picker.is_some() {
            self.handle_settings_picker_key(key);
            return;
        }
        let count = 14;
        match key {
            KeyCode::Up => {
                self.selected_setting = self.selected_setting.saturating_sub(1);
            }
            KeyCode::Down => {
                self.selected_setting = (self.selected_setting + 1).min(count - 1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_setting(),
            KeyCode::Left | KeyCode::Right
                if self.selected_setting == 0 || self.selected_setting == 1 =>
            {
                self.toggle_setting();
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Esc => self.close_settings(),
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
    }

    fn close_settings(&mut self) {
        let target = if self.visible_sections().contains(&self.previous_section) {
            self.previous_section
        } else {
            self.visible_sections()
                .into_iter()
                .next()
                .unwrap_or(NavSection::Projects)
        };
        self.switch_section(target);
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
        if self.sync_dialog.is_some() {
            self.handle_sync_settings_key(key);
            return;
        }
        if self.host_group_dialog.is_some() {
            self.handle_host_group_dialog_key(key);
            return;
        }
        if self.host_action_dialog.is_some() {
            self.handle_host_action_dialog_key(key);
            return;
        }
        if self.connection_dialog.is_some() {
            let dialog = self.connection_dialog.as_mut().expect("dialog exists");
            match key {
                KeyCode::Up => dialog.scroll = dialog.scroll.saturating_sub(1),
                KeyCode::Down => dialog.scroll = dialog.scroll.saturating_add(1),
                KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(6),
                KeyCode::PageDown => dialog.scroll = dialog.scroll.saturating_add(6),
                KeyCode::Esc if dialog.pending && dialog.cancelable => {
                    self.connection_dialog = None;
                    self.native_connection = None;
                }
                KeyCode::Enter | KeyCode::Esc if !dialog.pending => {
                    self.connection_dialog = None;
                }
                _ => {}
            }
            return;
        }
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
        if self.section == NavSection::Settings {
            self.handle_settings_key(key);
            return;
        }
        if self.section == NavSection::Credentials {
            self.handle_credentials_key(key);
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
            KeyCode::Char('g')
                if self.section == NavSection::Hosts && self.selected_group().is_none() =>
            {
                if let Some(host) = self.selected_host() {
                    self.tags_dialog = Some((host.alias.clone(), self.tags(host).join(", ")));
                }
            }
            KeyCode::Char('r') if self.section == NavSection::Hosts => self.refresh_hosts(),
            KeyCode::Char('g')
                if self.section == NavSection::Hosts
                    && self.showing_groups()
                    && (self.selected_group().is_some() || self.selected_host().is_none()) =>
            {
                self.host_group_dialog = Some(HostGroupDialog {
                    name: String::new(),
                    original_name: None,
                    deleting: false,
                });
            }
            KeyCode::Char('e')
                if self.section == NavSection::Hosts
                    && self.showing_groups()
                    && self.selected_group().is_some() =>
            {
                if let Some(HostGroup::Tag(name)) = self.selected_group() {
                    self.host_group_dialog = Some(HostGroupDialog {
                        name: name.clone(),
                        original_name: Some(name),
                        deleting: false,
                    });
                }
            }
            KeyCode::Char('d')
                if self.section == NavSection::Hosts
                    && self.showing_groups()
                    && self.selected_group().is_some() =>
            {
                if let Some(HostGroup::Tag(name)) = self.selected_group() {
                    self.host_group_dialog = Some(HostGroupDialog {
                        name: name.clone(),
                        original_name: Some(name),
                        deleting: true,
                    });
                }
            }
            KeyCode::Char('a') if self.section == NavSection::Hosts => self.open_host_form(None),
            KeyCode::Char('k') if self.section == NavSection::Hosts => {
                if let Some(host) = self.selected_host() {
                    self.open_host_ssh_key_selector(host.alias.clone());
                }
            }
            KeyCode::Char('x') if self.section == NavSection::Hosts => {
                if let Some(alias) = self.selected_host().map(|host| host.alias.clone()) {
                    self.confirm_delete_host_password(&alias);
                }
            }
            KeyCode::Char('p') if self.section == NavSection::Hosts => {
                if let Some(alias) = self.selected_host().map(|host| host.alias.clone()) {
                    self.open_host_password_editor(&alias);
                }
            }
            KeyCode::Char('e') if self.section == NavSection::Hosts => {
                if let Some(host) = self.selected_host() {
                    self.open_host_form(Some(host.clone()));
                }
            }
            KeyCode::Char('d') if self.section == NavSection::Hosts => {
                if let Some(host) = self.selected_host() {
                    self.host_dialog = Some(HostDialog {
                        fields: [
                            host.alias.clone(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                            String::new(),
                        ],
                        tags: Vec::new(),
                        tag_picker_open: false,
                        tag_filter: String::new(),
                        tag_picker_selected: 0,
                        proxy_jump_picker_open: false,
                        proxy_jump_picker_selected: 0,
                        original_alias: None,
                        selected_field: 0,
                        editing: false,
                        deleting: true,
                    });
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
            KeyCode::Char('2') => self.switch_section(NavSection::Files),
            KeyCode::Char('3') => self.switch_section(NavSection::Tunnels),
            KeyCode::Char('4') => self.switch_section(NavSection::Snippets),
            KeyCode::Char('5') => self.switch_section(NavSection::Runtime),
            KeyCode::Char('6') => self.switch_section(NavSection::Projects),
            KeyCode::Char('7') => self.switch_section(NavSection::Credentials),
            KeyCode::Char(',') => self.switch_section(NavSection::Settings),
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

    fn open_snippet_form(&mut self, package_id: uuid::Uuid, snippet: Option<SnippetDefinition>) {
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
                let Some(dialog) = self.package_dialog.take() else {
                    return;
                };
                let name = dialog.fields[0].trim();
                if name.is_empty() {
                    self.notice = Some("Package name is required.".to_string());
                    self.package_dialog = Some(dialog);
                    return;
                }
                let id = dialog.editing_id.unwrap_or_else(uuid::Uuid::new_v4);
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
                if dialog.selected_field < 3 {
                    dialog.fields[dialog.selected_field].pop();
                }
            }
            KeyCode::Char(' ') if dialog.selected_field == 3 => {
                let dangerous = dialog.fields[3].parse::<bool>().unwrap_or(false);
                dialog.fields[3] = (!dangerous).to_string();
            }
            KeyCode::Enter => {
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
                let dangerous = dialog.fields[3].parse::<bool>().unwrap_or(false);
                let id = dialog.editing_id.unwrap_or_else(uuid::Uuid::new_v4);
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
                    self.notice = Some(format!("Saved snippet {name}."));
                    self.save_metadata();
                }
            }
            KeyCode::Char(value) if dialog.selected_field < 3 && !value.is_control() => {
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
        let inherited_group = self.active_group.clone().or_else(|| self.selected_group());
        let tags = host.as_ref().map_or_else(
            || match &inherited_group {
                Some(HostGroup::Tag(tag)) => vec![tag.clone()],
                _ => Vec::new(),
            },
            |host| self.tags(host).to_vec(),
        );
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
                        host.proxy_jump.unwrap_or_default(),
                    ]
                },
            ),
            tags,
            tag_picker_open: false,
            tag_filter: String::new(),
            tag_picker_selected: 0,
            proxy_jump_picker_open: false,
            proxy_jump_picker_selected: 0,
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
                let id = dialog.editing_id.unwrap_or_else(uuid::Uuid::new_v4);
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
        if self
            .host_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.proxy_jump_picker_open)
        {
            let aliases = self.proxy_jump_choices();
            let dialog = self.host_dialog.as_mut().expect("dialog exists");
            match key {
                KeyCode::Esc => dialog.proxy_jump_picker_open = false,
                KeyCode::Up => {
                    dialog.proxy_jump_picker_selected =
                        dialog.proxy_jump_picker_selected.saturating_sub(1);
                }
                KeyCode::Down if !aliases.is_empty() => {
                    dialog.proxy_jump_picker_selected =
                        (dialog.proxy_jump_picker_selected + 1).min(aliases.len() - 1);
                }
                KeyCode::Enter => {
                    if let Some(alias) = aliases.get(dialog.proxy_jump_picker_selected) {
                        dialog.fields[5] = alias.clone();
                        dialog.proxy_jump_picker_open = false;
                    }
                }
                _ => {}
            }
            return;
        }
        if self
            .host_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.tag_picker_open)
        {
            let available = self
                .metadata
                .tags
                .values()
                .flatten()
                .cloned()
                .chain(self.metadata.host_groups.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let dialog = self.host_dialog.as_ref().expect("dialog exists");
            let filter = dialog.tag_filter.to_lowercase();
            let matches = available
                .iter()
                .filter(|tag| tag.to_lowercase().contains(&filter))
                .cloned()
                .collect::<Vec<_>>();
            let exact_match = available
                .iter()
                .any(|tag| tag.eq_ignore_ascii_case(dialog.tag_filter.trim()));
            let has_create = !dialog.tag_filter.trim().is_empty() && !exact_match;
            let option_count = matches.len() + usize::from(has_create);
            match key {
                KeyCode::Esc => {
                    let dialog = self.host_dialog.as_mut().expect("dialog exists");
                    dialog.tag_picker_open = false;
                    dialog.tag_filter.clear();
                }
                KeyCode::Up => {
                    let dialog = self.host_dialog.as_mut().expect("dialog exists");
                    dialog.tag_picker_selected = dialog.tag_picker_selected.saturating_sub(1);
                }
                KeyCode::Down if option_count > 0 => {
                    let dialog = self.host_dialog.as_mut().expect("dialog exists");
                    dialog.tag_picker_selected =
                        (dialog.tag_picker_selected + 1).min(option_count - 1);
                }
                KeyCode::Backspace => {
                    let dialog = self.host_dialog.as_mut().expect("dialog exists");
                    dialog.tag_filter.pop();
                    dialog.tag_picker_selected = 0;
                }
                KeyCode::Char(' ') if dialog.tag_filter.is_empty() && option_count > 0 => {
                    if let Some(tag) = matches.get(dialog.tag_picker_selected) {
                        let dialog = self.host_dialog.as_mut().expect("dialog exists");
                        toggle_host_tag(&mut dialog.tags, tag);
                    }
                }
                KeyCode::Char(' ') if option_count > 0 => {
                    if let Some(tag) = matches.get(dialog.tag_picker_selected) {
                        let dialog = self.host_dialog.as_mut().expect("dialog exists");
                        toggle_host_tag(&mut dialog.tags, tag);
                    }
                }
                KeyCode::Enter if has_create && dialog.tag_picker_selected == matches.len() => {
                    let tag = dialog.tag_filter.trim().to_string();
                    let previous_groups = self.metadata.host_groups.clone();
                    self.metadata.host_groups.push(tag.clone());
                    self.metadata.host_groups.sort();
                    self.metadata.host_groups.dedup();
                    if self.save_metadata() {
                        if let Some(dialog) = self.host_dialog.as_mut() {
                            toggle_host_tag(&mut dialog.tags, &tag);
                            dialog.tag_picker_open = false;
                            dialog.tag_filter.clear();
                            dialog.tag_picker_selected = 0;
                        }
                        self.notice = Some(format!(
                            "Created group {tag}; save Host to apply the group."
                        ));
                    } else {
                        self.metadata.host_groups = previous_groups;
                    }
                }
                KeyCode::Enter => {
                    let dialog = self.host_dialog.as_mut().expect("dialog exists");
                    dialog.tag_picker_open = false;
                    dialog.tag_filter.clear();
                }
                KeyCode::Char(value) if !value.is_control() => {
                    let dialog = self.host_dialog.as_mut().expect("dialog exists");
                    dialog.tag_filter.push(value);
                    dialog.tag_picker_selected = 0;
                }
                _ => {}
            }
            return;
        }
        let dialog = self.host_dialog.as_mut().expect("dialog exists");
        if dialog.deleting {
            match key {
                KeyCode::Enter | KeyCode::Char('y') => {
                    let alias = dialog.fields[0].clone();
                    self.host_dialog = None;
                    match self.hosts.delete_host(&alias) {
                        Ok(()) => {
                            self.metadata.tags.remove(&alias);
                            self.metadata.remote_paths.remove(&alias);
                            self.metadata.host_passwords.remove(&alias);
                            self.metadata.uploaded_keys.remove(&alias);
                            self.metadata.host_public_key_auth.remove(&alias);
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
            KeyCode::Tab | KeyCode::Down => dialog.selected_field = (dialog.selected_field + 1) % 7,
            KeyCode::BackTab | KeyCode::Up => {
                dialog.selected_field = (dialog.selected_field + 6) % 7
            }
            KeyCode::Backspace => {
                if dialog.selected_field == 2 {
                    dialog.tags.pop();
                } else {
                    dialog.fields[host_field_index(dialog.selected_field)].pop();
                }
            }
            KeyCode::Enter => {
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
                    proxy_jump: nonempty(&dialog.fields[5]),
                    managed: true,
                };
                match self
                    .hosts
                    .upsert_from(&host, dialog.original_alias.as_deref())
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
                            if let Some(id) = self.metadata.host_passwords.remove(original) {
                                self.metadata.host_passwords.insert(host.alias.clone(), id);
                            }
                            if let Some(keys) = self.metadata.uploaded_keys.remove(original) {
                                self.metadata.uploaded_keys.insert(host.alias.clone(), keys);
                            }
                            if self.metadata.host_public_key_auth.remove(original) {
                                self.metadata
                                    .host_public_key_auth
                                    .insert(host.alias.clone());
                            }
                            for tunnel in &mut self.metadata.tunnels {
                                if !tunnel.custom_host && tunnel.host_alias == *original {
                                    tunnel.host_alias = host.alias.clone();
                                }
                            }
                        }
                        let tags = dialog.tags.clone();
                        if tags.is_empty() {
                            self.metadata.tags.remove(&host.alias);
                        } else {
                            self.metadata.tags.insert(host.alias.clone(), tags);
                        }
                        self.notice = Some(format!("Saved host {}.", host.alias));
                        self.save_metadata();
                        self.clamp_selection();
                    }
                    Err(error) => self.notice = Some(format!("Could not save host: {error}")),
                }
            }
            KeyCode::Char(value) if !value.is_control() => {
                if dialog.selected_field == 6 && value == ' ' {
                    dialog.proxy_jump_picker_open = true;
                    dialog.proxy_jump_picker_selected = 0;
                } else if dialog.selected_field == 2 {
                    if value == ' ' {
                        dialog.tag_picker_open = true;
                        dialog.tag_filter.clear();
                        dialog.tag_picker_selected = 0;
                    }
                } else if dialog.selected_field != 2 {
                    dialog.fields[host_field_index(dialog.selected_field)].push(value);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn proxy_jump_choices(&self) -> Vec<String> {
        let Some(dialog) = self.host_dialog.as_ref() else {
            return Vec::new();
        };
        self.hosts
            .hosts
            .iter()
            .filter(|host| {
                !host.alias.eq_ignore_ascii_case(dialog.fields[0].trim())
                    && !dialog
                        .original_alias
                        .as_ref()
                        .is_some_and(|alias| host.alias.eq_ignore_ascii_case(alias))
            })
            .map(|host| host.alias.clone())
            .collect()
    }

    fn handle_host_action_dialog_key(&mut self, key: KeyCode) {
        if matches!(key, KeyCode::Enter | KeyCode::Esc) {
            self.host_action_dialog = None;
        }
    }

    fn handle_host_group_dialog_key(&mut self, key: KeyCode) {
        if self
            .host_group_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.deleting)
        {
            match key {
                KeyCode::Enter | KeyCode::Char('y') => {
                    let dialog = self.host_group_dialog.take().unwrap();
                    let Some(old_name) = dialog.original_name else {
                        return;
                    };
                    let previous_groups = self.metadata.host_groups.clone();
                    let previous_tags = self.metadata.tags.clone();
                    self.metadata.host_groups.retain(|group| group != &old_name);
                    for tags in self.metadata.tags.values_mut() {
                        tags.retain(|tag| tag != &old_name);
                    }
                    self.metadata.tags.retain(|_, tags| !tags.is_empty());
                    if self.save_metadata() {
                        self.notice = Some(format!("Deleted group {old_name}; hosts were kept."));
                        self.clamp_selection();
                    } else {
                        self.metadata.host_groups = previous_groups;
                        self.metadata.tags = previous_tags;
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => self.host_group_dialog = None,
                _ => {}
            }
            return;
        }

        let dialog = self.host_group_dialog.as_mut().unwrap();
        match key {
            KeyCode::Esc => self.host_group_dialog = None,
            KeyCode::Backspace => {
                dialog.name.pop();
            }
            KeyCode::Enter => {
                let Some(dialog) = self.host_group_dialog.take() else {
                    return;
                };
                let name = dialog.name.trim().to_string();
                if name.is_empty() || name.contains(',') {
                    self.notice = Some("Group name is required and cannot contain commas.".into());
                    self.host_group_dialog = Some(dialog);
                    return;
                }
                if self.metadata.host_groups.iter().any(|group| {
                    group.eq_ignore_ascii_case(&name)
                        && dialog.original_name.as_deref() != Some(group.as_str())
                }) {
                    self.notice = Some(format!("Group {name} already exists."));
                    self.host_group_dialog = Some(dialog);
                    return;
                }

                let previous_groups = self.metadata.host_groups.clone();
                let previous_tags = self.metadata.tags.clone();
                if let Some(old_name) = &dialog.original_name {
                    if old_name != &name {
                        self.metadata.host_groups.retain(|group| group != old_name);
                        self.metadata.host_groups.push(name.clone());
                        for tags in self.metadata.tags.values_mut() {
                            for tag in tags.iter_mut() {
                                if tag == old_name {
                                    *tag = name.clone();
                                }
                            }
                            tags.sort();
                            tags.dedup();
                        }
                    }
                } else {
                    self.metadata.host_groups.push(name.clone());
                }
                self.metadata.host_groups.sort();
                self.metadata.host_groups.dedup();
                if self.save_metadata() {
                    self.notice = Some(if dialog.original_name.is_some() {
                        format!("Renamed group to {name}.")
                    } else {
                        format!("Created group {name}.")
                    });
                    self.selected_browse_item = self
                        .visible_groups()
                        .iter()
                        .position(|group| group == &HostGroup::Tag(name.clone()))
                        .unwrap_or(0);
                    self.selected_group = self.selected_browse_item;
                } else {
                    self.metadata.host_groups = previous_groups;
                    self.metadata.tags = previous_tags;
                }
            }
            KeyCode::Char(value) if !value.is_control() => dialog.name.push(value),
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
                self.active_file_filter_mut().clear();
                self.reset_active_file_selection();
            }
            KeyCode::Backspace => {
                self.active_file_filter_mut().pop();
                self.reset_active_file_selection();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.file_filtering = false;
                self.active_file_pane = match self.active_file_pane {
                    FilePane::Local => FilePane::Remote,
                    FilePane::Remote => FilePane::Local,
                };
            }
            KeyCode::Char(character) if !character.is_control() => {
                self.active_file_filter_mut().push(character);
                self.reset_active_file_selection();
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
        let visible = self.visible_sections();
        let index = visible
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.switch_section(visible[(index + 1) % visible.len()]);
    }

    pub(crate) fn previous_section(&mut self) {
        let visible = self.visible_sections();
        let index = visible
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or_default();
        self.switch_section(visible[(index + visible.len() - 1) % visible.len()]);
    }

    fn refresh_hosts(&mut self) {
        self.hosts = HostCatalog::load();
        self.refresh_host_key_sources();
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

    #[track_caller]
    pub(crate) fn save_metadata(&mut self) -> bool {
        if self.metadata_error {
            self.notice = Some(
                "Metadata unavailable; changes cannot be saved. Reload after fixing hosts.json."
                    .into(),
            );
            false
        } else {
            self.metadata.reconcile(&self.hosts);
            self.metadata.normalize_host_groups();
            #[cfg(test)]
            let result = self
                .metadata_save_path
                .as_ref()
                .ok_or_else(|| {
                    std::io::Error::other("Tests must set an isolated metadata_save_path")
                })
                .and_then(|path| self.metadata.save_to(path));
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

fn toggle_host_tag(tags: &mut Vec<String>, tag: &str) {
    if let Some(index) = tags.iter().position(|existing| existing == tag) {
        tags.remove(index);
    } else if !tag.trim().is_empty() {
        tags.push(tag.trim().to_string());
        tags.sort();
    }
}

fn host_field_index(display_index: usize) -> usize {
    match display_index {
        0 => 0,
        1 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        6 => 5,
        _ => unreachable!("group field has no host config value"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::Theme;

    fn app_fixture() -> App {
        let mut app = App::with_catalog(HostCatalog::default(), Ok(Metadata::default()));
        // Keyboard tests must never write to the developer's metadata.
        app.metadata_error = true;
        app.viewport_width = 32;
        app
    }

    #[test]
    #[ignore = "isolated App/Tunnel owner for the disposable SSH recovery fixture"]
    fn tunnel_recovery_owner_process() {
        use crate::authentication::{Answer, Event, SavedHost, Service};
        let Some(root) = std::env::var_os("LAZYTERM_TUNNEL_RECOVERY_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let definition: TunnelDefinition =
            serde_json::from_slice(&fs::read(root.join("tunnel.json")).unwrap()).unwrap();
        let service = Service::new().unwrap();
        let auth = crate::credentials::HostAuth {
            broker: Some(service.client(vec![SavedHost {
                alias: "web".into(),
                username: Some("tester".into()),
                reference: None,
            }])),
            config: Some(root.join("config")),
            askpass: Some(PathBuf::from(
                std::env::var_os("LAZYTERM_TUNNEL_RECOVERY_BINARY").unwrap(),
            )),
            ..Default::default()
        };
        let mut app = app_fixture();
        app.metadata.tunnels.push(definition.clone());
        let (child, authentication) =
            tunnel::spawn_forward(&definition, app.tunnel_sender.clone(), &auth).unwrap();
        let pid = child.id();
        app.tunnel_processes.insert(definition.id, child);
        app.tunnel_authentication
            .insert(definition.id, authentication.unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ready = false;
        loop {
            while let Ok(event) = service.events.try_recv() {
                if let Event::Challenge(challenge) = event {
                    challenge.respond(Some(Answer {
                        password: zeroize::Zeroizing::new("fixture-password".into()),
                        save: false,
                    }));
                }
            }
            app.poll_tunnels();
            if !ready
                && app.tunnel_statuses.get(&definition.id) == Some(&TunnelStatus::Running)
                && std::net::TcpStream::connect(("127.0.0.1", definition.local_port)).is_ok()
            {
                fs::write(root.join("tunnel-owner-ready"), pid.to_string()).unwrap();
                ready = true;
            }
            if matches!(
                app.tunnel_statuses.get(&definition.id),
                Some(TunnelStatus::Failed(_))
            ) {
                assert!(ready, "Tunnel failed before the recovery fixture was ready");
                assert!(!app.tunnel_processes.contains_key(&definition.id));
                fs::write(root.join("tunnel-owner-failed"), b"failed").unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "Tunnel recovery owner timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn shared_authentication_waits_for_modals_cancels_cleanly_and_keeps_other_prompts_queued() {
        use crate::authentication::tests::{fixture, request};
        let (service, first_trace) = fixture(None);
        let first = request(&first_trace, "tester@127.0.0.1's password:");
        let (other_service, second_trace) = fixture(None);
        let second = request(&second_trace, "tester@127.0.0.1's password:");
        let mut app = app_fixture();
        app.authentication = Some(service);
        app.tags_dialog = Some(("web".into(), String::new()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.authentication_queue.is_empty() {
            app.poll_authentication();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.host_password_dialog.is_none());
        let crate::authentication::Event::Challenge(second_prompt) = other_service
            .events
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        else {
            panic!("Expected second challenge");
        };
        app.authentication_queue.push_back(second_prompt);
        app.tags_dialog = None;
        app.poll_authentication();
        assert_eq!(app.host_password_dialog.as_ref().unwrap().selected_field, 1);
        press(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
        app.poll_authentication();
        assert_eq!(
            app.host_password_dialog.as_ref().unwrap().password.as_str(),
            "p"
        );
        assert_eq!(app.authentication_queue.len(), 1);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(first.join().unwrap().is_err());
        app.poll_authentication();
        assert!(
            app.host_password_dialog.is_some(),
            "The other request survives cancellation"
        );
        drop(second_trace);
        app.poll_authentication();
        assert!(app.host_password_dialog.is_none());
        assert!(second.join().unwrap().is_err());
    }

    #[test]
    fn shared_authentication_saves_only_successful_passwords_and_replaces_the_previous_record() {
        use crate::authentication::tests::{fixture, request};
        let isolated = crate::projects::tests::Fixture::new("auth-save");
        let mut app = grouped_fixture();
        app.metadata_error = false;
        app.metadata_save_path = Some(isolated.root.join("hosts.json"));
        assert!(app.save_metadata());
        let mut old = crate::project_credentials::prepare_credential(Zeroizing::new("old".into()))
            .recv()
            .unwrap()
            .unwrap();
        assert!(app.save_host_password_prepared("web", "tester", &mut old, None));
        let previous = app.host_password_reference("web").unwrap().to_string();
        let (service, trace) = fixture(Some(previous.clone()));
        app.authentication = Some(service);
        let saved = request(&trace, "tester@127.0.0.1's password:");
        assert_eq!(&**saved.join().unwrap().unwrap(), b"old");
        let next = request(&trace, "tester@127.0.0.1's password:");
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.host_password_dialog.is_none() {
            app.poll_authentication();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        for value in "updated".chars() {
            press(&mut app, KeyCode::Char(value), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(&**next.join().unwrap().unwrap(), b"updated");
        app.poll_authentication();
        assert_eq!(app.host_password_reference("web"), Some(previous.as_str()));
        trace.observe("Authenticated to 127.0.0.1 ([127.0.0.1]:22) using \"password\".");
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.host_password_reference("web") == Some(previous.as_str()) {
            app.poll_authentication();
            assert!(Instant::now() < deadline, "{:?}", app.notice);
            std::thread::sleep(Duration::from_millis(10));
        }
        let reference = app.host_password_reference("web").unwrap();
        assert_eq!(
            crate::project_credentials::read_password(reference)
                .unwrap()
                .as_str(),
            "updated"
        );
        assert_eq!(app.metadata.credentials.iter().filter(|credential| credential.kind == crate::credentials::CredentialKind::Password).count(), 1);
        let contents = std::fs::read_to_string(app.metadata_save_path.as_ref().unwrap()).unwrap();
        assert!(!contents.contains("updated"));
        crate::project_credentials::delete_password(reference).unwrap();
    }

    #[test]
    fn toast_reappears_when_the_same_message_changes_source() {
        let mut app = app_fixture();
        app.notice = Some("Saved.".into());
        app.update_toast();
        let global_expiry = app.toast.as_ref().unwrap().expires_at;

        app.credential_manager.open = true;
        app.credential_manager.message = Some("Saved.".into());
        app.update_toast();
        assert!(app.toast.as_ref().unwrap().expires_at > global_expiry);

        let credential_expiry = app.toast.as_ref().unwrap().expires_at;
        app.credential_manager.open = false;
        app.update_toast();
        assert!(app.toast.as_ref().unwrap().expires_at > credential_expiry);
    }

    #[test]
    fn external_hosts_can_be_edited_and_deleted_after_confirmation() {
        let mut app = grouped_fixture();
        let fixture = crate::projects::tests::Fixture::new("host-action-dialog");
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.section = NavSection::Hosts;
        app.selected_browse_item = app.visible_groups().len()
            + app
                .matching_hosts()
                .iter()
                .position(|host| host.alias == "web")
                .unwrap();
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(app.host_dialog.as_ref().unwrap().deleting);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.host_dialog.is_none());
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(
            app.host_dialog.as_ref().unwrap().original_alias.as_deref(),
            Some("web")
        );
        assert!(app.host_action_dialog.is_none());
    }

    #[test]
    fn tests_without_an_explicit_metadata_path_cannot_save_user_configuration() {
        let mut app = App::with_catalog(HostCatalog::default(), Ok(Metadata::default()));
        assert!(!app.save_metadata());
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("isolated metadata_save_path")
        );
    }

    fn upload_password_purpose() -> HostPasswordPurpose {
        HostPasswordPurpose::UploadKey {
            credential: crate::credentials::Credential {
                id: uuid::Uuid::new_v4(),
                name: "test key".into(),
                kind: crate::credentials::CredentialKind::SshKey,
                username: String::new(),
                password_ref: None,
                private_key_path: None,
                public_key: String::new(),
                fingerprint: String::new(),
            },
            identity_configured: false,
        }
    }

    #[test]
    fn host_password_dialog_masks_flow_and_returns_upload_retry_action() {
        let mut app = app_fixture();
        app.prompt_host_password(
            "web".into(),
            "tester".into(),
            None,
            false,
            upload_password_purpose(),
        );
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.host_password_dialog.as_ref().unwrap().selected_field, 0);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            app.host_password_dialog.as_ref().unwrap().password.as_str(),
            "s"
        );
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.host_password_dialog.as_ref().unwrap().save_in_keychain);
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::RetryPublicKeyUpload
        );
        let submitted = app.take_pending_host_password().unwrap();
        assert_eq!(submitted.username, "tester");
        assert_eq!(submitted.password.as_str(), "s");
        assert!(submitted.save_in_keychain);
        assert_eq!(submitted.alias, "web");
    }

    #[test]
    fn host_password_requires_input_and_space_toggles_keychain_choice() {
        let mut app = app_fixture();
        app.prompt_host_password(
            "web".into(),
            "tester".into(),
            None,
            false,
            upload_password_purpose(),
        );
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert!(app.take_pending_host_password().is_none());
        assert!(app.host_password_dialog.as_ref().unwrap().error.is_some());
        assert_eq!(app.host_password_dialog.as_ref().unwrap().selected_field, 1);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.host_password_dialog.as_ref().unwrap().save_in_keychain);
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!app.host_password_dialog.as_ref().unwrap().save_in_keychain);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.host_password_dialog.is_none());
        assert!(app.take_pending_host_password().is_none());
    }

    #[test]
    fn host_password_dialog_edits_username_and_requires_both_fields() {
        let mut app = app_fixture();
        app.prompt_host_password(
            "web".into(),
            "dada".into(),
            None,
            false,
            upload_password_purpose(),
        );
        for _ in 0..4 {
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        }
        for value in "root".chars() {
            press(&mut app, KeyCode::Char(value), KeyModifiers::NONE);
        }
        assert_eq!(app.host_password_dialog.as_ref().unwrap().username, "root");
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::None
        );
        assert_eq!(app.host_password_dialog.as_ref().unwrap().selected_field, 0);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        for value in "secret".chars() {
            press(&mut app, KeyCode::Char(value), KeyModifiers::NONE);
        }
        assert_eq!(
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            InputAction::RetryPublicKeyUpload
        );
        let submitted = app.take_pending_host_password().unwrap();
        assert_eq!(submitted.username, "root");
        assert_eq!(submitted.password.as_str(), "secret");
    }

    #[test]
    fn hosts_uses_one_key_configuration_entry_point() {
        let mut app = grouped_fixture();
        app.enter_group();
        assert!(app.selected_host().is_some());
        press(&mut app, KeyCode::Char('U'), KeyModifiers::NONE);
        assert!(!app.credential_manager.open);
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(app.credential_manager.open);
        assert!(app.credential_manager.ssh_key_only);
    }

    #[test]
    fn credential_export_is_age_encrypted_and_round_trips() {
        let plaintext = br#"{"reference":"web-test","password":"secret-value"}"#;
        let password = Zeroizing::new("export-password".to_string());
        let encrypted = App::encrypt_credentials(plaintext, password).unwrap();
        assert!(encrypted.starts_with(b"age-encryption.org/v1"));
        assert!(
            !encrypted
                .windows(b"secret-value".len())
                .any(|window| { window == b"secret-value" })
        );

        let decryptor = age::Decryptor::new(&encrypted[..]).unwrap();
        let identity =
            age::scrypt::Identity::new(SecretString::from("export-password".to_string()));
        let mut reader = decryptor.decrypt(std::iter::once(&identity as _)).unwrap();
        let mut decrypted = Vec::new();
        reader.read_to_end(&mut decrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn credential_import_uses_link_targets_remapped_by_configuration_merge() {
        let fixture = crate::projects::tests::Fixture::new("remapped-credential-import");
        let mut app = app_fixture();
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("lazyterm/hosts.json"));
        let mut project = fixture.project.clone();
        project.id = uuid::Uuid::from_u128(10);
        project.links.push(crate::projects::ProjectLink {
            id: uuid::Uuid::from_u128(20),
            name: "Home".into(),
            url: "http://localhost:3000/".into(),
            password_storage: crate::projects::PasswordStorage::Config,
            ..Default::default()
        });
        let imported_project_id = project.id;
        app.metadata.projects.push(project);
        app.metadata.credential_import_targets.insert(
            "config:1:1".into(),
            format!(
                "config:{}:{}",
                imported_project_id,
                uuid::Uuid::from_u128(20)
            ),
        );
        let count = app
            .apply_credential_archive(
                CredentialImport {
                    format_version: 1,
                    credentials: vec![ImportedCredentialRecord {
                        reference: "config:1:1".into(),
                        password: "imported-test-secret".into(),
                    }],
                },
                Zeroizing::new("archive-password".into()),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            app.metadata.projects[0].links[0].password.as_deref(),
            Some("imported-test-secret")
        );
    }

    #[test]
    fn credential_import_rejects_wrong_password_and_replaces_keychain_with_encrypted_backup() {
        let reference = format!("credential-{}", "a".repeat(64));
        crate::project_credentials::replace_password(&reference, "old-secret").unwrap();
        let payload = CredentialExport {
            format_version: 1,
            credentials: vec![CredentialRecord {
                reference: reference.clone(),
                password: "new-secret".into(),
            }],
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&payload).unwrap());
        let password = Zeroizing::new("archive-password".to_string());
        let encrypted = App::encrypt_credentials(&plaintext, password.clone()).unwrap();
        assert!(
            App::decrypt_credential_archive(&encrypted, Zeroizing::new("wrong-password".into()))
                .is_err()
        );
        let archive = App::decrypt_credential_archive(&encrypted, password.clone()).unwrap();

        let fixture = crate::projects::tests::Fixture::new("credential-import");
        let mut app = app_fixture();
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("lazyterm/hosts.json"));
        let count = app.apply_credential_archive(archive, password).unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            &**crate::project_credentials::read_password(&reference).unwrap(),
            "new-secret"
        );
        let backup_dir = fixture.root.join("lazyterm/import-backups");
        let backup = fs::read_dir(backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "age")
            })
            .unwrap();
        let bytes = fs::read(backup.path()).unwrap();
        assert!(bytes.starts_with(b"age-encryption.org/v1"));
        let archive =
            App::decrypt_credential_archive(&bytes, Zeroizing::new("archive-password".into()))
                .unwrap();
        assert_eq!(archive.credentials.len(), 1);
        assert_eq!(archive.credentials[0].password, "old-secret");
        crate::project_credentials::delete_password(&reference).unwrap();
    }

    #[test]
    fn configuration_export_keeps_current_features_and_excludes_inline_passwords() {
        let fixture = crate::projects::tests::Fixture::new("configuration-export");
        let mut metadata = Metadata::default();
        metadata.host_groups.push("production".into());
        metadata
            .tags
            .insert("web".into(), vec!["production".into()]);
        let credential_id = uuid::Uuid::from_u128(7);
        metadata.host_passwords.insert("web".into(), credential_id);
        metadata.credentials.push(crate::credentials::Credential {
            id: credential_id,
            name: "web password".into(),
            kind: crate::credentials::CredentialKind::Password,
            username: "deploy".into(),
            password_ref: Some("credential-reference".into()),
            private_key_path: None,
            public_key: String::new(),
            fingerprint: String::new(),
        });
        metadata.tunnels.push(crate::metadata::TunnelDefinition {
            id: uuid::Uuid::from_u128(1),
            label: "database".into(),
            host_alias: "web".into(),
            custom_host: false,
            kind: crate::metadata::TunnelKind::Local,
            bind_address: "127.0.0.1".into(),
            local_port: 13306,
            remote_host: Some("db.internal".into()),
            remote_port: Some(3306),
            lifecycle: crate::metadata::TunnelLifecycle::App,
            auto_reconnect: false,
            connect_timeout_secs: 10,
            keepalive_interval_secs: 30,
        });
        metadata
            .snippet_packages
            .push(crate::metadata::SnippetPackage {
                id: uuid::Uuid::from_u128(1),
                name: "Operations".into(),
                description: String::new(),
                snippets: vec![crate::metadata::SnippetDefinition {
                    id: uuid::Uuid::from_u128(1),
                    name: "Health".into(),
                    command: "uptime".into(),
                    description: String::new(),
                    dangerous: false,
                }],
            });
        let mut project = fixture.project.clone();
        project.commands.push(fixture.command("npm run start"));
        project.links.push(crate::projects::ProjectLink {
            id: uuid::Uuid::from_u128(1),
            name: "Admin".into(),
            url: "https://example.invalid/admin".into(),
            username: "admin".into(),
            password: Some("inline-web-secret".into()),
            ..Default::default()
        });
        metadata.projects.push(project);
        let host = Host {
            alias: "web".into(),
            hostname: Some("192.0.2.10".into()),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_file: Some("~/.ssh/id_ed25519".into()),
            proxy_jump: Some("bastion".into()),
            managed: true,
        };
        let include_path = fixture.root.join("work.conf").display().to_string();
        let ssh_config = "Host web\n  HostName 192.0.2.10\n".to_string();
        let include_config = "Host bastion\n  HostName bastion.example.invalid\n".to_string();
        let bytes = configuration_export_bytes(
            &metadata,
            std::slice::from_ref(&host),
            Some(fixture.root.join("config").display().to_string()),
            ssh_config.clone(),
            [(include_path.clone(), include_config.clone())]
                .into_iter()
                .collect(),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let mut expected_metadata = metadata.clone();
        for project in &mut expected_metadata.projects {
            for link in &mut project.links {
                link.password = None;
            }
        }
        assert_eq!(
            json["metadata"],
            serde_json::to_value(expected_metadata).unwrap()
        );
        assert_eq!(json["format_version"], 1);
        assert_eq!(json["passwords_included"], false);
        assert_eq!(json["ssh_config"], ssh_config);
        assert_eq!(json["ssh_config_files"][include_path], include_config);
        assert_eq!(json["hosts"][0]["proxy_jump"], "bastion");
        assert_eq!(json["metadata"]["host_groups"][0], "production");
        assert_eq!(
            json["metadata"]["host_passwords"]["web"],
            credential_id.to_string()
        );
        assert_eq!(
            json["metadata"]["credentials"][0]["password_ref"],
            "credential-reference"
        );
        assert_eq!(
            json["metadata"]["snippet_packages"][0]["snippets"][0]["command"],
            "uptime"
        );
        assert_eq!(
            json["metadata"]["projects"][0]["commands"][0]["command"],
            "npm run start"
        );
        assert_eq!(
            json["metadata"]["projects"][0]["links"][0]["url"],
            "https://example.invalid/admin"
        );
        assert!(json["metadata"]["projects"][0]["links"][0]["password"].is_null());
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("inline-web-secret")
        );
    }

    #[test]
    fn exports_are_written_atomically_with_private_permissions() {
        let fixture = crate::projects::tests::Fixture::new("private-export");
        let path = fixture.root.join("export.age");
        write_private_export(&path, b"first").unwrap();
        write_private_export(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn credential_export_keeps_feedback_inside_dialog() {
        let mut app = app_fixture();
        app.credential_export_dialog = Some(CredentialExportDialog {
            fields: ["short".into(), "short".into()],
            selected_field: 1,
            error: None,
        });
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.credential_export_dialog
                .as_ref()
                .and_then(|dialog| dialog.error.as_deref()),
            Some("Use at least 8 characters.")
        );
        assert_eq!(
            app.credential_export_dialog
                .as_ref()
                .map(|dialog| dialog.selected_field),
            Some(1)
        );

        app.credential_export_dialog = Some(CredentialExportDialog {
            fields: ["long-enough".into(), "different".into()],
            selected_field: 1,
            error: None,
        });
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.credential_export_dialog
                .as_ref()
                .and_then(|dialog| dialog.error.as_deref()),
            Some("The two passwords do not match.")
        );

        app.export_credentials(Zeroizing::new("long-enough".into()));
        assert_eq!(
            app.credential_export_dialog
                .as_ref()
                .and_then(|dialog| dialog.error.as_deref()),
            Some("No saved credentials are available to export.")
        );
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
        press(&mut app, KeyCode::Char('6'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait_for_workspace(&mut app, |app| !app.external_project_runs(key).is_empty());
        let (_, port) = external.fixture.ready().unwrap();
        assert!(app.project_is_running(key));
        assert!(app.external_project_runs(key)[0].ports.contains(&port));
        app.viewport_width = 120;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let text = format!("{}", terminal.backend());
        assert!(text.contains("external running"), "{text}");
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
        press(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
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
        for lifecycle in [
            crate::projects::ProjectLifecycle::App,
            crate::projects::ProjectLifecycle::Persistent,
        ] {
            let fixture = Fixture::new("runtime-flow");
            let mut app = app_fixture();
            app.project_service = fixture.service();
            let mut command = fixture.listener_command(false);
            command.lifecycle = lifecycle;
            let key = (fixture.project.id, command.id);
            let _cleanup = crate::projects::tests::PersistentCleanup::new(&fixture, key);
            let mut project = fixture.project.clone();
            project.commands.push(command);
            app.metadata.projects.push(project);
            press(&mut app, KeyCode::Char('6'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            let deadline = Instant::now() + Duration::from_secs(10);
            while fixture.ready().is_none() {
                app.poll_projects();
                assert!(
                    Instant::now() < deadline,
                    "Project start preflight timed out: notice={:?}, checking={}, generation={}, completed={}, error={:?}",
                    app.notice,
                    app.workspace.monitor.checking,
                    app.workspace.monitor.generation,
                    app.workspace.monitor.completed,
                    app.workspace.monitor.error
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let (pid, port) = fixture.ready().unwrap();
            assert!(app.project_service.active(key));
            if lifecycle == crate::projects::ProjectLifecycle::Persistent {
                let definitions = app.metadata.projects.clone();
                drop(app);
                assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
                app = app_fixture();
                app.metadata.projects = definitions;
                app.project_service = fixture.service();
                app.poll_projects();
                assert!(app.project_service.active(key));
                app.section = NavSection::Projects;
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert!(
                app.workspace.form.is_none(),
                "Live commands must not be edited"
            );
            press(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
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
            tunnel: None,
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
        assert_eq!(app.section, NavSection::Credentials);
        app.handle_key(KeyCode::Tab);
        assert_eq!(app.section, NavSection::Hosts);
        app.handle_key(KeyCode::Down);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_browse_item, 2);
        assert_eq!(app.selected_host().unwrap().alias, "two");
    }

    #[test]
    fn settings_change_theme_and_hide_tabs_without_breaking_navigation() {
        let fixture = crate::projects::tests::Fixture::new("settings-picker-save");
        let mut app = app_fixture();
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.section = NavSection::Settings;
        app.selected_setting = 0;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.settings_picker.is_some());
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.metadata.theme, Theme::Ocean);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.metadata.theme, Theme::Ocean);

        app.selected_setting = 1;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.metadata.language, crate::metadata::Language::Chinese);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.metadata.language, crate::metadata::Language::Chinese);
        let saved = Metadata::read_from(app.metadata_save_path.as_ref().unwrap()).unwrap();
        assert_eq!(saved.theme, Theme::Ocean);
        assert_eq!(saved.language, Language::Chinese);

        app.selected_setting = 2;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.visible_sections().contains(&NavSection::Hosts));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Files);
    }

    #[test]
    fn sync_settings_modal_owns_navigation_and_text_input() {
        let mut app = app_fixture();
        app.section = NavSection::Settings;
        app.selected_setting = 13;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        app.sync_dialog.as_mut().unwrap().remote.clear();
        app.sync_dialog.as_mut().unwrap().branch = "main".into();
        assert_eq!(app.sync_dialog.as_ref().unwrap().branch, "main");

        app.handle_paste("git@example.test:team/config.git\n");
        assert_eq!(
            app.sync_dialog.as_ref().unwrap().remote,
            "git@example.test:team/config.git"
        );
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.sync_dialog.as_ref().unwrap().branch, "maind");
        press(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Settings);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.sync_dialog.is_none());
    }

    #[test]
    fn settings_picker_previews_choices_and_escape_restores_original_value() {
        let mut app = app_fixture();
        app.section = NavSection::Settings;
        app.selected_setting = 0;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.metadata.theme, Theme::Ocean);
        press(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Settings);
        assert!(app.settings_picker.is_some());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.metadata.theme, Theme::Forest);
        assert!(app.settings_picker.is_none());
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
            tunnel: None,
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
            tunnel: None,
        };
        let mut system = own.clone();
        system.pid = Some(456);
        system.kind = RuntimeProcessKind::System;
        system.owned_by_current_user = false;
        app.runtime_entries = vec![system.clone(), own.clone()];
        assert_eq!(app.selected_runtime_entry(), Some(&own));
        press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.runtime_kill_confirmation, Some(own.clone()));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('K'), KeyModifiers::NONE);
        assert_eq!(app.runtime_force_kill_confirmation, Some(own));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.runtime_force_kill_confirmation.is_none());
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
    fn host_proxy_jump_form_supports_existing_host_selection_and_manual_input() {
        let mut app = grouped_fixture();
        app.open_host_form(None);
        assert!(app.host_dialog.as_ref().unwrap().fields[5].is_empty());
        let mut host = app
            .hosts
            .hosts
            .iter()
            .find(|host| host.alias == "web")
            .unwrap()
            .clone();
        host.proxy_jump = Some("existing-jump".into());
        app.open_host_form(Some(host));
        let form = app.host_dialog.as_mut().unwrap();
        assert_eq!(form.fields[5], "existing-jump");
        form.fields[0] = "db".into();
        form.selected_field = 6;
        assert!(!app.proxy_jump_choices().contains(&"web".into()));
        assert!(!app.proxy_jump_choices().contains(&"db".into()));
        app.handle_host_dialog_key(KeyCode::Char(' '));
        assert!(app.host_dialog.as_ref().unwrap().proxy_jump_picker_open);
        app.handle_host_dialog_key(KeyCode::Esc);
        assert_eq!(app.host_dialog.as_ref().unwrap().fields[5], "existing-jump");
        app.handle_host_dialog_key(KeyCode::Char(' '));
        let selected = app.proxy_jump_choices()[0].clone();
        app.handle_host_dialog_key(KeyCode::Enter);
        let form = app.host_dialog.as_ref().unwrap();
        assert!(!form.proxy_jump_picker_open);
        assert_eq!(form.fields[5], selected);
        assert_eq!(form.selected_field, 6);
        app.handle_host_dialog_key(KeyCode::Char(','));
        app.handle_host_dialog_key(KeyCode::Char('x'));
        assert_eq!(
            app.host_dialog.as_ref().unwrap().fields[5],
            format!("{selected},x")
        );
        app.handle_host_dialog_key(KeyCode::Tab);
        assert_eq!(app.host_dialog.as_ref().unwrap().selected_field, 0);
    }

    #[test]
    fn host_add_and_edit_forms_include_group_tags() {
        let mut app = grouped_fixture();
        let fixture = crate::projects::tests::Fixture::new("host-group-picker");
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.active_group = Some(HostGroup::Tag("prod".into()));
        app.open_host_form(None);
        assert_eq!(app.host_dialog.as_ref().unwrap().tags, ["prod"]);
        app.host_dialog = None;

        let host = app
            .hosts
            .hosts
            .iter()
            .find(|host| host.alias == "web")
            .unwrap()
            .clone();
        app.open_host_form(Some(host));
        let form = app.host_dialog.as_ref().unwrap();
        assert_eq!(form.tags, ["prod", "work"]);
        assert_eq!(form.selected_field, 0);
        for _ in 0..7 {
            app.handle_host_dialog_key(KeyCode::Tab);
        }
        assert_eq!(app.host_dialog.as_ref().unwrap().selected_field, 0);
        for _ in 0..2 {
            app.handle_host_dialog_key(KeyCode::Tab);
        }
        app.handle_host_dialog_key(KeyCode::Char(' '));
        assert!(app.host_dialog.as_ref().unwrap().tag_picker_open);
        app.handle_host_dialog_key(KeyCode::Char('d'));
        app.handle_host_dialog_key(KeyCode::Char('o'));
        app.handle_host_dialog_key(KeyCode::Char('g'));
        app.handle_host_dialog_key(KeyCode::Char('f'));
        app.handle_host_dialog_key(KeyCode::Char('o'));
        app.handle_host_dialog_key(KeyCode::Char('o'));
        app.handle_host_dialog_key(KeyCode::Down);
        app.handle_host_dialog_key(KeyCode::Enter);
        assert!(
            app.host_dialog
                .as_ref()
                .unwrap()
                .tags
                .contains(&"dogfoo".into())
        );
        assert!(!app.host_dialog.as_ref().unwrap().tag_picker_open);
        assert!(app.metadata.host_groups.contains(&"dogfoo".into()));
        assert!(app.notice.as_deref().unwrap().contains("save Host"));
    }

    #[test]
    fn creating_a_group_from_an_empty_host_picker_persists_and_returns_to_form() {
        let mut app = app_fixture();
        let fixture = crate::projects::tests::Fixture::new("empty-host-group-picker");
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.open_host_form(None);
        let dialog = app.host_dialog.as_mut().unwrap();
        dialog.tag_picker_open = true;
        dialog.tag_filter = "dogfooding".into();

        app.handle_host_dialog_key(KeyCode::Enter);

        let dialog = app.host_dialog.as_ref().unwrap();
        assert!(!dialog.tag_picker_open);
        assert!(dialog.tags.contains(&"dogfooding".into()));
        assert!(app.metadata.host_groups.contains(&"dogfooding".into()));
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Created group dogfooding")
        );
    }

    #[test]
    fn failed_group_creation_stays_in_picker_and_does_not_select_unsaved_group() {
        let mut app = app_fixture();
        app.open_host_form(None);
        let dialog = app.host_dialog.as_mut().unwrap();
        dialog.tag_picker_open = true;
        dialog.tag_filter = "dogfooding".into();

        app.handle_host_dialog_key(KeyCode::Enter);

        let dialog = app.host_dialog.as_ref().unwrap();
        assert!(dialog.tag_picker_open);
        assert!(dialog.tags.is_empty());
        assert!(!app.metadata.host_groups.contains(&"dogfooding".into()));
        assert!(
            app.notice
                .as_deref()
                .unwrap()
                .contains("Metadata unavailable")
        );
    }

    #[test]
    fn host_groups_can_be_created_renamed_and_deleted_without_deleting_hosts() {
        let mut app = grouped_fixture();
        let fixture = crate::projects::tests::Fixture::new("host-group-crud");
        app.metadata_error = false;
        app.metadata_save_path = Some(fixture.root.join("hosts.json"));
        app.section = NavSection::Hosts;

        app.handle_key(KeyCode::Char('g'));
        for character in "temporary".chars() {
            app.handle_key(KeyCode::Char(character));
        }
        app.handle_key(KeyCode::Enter);
        assert!(app.metadata.host_groups.contains(&"temporary".into()));

        app.selected_browse_item = app
            .visible_groups()
            .iter()
            .position(|group| group == &HostGroup::Tag("prod".into()))
            .unwrap();
        app.handle_key(KeyCode::Char('e'));
        app.host_group_dialog.as_mut().unwrap().name = "production".into();
        app.handle_key(KeyCode::Enter);
        assert!(app.metadata.host_groups.contains(&"production".into()));
        assert!(!app.metadata.host_groups.contains(&"prod".into()));
        assert_eq!(app.metadata.tags["web"], ["production", "work"]);
        assert_eq!(app.metadata.tags["db"], ["production"]);

        app.selected_browse_item = app
            .visible_groups()
            .iter()
            .position(|group| group == &HostGroup::Tag("production".into()))
            .unwrap();
        app.handle_key(KeyCode::Char('d'));
        app.handle_key(KeyCode::Enter);
        assert!(!app.metadata.host_groups.contains(&"production".into()));
        assert_eq!(app.metadata.tags["web"], ["work"]);
        assert!(!app.metadata.tags.contains_key("db"));
        assert!(app.hosts.hosts.iter().any(|host| host.alias == "web"));
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
            for number in 1..=7 {
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
            assert_eq!(app.section, NavSection::Credentials);
            press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
            assert_eq!(app.section, NavSection::Hosts);
            press(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
            assert_eq!(app.section, NavSection::Credentials);
        }
    }

    #[test]
    fn letters_and_function_keys_do_not_switch_tabs() {
        let mut app = app_fixture();
        app.section = NavSection::Hosts;
        for key in ['f', 't', 's', 'p', 'S'] {
            press(&mut app, KeyCode::Char(key), KeyModifiers::NONE);
            assert_eq!(app.section, NavSection::Hosts);
        }
        for number in 1..=7 {
            press(&mut app, KeyCode::F(number), KeyModifiers::NONE);
            assert_eq!(app.section, NavSection::Hosts);
        }
    }

    #[test]
    fn comma_opens_settings_from_projects() {
        let mut app = app_fixture();
        app.section = NavSection::Projects;
        press(&mut app, KeyCode::Char(','), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Settings);
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
    fn search_owns_characters_and_numeric_tab_shortcuts_still_work() {
        let mut app = grouped_fixture();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        for character in ['2', 'f', 't', '`', 'S'] {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(app.search_query, "2ft`S");
        assert_eq!(app.section, NavSection::Hosts);
        press(&mut app, KeyCode::F(2), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Hosts);
        assert_eq!(app.search_query, "2ft`S");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.searching);
        press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(app.section, NavSection::Files);
        assert!(!app.searching);
        press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(app.section, NavSection::Tunnels);
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
            cancelable: false,
            scroll: 0,
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
    fn file_name_dialogs_edit_at_the_cursor_without_navigating_panes() {
        let mut app = app_fixture();
        app.section = NavSection::Files;
        for pane in [FilePane::Local, FilePane::Remote] {
            for rename in [true, false] {
                app.active_file_pane = pane;
                app.file_dialog = Some(if rename {
                    FileDialog::Rename {
                        pane,
                        old_name: "ab.txt".into(),
                        value: "ab.txt".into(),
                        cursor: 6,
                    }
                } else {
                    FileDialog::CreateDir {
                        pane,
                        value: "ab.txt".into(),
                        cursor: 6,
                    }
                });
                for key in [
                    KeyCode::Home,
                    KeyCode::Right,
                    KeyCode::Char('中'),
                    KeyCode::Left,
                    KeyCode::Delete,
                    KeyCode::Char('文'),
                    KeyCode::Backspace,
                    KeyCode::End,
                    KeyCode::Left,
                    KeyCode::Char('!'),
                ] {
                    press(&mut app, key, KeyModifiers::NONE);
                }
                let (value, cursor) = match app.file_dialog.as_ref().unwrap() {
                    FileDialog::Rename { value, cursor, .. }
                    | FileDialog::CreateDir { value, cursor, .. } => (value, cursor),
                    _ => unreachable!(),
                };
                assert_eq!(value, "ab.tx!t");
                assert_eq!(*cursor, 6);
                assert_eq!(app.active_file_pane, pane);
                press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                assert!(app.file_dialog.is_none());
            }
        }
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
    fn file_filters_are_independent_when_switching_panes_and_clearing() {
        let mut app = app_fixture();
        app.section = NavSection::Files;
        app.active_file_pane = FilePane::Local;
        app.local_files.entries = vec![
            LocalEntry {
                name: "notes.txt".into(),
                kind: LocalEntryKind::File,
                size: None,
            },
            LocalEntry {
                name: "other.txt".into(),
                kind: LocalEntryKind::File,
                size: None,
            },
        ];
        app.files = FilesState::Loaded(crate::sftp::SftpListing {
            host_alias: "web".into(),
            path: String::new(),
            entries: ["logs", "README.md"]
                .into_iter()
                .map(|name| crate::sftp::SftpEntry {
                    name: name.into(),
                    kind: crate::sftp::SftpEntryKind::File,
                    ..Default::default()
                })
                .collect(),
        });
        app.selected_file = 1;
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.file_filter_visible(FilePane::Local));
        assert!(!app.file_filter_visible(FilePane::Remote));
        for character in "note".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(app.visible_local_entries().len(), 1);
        assert_eq!(app.visible_remote_entries().len(), 2);
        assert_eq!(app.selected_file, 1);

        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.active_file_pane, FilePane::Remote);
        assert!(!app.file_filtering);
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        for character in "log".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(app.local_file_filter, "note");
        assert_eq!(app.remote_file_filter, "log");
        assert_eq!(app.visible_remote_entries().len(), 1);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.remote_file_filter.is_empty());
        assert_eq!(app.local_file_filter, "note");
        assert_eq!(app.visible_local_entries().len(), 1);
        assert_eq!(app.visible_remote_entries().len(), 2);

        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        app.handle_mouse(10, 5, 100, 24);
        assert_eq!(app.active_file_pane, FilePane::Local);
        assert!(!app.file_filtering);
        assert_eq!(app.local_file_filter, "note");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.local_file_filter.is_empty());
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
        assert_eq!(app.local_file_filter, "note");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.local_file_filter.is_empty());
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
        app.sftp_receiver = Some(sftp::PendingListing::from_receiver(receiver));
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
        dialog.selected_field = 0;
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
        dialog.selected_field = 0;
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
        dialog.selected_field = 0;
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
        dialog.selected_field = 0;
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
    fn package_and_snippet_forms_save_from_the_current_field_on_enter() {
        let mut app = app_fixture();
        app.open_package_form(None);
        app.package_dialog.as_mut().unwrap().fields = ["Operations".into(), "Common tasks".into()];
        app.handle_package_dialog_key(KeyCode::Enter);
        assert!(app.package_dialog.is_none());
        assert_eq!(app.metadata.snippet_packages[0].name, "Operations");

        let package_id = app.metadata.snippet_packages[0].id;
        app.open_snippet_form(package_id, None);
        app.snippet_dialog.as_mut().unwrap().fields = [
            "Health check".into(),
            "uptime".into(),
            "Show system uptime".into(),
            "false".into(),
        ];
        app.handle_snippet_dialog_key(KeyCode::Enter);
        assert!(app.snippet_dialog.is_none());
        assert_eq!(
            app.metadata.snippet_packages[0].snippets[0].name,
            "Health check"
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
            id: uuid::Uuid::from_u128(1),
            name: "Ops".into(),
            description: String::new(),
            snippets: vec![
                SnippetDefinition {
                    id: uuid::Uuid::from_u128(1),
                    name: "status".into(),
                    command: "uptime".into(),
                    description: String::new(),
                    dangerous: false,
                },
                SnippetDefinition {
                    id: uuid::Uuid::from_u128(2),
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
            id: uuid::Uuid::from_u128(1),
            name: "Ops".into(),
            description: String::new(),
            snippets: vec![SnippetDefinition {
                id: uuid::Uuid::from_u128(1),
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
