use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CredentialKind {
    Password,
    SshKey,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostAuth {
    pub username: Option<String>,
    pub password_ref: Option<String>,
    pub broker: Option<crate::authentication::Client>,
    pub config: Option<PathBuf>,
    #[cfg(test)]
    pub askpass: Option<PathBuf>,
}

pub fn configure_host_auth(
    command: &mut Command,
    auth: &HostAuth,
    alias: &str,
) -> io::Result<Option<crate::authentication::Trace>> {
    if let Some(config) = &auth.config {
        command.arg("-F").arg(config);
    }
    if let Some(username) = auth.username.as_deref() {
        command.args(["-l", username]);
    }
    if let Some(broker) = &auth.broker {
        return broker.configure(command, alias, auth).map(Some);
    }
    if let Some(reference) = auth.password_ref.as_deref() {
        command.args(["-o", "BatchMode=no"]);
        configure_password_for(command, reference, alias, auth.username.as_deref())?;
    } else {
        command.args(["-o", "BatchMode=yes"]);
    }
    Ok(None)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Credential {
    pub id: crate::projects::EntityId,
    pub name: String,
    pub kind: CredentialKind,
    #[serde(default)]
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_path: Option<PathBuf>,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredentialSource {
    Managed,
    ProjectLink {
        project_id: crate::projects::EntityId,
        link_id: crate::projects::EntityId,
    },
    DiscoveredKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialItem {
    pub credential: Credential,
    pub source: CredentialSource,
}

pub fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest)
    } else {
        PathBuf::from(value)
    }
}

pub fn same_path(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left).unwrap_or_else(|_| left.into())
        == fs::canonicalize(right).unwrap_or_else(|_| right.into())
}

pub fn import_key(path: &Path, id: uuid::Uuid, name: String) -> Result<Credential, String> {
    if !path.is_file() {
        return Err("Private key file does not exist.".into());
    }
    let mut public_path = path.as_os_str().to_os_string();
    public_path.push(".pub");
    let public_path = PathBuf::from(public_path);
    let public_key = fs::read_to_string(&public_path)
        .map_err(|_| "A matching .pub file is required beside the private key.".to_string())?;
    validate_public_key(&public_key)?;
    let fingerprint = key_fingerprint(&public_path)?;
    let private_key = ssh_key::PrivateKey::read_openssh_file(path)
        .map_err(|_| "Expected an OpenSSH private key file.".to_string())?;
    if private_key
        .public_key()
        .fingerprint(ssh_key::HashAlg::Sha256)
        .to_string()
        != fingerprint
    {
        return Err("The public key does not match the private key.".into());
    }
    Ok(Credential {
        id,
        name,
        kind: CredentialKind::SshKey,
        username: String::new(),
        password_ref: None,
        private_key_path: Some(
            fs::canonicalize(path).map_err(|_| "Could not resolve private key path.")?,
        ),
        public_key: public_key.trim().into(),
        fingerprint,
    })
}

pub fn discover_local_keys() -> Vec<Credential> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    discover_keys_in(&home.join(".ssh"))
}

pub fn discover_keys_in(directory: &Path) -> Vec<Credential> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut keys = entries
        .flatten()
        .filter_map(|entry| {
            let public_path = entry.path();
            let file_name = public_path.file_name()?.to_str()?;
            if !file_name.ends_with(".pub") || file_name.ends_with("-cert.pub") {
                return None;
            }
            let private_name = file_name.strip_suffix(".pub")?;
            let private_path = public_path.with_file_name(private_name);
            if !private_path.is_file() {
                return None;
            }
            let public_key = fs::read_to_string(&public_path).ok()?;
            validate_public_key(&public_key).ok()?;
            let fingerprint = key_fingerprint(&public_path).ok()?;
            Some(Credential {
                id: uuid::Uuid::nil(),
                name: private_name.to_string(),
                kind: CredentialKind::SshKey,
                username: String::new(),
                password_ref: None,
                private_key_path: fs::canonicalize(private_path).ok(),
                public_key: public_key.trim().to_string(),
                fingerprint,
            })
        })
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| left.name.cmp(&right.name));
    keys
}

