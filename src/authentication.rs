//! Per-connection askpass routing. Secrets travel only through a private local socket.
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, BufRead, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::credentials::HostAuth;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedHost {
    pub alias: String,
    pub username: Option<String>,
    pub reference: Option<String>,
}

pub struct Service {
    state: Arc<State>,
    pub events: Receiver<Event>,
    directory: PathBuf,
}

struct State {
    socket: PathBuf,
    sessions: Mutex<HashMap<String, Weak<Session>>>,
    events: Sender<Event>,
    stopped: AtomicBool,
}

#[derive(Clone)]
pub struct Client {
    state: Arc<State>,
    pub hosts: Vec<SavedHost>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticationClient")
            .finish_non_exhaustive()
    }
}

impl PartialEq for Client {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state) && self.hosts == other.hosts
    }
}
impl Eq for Client {}

pub enum Event {
    Challenge(Challenge),
    PublicKeyAccepted {
        alias: String,
    },
    Accepted {
        alias: String,
        username: String,
        password: Zeroizing<String>,
    },
}

pub struct Challenge {
    pub alias: String,
    pub username: String,
    pub rejected: bool,
    pub can_save: bool,
    session: Weak<Session>,
    response: Sender<Option<Answer>>,
}

pub struct Answer {
    pub password: Zeroizing<String>,
    pub save: bool,
}

impl Challenge {
    pub fn active(&self) -> bool {
        self.session.upgrade().is_some_and(|s| {
            !s.cancelled.load(Ordering::Acquire) && !s.closed.load(Ordering::Acquire)
        })
    }

    pub fn respond(self, answer: Option<Answer>) {
        if answer.is_none()
            && let Some(session) = self.session.upgrade()
        {
            session.cancelled.store(true, Ordering::Release);
        }
        let _ = self.response.send(answer);
    }
}

#[derive(Clone)]
struct Route {
    alias: String,
    hostname: String,
    username: String,
    port: String,
    reference: Option<String>,
    can_save: bool,
}

struct Session {
    token: String,
    routes: Vec<Route>,
    state: Arc<State>,
    attempts: Mutex<HashMap<String, usize>>,
    pending: Mutex<HashMap<String, Answer>>,
    cancelled: AtomicBool,
    closed: AtomicBool,
    authenticated: AtomicBool,
    diagnostics: Mutex<String>,
}

#[derive(Clone)]
pub struct Status(Arc<Session>);

impl Status {
    pub fn cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn authenticated(&self) -> bool {
        self.0.authenticated.load(Ordering::Acquire)
    }
}

/// Owned by the stderr reader; dropping it releases blocked askpass requests.
pub struct Trace(Arc<Session>, Option<(PathBuf, std::thread::JoinHandle<()>)>);

impl Drop for Trace {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Trace {
    fn finish(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        if let Some((path, worker)) = self.1.take() {
            let _ = worker.join();
            let _ = fs::remove_file(path);
        }
        if let Ok(mut sessions) = self.0.state.sessions.lock() {
            sessions.remove(&self.0.token);
        }
    }
}

impl Trace {
    pub fn status(&self) -> Status {
        Status(self.0.clone())
    }

    #[cfg(test)]
    pub(crate) fn observe(&self, line: &str) -> bool {
        self.0.observe(line)
    }

    pub(crate) fn filter_stderr(&self, line: &str) -> bool {
        // Only synthetic unit-test traces parse stderr. Production reads the private -E log.
        #[cfg(test)]
        if self.1.is_none() {
            return self.0.observe(line);
        }
        let _ = line;
        false
    }

