use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

use crate::metadata::Metadata;

#[derive(Deserialize)]
struct ConfigurationArchive {
    ssh_config_path: Option<PathBuf>,
    #[serde(default)]
    ssh_config: String,
    #[serde(default)]
    ssh_config_files: BTreeMap<PathBuf, String>,
    metadata: Metadata,
    passwords_included: bool,
}

pub struct ConfigurationImport {
    pub metadata: Metadata,
    pub ssh_files: BTreeMap<PathBuf, Vec<u8>>,
    pub host_count: usize,
    pub ssh_root: PathBuf,
}

pub(crate) fn portable_ssh_snapshot(
    config_path: &Path,
    ssh_root: &Path,
) -> io::Result<BTreeMap<String, String>> {
    let snapshot = crate::hosts::HostCatalog::snapshot_from(config_path, ssh_root)?;
    let canonical_root = fs::canonicalize(ssh_root).unwrap_or_else(|_| ssh_root.to_path_buf());
    let mut portable = BTreeMap::new();
    for (path, contents) in snapshot {
        let path = PathBuf::from(path);
        if path.strip_prefix(&canonical_root).is_err() {
            continue;
        }
        let (contents, _) = strip_external_include_directives(&contents, ssh_root)?;
        portable.insert(path.display().to_string(), contents);
    }
    Ok(portable)
}

pub(crate) fn strip_external_include_directives(
    contents: &str,
    ssh_root: &Path,
) -> io::Result<(String, Vec<(usize, String)>)> {
    let canonical_root = fs::canonicalize(ssh_root).unwrap_or_else(|_| ssh_root.to_path_buf());
    let mut filtered = String::with_capacity(contents.len());
    let mut local_includes = Vec::new();
    for (line_index, line) in contents.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        let Some(end) =
            trimmed.find(|character: char| character.is_whitespace() || character == '=')
        else {
            filtered.push_str(line);
            continue;
        };
        if !trimmed[..end].eq_ignore_ascii_case("include") {
            filtered.push_str(line);
            continue;
        }
        let value = trimmed[end..]
            .trim_start()
            .trim_start_matches('=')
            .trim_start();
        let value = crate::hosts::strip_comment(value);
        let paths =
            shell_words::split(value).map_err(|_| invalid("Invalid SSH Include directive."))?;
        let mut external = false;
        for include in paths {
            for path in crate::hosts::expand_include(&include, ssh_root)? {
                let path = fs::canonicalize(&path).unwrap_or(path);
                if !path.starts_with(&canonical_root) {
                    external = true;
                    break;
                }
            }
            if external {
                break;
            }
        }
        if external {
            local_includes.push((line_index, line.trim_end_matches(['\r', '\n']).to_string()));
        } else {
            filtered.push_str(line);
        }
    }
    Ok((filtered, local_includes))
}

