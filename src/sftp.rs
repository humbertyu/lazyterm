use std::{
    collections::HashMap,
    path::Path,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use async_recursion::async_recursion;
use futures_util::StreamExt;
use openssh_sftp_client::{Sftp, SftpAuxiliaryData, file::File, metadata::FileType};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, mpsc as tokio_mpsc},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LISTING_TIMEOUT: Duration = Duration::from_secs(25);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(60);
const TRANSFER_BUFFER_SIZE: usize = 64 * 1024;

#[cfg(test)]
static TEST_CONNECTION_CONFIG: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SftpEntry {
    pub name: String,
    pub kind: SftpEntryKind,
    pub size: Option<u64>,
    pub modified: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub permissions: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SftpEntryKind {
    Directory,
    File,
    Symlink,
    Other,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpListing {
    pub host_alias: String,
    pub path: String,
    pub entries: Vec<SftpEntry>,
}

pub type SftpResult = Result<SftpListing, String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferRequest {
    pub task_id: u64,
    pub host_alias: String,
    pub local_path: PathBuf,
    pub remote_path: String,
    pub direction: TransferDirection,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferEvent {
    Progress {
        task_id: u64,
        transferred: u64,
        total: u64,
    },
    Finished {
        task_id: u64,
    },
    Cancelled {
        task_id: u64,
    },
    Failed {
        task_id: u64,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mutation {
    Rename { from: String, to: String },
    Delete { path: String, is_dir: bool },
    CreateDir { path: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationRequest {
    pub operation_id: u64,
    pub host_alias: String,
    pub mutation: Mutation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationEvent {
    Finished { operation_id: u64 },
    Failed { operation_id: u64, message: String },
}

enum SftpCommand {
    List {
        host_alias: String,
        path: String,
        auth: crate::credentials::HostAuth,
        cancelled: Arc<AtomicBool>,
        sender: Sender<SftpResult>,
    },
    Transfer {
        request: TransferRequest,
        sender: Sender<TransferEvent>,
        cancelled: Arc<AtomicBool>,
    },
    Mutation {
        request: MutationRequest,
        sender: Sender<MutationEvent>,
    },
}

#[derive(Default)]
struct Connections {
    sessions: HashMap<String, (Arc<Mutex<Sftp>>, Arc<AtomicBool>)>,
    auth: HashMap<String, crate::credentials::HostAuth>,
    connecting: HashMap<String, Arc<Mutex<()>>>,
}

type ConnectionCache = Arc<Mutex<Connections>>;

fn service_sender() -> &'static tokio_mpsc::UnboundedSender<SftpCommand> {
    static SERVICE: OnceLock<tokio_mpsc::UnboundedSender<SftpCommand>> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (sender, receiver) = tokio_mpsc::unbounded_channel();
        thread::Builder::new()
            .name("lazyterm-sftp".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Could not start SFTP service runtime.");
                runtime.block_on(sftp_service(receiver));
            })
            .expect("Could not start SFTP service thread.");
        sender
    })
}

async fn sftp_service(mut receiver: tokio_mpsc::UnboundedReceiver<SftpCommand>) {
    let cache = Arc::new(Mutex::new(Connections::default()));
    while let Some(command) = receiver.recv().await {
        match command {
            SftpCommand::List {
                host_alias,
                path,
                auth,
                cancelled,
                sender,
            } => {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move {
                    let result = tokio::select! {
                        result = list_directory(&host_alias, &path, &cache, auth) => result,
                        _ = wait_cancelled(&cancelled) => Err("SFTP listing cancelled.".into()),
                    };
                    let _ = sender.send(result);
                });
            }
            SftpCommand::Transfer {
                request,
                sender,
                cancelled,
            } => {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move {
                    let task_id = request.task_id;
                    let result = transfer(&request, &sender, &cancelled, &cache).await;
                    let event = match result {
                        Ok(()) => TransferEvent::Finished { task_id },
                        Err(message) if message == "TRANSFER_CANCELLED" => {
                            TransferEvent::Cancelled { task_id }
                        }
                        Err(message) => TransferEvent::Failed { task_id, message },
                    };
                    let _ = sender.send(event);
                });
            }
            SftpCommand::Mutation { request, sender } => {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move {
                    let operation_id = request.operation_id;
                    let event = match mutate(&request, &cache).await {
                        Ok(()) => MutationEvent::Finished { operation_id },
                        Err(message) => MutationEvent::Failed {
                            operation_id,
                            message,
                        },
                    };
                    let _ = sender.send(event);
                });
            }
        }
    }
}

#[cfg(test)]
pub fn spawn_list(host_alias: String, path: String) -> PendingListing {
    spawn_list_with_auth(host_alias, path, Default::default())
}

pub struct PendingListing {
    receiver: Receiver<SftpResult>,
    cancelled: Arc<AtomicBool>,
}

impl PendingListing {
    pub fn try_recv(&self) -> Result<SftpResult, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    #[cfg(test)]
    pub fn from_receiver(receiver: Receiver<SftpResult>) -> Self {
        Self {
            receiver,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub fn recv_timeout(&self, timeout: Duration) -> Result<SftpResult, mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

impl Drop for PendingListing {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

async fn wait_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn spawn_list_with_auth(
    host_alias: String,
    path: String,
    auth: crate::credentials::HostAuth,
) -> PendingListing {
    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let _ = service_sender().send(SftpCommand::List {
        host_alias,
        path,
        auth,
        cancelled: cancelled.clone(),
        sender,
    });
    PendingListing {
        receiver,
        cancelled,
    }
}

pub fn spawn_transfer(
    request: TransferRequest,
    sender: Sender<TransferEvent>,
    cancelled: Arc<AtomicBool>,
) {
    let _ = service_sender().send(SftpCommand::Transfer {
        request,
        sender,
        cancelled,
    });
}

pub fn spawn_mutation(request: MutationRequest, sender: Sender<MutationEvent>) {
    let _ = service_sender().send(SftpCommand::Mutation { request, sender });
}

async fn mutate(request: &MutationRequest, cache: &ConnectionCache) -> Result<(), String> {
    cached_sftp(&request.host_alias, cache, None).await?;
    tokio::time::timeout(MUTATION_TIMEOUT, mutate_inner(request, cache))
        .await
        .map_err(|_| {
            format!(
                "Remote file operation timed out for {}.",
                request.host_alias
            )
        })?
}

async fn mutate_inner(request: &MutationRequest, cache: &ConnectionCache) -> Result<(), String> {
    let connection = cached_sftp(&request.host_alias, cache, None).await?;
    let sftp = connection.lock().await;
    let mut fs = sftp.fs();
    match &request.mutation {
        Mutation::Rename { from, to } => fs
            .rename(Path::new(from), Path::new(to))
            .await
            .map_err(|error| format!("Could not rename {from}: {error}")),
        Mutation::Delete { path, is_dir } => {
            if *is_dir {
                fs.remove_dir(Path::new(path))
                    .await
                    .map_err(|error| format!("Could not remove directory {path}: {error}"))
            } else {
                fs.remove_file(Path::new(path))
                    .await
                    .map_err(|error| format!("Could not remove file {path}: {error}"))
            }
        }
        Mutation::CreateDir { path } => fs
            .create_dir(Path::new(path))
            .await
            .map_err(|error| format!("Could not create directory {path}: {error}")),
    }
}

async fn list_directory(
    host_alias: &str,
    path: &str,
    cache: &ConnectionCache,
    auth: crate::credentials::HostAuth,
) -> SftpResult {
    let connection = cached_sftp(host_alias, cache, Some(auth)).await?;
    tokio::time::timeout(
        LISTING_TIMEOUT,
        list_directory_inner(host_alias, path, connection),
    )
    .await
    .map_err(|_| format!("Timed out while loading {path} from {host_alias}."))?
}

async fn list_directory_inner(
    host_alias: &str,
    path: &str,
    connection: Arc<Mutex<Sftp>>,
) -> SftpResult {
    let sftp = connection.lock().await;
    let directory = sftp
        .fs()
        .open_dir(if path.is_empty() {
            Path::new(".")
        } else {
            Path::new(path)
        })
        .await
        .map_err(|error| format!("Could not read {path}: {error}"))?;
    let mut entries = Vec::new();
    {
        let read_dir = directory.read_dir();
        futures_util::pin_mut!(read_dir);

        while let Some(entry) = read_dir.next().await {
            let entry = entry.map_err(|error| format!("Could not read {path}: {error}"))?;
            let name = entry.filename().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let metadata = entry.metadata();
            let kind = metadata
                .file_type()
                .map_or(SftpEntryKind::Unknown, entry_kind);
            entries.push(SftpEntry {
                name,
                kind,
                size: metadata.len(),
                modified: metadata.modified().map(|value| value.into_raw() as u64),
                uid: metadata.uid(),
                gid: metadata.gid(),
                permissions: metadata.permissions().map(permission_mode),
            });
        }
    }
    entries.sort_by_key(|entry| {
        (
            entry.kind != SftpEntryKind::Directory,
            entry.name.to_lowercase(),
        )
    });
    Ok(SftpListing {
        host_alias: host_alias.to_string(),
        path: path.to_string(),
        entries,
    })
}

async fn connect_sftp(
    host_alias: &str,
    auth: &crate::credentials::HostAuth,
) -> Result<(Sftp, Arc<AtomicBool>), String> {
    use std::process::{Command, Stdio};
    use tokio::io::AsyncBufReadExt;
    let mut command = Command::new("ssh");
    command.args([
        "-T",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        &format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()),
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]);
    #[cfg(test)]
    if let Some((config_file, known_hosts_file)) = TEST_CONNECTION_CONFIG.get() {
        command
            .arg("-F")
            .arg(config_file)
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", known_hosts_file.display()));
    }
    let trace = crate::credentials::configure_host_auth(&mut command, auth, host_alias)
        .map_err(|error| format!("Could not prepare SFTP authentication: {error}"))?;
    command
        .arg("-s")
        .arg(host_alias)
        .arg("sftp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start SFTP transport: {error}"))?;
    let stdin = child.stdin.take().expect("piped SFTP stdin");
    let stdout = child.stdout.take().expect("piped SFTP stdout");
    let stderr = child.stderr.take().expect("piped SFTP stderr");
    let errors = Arc::new(std::sync::Mutex::new(String::new()));
    let output = errors.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let transport_alive = alive.clone();
    let stderr_task = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if trace.as_ref().is_some_and(|trace| trace.observe(&line)) {
                continue;
            }
            let mut errors = output.lock().unwrap();
            if errors.len() < 16 * 1024 {
                errors.push_str(&crate::diagnostics::redact_sensitive_text(&line));
                errors.push('\n');
            }
        }
        transport_alive.store(false, Ordering::Release);
    });
    struct Transport {
        child: std::sync::Mutex<tokio::process::Child>,
        stderr: tokio::task::JoinHandle<()>,
    }
    impl Drop for Transport {
        fn drop(&mut self) {
            let _ = self.child.get_mut().unwrap().start_kill();
            self.stderr.abort();
        }
    }
    struct PendingTransport(Arc<Transport>, bool);
    impl Drop for PendingTransport {
        fn drop(&mut self) {
            if self.1 {
                let _ = self.0.child.lock().unwrap().start_kill();
                self.0.stderr.abort();
            }
        }
    }
    let transport = Arc::new(Transport {
        child: std::sync::Mutex::new(child),
        stderr: stderr_task,
    });
    // SFTP's internal tasks may outlive a cancelled handshake future.
    let mut pending_transport = PendingTransport(transport.clone(), true);
    let sftp = tokio::time::timeout(
        Duration::from_secs(210),
        Sftp::new_with_auxiliary(
            stdin,
            stdout,
            Default::default(),
            SftpAuxiliaryData::Boxed(Box::new(transport)),
        ),
    )
    .await
    .map_err(|_| "SFTP authentication timed out.".to_string())?
    .map_err(|error| {
        let detail = errors.lock().unwrap();
        if detail.trim().is_empty() {
            format!("SFTP connection failed: {error}")
        } else {
            format!("SFTP connection failed: {}", detail.trim())
        }
    })?;
    pending_transport.1 = false;
    Ok((sftp, alive))
}

