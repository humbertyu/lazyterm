use crate::{
    app::App,
    credential_app::FormKind,
    credentials::{CredentialKind, CredentialSource},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

const ACCENT: Color = Color::Rgb(133, 196, 159);
const TEXT: Color = Color::Rgb(215, 228, 218);
const MUTED: Color = Color::Rgb(140, 156, 151);
const BG: Color = Color::Rgb(18, 23, 23);
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(BG).fg(TEXT))
}

pub fn render(frame: &mut Frame, app: &App) {
    if app.credential_manager.open {
        render_page(frame, frame.area().inner(Margin::new(1, 1)), app);
    }
    if app.credential_manager.open || app.section == crate::app::NavSection::Credentials {
        render_overlays(frame, app);
    }
}

pub fn render_page(frame: &mut Frame, area: Rect, app: &App) {
    let state = &app.credential_manager;
    frame.render_widget(Clear, area);
    let inner = if state.open {
        let title = state.target.as_ref().map_or_else(
            || " CREDENTIALS / PASSWORDS & SSH KEYS ".into(),
            |alias| format!(" CREDENTIALS / {alias} "),
        );
        let block = panel(&title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        area
    };
    let rows = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(if inner.width < 84 { 4 } else { 2 }),
    ])
    .split(inner);
    let content = if rows[0].width >= 84 {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(rows[0])
    } else {
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[0])
    };
    let items = app.visible_credentials();
    let password_count = items
        .iter()
        .filter(|item| item.credential.kind == CredentialKind::Password)
        .count();
    let key_count = items.len() - password_count;
    let mut selected_row = None;
    let mut previous_kind = None;
    let mut list_rows = Vec::new();
    for (credential_index, item) in items.iter().enumerate() {
        let kind = &item.credential.kind;
        if previous_kind != Some(kind) {
            let (label, count) = match kind {
                CredentialKind::Password => ("KEYCHAIN", password_count),
                CredentialKind::SshKey => ("SSH KEYS", key_count),
            };
            list_rows.push(ListItem::new(Line::from(Span::styled(
                format!("{label}  ({count})"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ))));
            previous_kind = Some(kind);
        }
        if credential_index == state.selected {
            selected_row = Some(list_rows.len());
        }
        list_rows.push(ListItem::new(vec![
            Line::from(Span::styled(
                format!("  {}", item.credential.name),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                match (&item.credential.kind, &item.source) {
                    (CredentialKind::Password, CredentialSource::Managed) => {
                        "  LazyTerm / Keychain"
                    }
                    (CredentialKind::Password, CredentialSource::ProjectLink { .. }) => {
                        "  Project Web Link / Keychain"
                    }
                    (CredentialKind::SshKey, CredentialSource::DiscoveredKey) => "  Local ~/.ssh",
                    (CredentialKind::SshKey, _) => "  SSH key / file",
                    (CredentialKind::Password, CredentialSource::DiscoveredKey) => "  Password",
                },
                Style::default().fg(MUTED),
            )),
        ]));
    }
    let mut selection = ListState::default().with_selected(selected_row);
    frame.render_stateful_widget(
        List::new(list_rows)
            .highlight_symbol("> ")
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .block(panel(" KEYCHAIN & SSH KEYS ")),
        content[0],
        &mut selection,
    );
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No credentials found. Managed credentials and Project Keychain links appear here; local ~/.ssh keys are discovered on entry. Press r to refresh.")
                .wrap(Wrap { trim: false }),
            content[0].inner(Margin::new(1, 1)),
        );
    }
    let mut details = vec![];
    if let Some(item) = app.selected_credential() {
        details.push(Line::from(item.credential.name.clone()));
        details.push(Line::from(match item.source {
            CredentialSource::Managed => "Source: LazyTerm credential store",
            CredentialSource::ProjectLink { .. } => "Source: Project Web Link (manage in Projects)",
            CredentialSource::DiscoveredKey => "Source: local ~/.ssh scan",
        }));
        match item.credential.kind {
            CredentialKind::Password => {
                details.push(Line::from(format!(
                    "Username: {}",
                    item.credential.username
                )));
                details.push(Line::from(
                    "Password: ******** (stored in system credential store)",
                ));
                details.push(Line::from("Host username is resolved from OpenSSH config."));
            }
            CredentialKind::SshKey => {
                details.push(Line::from(format!(
                    "Private key: {}",
                    item.credential
                        .private_key_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                )));
                details.push(Line::from(format!(
                    "Fingerprint: {}",
                    item.credential.fingerprint
                )));
                details.push(Line::from(""));
                details.push(Line::from(item.credential.public_key.clone()));
            }
        }
        details.push(Line::from(""));
        let hosts = app.credential_item_hosts(&item);
        details.push(Line::from(format!(
            "Related Hosts ({}): {}",
            hosts.len(),
            if hosts.is_empty() {
                "None".into()
            } else {
                hosts.join(", ")
            }
        )));
        if let Some(alias) = &state.target {
            details.push(Line::from(format!("Target: {alias}")));
        }
    }
    frame.render_widget(
        Paragraph::new(details)
            .wrap(Wrap { trim: false })
            .block(panel(" DETAILS ")),
        content[1],
    );
    let message = if state.deleting || state.confirm_upload.is_some() {
        ""
    } else {
        state.message.as_deref().unwrap_or("")
    };
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(ACCENT)),
        rows[1],
    );
    let help = if state.ssh_key_only {
        format!(
            "Host: {}  Enter set SSH key  u upload public key\nEsc back",
            state.target.as_deref().unwrap_or_default()
        )
    } else if state.target.is_some() && !state.upload_only {
        format!(
            "Host: {}  Enter set SSH key  u upload public key  x unlink password\na password  g generate key  i import  e edit  d remove  p copy  r refresh\nEsc back",
            state.target.as_deref().unwrap_or_default()
        )
    } else if state.target.is_some() && state.upload_only {
        format!(
            "Host: {}  Enter upload public key  p copy  r refresh\nEsc back",
            state.target.as_deref().unwrap_or_default()
        )
    } else {
        "a password  g generate key  i import  e edit  d remove  p copy  r refresh\nEsc back".into()
    };
    frame.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(MUTED))
            .wrap(Wrap { trim: false }),
        rows[2],
    );
}

