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

const ACCENT: crate::theme::ThemeColor = crate::theme::color(crate::theme::Role::Accent);
const TEXT: crate::theme::ThemeColor = crate::theme::color(crate::theme::Role::Text);
const MUTED: crate::theme::ThemeColor = crate::theme::color(crate::theme::Role::Muted);
const BG: crate::theme::ThemeColor = crate::theme::color(crate::theme::Role::Panel);
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT.into()))
        .style(Style::default().bg(BG.into()).fg(TEXT.into()))
}

fn panel_l(app: &App, title: &str) -> Block<'static> {
    Block::default()
        .title(format!(
            " {} ",
            crate::i18n::text(app.metadata.language, title)
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT.into()))
        .style(Style::default().bg(BG.into()).fg(TEXT.into()))
}

fn action_panel<'a>(title: &'a str, footer: &str) -> Block<'a> {
    Block::default()
        .title(title)
        .title_bottom(Span::styled(
            format!(" {footer} "),
            Style::default().fg(MUTED.into()),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT.into()))
        .style(Style::default().bg(BG.into()).fg(TEXT.into()))
}

pub fn render(frame: &mut Frame, app: &App) {
    if app.credential_manager.open {
        render_page(frame, frame.area().inner(Margin::new(1, 1)), app);
    }
    if app.credential_manager.open || app.section == crate::app::NavSection::Credentials {
        render_overlays(frame, app);
    }
    if let Some((alias, credential)) = &app.credential_manager.delete_host_password {
        let related = app.credential_hosts(credential);
        let message = format!(
            "Delete the saved password for {alias}?\n\nRemove its saved record and unused Keychain password.\nAffected Hosts: {}\nThe server password will not change.",
            related.join(", ")
        );
        let area = centered(frame.area(), 76, 12);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: false })
                .block(action_panel(
                    " DELETE HOST PASSWORD ",
                    "y/Enter delete  n/Esc cancel",
                )),
            area,
        );
    }
}

pub fn render_page(frame: &mut Frame, area: Rect, app: &App) {
    crate::theme::set_active(app.metadata.theme);
    let state = &app.credential_manager;
    if state.ssh_key_only {
        render_host_key_selector(frame, app);
        return;
    }
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
                Style::default()
                    .fg(ACCENT.into())
                    .add_modifier(Modifier::BOLD),
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
                Style::default().fg(MUTED.into()),
            )),
        ]));
    }
    let mut selection = ListState::default().with_selected(selected_row);
    frame.render_stateful_widget(
        List::new(list_rows)
            .highlight_symbol("> ")
            .highlight_style(
                Style::default()
                    .fg(ACCENT.into())
                    .add_modifier(Modifier::BOLD),
            )
            .block(panel_l(app, "KEYCHAIN & SSH KEYS")),
        content[0],
        &mut selection,
    );
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(match app.metadata.language {
                crate::metadata::Language::English => "No credentials found. Managed credentials and Project Keychain links appear here; local ~/.ssh keys are discovered on entry. Press r to refresh.",
                crate::metadata::Language::Chinese => "没有找到凭据。此处显示 LazyTerm 凭据和项目钥匙串链接；进入此页时会扫描本地 ~/.ssh 密钥。按 r 刷新。",
                crate::metadata::Language::Japanese => "認証情報がありません。LazyTerm の認証情報とプロジェクトのキーチェーンリンクを表示します。ローカルの ~/.ssh 鍵はこの画面を開くと検出されます。r で更新します。",
            })
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
            .block(panel_l(app, "DETAILS")),
        content[1],
    );
    let help = if state.ssh_key_only {
        format!(
            "Host: {}  Enter configure SSH key  u upload public key\nEsc back",
            state.target.as_deref().unwrap_or_default()
        )
    } else if state.target.is_some() && !state.upload_only {
        format!(
            "Host: {}  Enter configure SSH key  u upload public key  x delete password\na password  g generate key  i import  e edit  d remove  p copy  r refresh\nEsc back",
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
            .style(Style::default().fg(MUTED.into()))
            .wrap(Wrap { trim: false }),
        rows[1],
    );
}

