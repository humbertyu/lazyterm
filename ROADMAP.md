# LazyTerm Roadmap

LazyTerm is early-stage software. This roadmap describes intended release boundaries, not guaranteed dates. Work may move between releases when implementation or security validation changes the scope.

## Current

The integrated v0.1-v0.6 workflows are the implementation baseline: native SSH, embedded SFTP, tunnels, snippets, Runtime, Projects, credential import/export, crash recovery, and the macOS Universal Binary release pipeline. v0.1 and v0.2 are historical implementation milestones, not pending feature lists; their remaining items below are specific coverage, scope, or acceptance gaps. Initial v0.7 Git configuration sync is available for testing; its remaining implementation and acceptance work is explicitly deferred below while development moves to v0.8 Agent interoperability on `v0.8-agent-friendly`. The former v0.7 Stable Core milestone remains v0.9. No public release is implied.

## Delivery Strategy

Build the integrated core workflows first: Hosts -> SSH / Files -> Tunnels -> Snippets, then consolidate hardening and acceptance. A development milestone may start before the preceding release gate is met. Starting a milestone does not mark unfinished work accepted or change the package version.

- Keep incomplete implementation and unverified behavior visible in the carry-over backlog.
- Use `~/.ssh/config` (including Includes) as the host source of truth; LazyTerm stores only supplemental metadata and module definitions.
- Share host references by alias, never by list index or copied connection fields. Files, tunnels, and snippets resolve references against the current catalog.
- Preserve a selected host across tab switches. Selecting a group must not silently choose its first host for an operation.
- In-app host renames migrate references. External removal/rename invalidates the old reference; discovery failures retain data but block new operations that cannot be resolved safely.
- Runtime state belongs to each module, not to host configuration. Switching tabs must not implicitly launch a connection.
- Never defer minimum safety: no plaintext secrets, no bypass of host-key checking, no silent wrong-host fallback, and no unconfirmed destructive operations initiated by LazyTerm.

### Remaining Cross-Cut Work

| Owner | Incomplete / Unverified | Required Before |
|---|---|---|
| Hosts | Broader OpenSSH grammar/Include collision coverage; favorites | Managed CRUD acceptance / product scope |
| SSH | Manual host-key, key/agent, ProxyCommand, terminal-restoration acceptance; cancellable preflight and restricted-account behavior | Declaring native SSH accepted |
| Hosts / UI | Keyboard usability acceptance; legacy metadata cleanup if needed | Beta scope review |
| Files | Manual edge-case acceptance for external config edits, refresh failures, and rename behavior | Declaring Files accepted |
| Runtime / Projects | Cross-platform and user acceptance; remote persistent-runner rediscovery remains deferred | Declaring integrated workflows accepted |
| Release | Native hosted arm64/Intel workflow, public release assets and tap, signing/notarization decision | Publishing a release |

Completed fixtures, credentials, imports, and transfer controls are recorded as completed in their milestone sections, not as carry-over work. Remaining user acceptance is tracked separately from implementation status.

## v0.1.0 - Host Management and Native SSH

Goal: manage common SSH host profiles and enter reliable native OpenSSH sessions.

- [x] Separate app state, UI rendering, SSH config discovery, and metadata into modules. Further domain/adapter extraction remains open.
- [x] Discover concrete aliases from `~/.ssh/config` and basic included config files. Full OpenSSH grammar and expansion support remain open.
- [x] Search and select hosts.
- [x] Add a host from the TUI with alias, hostname, username, port, and identity file.
- [x] Edit and delete basic hosts created or managed by LazyTerm.
- [x] Write basic host fields safely without exposing or storing passwords.
- [x] Preserve unrelated comments, `Include` directives, advanced OpenSSH options, and unmanaged host blocks when updating managed entries. Full OpenSSH grammar support remains out of scope.
- [x] Show confirmation before deleting a managed host.
- [ ] Fully validate alias conflicts across included files; keep overwrites refused unless explicitly confirmed.
- [x] Support tag-based groups as separate, versioned LazyTerm metadata.
- [x] Clean up metadata for removed SSH aliases on startup/refresh; retain it when discovery fails.
- [ ] Support favorites.
- [x] Launch `ssh <alias>` with inherited terminal and return cleanly to the TUI.
- [x] Preserve OpenSSH host-key verification, identity files, `ssh-agent`, `ProxyJump`, and `ProxyCommand`.
- [x] Show connection exit status and useful errors.
- [x] Support macOS first and document the locally tested OpenSSH/Rust versions. Native multi-machine acceptance remains open.

