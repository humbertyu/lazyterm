mod app;
mod authentication;
mod credential_app;
mod credential_ui;
mod credentials;
mod diagnostics;
mod hosts;
mod i18n;
mod import_export;
mod instance;
mod metadata;
mod native_connection;
mod process_guard;
mod project_app;
mod project_credentials;
mod project_monitor;
mod project_runner;
mod projects;
mod runtime;
mod sftp;
mod theme;
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
    if std::env::var_os("LAZYTERM_AUTH_SOCKET").is_some() {
        if crate::authentication::askpass().is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    if std::env::var_os("LAZYTERM_ASKPASS_REFERENCE").is_some() {
        if crate::credentials::askpass().is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments.next();
    match mode.as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("--version" | "-V") => {
            println!("lazyterm {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!(
                "LazyTerm {}\n\nUsage: lazyterm [--version | --diagnostics | --help]\n\nRun without arguments to open the terminal workspace.\n--diagnostics prints platform information without reading configuration or credentials.",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        Some("--diagnostics") => {
            print!("{}", crate::diagnostics::support_report());
            return Ok(());
        }
        _ => {}
    }
    if mode.as_deref() == Some(std::ffi::OsStr::new("--process-guard")) {
        crate::process_guard::run()?;
        return Ok(());
    }
    if mode.as_deref() == Some(std::ffi::OsStr::new("--project-runner")) {
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
        app.poll_authentication();
        poll_native_connection(terminal, &mut app)?;
        app.update_toast();
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

    begin_native_connection(terminal, app, host.alias, native_connection::Kind::Sftp)
}

fn connect_selected(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let Some(host) = app.selected_host().cloned() else {
        return Ok(());
    };
    begin_native_connection(terminal, app, host.alias, native_connection::Kind::Shell)
}

fn begin_native_connection(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    alias: String,
    kind: native_connection::Kind,
) -> io::Result<()> {
    if app.native_connection.is_some() {
        app.notice = Some("A connection is already being prepared.".into());
        return Ok(());
    }
    match native_connection::Pending::start(alias.clone(), kind, app.host_password_auth(&alias)) {
        Ok(pending) => {
            app.native_connection = Some(pending);
            app.connection_dialog = Some(ConnectionDialog {
                title: format!("Connecting to {alias}"),
                message: "Authenticating SSH connection...".into(),
                pending: true,
                cancelable: true,
                scroll: 0,
            });
        }
        Err(error) => app.notice = Some(format!("Could not prepare SSH connection: {error}")),
    }
    terminal.draw(|frame| ui::render(frame, app))?;
    Ok(())
}

fn poll_native_connection(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    app.poll_authentication();
    if app.authentication_saving() {
        return Ok(());
    }
    let Some(pending) = &app.native_connection else {
        return Ok(());
    };
    let result = match pending.receiver.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            Err(io::Error::other("SSH connection worker stopped."))
        }
    };
    let alias = pending.alias.clone();
    app.native_connection = None;
    let connection = match result {
        Ok(connection) => connection,
        Err(error) => {
            app.connection_dialog = Some(ConnectionDialog {
                title: format!("Connection to {alias} failed"),
                message: crate::diagnostics::redact_sensitive_text(&error.to_string()),
                pending: false,
                cancelable: false,
                scroll: 0,
            });
            return Ok(());
        }
    };
    app.connection_dialog = None;
    // The terminal client attaches to the authenticated private master, without a second login.
    let mut command = connection.command();
    terminal.show_cursor()?;
    execute!(io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Show)?;

    let result = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let notice = match result {
        Ok(status) if status.success() => format!("Session to {alias} ended."),
        Ok(status) => format!("Session to {alias} failed ({})", exit_status(status)),
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

fn exit_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| format!("exit {code}"),
    )
}
