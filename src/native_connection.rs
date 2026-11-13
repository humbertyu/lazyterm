use std::{
    fs,
    io::{self, Read},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

use crate::credentials::HostAuth;

#[derive(Clone, Copy)]
pub enum Kind {
    Shell,
    Sftp,
}

pub struct Pending {
    pub alias: String,
    pub cancelled: Arc<AtomicBool>,
    pub receiver: Receiver<io::Result<Connection>>,
}

pub struct Connection {
    pub alias: String,
    pub kind: Kind,
    username: Option<String>,
    directory: PathBuf,
    child: crate::process_guard::ManagedChild,
    #[cfg(test)]
    config: Option<PathBuf>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(self.directory.join("ssh"));
        let _ = fs::remove_dir(&self.directory);
    }
}

impl Pending {
    pub fn start(alias: String, kind: Kind, auth: HostAuth) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();
        let target = alias.clone();
        std::thread::Builder::new()
            .name("lazyterm-connect".into())
            .spawn(move || {
                let _ = sender.send(connect(target, kind, auth, cancellation));
            })?;
        Ok(Self {
            alias,
            cancelled,
            receiver,
        })
    }
}

impl Connection {
    #[cfg(test)]
    pub fn control_path(&self) -> PathBuf {
        self.directory.join("ssh")
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(match self.kind {
            Kind::Shell => "ssh",
            Kind::Sftp => "sftp",
        });
        #[cfg(test)]
        if let Some(config) = &self.config {
            command.arg("-F").arg(config);
        }
        command
            .arg("-o")
            .arg(format!(
                "ControlPath={}",
                self.directory.join("ssh").display()
            ))
            .args(["-o", "ControlMaster=no", "-o", "BatchMode=yes"]);
        if let Some(user) = &self.username {
            match self.kind {
                Kind::Shell => {
                    command.args(["-l", user]);
                }
                Kind::Sftp => {
                    command.arg("-o").arg(format!("User={user}"));
                }
            }
        }
        if matches!(self.kind, Kind::Shell) {
            command.arg("-t");
        }
        command.arg(&self.alias);
        command
    }
}

fn connect(
    alias: String,
    kind: Kind,
    auth: HostAuth,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Connection> {
    let directory = PathBuf::from("/tmp").join(format!("ltn-{}", uuid::Uuid::new_v4().simple()));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut command = Command::new("ssh");
    command
        .args(["-M", "-N", "-T", "-S"])
        .arg(directory.join("ssh"))
        .args([
            "-o",
            "ControlPersist=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]);
    let trace = match crate::credentials::configure_host_auth(&mut command, &auth, &alias) {
        Ok(trace) => trace,
        Err(error) => {
            let _ = fs::remove_dir(&directory);
            return Err(error);
        }
    };
    let authentication = trace.as_ref().map(crate::authentication::Trace::status);
    command
        .arg(&alias)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let spawned = crate::process_guard::ManagedChild::spawn(&mut command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir(&directory);
            return Err(error);
        }
    };
    let stderr = child.stderr.take().expect("piped SSH stderr");
    let errors = Arc::new(Mutex::new(String::new()));
    let output = errors.clone();
    let reader = std::thread::spawn(move || {
        let mut reader = crate::authentication::Reader::new(stderr, trace);
        let mut buffer = [0_u8; 4096];
        while let Ok(size) = reader.read(&mut buffer) {
            if size == 0 {
                break;
            }
            let mut errors = output.lock().unwrap();
            if errors.len() < 16 * 1024 {
                errors.push_str(&crate::diagnostics::redact_sensitive_text(
                    &String::from_utf8_lossy(&buffer[..size]),
                ));
            }
        }
    });
    let mut connection = Connection {
        alias,
        kind,
        username: auth.username,
        directory,
        child,
        #[cfg(test)]
        config: auth.config,
    };
    let deadline = Instant::now() + Duration::from_secs(210);
    loop {
        if cancelled.load(Ordering::Acquire)
            || authentication.as_ref().is_some_and(|auth| auth.cancelled())
            || Instant::now() >= deadline
        {
            let _ = connection.child.kill();
            let _ = connection.child.wait();
            let _ = reader.join();
            return Err(io::Error::other("SSH connection cancelled or timed out."));
        }
        if connection.child.try_wait()?.is_some() {
            let _ = reader.join();
            return Err(io::Error::other({
                let detail = errors.lock().unwrap();
                if detail.trim().is_empty() {
                    "SSH connection failed.".to_string()
                } else {
                    detail.trim().to_string()
                }
            }));
        }
        if connection.directory.join("ssh").exists()
            && authentication
                .as_ref()
                .is_none_or(|auth| auth.authenticated())
        {
            return Ok(connection);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}
