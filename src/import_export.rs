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
    format_version: u32,
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

pub fn parse_configuration(bytes: &[u8], home: &Path) -> io::Result<ConfigurationImport> {
    let archive: ConfigurationArchive = serde_json::from_slice(bytes)?;
    if archive.format_version != 1 {
        return Err(invalid("Unsupported configuration export version."));
    }
    if archive.passwords_included {
        return Err(invalid(
            "This configuration archive unexpectedly contains password data.",
        ));
    }
    if archive.metadata.version == 0 || archive.metadata.version > 2 {
        return Err(invalid("Unsupported LazyTerm metadata version."));
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
        ensure_secure_parent_dirs(&path)?;
        if let Err(error) = crate::hosts::safe_write(&path, &contents) {
            let rollback = rollback_ssh_files(&changed, &originals);
            return combine_rollback(error, rollback);
        }
        changed.push(path);
    }

    match Metadata::save_imported(metadata_path, import.metadata) {
        Ok(metadata) => Ok(metadata),
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

fn rewrite_absolute_includes(
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
        assert_eq!(metadata.version, 2);
        assert!(fs::read_to_string(&config).unwrap().contains("imported"));
        assert!(metadata_path.with_file_name("hosts.json.backups").exists());
        let backups = fs::read_dir(root.join("lazyterm/import-backups"))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert!(!backups.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
