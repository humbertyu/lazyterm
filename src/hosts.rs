use std::{
    collections::{BTreeMap, HashSet},
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Host {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
    pub managed: bool,
}

impl Host {
    pub fn destination(&self) -> &str {
        self.hostname.as_deref().unwrap_or(&self.alias)
    }
}

/// Inspect local OpenSSH candidates without connecting to the server.
pub fn ssh_key_sources(alias: &str) -> Vec<String> {
    let Ok(output) = Command::new("ssh").args(["-G", "--", alias]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let config = String::from_utf8_lossy(&output.stdout);
    let mut sources = configured_key_sources(&config, |value| {
        crate::credentials::expand_path(value).is_file()
    });
    let option = |name: &str| {
        config
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    if option("pubkeyauthentication ") != Some("no")
        && option("identitiesonly ") != Some("yes")
        && option("identityagent ") != Some("none")
    {
        let mut agent = Command::new("ssh-add");
        agent.arg("-l");
        if let Some(socket) = option("identityagent ")
            && socket != "SSH_AUTH_SOCK"
        {
            agent.env("SSH_AUTH_SOCK", crate::credentials::expand_path(socket));
        }
        if agent.output().is_ok_and(|output| output.status.success()) {
            sources.push("ssh-agent".into());
        }
    }
    sources
}

fn configured_key_sources(config: &str, exists: impl Fn(&str) -> bool) -> Vec<String> {
    if config.lines().any(|line| line == "pubkeyauthentication no") {
        return Vec::new();
    }
    config
        .lines()
        .filter_map(|line| line.strip_prefix("identityfile "))
        .map(str::trim)
        .filter(|value| *value != "none" && exists(value))
        .map(str::to_string)
        .collect()
}

pub fn ssh_config_user(alias: &str) -> Option<String> {
    let output = Command::new("ssh").args(["-G", alias]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("user "))
        .map(str::trim)
        .filter(|user| !user.is_empty())
        .map(str::to_string)
}

#[derive(Default)]
pub struct HostCatalog {
    pub hosts: Vec<Host>,
    pub error: Option<String>,
}

impl HostCatalog {
    pub fn load() -> Self {
        let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
            return Self {
                error: Some("Could not determine the home directory.".to_string()),
                ..Self::default()
            };
        };
        let ssh_dir = home.join(".ssh");
        let config_path = ssh_dir.join("config");
        Self::load_from(&config_path, &ssh_dir)
    }

    pub(crate) fn load_from(config_path: &Path, ssh_dir: &Path) -> Self {
        let mut catalog = Self {
            ..Default::default()
        };

        let mut visited = HashSet::new();
        let mut seen_aliases = HashSet::new();
        let mut aliases = Vec::new();
        match scan_file(
            config_path,
            ssh_dir,
            &mut visited,
            &mut seen_aliases,
            &mut aliases,
        ) {
            Ok(()) => catalog.hosts = aliases,
            Err(error) => catalog.error = Some(format!("Could not read SSH config: {error}")),
        }
        catalog
    }

    pub fn config_path() -> Option<PathBuf> {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".ssh/config"))
    }

    pub fn config_snapshot() -> io::Result<BTreeMap<String, String>> {
        let Some(config_path) = Self::config_path() else {
            return Ok(BTreeMap::new());
        };
        let Some(ssh_dir) = config_path.parent() else {
            return Ok(BTreeMap::new());
        };
        let mut visited = HashSet::new();
        let mut files = BTreeMap::new();
        snapshot_file(&config_path, ssh_dir, &mut visited, &mut files)?;
        Ok(files)
    }

    pub fn upsert_managed_from(
        &mut self,
        host: &Host,
        original_alias: Option<&str>,
    ) -> io::Result<()> {
        let path = Self::config_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let mut contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        if let Some(original_alias) = original_alias
            && managed_block_range(&contents, original_alias).is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original host is no longer managed by LazyTerm",
            ));
        }
        if let Some(original_alias) = original_alias
            && original_alias != host.alias
        {
            let Some(range) = managed_block_range(&contents, original_alias) else {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Original host is not managed by LazyTerm",
                ));
            };
            contents.replace_range(range, "");
        }
        if host_exists(&contents, &host.alias)? && original_alias != Some(host.alias.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "That SSH alias already exists and is not managed by LazyTerm",
            ));
        }
        let block = format_host_block(host);
        if let Some(range) = managed_block_range(&contents, &host.alias) {
            contents.replace_range(range, &block);
        } else {
            if !contents.is_empty() && !contents.ends_with('\n') {
                contents.push('\n');
            }
            if !contents.is_empty() {
                contents.push('\n');
            }
            contents.push_str(&block);
        }
        safe_write(&path, contents.as_bytes())?;
        *self = Self::load();
        Ok(())
    }

    pub fn delete_host(&mut self, alias: &str) -> io::Result<()> {
        let files = Self::config_snapshot()?;
        let mut changes = Vec::new();
        for (path, contents) in files {
            if let Some(updated) = delete_host_alias(&contents, alias)? {
                changes.push((PathBuf::from(path), updated));
            }
        }
        if changes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Host alias was not found in SSH config",
            ));
        }
        for (path, contents) in changes {
            safe_write(&path, contents.as_bytes())?;
        }
        *self = Self::load();
        Ok(())
    }

    pub fn set_identity(&mut self, alias: &str, path: &Path) -> io::Result<()> {
        let path_text = path
            .to_str()
            .ok_or_else(|| io::Error::other("Key path is not valid UTF-8"))?;
        if path_text.chars().any(char::is_control) {
            return Err(io::Error::other("Invalid key path"));
        }
        let mut changes = Vec::new();
        for (file, contents) in Self::config_snapshot()? {
            if let Some(updated) = replace_identity(&contents, alias, path_text)? {
                changes.push((file, updated));
            }
        }
        if changes.len() > 1 {
            return Err(io::Error::other(
                "Multiple Host blocks define this alias; edit its IdentityFile in SSH config explicitly.",
            ));
        }
        if let Some((file, updated)) = changes.pop() {
            safe_write(Path::new(&file), updated.as_bytes())?;
            *self = Self::load();
            return Ok(());
        }
        Err(io::Error::other("Host was not found in SSH config"))
    }
}

