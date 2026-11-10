mod app;
mod credential_app;
mod credential_ui;
mod credentials;
mod hosts;
mod instance;
mod metadata;
mod project_app;
mod project_credentials;
mod project_monitor;
mod project_runner;
mod projects;
mod runtime;
mod sftp;
mod tunnel;
mod ui;

use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};

use color_eyre::eyre::Result;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{Clear, ClearType, enable_raw_mode},
};
use ratatui::DefaultTerminal;

use crate::app::{App, ConnectionDialog, InputAction};

fn main() -> Result<()> {
    if std::env::var_os("LAZYTERM_ASKPASS_REFERENCE").is_some() {
        if crate::credentials::askpass().is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new("--project-runner")) {
        let spec = arguments
            .next()
            .ok_or_else(|| io::Error::other("Missing runner launch file"))?;
        crate::project_runner::run(std::path::Path::new(&spec))?;
        return Ok(());
    }
    color_eyre::install()?;
    let _instance = match instance::InstanceGuard::acquire(&metadata::Metadata::data_dir()?) {
        Ok(instance) => instance,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            eprintln!("{error}");
            std::process::exit(1);
        }
        Err(error) => return Err(error.into()),
    };
    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, App::new());
    crate::project_credentials::clear_password_clipboard();
    ratatui::restore();
    execute!(io::stdout(), DisableMouseCapture)?;
    result
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    while !app.should_quit {
        app.poll_sftp();
        app.poll_transfers();
        app.poll_mutations();
        app.poll_tunnels();
        app.poll_snippet_runs();
        app.poll_projects();
        app.poll_runtime();
        app.poll_credentials();
        let size = terminal.size()?;
        app.viewport_width = size.width;
        app.viewport_height = size.height;
        terminal.draw(|frame| ui::render(frame, &app))?;

        if event::poll(Duration::from_millis(100))? {
            let action = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_event(key),
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                    let size = terminal.size()?;
                    app.handle_mouse(mouse.column, mouse.row, size.width, size.height)
                }
                _ => InputAction::None,
            };
            match action {
                InputAction::Connect => connect_selected(terminal, &mut app)?,
                InputAction::RetryPublicKeyUpload => {
                    let submitted = app.take_pending_host_password();
                    if let Some(submitted) = submitted {
                        app.start_public_key_upload(
                            submitted.alias.clone(),
                            Some(submitted),
                            false,
                        );
                    }
                }
                InputAction::LoadFiles { host_alias, path } => {
                    app.start_sftp_listing(host_alias, path)
                }
                InputAction::OpenNativeSftp => open_files(terminal, &mut app)?,
                InputAction::RunSnippets {
                    host_aliases,
                    command,
                    snippet_name,
                } => app.start_snippet_runs(host_aliases, command, snippet_name),
                InputAction::CopySnippet { command } => copy_to_clipboard(&mut app, &command),
                InputAction::None => {}
            }
        }
    }
    Ok(())
}

fn copy_to_clipboard(app: &mut App, command: &str) {
    let candidates = if cfg!(target_os = "macos") {
        vec![("pbcopy", Vec::<&str>::new())]
    } else if cfg!(target_os = "windows") {
        vec![("clip", Vec::<&str>::new())]
    } else {
        vec![
            ("wl-copy", Vec::<&str>::new()),
            ("xclip", vec!["-selection", "clipboard"]),
        ]
    };
    for (program, args) in candidates {
        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(command.as_bytes());
        }
        match child.wait() {
            Ok(status) if status.success() => {
                app.notice = Some("Copied snippet command to clipboard.".to_string());
                return;
            }
            _ => {}
        }
    }
    app.notice = Some("Clipboard tool is unavailable.".to_string());
}

