# LazyTerm Architecture

## 1. Product boundary

LazyTerm is a lightweight, local-first Termius alternative presented as a keyboard-driven TUI.

The first target is:

```text
Host catalog
  + native OpenSSH sessions
  + SFTP file operations
  + independently managed SSH tunnels
  + snippets and batch commands
  + Keychain-backed credentials
```

The first target is not:

- a new SSH protocol implementation;
- an embedded terminal emulator;
- a cloud sync service;
- a team collaboration product.

This boundary is deliberate. OpenSSH already handles terminal modes, host key verification, `ProxyJump`, `ProxyCommand`, `ssh-agent`, port forwarding, and full-screen remote programs. LazyTerm should organize those capabilities before replacing them.

## 2. What to borrow from SSHub

SSHub combines read-only `~/.ssh/config` hosts with a managed host database, and brings sessions, SFTP, tunnels, credentials, and an optional headless CLI into one product. Its documented stack includes Ratatui/Crossterm, SQLite, `ssh2`/libssh2 for SFTP, and `keyring`; the embedded-session path adds PTY and VT100 emulation. These are useful product and boundary references, but LazyTerm should not copy its implementation wholesale. See the [SSHub README](https://github.com/Petyok/SSHub#readme) and [quickstart](https://github.com/Petyok/SSHub/blob/main/openwiki/quickstart.md).

| SSHub approach | LazyTerm decision |
|---|---|
| Hybrid SSH config and managed-host catalog | Adapt: discover existing OpenSSH aliases, support basic CRUD for LazyTerm-managed host blocks, and keep UI-only metadata separately. |
| Embedded PTY sessions and tabs | Defer: keep native terminal handoff until multiple live sessions are a proven need. |
| Integrated SFTP browser | Adopt the workflow; validate the transport separately because LazyTerm's terminal path is OpenSSH. |
| Tunnel definitions, status, and reconnect | Adopt, with explicit `session`, `app`, and `persistent` lifecycles. |
| Credential prompts and secure-store integration | Adopt the Keychain boundary; do not copy a plaintext-file fallback when no keychain is available. |
| Audit/session logs and broad headless CLI | Defer until core host, transfer, and tunnel workflows are reliable. |

SSHub is licensed under AGPL-3.0. It is fine to study its behavior and architecture, but copying or adapting its source code is a separate licensing decision; this document proposes independent LazyTerm interfaces and does not reuse SSHub code. See the [SSHub license](https://github.com/Petyok/SSHub/blob/main/LICENSE).

The parts worth borrowing are:

### 2.1 Explicit application state

The TUI should render from a single application state and turn key presses into actions. Rendering should not start SSH processes, read Keychain entries, or mutate the database.

```text
Input event
  -> Action
  -> Application service
  -> State update
  -> Render
```

This is more important than the specific Ratatui widget layout. It keeps the UI testable and prevents network or filesystem work from blocking the event loop.

### 2.2 Separate host configuration, credentials, and runtime sessions

SSHub distinguishes saved host data from active connections. LazyTerm should do the same:

```text
HostProfile      stable configuration
RuntimeSession   one current SSH/SFTP/Tunnel process
```

Never store process IDs or transient connection errors inside a host profile.

### 2.3 Basic host management in v0.1.0

The first release should let users create, edit, and delete common connection profiles from the TUI. The managed fields are intentionally limited to:

```text
Host alias
HostName
User
Port
IdentityFile
```

OpenSSH remains the connection and authentication authority. LazyTerm must never persist passwords in SSH config, application data, process arguments, or logs. Advanced directives such as `Match`, complex `Include` layouts, `ProxyCommand`, and per-host policy remain user-managed in `~/.ssh/config`; LazyTerm should not flatten or silently rewrite these rules.

For reliable CRUD, identify which host entries LazyTerm owns and update only those entries. Before overwriting an alias or deleting a managed entry, require explicit confirmation. Writes should be atomic and preserve unrelated content; implementation and release criteria must include backup or recovery behavior. Existing arbitrary `~/.ssh/config` entries may be listed and connected to, but should not be offered for destructive edits unless LazyTerm can preserve their full structure.

### 2.4 One persistence boundary

SSHub centralizes persistent settings and credentials instead of letting each screen write its own files. LazyTerm should have one storage boundary:

```text
Host catalog / metadata / snippets / tunnel / project definitions
Host credential store (platform secret service)
                          |
                     Repository
                          |
                 SQLite or versioned files
```

Secrets go through a separate `CredentialStore`; they must not be serialized into ordinary application records.

Metadata retains the exact file snapshot obtained on read. `save_to` holds an OS file lock across snapshot comparison, backup and atomic replacement. A stale snapshot, external deletion or corrupt replacement blocks saving; the user reloads through Hosts before retrying. Top-level unknown fields survive round trips. Before replacement, the last contents are copied to a private timestamped file in `hosts.json.backups/` (30 retained); the existing single backup remains for compatibility. A private append-only `hosts.json.writes.jsonl` records attempts, successful saves, failures and conflicts with PID, timestamp, caller and collection counts only. Unit tests cannot invoke the production save entry point and App tests require an explicit temporary metadata path. Older binaries do not participate in the lock or snapshot protocol and must be closed.

The interactive startup path acquires a nonblocking OS lock on `tui.instance.lock` in the same data directory before initializing the terminal or loading App state. Its guard lives through terminal restoration and clipboard cleanup. The file stores only owner PID and terminal; lock ownership, rather than PID liveness or file presence, determines exclusion. It stays in place on exit to avoid unlink/inode races. Askpass and `--project-runner` dispatch happen before this gate, allowing authentication and persistent tasks while a TUI is running. Cross-process tests verify duplicate rejection and release after process death.

### 2.5 Host credential storage

OpenSSH configuration remains the source of truth for standard Host fields such as `HostName`, `User`, `Port`, identity files, and routing options. LazyTerm metadata stores only UI data and opaque credential references. Host passwords never enter `.ssh/config`, JSON metadata, command arguments, environment variables, logs, or configuration exports.

The credential store provides platform adapters for macOS Keychain, Linux Secret Service, and Windows Credential Manager. A Host credential is keyed by a stable LazyTerm host identity plus the resolved alias, so renaming or reconciling an alias cannot silently attach a password to an unrelated Host. Username may be read from OpenSSH config and optionally saved with the credential record when the user chooses; password retrieval always requires an explicit connection operation.

SSH, SFTP, Tunnel, and remote Project connections use the same credential lookup path. Password authentication is supplied through the connection backend's supported prompt or authentication API; the application must never fall back to `sshpass` or place the secret in a process-visible argument. If the platform store is unavailable, the UI reports the failure and allows an interactive one-time password entry without persisting it.

### 2.6 Runtime process supervision

Tunnel processes, snippet commands, and native SSH sessions need a shared process policy:

- capture exit status and stderr;
- classify expected exit versus failure;
- avoid logging secrets;
- terminate the correct process group;
- preserve persistent tunnels when the TUI exits;
- rediscover running persistent tunnels on the next launch.

### 2.7 Runtime inspection

The Runtime view inspects listening TCP sockets on the local machine or on a selected OpenSSH Host. It does not replace Tunnel definitions: Tunnels own forwarding processes that LazyTerm starts, while Runtime observes any listener and can request termination after confirmation.

Inspection runs in a worker thread so the TUI event loop remains responsive. The local and remote adapters use a fixed command policy: prefer `lsof` field output, then fall back to `ss` on systems that provide it. Remote inspection sends the selected alias to OpenSSH and never copies resolved hostname, user, identity, or proxy settings into Runtime state.

Runtime entries are transient observations. PID values are not stable identities and must be revalidated by a fresh inspection before future process actions. When permissions allow, the adapter enriches an entry with the full command line and current working directory using `ps` and `lsof` or `/proc`. External process termination sends `TERM`; a separate confirmed `KILL` action is available for entries that do not stop cleanly. Managed Project commands use the separate group lifecycle described below. Missing tools, permission limits, authentication failures, and unavailable process metadata are displayed as inspection errors or unavailable fields.

Runtime origin is independent of process ownership and system classification. A single `ps` snapshot per inspection supplies PID, PPID, PGID, terminal, and command. A bounded, cycle-checked ancestry walk recognizes shells, attached terminals, known Agent executables, and macOS app bundles. Agent ancestry takes precedence over its launcher; a CLI child inside a desktop terminal remains CLI. Missing or reparented ancestors leave origin unknown. Origin is a hint displayed with evidence, not authorization for automatic termination.

Runtime defaults to the `Mine` category, comparing process UID against `id -u` on the inspected machine and excluding recognized system services. `Apps`, `System`, and `All` expose broader views. These observations cannot establish Agent provenance. Filtering is shared by rendering, navigation, details, and termination confirmation so a visible selection always refers to the same process.

Runtime separates machine selection (`Local` and a catalog-backed `Hosts` picker) from process categories. Each target has a session-only cached list, filter, selected row, and scroll offset. Switching restores that view and starts a fresh inspection; termination remains disabled while loading or after an inspection failure. Results carry a monotonically increasing request ID as well as the target, preventing a delayed result from an earlier visit to the same machine from replacing current data. Refresh preserves the selected listener by PID, endpoint, and protocol when it remains present. Termination confirmation displays the machine and prevents target changes; missing catalog aliases cannot start new inspections. Rendering and mouse hit testing share the same target/category/list geometry.

Local listeners in a currently owned Project process group receive a green `PROJECT` origin, project/command IDs and names, and the current run ID. The UI validates this identity again when receiving worker results and before routing a confirmed stop to the Project supervisor. Remote PID/PGID values never match local ownership. The Runtime `o` action opens the linked command output; entering Runtime refreshes the observation so new project listeners are discoverable.

### 2.8 Project workspaces

`projects.rs` owns Project/command definitions and the local process supervisor; `project_app.rs` owns workspace navigation and forms. A Project has a stable ID, name, base directory, and named commands with stable per-project IDs, shell text, and optional working-directory overrides. Definitions are added to version-1 metadata with an empty default for old files. They share the existing atomic write/backup boundary and survive Host reconciliation. Failed saves roll back definition changes and retain form input.

Projects optionally persist ordered group names; each command references at most one group by name. Browsing derives groups from both explicit groups and command references, preserving empty groups and tolerating older or hand-edited definitions. Group rename updates all references atomically; group removal clears references without deleting commands. The browser is a default-expanded tree with group headers, indented commands, and ungrouped commands, plus an optional flat view. A session-only set keyed by Project ID and group name remembers collapsed groups across navigation and flat-view toggles. Rename migrates fold state after a successful save; removal discards it. Selection stores command identity separately from visible row position so output, mutations, and Runtime navigation resolve the intended command. Saving a command or following its Runtime link expands its group before selecting it. Services and Tasks are distinct, defaulting to Task for older definitions. Batch controls select only Services within the selected header/command's group or whole project.

Project definitions also contain an additive `links` array of stable ID, name, and URL records; older definitions default to no links. Tree views place links under a distinct default-expanded `Web links` header above command groups. This header uses a typed workspace item and separate per-Project session fold state, avoiding collisions with user-defined command groups; flat views omit headers. Link and command rows share name styling, child indentation, and muted secondary information. Creating/editing a link expands the header and resolves selection through workspace-item identity. Their forms and confirmed deletion share the metadata save/rollback path, and editing a Project preserves its links. Neither link nor link-header selection resolves to a command or a service batch scope. The `url` parser validates HTTP/HTTPS addresses on save and again before opening imported metadata; `webbrowser` handles platform browser launching on a background worker, with completion/errors polled by the App. URLs are literal browser destinations and are independent of the Project's execution target.

Links have optional username and password fields plus a storage choice. Keychain storage uses an opaque password reference and the `keyring` crate stores the secret under the dedicated `lazyterm.project-links` service, using a random reference per replacement. Config-file storage writes the password to `hosts.json` as plaintext and is intended only for low-sensitivity services; the form labels this choice explicitly and warns before saving. No password is embedded in a URL or passed on a command line. Forms mask new input, preserve existing passwords when blank, and zero their password buffer on drop. A worker prepares a new Keychain credential, then the UI saves metadata and commits the reference; a canceled or failed save discards only the prepared credential. Successfully saved edits/deletions asynchronously remove references no longer used by any Project link. Cleanup errors are surfaced; process crashes can leave orphaned keychain entries, and automatic orphan reconciliation is deferred. Credential operations never block the UI event loop. A persistent clipboard worker owns copied text, expires passwords after 30 seconds, and clears only matching clipboard contents; normal application exit requests the same cleanup. Unit tests use an in-memory credential store; the ignored `native_keychain_round_trip` is an opt-in native-store smoke test.

Settings can export referenced manager/Host passwords and Project Web Link passwords, including config-file passwords, to a user-selected `.age` file. The export asks for a separate passphrase, builds the payload in memory, encrypts it before writing, and excludes saved password fields and private key files from ordinary JSON configuration export.

### Credential manager implementation

Credentials is a top-level workspace tab alongside Hosts, Files, Tunnels, Snippets, Runtime and Projects. Settings controls its visibility and retains configuration/credential export; it is not the credential manager's entry point. The tab shares the normal numbered/function-key, cycle, previous-tab and Settings navigation. Hosts can still open a modal credential selector for assignment or upload; only its forms/pickers/confirmations own modal input on the main tab. Rendering keeps the top navigation visible while sharing manager content and dialogs between both entry points.

`credentials.rs` owns serializable credential metadata, OpenSSH key generation/import/discovery, target-scoped askpass, and public-key installation. `credential_app.rs` owns modal navigation, transactional password replacement, Host associations and worker completion; `credential_ui.rs` renders the manager, forms, Host picker and confirmations. Metadata adds default-empty `credentials` and `host_passwords` fields without replacing OpenSSH as the Host source of truth. The existing secret-store adapter routes manager references to `lazyterm.credentials` and legacy Web Link references to `lazyterm.project-links`; random references use the platform CSPRNG through `getrandom`. The Credentials tab and Host credential selector rescan `~/.ssh` on entry and support manual refresh. Discovery uses matching `.pub` and private-file paths, reads only public-key data, and does not enumerate third-party system credential entries. Project Web Link Keychain references are displayed as linked credentials, but remain edited and removed through Projects.

Passwords use prepare/save/commit semantics and unused-reference cleanup checks both manager records and Project links. Host reconciliation removes associations for confirmed missing aliases but preserves reusable credential records, and in-app rename migrates password associations. SSH keys store a canonical private-file path plus public material and fingerprint. Import uses `ssh-key` to read OpenSSH private-key public metadata and compares it with the fingerprint of the `.pub` sidecar. Generation reserves a new mode-0700 directory and invokes system `ssh-keygen` to produce an unencrypted Ed25519 pair (private file mode 0600 on Unix); this limitation is visible before saving. Private key files are never serialized or automatically removed. Key assignment changes only the chosen single-alias block's `IdentityFile` in its original root/Include file with the existing backup and atomic-write boundary; ambiguous/shared blocks are refused. Successful Host preflight connections run OpenSSH verbose diagnostics and persist aliases for which OpenSSH reports public-key authentication; local identity candidates alone do not count as successful key authentication. These observations and upload history are distinct metadata, and both are removed when a confirmed missing Host is reconciled.

Hosts `p` opens the associated password editor without reading the stored secret into the form. Password replacement retains the credential ID and Host associations. Hosts `x` opens a direct password deletion confirmation without opening the credential manager. It shares the same deletion transaction as Credentials: confirm affected Hosts and remove their associations with the record in a single metadata save; save failure restores both collections and leaves the Keychain secret untouched. Unused-secret cleanup runs only after saving and preserves references used by other credentials or Project Web Links. The deletion confirmation owns keyboard and mouse input so it cannot change tabs, select a different Host, or initiate a connection.

Host Add/Edit use one form and confirmation style. Fields are displayed as Alias, HostName, Groups, User, Port, and IdentityFile; only SSH connection fields are written to the OpenSSH Host stanza, while selected groups are stored in LazyTerm metadata. The Groups field opens a multi-select picker over persisted groups and existing Host assignments; a typed non-existing name can be created and added. Creation inside a group preselects that group, and edits load current assignments. In the Hosts overview, lowercase `a/e/d` act on the selected item: a selected group opens group create/rename/delete, while a selected Host opens Host add/edit/delete. Entering a group and adding a Host preassigns it to that group. Rename updates all Host assignments; delete removes the group and assignments but never deletes Hosts. Empty groups remain persisted in `host_groups`, which is normalized with legacy tag assignments on load and save.

Public-key upload requires an explicit confirmation and runs a bounded OpenSSH worker using the Host alias, strict host-key checking and existing routing options. The controlled remote script preserves existing authorized keys, compares type/blob before appending, sets private permissions and refuses symlink targets. A disposable SSH fixture installs the new key twice and authenticates using it without touching the user's actual authorized_keys. Native SSH/SFTP and upload retrieve saved target passwords through the app executable acting as `SSH_ASKPASS`; only opaque references and the resolved prompt target enter the child's environment. The helper answers only the exact resolved `user@hostname` password prompt, refusing jump-host, host-key and private-key passphrase prompts. Embedded SFTP/Tunnel/remote Project password integration, interactive challenges, first-use host-key confirmation, encrypted-key passphrase storage, password-auth fixture coverage and the cross-platform native-store acceptance matrix remain pending.

The additive target model stores a Project default (`Local` or `Host(alias)`) and optional per-command overrides. Target choices come from the existing catalog without copying SSH settings. Remote definitions are preserved when Hosts disappear. Remote execution is deliberately unavailable in this phase: the application refuses remote actions before batching, the supervisor rejects them before launching a process, and external recognition cannot associate them with local processes. Remote paths are stored without local canonicalization; their actual existence must be validated when remote execution is implemented.

Execution uses the standard process API with `sh -c`, a validated directory, inherited environment, null stdin, and a new Unix process group. Commands do not start from navigation, tab changes, or application startup. Start, stop, restart, and start/stop-all are explicit actions. Stop sends group TERM and escalates to KILL after two seconds; restart waits for group cleanup and both output readers. If the shell exits while children retain its pipes, the remaining group is also cleaned up. Zombie-only groups do not block completion. App-scoped Drop kills remaining owned groups and reaps direct children. Daemonized processes that leave the group and app-scoped recovery after SIGKILL of LazyTerm itself remain unsupported.

The additive command lifecycle defaults to `App` (`Stop on exit`). `Persistent` (`Keep running`) launches the same executable in a private `--project-runner` mode before terminal initialization. The runner detaches with `setsid`, null stdio, and supervises one ordinary app-scoped command using the existing output readers and TERM/KILL handling. It holds an exclusive per-command `flock`, exposes a Unix socket in a short, private `/tmp` directory, and authenticates control requests with a random run token. Atomic mode-0600 records under `project-logs` store the run identity, real command/directory, log paths, status, and final bounded output tail; intermediate output is streamed through the socket. Status snapshots are queried on workers without blocking the event loop. The TUI reattaches local persistent definitions on startup and periodically, retains runner-owned process groups for Runtime links, and sends stop/restart through authenticated control requests instead of signaling persisted PIDs. A replaced run updates cached identity, protecting stale Runtime confirmations. Dropping a TUI handle leaves the runner and its output readers alive. Completed runner state remains readable after the socket is removed. Deleting a stopped command or Project removes its completed run index only after metadata saves successfully, retaining raw logs; the runner lock protects live records. An unreachable active runner stays reserved with an error; automatic recovery of a killed runner, log rotation, full run history, and reboot autostart remain deferred.

Dedicated readers write complete raw stdout/stderr to private per-run files and send bounded chunks over a bounded channel. The event loop drains a bounded number of chunks per frame and keeps a UTF-8-safe 256 KiB display tail. A streaming `vte` parser removes terminal controls for plain output; this is a log view rather than an interactive terminal. The view follows the tail until scrolled, preserves scroll across tab changes, and shows status and executed directory. Full log paths appear in details. Logs live under the metadata directory's `project-logs/`, with directory mode `0700` and file mode `0600`, and are retained when rerunning or deleting a definition. Rotation and persistent run indexing remain deferred.

`project_monitor.rs` observes external commands separately from the owning supervisor. A single-flight worker scans every three seconds, matching effective UID, canonical working directory, and normalized command tokens. npm launch argument rewriting is supported; descendant processes supply listening ports. Managed process groups and the application's ancestors are excluded. Ambiguous definitions are not associated. Returned snapshots carry definitions and managed owner identities; changed definitions or run generations invalidate stale results. Missing inspection data is reported, and actions waiting for inspection fail closed.

Start/start-all and restart wait for a fresh scan before execution. Current external-running status takes priority over a previous managed exit result, which remains separately labeled. External observation never grants lifecycle ownership or output capture. Runtime links external listeners by observed PID and command only on the local target; expired links cannot fall through to a stale generic termination action.

External stop/restart presents an explicit confirmation. Before signaling, the worker revalidates the root's PID, UID, start time, command, and directory, then snapshots descendants. It sends TERM only to individually revalidated processes, protecting unrelated processes that share the terminal process group. External stops do not escalate to KILL; timeout cancels restart. Successful completion triggers a fresh inspection before the requested action, so a replacement external process requires confirmation again. Monitor Drop leaves external processes running.

### 2.9 Capability-focused tests

Connection features should be tested through narrow interfaces and fake implementations:

- fake host repository;
- fake credential store;
- fake process supervisor;
- fake SFTP client;
- fake clock for reconnect backoff.

Do not make unit tests depend on a real SSH server, a real Keychain, or the user's `~/.ssh/config`.

Project process tests launch the test binary as a disposable local server in a temporary directory, bind an OS-assigned loopback port, and remove the fixture on completion. They verify cwd, stdout/stderr, Unicode and ANSI decoding, log retention beyond the display cap, exit codes, concurrent tasks, duplicate rejection, TERM escalation, restart, Drop cleanup, metadata compatibility/backup/failure, and Projects -> Runtime -> output -> confirmed stop. Pure ownership tests cover child PGIDs and remote PID collisions. Nonempty workspace and output rendering is checked from 32x12 through 120x40.

The opt-in SSH fixture also validates remote Runtime inspection against a temporary `sshd` listener, using an isolated client config and host key file. The fixture is disposable and never uses the user's SSH configuration.

Persistent fixtures launch an independent runner subprocess and exercise TUI Drop/reopen, output generated while no TUI exists, exit-status restoration, duplicate prevention across handles, exclusive runner locking, token rejection, restart/TERM escalation, replacement discovery from another window, and Runtime ownership/output/stop after reopening. Test cleanup explicitly stops background fixtures; lifecycle form changes and legacy defaults are checked without launching user commands.

External fixtures run outside the supervisor and deliberately share the test process group. Tests cover conservative command matching, descendants and ports, ambiguous definitions, stale definition/owner snapshots, identity revalidation, non-owning monitor Drop, and individual-process stopping. The App flow retains a previous failed managed run while an external server runs, rejects a duplicate launch, follows its Runtime output link, cancels a stop, then confirms a restart into a new managed generation.

## 3. LazyTerm target architecture

```text
src/
  main.rs
  app/
    state.rs
    action.rs
    reducer.rs
    commands.rs
  ui/
    layout.rs
    hosts.rs
    tunnels.rs
    snippets.rs
    dialogs.rs
  domain/
    host.rs
    credential.rs
    session.rs
    tunnel.rs
    snippet.rs
    error.rs
  services/
    host_service.rs
    session_service.rs
    sftp_service.rs
    tunnel_service.rs
    snippet_service.rs
    reconnect_service.rs
  ports/
    host_repository.rs
    credential_store.rs
    ssh_client.rs
    sftp_client.rs
    process_supervisor.rs
    clipboard.rs
  adapters/
    openssh/
    sftp/
    keychain/
    persistence/
    processes/
```

The names are illustrative. The important rule is dependency direction:

```text
UI -> application services -> ports -> adapters
                  \-> domain models
```

Adapters must not call the TUI directly. They report results through service events or returned errors.

## 4. Domain model

### 4.1 HostProfile

```text
HostProfile
  id: HostId
  alias: String
  hostname: String?
  port: u16?
  username: String?
  source: OpenSsh | LazyTerm
  group_id: GroupId?
  tags: Vec<String>
  favorite: bool
  default_directory: Option<String>
  ssh_config_alias: Option<String>
  credential_ref: Option<CredentialRef>
  keepalive: KeepalivePolicy
  reconnect: ReconnectPolicy
```

`hostname`, `port`, username, identity files, `ProxyJump`, and `ProxyCommand` may be inherited from OpenSSH. The model should preserve the alias instead of flattening every value too early.

### 4.2 RuntimeSession

```text
RuntimeSession
  id: SessionId
  host_id: HostId
  kind: InteractiveSsh | Sftp | Tunnel
  state: Starting | Connected | Degraded | Reconnecting | Stopped | Failed
  started_at: Instant
  last_error: Option<ConnectionError>
```

Interactive SSH is initially a handoff to an OpenSSH child process. The TUI waits for it and resumes when it exits. SFTP and Tunnel are service-managed runtimes.

### 4.3 TunnelDefinition

```text
TunnelDefinition
  id: TunnelId
  host_id: HostId
  kind: Local | Remote | Dynamic
  bind_address: String
  local_port: u16
  remote_host: Option<String>
  remote_port: Option<u16>
  lifecycle: Session | App | Persistent
  auto_reconnect: bool
```

## 5. Main services

### 5.1 HostService

Responsibilities:

- load OpenSSH aliases;
- merge OpenSSH config with LazyTerm metadata;
- search, group, tag, favorite, and edit hosts;
- expose an effective connection profile to other services.

It should not own active SSH processes.

### 5.2 SessionService

Responsibilities:

- build a safe OpenSSH invocation from an effective host profile;
- launch an interactive `ssh` process with inherited terminal descriptors;
- return the terminal to Ratatui when the process exits;
- classify exit status and connection errors.

The first version should prefer invoking an alias:

```text
ssh <alias>
```

This preserves the user's complete OpenSSH configuration. Only add explicit `-o` overrides for settings that LazyTerm owns, such as a validated timeout or Keepalive policy.

### 5.3 SftpService

The first v0.2 slice uses `openssh` plus `openssh-sftp-client`. `openssh` launches the system OpenSSH connection with the selected alias, while `openssh-sftp-client` speaks SFTP v3 over the subsystem and provides directory and file operations. This reuses `~/.ssh/config`, Includes, ProxyJump, agent/key routing, and known-host checking without implementing the protocol ourselves. The current UI implements directory listing, basic metadata, entering directories, and returning to the parent.

The system `sftp` client remains available as a native handoff for interactive password prompts, first-time host-key confirmation, and other cases where the library's non-interactive session cannot proceed. It is a compatibility path, not the embedded browser. See the [OpenSSH sftp manual](https://man.openbsd.org/sftp).

The service should provide:

- list directory;
- stat;
- upload;
- download;
- mkdir;
- rename;
- remove;
- cancellation and progress.

The intended contract is one shared host reference and credential policy. The current listing slice resolves the same alias and OpenSSH config but requires key/agent authentication and a previously trusted host key (`StrictHostKeyChecking=yes`); it does not yet share interactive password prompts with the native SSH/SFTP handoff.

### 5.3.1 Shared host context

The workspace retains an optional host alias independently of the Hosts cursor, group, or search. An explicit connection from Hosts or a selection in the Files picker updates that context; merely switching tabs never infers a host from the current cursor or connects.

Each operation resolves the alias against the current catalog. A successful refresh that removes the alias clears the context, while discovery failure retains it and blocks new operations. In-app rename migrates the context. An external rename is deletion plus addition and requires explicit selection of the new alias. Future persisted tunnel/snippet references use this same policy; missing references are disabled, not retargeted.

No module copies hostname, user, port, identity, or proxy fields into a separate host store. OpenSSH receives the alias to resolve inherited settings at execution time.

### 5.4 TunnelService

TunnelService owns tunnel definitions and runtime state. It starts OpenSSH with `-N` and the relevant forwarding options, then delegates lifecycle to ProcessSupervisor.

```text
TUI action
  -> TunnelService.start(id)
  -> ProcessSupervisor.spawn()
  -> RuntimeSession state
  -> UI status update
```

Tunnel lifecycle must be explicit:

- `session`: tied to a selected SSH workflow;
- `app`: stopped during normal TUI shutdown;
- `persistent`: detached and rediscovered after the next launch.

### 5.5 SnippetService

Snippet modes:

```text
Run       execute one remote command
Batch     execute across selected hosts
Connect   compose an initialization shell command
Copy      copy the rendered command
```

Use a shell-safe argument model. Do not build one unescaped shell string from user fields. For a single remote command, pass the command as the intended OpenSSH remote command and show a preview before execution.

### 5.6 ReconnectService

Keepalive and reconnect are separate:

```text
OpenSSH ServerAlive*
  -> detects an unresponsive connection

ReconnectService
  -> applies policy after the process exits or a tunnel fails
```

Suggested policy:

```text
1s -> 2s -> 5s -> 10s -> 30s
```

Do not automatically retry authentication failures, host-key changes, malformed configuration, or local port conflicts. User-initiated stop always cancels reconnect.

## 6. Credentials and macOS

The credential boundary should be explicit:

```text
CredentialStore
  -> macOS Keychain adapter
  -> future Secret Service / Windows adapter
```

Store only opaque references in LazyTerm's database. The application may retrieve a password briefly for a library-backed SFTP connection, but it must never put the secret in:

- process arguments;
- environment variables;
- ordinary TOML/JSON records;
- stdout, stderr, or debug logs.

Important implementation constraint: native OpenSSH password automation is not as clean as key-based authentication. The first SSH handoff should allow OpenSSH to show its own password prompt. Keychain-backed automatic password entry should be treated as a separate design spike, using a controlled askpass mechanism or a native SSH library. Do not introduce `sshpass`.

## 7. OpenSSH integration

### 7.1 Host configuration

The initial integration should use OpenSSH aliases wherever possible:

```text
LazyTerm host selection
  -> effective alias
  -> ssh alias
```

This gives immediate support for:

- `IdentityFile`;
- `IdentitiesOnly`;
- `ProxyJump`;
- `ProxyCommand`;
- `Include`;
- `known_hosts`;
- `ControlMaster`;
- user-specific OpenSSH options.

For display, use `ssh -G <alias>` to inspect the effective configuration without attempting a connection. Parse only the fields LazyTerm needs and keep the original alias for execution.

### 7.2 Timeout and Keepalive

These are host policy values, not reconnect logic:

```text
ConnectTimeout
ServerAliveInterval
ServerAliveCountMax
TCPKeepAlive
```

`ConnectTimeout` bounds connection establishment. `ServerAlive*` detects an established SSH connection that no longer responds. Reconnect policy decides what happens after failure.

### 7.3 ProxyJump

For the first version, visualize and preserve `ProxyJump`; do not implement the jump network layer ourselves.

```text
Host internal-db
  ProxyJump bastion
```

The effective configuration must allow separate credentials and failures for the jump host and the target. Error reporting should identify which hop failed.

## 8. Event and concurrency model

The TUI event loop must stay responsive while SFTP, snippets, and Tunnel supervision run:

```text
Input / timer
  -> Action
  -> service task
  -> ServiceEvent
  -> reducer
  -> render
```

Use bounded channels for service events. Long-running work belongs in worker tasks or threads. The UI should display `Starting`, `Transferring`, `Reconnecting`, and `Failed` rather than block on a synchronous network call.

The current SFTP adapter uses a short-lived Tokio runtime inside a worker thread and returns listing and transfer events through channels. The Ratatui event loop remains non-blocking, and the Files view keeps a bounded transfer history with a progress bar, percentage, and terminal status. The metadata store also remembers the local path, the last remote path by host alias, and the shared hidden-file setting. A supervised worker model and cancellation protocol remain future work.

## 9. Persistence layout

Keep user data under the platform data directory, not in the repository:

```text
database:
  hosts metadata
  groups and tags
  snippets
  tunnel definitions
  UI preferences

Keychain:
  passwords
  private-key passphrases
```

The storage layer needs schema versioning from its first migration. It should support importing OpenSSH aliases without modifying `~/.ssh/config`.

## 10. Implementation order

### Phase 1: architecture foundation

- split the current `main.rs` into app, UI, and domain modules;
- define HostProfile, RuntimeSession, TunnelDefinition, and Snippet;
- define repository and credential interfaces;
- add a fake in-memory repository for tests;
- add OpenSSH config discovery and `ssh -G` inspection.

### Phase 2: native SSH

- launch `ssh <alias>` with inherited terminal;
- return cleanly to Ratatui;
- surface exit status and common errors;
- add host search and favorite/group metadata.

### Phase 3: integrated Files

- retain shared host context across modules and validate references;
- add native SFTP terminal handoff as a small usable bridge;
- implement the initial dual-pane directory browser: local files stay visible, while the remote pane reuses the Hosts catalog and offers a Groups/Hosts picker;
- add transfers, progress, cancellation, and mutations around the same service.

Keychain and automatic saved-password entry are a separate deferred track, not a dependency for starting Files with native prompts or agent/key authentication.

### Phase 4: tunnels

- implement local forwarding first;
- add process groups, PID/status tracking, and port checks;
- add app and persistent lifecycles;
- add reconnect backoff and rediscovery.

### Phase 5: snippets and diagnostics

- single-host snippets;
- batch execution;
- preview, confirmation, output capture, and cancellation;
- timeout/Keepalive controls;
- ProxyJump display and hop-specific errors.

### Phase 5a: Runtime and Project workspaces

- inspect local and remote listeners through the Hosts catalog;
- add confirmed process termination with platform-specific diagnostics;
- define local Projects with base paths and named commands;
- supervise project process groups and capture complete output;
- keep Project execution separate from Snippet command records.

### Phase 6: reassess embedded sessions

Only add embedded PTY sessions after measuring a real need for multiple live terminals, tab switching, or command injection into an active session. Until then, native OpenSSH gives better compatibility for less code.

## 11. Architectural decisions

Development follows integrated vertical slices, not strict release acceptance order. The carry-over backlog in `ROADMAP.md` retains incomplete and unverified work. Safety invariants remain mandatory; starting the next milestone does not claim the previous release is accepted.

1. OpenSSH is the default interactive SSH and Tunnel backend.
2. SFTP reuses the OpenSSH alias; the embedded adapter currently requires a trusted host key and key/agent auth, with credential-policy unification still open.
3. The TUI projects application state; dedicated supervisors own Tunnel and Project processes, and credentials use a separate store.
4. Persistent tunnels are supervised independently of the interactive SSH handoff.
5. Password support starts with safe prompts and Keychain storage; automatic native SSH password injection is a separately validated feature.
6. Reconnect, Keepalive, and timeout are separate policies.
7. Embedded terminal support remains optional and deferred.
