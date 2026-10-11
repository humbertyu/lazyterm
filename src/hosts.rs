use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Host {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub managed: bool,
}

impl Host {
    pub fn destination(&self) -> &str {
        self.hostname.as_deref().unwrap_or(&self.alias)
    }
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
        let mut catalog = Self {
            ..Default::default()
        };

        if !config_path.exists() {
            return catalog;
        }

        let mut visited = HashSet::new();
        let mut seen_aliases = HashSet::new();
        let mut aliases = Vec::new();
        match scan_file(
            &config_path,
            &ssh_dir,
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
        if host_exists(&contents, &host.alias) && original_alias != Some(host.alias.as_str()) {
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

    pub fn delete_managed(&mut self, alias: &str) -> io::Result<()> {
        let path = Self::config_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let contents = fs::read_to_string(&path)?;
        let Some(range) = managed_block_range(&contents, alias) else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Host is not managed by LazyTerm",
            ));
        };
        let mut updated = contents;
        updated.replace_range(range, "");
        safe_write(&path, updated.as_bytes())?;
        *self = Self::load();
        Ok(())
    }
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let contents = fs::read_to_string(&canonical)?;
    for (host, includes) in hosts_and_includes(&contents) {
        if let Some(host) = host
            && seen_aliases.insert(host.alias.clone())
        {
            aliases.push(host);
        }
        for include in includes {
            for included_path in expand_include(&include, ssh_dir) {
                scan_file(&included_path, ssh_dir, visited, seen_aliases, aliases)?;
            }
        }
    }
    Ok(())
}

fn hosts_and_includes(contents: &str) -> Vec<(Option<Host>, Vec<String>)> {
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
        let Ok(parts) = shell_words::split(line) else {
            continue;
        };
        let Some((directive, values)) = parts.split_first() else {
            continue;
        };
        match directive.to_ascii_lowercase().as_str() {
            "host" => {
                flush_hosts(&mut current, &mut entries);
                if values.iter().all(|value| is_concrete_alias(value)) {
                    current = values
                        .iter()
                        .map(|alias| Host {
                            alias: alias.clone(),
                            hostname: None,
                            user: None,
                            port: None,
                            identity_file: None,
                            managed: managed_marker,
                        })
                        .collect();
                }
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
    entries
}

fn flush_hosts(current: &mut Vec<Host>, entries: &mut Vec<(Option<Host>, Vec<String>)>) {
    for host in current.drain(..) {
        entries.push((Some(host), Vec::new()));
    }
}

#[cfg(test)]
fn aliases_and_includes(contents: &str) -> Vec<(Option<String>, Vec<String>)> {
    hosts_and_includes(contents)
        .into_iter()
        .map(|(host, includes)| (host.map(|host| host.alias), includes))
        .collect()
}

fn host_exists(contents: &str, alias: &str) -> bool {
    hosts_and_includes(contents)
        .into_iter()
        .filter_map(|(host, _)| host)
        .any(|host| host.alias == alias)
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
        let parsed_aliases = host_line
            .strip_prefix("Host ")
            .map(shell_words::split)
            .and_then(Result::ok)
            .unwrap_or_default();
        if parsed_aliases.len() != 1 || parsed_aliases[0] != alias {
            offset += lines[index].len();
            continue;
        }
        let mut end = offset + lines[index].len() + lines[index + 1].len();
        let mut cursor = index + 2;
        while let Some(line) = lines.get(cursor) {
            if line.trim_start().starts_with("Host ") || line.trim_start().starts_with("Match ") {
                break;
            }
            end += line.len();
            cursor += 1;
        }
        return Some(offset..end);
    }
    None
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn safe_write(path: &Path, contents: &[u8]) -> io::Result<()> {
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

fn expand_include(include: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    if include.contains('%') || include.contains("${") {
        return Vec::new();
    }
    let expanded = if include == "~" {
        env::var_os("HOME").map(PathBuf::from)
    } else if let Some(suffix) = include.strip_prefix("~/") {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(suffix))
    } else {
        Some(PathBuf::from(include))
    };
    let Some(mut pattern) = expanded else {
        return Vec::new();
    };
    if !pattern.is_absolute() {
        pattern = ssh_dir.join(pattern);
    }
    let pattern = pattern.to_string_lossy().into_owned();
    let mut paths = if pattern.contains(['*', '?', '[']) {
        glob::glob(&pattern)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    } else {
        vec![PathBuf::from(pattern)]
    };
    paths.sort();
    paths
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
    fn recognizes_lazyterm_managed_host_fields() {
        let entries = hosts_and_includes(
            "# managed-by: lazyterm\nHost app\n  HostName app.example.com\n  User deploy\n  Port 2222\n  IdentityFile ~/.ssh/app\nHost api db\n  HostName shared.example.com\n",
        );
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