    pub(crate) fn finish_diagnostics(&mut self) -> String {
        self.finish();
        std::mem::take(&mut self.0.diagnostics.lock().unwrap())
    }
}

impl Session {
    fn observe(&self, line: &str) -> bool {
        let detail = line.trim().strip_prefix("debug1: ").unwrap_or(line.trim());
        if let Some(authenticated) = detail.strip_prefix("Authenticated to ") {
            let via_proxy = authenticated.contains(" (via proxy) ");
            for route in &self.routes {
                let is_target = self
                    .routes
                    .last()
                    .is_some_and(|target| target.alias == route.alias);
                let host_matches = authenticated.starts_with(&format!("{} ", route.hostname))
                    || (via_proxy
                        && authenticated.starts_with(&format!("{} (via proxy)", route.alias)));
                let endpoint_matches =
                    via_proxy || authenticated.contains(&format!("]:{})", route.port));
                if !host_matches || !endpoint_matches {
                    continue;
                }
                if authenticated.contains("using \"publickey\"") && route.can_save {
                    let _ = self.state.events.send(Event::PublicKeyAccepted {
                        alias: route.alias.clone(),
                    });
                }
                if is_target {
                    self.authenticated.store(true, Ordering::Release);
                }
                if authenticated.contains("using \"password\"")
                    && let Some(answer) = self.pending.lock().unwrap().remove(&route.alias)
                    && answer.save
                    && route.can_save
                {
                    let _ = self.state.events.send(Event::Accepted {
                        alias: route.alias.clone(),
                        username: route.username.clone(),
                        password: answer.password,
                    });
                }
            }
        }
        line.starts_with("debug1: ")
            || line.starts_with("debug2: ")
            || line.starts_with("debug3: ")
            || detail.starts_with("Authenticated to ")
    }
}

fn watch_client_log(mut file: fs::File, session: &Session) {
    let mut buffer = [0; 4096];
    let mut line = Vec::new();
    let mut overflow = false;
    loop {
        match file.read(&mut buffer) {
            Ok(0) if session.closed.load(Ordering::Acquire) => break,
            Ok(0) => std::thread::sleep(Duration::from_millis(10)),
            Ok(size) => {
                for byte in &buffer[..size] {
                    if *byte == b'\n' {
                        if !overflow {
                            let text = String::from_utf8_lossy(&line);
                            if !session.observe(&text) {
                                let mut errors = session.diagnostics.lock().unwrap();
                                if errors.len() < 16 * 1024 {
                                    errors.push_str(&crate::diagnostics::redact_sensitive_text(
                                        &text,
                                    ));
                                    errors.push('\n');
                                }
                            }
                        }
                        line.clear();
                        overflow = false;
                    } else if line.len() < 16 * 1024 {
                        line.push(*byte);
                    } else {
                        overflow = true;
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Removes SSH's authentication tracing before errors reach logs or output panes.
pub struct Reader<R> {
    inner: io::BufReader<R>,
    trace: Option<Trace>,
    pending: Vec<u8>,
    offset: usize,
    finished: bool,
    client_log: bool,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R, trace: Option<Trace>) -> Self {
        let client_log = trace.as_ref().is_some_and(|trace| trace.1.is_some());
        Self {
            inner: io::BufReader::new(inner),
            trace,
            pending: Vec::new(),
            offset: 0,
            finished: false,
            client_log,
        }
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.trace.is_none() {
            return self.inner.read(buffer);
        }
        if self.client_log && self.offset == self.pending.len() && !self.finished {
            let size = self.inner.read(buffer)?;
            if size > 0 {
                return Ok(size);
            }
            self.finished = true;
            self.pending = self
                .trace
                .as_mut()
                .unwrap()
                .finish_diagnostics()
                .into_bytes();
            self.offset = 0;
        }
        if self.client_log && self.offset == self.pending.len() && self.finished {
            return Ok(0);
        }
        while self.offset == self.pending.len() {
            self.pending.clear();
            self.offset = 0;
            if self.inner.read_until(b'\n', &mut self.pending)? == 0 {
                if self.finished {
                    return Ok(0);
                }
                self.finished = true;
                self.pending = self
                    .trace
                    .as_mut()
                    .unwrap()
                    .finish_diagnostics()
                    .into_bytes();
                if self.pending.is_empty() {
                    return Ok(0);
                }
                break;
            }
            if self
                .trace
                .as_ref()
                .unwrap()
                .filter_stderr(&String::from_utf8_lossy(&self.pending))
            {
                self.pending.clear();
            }
        }
        let size = buffer.len().min(self.pending.len() - self.offset);
        buffer[..size].copy_from_slice(&self.pending[self.offset..self.offset + size]);
        self.offset += size;
        Ok(size)
    }
}

impl Service {
    pub fn new() -> io::Result<Self> {
        let directory = std::env::temp_dir().join(format!("lta-{}", uuid::Uuid::new_v4().simple()));
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let socket = directory.join("auth");
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let (sender, events) = mpsc::channel();
        let state = Arc::new(State {
            socket,
            sessions: Mutex::new(HashMap::new()),
            events: sender,
            stopped: AtomicBool::new(false),
        });
        let worker = state.clone();
        std::thread::Builder::new()
            .name("lazyterm-auth".into())
            .spawn(move || {
                while !worker.stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let state = worker.clone();
                            std::thread::spawn(move || {
                                let _ = serve(stream, &state);
                            });
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            state,
            events,
            directory,
        })
    }

    pub fn client(&self, hosts: Vec<SavedHost>) -> Client {
        Client {
            state: self.state.clone(),
            hosts,
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        for session in self
            .state
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter_map(Weak::upgrade)
        {
            session.cancelled.store(true, Ordering::Release);
        }
        let _ = fs::remove_file(&self.state.socket);
        let _ = fs::remove_dir(&self.directory);
    }
}

impl Client {
    pub fn configure(
        &self,
        command: &mut Command,
        alias: &str,
        auth: &HostAuth,
    ) -> io::Result<Trace> {
        let args = command.get_args().map(|s| s.to_owned()).collect::<Vec<_>>();
        if !args.iter().any(|arg| arg == "-S") {
            command.args(["-o", "ControlMaster=no", "-o", "ControlPath=none"]);
        }
        let config = args
            .windows(2)
            .find(|a| a[0] == "-F")
            .map(|a| PathBuf::from(&a[1]));
        let mut routes = Vec::new();
        resolve_routes(
            alias,
            auth.username.as_deref(),
            None,
            config.as_deref(),
            &self.hosts,
            &mut Vec::new(),
            &mut routes,
        )?;
        let token = uuid::Uuid::new_v4().to_string();
        let log = self
            .state
            .socket
            .parent()
            .unwrap()
            .join(format!("{token}.ssh.log"));
        let log_reader = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&log)?;
        // Give every jump its own client-generated log boundary, separate from remote stderr.
        let mut proxy = "none".to_string();
        for (index, route) in routes.iter().take(routes.len() - 1).enumerate() {
            let next = &routes[index + 1];
            let mut args = vec![
                "ssh".into(),
                "-v".into(),
                "-E".into(),
                log.to_string_lossy().into_owned(),
                "-o".into(),
                "ControlMaster=no".into(),
                "-o".into(),
                "ControlPath=none".into(),
                "-o".into(),
                "BatchMode=no".into(),
                "-o".into(),
                "NumberOfPasswordPrompts=3".into(),
                "-o".into(),
                format!("ProxyCommand={proxy}"),
                "-l".into(),
                route.username.clone(),
                "-p".into(),
                route.port.clone(),
                "-W".into(),
                format!("[{}]:{}", next.hostname, next.port),
            ];
            if let Some(config) = &config {
                args.extend(["-F".into(), config.to_string_lossy().into_owned()]);
            }
            args.push(route.alias.clone());
            proxy = shell_words::join(args);
        }
        if routes.len() > 1 {
            command.args(["-o", &format!("ProxyCommand={proxy}")]);
        }
        let session = Arc::new(Session {
            token: token.clone(),
            routes,
            state: self.state.clone(),
            attempts: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            cancelled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            authenticated: AtomicBool::new(false),
            diagnostics: Mutex::new(String::new()),
        });
        self.state
            .sessions
            .lock()
            .unwrap()
            .insert(token.clone(), Arc::downgrade(&session));
        command
            .arg("-E")
            .arg(&log)
            .args([
                "-v",
                "-o",
                "BatchMode=no",
                "-o",
                "NumberOfPasswordPrompts=3",
            ])
            .env("SSH_ASKPASS", std::env::current_exe()?)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "lazyterm:0")
            .env("LAZYTERM_AUTH_SOCKET", &self.state.socket)
            .env("LAZYTERM_AUTH_TOKEN", token);
        #[cfg(test)]
        if let Some(executable) = &auth.askpass {
            command.env("SSH_ASKPASS", executable);
        }
        let worker_session = session.clone();
        let worker = std::thread::spawn(move || watch_client_log(log_reader, &worker_session));
        Ok(Trace(session, Some((log, worker))))
    }
}

fn resolve_routes(
    alias: &str,
    username: Option<&str>,
    port: Option<&str>,
    config: Option<&Path>,
    hosts: &[SavedHost],
    visiting: &mut Vec<String>,
    routes: &mut Vec<Route>,
) -> io::Result<()> {
    if visiting.len() >= 8 || visiting.iter().any(|v| v == alias) {
        return Err(io::Error::other(
            "Cyclic or excessively long ProxyJump chain.",
        ));
    }
    visiting.push(alias.to_string());
    let saved = hosts.iter().find(|h| h.alias == alias);
    let username = username.or_else(|| saved.and_then(|h| h.username.as_deref()));
    let mut resolver = Command::new("ssh");
    resolver.arg("-G");
    if let Some(config) = config {
        resolver.arg("-F").arg(config);
    }
    if let Some(user) = username {
        resolver.args(["-l", user]);
    }
    if let Some(port) = port {
        resolver.args(["-p", port]);
    }
    let output = resolver.arg("--").arg(alias).output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "Could not resolve SSH authentication route.",
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let option = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key).map(str::trim))
    };
    if let Some(jumps) = option("proxyjump ").filter(|j| !j.eq_ignore_ascii_case("none")) {
        for jump in jumps.split(',') {
            let (user, host, port) = split_jump(jump)?;
            resolve_routes(
                &host,
                user.as_deref(),
                port.as_deref(),
                config,
                hosts,
                visiting,
                routes,
            )?;
        }
    }
    let hostname = option("hostname ")
        .ok_or_else(|| io::Error::other("Missing SSH hostname."))?
        .to_string();
    let username = option("user ")
        .ok_or_else(|| io::Error::other("Missing SSH user."))?
        .to_string();
    let port = option("port ")
        .ok_or_else(|| io::Error::other("Missing SSH port."))?
        .to_string();
    let route = Route {
        alias: alias.into(),
        hostname,
        username: username.clone(),
        port: port.clone(),
        reference: saved.and_then(|s| s.reference.clone()),
        can_save: saved.is_some(),
    };
    if routes.iter().any(|r| r.hostname == route.hostname) {
        return Err(io::Error::other(
            "Ambiguous authentication targets in ProxyJump chain.",
        ));
    }
    routes.push(route);
    visiting.pop();
    Ok(())
}

fn split_jump(jump: &str) -> io::Result<(Option<String>, String, Option<String>)> {
    if jump.starts_with("ssh://") {
        let uri = url::Url::parse(jump).map_err(|_| io::Error::other("Invalid ProxyJump URI."))?;
        let host = uri
            .host_str()
            .ok_or_else(|| io::Error::other("Missing ProxyJump host."))?
            .trim_matches(['[', ']'])
            .to_string();
        return Ok((
            (!uri.username().is_empty()).then(|| uri.username().to_string()),
            host,
            uri.port().map(|port| port.to_string()),
        ));
    }
    let (user, address) = jump
        .rsplit_once('@')
        .map_or((None, jump), |(u, h)| (Some(u.to_string()), h));
    let (host, port) = if let Some(address) = address.strip_prefix('[') {
        let (host, suffix) = address
            .split_once(']')
            .ok_or_else(|| io::Error::other("Invalid IPv6 jump host."))?;
        if !suffix.is_empty() && !suffix.starts_with(':') {
            return Err(io::Error::other("Invalid IPv6 jump port."));
        }
        (host, suffix.strip_prefix(':').map(str::to_string))
    } else if address.matches(':').count() == 1 {
        let (host, port) = address.split_once(':').unwrap();
        (host, Some(port.to_string()))
    } else {
        (address, None)
    };
    if host.is_empty() || host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return Err(io::Error::other("Invalid ProxyJump host."));
    }
    if port
        .as_ref()
        .is_some_and(|port| port.parse::<u16>().ok().is_none_or(|port| port == 0))
    {
        return Err(io::Error::other("Invalid ProxyJump port."));
    }
    Ok((user, host.to_string(), port))
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    prompt: String,
}

fn send_packet(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)
}