fn merge_metadata(current: Metadata, incoming: Metadata) -> io::Result<Metadata> {
    use serde_json::Value;
    fn merge(local: &mut Value, incoming: Value, field: &str) {
        match (local, incoming) {
            (Value::Object(local), Value::Object(incoming)) => {
                let keep_password_storage = field == "links"
                    && ["password", "password_ref"]
                        .iter()
                        .any(|key| local.get(*key).is_some_and(|value| !value.is_null()));
                for (key, value) in incoming {
                    let child_field =
                        if (field.is_empty() && key == "tags") || field == "tags_by_alias" {
                            "tags_by_alias"
                        } else {
                            &key
                        };
                    // Ordinary configuration import must not clear local secrets.
                    if matches!(key.as_str(), "password" | "password_ref")
                        && local.contains_key(&key)
                    {
                        continue;
                    }
                    if key == "password_storage" && keep_password_storage {
                        continue;
                    }
                    merge(
                        local.entry(key.clone()).or_insert(Value::Null),
                        value,
                        child_field,
                    );
                }
            }
            (Value::Array(local), Value::Array(incoming))
                if matches!(
                    field,
                    "projects"
                        | "commands"
                        | "links"
                        | "snippet_packages"
                        | "snippets"
                        | "tunnels"
                        | "credentials"
                ) =>
            {
                let key = if field == "tunnels" { "label" } else { "name" };
                for mut item in incoming {
                    if let Some(existing) = local.iter_mut().find(|existing| {
                        let same_identity = existing["id"].is_string()
                            && item["id"].is_string()
                            && existing["id"] == item["id"];
                        same_identity
                            || (existing[key] == item[key]
                                && (field != "credentials" || existing["kind"] == item["kind"]))
                    }) {
                        item["id"] = existing["id"].clone();
                        merge(existing, item, field);
                    } else {
                        local.push(item);
                    }
                }
            }
            (Value::Array(local), Value::Array(incoming)) if field != "hidden_tabs" => {
                if field == "tags_by_alias" {
                    *local = incoming;
                    return;
                }
                for value in incoming {
                    if !local.contains(&value) {
                        local.push(value);
                    }
                }
            }
            (local, incoming) => *local = incoming,
        }
    }
    let mut document = serde_json::to_value(&current)?;
    let mut incoming = serde_json::to_value(incoming)?;
    let source = incoming.clone();
    // Merge credentials first so Host references use the destination's IDs.
    let credentials = incoming["credentials"].take();
    let source_credentials = credentials.clone();
    merge(&mut document["credentials"], credentials, "credentials");
    if let Some(associations) = incoming["host_passwords"].as_object_mut() {
        for id in associations.values_mut() {
            if let Some(source) = source_credentials
                .as_array()
                .and_then(|items| items.iter().find(|item| item["id"] == *id))
                && let Some(destination) = document["credentials"].as_array().and_then(|items| {
                    items.iter().find(|item| {
                        item["id"] == source["id"]
                            || (item["name"] == source["name"] && item["kind"] == source["kind"])
                    })
                })
            {
                *id = destination["id"].clone();
            }
        }
    }
    incoming.as_object_mut().unwrap().remove("credentials");
    merge(&mut document, incoming, "");
    let mut targets = BTreeMap::new();
    if let Some(projects) = source["projects"].as_array() {
        for project in projects {
            let destination = document["projects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == project["id"] || item["name"] == project["name"])
                .unwrap();
            for link in project["links"].as_array().into_iter().flatten() {
                let target = destination["links"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["id"] == link["id"] || item["name"] == link["name"])
                    .unwrap();
                let source_ref = format!(
                    "config:{}:{}",
                    project["id"].as_str().unwrap_or_default(),
                    link["id"].as_str().unwrap_or_default()
                );
                targets.insert(
                    source_ref,
                    format!(
                        "config:{}:{}",
                        destination["id"].as_str().unwrap_or_default(),
                        target["id"].as_str().unwrap_or_default()
                    ),
                );
                if let Some(reference) = link["password_ref"].as_str() {
                    let target_ref = target["password_ref"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            format!(
                                "config:{}:{}",
                                destination["id"].as_str().unwrap_or_default(),
                                target["id"].as_str().unwrap_or_default()
                            )
                        });
                    targets.insert(reference.into(), target_ref);
                }
            }
        }
    }
    for credential in source["credentials"].as_array().into_iter().flatten() {
        if let Some(reference) = credential["password_ref"].as_str() {
            let target = document["credentials"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| {
                    item["id"] == credential["id"]
                        || (item["name"] == credential["name"]
                            && item["kind"] == credential["kind"])
                })
                .unwrap();
            if let Some(target_ref) = target["password_ref"].as_str() {
                targets.insert(reference.into(), target_ref.into());
            }
        }
    }
    current.merged_document(document, targets)
}

// Keep complete SSH stanza bodies, including comments and unsupported directives.
// An incoming alias replaces its old stanza; aliases absent from the archive survive.
type SshSections = Vec<(Option<Vec<String>>, String)>;

pub(crate) fn ssh_sections(text: &str) -> io::Result<SshSections> {
    let mut result = vec![(None, String::new())];
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let end = trimmed
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(trimmed.len());
        let keyword = &trimmed[..end];
        if keyword.eq_ignore_ascii_case("host") || keyword.eq_ignore_ascii_case("match") {
            let values = trimmed[end..]
                .trim_start()
                .trim_start_matches('=')
                .trim_start();
            let values = shell_words::split(crate::hosts::strip_comment(values))
                .map_err(io::Error::other)?;
            let aliases = keyword.eq_ignore_ascii_case("host").then_some(values);
            result.push((aliases, String::new()));
        }
        result.last_mut().unwrap().1.push_str(line);
    }
    Ok(result)
}