Release gate: a clean install can discover or create a normal OpenSSH host, edit or remove a LazyTerm-managed host safely, connect to a host, and return to the TUI without corrupting the terminal.

### v0.1.0 Audit - 2026-11-13

The historical implementation is substantially complete; this is not a claim that v0.1 shipped or passed full user acceptance.

- Safe writing: backups, atomic replacement, private permissions on newly created files, protection of neighboring managed markers, and preservation of manually added advanced options/comments inside managed blocks are implemented and tested. Full OpenSSH grammar and every alias collision across complex Include expansion still need broader coverage.
- SSH acceptance: automated disposable fixtures cover password authentication, ProxyJump, and related connection flows. Complete manual acceptance of first-time host-key confirmation, changed-key rejection, key/agent authentication, ProxyCommand, and terminal restoration on success, failure, and interruption remains open.
- SSH responsiveness: preflight currently blocks the event loop and cannot be cancelled. It runs a remote `true` command, so restricted-command accounts may fail preflight despite supporting the intended session.
- Error handling: a missing/unlaunchable SSH executable stays in a TUI error dialog; managed process cleanup and recovery are implemented. Full cross-platform terminal/process acceptance remains open.
- Persistence: groups and tags are implemented; favorites are not. The legacy `view` metadata property remains readable but no longer selects separate Groups/Hosts pages.
- Environment: local checks ran with Rust 1.92.0 and system OpenSSH 9.9p2 (LibreSSL 3.3.6) on macOS 15.8.1 Apple Silicon. The release workflow targets macOS 15 Intel and Apple Silicon; its hosted native run is still a release gate.
- Release preparation and beta checks are tracked under v0.6; package versions do not imply a release has shipped.

## v0.2.0 - Integrated Files / SFTP

Goal: use the Hosts catalog for file workflows, without a second host database. The embedded directory browser, transfer controls, and shared authentication are implemented; this milestone records the progression from the initial browser to the integrated Files workflow.

- [x] Carry a stable selected-host alias from Hosts into other modules.
- [x] Add a native `sftp` handoff using that alias and restore the TUI on exit; this remains a compatibility bridge.
- [x] Retain host context across tabs, migrate it on in-app rename, and clear it when refresh confirms the alias is gone.
- [x] Verify the Hosts -> Files -> Hosts context flow end to end against a disposable SSH/SFTP fixture. Additional manual edge-case acceptance for external SSH config edits and refresh failures remains open.
- [x] Select and document `openssh` plus `openssh-sftp-client` for the embedded browser; retain OpenSSH configuration and strict host-key checking.
- [x] Share current Host resolution and saved-password lookup across native SSH and embedded SFTP.
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

An opt-in disposable `sshd` fixture covers the embedded SFTP transport and the application-level Hosts -> Files -> Hosts context flow. Broader manual refresh, external-config rename, and failure-path acceptance remains tracked separately. The former credential spike is complete: the Keychain-backed store and shared scoped authentication prompt are implemented; no `sshpass` dependency is used. First-time host-key approval in background/noninteractive workflows and encrypted private-key passphrase storage remain unsupported.

Implementation gate: SSH and SFTP share Host resolution and saved-password authentication. Full acceptance still requires the manual edge cases above; noninteractive first-use host-key approval and encrypted private-key passphrase storage are documented exclusions.

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

Project limits: app-scoped commands may run through the Hosts catalog, but persistent remote commands, interactive PTY, dependency ordering, and remote process rediscovery remain deferred. Only the latest persistent run per command has a saved status/output index; full run history and raw-log rotation remain deferred. Beta hardening adds forced-termination cleanup for managed process groups and state recovery for a killed independent runner. Persistent runs do not automatically relaunch after a crash, logout, or reboot; completing this slice does not declare the v0.5.0 release accepted.

## v0.6.0 - Beta Hardening

Goal: make the integrated v0.5 workflows recoverable, diagnosable, installable, and ready for user acceptance.

