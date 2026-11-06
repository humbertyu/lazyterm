# LazyTerm Roadmap

LazyTerm is early-stage software. This roadmap describes intended release boundaries, not guaranteed dates. Work may move between releases when implementation or security validation changes the scope.

## Current

The current feature branch implements top-level tabs, compact group cards, tag metadata, orphaned-metadata cleanup, host search, basic managed-host CRUD, native SSH handoff, and connection error dialogs. These are development features, not a completed or tagged v0.1.0 release. SFTP, tunnel management, credential storage, and snippets remain unimplemented.

## v0.1.0 - Host Management and Native SSH

Goal: manage common SSH host profiles and enter reliable native OpenSSH sessions.

- [x] Separate app state, UI rendering, SSH config discovery, and metadata into modules. Further domain/adapter extraction remains open.
- [x] Discover concrete aliases from `~/.ssh/config` and basic included config files. Full OpenSSH grammar and expansion support remain open.
- [x] Search and select hosts.
- [x] Add a host from the TUI with alias, hostname, username, port, and identity file.
- [x] Edit and delete basic hosts created or managed by LazyTerm.
- [ ] Write basic host fields safely without exposing or storing passwords.
- [ ] Preserve unrelated comments, `Include` directives, advanced OpenSSH options, and unmanaged host blocks where possible.
- [x] Show confirmation before deleting a managed host.
- [ ] Fully validate alias conflicts across included files; keep overwrites refused unless explicitly confirmed.
- [x] Support tag-based groups as separate, versioned LazyTerm metadata.
- [x] Clean up metadata for removed SSH aliases on startup/refresh; retain it when discovery fails.
- [ ] Support favorites.
- [ ] Launch `ssh <alias>` with inherited terminal and return cleanly to the TUI.
- [ ] Preserve OpenSSH host-key verification, identity files, `ssh-agent`, `ProxyJump`, and `ProxyCommand`.
- [ ] Show connection exit status and useful errors.
- [ ] Support macOS first and document the tested OpenSSH/Rust versions.

Release gate: a clean install can discover or create a normal OpenSSH host, edit or remove a LazyTerm-managed host safely, connect to a host, and return to the TUI without corrupting the terminal.

### v0.1.0 Audit - 2026-11-06

The release gate is **not yet met**. Checked boxes above record implemented behavior, not full release acceptance.

- Safe writing: backups, atomic replacement, private permissions on newly created files, and protection of neighboring managed markers are implemented and tested. Input quoting/validation, included-file alias conflicts, and preservation of manually added advanced options/comments inside a managed block still need dedicated coverage and hardening.
- SSH acceptance: verify first-time host-key confirmation, changed-key rejection, password/key authentication, ProxyJump/ProxyCommand, and terminal restoration on success, failure, and interruption against disposable SSH fixtures.
- SSH responsiveness: preflight currently blocks the event loop and cannot be cancelled. It runs a remote `true` command, so restricted-command accounts may fail preflight despite supporting the intended session.
- Error handling: a missing/unlaunchable SSH executable now stays in a TUI error dialog. Process supervision and simulated process/terminal integration tests remain open.
- Persistence: groups and tags are implemented; favorites are not. The legacy `view` metadata property remains readable but no longer selects separate Groups/Hosts pages.
- Environment: local checks ran with Rust 1.92.0 and system OpenSSH 9.9p2 (LibreSSL 3.3.6) on macOS. This does not constitute a tested support matrix.
- Release preparation: finish the remaining safety and SSH checks, publish release notes, and only then tag a release. Package version `0.1.0` alone does not mean a release has shipped.

## v0.2.0 - Credentials and SFTP

Goal: add safe saved credentials and basic file transfer.

- [ ] Add a macOS Keychain-backed credential store.
- [ ] Support password authentication without writing secrets to config, arguments, environment, or logs.
- [ ] Complete a security and behavior spike for supplying a saved password to native OpenSSH; do not ship an `sshpass` dependency.
- [ ] Select and document a maintained SFTP transport.
- [ ] Share host resolution and credential lookup across SSH and SFTP.
- [ ] Browse directories and inspect file metadata.
- [ ] Upload and download with progress, cancellation, and clear errors.
- [ ] Add rename, mkdir, and delete with confirmation.

Release gate: SSH and SFTP use the same host identity and authentication policy; transfers handle interruption without freezing the TUI.

## v0.3.0 - Tunnels and Connection Policies

Goal: manage common port forwards safely.

- [ ] Create and edit local forwards.
- [ ] Add remote and dynamic/SOCKS forwards.
- [ ] Display process state, bound address, and startup errors.
- [ ] Detect local port conflicts.
- [ ] Add connection timeout and SSH-level Keepalive settings.
- [ ] Separate connection failure detection from reconnect policy.
- [ ] Add bounded reconnect backoff and stop/reconnect controls.
- [ ] Define `app` lifecycle first; add persistent detached tunnels only with reliable rediscovery and cleanup.
- [ ] Display `ProxyJump` chains and identify the failing hop where possible.

Release gate: users can start, inspect, stop, and restart tunnels without orphaning untracked processes.

## v0.4.0 - Snippets and Batch Operations

Goal: make repeatable remote tasks easy without requiring an embedded terminal.

- [ ] Save and edit snippets.
- [ ] Preview the rendered command and target hosts before execution.
- [ ] Run a snippet on one host.
- [ ] Run on selected hosts with bounded concurrency.
- [ ] Show per-host output, exit status, and cancellation.
- [ ] Copy a rendered command to the system clipboard.
- [ ] Require confirmation for commands marked destructive.

Release gate: batch results clearly distinguish success, command failure, connection failure, and cancellation.

## v0.5.0 - Beta Hardening

Goal: make the tool practical to install and operate across repeated daily use.

- [ ] Configuration import/export and documented data locations.
- [ ] Schema migrations and backup behavior.
- [ ] Better diagnostics with secret redaction.
- [ ] Recovery behavior for killed TUI and tunnel processes.
- [ ] macOS builds for supported architectures.
- [ ] Installation instructions and checksums.
- [ ] User testing of SSH, SFTP, credentials, tunnels, and snippets.

## v1.0.0 - Stable

Goal: declare the core local workflow stable.

- [ ] The v0.x release gates are met or remaining exclusions are documented.
- [ ] Data and configuration formats have a migration policy.
- [ ] Security review covers credentials, host keys, process arguments, logs, and file permissions.
- [ ] Supported operating systems and OpenSSH versions are explicit.
- [ ] Install, upgrade, backup, and uninstall instructions are complete.
- [ ] No known release-blocking defects remain.

## Later, only if justified

- Embedded terminal and multiple live SSH tabs.
- Split view and command injection into an active session.
- Cloud sync, shared vaults, and team features.
- Mosh, serial connections, or non-SFTP protocols.

## Issue labels

Start with a small label set:

- `area: tui`, `area: ssh`, `area: sftp`, `area: tunnels`, `area: credentials`, `area: docs`
- `type: feature`, `type: bug`, `type: maintenance`, `type: security`
- `good first issue`
- `needs decision`

An issue should describe the user problem and acceptance criteria. A roadmap checkbox should link to its tracking issue once the work is ready to start.