fn replace_identity(contents: &str, alias: &str, path: &str) -> io::Result<Option<String>> {
    let lines = contents.split_inclusive('\n').collect::<Vec<_>>();
    let mut start = None;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate() {
        let line = strip_comment(line).trim();
        let boundary = line
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(line.len());
        let keyword = &line[..boundary];
        let values = line[boundary..]
            .trim_start()
            .strip_prefix('=')
            .unwrap_or(line[boundary..].trim_start())
            .trim_start();
        let tokens = shell_words::split(values).map_err(io::Error::other)?;
        if keyword.eq_ignore_ascii_case("Host") || keyword.eq_ignore_ascii_case("Match") {
            if start.is_some() && end == lines.len() {
                end = index;
            }
            if keyword.eq_ignore_ascii_case("Host") && tokens.iter().any(|value| value == alias) {
                if start.is_some() {
                    return Err(io::Error::other(
                        "Multiple Host blocks define this alias; edit its IdentityFile in SSH config explicitly.",
                    ));
                }
                if tokens.len() != 1 {
                    return Err(io::Error::other(
                        "This Host block shares multiple aliases; split it into individual blocks before assigning a key.",
                    ));
                }
                start = Some(index);
            }
        }
    }
    let Some(start) = start else {
        return Ok(None);
    };
    let escaped = path
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let mut result = lines[..=start].concat();
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(&format!("    IdentityFile \"{escaped}\"\n"));
    for line in &lines[start + 1..end] {
        let keyword = line
            .trim_start()
            .split([' ', '\t', '='])
            .next()
            .unwrap_or("");
        if !keyword.eq_ignore_ascii_case("IdentityFile") {
            result.push_str(line);
        }
    }
    result.push_str(&lines[end..].concat());
    Ok(Some(result))
}