- [x] Add a cross-platform Host credential store for usernames and passwords, with a dedicated Credentials tab and reusable Host associations.
- [x] Manage OpenSSH key references and fingerprints, generate Ed25519 key pairs without overwriting existing files, and upload public keys through the existing Groups/Hosts catalog.
- [x] Reuse saved Host credentials from native SSH/SFTP, Tunnels, Snippets, and app-scoped remote Projects through target-scoped askpass. The secret stays in the OS credential store and is not written to config, arguments, environment, or logs.
- [x] Add first-use/rejected-password prompts to Tunnels, Snippets, Projects, native SSH/SFTP, embedded SFTP, and ProxyJump using one per-connection broker. Save a replacement only after OpenSSH confirms password authentication.
- [x] Configuration import/export and documented data locations. Credential import/export handles saved manager/Host passwords and Project Web Link passwords in a user-selected, password-protected age file; private-key export remains deferred.
- [x] Introduce additive metadata schema migration with version validation and a pre-migration backup using the existing bounded backup rotation.
- [x] Review diagnostics, credentials, exports/backups, process ownership and release packaging; fix repeated/quoted/header/stream-boundary redaction, mask Project Web Link Debug passwords, separate trusted OpenSSH auth logs from remote stderr, and upgrade rustix after advisory scanning. Document sensitive raw-command logs, dormant/maintenance dependency findings and remaining manual acceptance in docs/SECURITY-REVIEW.md.
- [x] Recovery behavior for killed TUI, Tunnel, and independent Project runner processes: ownership-pipe guards clean abandoned managed groups; surviving persistent runners reattach; dead runner records reconcile without signaling saved PIDs. Isolated SIGKILL tests cover cleanup, restart, and unrelated-process isolation.
- [x] Prepare macOS Universal Binary release builds from independently validated arm64/x86_64 slices, with ad-hoc signature/system-library/deployment-target checks and version/diagnostics smoke tests. The combined binary runs locally under both architectures; Intel execution is verified under Rosetta. Hosted native runs remain a publication gate.
- [x] Add installation/upgrade/backup/uninstall and support-matrix documentation, explicit-allowlist Universal Binary archive, SHA256SUMS and a generated Homebrew Formula using one download for Intel and Apple Silicon. Combine validated per-CPU builds with lipo, sign again, and verify the final package on both native CI runners. A published tap/assets, native-hosted acceptance and Developer ID/notarization remain external release steps; no release has been published.
- [ ] User testing of SSH, SFTP, credentials, tunnels, snippets, Runtime, and Projects.
- [x] Add the first Settings surface for theme, language, tab visibility, and metadata-only export.
- [ ] Complete English, Simplified Chinese, and Japanese localization for all workspace labels, dialogs, notices, validation errors, and help text.

0.6 implementation status: Host credentials, schema migration, configuration import/export, and encrypted credential import/export are implemented. Configuration import is a separate operation from importing the password-protected `.age` credential file. The configuration bundle merges SSH stanzas and metadata with incoming values taking priority for matching identities; it omits saved password values. The encrypted credential bundle carries saved manager/Host passwords and Project Web Link passwords; private-key export is not supported. The credential manager supports password CRUD, Host association/unlinking, key generation/import, direct association display, and confirmed public-key installation. Native SSH/SFTP, Tunnels, Snippets, remote app-scoped Projects, embedded SFTP, and public-key upload share scoped askpass prompts for target and ProxyJump Host passwords. First-use and rejected-password prompts are integrated; a password is saved or replaced only after OpenSSH confirms successful password authentication. Private-key passphrase storage, first-time host-key approval in noninteractive workflows, and full user acceptance remain open. Opt-in disposable SSH fixtures verify key installation as well as real password authentication across ProxyJump, native SSH, SFTP, Tunnel, Snippet, and Project paths. Ordinary configuration exports omit saved password values and private key files.

Configuration import automatically merges with incoming values taking priority: same-name Host stanzas are replaced, local-only aliases and metadata entries survive, and LazyTerm entities use UUIDs matched before name fallback. Existing numeric metadata IDs migrate to UUIDs with a pre-migration backup and their internal references are rewritten. There is one overall import confirmation, with no per-conflict prompts. Host renames, three-way synchronization, and deletion propagation are deferred until Host identity and shared revision history are available.

