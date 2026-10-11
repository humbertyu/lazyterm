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

### 2.2 Separate host configuration from runtime sessions

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
Host catalog / metadata / snippets / tunnel definitions
                          |
                     Repository
                          |
                 SQLite or versioned files
```

Secrets go through a separate `CredentialStore`; they must not be serialized into ordinary application records.

### 2.5 Runtime process supervision

Tunnel processes, snippet commands, and native SSH sessions need a shared process policy:

- capture exit status and stderr;
- classify expected exit versus failure;
- avoid logging secrets;
- terminate the correct process group;
- preserve persistent tunnels when the TUI exits;
- rediscover running persistent tunnels on the next launch.

### 2.6 Capability-focused tests

Connection features should be tested through narrow interfaces and fake implementations:

- fake host repository;
- fake credential store;
- fake process supervisor;
- fake SFTP client;
- fake clock for reconnect backoff.

Do not make unit tests depend on a real SSH server, a real Keychain, or the user's `~/.ssh/config`.

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

SFTP is the first area that may need a library-backed adapter rather than the system `sftp` command. The service should provide:

- list directory;
- stat;
- upload;
- download;
- mkdir;
- rename;
- remove;
- cancellation and progress.

It must consume the same effective host profile and credential provider as SSH. This avoids a host working in Terminal but failing in Files because the two screens use different authentication rules.

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

For the current Rust stack, start with standard threads and channels where ownership is simple. Introduce Tokio only when SFTP concurrency, tunnel supervision, or cancellation actually requires an async runtime.

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

### Phase 3: SFTP and credentials

- choose a maintained SFTP library;
- implement Keychain adapter;
- share effective connection profiles;
- implement directory listing and one transfer path before building the full browser.

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

### Phase 6: reassess embedded sessions

Only add embedded PTY sessions after measuring a real need for multiple live terminals, tab switching, or command injection into an active session. Until then, native OpenSSH gives better compatibility for less code.

## 11. Architectural decisions

1. OpenSSH is the default interactive SSH and Tunnel backend.
2. SFTP is a separate library-backed capability with shared host and credential models.
3. The TUI is a state projection, not a process manager or credential store.
4. Persistent tunnels are supervised independently of the interactive SSH handoff.
5. Password support starts with safe prompts and Keychain storage; automatic native SSH password injection is a separately validated feature.
6. Reconnect, Keepalive, and timeout are separate policies.
7. Embedded terminal support remains optional and deferred.