fn render_host_key_selector(frame: &mut Frame, app: &App) {
    let state = &app.credential_manager;
    let area = centered(frame.area(), 76, 20);
    frame.render_widget(Clear, area);
    let title = format!(
        " SET SSH KEY / {} ",
        state.target.as_deref().unwrap_or_default()
    );
    let block = panel(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
    let items = app.visible_credentials();
    let rows_content = items.iter().map(|item| {
        ListItem::new(vec![
            Line::from(Span::styled(
                item.credential.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                item.credential
                    .private_key_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                Style::default().fg(MUTED.into()),
            )),
        ])
    });
    let mut selection = ListState::default().with_selected(
        (!items.is_empty()).then_some(state.selected.min(items.len().saturating_sub(1))),
    );
    frame.render_stateful_widget(
        List::new(rows_content)
            .highlight_symbol("> ")
            .highlight_style(
                Style::default()
                    .fg(ACCENT.into())
                    .add_modifier(Modifier::BOLD),
            ),
        rows[0],
        &mut selection,
    );
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No SSH keys found. Generate or import a key from Credentials.")
                .wrap(Wrap { trim: false }),
            rows[0],
        );
    }
    frame.render_widget(
        Paragraph::new("↑↓ select  Enter set IdentityFile  Esc cancel")
            .style(Style::default().fg(MUTED.into())),
        rows[1],
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
                let related = form.id.and_then(|id| {
                    app.metadata.credentials.iter().find(|item| item.id == id)
                }).map(|item| app.credential_hosts(item)).unwrap_or_default();
                if related.is_empty() {
                    "Saved in the OS credential store. Configuration contains only a reference.".into()
                } else {
                    format!("Updates the saved password for Hosts: {}. Blank keeps the current password.", related.join(", "))
                }
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
                    MUTED.into()
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
                Paragraph::new(crate::ui::input_value(&visible, focused, false))
                    .block(
                        Block::default()
                            .title(*label)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(if focused {
                                ACCENT.into()
                            } else {
                                MUTED.into()
                            })),
                    )
                    .style(Style::default().fg(TEXT.into())),
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
            Paragraph::new("Tab / Up / Down field   Enter save   Esc cancel")
                .style(Style::default().fg(MUTED.into())),
            rows[3],
        );
    }
    if let Some(alias) = &state.confirm_upload {
        let area = centered(frame.area(), 70, 11);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(format!(
                "Host: {alias}\nRemote file: ~/.ssh/authorized_keys\n\nAppend the selected public key (duplicates are skipped)?\nOnly the public key is sent. SSH config is unchanged."
            ))
            .wrap(Wrap { trim: false })
            .block(action_panel(
                " UPLOAD PUBLIC KEY ",
                "y/Enter yes  n/Esc no",
            )),
            area,
        );
    }
    if state.deleting {
        let area = centered(frame.area(), 72, 9);
        frame.render_widget(Clear, area);
        let (message, title, can_delete) = match app.selected_credential() {
            Some(item) if item.source == CredentialSource::Managed => (
                if item.credential.kind == CredentialKind::Password {
                    let related = app.credential_hosts(&item.credential);
                    format!(
                        "Delete saved password \"{}\" from Keychain?\n{}\nThe server password will not change.",
                        item.credential.name,
                        if related.is_empty() {
                            "No Hosts are linked.".into()
                        } else {
                            format!("Unlink from Hosts: {}.", related.join(", "))
                        }
                    )
                } else {
                    format!(
                        "Remove \"{}\" from LazyTerm?\nSSH key files and IdentityFile entries will be retained.",
                        item.credential.name
                    )
                },
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
        let (title, footer) = if can_delete {
            (title, "y/Enter yes  n/Esc no")
        } else {
            (title, "Enter/Esc close")
        };
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: false })
                .block(action_panel(title, footer)),
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
            id: uuid::Uuid::from_u128(1),
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
            id: uuid::Uuid::nil(),
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
    fn host_ssh_key_selector_renders_as_a_single_panel() {
        let mut metadata = Metadata::default();
        metadata.credentials.push(Credential {
            id: uuid::Uuid::from_u128(1),
            name: "Deploy key".into(),
            kind: CredentialKind::SshKey,
            username: String::new(),
            password_ref: None,
            private_key_path: Some("/home/test/.ssh/deploy".into()),
            public_key: "ssh-ed25519 AAAA".into(),
            fingerprint: "SHA256:test".into(),
        });
        let mut app = App::with_catalog(HostCatalog::default(), Ok(metadata));
        app.credential_manager.open = true;
        app.credential_manager.ssh_key_only = true;
        app.credential_manager.target = Some("build-prod".into());

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
        assert!(text.contains("SET SSH KEY / build-prod"));
        assert!(text.contains("Deploy key"));
        assert!(text.contains("/home/test/.ssh/deploy"));
        assert!(!text.contains("CREDENTIALS /"));
        assert!(!text.contains("KEYCHAIN & SSH KEYS"));
        assert!(!text.contains("DETAILS"));
    }

    #[test]
    fn credential_removal_confirmation_is_a_centered_dialog() {
        let mut metadata = Metadata::default();
        metadata.credentials.push(Credential {
            id: uuid::Uuid::from_u128(1),
            name: "Deploy password".into(),
            kind: CredentialKind::Password,
            username: "deploy".into(),
            password_ref: Some("credential-test".into()),
            private_key_path: None,
            public_key: String::new(),
            fingerprint: String::new(),
        });
        metadata
            .host_passwords
            .insert("web".into(), uuid::Uuid::from_u128(1));
        let mut app = App::with_catalog(
            HostCatalog {
                hosts: vec![crate::hosts::Host {
                    alias: "web".into(),
                    hostname: Some("example.com".into()),
                    user: Some("deploy".into()),
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                    managed: false,
                }],
                error: None,
            },
            Ok(metadata),
        );
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
        assert!(text.contains("y/Enter yes"));
        assert!(text.contains("Unlink from Hosts: web."));
        assert!(text.contains("The server password will not change."));
        assert!(!text.contains("Remove this credential record?"));

        app.credential_manager.deleting = false;
        app.section = crate::app::NavSection::Hosts;
        app.confirm_delete_host_password("web");
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("DELETE HOST PASSWORD"));
        assert!(text.contains("saved password for web?"));
        assert!(text.contains("Affected Hosts: web"));
        assert!(text.contains("y/Enter delete"));
        assert!(!app.credential_manager.open);
    }
}