fn receive_packet(stream: &mut UnixStream) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 16 * 1024 {
        return Err(io::Error::other("Authentication message is too large."));
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn serve(mut stream: UnixStream, state: &State) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let bytes = receive_packet(&mut stream)?;
    let request: Request = serde_json::from_slice(&bytes)?;
    let session = state
        .sessions
        .lock()
        .unwrap()
        .get(&request.token)
        .and_then(Weak::upgrade)
        .ok_or_else(|| io::Error::other("Expired authentication request."))?;
    let prompt = request.prompt.trim();
    let route = session
        .routes
        .iter()
        .find(|r| prompt == format!("{}@{}'s password:", r.username, r.hostname))
        .or_else(|| {
            let (username, host) = prompt.split_once('@')?;
            let hostname = host.strip_suffix("'s password:")?;
            let target_alias = &session.routes.last()?.alias;
            let route = session
                .routes
                .iter()
                .find(|route| route.username == username && &route.alias == target_alias)?;
            (hostname == route.alias).then_some(route)
        })
        .ok_or_else(|| io::Error::other("Unrecognized SSH authentication prompt."))?;
    if session.cancelled.load(Ordering::Acquire) || session.closed.load(Ordering::Acquire) {
        return Ok(());
    }
    let attempt = {
        let mut attempts = session.attempts.lock().unwrap();
        let value = attempts.entry(route.alias.clone()).or_default();
        *value += 1;
        *value
    };
    if attempt > 3 {
        session.cancelled.store(true, Ordering::Release);
        return Ok(());
    }
    if attempt == 1
        && let Some(reference) = &route.reference
        && let Ok(secret) = crate::project_credentials::read_password(reference)
    {
        return send_packet(&mut stream, secret.as_bytes());
    }
    session.pending.lock().unwrap().remove(&route.alias);
    let (sender, receiver) = mpsc::channel();
    let _ = state.events.send(Event::Challenge(Challenge {
        alias: route.alias.clone(),
        username: route.username.clone(),
        rejected: attempt > 1,
        can_save: route.can_save,
        session: Arc::downgrade(&session),
        response: sender,
    }));
    let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;
    while !session.cancelled.load(Ordering::Acquire)
        && !session.closed.load(Ordering::Acquire)
        && std::time::Instant::now() < deadline
    {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(answer)) => {
                let secret = Zeroizing::new(answer.password.to_string());
                session
                    .pending
                    .lock()
                    .unwrap()
                    .insert(route.alias.clone(), answer);
                send_packet(&mut stream, secret.as_bytes())?;
                return Ok(());
            }
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    if !session.closed.load(Ordering::Acquire) {
        session.cancelled.store(true, Ordering::Release);
    }
    Ok(())
}