async fn cached_sftp(
    host_alias: &str,
    cache: &ConnectionCache,
    requested_auth: Option<crate::credentials::HostAuth>,
) -> Result<Arc<Mutex<Sftp>>, String> {
    let connecting = cache
        .lock()
        .await
        .connecting
        .entry(host_alias.to_string())
        .or_default()
        .clone();
    let _connecting = connecting.lock().await;
    if let Some(auth) = requested_auth {
        let mut connections = cache.lock().await;
        if connections.auth.get(host_alias) != Some(&auth) {
            connections.sessions.remove(host_alias);
            connections.auth.insert(host_alias.to_string(), auth);
        }
    }
    if let Some((connection, alive)) = cache.lock().await.sessions.get(host_alias).cloned()
        && alive.load(Ordering::Acquire)
    {
        return Ok(connection);
    }
    cache.lock().await.sessions.remove(host_alias);
    let auth = cache
        .lock()
        .await
        .auth
        .get(host_alias)
        .cloned()
        .unwrap_or_default();
    let (sftp, alive) = connect_sftp(host_alias, &auth).await?;
    let sftp = Arc::new(Mutex::new(sftp));
    let mut connections = cache.lock().await;
    Ok(connections
        .sessions
        .entry(host_alias.to_string())
        .or_insert_with(|| (Arc::clone(&sftp), alive))
        .0
        .clone())
}