Recovery note: independent guards anchor managed process groups and monitor a private ownership pipe. TUI death cleans app-scoped Project/Tunnel/native-master groups with TERM followed by KILL after two seconds; persistent Project runners survive TUI death. Runner death cleans its command group, then status polling/startup reconciles the private record only after the runner lock is released and the group is confirmed empty, removes the abandoned control socket, and allows restart. Real subprocess tests kill an App owner and runner, check port release and unrelated-service isolation, and verify persistent reattachment and duplicate prevention. The opt-in disposable SSH fixture kills a real Tunnel and its App owner, verifies App failure state/port release, and restarts the same forwarding configuration. Arbitrary daemonized processes outside the owned group, remote process rediscovery, and pre-guard runs are excluded; cross-platform manual acceptance remains open.

## v0.7.0 - Git-Based Configuration Sync

Goal: synchronize LazyTerm and OpenSSH configuration across a user's devices through a Git service they already use, without LazyTerm-operated sync infrastructure.

- [x] Use the installed Git CLI and a user-configured remote/branch; remain provider-agnostic (GitHub, GitLab, CNB, self-hosted Git, or another compatible remote).
- [x] Keep sync state in a dedicated LazyTerm-managed local checkout, separate from the user's working repositories and live `~/.ssh` files.
- [x] Define an explicit versioned sync manifest: SSH config and readable in-tree Includes, Host groups/tags, Tunnels, Snippet packages, Projects, and portable preferences. Machine-local Includes outside `~/.ssh` remain local and are excluded from exports/sync. Credentials, process state, logs, Keychain contents, and private-key files are excluded; obvious inline credentials in synchronized content are rejected.
- [ ] Reuse the existing encrypted credential archive as a separately opted-in `.age` payload; never commit plaintext credential data or Git authentication tokens.
- [x] Reuse Git's configured SSH agent/credential helper for remote authentication; LazyTerm does not collect or persist Git passwords/tokens.
- [x] Track a common base snapshot and compare base, local, and remote changes before applying anything.
- [x] Merge non-overlapping SSH sections and UUID-identified LazyTerm entities; represent deletion through sync history.
- [x] Detect same-field divergent edits and delete/modify conflicts. Stop before applying live files and require a per-conflict local/remote choice.
- [x] Validate the reviewed result before push, then apply through the existing backup, atomic-write, and rollback paths; advance the base snapshot only after successful application.
- [ ] Complete recovery for process termination at every push/apply boundary and add a visible sync status/history view in Settings.
- [x] Provide remote setup, connection test, and manual sync controls in Settings. Retry is explicit; status/history remains pending.
- [ ] Expand disposable bare Git tests to cover interrupted sync at every boundary, Include deletion, malformed/oversized manifests, more than six conflicts, and rollback failures.
- [ ] Document private-repository visibility, credential encryption, data scope, backup, remote migration, and recovery.

Initial implementation order: manifest and scope rules; Git subprocess adapter and isolated checkout; base-revision/change planner; conflict UI and transactional apply; credentials payload opt-in; disposable two-client acceptance. Do not implement sync by repeatedly running the current import-priority merge: that merge intentionally has no base revision or deletion propagation and is not a three-way sync algorithm.

Current verification uses a disposable local bare Git repository only. Before relying on a hosted remote, manually verify provider authentication, private-repository permissions, backup restoration, and expected conflict choices. Use a dedicated private repository or branch containing no unrelated files; LazyTerm refuses branches with unrelated files. SSH configuration is synchronized as text, so review it for sensitive custom directives before the first push.

Release gate: two installations can sync the same repository without overwriting unreviewed concurrent changes, leaking credential data, or corrupting either live SSH configuration.

### Deferred v0.7 Work

Development moves to v0.8 by user decision on November 13, 2026. The initial sync implementation is testable, but v0.7 is not declared complete or accepted.

