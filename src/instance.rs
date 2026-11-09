use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};

pub struct InstanceGuard {
    _file: File,
}

#[derive(Deserialize, Serialize)]
struct Owner {
    pid: u32,
    terminal: Option<String>,
}

impl InstanceGuard {
    pub fn acquire(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(directory.join("tui.instance.lock"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut contents = String::new();
                let _ = Read::by_ref(&mut file)
                    .take(4096)
                    .read_to_string(&mut contents);
                let owner = serde_json::from_str::<Owner>(&contents).ok();
                let identity = owner.map_or_else(String::new, |owner| {
                    format!(
                        " (PID {}{})",
                        owner.pid,
                        owner
                            .terminal
                            .map_or_else(String::new, |terminal| format!(", {terminal}"))
                    )
                });
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "LazyTerm is already running{identity}. Use that instance or close it before starting another."
                    ),
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
        let owner = Owner {
            pid: std::process::id(),
            terminal: terminal_name(),
        };
        let bytes = serde_json::to_vec(&owner)?;
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Keep the inode in place; unlinking it can let a second process lock another inode.
        Ok(Self { _file: file })
    }
}

fn terminal_name() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buffer = [0_i8; 1024];
        if unsafe { libc::ttyname_r(libc::STDIN_FILENO, buffer.as_mut_ptr(), buffer.len()) } == 0 {
            return Some(
                unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    #[test]
    fn duplicate_instance_is_rejected_and_stale_owner_data_does_not_block_restart() {
        let fixture = crate::projects::tests::Fixture::new("single-instance");
        let first = InstanceGuard::acquire(&fixture.root).unwrap();
        let error = InstanceGuard::acquire(&fixture.root).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(error.to_string().contains(&std::process::id().to_string()));
        drop(first);
        assert!(InstanceGuard::acquire(&fixture.root).is_ok());
    }

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn another_process_is_blocked_and_process_death_releases_the_lock() {
        let fixture = crate::projects::tests::Fixture::new("single-instance-process");
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "instance::tests::instance_lock_process",
            ])
            .env("LAZYTERM_INSTANCE_FIXTURE", &fixture.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before acquiring its lock"
            );
            assert!(Instant::now() < deadline, "lock fixture timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            InstanceGuard::acquire(&fixture.root).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(InstanceGuard::acquire(&fixture.root).is_ok());
    }

    #[test]
    #[ignore = "single-instance lock subprocess helper"]
    fn instance_lock_process() {
        let Some(root) = std::env::var_os("LAZYTERM_INSTANCE_FIXTURE") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let _instance = InstanceGuard::acquire(&root).unwrap();
        fs::write(root.join("ready"), b"ready").unwrap();
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
}
