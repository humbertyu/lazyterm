use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Host {
    pub alias: String,
}

impl Host {
    pub fn destination(&self) -> &str {
        &self.alias
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
            Ok(()) => {
                catalog.hosts = aliases.into_iter().map(|alias| Host { alias }).collect();
            }
            Err(error) => catalog.error = Some(format!("Could not read SSH config: {error}")),
        }
        catalog
    }
}

fn scan_file(
    path: &Path,
    ssh_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    seen_aliases: &mut HashSet<String>,
    aliases: &mut Vec<String>,
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
    for (alias, includes) in aliases_and_includes(&contents) {
        if let Some(alias) = alias
            && seen_aliases.insert(alias.clone())
        {
            aliases.push(alias);
        }
        for include in includes {
            for included_path in expand_include(&include, ssh_dir) {
                scan_file(&included_path, ssh_dir, visited, seen_aliases, aliases)?;
            }
        }
    }
    Ok(())
}

fn aliases_and_includes(contents: &str) -> Vec<(Option<String>, Vec<String>)> {
    let mut entries = Vec::new();
    let mut in_host_block = false;
    for line in contents.lines() {
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
                in_host_block = true;
                entries.extend(
                    values
                        .iter()
                        .filter(|value| is_concrete_alias(value))
                        .map(|value| (Some(value.clone()), Vec::new())),
                );
            }
            "match" => in_host_block = false,
            "include" => entries.push((None, values.to_vec())),
            _ if in_host_block => {}
            _ => {}
        }
    }
    entries
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
        let _ = fs::remove_dir_all(root);

        assert_eq!(aliases, ["main", "worker"]);
    }
}
