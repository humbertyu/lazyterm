mod app;
mod hosts;
mod ui;

use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};

use color_eyre::eyre::Result;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{Clear, ClearType, enable_raw_mode},
};
use ratatui::DefaultTerminal;

use crate::app::{App, NavSection};

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, App::new());
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, &app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if app.searching {
                app.handle_search_key(key.code);
            } else if app.section == NavSection::Hosts && key.code == KeyCode::Enter {
                connect_selected(terminal, &mut app)?;
            } else {
                app.handle_key(key.code);
            }
        }
    }
    Ok(())
}

fn connect_selected(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let Some(host) = app.selected_host().cloned() else {
        return Ok(());
    };

    app.notice = Some(format!("Checking connection to {}...", host.alias));
    terminal.draw(|frame| ui::render(frame, app))?;

    let preflight = Command::new("ssh")
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
        .output()?;

    let preflight_detail = String::from_utf8_lossy(&preflight.stderr);
    if !preflight.status.success() && !needs_interactive_auth(&preflight_detail) {
        app.notice = Some(format!(
            "SSH to {} failed: {}",
            host.alias,
            ssh_error_detail(&preflight_detail, preflight.status)
        ));
        return Ok(());
    }

    // Leave Ratatui's alternate screen completely before handing the terminal
    // to OpenSSH. The preflight above guarantees that failures stay in the TUI.
    terminal.show_cursor()?;
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