fn key_fingerprint(path: &Path) -> Result<String, String> {
    let output = Command::new("ssh-keygen")
        .args(["-l", "-E", "sha256", "-f"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "Could not start ssh-keygen.".to_string())?;
    if !output.status.success() {
        return Err("ssh-keygen could not validate the key file.".into());
    }
    let fingerprint = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .nth(1)
        .ok_or("Missing key fingerprint.")?
        .to_string();
    Ok(fingerprint)
}

// Reserve a new private directory so ssh-keygen can never overwrite an existing key.
pub fn generate_key(directory: &Path, id: uuid::Uuid, name: String) -> Result<Credential, String> {
    if !directory.is_absolute() {
        return Err("Use an absolute directory or ~/path.".into());
    }
    if !directory.parent().is_some_and(Path::is_dir) {
        return Err("The parent directory must already exist.".into());
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|_| {
        "Choose a new directory; existing directories are never overwritten.".to_string()
    })?;
    let path = directory.join("id_ed25519");
    let result = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "lazyterm", "-f"])
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !result.is_ok_and(|status| status.success()) {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(directory.join("id_ed25519.pub"));
        let _ = fs::remove_dir(directory);
        return Err("Could not generate SSH key with ssh-keygen.".into());
    }
    import_key(&path, id, name)
}

fn validate_public_key(key: &str) -> Result<(), String> {
    let parts = key.split_whitespace().collect::<Vec<_>>();
    if key.trim().contains(['\n', '\r'])
        || parts.len() < 2
        || !matches!(
            parts[0],
            "ssh-ed25519"
                | "ssh-rsa"
                | "ecdsa-sha2-nistp256"
                | "ecdsa-sha2-nistp384"
                | "ecdsa-sha2-nistp521"
                | "sk-ssh-ed25519@openssh.com"
                | "sk-ecdsa-sha2-nistp256@openssh.com"
        )
        || !parts[1]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
    {
        return Err("Expected one OpenSSH public key.".into());
    }
    Ok(())
}

pub fn upload_command(key: &str) -> Result<String, String> {
    validate_public_key(key)?;
    Ok(format!(
        "key={}; umask 077; [ ! -L \"$HOME/.ssh\" ] && mkdir -p \"$HOME/.ssh\" && chmod 700 \"$HOME/.ssh\" || exit 11; file=\"$HOME/.ssh/authorized_keys\"; [ ! -L \"$file\" ] || exit 12; touch \"$file\" && chmod 600 \"$file\" || exit 13; kind=$(printf '%s' \"$key\" | awk '{{print $1}}'); blob=$(printf '%s' \"$key\" | awk '{{print $2}}'); if ! awk -v k=\"$kind\" -v b=\"$blob\" '$1==k && $2==b {{found=1}} END {{exit !found}}' \"$file\"; then printf '\\n%s\\n' \"$key\" >> \"$file\" || exit 14; fi",
        shell_words::quote(key.trim())
    ))
}

pub fn configure_password_for(
    command: &mut Command,
    reference: &str,
    alias: &str,
    username: Option<&str>,
) -> io::Result<()> {
    let mut resolver = Command::new("ssh");
    resolver.args(["-G"]);
    if let Some(username) = username {
        resolver.args(["-l", username]);
    }
    let output = resolver
        .arg("--")
        .arg(alias)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("Could not resolve SSH password target"));
    }
    let config = String::from_utf8_lossy(&output.stdout);
    let option = |name: &str| {
        config
            .lines()
            .find_map(|line| line.strip_prefix(name).map(str::trim))
    };
    let user = option("user ").ok_or_else(|| io::Error::other("Missing resolved SSH user"))?;
    let hostname =
        option("hostname ").ok_or_else(|| io::Error::other("Missing resolved SSH hostname"))?;
    command
        .env("SSH_ASKPASS", std::env::current_exe()?)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "lazyterm:0")
        .env("LAZYTERM_ASKPASS_REFERENCE", reference)
        .env(
            "LAZYTERM_ASKPASS_TARGET",
            format!("{user}@{hostname}'s password:"),
        );
    Ok(())
}