async fn transfer(
    request: &TransferRequest,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    cache: &ConnectionCache,
) -> Result<(), String> {
    tokio::time::timeout(
        TRANSFER_TIMEOUT,
        transfer_inner(request, sender, cancelled, cache),
    )
    .await
    .map_err(|_| format!("Transfer timed out for {}.", request.local_path.display()))?
}

async fn transfer_inner(
    request: &TransferRequest,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    cache: &ConnectionCache,
) -> Result<(), String> {
    check_cancelled(cancelled)?;
    let connection = tokio::select! {
        connection = cached_sftp(&request.host_alias, cache, None) => connection?,
        _ = wait_cancelled(cancelled) => return Err("TRANSFER_CANCELLED".into()),
    };
    let sftp = tokio::select! {
        sftp = connection.lock() => sftp,
        _ = wait_cancelled(cancelled) => return Err("TRANSFER_CANCELLED".into()),
    };
    let total = match request.direction {
        TransferDirection::Upload => local_size(&request.local_path).await?,
        TransferDirection::Download => {
            remote_size(&sftp, Path::new(&request.remote_path), request.is_dir).await?
        }
    };
    let _ = sender.send(TransferEvent::Progress {
        task_id: request.task_id,
        transferred: 0,
        total,
    });
    let mut transferred = 0;
    let mut created_directories = Vec::new();
    let result = match request.direction {
        TransferDirection::Upload => {
            upload_path(
                request,
                &sftp,
                sender,
                cancelled,
                &mut transferred,
                total,
                &mut created_directories,
            )
            .await
        }
        TransferDirection::Download => {
            download_path(
                request,
                &sftp,
                sender,
                cancelled,
                &mut transferred,
                total,
                &mut created_directories,
            )
            .await
        }
    };
    if result.is_err() {
        cleanup_created_directories(request.direction, &sftp, &created_directories).await;
    }
    result
}