pub fn askpass() -> io::Result<()> {
    let socket = std::env::var_os("LAZYTERM_AUTH_SOCKET")
        .ok_or_else(|| io::Error::other("Missing authentication socket."))?;
    let mut stream = UnixStream::connect(PathBuf::from(socket))?;
    stream.set_read_timeout(Some(RESPONSE_TIMEOUT + Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = Request {
        token: std::env::var("LAZYTERM_AUTH_TOKEN")
            .map_err(|_| io::Error::other("Missing authentication token."))?,
        prompt: std::env::args().nth(1).unwrap_or_default(),
    };
    send_packet(&mut stream, &serde_json::to_vec(&request)?)?;
    let password = receive_packet(&mut stream)?;
    io::stdout().write_all(&password)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::thread::JoinHandle;

    pub fn fixture(reference: Option<String>) -> (Service, Trace) {
        let service = Service::new().unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(Session {
            token: token.clone(),
            routes: vec![Route {
                alias: "web".into(),
                hostname: "127.0.0.1".into(),
                username: "tester".into(),
                port: "22".into(),
                reference,
                can_save: true,
            }],
            state: service.state.clone(),
            attempts: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            cancelled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            authenticated: AtomicBool::new(false),
            diagnostics: Mutex::new(String::new()),
        });
        service
            .state
            .sessions
            .lock()
            .unwrap()
            .insert(token, Arc::downgrade(&session));
        (service, Trace(session, None))
    }

    pub fn request(trace: &Trace, prompt: &str) -> JoinHandle<io::Result<Zeroizing<Vec<u8>>>> {
        let socket = trace.0.state.socket.clone();
        let request = Request {
            token: trace.0.token.clone(),
            prompt: prompt.into(),
        };
        std::thread::spawn(move || {
            let mut stream = UnixStream::connect(socket)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            send_packet(&mut stream, &serde_json::to_vec(&request)?)?;
            receive_packet(&mut stream)
        })
    }

    fn challenge(service: &Service) -> Challenge {
        match service.events.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Challenge(challenge) => challenge,
            _ => panic!("Expected password challenge"),
        }
    }

    fn answer(challenge: Challenge, password: &str, save: bool) {
        challenge.respond(Some(Answer {
            password: Zeroizing::new(password.into()),
            save,
        }));
    }

    #[test]
    fn password_is_only_saved_after_openssh_confirms_correct_target_and_method() {
        let (service, trace) = fixture(None);
        let first = request(&trace, "tester@127.0.0.1's password: ");
        answer(challenge(&service), "wrong-password", true);
        assert_eq!(&**first.join().unwrap().unwrap(), b"wrong-password");
        assert!(service.events.try_recv().is_err());
        trace.observe("debug1: Authentications that can continue: password");
        let second = request(&trace, "tester@127.0.0.1's password:");
        let retry = challenge(&service);
        assert!(retry.rejected);
        answer(retry, "current-password", true);
        second.join().unwrap().unwrap();
        trace.observe("Authenticated to other ([127.0.0.1]:22) using \"password\".");
        trace.observe("Authenticated to 127.0.0.1 ([127.0.0.1]:2222) using \"password\".");
        assert!(service.events.try_recv().is_err());
        trace.observe("Authenticated to 127.0.0.1 ([127.0.0.1]:22) using \"password\".");
        match service.events.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::Accepted {
                alias,
                username,
                password,
            } => {
                assert_eq!(alias, "web");
                assert_eq!(username, "tester");
                assert_eq!(password.as_str(), "current-password");
            }
            _ => panic!("Expected accepted password"),
        }
        assert!(trace.status().authenticated());
        trace.observe("Authenticated to 127.0.0.1 ([127.0.0.1]:22) using \"password\".");
        assert!(
            service.events.try_recv().is_err(),
            "Never save the same answer twice"
        );
    }

    #[test]
    fn saved_password_is_offered_once_then_rejection_prompts_for_a_replacement() {
        let mut prepared =
            crate::project_credentials::prepare_credential(Zeroizing::new("old-password".into()))
                .recv()
                .unwrap()
                .unwrap();
        let (service, trace) = fixture(Some(prepared.reference.clone()));
        let first = request(&trace, "tester@127.0.0.1's password:");
        assert_eq!(&**first.join().unwrap().unwrap(), b"old-password");
        assert!(service.events.try_recv().is_err());
        let second = request(&trace, "tester@127.0.0.1's password:");
        let retry = challenge(&service);
        assert!(retry.rejected);
        answer(retry, "one-time", false);
        second.join().unwrap().unwrap();
        trace.observe("Authenticated to 127.0.0.1 ([127.0.0.1]:22) using \"password\".");
        assert!(service.events.try_recv().is_err());
        crate::project_credentials::delete_password(&prepared.reference).unwrap();
        prepared.commit();
    }

    #[test]
    fn reject_hostkey_passphrase_wrong_user_and_expired_requests_without_exposing_secrets() {
        let (service, trace) = fixture(None);
        for prompt in [
            "root@127.0.0.1's password:",
            "tester@other's password:",
            "Password:",
            "Enter passphrase for key:",
            "Are you sure you want to continue connecting (yes/no)?",
        ] {
            assert!(request(&trace, prompt).join().unwrap().is_err());
        }
        assert!(service.events.try_recv().is_err());
        let socket = service.state.socket.clone();
        let mut stream = UnixStream::connect(socket).unwrap();
        send_packet(
            &mut stream,
            &serde_json::to_vec(&Request {
                token: "unknown-token".into(),
                prompt: "tester@127.0.0.1's password:".into(),
            })
            .unwrap(),
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(receive_packet(&mut stream).is_err());
    }

    #[test]
    fn cancel_and_drop_release_only_their_own_waiting_helpers() {
        let (service, trace) = fixture(None);
        let waiting = request(&trace, "tester@127.0.0.1's password:");
        let prompt = challenge(&service);
        let status = trace.status();
        prompt.respond(None);
        assert!(waiting.join().unwrap().is_err());
        assert!(status.cancelled());
        let (other, trace) = fixture(None);
        let waiting = request(&trace, "tester@127.0.0.1's password:");
        let prompt = challenge(&other);
        assert!(prompt.active());
        let status = trace.status();
        drop(trace);
        assert!(!prompt.active());
        assert!(waiting.join().unwrap().is_err());
        assert!(
            !status.cancelled(),
            "A transport failure is different from user cancellation"
        );
    }

    #[test]
    fn reader_filters_auth_tracing_preserves_errors_and_confirms_public_key_use() {
        let (service, trace) = fixture(None);
        let input = "debug1: Offering public key: key\n\
            WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\r\n\
            SHA256:fingerprint\r\n\
            Authenticated to 127.0.0.1 ([127.0.0.1]:22) using \"publickey\".\n\
            command output without newline";
        let mut reader = Reader::new(input.as_bytes(), Some(trace));
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        assert_eq!(
            output,
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\r\nSHA256:fingerprint\r\ncommand output without newline"
        );
        assert!(
            matches!(service.events.try_recv(), Ok(Event::PublicKeyAccepted { alias }) if alias == "web")
        );
        assert!(service.events.try_recv().is_err());
    }

    #[test]
    fn remote_stderr_cannot_authenticate_or_commit_a_saved_password() {
        let (service, trace) = fixture(None);
        let worker = request(&trace, "tester@127.0.0.1's password:");
        answer(challenge(&service), "do-not-save-from-remote-output", true);
        worker.join().unwrap().unwrap();
        let session = trace.0.clone();
        let path = service.directory.join("client.log");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let log_session = session.clone();
        let watcher = std::thread::spawn(move || watch_client_log(file, &log_session));
        let mut production = Trace(session, Some((path, watcher)));
        assert!(
            !production
                .filter_stderr("Authenticated to 127.0.0.1 ([127.0.0.1]:22) using \"password\".")
        );
        assert!(!production.status().authenticated());
        assert!(service.events.try_recv().is_err());
        production.finish_diagnostics();
        drop(trace);
    }

    #[test]
    fn routes_resolve_nested_jumps_and_keychain_users_without_reading_user_configuration() {
        use crate::projects::tests::Fixture;
        let fixture = Fixture::new("authentication-routes");
        let config = fixture.root.join("config");
        fs::write(
            &config,
            "Host target\n HostName 192.0.2.2\n User deploy\n ProxyJump jump\n\
            Host jump\n HostName 192.0.2.1\n User old-user\n Port 2222\n",
        )
        .unwrap();
        let service = Service::new().unwrap();
        let client = service.client(vec![SavedHost {
            alias: "jump".into(),
            username: Some("saved-user".into()),
            reference: Some("jump-only-ref".into()),
        }]);
        let auth = HostAuth {
            broker: Some(client),
            ..Default::default()
        };
        let mut command = Command::new("ssh");
        command.arg("-F").arg(&config);
        let trace = crate::credentials::configure_host_auth(&mut command, &auth, "target")
            .unwrap()
            .unwrap();
        assert_eq!(trace.0.routes.len(), 2);
        assert_eq!(trace.0.routes[0].username, "saved-user");
        assert_eq!(
            trace.0.routes[0].reference.as_deref(),
            Some("jump-only-ref")
        );
        assert_eq!(trace.0.routes[1].username, "deploy");
        assert!(trace.0.routes[1].reference.is_none());
        assert!(!trace.0.routes[1].can_save);
        let args = format!("{:?}", command.get_args().collect::<Vec<_>>());
        let env = format!("{:?}", command.get_envs().collect::<Vec<_>>());
        assert!(
            args.contains("saved-user") && args.contains("2222") && args.contains("ProxyCommand=")
        );
        assert!(!args.contains("jump-only-ref") && !env.contains("jump-only-ref"));
        fs::write(&config, "Host a\n ProxyJump b\nHost b\n ProxyJump a\n").unwrap();
        let mut routes = Vec::new();
        assert!(
            resolve_routes(
                "a",
                None,
                None,
                Some(&config),
                &[],
                &mut Vec::new(),
                &mut routes
            )
            .is_err()
        );
    }

    #[test]
    fn proxyjump_addresses_support_ipv6_uri_and_explicit_users() {
        assert_eq!(
            split_jump("root@[::1]:2222").unwrap(),
            (Some("root".into()), "::1".into(), Some("2222".into()))
        );
        assert_eq!(
            split_jump("ssh://deploy@jump.example:2222").unwrap(),
            (
                Some("deploy".into()),
                "jump.example".into(),
                Some("2222".into())
            )
        );
        for invalid in ["", "-jump", "[::1]oops", "jump:0", "jump:bad"] {
            assert!(split_jump(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn disposable_password_ssh_proxyjump_sftp_project_tunnel_and_snippet_workflows() {
        if std::env::var("LAZYTERM_SSH_PASSWORD_FIXTURE").as_deref() != Ok("1") {
            eprintln!(
                "skipped: set LAZYTERM_SSH_PASSWORD_FIXTURE=1 to enable disposable password SSH integration"
            );
            return;
        }
        use crate::{
            metadata::{TunnelDefinition, TunnelKind, TunnelLifecycle},
            projects::{
                ProjectCommand, ProjectDefinition, ProjectLifecycle, ProjectService, ProjectTarget,
                TaskStatus,
            },
            sftp,
        };
        use std::{
            net::{TcpListener, TcpStream},
            process::{Child, Stdio},
        };

        struct Fixture {
            root: PathBuf,
            child: Child,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        let python =
            std::env::var_os("LAZYTERM_PARAMIKO_PYTHON").unwrap_or_else(|| "python3".into());
        assert!(
            Command::new(&python)
                .args(["-c", "import paramiko"])
                .status()
                .unwrap()
                .success(),
            "Install Paramiko in a test-only Python environment and set LAZYTERM_PARAMIKO_PYTHON"
        );
        let root = std::env::temp_dir().join(format!(
            "lazyterm-password-ssh-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/ssh_password_fixture.py");
        let fixture = Fixture {
            child: Command::new(python)
                .arg(script)
                .arg(&root)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
            root: root.clone(),
        };
        let ready = root.join("ready");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "Password SSH fixture failed to start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let port: u16 = fs::read_to_string(&ready).unwrap().parse().unwrap();
        let config = root.join("config");
        let test_binary = std::env::var_os("LAZYTERM_AUTH_FIXTURE_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("lazyterm")
            });
        assert!(
            test_binary.is_file(),
            "Run cargo build before the password SSH fixture"
        );
        let auth = |service: &Service| HostAuth {
            broker: Some(service.client(vec![
                SavedHost {
                    alias: "web".into(),
                    username: Some("tester".into()),
                    reference: None,
                },
                SavedHost {
                    alias: "jump".into(),
                    username: Some("jump-user".into()),
                    reference: None,
                },
                SavedHost {
                    alias: "via-jump".into(),
                    username: Some("tester".into()),
                    reference: None,
                },
            ])),
            config: Some(config.clone()),
            askpass: Some(test_binary.clone()),
            ..Default::default()
        };
        let accepted_credentials = Arc::new(Mutex::new(Vec::new()));
        let answer = |service: &Service, username: &str, save: bool| loop {
            match service
                .events
                .recv_timeout(Duration::from_secs(10))
                .expect("OpenSSH password prompt")
            {
                Event::Challenge(challenge) => {
                    assert_eq!(challenge.username, username);
                    challenge.respond(Some(Answer {
                        password: Zeroizing::new(
                            if username == "jump-user" {
                                "jump-password"
                            } else {
                                "fixture-password"
                            }
                            .into(),
                        ),
                        save,
                    }));
                    break;
                }
                Event::Accepted {
                    alias, password, ..
                } => accepted_credentials.lock().unwrap().push((alias, password)),
                Event::PublicKeyAccepted { .. } => {}
            }
        };
        let service = Service::new().unwrap();

        // The snippet-style one-shot SSH command uses the same broker.
        let mut snippet = Command::new("ssh");
        let snippet_trace =
            crate::credentials::configure_host_auth(&mut snippet, &auth(&service), "web").unwrap();
        let mut snippet = snippet
            .args(["web", "printf snippet-ok"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = snippet.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut reader = Reader::new(stderr, Some(snippet_trace.unwrap()));
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            bytes
        });
        answer(&service, "tester", false);
        let output = snippet.wait_with_output().unwrap();
        reader.join().unwrap();
        assert!(
            output.status.success(),
            "snippet SSH failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"snippet-ok");

        let project = ProjectDefinition {
            name: "Password fixture".into(),
            base_path: "/".into(),
            target: ProjectTarget::Host("web".into()),
            ..Default::default()
        };
        let task = ProjectCommand {
            name: "probe".into(),
            command: "printf project-ok".into(),
            lifecycle: ProjectLifecycle::App,
            ..Default::default()
        };
        let key = (project.id, task.id);
        let mut projects = ProjectService::new(Some(root.join("project-logs")));
        projects
            .start_with_auth(&project, &task, &auth(&service))
            .unwrap();
        answer(&service, "tester", false);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            projects.poll();
            if let Some(task) = projects.tasks.get(&key)
                && matches!(task.status, TaskStatus::Exited(_))
            {
                assert!(task.output.contains("project-ok"), "{}", task.output);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Project SSH did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let listing = sftp::spawn_list_with_auth("web".into(), "/".into(), auth(&service));
        answer(&service, "tester", false);
        assert!(
            listing
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
                .entries
                .is_empty()
        );

        let mut jump = Command::new("ssh");
        let jump_trace =
            crate::credentials::configure_host_auth(&mut jump, &auth(&service), "via-jump")
                .unwrap();
        let mut jump = jump
            .args(["via-jump", "printf jump-ok"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = jump.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut reader = Reader::new(stderr, Some(jump_trace.unwrap()));
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            bytes
        });
        answer(&service, "jump-user", true);
        answer(&service, "tester", true);
        let output = jump.wait_with_output().unwrap();
        reader.join().unwrap();
        assert!(
            output.status.success(),
            "ProxyJump failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"jump-ok");
        while let Ok(event) = service.events.try_recv() {
            if let Event::Accepted {
                alias, password, ..
            } = event
            {
                accepted_credentials.lock().unwrap().push((alias, password));
            }
        }
        let mut accepted = accepted_credentials
            .lock()
            .unwrap()
            .iter()
            .map(|(alias, password)| {
                assert_eq!(
                    password.as_str(),
                    if alias == "jump" {
                        "jump-password"
                    } else {
                        "fixture-password"
                    }
                );
                alias.clone()
            })
            .collect::<Vec<_>>();
        accepted.sort();
        assert_eq!(accepted, ["jump", "via-jump"]);

        let port_forward = TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let tunnel = TunnelDefinition {
            id: uuid::Uuid::new_v4(),
            label: "fixture".into(),
            host_alias: "web".into(),
            custom_host: false,
            kind: TunnelKind::Local,
            bind_address: "127.0.0.1".into(),
            local_port: port_forward,
            remote_host: Some("127.0.0.1".into()),
            remote_port: Some(port),
            lifecycle: TunnelLifecycle::App,
            auto_reconnect: false,
            connect_timeout_secs: 5,
            keepalive_interval_secs: 5,
        };
        let (events, _) = mpsc::channel();
        let (mut tunnel_process, status) =
            crate::tunnel::spawn_forward(&tunnel, events, &auth(&service)).unwrap();
        answer(&service, "tester", false);
        let status = status.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !status.authenticated() || TcpStream::connect(("127.0.0.1", port_forward)).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "Tunnel did not open its local port"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        tunnel_process.kill().unwrap();
        tunnel_process.wait().unwrap();
        drop(tunnel_process);

        // Exercise App polling after a real SSH SIGKILL, then kill the App owner itself.
        fs::write(
            root.join("tunnel.json"),
            serde_json::to_vec(&tunnel).unwrap(),
        )
        .unwrap();
        for kill_owner in [false, true] {
            let _ = fs::remove_file(root.join("tunnel-owner-ready"));
            let _ = fs::remove_file(root.join("tunnel-owner-failed"));
            let mut owner = crate::process_guard::tests::OwnerProcess(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        "app::tests::tunnel_recovery_owner_process",
                        "--nocapture",
                    ])
                    .env("LAZYTERM_TUNNEL_RECOVERY_ROOT", &root)
                    .env("LAZYTERM_TUNNEL_RECOVERY_BINARY", &test_binary)
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let mut tunnel_pid = None;
            crate::process_guard::tests::wait_until(|| {
                assert!(
                    owner.0.try_wait().unwrap().is_none(),
                    "Tunnel owner exited before ready"
                );
                tunnel_pid = fs::read_to_string(root.join("tunnel-owner-ready"))
                    .ok()
                    .and_then(|value| value.parse::<i32>().ok());
                tunnel_pid.is_some()
            });
            if kill_owner {
                owner.0.kill().unwrap();
                owner.0.wait().unwrap();
            } else {
                assert_eq!(unsafe { libc::kill(tunnel_pid.unwrap(), libc::SIGKILL) }, 0);
                crate::process_guard::tests::wait_until(|| {
                    root.join("tunnel-owner-failed").exists()
                });
                assert!(owner.0.wait().unwrap().success());
            }
            crate::process_guard::tests::wait_until(|| {
                TcpStream::connect(("127.0.0.1", port_forward)).is_err()
            });
            assert!(
                TcpListener::bind(("127.0.0.1", port_forward)).is_ok(),
                "Killed Tunnel must release its bind port"
            );
        }

        let (events, _) = mpsc::channel();
        let (mut restarted, restarted_status) =
            crate::tunnel::spawn_forward(&tunnel, events, &auth(&service)).unwrap();
        answer(&service, "tester", false);
        crate::process_guard::tests::wait_until(|| {
            restarted_status.as_ref().unwrap().authenticated()
                && TcpStream::connect(("127.0.0.1", port_forward)).is_ok()
        });
        restarted.kill().unwrap();
        restarted.wait().unwrap();
        drop(restarted);

        let pending = crate::native_connection::Pending::start(
            "web".into(),
            crate::native_connection::Kind::Shell,
            auth(&service),
        )
        .unwrap();
        answer(&service, "tester", false);
        let connection = pending
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert!(connection.control_path().exists());
        let mut session = connection.command();
        session
            .args(["exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        assert!(session.status().unwrap().success());
        drop(connection);
        drop(fixture);
    }
}
