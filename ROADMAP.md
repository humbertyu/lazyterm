# LazyTerm Roadmap

LazyTerm is early-stage software. This roadmap describes intended release boundaries, not guaranteed dates. Work may move between releases when implementation or security validation changes the scope.

## Current

Host management and native SSH are implemented in development, with the v0.1 acceptance gaps below still open. v0.2 has started with shared host context and an initial library-backed SFTP directory browser; the native `sftp` client remains a compatibility path. This is not a completed or tagged release; tunnel management, credential storage, and snippets remain open.

## Delivery Strategy

Build the integrated core workflows first: Hosts -> SSH / Files -> Tunnels -> Snippets, then consolidate hardening and acceptance. A development milestone may start before the preceding release gate is met. Starting a milestone does not mark unfinished work accepted or change the package version.

- Keep incomplete implementation and unverified behavior visible in the carry-over backlog.
- Use `~/.ssh/config` (including Includes) as the host source of truth; LazyTerm stores only supplemental metadata and module definitions.
- Share host references by alias, never by list index or copied connection fields. Files, tunnels, and snippets resolve references against the current catalog.
- Preserve a selected host across tab switches. Selecting a group must not silently choose its first host for an operation.
- In-app host renames migrate references. External removal/rename invalidates the old reference; discovery failures retain data but block new operations that cannot be resolved safely.
- Runtime state belongs to each module, not to host configuration. Switching tabs must not implicitly launch a connection.
- Never defer minimum safety: no plaintext secrets, no bypass of host-key checking, no silent wrong-host fallback, and no unconfirmed destructive operations initiated by LazyTerm.

### Carry-Over Backlog

| Owner | Incomplete / Unverified | Required Before |
|---|---|---|
| Hosts | Input quoting, advanced managed-block preservation, Include alias conflicts | Declaring managed CRUD safe for release |
| SSH | Real host-key/auth/proxy/terminal acceptance; cancellable preflight and restricted-account behavior | Declaring native SSH accepted |
| Hosts / UI | Favorites, legacy view cleanup, keyboard usability acceptance | Beta scope review |
| Files | Directory transfers, cancellation, confirmed mutations, and integration acceptance | v0.2 release acceptance |
| Credentials | Keychain and native saved-password security spike | Enabling saved credentials |
| Cross-module | Host rename/removal handling for future persisted tunnel/snippet references | Shipping those definitions |
| Release | Disposable SSH/SFTP integration fixtures, supported OS matrix, packaging and release notes | Tagging the corresponding release |

The detailed unchecked items and audit below remain authoritative; none are waived by progressing to the next development milestone.

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

## v0.2.0 - Integrated Files / SFTP

Goal: use the Hosts catalog for file workflows, without a second host database. Grow the initial in-TUI directory browser into a complete Files workflow. Saved-password automation is not a prerequisite for starting Files.

- [x] Carry a stable selected-host alias from Hosts into other modules.
- [x] Add a native `sftp` handoff using that alias and restore the TUI on exit; this remains a compatibility bridge.
- [x] Retain host context across tabs, migrate it on in-app rename, and clear it when refresh confirms the alias is gone.
- [ ] Verify context behavior end to end with SSH config edits, refresh failures, and renamed managed aliases.
- [x] Select and document `openssh` plus `openssh-sftp-client` for the embedded browser; retain OpenSSH configuration and strict host-key checking.
- [ ] Share host resolution and credential lookup across SSH and SFTP.
- [x] List a remote directory, inspect basic entry metadata, enter directories, and return to the parent.
- [x] Build the first integrated Files browser slice with a persistent local pane, remote pane, and Groups/Hosts picker.
- [x] Extend the dual-pane browser with single-file transfers and progress reporting.
- [x] Upload and download ordinary files with progress and clear errors.
- [x] Remember the local directory and the last remote directory for each host.
- [x] Hide dot files and directories by default, with a shared Files toggle.
- [x] Filter the focused local or remote pane by entry name.
- [x] Upload and download directories recursively with aggregate progress.
- [x] Complete cancellation and queued-task controls, including per-task selection/cancellation, retry, history cleanup, progress, and sequential execution.
- [x] Clean up empty directories created by an interrupted or failed recursive transfer while preserving pre-existing directories.
- [x] Show remote size, permissions, owner/group IDs, and readable modification time when the server provides them.
- [x] Add rename, mkdir, and delete with confirmation for individual entries.
- [x] Verify the integrated Hosts -> Files -> Hosts workflow against a disposable SSH/SFTP fixture.
- [x] Reuse one live SFTP connection per host inside a dedicated background Tokio service; the cache is isolated to the LazyTerm process and ends with that service.

