use std::{
    path::Path,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use futures_util::StreamExt;
use openssh::{KnownHosts, SessionBuilder};
use openssh_sftp_client::{Sftp, file::File, metadata::FileType};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LISTING_TIMEOUT: Duration = Duration::from_secs(25);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const TRANSFER_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpEntry {
    pub name: String,
    pub kind: SftpEntryKind,
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpEntryKind {
    Directory,
    File,
    Symlink,
    Other,
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
    Failed {
        task_id: u64,
        message: String,
    },
}

pub fn spawn_list(host_alias: String, path: String) -> Receiver<SftpResult> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Could not start SFTP runtime: {error}"))
            .and_then(|runtime| runtime.block_on(list_directory(&host_alias, &path)));
        let _ = sender.send(result);
    });
    receiver
}

pub fn spawn_transfer(request: TransferRequest, sender: Sender<TransferEvent>) {
    thread::spawn(move || {
        let task_id = request.task_id;
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Could not start SFTP runtime: {error}"))
            .and_then(|runtime| runtime.block_on(transfer(&request, &sender)));
        match result {
            Ok(()) => {
                let _ = sender.send(TransferEvent::Finished { task_id });
            }
            Err(message) => {
                let _ = sender.send(TransferEvent::Failed { task_id, message });
            }
        }
    });
}

async fn list_directory(host_alias: &str, path: &str) -> SftpResult {
    tokio::time::timeout(LISTING_TIMEOUT, list_directory_inner(host_alias, path))
        .await
        .map_err(|_| format!("Timed out while loading {path} from {host_alias}."))?
}

async fn list_directory_inner(host_alias: &str, path: &str) -> SftpResult {
    let sftp = connect_sftp(host_alias).await?;
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
            });
        }
    }
    entries.sort_by_key(|entry| {
        (
            entry.kind != SftpEntryKind::Directory,
            entry.name.to_lowercase(),
        )
    });
    let listing = SftpListing {
        host_alias: host_alias.to_string(),
        path: path.to_string(),
        entries,
    };
    sftp.close()
        .await
        .map_err(|error| format!("SFTP session close failed: {error}"))?;
    Ok(listing)
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

async fn transfer(request: &TransferRequest, sender: &Sender<TransferEvent>) -> Result<(), String> {
    tokio::time::timeout(TRANSFER_TIMEOUT, transfer_inner(request, sender))
        .await
        .map_err(|_| format!("Transfer timed out for {}.", request.local_path.display()))?
}

async fn transfer_inner(
    request: &TransferRequest,
    sender: &Sender<TransferEvent>,
) -> Result<(), String> {
    let sftp = connect_sftp(&request.host_alias).await?;
    let result = match request.direction {
        TransferDirection::Upload => upload_file(request, &sftp, sender).await,
        TransferDirection::Download => download_file(request, &sftp, sender).await,
    };
    let close_result = sftp
        .close()
        .await
        .map_err(|error| format!("SFTP session close failed: {error}"));
    result.and(close_result)
}

async fn upload_file(
    request: &TransferRequest,
    sftp: &Sftp,
    sender: &Sender<TransferEvent>,
) -> Result<(), String> {
    let metadata = fs::metadata(&request.local_path).await.map_err(|error| {
        format!(
            "Could not inspect {}: {error}",
            request.local_path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "Only files can be uploaded right now: {}",
            request.local_path.display()
        ));
    }
    let total = metadata.len();
    let mut local = fs::File::open(&request.local_path)
        .await
        .map_err(|error| format!("Could not open {}: {error}", request.local_path.display()))?;
    let mut remote = sftp
        .create(Path::new(&request.remote_path))
        .await
        .map_err(|error| format!("Could not create {}: {error}", request.remote_path))?;
    copy_local_to_remote(&mut local, &mut remote, request.task_id, total, sender).await?;
    remote
        .close()
        .await
        .map_err(|error| format!("Could not close {}: {error}", request.remote_path))?;
    Ok(())
}

async fn download_file(
    request: &TransferRequest,
    sftp: &Sftp,
    sender: &Sender<TransferEvent>,
) -> Result<(), String> {
    let mut remote = sftp
        .open(Path::new(&request.remote_path))
        .await
        .map_err(|error| format!("Could not open {}: {error}", request.remote_path))?;
    let total = remote
        .metadata()
        .await
        .map_err(|error| format!("Could not inspect {}: {error}", request.remote_path))?
        .len()
        .unwrap_or_default();
    let mut local = fs::File::create(&request.local_path)
        .await
        .map_err(|error| format!("Could not create {}: {error}", request.local_path.display()))?;
    copy_remote_to_local(&mut remote, &mut local, request.task_id, total, sender).await?;
    remote
        .close()
        .await
        .map_err(|error| format!("Could not close {}: {error}", request.remote_path))?;
    Ok(())
}

async fn copy_local_to_remote(
    local: &mut fs::File,
    remote: &mut File,
    task_id: u64,
    total: u64,
    sender: &Sender<TransferEvent>,
) -> Result<(), String> {
    let mut buffer = vec![0_u8; TRANSFER_BUFFER_SIZE];
    let mut transferred = 0_u64;
    let _ = sender.send(TransferEvent::Progress {
        task_id,
        transferred,
        total,
    });
    loop {
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
        transferred = transferred.saturating_add(read as u64);
        let _ = sender.send(TransferEvent::Progress {
            task_id,
            transferred,
            total,
        });
    }
    Ok(())
}

async fn copy_remote_to_local(
    remote: &mut File,
    local: &mut fs::File,
    task_id: u64,
    total: u64,
    sender: &Sender<TransferEvent>,
) -> Result<(), String> {
    let mut transferred = 0_u64;
    let _ = sender.send(TransferEvent::Progress {
        task_id,
        transferred,
        total,
    });
    loop {
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
        transferred = transferred.saturating_add(buffer.len() as u64);
        let _ = sender.send(TransferEvent::Progress {
            task_id,
            transferred,
            total,
        });
    }
    local
        .flush()
        .await
        .map_err(|error| format!("Could not flush local file: {error}"))?;
    Ok(())
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