async fn upload_path(
    request: &TransferRequest,
    sftp: &Sftp,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if request.is_dir {
        upload_directory(
            &request.local_path,
            Path::new(&request.remote_path),
            sftp,
            request.task_id,
            sender,
            cancelled,
            transferred,
            total,
            created_directories,
        )
        .await
    } else {
        upload_single_file(
            &request.local_path,
            Path::new(&request.remote_path),
            sftp,
            request.task_id,
            sender,
            cancelled,
            transferred,
            total,
        )
        .await
    }
}

#[async_recursion]
#[allow(clippy::too_many_arguments)]
async fn upload_directory(
    local_path: &Path,
    remote_path: &Path,
    sftp: &Sftp,
    task_id: u64,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut remote_fs = sftp.fs();
    if remote_fs.create_dir(remote_path).await.is_ok() {
        created_directories.push(remote_path.to_path_buf());
    } else {
        remote_fs
            .open_dir(remote_path)
            .await
            .map_err(|error| format!("Could not create directory {remote_path:?}: {error}"))?;
    }
    drop(remote_fs);

    let mut entries = fs::read_dir(local_path)
        .await
        .map_err(|error| format!("Could not read {}: {error}", local_path.display()))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Could not read {}: {error}", local_path.display()))?
    {
        check_cancelled(cancelled)?;
        let metadata = entry
            .metadata()
            .await
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        let child_local = entry.path();
        let child_remote = remote_path.join(entry.file_name());
        if metadata.is_dir() {
            upload_directory(
                &child_local,
                &child_remote,
                sftp,
                task_id,
                sender,
                cancelled,
                transferred,
                total,
                created_directories,
            )
            .await?;
        } else if metadata.is_file() {
            upload_single_file(
                &child_local,
                &child_remote,
                sftp,
                task_id,
                sender,
                cancelled,
                transferred,
                total,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upload_single_file(
    local_path: &Path,
    remote_path: &Path,
    sftp: &Sftp,
    task_id: u64,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
) -> Result<(), String> {
    let mut local = fs::File::open(local_path)
        .await
        .map_err(|error| format!("Could not open {}: {error}", local_path.display()))?;
    let temporary_path = temporary_sibling(remote_path, task_id);
    let mut remote = sftp
        .create(&temporary_path)
        .await
        .map_err(|error| format!("Could not create {temporary_path:?}: {error}"))?;
    let copy_result = copy_local_to_remote(
        &mut local,
        &mut remote,
        task_id,
        sender,
        cancelled,
        transferred,
        total,
    )
    .await;
    remote
        .close()
        .await
        .map_err(|error| format!("Could not close {temporary_path:?}: {error}"))?;
    if let Err(error) = copy_result {
        let mut remote_fs = sftp.fs();
        let _ = remote_fs.remove_file(&temporary_path).await;
        return Err(error);
    }
    let mut remote_fs = sftp.fs();
    remote_fs
        .rename(&temporary_path, remote_path)
        .await
        .map_err(|error| format!("Could not finalize {remote_path:?}: {error}"))?;
    Ok(())
}

async fn download_path(
    request: &TransferRequest,
    sftp: &Sftp,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if request.is_dir {
        download_directory(
            Path::new(&request.remote_path),
            &request.local_path,
            sftp,
            request.task_id,
            sender,
            cancelled,
            transferred,
            total,
            created_directories,
        )
        .await
    } else {
        download_single_file(
            Path::new(&request.remote_path),
            &request.local_path,
            sftp,
            request.task_id,
            sender,
            cancelled,
            transferred,
            total,
        )
        .await
    }
}

#[async_recursion]
#[allow(clippy::too_many_arguments)]
async fn download_directory(
    remote_path: &Path,
    local_path: &Path,
    sftp: &Sftp,
    task_id: u64,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), String> {
    ensure_local_directory(local_path, created_directories).await?;
    for (name, is_dir) in remote_children(sftp, remote_path).await? {
        check_cancelled(cancelled)?;
        let child_remote = remote_path.join(&name);
        let child_local = local_path.join(&name);
        if is_dir {
            download_directory(
                &child_remote,
                &child_local,
                sftp,
                task_id,
                sender,
                cancelled,
                transferred,
                total,
                created_directories,
            )
            .await?;
        } else {
            download_single_file(
                &child_remote,
                &child_local,
                sftp,
                task_id,
                sender,
                cancelled,
                transferred,
                total,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_single_file(
    remote_path: &Path,
    local_path: &Path,
    sftp: &Sftp,
    task_id: u64,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
) -> Result<(), String> {
    let mut remote = sftp
        .open(remote_path)
        .await
        .map_err(|error| format!("Could not open {remote_path:?}: {error}"))?;
    if let Some(parent) = local_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    let temporary_path = temporary_sibling(local_path, task_id);
    let mut local = fs::File::create(&temporary_path)
        .await
        .map_err(|error| format!("Could not create {}: {error}", temporary_path.display()))?;
    let copy_result = copy_remote_to_local(
        &mut remote,
        &mut local,
        task_id,
        sender,
        cancelled,
        transferred,
        total,
    )
    .await;
    remote
        .close()
        .await
        .map_err(|error| format!("Could not close {remote_path:?}: {error}"))?;
    drop(local);
    if let Err(error) = copy_result {
        let _ = fs::remove_file(&temporary_path).await;
        return Err(error);
    }
    fs::rename(&temporary_path, local_path)
        .await
        .map_err(|error| format!("Could not finalize {}: {error}", local_path.display()))?;
    Ok(())
}

fn temporary_sibling(path: &Path, task_id: u64) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "transfer".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!(".lazyterm-{task_id}-{name}.part"))
}

async fn ensure_local_directory(
    path: &Path,
    created_directories: &mut Vec<PathBuf>,
) -> Result<(), String> {
    match fs::metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} is not a directory.", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .await
                .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
            created_directories.push(path.to_path_buf());
            Ok(())
        }
        Err(error) => Err(format!("Could not inspect {}: {error}", path.display())),
    }
}

async fn cleanup_created_directories(
    direction: TransferDirection,
    sftp: &Sftp,
    created_directories: &[PathBuf],
) {
    for path in created_directories.iter().rev() {
        match direction {
            TransferDirection::Upload => {
                let mut remote_fs = sftp.fs();
                let _ = remote_fs.remove_dir(path).await;
            }
            TransferDirection::Download => {
                let _ = fs::remove_dir(path).await;
            }
        }
    }
}

#[async_recursion]
async fn local_size(path: &Path) -> Result<u64, String> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(format!("Unsupported local file type: {}", path.display()));
    }
    let mut total = 0_u64;
    let mut entries = fs::read_dir(path)
        .await
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?
    {
        total = total.saturating_add(local_size(&entry.path()).await?);
    }
    Ok(total)
}

#[async_recursion]
async fn remote_size(sftp: &Sftp, path: &Path, is_dir: bool) -> Result<u64, String> {
    if !is_dir {
        return Ok(sftp
            .open(path)
            .await
            .map_err(|error| format!("Could not inspect {path:?}: {error}"))?
            .metadata()
            .await
            .map_err(|error| format!("Could not inspect {path:?}: {error}"))?
            .len()
            .unwrap_or_default());
    }
    let mut total = 0_u64;
    for (name, child_is_dir) in remote_children(sftp, path).await? {
        total = total.saturating_add(remote_size(sftp, &path.join(name), child_is_dir).await?);
    }
    Ok(total)
}

async fn remote_children(sftp: &Sftp, path: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut fs = sftp.fs();
    let directory = fs
        .open_dir(path)
        .await
        .map_err(|error| format!("Could not read {path:?}: {error}"))?;
    let mut entries = Vec::new();
    let read_dir = directory.read_dir();
    futures_util::pin_mut!(read_dir);
    while let Some(entry) = read_dir
        .next()
        .await
        .transpose()
        .map_err(|error| format!("Could not read {path:?}: {error}"))?
    {
        let name = entry.filename().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        entries.push((
            name,
            entry
                .metadata()
                .file_type()
                .is_some_and(|file_type| file_type.is_dir()),
        ));
    }
    Ok(entries)
}

async fn copy_local_to_remote(
    local: &mut fs::File,
    remote: &mut File,
    task_id: u64,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
) -> Result<(), String> {
    let mut buffer = vec![0_u8; TRANSFER_BUFFER_SIZE];
    loop {
        check_cancelled(cancelled)?;
        let read = local
            .read(&mut buffer)
            .await
            .map_err(|error| format!("Read failed: {error}"))?;
        if read == 0 {
            break;
        }
        let mut written = 0;
        while written < read {
            let count = remote
                .write(&buffer[written..read])
                .await
                .map_err(|error| format!("Write failed: {error}"))?;
            if count == 0 {
                return Err("Remote file write returned zero bytes.".to_string());
            }
            written += count;
        }
        *transferred = transferred.saturating_add(read as u64);
        let _ = sender.send(TransferEvent::Progress {
            task_id,
            transferred: *transferred,
            total,
        });
    }
    Ok(())
}

async fn copy_remote_to_local(
    remote: &mut File,
    local: &mut fs::File,
    task_id: u64,
    sender: &Sender<TransferEvent>,
    cancelled: &Arc<AtomicBool>,
    transferred: &mut u64,
    total: u64,
) -> Result<(), String> {
    loop {
        check_cancelled(cancelled)?;
        let buffer = remote
            .read(
                TRANSFER_BUFFER_SIZE
                    .try_into()
                    .expect("transfer buffer size fits in u32"),
                bytes::BytesMut::with_capacity(TRANSFER_BUFFER_SIZE),
            )
            .await
            .map_err(|error| format!("Read failed: {error}"))?;
        let Some(buffer) = buffer else {
            break;
        };
        local
            .write_all(&buffer)
            .await
            .map_err(|error| format!("Write failed: {error}"))?;
        *transferred = transferred.saturating_add(buffer.len() as u64);
        let _ = sender.send(TransferEvent::Progress {
            task_id,
            transferred: *transferred,
            total,
        });
    }
    local
        .flush()
        .await
        .map_err(|error| format!("Could not flush local file: {error}"))?;
    Ok(())
}

fn check_cancelled(cancelled: &Arc<AtomicBool>) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("TRANSFER_CANCELLED".to_string())
    } else {
        Ok(())
    }
}