An opt-in disposable `sshd` fixture covers the embedded SFTP transport and the application-level Hosts -> Files -> Hosts context flow. Broader refresh, rename, and failure-path acceptance remains tracked separately.

Deferred credential track:

- [ ] Add a macOS Keychain-backed credential store.
- [ ] Support password authentication without writing secrets to config, arguments, environment, or logs.
- [ ] Complete a security and behavior spike for supplying a saved password to native OpenSSH; do not ship an `sshpass` dependency.

Password authentication and first-use host-key confirmation remain outside the embedded browser until the backend can provide secure callbacks. The current `openssh` adapter requires non-interactive authentication and strict known-hosts validation; the native `sftp` handoff remains the compatibility path for interactive prompts.

Release gate: SSH and SFTP use the same host identity and authentication policy; transfers handle interruption without freezing the TUI.

## v0.3.0 - Tunnels and Connection Policies

Goal: manage common port forwards safely.

- [x] Create and edit local forwards.
- [x] Add remote and dynamic/SOCKS forwards.
- [x] Display process state, bound address, and startup errors.
- [x] Detect local port conflicts.
- [x] Add connection timeout and SSH-level Keepalive settings.
- [x] Separate connection failure detection from reconnect policy.
- [x] Add bounded reconnect backoff and stop/reconnect controls.
- [ ] Define `app` lifecycle first; add persistent detached tunnels only with reliable rediscovery and cleanup.
- [x] Display configured `ProxyJump` chains. Identifying the failing hop remains open.

Release gate: users can start, inspect, stop, and restart tunnels without orphaning untracked processes.

## v0.4.0 - Snippets and Batch Operations

Goal: make repeatable remote tasks easy without requiring an embedded terminal.

- [x] Save and edit snippets inside Packages.
- [x] Preview the command and choose a target Host or tagged Host group before execution.
- [x] Run a snippet on one host.
- [x] Run a snippet on a Host group with bounded concurrency.
- [x] Show per-host output, exit status, connection errors, and cancellation.
- [x] Copy a snippet command to the system clipboard.
- [x] Require confirmation for commands marked destructive.

Release gate: batch results clearly distinguish success, command failure, connection failure, and cancellation.

## v0.5.0 - Runtime and Project Workspaces

Goal: make the tool useful for inspecting live workloads and running repeatable commands inside local projects.

Runtime:

- [x] Add a Runtime view for local TCP listeners.
- [x] Reuse the Hosts catalog to inspect a remote machine over OpenSSH.
- [x] Add explicit Local/Hosts selectors, independent per-machine browsing state, and machine-labeled termination confirmation.
- [x] Show protocol, listen address, port, PID, process, user, command, and working directory when available.
- [x] Refresh inspection results without blocking the TUI.
- [x] Confirm before sending `TERM` to a selected process.
- [x] Default to current-user non-system listeners; provide Mine, Apps, System, and All category tabs.
- [x] Color CLI, Agent, desktop, and unknown origins using visible process ancestry; prioritize CLI/Agent listeners.
- [x] Add process details and a controlled `KILL` follow-up.
- [x] Identify Project command listeners by owned process group; show project/command names and jump to output.
- [x] Revalidate the current Project run before confirmed group termination from Runtime.
- [x] Link recognized external Project listeners with a distinct origin and confirmed individual-process termination.
- [x] Identify local Tunnel-managed listeners separately from external processes.
- [x] Add disposable local process fixtures and the Projects -> Runtime -> output -> stop flow.
- [x] Add disposable remote Runtime process fixtures.

Project workspaces:

