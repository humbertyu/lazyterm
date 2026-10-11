# LazyTerm Roadmap

LazyTerm is early-stage software. This roadmap describes intended release boundaries, not guaranteed dates. Work may move between releases when implementation or security validation changes the scope.

## Current

The current feature branch has the initial navigation shell, OpenSSH alias discovery, host search, native SSH handoff, and connection error dialogs in progress. Basic host editing is now part of v0.1.0. SFTP, tunnel management, credential storage, and snippets remain unimplemented until delivered and included in a tagged release.

## v0.1.0 - Host Management and Native SSH

Goal: manage common SSH host profiles and enter reliable native OpenSSH sessions.

- [ ] Split the TUI shell into app, UI, domain, and adapter modules.
- [ ] Discover concrete aliases from `~/.ssh/config` and included config files.
- [ ] Search and select hosts.
- [ ] Add a host from the TUI with alias, hostname, username, port, and identity file.
- [ ] Edit and delete hosts created or managed by LazyTerm.
- [ ] Write basic host fields safely without exposing or storing passwords.
- [ ] Preserve unrelated comments, `Include` directives, advanced OpenSSH options, and unmanaged host blocks where possible.
- [ ] Show confirmation before deleting a host or overwriting an existing alias.
- [ ] Support favorites, groups, and tags as LazyTerm metadata.
- [ ] Launch `ssh <alias>` with inherited terminal and return cleanly to the TUI.
- [ ] Preserve OpenSSH host-key verification, identity files, `ssh-agent`, `ProxyJump`, and `ProxyCommand`.
- [ ] Show connection exit status and useful errors.
- [ ] Support macOS first and document the tested OpenSSH/Rust versions.

Release gate: a clean install can discover or create a normal OpenSSH host, edit or remove a LazyTerm-managed host safely, connect to a host, and return to the TUI without corrupting the terminal.

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