fn merge_ssh_config(
    local: &str,
    incoming: &str,
    aliases: &std::collections::HashSet<String>,
) -> io::Result<String> {
    let incoming_sections = ssh_sections(incoming)?;
    let mut output = incoming.trim_end().to_string();
    for (index, (local_aliases, body)) in ssh_sections(local)?.into_iter().enumerate() {
        if body.trim().is_empty()
            || incoming_sections
                .iter()
                .any(|(_, text)| text.trim() == body.trim())
        {
            continue;
        }
        let retained = match local_aliases {
            Some(names) => {
                let remaining = names
                    .iter()
                    .filter(|name| !aliases.contains(name.as_str()))
                    .collect::<Vec<_>>();
                if remaining.is_empty() {
                    continue;
                }
                if remaining.len() == names.len() {
                    body
                } else {
                    let rest = body.split_once('\n').map_or("", |(_, rest)| rest);
                    format!("Host {}\n{rest}", shell_words::join(remaining))
                }
            }
            None if index == 0 && !output.is_empty() => {
                // A preamble appended after a Host/Match needs a global scope reset.
                format!("Host *\n{body}")
            }
            None => body,
        };
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(retained.trim_end());
        output.push('\n');
    }
    if !output.ends_with('\n') && !output.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

pub fn parse_configuration(bytes: &[u8], home: &Path) -> io::Result<ConfigurationImport> {
    let mut document: serde_json::Value = serde_json::from_slice(bytes)?;
    let format_version = document["format_version"]
        .as_u64()
        .ok_or_else(|| invalid("Invalid configuration export version."))?;
    if format_version != 1 {
        return Err(invalid("Unsupported configuration export version."));
    }
    let metadata = document
        .get_mut("metadata")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| invalid("Configuration archive is missing LazyTerm metadata."))?;
    let metadata_version = metadata
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    if metadata_version == 0 || metadata_version > 3 {
        return Err(invalid("Unsupported LazyTerm metadata version."));
    }
    if metadata_version < 3 {
        crate::metadata::migrate_entity_ids(
            document
                .get_mut("metadata")
                .ok_or_else(|| invalid("Configuration archive is missing metadata."))?,
        )?;
        document["metadata"]["version"] = 3.into();
    }
    let archive: ConfigurationArchive = serde_json::from_value(document)?;
    if archive.passwords_included {
        return Err(invalid(
            "This configuration archive unexpectedly contains password data.",
        ));
    }
    if archive
        .metadata
        .projects
        .iter()
        .flat_map(|project| &project.links)
        .any(|link| link.password.is_some())
    {
        return Err(invalid(
            "Configuration archive contains plaintext Project passwords; use the encrypted credentials archive.",
        ));
    }
    if archive.ssh_config_path.is_none()
        && (!archive.ssh_config.is_empty() || !archive.ssh_config_files.is_empty())
    {
        return Err(invalid(
            "Configuration archive has SSH files but no SSH config root path.",
        ));
    }

    let ssh_dir = home.join(".ssh");
    let current_config = ssh_dir.join("config");
    let mut ssh_files = BTreeMap::new();
    let mut source_root = None;
    if let Some(source_config) = archive.ssh_config_path.as_deref() {
        let source_config = lexical_absolute(source_config)?;
        let source_dir = source_config
            .parent()
            .ok_or_else(|| invalid("SSH config path has no parent directory."))?;
        if let Some(root_contents) = archive.ssh_config_files.get(&source_config)
            && root_contents != &archive.ssh_config
        {
            return Err(invalid(
                "Configuration archive contains conflicting root SSH config data.",
            ));
        }
        source_root = Some(source_dir.to_path_buf());
        let mut found_source_root_file = false;
        for (source_path, contents) in &archive.ssh_config_files {
            let source_path = lexical_absolute(source_path)?;
            let relative = source_path.strip_prefix(source_dir).map_err(|_| {
                invalid("SSH config archive contains a path outside its SSH directory.")
            })?;
            validate_relative(relative)?;
            if source_path == source_config {
                found_source_root_file = true;
                continue;
            }
            let destination = ssh_dir.join(relative);
            if ssh_files
                .insert(destination, contents.as_bytes().to_vec())
                .is_some()
            {
                return Err(invalid("SSH config archive contains duplicate paths."));
            }
        }
        if !archive.ssh_config_files.is_empty() && !found_source_root_file {
            return Err(invalid(
                "SSH config archive is incomplete: the root config file is missing.",
            ));
        }
        ssh_files.insert(
            current_config.clone(),
            archive.ssh_config.as_bytes().to_vec(),
        );
    }

    if let Some(source_root) = source_root.as_deref() {
        for contents in ssh_files.values_mut() {
            let text = std::str::from_utf8(contents)
                .map_err(|_| invalid("SSH config files must be valid UTF-8."))?;
            *contents = rewrite_absolute_includes(text, source_root, &ssh_dir)?.into_bytes();
        }
    }

    let host_count = preflight_ssh_files(&ssh_files, &current_config, &ssh_dir)?;
    Ok(ConfigurationImport {
        metadata: archive.metadata,
        ssh_files,
        host_count,
        ssh_root: ssh_dir,
    })
}

pub fn apply_configuration(
    import: ConfigurationImport,
    metadata_path: &Path,
) -> io::Result<Metadata> {
    let current = Metadata::read_for_import(metadata_path)?;
    apply_configuration_inner(import, metadata_path, current, true, false)
}

pub(crate) fn apply_sync_configuration(
    import: ConfigurationImport,
    metadata_path: &Path,
    expected: Metadata,
    expected_ssh: &BTreeMap<String, String>,
    local_only_includes: &BTreeMap<PathBuf, Vec<(usize, String)>>,
) -> io::Result<Metadata> {
    let config = import.ssh_root.join("config");
    if crate::hosts::HostCatalog::snapshot_from(&config, &import.ssh_root)? != *expected_ssh {
        return Err(invalid(
            "SSH configuration changed during sync; retry synchronization.",
        ));
    }
    let mut import = import;
    for (relative, lines) in local_only_includes {
        let path = import.ssh_root.join(relative);
        let Some(contents) = import.ssh_files.get_mut(&path) else {
            continue;
        };
        let text = std::str::from_utf8(contents)
            .map_err(|_| invalid("SSH config files must be valid UTF-8."))?;
        let mut output = text.lines().map(str::to_string).collect::<Vec<_>>();
        for (index, line) in lines.iter().rev() {
            output.insert((*index).min(output.len()), line.clone());
        }
        *contents = format!("{}\n", output.join("\n")).into_bytes();
    }
    apply_configuration_inner(import, metadata_path, expected, false, true)
}