fn snapshot_file(
    path: &Path,
    ssh_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    files: &mut BTreeMap<String, String>,
) -> io::Result<()> {
    let canonical = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && fs::symlink_metadata(path).is_err() =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let contents = fs::read_to_string(&canonical)?;
    files.insert(canonical.display().to_string(), contents.clone());
    for (_, includes) in hosts_and_includes(&contents)? {
        for include in includes {
            for included_path in expand_include(&include, ssh_dir)? {
                snapshot_file(&included_path, ssh_dir, visited, files)?;
            }
        }
    }
    Ok(())
}

fn scan_file(
    path: &Path,
    ssh_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    seen_aliases: &mut HashSet<String>,
    aliases: &mut Vec<Host>,
) -> std::io::Result<()> {
    let canonical = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && fs::symlink_metadata(path).is_err() =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let contents = fs::read_to_string(&canonical)?;
    for (host, includes) in hosts_and_includes(&contents)? {
        if let Some(host) = host
            && seen_aliases.insert(host.alias.clone())
        {
            aliases.push(host);
        }
        for include in includes {
            for included_path in expand_include(&include, ssh_dir)? {
                scan_file(&included_path, ssh_dir, visited, seen_aliases, aliases)?;
            }
        }
    }
    Ok(())
}

type ConfigEntries = Vec<(Option<Host>, Vec<String>)>;

fn hosts_and_includes(contents: &str) -> io::Result<ConfigEntries> {
    let mut entries = Vec::new();
    let mut current: Vec<Host> = Vec::new();
    let mut managed_marker = false;
    for raw_line in contents.lines() {
        if raw_line.trim() == "# managed-by: lazyterm" {
            managed_marker = true;
        }
        let line = raw_line;
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let directive_end = line
            .find(|character: char| character.is_whitespace() || character == '=')
            .unwrap_or(line.len());
        let directive = &line[..directive_end];
        let values = line[directive_end..].trim_start();
        let values = values.strip_prefix('=').unwrap_or(values).trim_start();
        let values = shell_words::split(values)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if values.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Missing value for {directive}"),
            ));
        }
        match directive.to_ascii_lowercase().as_str() {
            "host" => {
                flush_hosts(&mut current, &mut entries);
                current = values
                    .iter()
                    .filter(|alias| is_concrete_alias(alias))
                    .map(|alias| Host {
                        alias: alias.clone(),
                        hostname: None,
                        user: None,
                        port: None,
                        identity_file: None,
                        proxy_jump: None,
                        managed: managed_marker,
                    })
                    .collect();
                managed_marker = false;
            }
            "hostname" if !current.is_empty() => {
                for host in &mut current {
                    host.hostname = values.first().cloned();
                }
            }
            "user" if !current.is_empty() => {
                for host in &mut current {
                    host.user = values.first().cloned();
                }
            }
            "port" if !current.is_empty() => {
                for host in &mut current {
                    host.port = values.first().and_then(|value| value.parse().ok());
                }
            }
            "identityfile" if !current.is_empty() => {
                for host in &mut current {
                    host.identity_file = values.first().cloned();
                }
            }
            "proxyjump" if !current.is_empty() => {
                for host in &mut current {
                    host.proxy_jump = values.first().cloned();
                }
            }
            "match" => {
                flush_hosts(&mut current, &mut entries);
            }
            "include" => {
                flush_hosts(&mut current, &mut entries);
                entries.push((None, values.to_vec()));
            }
            _ => {}
        }
    }
    flush_hosts(&mut current, &mut entries);
    Ok(entries)
}

fn flush_hosts(current: &mut Vec<Host>, entries: &mut Vec<(Option<Host>, Vec<String>)>) {
    for host in current.drain(..) {
        entries.push((Some(host), Vec::new()));
    }
}

#[cfg(test)]
fn aliases_and_includes(contents: &str) -> Vec<(Option<String>, Vec<String>)> {
    hosts_and_includes(contents)
        .expect("valid test config")
        .into_iter()
        .map(|(host, includes)| (host.map(|host| host.alias), includes))
        .collect()
}

