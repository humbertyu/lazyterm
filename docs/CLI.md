# Agent CLI

Run `lazyterm` without arguments for the TUI. Subcommands use the same binary,
OpenSSH configuration, LazyTerm metadata, and persistent runner records. They work
with the TUI open or closed and do not acquire its single-instance lock.

```sh
lazyterm --help
lazyterm schema --json
lazyterm hosts list --json
lazyterm projects list --json
lazyterm projects status --json
lazyterm runtime list --json
lazyterm snippets list --json
```

From a source checkout, substitute `cargo run --` for `lazyterm`.

## Contract

Results default to JSON; `--json` is accepted explicitly. Stdout contains one
versioned envelope. Errors also send a redacted diagnostic to stderr.

```json
{"schema_version":1,"ok":true,"data":[]}
```

```json
{"schema_version":1,"ok":false,"error":{"code":"not_found","message":"Host alias not found."}}
```

Exit codes: `0` success, `1` operation failure, `2` invalid input, `3` missing
entity, `4` action authorization required, `5` conflict/concurrent change.
`--help`, `--version`, and `--diagnostics` are human-readable exceptions.

Projects, commands, links, Tunnels, packages, and Snippets have UUIDs. Hosts use
OpenSSH aliases, not IP addresses; groups use names. Run IDs are opaque decimal
**strings** to avoid numeric precision loss in JSON clients. Preserve them exactly.
Status can be `unobserved` or `unavailable`; neither proves a command is stopped.
Runner inventory does not discover externally started commands or app-scoped
TUI tasks. Local listener inspection supplies additional live evidence.

## Configuration

Use `--input FILE` or `--input -` for a JSON object on stdin, limited to 1 MiB.
`lazyterm schema --json` returns accepted fields and example definitions.
Unknown fields and credential fields are rejected.

```sh
printf '%s\n' '{"alias":"dev","hostname":"192.0.2.10","user":"deploy","port":22}' |
  lazyterm hosts create --input -
printf '%s\n' '{"proxy_jump":"bastion"}' |
  lazyterm hosts update dev --input -
printf '%s\n' '{"groups":["Development"]}' |
  lazyterm hosts tag dev --input -
printf '%s\n' '{"name":"Development"}' | lazyterm groups create --input -
```

Host create/update writes standard fields into `~/.ssh/config`, preserving custom
directives and comments. Included Host definitions are updated in their original
file. `hosts tag` writes supplemental group membership separately. An alias rename
is currently available only in the TUI, which migrates its references.

Metadata create/update endpoints:

| Resource | Create | Partial Update |
| --- | --- | --- |
| Groups | `groups create` | `groups update NAME` |
| Snippet packages | `snippets create` | `snippets update PACKAGE-UUID` |
| Projects | `projects create` | `projects update PROJECT-UUID` |
| Tunnel definitions | `tunnels create` | `tunnels update TUNNEL-UUID` |

`list` returns each collection, and `get ID` returns one entity (except Groups,
which only have `list`). For example:

```sh
printf '%s\n' '{
  "name":"example",
  "base_path":"~/code/example",
  "target":"Local",
  "groups":["Development"],
  "commands":[
    {"name":"start","command":"npm run dev","kind":"Service","lifecycle":"Persistent","group":"Development"}
  ],
  "links":[{"name":"Home","url":"http://localhost:3000/"}]
}' | lazyterm projects create --input -
```

Creates assign UUIDs. Updates preserve omitted fields. Nested `commands`, `links`,
and `snippets` merge by UUID: an item with an existing UUID updates that item;
an item without a UUID adds one. Empty arrays do not delete existing child items.
There are no deletion endpoints in this version. Partial link updates retain
existing saved passwords without reading or returning them.

Metadata saves use the shared backup, file-lock, and stale-snapshot checks. SSH
Host writers share a lock and reject changes to a Host edited since loading.
On conflict, re-query before preparing a new update. Do not retry stale payloads
blindly. Direct edits from tools that do not participate in these locks can still
race file writes.

The running TUI reloads shared configuration through `r` on Hosts. A stale TUI
save is rejected rather than replacing newer metadata. This version does not
automatically refresh every open TUI view. If CLI queries report an old metadata
version, open the TUI once to perform its backed-up migration.

## Process Actions

These endpoints support **Local Project commands with Persistent lifecycle**.
Host-targeted or app-scoped starts, external PID termination, Tunnel lifecycle
control, and remote Runtime inspection remain in the TUI.

```sh
lazyterm projects start PROJECT-UUID COMMAND-UUID --yes
lazyterm projects status --json
lazyterm projects output PROJECT-UUID COMMAND-UUID --json
lazyterm projects stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes
```

`runtime stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes` is an alias for
stopping that managed run. It is not a PID kill command.

Use `--yes` only for the action the user authorized. Read the exact command,
working directory, and target before starting. Starting a stored Project command
executes its shell text; adding its definition does not authorize execution.
Start validates the directory and relies on the runner lock to prevent duplicate
managed starts. It does not detect every externally started copy.

Before stopping, query status and pass the returned run ID. The existing runner
protocol authenticates the run and verifies ownership; a replaced run ID fails
with a conflict rather than stopping the replacement. TERM/KILL behavior is the
same as the TUI Project supervisor. Persistent processes survive CLI and TUI exit.

`runtime list` inspects local TCP listeners and provides command/path/origin
evidence when the OS exposes it. Managed persistent Project attribution includes
the Project UUID, command UUID, and run ID. Insufficient process permissions or
missing `lsof`/`ss` can limit inspection.

Output returns at most the latest 16,384 Unicode characters of the runner's
retained output, with a `truncated` flag for this additional CLI limit. The runner
itself already retains a bounded tail; this is not a complete historical log.

## Credentials and Server Access

No CLI endpoints retrieve saved passwords, Keychain entries, private key contents,
runner control tokens/sockets, SSH sessions, or SFTP operations. Configuration
queries omit saved web passwords and credential references. Host results report
the configured identity path and whether that local file exists, never its contents.

Text results redact recognized password/token assignments, credential-bearing
URLs, flags, and private-key blocks. Redaction is heuristic: an arbitrary bare
secret printed by an application may not be recognizable. Do not put secrets in
command templates or use Project output as a secret transport.

For SSH login, remote commands, and file transfers, Agents use their own
SSH/SFTP tools with the discovered Host alias. Snippets are templates for those
tools; the CLI does not execute Snippets. CLI configuration queries never run
`ssh -G`, `Match exec`, or establish remote connections.

## Skill

The distributable instructions live at `skills/lazyterm/SKILL.md`. Copy that
directory into the Agent client's supported skill directory, or add it using
that client's skill loader. Only instructions are installed; configuration and
credentials remain in LazyTerm's normal locations. Ensure the `lazyterm` binary
is on the Agent's PATH. The Skill uses `lazyterm schema --json` for the current
contract and does not require MCP or a separate server.
