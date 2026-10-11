# LazyTerm

A lightweight, keyboard-driven workspace for remote hosts. LazyTerm aims to bring the useful parts of a Termius-style workflow to a local-first TUI: SSH host management, terminal access, SFTP, tunnels, and command snippets.

The project is at an early stage. The current build is a navigable TUI shell; network and credential features are not implemented yet.

## Requirements

- Rust stable
- macOS currently targeted; OpenSSH is expected at `/usr/bin/ssh`

## Run

```sh
cargo run
```

Use `1`-`3` or `Tab` to switch between Hosts, Tunnels, and Snippets. Press `q` or `Esc` to quit.

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