pub fn askpass() -> Result<(), String> {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let expected =
        std::env::var("LAZYTERM_ASKPASS_TARGET").map_err(|_| "Missing authentication target.")?;
    // Never answer a host-key or private-key passphrase prompt with a login password.
    if !password_prompt_matches(&prompt, &expected) {
        return Err("Unsupported authentication prompt.".into());
    }
    let reference =
        std::env::var("LAZYTERM_ASKPASS_REFERENCE").map_err(|_| "Missing credential reference.")?;
    let secret = crate::project_credentials::read_password(&reference)?;
    use std::io::Write;
    std::io::stdout()
        .write_all(secret.as_bytes())
        .map_err(|_| "Could not supply password.".to_string())
}

fn password_prompt_matches(prompt: &str, expected: &str) -> bool {
    !expected.is_empty() && prompt.trim() == expected
}

pub fn upload(
    key: &str,
    alias: &str,
    username: Option<&str>,
    password_ref: Option<&str>,
    config: Option<&Path>,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    run_upload(
        upload_command(key)?,
        alias,
        username,
        password_ref,
        config,
        cancelled,
        None,
    )
}

pub fn upload_with_auth(
    key: &str,
    alias: &str,
    auth: &HostAuth,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    run_upload(
        upload_command(key)?,
        alias,
        None,
        None,
        None,
        cancelled,
        Some(auth),
    )
}

#[cfg(test)]
pub fn upload_fixture(key: &str, config: &Path, remote_home: &Path) -> Result<String, String> {
    let script = format!(
        "HOME={}; {}",
        shell_words::quote(&remote_home.to_string_lossy()),
        upload_command(key)?
    );
    run_upload(
        script,
        "lazyterm-fixture",
        None,
        None,
        Some(config),
        &AtomicBool::new(false),
        None,
    )
}

fn run_upload(
    script: String,
    alias: &str,
    username: Option<&str>,
    password_ref: Option<&str>,
    config: Option<&Path>,
    cancelled: &AtomicBool,
    auth: Option<&HostAuth>,
) -> Result<String, String> {
    if alias.is_empty() || alias.starts_with('-') || alias.chars().any(char::is_control) {
        return Err("Invalid Host alias.".into());
    }
    let mut command = Command::new("ssh");
    if let Some(username) = username {
        if username.is_empty()
            || username.starts_with('-')
            || username.chars().any(char::is_whitespace)
            || username.chars().any(char::is_control)
        {
            return Err("Invalid SSH username.".into());
        }
        command.arg("-l").arg(username);
    }
    if let Some(config) = config {
        command.arg("-F").arg(config);
    }
    command.args([
        "-T",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=2",
    ]);
    let trace = if let Some(auth) = auth {
        configure_host_auth(&mut command, auth, alias)
            .map_err(|error| format!("Could not prepare SSH authentication: {error}"))?
    } else if let Some(reference) = password_ref {
        command.args(["-o", "BatchMode=no"]);
        configure_password_for(&mut command, reference, alias, username)
            .map_err(|_| "Could not prepare password authentication.")?;
        None
    } else {
        command.args(["-o", "BatchMode=yes"]);
        None
    };
    command
        .arg(alias)
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Could not start OpenSSH.".to_string())?;
    // Drain diagnostics while waiting; a noisy remote cannot block the worker on a full pipe.
    let stderr = child.stderr.take();
    let (diagnostic_sender, diagnostic_receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        if let Some(stderr) = stderr {
            let stderr = crate::authentication::Reader::new(stderr, trace);
            let diagnostic = crate::diagnostics::collect(stderr).unwrap_or_default();
            let _ = diagnostic_sender.send(diagnostic.trim().to_string());
        }
    });
    let deadline =
        std::time::Instant::now() + Duration::from_secs(if auth.is_some() { 210 } else { 45 });
    let result = loop {
        if cancelled.load(Ordering::Relaxed) {
            stop_upload(&mut child);
            break Err("Public key upload cancelled.".into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                break Ok(format!(
                    "Public key installed on {alias} (~/.ssh/authorized_keys)."
                ));
            }
            Ok(Some(status)) => {
                let detail = diagnostic_receiver
                    .recv_timeout(Duration::from_millis(100))
                    .ok()
                    .filter(|value| !value.is_empty());
                break Err(detail.map_or_else(
                    || format!("Public key upload failed ({status}); check SSH authentication, known_hosts and remote ~/.ssh permissions."),
                    |detail| format!("Public key upload failed: {detail}"),
                ));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                stop_upload(&mut child);
                break Err("Public key upload timed out or SSH stopped.".into());
            }
        }
    };
    // SSH may leave ProxyCommand descendants holding stderr; do not block on their pipes.
    drop(reader);
    result
}