fn entry_kind(file_type: FileType) -> SftpEntryKind {
    if file_type.is_dir() {
        SftpEntryKind::Directory
    } else if file_type.is_file() {
        SftpEntryKind::File
    } else if file_type.is_symlink() {
        SftpEntryKind::Symlink
    } else {
        SftpEntryKind::Other
    }
}

fn permission_mode(permissions: openssh_sftp_client::metadata::Permissions) -> u32 {
    let mut mode = 0;
    if permissions.read_by_owner() {
        mode |= 0o400;
    }
    if permissions.write_by_owner() {
        mode |= 0o200;
    }
    if permissions.execute_by_owner() {
        mode |= 0o100;
    }
    if permissions.read_by_group() {
        mode |= 0o040;
    }
    if permissions.write_by_group() {
        mode |= 0o020;
    }
    if permissions.execute_by_group() {
        mode |= 0o010;
    }
    if permissions.read_by_other() {
        mode |= 0o004;
    }
    if permissions.write_by_other() {
        mode |= 0o002;
    }
    if permissions.execute_by_other() {
        mode |= 0o001;
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        net::TcpStream,
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    struct SshFixture {
        root: PathBuf,
        port: u16,
        child: Child,
    }

    impl Drop for SshFixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn entry_kinds_distinguish_navigable_directories() {
        let entries = [
            SftpEntryKind::Directory,
            SftpEntryKind::File,
            SftpEntryKind::Symlink,
            SftpEntryKind::Other,
            SftpEntryKind::Unknown,
        ];
        assert_eq!(
            entries
                .iter()
                .filter(|kind| **kind == SftpEntryKind::Directory)
                .count(),
            1
        );
    }

    #[test]
    fn sftp_fixture_end_to_end() {
        if std::env::var("LAZYTERM_SFTP_FIXTURE").as_deref() != Ok("1") {
            eprintln!("skipped: set LAZYTERM_SFTP_FIXTURE=1 to run the disposable SSH fixture");
            return;
        }

        let Some(sshd) = command_path("sshd") else {
            eprintln!("skipped: sshd is unavailable");
            return;
        };
        let Some(ssh_keygen) = command_path("ssh-keygen") else {
            eprintln!("skipped: ssh-keygen is unavailable");
            return;
        };
        let Some(ssh_keyscan) = command_path("ssh-keyscan") else {
            eprintln!("skipped: ssh-keyscan is unavailable");
            return;
        };
        let sftp_server = [
            std::env::var_os("LAZYTERM_SFTP_SERVER").map(PathBuf::from),
            Some(PathBuf::from("/usr/libexec/sftp-server")),
            Some(PathBuf::from("/usr/lib/openssh/sftp-server")),
        ]
        .into_iter()
        .flatten()
        .find(|path| path.is_file());
        let Some(sftp_server) = sftp_server else {
            eprintln!("skipped: sftp-server is unavailable");
            return;
        };

        let fixture = create_fixture(&sshd, &ssh_keygen, &ssh_keyscan, &sftp_server);
        let new_key = crate::credentials::generate_key(
            &fixture.root.join("generated-key"),
            uuid::Uuid::from_u128(1),
            "fixture".into(),
        )
        .expect("generate upload key");
        for _ in 0..2 {
            crate::credentials::upload_fixture(
                &new_key.public_key,
                &fixture.root.join("ssh_config"),
                &fixture.root.join("remote-home"),
            )
            .expect("install public key through real SSH");
        }
        let installed =
            fs::read_to_string(fixture.root.join("remote-home/.ssh/authorized_keys")).unwrap();
        assert_eq!(
            installed
                .matches(new_key.public_key.split_whitespace().nth(1).unwrap())
                .count(),
            1
        );
        let new_config = fs::read_to_string(fixture.root.join("ssh_config"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("IdentityFile "))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(fixture.root.join("new_key_config"), new_config).unwrap();
        run(
            Command::new("ssh")
                .arg("-F")
                .arg(fixture.root.join("new_key_config"))
                .arg("-i")
                .arg(new_key.private_key_path.as_ref().unwrap())
                .args(["lazyterm-fixture", "true"]),
            "authenticate using uploaded key",
        );
        let _ = TEST_CONNECTION_CONFIG.set((
            fixture.root.join("ssh_config"),
            fixture.root.join("known_hosts"),
        ));

        let remote_runtime = crate::runtime::inspect_listeners_with_test_config(
            &crate::runtime::RuntimeTarget::Host("lazyterm-fixture".into()),
            &fixture.root.join("ssh_config"),
        )
        .expect("inspect the disposable remote Runtime fixture");
        assert!(
            remote_runtime
                .iter()
                .any(|entry| entry.port == fixture.port),
            "remote Runtime did not report fixture sshd port {}: {remote_runtime:?}",
            fixture.port
        );

        let remote_root = fixture.root.join("remote");
        let local_root = fixture.root.join("local");
        fs::create_dir(&remote_root).expect("create remote fixture directory");
        fs::create_dir(&local_root).expect("create local fixture directory");
        fs::write(local_root.join("upload.txt"), "fixture upload").expect("write upload fixture");

        let listing = spawn_list("lazyterm-fixture".into(), remote_root.display().to_string())
            .recv_timeout(Duration::from_secs(15))
            .expect("receive directory listing")
            .expect("list remote fixture directory");
        assert!(listing.entries.is_empty());

        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        spawn_transfer(
            TransferRequest {
                task_id: 1,
                host_alias: "lazyterm-fixture".into(),
                local_path: local_root.join("upload.txt"),
                remote_path: remote_root.join("uploaded.txt").display().to_string(),
                direction: TransferDirection::Upload,
                is_dir: false,
            },
            event_sender,
            Arc::new(AtomicBool::new(false)),
        );
        assert_transfer_finished(event_receiver);
        assert_eq!(
            fs::read_to_string(remote_root.join("uploaded.txt")).expect("read uploaded file"),
            "fixture upload"
        );

        let download_path = local_root.join("downloaded.txt");
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        spawn_transfer(
            TransferRequest {
                task_id: 2,
                host_alias: "lazyterm-fixture".into(),
                local_path: download_path.clone(),
                remote_path: remote_root.join("uploaded.txt").display().to_string(),
                direction: TransferDirection::Download,
                is_dir: false,
            },
            event_sender,
            Arc::new(AtomicBool::new(false)),
        );
        assert_transfer_finished(event_receiver);
        assert_eq!(
            fs::read_to_string(download_path).expect("read downloaded file"),
            "fixture upload"
        );

        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        spawn_mutation(
            MutationRequest {
                operation_id: 3,
                host_alias: "lazyterm-fixture".into(),
                mutation: Mutation::CreateDir {
                    path: remote_root.join("created").display().to_string(),
                },
            },
            event_sender,
        );
        assert_mutation_finished(event_receiver);

        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        spawn_mutation(
            MutationRequest {
                operation_id: 4,
                host_alias: "lazyterm-fixture".into(),
                mutation: Mutation::Rename {
                    from: remote_root.join("uploaded.txt").display().to_string(),
                    to: remote_root.join("renamed.txt").display().to_string(),
                },
            },
            event_sender,
        );
        assert_mutation_finished(event_receiver);
        assert!(!remote_root.join("uploaded.txt").exists());
        assert!(remote_root.join("renamed.txt").exists());

        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        spawn_mutation(
            MutationRequest {
                operation_id: 5,
                host_alias: "lazyterm-fixture".into(),
                mutation: Mutation::Delete {
                    path: remote_root.join("renamed.txt").display().to_string(),
                    is_dir: false,
                },
            },
            event_sender,
        );
        assert_mutation_finished(event_receiver);
        assert!(!remote_root.join("renamed.txt").exists());

        let mut app = crate::app::App::with_catalog(
            crate::hosts::HostCatalog {
                hosts: vec![crate::hosts::Host {
                    alias: "lazyterm-fixture".into(),
                    hostname: Some("127.0.0.1".into()),
                    user: Some(std::env::var("USER").expect("USER is required").to_string()),
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                    managed: false,
                }],
                error: None,
            },
            Ok(crate::metadata::Metadata::default()),
        );
        app.metadata_save_path = Some(fixture.root.join("app-hosts.json"));
        app.set_host_context(Some("lazyterm-fixture".into()));
        app.handle_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.start_sftp_listing("lazyterm-fixture".into(), remote_root.display().to_string());
        for _ in 0..300 {
            app.poll_sftp();
            if matches!(app.files, crate::app::FilesState::Loaded(_)) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let crate::app::FilesState::Loaded(listing) = &app.files else {
            panic!("App did not load the fixture directory");
        };
        assert_eq!(listing.host_alias, "lazyterm-fixture");
        assert_eq!(
            app.context_host().map(|host| host.alias.as_str()),
            Some("lazyterm-fixture")
        );

        app.handle_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('1'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.section, crate::app::NavSection::Hosts);
        app.handle_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.section, crate::app::NavSection::Files);
        assert_eq!(
            app.context_host().map(|host| host.alias.as_str()),
            Some("lazyterm-fixture")
        );

        drop(fixture);
    }

    fn command_path(command: &str) -> Option<PathBuf> {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {command}"))
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (!path.is_empty()).then(|| PathBuf::from(path))
            })
    }

    fn create_fixture(
        sshd: &Path,
        ssh_keygen: &Path,
        ssh_keyscan: &Path,
        sftp_server: &Path,
    ) -> SshFixture {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lazyterm-sftp-fixture-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create fixture root");

        let client_key = root.join("client_key");
        run(
            Command::new(ssh_keygen)
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(&client_key),
            "generate client key",
        );
        let host_key = root.join("host_key");
        run(
            Command::new(ssh_keygen)
                .args(["-q", "-t", "ed25519", "-N", ""])
                .arg("-f")
                .arg(&host_key),
            "generate host key",
        );
        fs::create_dir_all(root.join("remote-home/.ssh")).unwrap();
        fs::copy(
            client_key.with_extension("pub"),
            root.join("remote-home/.ssh/authorized_keys"),
        )
        .expect("install authorized key");

        let user = std::env::var("USER").expect("USER is required for the SSH fixture");
        let port = free_port();
        let config = format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             HostKey {}\n\
             PidFile {}\n\
             AuthorizedKeysFile {}\n\
             AllowUsers {user}\n\
             PubkeyAuthentication yes\n\
             PasswordAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             StrictModes no\n\
             Subsystem sftp {}\n\
             LogLevel ERROR\n",
            host_key.display(),
            root.join("sshd.pid").display(),
            root.join("remote-home/.ssh/authorized_keys").display(),
            sftp_server.display(),
        );
        fs::write(root.join("sshd_config"), config).expect("write sshd config");

        let ssh_config = format!(
            "Host lazyterm-fixture\n\
             HostName 127.0.0.1\n\
             Port {port}\n\
             User {user}\n\
             IdentityFile {}\n\
             IdentitiesOnly yes\n\
             BatchMode yes\n",
            client_key.display()
        );
        fs::write(root.join("ssh_config"), ssh_config).expect("write client config");

        let child = Command::new(sshd)
            .args(["-D", "-e", "-f"])
            .arg(root.join("sshd_config"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start fixture sshd");
        wait_for_port(port);

        let known_hosts = Command::new(ssh_keyscan)
            .args(["-p", &port.to_string(), "127.0.0.1"])
            .output()
            .expect("scan fixture host key");
        assert!(
            known_hosts.status.success(),
            "ssh-keyscan failed: {}",
            String::from_utf8_lossy(&known_hosts.stderr)
        );
        fs::write(root.join("known_hosts"), known_hosts.stdout).expect("write known_hosts");
        let ssh_config = format!(
            "Host lazyterm-fixture\n\
             HostName 127.0.0.1\n\
             Port {port}\n\
             User {user}\n\
             IdentityFile {}\n\
             IdentitiesOnly yes\n\
             BatchMode yes\n\
             UserKnownHostsFile {}\n\
             StrictHostKeyChecking yes\n",
            client_key.display(),
            root.join("known_hosts").display(),
        );
        fs::write(root.join("ssh_config"), ssh_config).expect("update client config");

        SshFixture { root, port, child }
    }

    fn run(command: &mut Command, description: &str) {
        let output = command.output().unwrap_or_else(|error| {
            panic!("{description} failed to start: {error}");
        });
        assert!(
            output.status.success(),
            "{description} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind free port");
        listener.local_addr().expect("read free port").port()
    }

    fn wait_for_port(port: u16) {
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("fixture sshd did not start on port {port}");
    }

    fn assert_transfer_finished(receiver: Receiver<TransferEvent>) {
        let mut finished = false;
        for _ in 0..10 {
            match receiver
                .recv_timeout(Duration::from_secs(15))
                .expect("receive transfer event")
            {
                TransferEvent::Finished { .. } => {
                    finished = true;
                    break;
                }
                TransferEvent::Failed { message, .. } => panic!("transfer failed: {message}"),
                TransferEvent::Cancelled { .. } => panic!("transfer unexpectedly cancelled"),
                TransferEvent::Progress { .. } => {}
            }
        }
        assert!(finished, "transfer did not finish");
    }

    fn assert_mutation_finished(receiver: Receiver<MutationEvent>) {
        match receiver
            .recv_timeout(Duration::from_secs(15))
            .expect("receive mutation event")
        {
            MutationEvent::Finished { .. } => {}
            MutationEvent::Failed { message, .. } => panic!("mutation failed: {message}"),
        }
    }
}