fn host_exists(contents: &str, alias: &str) -> io::Result<bool> {
    Ok(hosts_and_includes(contents)?
        .into_iter()
        .filter_map(|(host, _)| host)
        .any(|host| host.alias == alias))
}

fn format_host_block(host: &Host) -> String {
    let mut lines = vec![
        "# managed-by: lazyterm".to_string(),
        format!("Host {}", host.alias),
    ];
    if let Some(value) = &host.hostname {
        lines.push(format!("    HostName {value}"));
    }
    if let Some(value) = &host.user {
        lines.push(format!("    User {value}"));
    }
    if let Some(value) = host.port {
        lines.push(format!("    Port {value}"));
    }
    if let Some(value) = &host.identity_file {
        lines.push(format!("    IdentityFile {value}"));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn managed_block_range(contents: &str, alias: &str) -> Option<std::ops::Range<usize>> {
    let lines = contents.split_inclusive('\n').collect::<Vec<_>>();
    let mut offset = 0;
    for index in 0..lines.len() {
        let line = lines[index].trim();
        if line != "# managed-by: lazyterm" {
            offset += lines[index].len();
            continue;
        }
        let Some(host_line) = lines.get(index + 1).map(|line| line.trim()) else {
            break;
        };
        let parts = shell_words::split(strip_comment(host_line)).ok()?;
        let parsed_aliases = match parts.split_first() {
            Some((directive, aliases)) if directive.eq_ignore_ascii_case("host") => aliases,
            _ => {
                offset += lines[index].len();
                continue;
            }
        };
        if parsed_aliases.len() != 1 || parsed_aliases[0] != alias {
            offset += lines[index].len();
            continue;
        }
        let mut end = offset + lines[index].len() + lines[index + 1].len();
        let mut cursor = index + 2;
        while let Some(line) = lines.get(cursor) {
            let directive = line
                .trim_start()
                .split([' ', '\t', '='])
                .next()
                .unwrap_or("");
            if line.trim() == "# managed-by: lazyterm"
                || directive.eq_ignore_ascii_case("host")
                || directive.eq_ignore_ascii_case("match")
            {
                break;
            }
            end += line.len();
            cursor += 1;
        }
        return Some(offset..end);
    }
    None
}

fn delete_host_alias(contents: &str, alias: &str) -> io::Result<Option<String>> {
    let lines = contents.split_inclusive('\n').collect::<Vec<_>>();
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    offsets.push(0);
    for line in &lines {
        let next = offsets.last().copied().unwrap_or_default() + line.len();
        offsets.push(next);
    }

    let mut edits = Vec::<(std::ops::Range<usize>, String)>::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((directive, aliases)) = parse_host_directive(line)? else {
            continue;
        };
        if !directive.eq_ignore_ascii_case("Host") || !aliases.iter().any(|item| item == alias) {
            continue;
        }

        if aliases.len() > 1 {
            let remaining = aliases
                .into_iter()
                .filter(|item| item != alias)
                .map(|item| shell_words::quote(&item).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let raw = line;
            let (line_content, newline) = if let Some(value) = raw.strip_suffix("\r\n") {
                (value, "\r\n")
            } else if let Some(value) = raw.strip_suffix('\n') {
                (value, "\n")
            } else {
                (*raw, "")
            };
            let comment_start = strip_comment(line_content).len();
            let comment = &line_content[comment_start..];
            let indent = line_content.len() - line_content.trim_start().len();
            let replacement = format!(
                "{}Host {}{}{}",
                &line_content[..indent],
                remaining,
                comment,
                newline
            );
            edits.push((offsets[index]..offsets[index + 1], replacement));
            continue;
        }

        let mut start_index = index;
        if index > 0 && lines[index - 1].trim() == "# managed-by: lazyterm" {
            start_index -= 1;
        }
        let end_index = lines
            .iter()
            .enumerate()
            .skip(index + 1)
            .find_map(|(next_index, next_line)| {
                let (directive, _) = parse_host_directive(next_line).ok().flatten()?;
                (directive.eq_ignore_ascii_case("Host") || directive.eq_ignore_ascii_case("Match"))
                    .then_some(next_index)
            })
            .unwrap_or(lines.len());
        edits.push((offsets[start_index]..offsets[end_index], String::new()));
    }

    if edits.is_empty() {
        return Ok(None);
    }
    let mut updated = contents.to_string();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in edits {
        updated.replace_range(range, &replacement);
    }
    Ok(Some(updated))
}

fn parse_host_directive(line: &str) -> io::Result<Option<(String, Vec<String>)>> {
    let line = strip_comment(line).trim();
    let Some(end) = line.find(|character: char| character.is_whitespace() || character == '=')
    else {
        return Ok(None);
    };
    let directive = line[..end].to_string();
    if !directive.eq_ignore_ascii_case("Host") && !directive.eq_ignore_ascii_case("Match") {
        return Ok(None);
    }
    let values = line[end..]
        .trim_start()
        .strip_prefix('=')
        .unwrap_or(line[end..].trim_start())
        .trim_start();
    Ok(Some((
        directive,
        shell_words::split(values).map_err(io::Error::other)?,
    )))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn safe_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Refusing to replace a symbolic-link SSH config",
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let existing = fs::metadata(path).ok();
    let backup = path.with_extension("config.lazyterm.bak");
    if path.exists() {
        fs::copy(path, &backup)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(metadata) = &existing {
                fs::set_permissions(
                    &backup,
                    fs::Permissions::from_mode(metadata.permissions().mode()),
                )?;
            }
        }
    }

    let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".lazyterm-config-{}-{suffix}.tmp",
        std::process::id()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    #[cfg(unix)]
    if let Some(metadata) = &existing {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))?;
    }
    use io::Write;
    if let Err(error) = file.write_all(contents).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn expand_include(include: &str, ssh_dir: &Path) -> io::Result<Vec<PathBuf>> {
    if include.contains('%') || include.contains("${") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Cannot fully resolve SSH Include expansion",
        ));
    }
    let expanded = if include == "~" {
        env::var_os("HOME").map(PathBuf::from)
    } else if let Some(suffix) = include.strip_prefix("~/") {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(suffix))
    } else {
        Some(PathBuf::from(include))
    };
    let Some(mut pattern) = expanded else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "HOME is not set"));
    };
    if !pattern.is_absolute() {
        pattern = ssh_dir.join(pattern);
    }
    let pattern = pattern.to_string_lossy().into_owned();
    let mut paths = if pattern.contains(['*', '?', '[']) {
        glob::glob(&pattern)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            .map(|entry| entry.map_err(io::Error::other))
            .collect::<io::Result<Vec<_>>>()?
    } else {
        vec![PathBuf::from(pattern)]
    };
    paths.sort();
    Ok(paths)
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

