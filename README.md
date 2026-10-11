# LazyTerm

A lightweight, keyboard-driven workspace for remote hosts. LazyTerm aims to bring the useful parts of a Termius-style workflow to a local-first TUI: SSH host management, terminal access, SFTP, tunnels, and command snippets.

The project is at an early stage. The current Hosts view reads aliases from `~/.ssh/config` and opens them with the system OpenSSH client. Basic in-app host creation, editing, and deletion are in scope for v0.1.0 but are not implemented yet. Files, Tunnels, and Snippets are currently navigation placeholders.

## Requirements

- Rust stable
- macOS currently targeted; OpenSSH is expected at `/usr/bin/ssh`

## Run

```sh
cargo run
```

Use `1`-`4` or `Tab` to switch sections. In Hosts, use `↑`/`↓` to select, `Enter` to connect, `/` to search, `r` to reload, `f` for Files, or `t` for Tunnels. Files, Tunnels, and Snippets are placeholders. Press `q` to quit.

The v0.1.0 host-management scope is limited to common connection fields: alias, hostname, username, port, and identity file. LazyTerm will not store passwords. Advanced OpenSSH directives and complex configuration rules remain editable directly in `~/.ssh/config`.

## Direction

- Reuse OpenSSH for interactive sessions, host key verification, and ProxyJump.
- Keep credentials in the operating system keychain; never persist plaintext passwords.
- Keep SSH sessions, the TUI, and tunnels on explicit, independent lifecycles.
- Build SFTP and tunnel management around shared host and credential models.
- Defer embedded terminal rendering, sync, and team features.

See [docs/architecture.md](docs/architecture.md) for the initial architecture and milestones.
See [ROADMAP.md](ROADMAP.md) for planned releases and [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