fn apply_configuration_inner(
    mut import: ConfigurationImport,
    metadata_path: &Path,
    current: Metadata,
    merge: bool,
    allow_external_local_includes: bool,
) -> io::Result<Metadata> {
    import.metadata = if merge {
        merge_metadata(current, import.metadata)?
    } else {
        current.merged_document(serde_json::to_value(&import.metadata)?, BTreeMap::new())?
    };
    let config = import.ssh_root.join("config");
    let canonical_root = fs::canonicalize(&import.ssh_root).unwrap_or(import.ssh_root.clone());
    let snapshot = if import.ssh_files.is_empty() {
        BTreeMap::new()
    } else {
        crate::hosts::HostCatalog::snapshot_from(&config, &import.ssh_root)?
    };
    let mut existing = snapshot
        .into_iter()
        .filter_map(|(path, contents)| {
            let path = PathBuf::from(path);
            match path.strip_prefix(&canonical_root) {
                Ok(relative) => Some(Ok((
                    import.ssh_root.join(relative).display().to_string(),
                    contents,
                ))),
                Err(_) if allow_external_local_includes => None,
                Err(_) => Some(Err(invalid(
                    "Local SSH Include points outside the SSH directory.",
                ))),
            }
        })
        .collect::<io::Result<BTreeMap<_, _>>>()?;
    for path in import.ssh_files.keys() {
        ensure_no_symlink_components(&import.ssh_root, path)?;
        if let std::collections::btree_map::Entry::Vacant(entry) =
            existing.entry(path.display().to_string())
        {
            match fs::read_to_string(path) {
                Ok(contents) => {
                    entry.insert(contents);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let mut aliases = std::collections::HashSet::new();
    for contents in import.ssh_files.values() {
        let text = std::str::from_utf8(contents)
            .map_err(|_| invalid("SSH config files must be valid UTF-8."))?;
        for (names, _) in ssh_sections(text)? {
            aliases.extend(names.into_iter().flatten());
        }
    }
    for (path, contents) in &existing {
        let path = PathBuf::from(path);
        if merge && !import.ssh_files.is_empty() {
            let incoming = import.ssh_files.entry(path).or_default();
            let incoming_text = std::str::from_utf8(incoming)
                .map_err(|_| invalid("SSH config files must be valid UTF-8."))?;
            *incoming = merge_ssh_config(contents, incoming_text, &aliases)?.into_bytes();
        }
    }
    if allow_external_local_includes {
        let mut portable_files = import.ssh_files.clone();
        for contents in portable_files.values_mut() {
            let text = std::str::from_utf8(contents)
                .map_err(|_| invalid("SSH config files must be valid UTF-8."))?;
            *contents = strip_external_include_directives(text, &import.ssh_root)?
                .0
                .into_bytes();
        }
        preflight_ssh_files(&portable_files, &config, &import.ssh_root)?;
    } else {
        preflight_ssh_files(&import.ssh_files, &config, &import.ssh_root)?;
    }
    let mut originals = BTreeMap::new();
    let ssh_root = import.ssh_root.clone();
    for path in import.ssh_files.keys() {
        ensure_no_symlink_components(&ssh_root, path)?;
        match fs::symlink_metadata(path) {
            Ok(file_type) if file_type.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Refusing to replace a symbolic-link SSH config.",
                ));
            }
            Ok(_) => {
                originals.insert(path.clone(), Some(fs::read(path)?));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                originals.insert(path.clone(), None);
            }
            Err(error) => return Err(error),
        }
    }

    for (path, original) in &originals {
        let expected = existing
            .get(&path.display().to_string())
            .map(|value| value.as_bytes());
        if original.as_deref() != expected {
            return Err(io::Error::other(
                "SSH configuration changed while preparing import; retry the import.",
            ));
        }
    }
    let backup_dir = import_backup_dir(metadata_path)?;
    let mut manifest_entries = Vec::new();
    for (index, (path, original)) in originals.iter().enumerate() {
        if let Some(original) = original {
            let backup = backup_dir.join(format!("{index:04}.bak"));
            write_private(&backup, original)?;
            manifest_entries.push((index, path.display().to_string()));
        }
    }
    write_private(
        &backup_dir.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest_entries)?,
    )?;

    let mut changed = Vec::new();
    for (path, contents) in import.ssh_files {
        let current = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return combine_rollback(error, rollback_ssh_files(&changed, &originals));
            }
        };
        if current.as_ref() != originals.get(&path).and_then(Option::as_ref) {
            return combine_rollback(
                io::Error::other("SSH configuration changed during import; retry the import."),
                rollback_ssh_files(&changed, &originals),
            );
        }
        if let Err(error) = ensure_secure_parent_dirs(&path)
            .and_then(|()| crate::hosts::safe_write(&path, &contents))
        {
            let rollback = rollback_ssh_files(&changed, &originals);
            return combine_rollback(error, rollback);
        }
        changed.push(path);
    }

    match import.metadata.save_to(metadata_path) {
        Ok(()) => Ok(import.metadata),
        Err(error) => {
            let rollback = rollback_ssh_files(&changed, &originals);
            combine_rollback(error, rollback)
        }
    }
}