fn is_concrete_alias(alias: &str) -> bool {
    !alias.is_empty() && !alias.starts_with('!') && !alias.contains('*') && !alias.contains('?')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_key_candidates_include_existing_default_and_custom_files_only() {
        let config = "pubkeyauthentication yes\nidentityfile ~/.ssh/custom key\nidentityfile ~/.ssh/id_ed25519\nidentityfile ~/.ssh/missing\nidentityfile none\n";
        assert_eq!(
            configured_key_sources(config, |path| {
                matches!(path, "~/.ssh/custom key" | "~/.ssh/id_ed25519")
            }),
            ["~/.ssh/custom key", "~/.ssh/id_ed25519"]
        );
        assert!(
            configured_key_sources(
                "pubkeyauthentication no\nidentityfile ~/.ssh/id_ed25519\n",
                |_| true
            )
            .is_empty()
        );
        assert!(configured_key_sources("identityfile none\n", |_| true).is_empty());
    }

    #[test]
    fn ssh_key_candidates_resolve_includes_and_wildcard_configuration() {
        let fixture = crate::projects::tests::Fixture::new("effective-host-keys");
        let config = fixture.root.join("config");
        let included = fixture.root.join("keys.conf");
        let key = fixture.root.join("external-key");
        fs::write(&key, "fixture").unwrap();
        fs::write(
            &included,
            format!("Host web*\n  IdentityFile {}\n", key.display()),
        )
        .unwrap();
        fs::write(
            &config,
            format!(
                "Include {}\nHost web\n  HostName example.invalid\n",
                included.display()
            ),
        )
        .unwrap();
        let output = Command::new("ssh")
            .arg("-G")
            .arg("-F")
            .arg(config)
            .arg("web")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            configured_key_sources(&String::from_utf8_lossy(&output.stdout), |path| {
                Path::new(path).is_file()
            }),
            [key.display().to_string()]
        );
    }

    #[test]
    fn assigning_identity_preserves_routing_comments_and_neighboring_blocks() {
        let contents = "# keep\nHost=web # comment\n  HostName example.com\n  IdentityFile old\n  ProxyJump bastion\n  # custom\n  ServerAliveInterval 30\nHost next\n  IdentityFile next-key\n";
        let updated = replace_identity(contents, "web", "/keys/a b%key")
            .unwrap()
            .unwrap();
        assert!(updated.contains("Host=web # comment\n    IdentityFile \"/keys/a b%%key\"\n"));
        assert!(!updated.contains("IdentityFile old"));
        assert!(updated.contains("  ProxyJump bastion\n  # custom\n  ServerAliveInterval 30\nHost next\n  IdentityFile next-key\n"));
        assert!(replace_identity("Host one two\n  User deploy\n", "one", "/key").is_err());
        assert!(
            replace_identity(
                "Host one\n  User deploy\nHost one\n  User root\n",
                "one",
                "/key"
            )
            .is_err()
        );
        assert!(
            replace_identity(contents, "missing", "/key")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn managed_block_changes_preserve_the_next_marker_and_case_insensitive_boundary() {
        let contents = "# keep\n# managed-by: lazyterm\nHost first\n    User deploy\n\n# managed-by: lazyterm\nHost second\n    User root\nhost\tthird\n    User ops\nmatch all\n";
        let first = managed_block_range(contents, "first").unwrap();
        let mut deleted = contents.to_string();
        deleted.replace_range(first, "");
        assert!(deleted.starts_with("# keep\n# managed-by: lazyterm\nHost second\n"));
        assert!(managed_block_range(&deleted, "second").is_some());
        let second = managed_block_range(&deleted, "second").unwrap();
        assert!(!deleted[second.clone()].contains("third"));
        deleted.replace_range(second, "");
        assert_eq!(deleted, "# keep\nhost\tthird\n    User ops\nmatch all\n");
    }

    #[test]
    fn deleting_host_alias_preserves_shared_aliases_and_neighboring_stanzas() {
        let contents = "# global\nHost shared api\n  User deploy\n\nHost remove-me\n  HostName old.example\n  IdentityFile ~/.ssh/old\n\nHost keep\n  User ops\n";
        let updated = delete_host_alias(contents, "shared").unwrap().unwrap();
        assert!(updated.contains("# global\nHost api\n  User deploy"));
        assert!(!updated.contains("Host shared api"));
        assert!(updated.contains("Host remove-me\n  HostName old.example"));
        assert!(
            delete_host_alias(&updated, "remove-me")
                .unwrap()
                .unwrap()
                .contains("Host keep\n  User ops")
        );
        assert!(delete_host_alias(contents, "missing").unwrap().is_none());
    }

    #[test]
    fn deleting_managed_host_removes_its_marker_but_keeps_adjacent_blocks() {
        let contents =
            "# managed-by: lazyterm\nHost managed\n  User app\n\nHost manual\n  User deploy\n";
        let updated = delete_host_alias(contents, "managed").unwrap().unwrap();
        assert_eq!(updated, "Host manual\n  User deploy\n");
    }

    #[cfg(unix)]
    #[test]
    fn new_config_is_private_and_updates_keep_permissions_and_backup() {
        use std::os::unix::fs::PermissionsExt;
        let root = env::temp_dir().join(format!("lazyterm-permissions-{}", std::process::id()));
        let path = root.join("config");
        safe_write(&path, b"Host original\n").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        safe_write(&path, b"Host changed\n").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::read(path.with_extension("config.lazyterm.bak")).unwrap(),
            b"Host original\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_concrete_aliases_and_ignores_patterns_and_match_blocks() {
        let aliases = aliases_and_includes(
            "Host *\n  User deploy\nHost api db\nHost *.internal !excluded\nMatch user deploy\n  HostName ignored.example\n",
        )
        .into_iter()
            .filter_map(|(alias, _)| alias)
        .collect::<Vec<_>>();
        assert_eq!(aliases, ["api", "db"]);
    }

    #[test]
    fn parses_multiple_include_paths_and_quoted_comments() {
        let entries = aliases_and_includes(
            "Host \"stage#1\" # this is a comment\nInclude config.d/*.conf extra.conf\n",
        );
        assert_eq!(entries[0].0.as_deref(), Some("stage#1"));
        assert_eq!(
            entries[1].1,
            ["config.d/*.conf".to_string(), "extra.conf".to_string()]
        );
    }

    #[test]
    fn skips_wildcard_and_negated_aliases() {
        assert!(!is_concrete_alias("*.internal"));
        assert!(!is_concrete_alias("!staging"));
        assert!(is_concrete_alias("prod-web"));
    }

    #[test]
    fn parses_proxy_jump_for_host_details() {
        let entries =
            hosts_and_includes("Host app\n    HostName app.internal\n    ProxyJump bastion\n")
                .unwrap();
        assert_eq!(
            entries[0].0.as_ref().unwrap().proxy_jump.as_deref(),
            Some("bastion")
        );
    }

    #[test]
    fn parses_equals_syntax_and_concrete_aliases_alongside_patterns() {
        let hosts = hosts_and_includes(
            "Host=web *.internal !excluded\n  HostName = web.example.com\nHost db\n  User=deploy\n",
        )
        .unwrap()
        .into_iter()
        .filter_map(|(host, _)| host)
        .collect::<Vec<_>>();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "web");
        assert_eq!(hosts[0].hostname.as_deref(), Some("web.example.com"));
        assert_eq!(hosts[1].user.as_deref(), Some("deploy"));
    }

    #[test]
    fn recognizes_lazyterm_managed_host_fields() {
        let entries = hosts_and_includes(
            "# managed-by: lazyterm\nHost app\n  HostName app.example.com\n  User deploy\n  Port 2222\n  IdentityFile ~/.ssh/app\nHost api db\n  HostName shared.example.com\n",
        ).unwrap();
        let hosts = entries
            .into_iter()
            .filter_map(|(host, _)| host)
            .collect::<Vec<_>>();
        assert_eq!(hosts[0].alias, "app");
        assert!(hosts[0].managed);
        assert_eq!(hosts[0].hostname.as_deref(), Some("app.example.com"));
        assert_eq!(hosts[0].user.as_deref(), Some("deploy"));
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[0].identity_file.as_deref(), Some("~/.ssh/app"));
        assert_eq!(
            hosts
                .iter()
                .map(|host| host.alias.as_str())
                .collect::<Vec<_>>(),
            ["app", "api", "db"]
        );
    }

    #[test]
    fn scans_included_config_files() {
        let root = env::temp_dir().join(format!("lazyterm-config-{}", std::process::id()));
        let ssh_dir = root.join(".ssh");
        let include_dir = ssh_dir.join("config.d");
        fs::create_dir_all(&include_dir).expect("create fixture directory");
        let config = ssh_dir.join("config");
        fs::write(&config, "Host main\nInclude config.d/*.conf\n").expect("write root config");
        fs::write(include_dir.join("work.conf"), "Host worker\n").expect("write included config");

        let mut visited = HashSet::new();
        let mut seen = HashSet::new();
        let mut aliases = Vec::new();
        scan_file(&config, &ssh_dir, &mut visited, &mut seen, &mut aliases)
            .expect("scan config files");
        let names = aliases
            .into_iter()
            .map(|host| host.alias)
            .collect::<Vec<_>>();
        let _ = fs::remove_dir_all(root);

        assert_eq!(names, ["main", "worker"]);
    }
}