- [ ] Add explicit opt-in encrypted password synchronization using the existing `.age` archive, including unlock, failure, and local Keychain restore behavior.
- [ ] Recover interrupted push, local apply, and baseline updates without losing local changes or advancing an uncommitted baseline.
- [ ] Show last attempt, last successful sync, current operation, and bounded failure/history information in Settings.
- [ ] Add fixtures for Include deletion, malformed/oversized manifests, many conflicts, interrupted transactions, rollback failures, and preservation of local-only Include context.
- [ ] Complete documentation for scope, Git authentication, private repository visibility, backups, recovery, and changing remotes/branches.
- [ ] Perform actual two-device acceptance: first merge, bidirectional edits, deletion propagation, conflicts, local credential retention, and machine-local SSH configuration.
- [ ] Handle a synchronized `IdentityFile` whose private key does not exist locally: show the missing-key state and offer local key selection or generation/deployment without claiming authentication is ready.
- [ ] Decide whether private-key migration/synchronization is a supported feature. This is a pending product decision, not an approved implementation task. Until decided, private-key files stay excluded; do not silently include them in configuration or password sync.

## v0.8.0 - Agent-Friendly LazyTerm

Goal: let a user-approved Agent discover the Hosts, Projects, and local/remote runtime context LazyTerm already manages, and perform narrowly scoped actions through a stable interface.

- [x] Provide an Agent Skill with installation/use guidance and clear capability/safety boundaries.
- [x] Provide CLI subcommands with versioned JSON output, backed by LazyTerm services rather than TUI screen scraping. MCP is outside v0.8 scope.
- [x] Provide validated configuration create/update commands for Hosts/groups, Snippet packages/templates, Projects/commands/web links, and Tunnel definitions. Agents can translate a user's description into LazyTerm configuration; reuse existing SSH config writers and metadata transactions rather than editing JSON directly.
- [x] Keep Hosts/SFTP operational actions outside the Agent CLI: no interactive SSH login, arbitrary remote command execution, file upload/download, remote file mutation, or SSH key deployment through these endpoints. Agents use their own SSH/SFTP tools with the queried Host information.
- [x] List Host aliases and safe connection metadata, Projects/commands/web links, and observed local listeners with process origin and ownership evidence.
- [x] Add explicit `runtime list --host ALIAS` remote Runtime inspection with catalog validation, BatchMode and strict host-key verification, without introducing a generic Host execution endpoint.
- [x] List Snippet packages, descriptions, and command templates for Agent reference. Agents execute approved commands through their own tools; the Snippet UI runner is not their execution API.
- [x] Expose managed persistent Project status and bounded, redacted output summaries.
- [x] Add explicitly authorized Local Project start/stop for both App/Persistent and Task/Service definitions. CLI runs use a background runner without editing saved policies or types; expose actual run lifecycle and reject stale run IDs during managed-run stop.
- [x] Extend CLI lifecycle/status/output to saved Host-targeted Project commands through a local authenticated runner and a connection-scoped remote process-group supervisor. Remote Python 3 and noninteractive SSH authentication are required; disconnect/heartbeat loss cleans the remote group. This is not remote detached-runner installation or rediscovery.
- [x] Detect recognizable Local external Project processes in status/Runtime, block duplicate starts, and stop one exact opaque external run ID after directory/user/start-time/command revalidation. External output capture, arbitrary PID stop, and remote external-process control remain excluded.
- [x] Require `--yes` for each process action and revalidate authenticated managed-run identity immediately before stopping. This flag expresses client authorization; the Skill must respect the user's actual authorization.
- [x] Omit saved credential fields, private key contents, Keychain data, runner authentication sockets/tokens, and raw unbounded logs. Redact recognized secret patterns in command/output text; arbitrary unlabelled application secrets cannot be guaranteed detectable.
- [x] Return versioned typed results, stable entity UUIDs/Host aliases, opaque run-ID strings, errors, and operation status.
- [x] Test real CLI subprocesses with disposable configuration/Projects, including stale run IDs, denied actions, missing Hosts, TUI-lock independence, credential rejection/redaction, and unrelated-process isolation.
- [x] Validate CLI writes, preserve unrelated SSH directives/metadata/child entities and saved link passwords, and reject stale metadata/Host saves through shared safeguards.
- [x] Automatically refresh CLI-authored metadata across TUI views, defer replacement while an edit/confirmation dialog is open, preserve data and block saves after missing/invalid metadata, and retain stale-save protection. A real PTY TUI/CLI fixture covers live refresh and deferred update after closing a form; additional Agent-client/platform manual acceptance remains a release gate.
- [x] Document CLI usage, JSON contracts, Skill installation, permissions, privacy, and troubleshooting.