fn open_files(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let Some(host) = app.context_host().cloned() else {
        app.notice = Some("Select a host from the Files remote pane first.".to_string());
        return Ok(());
    };

    let mut command = Command::new("sftp");
    if let Some(username) = app.host_password_username(&host.alias) {
        command.arg("-l").arg(username);
    }
    if let Some(reference) = app.host_password_reference(&host.alias) {
        command.args(["-o", "BatchMode=no"]);
        if let Err(error) =
            crate::credentials::configure_password(&mut command, reference, &host.alias)
        {
            app.notice = Some(format!("Could not prepare saved password: {error}"));
            return Ok(());
        }
    }

    terminal.show_cursor()?;
    execute!(io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Show)?;
    let result = command
        .arg(&host.alias)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let notice = match result {
        Ok(status) if status.success() => format!("SFTP session to {} ended.", host.alias),
        Ok(status) => format!("SFTP to {} failed ({})", host.alias, exit_status(status)),
        Err(error) => format!("Could not start OpenSSH sftp: {error}"),
    };

    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Hide)?;
    enable_raw_mode()?;
    *terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    terminal.clear()?;
    app.notice = Some(notice);
    Ok(())
}

fn connect_selected(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let Some(host) = app.selected_host().cloned() else {
        return Ok(());
    };
    let submitted = app.take_pending_host_password();
    let saved_username = app.host_password_username(&host.alias).map(str::to_string);
    let username = submitted
        .as_ref()
        .map(|value| value.username.clone())
        .or_else(|| saved_username.clone());
    let saved_reference = app.host_password_reference(&host.alias).map(str::to_string);
    let mut prepared = if let Some(value) = &submitted {
        match crate::project_credentials::prepare_credential(value.password.clone()).recv() {
            Ok(Ok(prepared)) => Some(prepared),
            Ok(Err(error)) => {
                app.notice = Some(format!(
                    "Could not prepare password in system Keychain: {error}"
                ));
                return Ok(());
            }
            Err(_) => {
                app.notice = Some("Password Keychain worker stopped.".into());
                return Ok(());
            }
        }
    } else {
        None
    };
    let password_reference = prepared
        .as_ref()
        .map(|value| value.reference.clone())
        .or(saved_reference.clone());

    app.connection_dialog = Some(ConnectionDialog {
        title: format!("Connecting to {}", host.alias),
        message: "Checking SSH authentication...".to_string(),
        pending: true,
        scroll: 0,
    });
    terminal.draw(|frame| ui::render(frame, app))?;
    let mut preflight_command = Command::new("ssh");
    if let Some(username) = &username {
        preflight_command.arg("-l").arg(username);
    }
    preflight_command.args([
        "-v",
        "-o",
        "RequestTTY=no",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "NumberOfPasswordPrompts=1",
    ]);
    if let Some(reference) = password_reference.as_deref() {
        preflight_command.args(["-o", "BatchMode=no"]);
        if let Err(error) =
            crate::credentials::configure_password(&mut preflight_command, reference, &host.alias)
        {
            app.connection_dialog = None;
            app.notice = Some(format!("Could not prepare Host password: {error}"));
            return Ok(());
        }
    } else {
        preflight_command.args(["-o", "BatchMode=yes"]);
    }
    let preflight = match preflight_command
        .arg(&host.alias)
        .arg("true")
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            app.connection_dialog = Some(ConnectionDialog {
                title: format!("Connection to {} failed", host.alias),
                message: format!("Could not start OpenSSH: {error}"),
                pending: false,
                scroll: 0,
            });
            return Ok(());
        }
    };
    let detail = String::from_utf8_lossy(&preflight.stderr);
    if !preflight.status.success() {
        app.connection_dialog = None;
        if needs_interactive_auth(&detail) {
            if submitted.is_none() && saved_reference.is_some() {
                app.unlink_host_password(&host.alias);
            }
            let save = submitted
                .as_ref()
                .map_or(saved_reference.is_some(), |value| value.save_in_keychain);
            let username = username
                .clone()
                .or_else(|| crate::hosts::ssh_config_user(&host.alias))
                .or_else(|| host.user.clone())
                .unwrap_or_default();
            app.prompt_host_password(
                host.alias,
                username,
                Some(if submitted.is_some() {
                    "Authentication failed. Check the password and try again.".into()
                } else if saved_reference.is_some() {
                    "Saved password was rejected. Enter the current Host password.".into()
                } else {
                    "SSH requires a password. Enter the Host password.".into()
                }),
                save,
                crate::app::HostPasswordPurpose::Connect,
            );
        } else {
            app.connection_dialog = Some(ConnectionDialog {
                title: format!("Connection to {} failed", host.alias),
                message: ssh_error_detail(&detail, preflight.status),
                pending: false,
                scroll: 0,
            });
        }
        return Ok(());
    }
    if authenticated_with_public_key(&detail) {
        app.metadata.host_public_key_auth.insert(host.alias.clone());
        app.save_metadata();
    }
    if let (Some(value), Some(prepared)) = (&submitted, &mut prepared)
        && value.save_in_keychain
        && !app.save_host_password_prepared(&host.alias, &value.username, prepared, None)
    {
        app.connection_dialog = None;
        app.notice =
            Some("Authenticated, but could not save the Host password in Keychain.".into());
        return Ok(());
    }
    app.connection_dialog = None;

    let mut command = Command::new("ssh");
    if let Some(username) = &username {
        command.arg("-l").arg(username);
    }
    if let Some(reference) = password_reference.as_deref() {
        command.args(["-o", "BatchMode=no"]);
        if let Err(error) =
            crate::credentials::configure_password(&mut command, reference, &host.alias)
        {
            app.notice = Some(format!("Could not prepare saved password: {error}"));
            return Ok(());
        }
    }

    // Leave Ratatui's alternate screen completely before handing the terminal
    // to OpenSSH. The preflight above guarantees that failures stay in the TUI.
    terminal.show_cursor()?;
    execute!(io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Show)?;

    let result = command
        .arg(&host.alias)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let notice = match result {
        Ok(status) if status.success() => format!("SSH session to {} ended.", host.alias),
        Ok(status) => format!("SSH to {} failed ({})", host.alias, exit_status(status)),
        Err(error) => format!("Could not start OpenSSH: {error}"),
    };

    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Hide)?;
    enable_raw_mode()?;
    *terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    terminal.clear()?;
    app.notice = Some(notice);
    Ok(())
}