fn ensure_secure_parent_dirs(path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut missing = Vec::new();
    let mut current = parent;
    while !current.exists() {
        missing.push(current.to_path_buf());
        current = current
            .parent()
            .ok_or_else(|| invalid("SSH config parent directory is invalid."))?;
    }
    for directory in missing.into_iter().rev() {
        fs::create_dir(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn rollback_ssh_files(
    changed: &[PathBuf],
    originals: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> io::Result<()> {
    let mut first_error = None;
    for path in changed.iter().rev() {
        let result = match originals.get(path).and_then(Option::as_deref) {
            Some(contents) => crate::hosts::safe_write(path, contents),
            None => fs::remove_file(path),
        };
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn combine_rollback<T>(error: io::Error, rollback: io::Result<()>) -> io::Result<T> {
    match rollback {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(io::Error::other(format!(
            "{error}; rollback also failed: {rollback_error}"
        ))),
    }
}

fn preflight_ssh_files(
    files: &BTreeMap<PathBuf, Vec<u8>>,
    config: &Path,
    ssh_dir: &Path,
) -> io::Result<usize> {
    if files.is_empty() {
        return Ok(0);
    }
    let temp = std::env::temp_dir().join(format!(
        "lazyterm-import-{}-{}",
        std::process::id(),
        timestamp()?
    ));
    fs::create_dir(&temp)?;
    let _cleanup = TemporaryDirectory(temp.clone());
    let temp_ssh = temp.join(".ssh");
    fs::create_dir_all(&temp_ssh)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&temp_ssh, fs::Permissions::from_mode(0o700))?;
    }
    for (path, contents) in files {
        let relative = path
            .strip_prefix(ssh_dir)
            .map_err(|_| invalid("SSH config destination escaped the SSH directory."))?;
        validate_relative(relative)?;
        let destination = temp_ssh.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = std::str::from_utf8(contents)
            .map_err(|_| invalid("SSH config files must be valid UTF-8."))?;
        let contents = rewrite_absolute_includes(contents, ssh_dir, &temp_ssh)?;
        write_private(&destination, contents.as_bytes())?;
    }
    let config_relative = config
        .strip_prefix(ssh_dir)
        .map_err(|_| invalid("SSH config destination escaped the SSH directory."))?;
    let catalog = crate::hosts::HostCatalog::load_from(&temp_ssh.join(config_relative), &temp_ssh);
    match catalog.error {
        Some(error) => Err(invalid(&format!("Imported SSH config is invalid: {error}"))),
        None => Ok(catalog.hosts.len()),
    }
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn rewrite_absolute_includes(
    contents: &str,
    source_root: &Path,
    target_root: &Path,
) -> io::Result<String> {
    let mut output = String::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        let Some((directive, value)) = trimmed.split_once(char::is_whitespace) else {
            output.push_str(line);
            output.push('\n');
            continue;
        };
        if !directive.eq_ignore_ascii_case("include") {
            output.push_str(line);
            output.push('\n');
            continue;
        }
        let words =
            shell_words::split(value).map_err(|_| invalid("Invalid SSH Include directive."))?;
        let mut rewritten = Vec::with_capacity(words.len());
        for word in words {
            let path = Path::new(&word);
            if path.is_absolute() {
                let relative = path.strip_prefix(source_root).map_err(|_| {
                    invalid("SSH Include points outside the exported SSH directory.")
                })?;
                validate_relative(relative)?;
                rewritten.push(target_root.join(relative).display().to_string());
            } else {
                rewritten.push(word);
            }
        }
        let indent = &line[..line.len() - trimmed.len()];
        output.push_str(indent);
        output.push_str("Include ");
        output.push_str(&shell_words::join(rewritten.iter().map(String::as_str)));
        output.push('\n');
    }
    if !contents.ends_with('\n') {
        output.pop();
    }
    Ok(output)
}

fn lexical_absolute(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("SSH config archive paths must be absolute."));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(invalid("SSH config archive path is not normalized."));
            }
        }
    }
    Ok(normalized)
}

fn validate_relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid(
            "SSH config archive contains an unsafe relative path.",
        ));
    }
    Ok(())
}

fn ensure_no_symlink_components(root: &Path, target: &Path) -> io::Result<()> {
    let root = lexical_absolute(root)?;
    let target = lexical_absolute(target)?;
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| invalid("SSH config destination escaped its root directory."))?;
    if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Refusing to use a symbolic-link SSH directory.",
        ));
    }
    let mut current = root;
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(invalid("SSH config destination contains an unsafe path."));
        };
        current.push(part);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Refusing to write through a symbolic link in the SSH config path.",
            ));
        }
    }
    Ok(())
}

