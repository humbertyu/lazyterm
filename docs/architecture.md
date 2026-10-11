# LazyTerm Architecture

## Product scope

LazyTerm is a lightweight, local-first Termius alternative presented as a keyboard-driven TUI. The first milestones focus on host management, native SSH sessions, SFTP, tunnels, and snippets. Embedded terminal rendering, cloud sync, and team collaboration are explicitly deferred.

## Initial boundaries

```text
TUI
  -> Application services
       -> Host catalog
       -> SSH process adapter
       -> SFTP client
       -> Tunnel supervisor
       -> Snippet runner
       -> Credential provider
```

The UI should not own SSH process policy or credential storage. Services communicate through explicit models so OpenSSH can be used for interactive SSH and tunnel processes while SFTP can evolve independently.

## Host and credential data

- Read OpenSSH aliases and connection behavior from `~/.ssh/config`.
- Keep application metadata such as labels, favorites, and tunnel definitions in LazyTerm storage.
- Reference credentials by opaque identifiers; store secrets in macOS Keychain.
- Prefer `ssh-agent` and configured identity files when available.
- Never put passwords in process arguments, environment variables, ordinary config files, or logs.

## SSH and tunnels

Interactive SSH initially hands the terminal to OpenSSH and returns to LazyTerm when the session exits. This avoids implementing a terminal emulator and preserves compatibility with full-screen remote programs.

Tunnels are managed separately from the interactive session. Each tunnel has an explicit lifecycle:

- `session`: stop when the associated session ends;
- `app`: stop when LazyTerm exits;
- `persistent`: detach from the TUI and continue independently.

Use OpenSSH options for connection timeout, server-alive probes, and ProxyJump. Keepalive detects an unresponsive connection; a reconnect policy decides whether and when to establish a new one. Interactive shell state is not recoverable after reconnect unless the remote workload runs inside `tmux` or `screen`.

## Milestones

1. Host catalog, OpenSSH config integration, native SSH handoff, keychain-backed credentials.
2. SFTP browser, tunnel lifecycle management, and single-host snippets.
3. Batch snippets, timeout/keepalive controls, ProxyJump presentation, diagnostics, and reconnect policies.
4. Consider embedded sessions and multiple terminal tabs only if workflows require them.
