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
use openssh::{KnownHosts, SessionBuilder};
use openssh_sftp_client::{Sftp, file::File, metadata::FileType};
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

type ConnectionCache = Arc<Mutex<HashMap<String, Arc<Mutex<Sftp>>>>>;

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
    let cache = Arc::new(Mutex::new(HashMap::new()));
    while let Some(command) = receiver.recv().await {
        match command {
            SftpCommand::List {
                host_alias,
                path,
                sender,
            } => {
                let cache = Arc::clone(&cache);
                tokio::spawn(async move {
                    let result = list_directory(&host_alias, &path, &cache).await;
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

pub fn spawn_list(host_alias: String, path: String) -> Receiver<SftpResult> {
    let (sender, receiver) = mpsc::channel();
    let _ = service_sender().send(SftpCommand::List {
        host_alias,
        path,
        sender,
    });
    receiver
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
    let connection = cached_sftp(&request.host_alias, cache).await?;
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

async fn list_directory(host_alias: &str, path: &str, cache: &ConnectionCache) -> SftpResult {
    tokio::time::timeout(
        LISTING_TIMEOUT,
        list_directory_inner(host_alias, path, cache),
    )
    .await
    .map_err(|_| format!("Timed out while loading {path} from {host_alias}."))?
}

async fn list_directory_inner(host_alias: &str, path: &str, cache: &ConnectionCache) -> SftpResult {
    let connection = cached_sftp(host_alias, cache).await?;
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

async fn connect_sftp(host_alias: &str) -> Result<Sftp, String> {
    let mut builder = SessionBuilder::default();
    builder
        .known_hosts_check(KnownHosts::Strict)
        .connect_timeout(CONNECT_TIMEOUT);
    let session = builder
        .connect(host_alias)
        .await
        .map_err(|error| format!("SSH connection failed: {error}"))?;
    Sftp::from_session(session, Default::default())
        .await
        .map_err(|error| format!("SFTP subsystem failed: {error}"))
}

async fn cached_sftp(
    host_alias: &str,
    cache: &ConnectionCache,
) -> Result<Arc<Mutex<Sftp>>, String> {
    if let Some(connection) = cache.lock().await.get(host_alias).cloned() {
        return Ok(connection);
    }
    let sftp = Arc::new(Mutex::new(connect_sftp(host_alias).await?));
    let mut connections = cache.lock().await;
    Ok(connections
        .entry(host_alias.to_string())
        .or_insert_with(|| Arc::clone(&sftp))
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
    let connection = cached_sftp(&request.host_alias, cache).await?;
    let sftp = connection.lock().await;
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
}