fn import_backup_dir(metadata_path: &Path) -> io::Result<PathBuf> {
    let parent = metadata_path.parent().unwrap_or_else(|| Path::new("."));
    let directory = parent
        .join("import-backups")
        .join(format!("ssh-{}", timestamp()?));
    fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn timestamp() -> io::Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .map_err(io::Error::other)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lazyterm-import-test-{}-{name}-{}",
            std::process::id(),
            timestamp().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn archive(home: &Path) -> serde_json::Value {
        let source_ssh = home.join(".ssh");
        let root = source_ssh.join("config");
        let include = source_ssh.join("config.d/work.conf");
        let include_path = source_ssh.join("config.d/work.conf");
        serde_json::json!({
            "format_version": 1,
            "ssh_config_path": root,
            "ssh_config": format!("Include {}\n", include_path.display()),
            "ssh_config_files": {
                root.display().to_string(): format!("Include {}\n", include_path.display()),
                include.display().to_string(): "Host app\n  HostName app.internal\n".to_string()
            },
            "hosts": [],
            "metadata": serde_json::to_value(Metadata::default()).unwrap(),
            "passwords_included": false
        })
    }

    #[test]
    fn maps_ssh_include_files_into_current_home_and_validates_host_catalog() {
        let root = fixture("include-map");
        let source_home = root.join("source");
        let target_home = root.join("target");
        let bundle = archive(&source_home);
        let imported =
            parse_configuration(&serde_json::to_vec(&bundle).unwrap(), &target_home).unwrap();
        assert_eq!(imported.host_count, 1);
        let config = imported
            .ssh_files
            .get(&target_home.join(".ssh/config"))
            .unwrap();
        assert!(
            String::from_utf8_lossy(config).contains(
                target_home
                    .join(".ssh/config.d/work.conf")
                    .to_str()
                    .unwrap()
            )
        );
        assert!(
            imported
                .ssh_files
                .keys()
                .any(|path| path.ends_with("config.d/work.conf"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn portable_ssh_export_omits_external_machine_local_includes() {
        let root = fixture("portable-ssh");
        let ssh_root = root.join(".ssh");
        fs::create_dir_all(&ssh_root).unwrap();
        let external = root.join(".orbstack/ssh/config");
        fs::create_dir_all(external.parent().unwrap()).unwrap();
        fs::write(&external, "Host orb-machine\n  HostName orb.internal\n").unwrap();
        let config = ssh_root.join("config");
        fs::write(
            &config,
            format!(
                "Include {}\nHost app\n  HostName app.internal\n",
                external.display()
            ),
        )
        .unwrap();

        let snapshot = portable_ssh_snapshot(&config, &ssh_root).unwrap();
        let root_config = snapshot
            .get(&fs::canonicalize(&config).unwrap().display().to_string())
            .unwrap();
        assert!(!root_config.contains("Include"));
        assert!(root_config.contains("Host app"));
        assert!(
            snapshot
                .values()
                .all(|contents| !contents.contains("orb-machine"))
        );
        assert!(
            snapshot
                .keys()
                .all(|path| !Path::new(path).starts_with(root.join(".orbstack")))
        );
        let source_config = fs::canonicalize(&config).unwrap();
        let root_config = snapshot
            .get(&source_config.display().to_string())
            .unwrap()
            .clone();
        let bundle = serde_json::json!({
            "format_version": 1,
            "ssh_config_path": source_config,
            "ssh_config": root_config,
            "ssh_config_files": snapshot,
            "metadata": serde_json::to_value(Metadata::default()).unwrap(),
            "passwords_included": false,
        });
        let target_home = root.join("other-device");
        let imported =
            parse_configuration(&serde_json::to_vec(&bundle).unwrap(), &target_home).unwrap();
        assert_eq!(imported.host_count, 1);
        assert!(
            !String::from_utf8_lossy(
                imported
                    .ssh_files
                    .get(&target_home.join(".ssh/config"))
                    .unwrap()
            )
            .contains(".orbstack")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_traversal_in_exported_ssh_config_paths() {
        let root = fixture("traversal");
        let mut bundle = archive(&root.join("source"));
        bundle["ssh_config_files"] = serde_json::json!({
            root.join("source/.ssh/config").display().to_string(): "",
            root.join("source/.ssh/../outside").display().to_string(): ""
        });
        assert!(
            parse_configuration(&serde_json::to_vec(&bundle).unwrap(), &root.join("target"))
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsupported_versions_and_inline_passwords_before_import() {
        let root = fixture("bad-archives");
        let mut bundle = archive(&root.join("source"));
        bundle["format_version"] = 2.into();
        assert!(
            parse_configuration(&serde_json::to_vec(&bundle).unwrap(), &root.join("target"))
                .is_err()
        );

        bundle = archive(&root.join("source"));
        bundle["metadata"]["projects"] = serde_json::json!([{
            "id": 1,
            "name": "demo",
            "base_path": "/tmp",
            "commands": [],
            "groups": [],
            "target": "Local",
            "links": [{
                "id": 1,
                "name": "link",
                "url": "https://example.test",
                "password": "plaintext"
            }]
        }]);
        assert!(
            parse_configuration(&serde_json::to_vec(&bundle).unwrap(), &root.join("target"))
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_import_backs_up_existing_ssh_files_and_metadata() {
        let root = fixture("apply");
        let target_home = root.join("home");
        let ssh_dir = target_home.join(".ssh");
        fs::create_dir_all(&ssh_dir).unwrap();
        let config = ssh_dir.join("config");
        fs::write(&config, "Host previous\n").unwrap();
        let metadata_path = root.join("lazyterm/hosts.json");
        fs::create_dir_all(metadata_path.parent().unwrap()).unwrap();
        fs::write(
            &metadata_path,
            serde_json::to_vec(&Metadata::default()).unwrap(),
        )
        .unwrap();
        let mut bundle = archive(&root.join("source"));
        bundle["ssh_config_path"] = root.join("source/.ssh/config").display().to_string().into();
        bundle["ssh_config"] = "Host imported\n  HostName imported.example\n".into();
        bundle["ssh_config_files"] = serde_json::json!({
            root.join("source/.ssh/config").display().to_string():
                "Host imported\n  HostName imported.example\n"
        });
        let imported =
            parse_configuration(&serde_json::to_vec(&bundle).unwrap(), &target_home).unwrap();
        let metadata = apply_configuration(imported, &metadata_path).unwrap();
        assert_eq!(metadata.version, 3);
        assert!(fs::read_to_string(&config).unwrap().contains("imported"));
        assert!(fs::read_to_string(&config).unwrap().contains("previous"));
        assert!(metadata_path.with_file_name("hosts.json.backups").exists());
        let backups = fs::read_dir(root.join("lazyterm/import-backups"))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert!(!backups.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incoming_hosts_win_across_includes_and_repeated_import_is_idempotent() {
        let root = fixture("merge-includes");
        let _cleanup = TemporaryDirectory(root.clone());
        let home = root.join("home");
        let ssh = home.join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(
            ssh.join("config"),
            "Include local.conf\nHost desktop\n  HostName 192.0.2.1\n",
        )
        .unwrap();
        fs::write(
            ssh.join("local.conf"),
            "# local shared block\nHost app spare\n  HostName old.internal\n  User localuser\n",
        )
        .unwrap();
        let metadata_path = root.join("data/hosts.json");
        let bundle = archive(&root.join("source"));
        let bytes = serde_json::to_vec(&bundle).unwrap();
        apply_configuration(parse_configuration(&bytes, &home).unwrap(), &metadata_path).unwrap();
        let catalog = crate::hosts::HostCatalog::load_from(&ssh.join("config"), &ssh);
        assert!(catalog.error.is_none(), "{:?}", catalog.error);
        assert_eq!(catalog.hosts.len(), 3);
        assert_eq!(
            catalog
                .hosts
                .iter()
                .find(|host| host.alias == "app")
                .unwrap()
                .hostname
                .as_deref(),
            Some("app.internal")
        );
        assert_eq!(
            catalog
                .hosts
                .iter()
                .find(|host| host.alias == "spare")
                .unwrap()
                .user
                .as_deref(),
            Some("localuser")
        );
        let resolved = std::process::Command::new("ssh")
            .args(["-G", "-F"])
            .arg(ssh.join("config"))
            .arg("app")
            .output()
            .unwrap();
        assert!(resolved.status.success());
        assert!(String::from_utf8_lossy(&resolved.stdout).contains("hostname app.internal"));
        let first = crate::hosts::HostCatalog::snapshot_from(&ssh.join("config"), &ssh).unwrap();
        apply_configuration(parse_configuration(&bytes, &home).unwrap(), &metadata_path).unwrap();
        let second = crate::hosts::HostCatalog::snapshot_from(&ssh.join("config"), &ssh).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn metadata_merge_keeps_local_projects_and_secrets_and_remaps_colliding_ids() {
        let id = |value: u128| uuid::Uuid::from_u128(value).to_string();
        let mut local = serde_json::to_value(Metadata::default()).unwrap();
        local["projects"] = serde_json::json!([
            {"id":id(1), "name":"local", "base_path":"/local"},
            {"id":id(2), "name":"shared", "base_path":"/old", "commands":[
                {"id":id(3),"name":"local-task","command":"local"},
                {"id":id(4),"name":"start","command":"old"}
            ], "links":[{"id":id(5),"name":"Home","url":"http://localhost:3000",
                "password_storage":"Config","password":"local-only-secret"}]}
        ]);
        local["credentials"] = serde_json::json!([{
            "id":id(6),"name":"local password","kind":"Password","password_ref":"local-ref"
        }]);
        local["host_passwords"] = serde_json::json!({"local":id(6)});
        local["tags"] = serde_json::json!({"local-host":["local-tag"],"shared-host":["old-tag"]});
        let mut incoming = serde_json::to_value(Metadata::default()).unwrap();
        incoming["projects"] = serde_json::json!([
            {"id":id(7),"name":"remote","base_path":"/remote"},
            {"id":id(8),"name":"shared","base_path":"/updated","commands":[
                {"id":id(9),"name":"start","command":"updated"},
                {"id":id(10),"name":"remote-task","command":"new"}
            ],"links":[{"id":id(11),"name":"Home","url":"http://localhost:4000"}]}
        ]);
        incoming["credentials"] = serde_json::json!([{
            "id":id(12),"name":"remote password","kind":"Password","password_ref":"remote-ref"
        }]);
        incoming["host_passwords"] = serde_json::json!({"remote":id(12)});
        incoming["tags"] = serde_json::json!({"shared-host":["new-tag"]});
        let local: Metadata = serde_json::from_value(local).unwrap();
        let incoming: Metadata = serde_json::from_value(incoming).unwrap();
        let result = merge_metadata(local, incoming.clone()).unwrap();
        assert_eq!(result.projects.len(), 3);
        assert_eq!(result.projects[0].name, "local");
        assert_eq!(result.projects[1].id, uuid::Uuid::from_u128(2));
        assert_eq!(result.projects[1].base_path, PathBuf::from("/updated"));
        assert_eq!(result.projects[1].commands.len(), 3);
        assert_eq!(result.projects[1].commands[1].id, uuid::Uuid::from_u128(4));
        assert_eq!(result.projects[1].commands[1].command, "updated");
        assert_eq!(
            result.projects[1].links[0].password.as_deref(),
            Some("local-only-secret")
        );
        assert_eq!(
            result.projects[1].links[0].password_storage,
            crate::projects::PasswordStorage::Config
        );
        assert_eq!(
            result.credential_import_targets[&format!("config:{}:{}", id(8), id(11))],
            format!("config:{}:{}", id(2), id(5))
        );
        assert_ne!(result.projects[2].id, result.projects[0].id);
        assert_ne!(
            result.host_passwords["local"],
            result.host_passwords["remote"]
        );
        assert_eq!(result.tags["shared-host"], ["new-tag"]);
        assert_eq!(result.tags["local-host"], ["local-tag"]);
        let once = serde_json::to_value(&result).unwrap();
        let twice = serde_json::to_value(merge_metadata(result, incoming).unwrap()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn metadata_merge_uses_matching_uuids_before_names() {
        let project_id = uuid::Uuid::new_v4();
        let command_id = uuid::Uuid::new_v4();
        let mut local = Metadata::default();
        local.projects.push(crate::projects::ProjectDefinition {
            id: project_id,
            name: "Old project name".into(),
            base_path: "/old".into(),
            commands: vec![crate::projects::ProjectCommand {
                id: command_id,
                name: "Old command name".into(),
                command: "old".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut incoming = Metadata::default();
        incoming.projects.push(crate::projects::ProjectDefinition {
            id: project_id,
            name: "Renamed project".into(),
            base_path: "/new".into(),
            commands: vec![crate::projects::ProjectCommand {
                id: command_id,
                name: "Renamed command".into(),
                command: "new".into(),
                ..Default::default()
            }],
            ..Default::default()
        });

        let merged = merge_metadata(local, incoming).unwrap();
        assert_eq!(merged.projects.len(), 1);
        assert_eq!(merged.projects[0].id, project_id);
        assert_eq!(merged.projects[0].name, "Renamed project");
        assert_eq!(merged.projects[0].commands[0].id, command_id);
        assert_eq!(merged.projects[0].commands[0].name, "Renamed command");
        assert_eq!(merged.projects[0].commands[0].command, "new");
    }

    #[test]
    fn configuration_import_migrates_legacy_numeric_ids() {
        let root = fixture("legacy-ids");
        let value = serde_json::json!({
            "format_version": 1,
            "ssh_config_path": null,
            "ssh_config": "",
            "ssh_config_files": {},
            "hosts": [],
            "passwords_included": false,
            "metadata": {
                "version": 2,
                "projects": [{
                    "id": 1,
                    "name": "Legacy",
                    "base_path": "/tmp",
                    "commands": [{"id": 2, "name": "run", "command": "true"}]
                }]
            }
        });
        let imported = parse_configuration(&serde_json::to_vec(&value).unwrap(), &root).unwrap();
        assert_ne!(imported.metadata.projects[0].id, uuid::Uuid::from_u128(1));
        assert_ne!(
            imported.metadata.projects[0].commands[0].id,
            uuid::Uuid::from_u128(2)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_local_metadata_aborts_before_any_ssh_write() {
        let root = fixture("bad-local");
        let _cleanup = TemporaryDirectory(root.clone());
        let home = root.join("home");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        let original = "Host local\n  HostName 192.0.2.1\n";
        fs::write(home.join(".ssh/config"), original).unwrap();
        let metadata_path = root.join("hosts.json");
        fs::write(&metadata_path, "broken").unwrap();
        let bytes = serde_json::to_vec(&archive(&root.join("source"))).unwrap();
        assert!(
            apply_configuration(parse_configuration(&bytes, &home).unwrap(), &metadata_path)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(home.join(".ssh/config")).unwrap(),
            original
        );
        assert!(!root.join("import-backups").exists());
    }
}
