use std::{
    io::{self, Read, Write},
    ops::{Deref, DerefMut},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    time::{Duration, Instant},
};

/// The guard anchors a private process group until its owner's pipe closes.
/// Unlike a saved PID, this group cannot be recycled while the guard is alive.
pub struct ManagedChild {
    child: Child,
    guard: Child,
    lifeline: Option<ChildStdin>,
}

impl ManagedChild {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut guard = Command::new(std::env::current_exe()?);
        #[cfg(not(test))]
        guard.arg("--process-guard");
        #[cfg(test)]
        guard.args([
            "--ignored",
            "--exact",
            "process_guard::tests::guard_process",
            "--nocapture",
        ]);
        guard
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut guard = guard.spawn()?;
        let lifeline = guard.stdin.take();
        let ready = wait_ready(&mut guard);
        if let Err(error) = ready {
            drop(lifeline);
            let _ = guard.kill();
            let _ = guard.wait();
            return Err(error);
        }
        command.process_group(guard.id() as i32);
        match command.spawn() {
            Ok(child) => Ok(Self {
                child,
                guard,
                lifeline,
            }),
            Err(error) => {
                drop(lifeline);
                let _ = guard.wait();
                Err(error)
            }
        }
    }

    pub fn process_group(&self) -> u32 {
        self.guard.id()
    }

    pub fn group_has_live_processes(&self) -> bool {
        group_has_members(self.guard.id(), Some(self.guard.id()))
    }

    pub fn signal_group(&mut self, signal: i32) -> io::Result<()> {
        if self.guard.try_wait()?.is_some() {
            return Err(io::Error::other("Process guard exited unexpectedly"));
        }
        // An unreaped child reserves this PID even if it dies during this call.
        if unsafe { libc::kill(-(self.guard.id() as i32), signal) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        if signal == libc::SIGKILL {
            let _ = self.child.kill();
        }
        Ok(())
    }
}

pub struct DetachedSsh(Child);

