mod app;
mod hosts;
mod ui;

use std::{
    io::{self, Write},
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

    // Leave Ratatui's alternate screen completely before handing the terminal
    // to OpenSSH. This keeps failed sessions visible on terminals that do not
    // repaint the alternate screen reliably.
    terminal.show_cursor()?;
    ratatui::restore();
    execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0), Show)?;

    let result = Command::new("ssh")
        .arg(&host.alias)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let connected = matches!(&result, Ok(status) if status.success());
    let notice = match result {
        Ok(status) if status.success() => format!("SSH session to {} ended.", host.alias),
        Ok(status) => format!("SSH to {} failed ({})", host.alias, exit_status(status)),
        Err(error) => format!("Could not start OpenSSH: {error}"),
    };

    if !connected {
        writeln!(
            io::stdout(),
            "\r\n{notice}\r\nPress Enter to return to LazyTerm..."
        )?;
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
    }

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
