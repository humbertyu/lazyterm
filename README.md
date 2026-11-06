# LazyTerm

A lightweight, keyboard-driven workspace for remote hosts. LazyTerm aims to bring the useful parts of a Termius-style workflow to a local-first TUI: SSH host management, terminal access, SFTP, tunnels, and command snippets.

The project is at an early stage. The Hosts view reads aliases from `~/.ssh/config`, organizes them by local tags, and opens them with the system OpenSSH client. Basic creation, editing, and deletion are available for LazyTerm-managed hosts. Files, Tunnels, and Snippets are currently navigation placeholders.

## Requirements

- Rust stable
- macOS currently targeted; OpenSSH is expected at `/usr/bin/ssh`

## Run

```sh
cargo run
```

The top tab bar switches between Hosts, Files, Tunnels, and Snippets. Use `1`-`4` or `F1`-`F4` to jump directly, `Tab` / `Shift+Tab` or `Ctrl+Left` / `Ctrl+Right` to cycle, and backtick (`` ` ``) to toggle back to the previous tab. Plain arrow keys navigate within Hosts, not between tabs. Returning to Hosts preserves its group, filter, and selection.

Search captures typed characters, including digits, rather than switching tabs; `F1`-`F4` and `Ctrl+Left` / `Ctrl+Right` can still switch from search without clearing the filter. Add/edit and tag dialogs keep keyboard focus until saved or cancelled; `Tab` moves between form fields there. Terminal-reserved shortcuts may require terminal configuration; plain digit navigation is available outside text input.

In Hosts, use `↑`/`↓` to select, `Enter` to open a group or connect to a host, `/` to search, `r` to reload, `f` for Files, or `t` for Tunnels. Files, Tunnels, and Snippets are placeholders. Press `q` to quit. These shortcuts navigate LazyTerm's workspace; they do not switch SSH sessions while OpenSSH owns the terminal.

Hosts has a compact left browser with **Groups** cards above the **Hosts** list, and a separate details pane on the right. Each card shows its name and host count; at most two columns and two rows are visible, with scrolling for additional groups. Narrow terminals show only the browser. Arrow keys follow the grid: `←`/`→` move within a card row and `↑`/`↓` move between rows. From the last group row, `↓` enters the host list; from the first host, `↑` returns to the last group. Movement stops at the edges rather than wrapping. Hosts without tags appear in **Untagged**. Select a group and press `Enter` to see its hosts. Press `←`, `Esc`, or `Backspace` inside a group to return to the previously selected group card. Press `v` to return directly to the overview.

Select a host and press `g` to edit its comma-separated tags, then `Enter` to save or `Esc` to cancel. Tags work for both managed and existing OpenSSH hosts. A host with multiple tags appears in every corresponding group. Search matches aliases, destinations, and tags, and stays within the current group. New hosts created inside a tag group inherit that tag.

Tags are stored separately from SSH configuration in `~/Library/Application Support/lazyterm/hosts.json` on macOS, or `$XDG_DATA_HOME/lazyterm/hosts.json` (default `~/.local/share/lazyterm/hosts.json`) elsewhere. The legacy `view` property is retained for compatibility but does not switch between separate Groups and Hosts pages. Tag edits do not modify `~/.ssh/config`.

OpenSSH configuration (including discovered `Include` files) is the source of truth for which hosts exist. On startup and `r` refresh, LazyTerm removes metadata for aliases that no longer exist and saves the cleanup only when needed. A missing root config is treated as an empty catalog. Read or parse failures preserve metadata instead of treating hosts as deleted. An externally renamed alias is treated as deletion plus addition; renames made in LazyTerm preserve tags. If a group loses its last host, the view returns to the group list.

The v0.1.0 host-management scope is limited to common connection fields: alias, hostname, username, port, and identity file. LazyTerm will not store passwords. Advanced OpenSSH directives and complex configuration rules remain editable directly in `~/.ssh/config`.

In the Hosts view, use `a` to add a managed host, `e` to edit a LazyTerm-managed host, and `d` to delete one after confirmation. Existing unmanaged OpenSSH hosts remain read-only in LazyTerm. Press `Enter` to hand an SSH session to the system OpenSSH client.

## Direction

- Reuse OpenSSH for interactive sessions, host key verification, and ProxyJump.
- Keep credentials in the operating system keychain; never persist plaintext passwords.
- Keep SSH sessions, the TUI, and tunnels on explicit, independent lifecycles.
- Build SFTP and tunnel management around shared host and credential models.
- Defer embedded terminal rendering, sync, and team features.

See [docs/architecture.md](docs/architecture.md) for the initial architecture and milestones.
See [ROADMAP.md](ROADMAP.md) for planned releases and [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow.

The v0.1.0 release gate is not yet met. See the roadmap audit for remaining favorites, SSH acceptance testing, and configuration-safety work; Files, Tunnels, and Snippets are not part of this milestone.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