impl DetachedSsh {
    pub fn spawn(ssh: &Command, directory: &Path) -> io::Result<Self> {
        let mut guard = Command::new(std::env::current_exe()?);
        #[cfg(not(test))]
        guard.arg("--ssh-guard").arg(directory).args(ssh.get_args());
        #[cfg(test)]
        {
            guard.args([
                "--ignored",
                "--exact",
                "process_guard::tests::ssh_guard_process",
                "--nocapture",
            ]);
            guard.env("LAZYTERM_TEST_SSH_DIRECTORY", directory);
            for (index, arg) in ssh.get_args().enumerate() {
                guard.env(format!("LAZYTERM_TEST_SSH_ARG_{index}"), arg);
            }
        }
        for (key, value) in ssh.get_envs() {
            if let Some(value) = value {
                guard.env(key, value);
            } else {
                guard.env_remove(key);
            }
        }
        guard.stdin(Stdio::piped()).stderr(Stdio::piped());
        guard.stdout(Stdio::null());
        // Keep the master and its jump processes outside the TUI's terminal session.
        unsafe {
            guard.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        guard.spawn().map(Self)
    }

    pub fn child(&mut self) -> &mut Child {
        &mut self.0
    }

    pub fn stop(&mut self) {
        self.0.stdin.take();
        let _ = self.0.wait();
    }
}

impl Drop for DetachedSsh {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn run_ssh(
    directory: &Path,
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> io::Result<()> {
    let group = checked_group()?;
    let result = (|| {
        // SSH must close normally on TERM. Only the supervisor ignores it.
        let mut ssh = Command::new("ssh")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        ignore_cleanup_signals();
        let mut lifeline = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            if ssh.try_wait()?.is_some() {
                return Ok(());
            }
            match unsafe { libc::poll(&mut lifeline, 1, 50) } {
                0 => continue,
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => continue,
                -1 => return Err(io::Error::last_os_error()),
                _ => return Ok(()),
            }
        }
    })();
    cleanup_group_with_grace(group, Duration::from_millis(250), || {
        let _ = std::fs::remove_file(directory.join("ssh"));
        let _ = std::fs::remove_dir(directory);
    });
    result
}

fn wait_ready(guard: &mut Child) -> io::Result<()> {
    let mut output = guard.stdout.take().expect("piped guard stdout");
    let fd = output.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut bytes = Vec::new();
    let mut buffer = [0; 256];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => return Err(io::Error::other("Process guard exited before ready")),
            Ok(size) => bytes.extend_from_slice(&buffer[..size]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        // libtest writes a banner before invoking the subprocess helper.
        if bytes.windows(12).any(|value| value == b"guard-ready\n") {
            return Ok(());
        }
        if bytes.len() > 4096 || Instant::now() >= deadline {
            return Err(io::Error::other("Process guard readiness timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Deref for ManagedChild {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.lifeline.take();
        let _ = self.guard.wait();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn group_has_members(group: u32, exclude: Option<u32>) -> bool {
    let Ok(snapshot) = Command::new("ps")
        .args(["-axo", "pid=,pgid=,stat="])
        .output()
    else {
        return true;
    };
    if !snapshot.status.success() {
        return true;
    }
    String::from_utf8_lossy(&snapshot.stdout)
        .lines()
        .any(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next().and_then(|value| value.parse::<u32>().ok());
            let pgid = fields.next().and_then(|value| value.parse::<u32>().ok());
            pgid == Some(group)
                && pid != exclude
                && fields.next().is_none_or(|state| !state.starts_with('Z'))
        })
}

pub(crate) fn run() -> io::Result<()> {
    let group = checked_group()?;
    ignore_cleanup_signals();
    io::stdout().write_all(b"guard-ready\n")?;
    io::stdout().flush()?;
    let mut buffer = [0; 64];
    loop {
        match io::stdin().read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    cleanup_group(group, || {});
    Ok(())
}

fn checked_group() -> io::Result<u32> {
    let group = std::process::id();
    if unsafe { libc::getpgrp() } != group as i32 || group <= 1 {
        return Err(io::Error::other(
            "Process guard requires its own process group",
        ));
    }
    Ok(group)
}

fn ignore_cleanup_signals() {
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
}

fn cleanup_group(group: u32, finish: impl FnOnce()) {
    cleanup_group_with_grace(group, Duration::from_secs(2), finish);
}

fn cleanup_group_with_grace(group: u32, grace: Duration, finish: impl FnOnce()) {
    unsafe { libc::kill(-(group as i32), libc::SIGTERM) };
    let deadline = Instant::now() + grace;
    while group_has_members(group, Some(group)) {
        if Instant::now() >= deadline {
            // The guard is still the group leader, so escalation cannot hit a reused group.
            finish();
            unsafe { libc::kill(-(group as i32), libc::SIGKILL) };
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    finish();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::projects::{ProjectDefinition, ProjectLifecycle, tests::Fixture};
    use std::{
        fs,
        net::{TcpListener, TcpStream},
    };

    pub(crate) struct OwnerProcess(pub Child);

    impl Drop for OwnerProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub(crate) fn start_owner(fixture: &Fixture, persistent: bool) -> OwnerProcess {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "process_guard::tests::app_owner_process",
                "--nocapture",
            ])
            .env("LAZYTERM_RECOVERY_ROOT", &fixture.root)
            .env(
                "LAZYTERM_RECOVERY_PERSISTENT",
                if persistent { "1" } else { "0" },
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut owner = OwnerProcess(child);
        wait_until(|| {
            assert!(
                owner.0.try_wait().unwrap().is_none(),
                "Fixture owner exited before ready"
            );
            fixture.ready().is_some() && fixture.root.join("owner-ready").exists()
        });
        owner
    }

    pub(crate) fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "Recovery fixture timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn killed_app_releases_lock_cleans_its_group_and_can_restart_without_harming_other_runs() {
        let fixture = Fixture::new("sigkill-app");
        let unrelated = Fixture::new("sigkill-unrelated");
        let mut sentinel = unrelated.service();
        sentinel
            .start(&unrelated.project, &unrelated.listener_command(false))
            .unwrap();
        crate::projects::tests::until(&mut sentinel, |_| unrelated.ready().is_some());
        let (_, unrelated_port) = unrelated.ready().unwrap();

        let mut owner = start_owner(&fixture, false);
        let (_, port) = fixture.ready().unwrap();
        let group: u32 = fs::read_to_string(fixture.root.join("owner-ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(crate::instance::InstanceGuard::acquire(&fixture.root).is_err());
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        let _instance = crate::instance::InstanceGuard::acquire(&fixture.root).unwrap();
        wait_until(|| !crate::projects::group_has_live_processes(group));
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        assert!(TcpStream::connect(("127.0.0.1", unrelated_port)).is_ok());

        fs::remove_file(fixture.root.join("ready")).unwrap();
        let mut service = fixture.service();
        let command = fixture.listener_command(false);
        service.start(&fixture.project, &command).unwrap();
        crate::projects::tests::until(&mut service, |_| fixture.ready().is_some());
        assert_ne!(service.owners()[0].process_group, group);
    }

    #[test]
    fn failed_spawn_cleans_the_guard_and_returns_the_original_error() {
        let error = ManagedChild::spawn(&mut Command::new("/no/lazyterm-fixture-executable"))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn killed_ssh_owner_cleans_up_before_authentication() {
        let started = Instant::now();
        let fixture = Fixture::new("sigkill-ssh");
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut owner = OwnerProcess(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "process_guard::tests::ssh_owner_process",
                ])
                .env("LAZYTERM_TEST_SSH_OWNER", &fixture.root)
                .env(
                    "LAZYTERM_TEST_SSH_PORT",
                    listener.local_addr().unwrap().port().to_string(),
                )
                .spawn()
                .unwrap(),
        );
        let mut pending_socket = None;
        wait_until(|| {
            if pending_socket.is_none() {
                pending_socket = listener.accept().ok();
            }
            pending_socket.is_some() && fixture.root.join("owner-ready").exists()
        });
        let group: u32 = fs::read_to_string(fixture.root.join("owner-ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::getsid(group as i32) }, group as i32);
        assert!(group_has_members(group, Some(group)));
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        wait_until(|| !group_has_members(group, None));
        assert!(!fixture.root.join("control").exists());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "SSH cleanup took {:?}",
            started.elapsed()
        );
    }

    #[test]
    #[ignore = "isolated SSH owner for SIGKILL recovery test"]
    fn ssh_owner_process() {
        let Some(root) = std::env::var_os("LAZYTERM_TEST_SSH_OWNER") else {
            return;
        };
        let root = Path::new(&root);
        let directory = root.join("control");
        fs::create_dir(&directory).unwrap();
        let mut command = Command::new("ssh");
        command
            .args(["-F", "/dev/null", "-M", "-N", "-T", "-S"])
            .arg(directory.join("ssh"))
            .arg("-p")
            .arg(std::env::var_os("LAZYTERM_TEST_SSH_PORT").unwrap())
            .arg("127.0.0.1");
        let mut child = DetachedSsh::spawn(&command, &directory).unwrap();
        fs::write(root.join("owner-ready"), child.child().id().to_string()).unwrap();
        while child.child().try_wait().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    #[ignore = "detached SSH supervisor subprocess helper"]
    fn ssh_guard_process() {
        let Some(directory) = std::env::var_os("LAZYTERM_TEST_SSH_DIRECTORY") else {
            return;
        };
        let args =
            (0..).map_while(|index| std::env::var_os(format!("LAZYTERM_TEST_SSH_ARG_{index}")));
        super::run_ssh(Path::new(&directory), args).unwrap();
    }

    #[test]
    #[ignore = "isolated App owner for SIGKILL recovery tests"]
    fn app_owner_process() {
        let Some(root) = std::env::var_os("LAZYTERM_RECOVERY_ROOT") else {
            return;
        };
        let root = Path::new(&root).to_path_buf();
        let _instance = crate::instance::InstanceGuard::acquire(&root).unwrap();
        let fixture = Fixture {
            project: ProjectDefinition {
                id: uuid::Uuid::from_u128(17),
                name: "Fixture project".into(),
                base_path: root.clone(),
                ..Default::default()
            },
            root,
        };
        let mut command = fixture.listener_command(true);
        if std::env::var("LAZYTERM_RECOVERY_PERSISTENT").as_deref() == Ok("1") {
            command.lifecycle = ProjectLifecycle::Persistent;
        }
        let mut app = crate::app::App::with_catalog(
            crate::hosts::HostCatalog::default(),
            Ok(Default::default()),
        );
        app.metadata_error = true;
        app.project_service = fixture.service();
        app.project_service
            .start(&fixture.project, &command)
            .unwrap();
        fs::write(
            fixture.root.join("owner-ready"),
            app.project_service.owners()[0].process_group.to_string(),
        )
        .unwrap();
        loop {
            app.project_service.poll();
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    #[ignore = "process group guard subprocess helper"]
    fn guard_process() {
        super::run().unwrap();
    }
}
