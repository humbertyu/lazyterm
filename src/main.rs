mod app;
mod hosts;
mod metadata;
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
    color_eyre::install()?;
    let mut terminal = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, App::new());
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
        app.viewport_width = terminal.size()?.width;
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
                InputAction::LoadFiles { host_alias, path } => {
                    app.start_sftp_listing(host_alias, path)
                }
                InputAction::OpenNativeSftp => open_files(terminal, &mut app)?,
                InputAction::None => {}
            }
        }
    }
    Ok(())
}

fn open_files(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let Some(host) = app.context_host().cloned() else {
        app.notice = Some("Select a host from the Files remote pane first.".to_string());
        return Ok(());
    };

    terminal.show_cursor()?;
    execute!(io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Show)?;
    let result = Command::new("sftp")
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

    app.connection_dialog = Some(ConnectionDialog {
        title: format!("Connecting to {}", host.alias),
        message: "Checking SSH connection...".to_string(),
        pending: true,
    });
    terminal.draw(|frame| ui::render(frame, app))?;

    let preflight = match Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "RequestTTY=no",
            "-o",
            "ConnectTimeout=5",
        ])
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
            });
            return Ok(());
        }
    };

    let preflight_detail = String::from_utf8_lossy(&preflight.stderr);
    if !preflight.status.success() && !needs_interactive_auth(&preflight_detail) {
        app.connection_dialog = Some(ConnectionDialog {
            title: format!("Connection to {} failed", host.alias),
            message: ssh_error_detail(&preflight_detail, preflight.status),
            pending: false,
        });
        return Ok(());
    }
    app.connection_dialog = None;

    // Leave Ratatui's alternate screen completely before handing the terminal
    // to OpenSSH. The preflight above guarantees that failures stay in the TUI.
    terminal.show_cursor()?;
    execute!(io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Show)?;

    let result = Command::new("ssh")
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
            .replace('\n', " ")
    }
}

fn needs_interactive_auth(stderr: &str) -> bool {
    let detail = stderr.to_ascii_lowercase();
    detail.contains("permission denied")
        || detail.contains("publickey")
        || detail.contains("keyboard-interactive")
        || detail.contains("password")
}