Initial implementation: `docs/CLI.md` and `skills/lazyterm/SKILL.md`. No separate
Agent database or MCP server is introduced. Local inventory and persistent runner
actions are implemented, together with explicit remote Runtime inspection,
Host-targeted Project lifecycle control, recognized local external-run stop and
live TUI refresh. Configuration deletion/Host alias rename, arbitrary PID kill,
remote external-process management, remote detached-runner rediscovery and
complete Agent-client/platform manual acceptance remain outside this slice.

v0.8 completion checks: real CLI subprocess fixtures cover Local App/Task and
Persistent/Service runs, recognized external commands, duplicate/stale-run
rejection and a live PTY TUI observing CLI changes without overwriting an open
form. `LAZYTERM_SFTP_FIXTURE=1 cargo test --test cli` additionally starts a
disposable real sshd and verifies explicit remote Runtime and Host Project
start/output/stop/restart, including remote port cleanup after local runner
SIGKILL. No live user configuration is used.

The Skill is the Agent-facing instruction layer; the CLI provides configuration access and approved Project/process control. An Agent with shell access calls the same `lazyterm` executable using subcommands and `--json`; launching without subcommands continues to open the TUI. MCP is deferred and is not required to expose the planned capabilities. Prioritize inventory and attribution ("which Hosts exist?", "what is listening?", "which Project started it?"), followed by configuration authoring and scoped lifecycle actions. Host discovery/configuration must not launch SSH or SFTP sessions. Project commands may still use LazyTerm's existing local/Host targets when explicitly started through the approved Project lifecycle interface; that does not introduce an arbitrary Host execution endpoint.

Storage: CLI and TUI share the existing OpenSSH configuration, LazyTerm metadata, and managed runner state. Do not create a separate Agent copy of Hosts or Projects, and do not persist inventory snapshots as authoritative state. Keep the distributable Skill in `skills/lazyterm/SKILL.md` in this repository; installing it copies instructions into the Agent client's own skill directory, not application configuration or credentials. Runtime inspection reads live process state.

Implementation sequence:

1. Add explicit CLI routing independent of the TUI single-instance lock, with help, stable exit codes, JSON results on stdout, and diagnostics on stderr. Use shared read services, avoid configuration reconciliation/writes during inventory, and return actionable errors for invalid configuration.
2. Implement typed, read-only subcommands for Hosts, Snippet templates, Project definitions, and managed service status (for example, `lazyterm hosts list --json` and `lazyterm projects status --json`). Use allowlisted response fields, credential redaction, stable entity UUIDs, and Host aliases; report missing local key files without returning key contents or credential references.
3. Add local Runtime inspection and bounded, redacted Project output. Remote Runtime inspection, if exposed, must be an explicit Runtime operation identifying the requested Host; generic inventory and Host queries must not connect to servers or trigger interactive authentication.
4. Test real CLI subprocesses as an Agent would invoke them, including help/discovery, JSON parsing, exit codes, operation while the TUI is open or closed, malformed arguments, missing files, and credential-bearing commands/output.
5. Add configuration create/update commands with structured inputs, validation, backups, and stale-write protection. Hosts write standard fields to SSH config and supplemental fields to LazyTerm metadata. Queries and configuration writes must work with the TUI open or closed without silently discarding either writer's changes.
6. Provide an installable Agent Skill and client setup instructions describing how to discover state, author configuration, use Snippets as templates, and use the Agent's own SSH/SFTP tools for server/file operations.
7. Build approved Project/process start/stop actions after the read-only and configuration interfaces are validated. Keep process identity checks, ownership boundaries, and explicit action authorization.

Private-key synchronization remains a v0.7 decision and does not block v0.8 discovery. No Agent tool should receive private keys, saved passwords, or permission to synchronize credentials.

Release gate: an Agent using the CLI can accurately discover LazyTerm state, author validated configuration, and operate approved Project/process actions without receiving credentials, losing concurrent configuration changes, or affecting unrelated processes. Hosts/SFTP server operations remain with the Agent's own tools.

## v0.9.0 - Stable Core

Goal: declare the core local workflow stable.

- [ ] The v0.1-v0.8 release gates are met or remaining exclusions are documented.
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