fn authenticated_with_public_key(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.contains("Authenticated to ") && line.contains("using \"publickey\""))
}

fn exit_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| format!("exit {code}"),
    )
}

fn ssh_error_detail(stderr: &str, status: ExitStatus) -> String {
    let detail = stderr.trim();
    if detail.is_empty() {
        exit_status(status)
    } else {
        detail
            .strip_prefix("ssh: ")
            .unwrap_or(detail)
            .replace("\r\n", "\n")
            .replace('\r', "")
    }
}

fn needs_interactive_auth(stderr: &str) -> bool {
    let detail = stderr.to_ascii_lowercase();
    detail.contains("permission denied")
        || detail.contains("publickey")
        || detail.contains("keyboard-interactive")
        || detail.contains("password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_error_detail_preserves_warning_line_breaks() {
        let detail = ssh_error_detail(
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\r\nSHA256:fingerprint\r\n",
            ExitStatus::default(),
        );
        assert_eq!(
            detail,
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nSHA256:fingerprint"
        );
    }

    #[test]
    fn public_key_auth_is_only_recorded_when_openssh_confirms_it() {
        assert!(authenticated_with_public_key(
            "debug1: Authenticated to host.example ([192.0.2.1]:22) using \"publickey\"."
        ));
        assert!(!authenticated_with_public_key(
            "debug1: Authenticated to host.example ([192.0.2.1]:22) using \"password\"."
        ));
        assert!(!authenticated_with_public_key(
            "debug1: Offering public key: /Users/test/.ssh/id_ed25519"
        ));
    }
}