- [x] Define Projects with a base path, named commands, and optional command working directories.
- [x] Add/edit/delete Projects and commands with the established a/e/d and Enter semantics.
- [x] Add optional command groups, group CRUD, default-expanded inline commands, session fold state, and a flat all-commands view.
- [x] Add named Project web links with add/edit/delete, HTTP/HTTPS validation, browser opening, and a default-expanded group matching command hierarchy.
- [x] Add optional web-link usernames, system-keychain passwords, masked editing, and username/password clipboard actions.
- [x] Distinguish Services from Tasks; scope batch start/stop to Services in a group or project.
- [x] Persist default Project targets and command overrides; block remote execution until its lifecycle is implemented.
- [x] Execute app-scoped remote Project commands through the Hosts catalog with explicit disconnect/exit behavior. Persistent remote commands remain deferred.
- [x] Start, stop, and restart project commands with supervised process groups; start/stop all commands.
- [x] Capture complete stdout/stderr in per-run private logs and show a bounded scrolling output tail and exit status.
- [x] Clean up app-scoped process groups on normal app exit; escalate stalled stops after two seconds.
- [x] Add per-command exit policy, independent persistent runners, log continuity, and status/output/control restoration across TUI restarts.
- [x] Keep Project execution separate from Snippet records used as command templates.
- [x] Add project persistence, additive version-1 compatibility, atomic writes, backup and save-failure coverage.
- [x] Detect externally running commands using current-user identity, working directory, and command/descendant evidence.
- [x] Separate current running status from the last managed result and inspect before starting to avoid recognized duplicates.
- [x] Confirm external stop/restart, revalidate process identity, and preserve external commands on application exit.

Project limits: app-scoped commands may run through the Hosts catalog, but persistent remote commands, interactive PTY, dependency ordering, and remote process rediscovery remain deferred. Only the latest persistent run per command has a saved status/output index; full run history and raw-log rotation remain deferred. App-scoped recovery after forced termination and recovery after the independent runner itself is killed remain in beta hardening. Persistent runs do not automatically relaunch after a crash, logout, or reboot; completing this slice does not declare the v0.5.0 release accepted.

## v0.6.0 - Beta Hardening

Goal: make the integrated v0.5 workflows recoverable, diagnosable, installable, and ready for user acceptance.

- [x] Add a cross-platform Host credential store for usernames and passwords, with a dedicated Credentials tab and reusable Host associations.
- [x] Manage OpenSSH key references and fingerprints, generate Ed25519 key pairs without overwriting existing files, and upload public keys through the existing Groups/Hosts catalog.
- [x] Reuse saved Host credentials from native SSH/SFTP, Tunnels, Snippets, and app-scoped remote Projects through target-scoped askpass. The secret stays in the OS credential store and is not written to config, arguments, environment, or logs.
- [ ] Add equivalent first-use/rejected-password prompts to Tunnels, Snippets, and Projects; support saved passwords in the embedded SFTP browser and jump-host authentication.
- [x] Configuration export and documented data locations. Credential export includes saved manager/Host passwords and Project Web Link passwords in a user-selected, password-protected age file; credential import and private-key export remain pending.
- [x] Introduce additive metadata schema migration with version validation and a pre-migration backup using the existing bounded backup rotation.
- [x] Redact password-like values and URI credentials from surfaced SSH, Tunnel, and Snippet stderr diagnostics; broader diagnostic audit remains part of acceptance.
- [ ] Recovery behavior for killed TUI and tunnel processes.
- [ ] macOS builds for supported architectures.
- [ ] Installation instructions and checksums.
- [ ] User testing of SSH, SFTP, credentials, tunnels, snippets, Runtime, and Projects.
- [x] Add the first Settings surface for theme, language, tab visibility, and metadata-only export.
- [ ] Complete English, Simplified Chinese, and Japanese localization for all workspace labels, dialogs, notices, validation errors, and help text.

0.6 implementation order: Host credentials, metadata migration, and configuration/credential import are in place; remaining beta-hardening work is next. Configuration import is separate from encrypted credential import. The credential manager supports password CRUD, Host association/unlinking, key generation/import, direct association display, and confirmed public-key installation. Native SSH/SFTP, Tunnels, Snippets, remote app-scoped Projects, and public-key upload use scoped askpass for saved target passwords. First-use prompts outside Hosts, embedded SFTP password authentication, jump-host passwords and private-key passphrase storage remain open. An opt-in disposable SSH fixture verifies generation, idempotent installation, and authentication with the newly installed key. Ordinary configuration archives omit saved password fields and private key files.

Configuration import automatically merges with incoming values taking priority: same-name Host stanzas are replaced, local-only aliases and metadata entries survive, and LazyTerm entities use UUIDs matched before name fallback. Existing numeric metadata IDs migrate to UUIDs with a pre-migration backup and their internal references are rewritten. There is one overall import confirmation, with no per-conflict prompts. Host renames, three-way synchronization, and deletion propagation are deferred until Host identity and shared revision history are available.

Recovery note: startup now scans local Project command records, reattaches a live runner, and marks an active record failed only when its runner lock is released and its process group is confirmed empty. Recovery after the runner itself is killed while its command process group remains alive, recovery of forced-killed TUI-owned Tunnel processes, and end-to-end forced-termination acceptance are still open.

## v0.7.0 - Stable Core

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