fn stop_upload(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_prompt_is_scoped_to_the_resolved_target() {
        let expected = "root@server.example's password:";
        assert!(password_prompt_matches(
            "root@server.example's password: ",
            expected
        ));
        for prompt in [
            "root@bastion's password:",
            "Password:",
            "Enter passphrase for key:",
            "Are you sure you want to continue connecting (yes/no)?",
        ] {
            assert!(!password_prompt_matches(prompt, expected));
        }
    }

    #[test]
    fn host_auth_adds_username_without_exposing_secret_material() {
        let auth = HostAuth {
            username: Some("deploy".into()),
            password_ref: None,
            broker: None,
            config: None,
            askpass: None,
        };
        let mut command = Command::new("ssh");
        configure_host_auth(&mut command, &auth, "web").unwrap();
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, ["-l", "deploy", "-o", "BatchMode=yes"]);
        assert!(
            command
                .get_envs()
                .all(|(name, _)| name != std::ffi::OsStr::new("LAZYTERM_ASKPASS_REFERENCE"))
        );
    }

    #[test]
    fn generate_import_and_refuse_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "lazyterm-key-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let key = generate_key(&root, uuid::Uuid::from_u128(1), "test".into()).unwrap();
        assert!(key.fingerprint.starts_with("SHA256:"));
        assert!(key.public_key.starts_with("ssh-ed25519 "));
        let original = fs::read(key.private_key_path.as_ref().unwrap()).unwrap();
        assert!(generate_key(&root, uuid::Uuid::from_u128(2), "replace".into()).is_err());
        assert_eq!(
            fs::read(key.private_key_path.as_ref().unwrap()).unwrap(),
            original
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(key.private_key_path.as_ref().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let other = generate_key(
            &root.join("other"),
            uuid::Uuid::from_u128(2),
            "other".into(),
        )
        .unwrap();
        fs::write(root.join("id_ed25519.pub"), other.public_key).unwrap();
        assert!(
            import_key(
                key.private_key_path.as_ref().unwrap(),
                uuid::Uuid::from_u128(3),
                "mismatch".into()
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_tracks_added_removed_and_unpaired_keys() {
        let root = std::env::temp_dir().join(format!(
            "lazyterm-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(discover_keys_in(&root).is_empty());

        let key_directory = root.join("first");
        let first = generate_key(&key_directory, uuid::Uuid::from_u128(1), "first".into()).unwrap();
        let first_private = first.private_key_path.unwrap();
        fs::copy(
            first_private.with_extension("pub"),
            key_directory.join("orphan.pub"),
        )
        .unwrap();
        fs::write(key_directory.join("first-cert.pub"), &first.public_key).unwrap();

        let found = discover_keys_in(&key_directory);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "id_ed25519");
        assert_eq!(found[0].fingerprint, first.fingerprint);
        assert_eq!(discover_keys_in(&key_directory).len(), 1);

        fs::remove_file(first_private).unwrap();
        assert!(discover_keys_in(&key_directory).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_is_quoted_idempotent_and_preserves_existing_keys() {
        let root = std::env::temp_dir().join(format!("lazyterm-install-{}", std::process::id()));
        fs::create_dir_all(root.join(".ssh")).unwrap();
        let file = root.join(".ssh/authorized_keys");
        fs::write(&file, "ssh-ed25519 AAAA existing").unwrap();
        let key = "ssh-ed25519 BBBB comment ' $(touch forbidden)";
        let script = upload_command(key).unwrap();
        for _ in 0..2 {
            assert!(
                Command::new("sh")
                    .args(["-c", &script])
                    .env("HOME", &root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let contents = fs::read_to_string(&file).unwrap();
        assert!(contents.starts_with("ssh-ed25519 AAAA existing\n"));
        assert_eq!(contents.matches("BBBB").count(), 1);
        assert!(upload_command("ssh-ed25519 AAAA\nmalicious").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