fn render_overlays(frame: &mut Frame, app: &App) {
    let state = &app.credential_manager;
    if let Some(form) = &state.form {
        let form_area = centered(frame.area(), 76, 19);
        frame.render_widget(Clear, form_area);
        let title = match form.kind {
            FormKind::Password => " PASSWORD / SYSTEM KEYCHAIN ",
            FormKind::Generate => " GENERATE ED25519 SSH KEY ",
            FormKind::Import => " IMPORT SSH KEY ",
            FormKind::RenameKey => " RENAME SSH KEY ",
        };
        let block = panel(title);
        let inner = block.inner(form_area);
        frame.render_widget(block, form_area);
        let note = match form.kind {
            FormKind::Password => {
                "Saved in the OS credential store. Configuration contains only a reference.".into()
            }
            FormKind::Generate => {
                "Creates id_ed25519 + id_ed25519.pub in a NEW directory. This key has no passphrase; keep its private file protected.".into()
            }
            FormKind::Import => {
                "References the existing private key file. Requires a matching .pub file; private keys are never copied into configuration.".into()
            }
            FormKind::RenameKey => {
                let path = form
                    .id
                    .and_then(|id| {
                        app.metadata
                            .credentials
                            .iter()
                            .find(|credential| credential.id == id)
                    })
                    .and_then(|credential| credential.private_key_path.as_deref())
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                format!("Display name only. Key file path is unchanged:\n{path}")
            }
        };
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(note)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(if form.kind == FormKind::Generate {
                    Color::Yellow
                } else {
                    MUTED
                })),
            rows[0],
        );
        let field_rows =
            Layout::vertical(vec![Constraint::Length(3); form.fields.len()]).split(rows[1]);
        for (index, label) in form.labels().iter().enumerate() {
            let focused = index == form.selected;
            let value = if form.kind == FormKind::Password && index == 2 {
                "*".repeat(form.fields[index].chars().count())
            } else {
                form.fields[index].clone()
            };
            let width = field_rows[index].width.saturating_sub(3) as usize;
            let chars = value.chars().collect::<Vec<_>>();
            let visible = chars
                .iter()
                .skip(chars.len().saturating_sub(width))
                .collect::<String>();
            frame.render_widget(
                Paragraph::new(format!("{visible}{}", if focused { "_" } else { "" }))
                    .block(
                        Block::default()
                            .title(*label)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(if focused {
                                ACCENT
                            } else {
                                MUTED
                            })),
                    )
                    .style(Style::default().fg(TEXT)),
                field_rows[index],
            );
        }
        frame.render_widget(
            Paragraph::new(form.error.as_deref().unwrap_or(if app.credentials_busy() {
                "Working..."
            } else {
                ""
            }))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::Yellow)),
            rows[2],
        );
        frame.render_widget(
            Paragraph::new("Tab / Up / Down field   Enter next/save   Esc cancel")
                .style(Style::default().fg(MUTED)),
            rows[3],
        );
    }
    if let Some(alias) = &state.confirm_upload {
        let area = centered(frame.area(), 70, 11);
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(format!("Host: {alias}\nRemote file: ~/.ssh/authorized_keys\n\nAppend the selected public key (duplicates are skipped)?\nOnly the public key is sent. SSH config is unchanged.\n\ny / Enter upload   n / Esc cancel")).wrap(Wrap { trim: false }).block(panel(" UPLOAD PUBLIC KEY ")), area);
    }
    if state.deleting {
        let area = centered(frame.area(), 72, 9);
        frame.render_widget(Clear, area);
        let (message, title, can_delete) = match app.selected_credential() {
            Some(item) if item.source == CredentialSource::Managed => (
                format!(
                    "Remove \"{}\" from LazyTerm?\nSSH key files and IdentityFile entries will be retained.",
                    item.credential.name
                ),
                " REMOVE CREDENTIAL ",
                true,
            ),
            Some(item) => (
                format!(
                    "\"{}\" is read-only here. {}",
                    item.credential.name,
                    match item.source {
                        CredentialSource::ProjectLink { .. } =>
                            "Manage this password from its Project Web Link.",
                        CredentialSource::DiscoveredKey =>
                            "Remove or rename its files under ~/.ssh, then refresh.",
                        CredentialSource::Managed => unreachable!(),
                    }
                ),
                " CANNOT REMOVE ",
                false,
            ),
            None => (
                "No credential is selected.".into(),
                " REMOVE CREDENTIAL ",
                false,
            ),
        };
        let action = if can_delete {
            "y / Enter remove    n / Esc cancel"
        } else {
            "Enter / Esc close"
        };
        frame.render_widget(
            Paragraph::new(format!("{message}\n\n{action}"))
                .wrap(Wrap { trim: false })
                .block(panel(title)),
            area,
        );
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        credential_app::CredentialForm,
        credentials::{Credential, CredentialKind},
        hosts::HostCatalog,
        metadata::Metadata,
    };
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn credentials_forms_mask_input_and_render_at_compact_sizes() {
        let mut app = App::with_catalog(HostCatalog::default(), Ok(Metadata::default()));
        app.open_credentials(None, false);
        app.credential_manager.form = Some(CredentialForm {
            kind: FormKind::Password,
            id: None,
            fields: vec!["Production".into(), "deploy".into(), "not-visible".into()],
            selected: 2,
            error: None,
        });
        for (width, height) in [(120, 32), (80, 24), (60, 18), (40, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(!text.contains("not-visible"));
            if height >= 18 {
                assert!(text.contains("***********"));
            }
            app.credential_manager.confirm_upload = Some("fixture-host".into());
            terminal.draw(|frame| render(frame, &app)).unwrap();
            app.credential_manager.confirm_upload = None;
        }
    }

    #[test]
    fn credential_list_separates_keychain_and_ssh_key_groups() {
        let mut metadata = Metadata::default();
        metadata.credentials.push(Credential {
            id: 1,
            name: "Saved password".into(),
            kind: CredentialKind::Password,
            username: String::new(),
            password_ref: Some("credential-test".into()),
            private_key_path: None,
            public_key: String::new(),
            fingerprint: String::new(),
        });
        let mut app = App::with_catalog(HostCatalog::default(), Ok(metadata));
        app.open_credentials(None, false);
        app.hosts.hosts.push(crate::hosts::Host {
            alias: "build-prod".into(),
            hostname: Some("build.example".into()),
            user: Some("deploy".into()),
            port: None,
            identity_file: Some("/tmp/id_ed25519".into()),
            proxy_jump: None,
            managed: false,
        });
        app.credential_manager.discovered_keys.clear();
        app.credential_manager.discovered_keys.push(Credential {
            id: 0,
            name: "id_ed25519".into(),
            kind: CredentialKind::SshKey,
            username: String::new(),
            password_ref: None,
            private_key_path: Some("/tmp/id_ed25519".into()),
            public_key: "ssh-ed25519 AAAA".into(),
            fingerprint: "SHA256:test".into(),
        });
        app.credential_manager.selected = 1;

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render_page(frame, frame.area(), &app))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("KEYCHAIN"));
        assert!(text.contains("SSH KEYS"));
        assert!(text.contains("Saved password"));
        assert!(text.contains("id_ed25519"));
        assert!(text.contains("Related Hosts (1): build-prod"), "{text:?}");
        assert!(!text.contains("IdentityFile Hosts"));
        assert!(!text.contains("Default-key candidates"));
    }

    #[test]
    fn credential_removal_confirmation_is_a_centered_dialog() {
        let mut metadata = Metadata::default();
        metadata.credentials.push(Credential {
            id: 1,
            name: "Deploy password".into(),
            kind: CredentialKind::Password,
            username: "deploy".into(),
            password_ref: Some("credential-test".into()),
            private_key_path: None,
            public_key: String::new(),
            fingerprint: String::new(),
        });
        let mut app = App::with_catalog(HostCatalog::default(), Ok(metadata));
        app.section = crate::app::NavSection::Credentials;
        app.credential_manager.deleting = true;

        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("REMOVE CREDENTIAL"));
        assert!(text.contains("Deploy password"));
        assert!(text.contains("Enter remove"));
        assert!(!text.contains("Remove this credential record?"));
    }
}
