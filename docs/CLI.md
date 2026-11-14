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
OpenSSH aliases, not IP addresses; groups use names. Run IDs are opaque
**strings**: decimal strings for managed runs and `external:...` for recognized
external Project processes. Preserve them exactly.
Status can be `unobserved` or `unavailable`; neither proves a command is stopped.
Status also discovers recognizable local external Project commands by user,
working directory, command and process ancestry. Unrecognizable external commands
and TUI tasks without a matching process remain unobserved.

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

The running TUI checks shared configuration about once a second. Refresh waits
until open edit/confirmation dialogs close; stale saves are still rejected.
Invalid or missing metadata retains the last loaded data and blocks saving
until a valid file is restored. Hosts `r` remains a manual reload.
If CLI queries report an old metadata version, open the TUI once to perform its
backed-up migration. Reads do not migrate configuration.

## Process Actions

These endpoints support **Local and Host Project commands with either saved lifecycle
(App/Persistent) and either kind (Task/Service)**. Each CLI start runs through an
independent background runner and reports `execution_lifecycle: "Persistent"`.
The saved definition stays unchanged: its lifecycle controls future TUI starts,
while a CLI-started run survives CLI/TUI exit until completion or explicit stop.
Tasks finish normally; they do not need to be reclassified as Services.
Host runs use the saved Host alias and OpenSSH config. They require noninteractive
key/agent authentication (including any jump hosts), a trusted host key, and
`python3`, `sh`, and `ps` on the POSIX remote. No password prompt is opened by the
CLI. Fix authentication through the TUI or the user's own SSH tools if needed.
A local background runner holds the SSH connection; this is not a detached runner
installed on the server. A connection-scoped Python supervisor owns the remote
process group. Stop/EOF cleans it with TERM and then KILL after two seconds;
missing heartbeats expire after 20 seconds. Remote network loss, logout/reboot,
and commands that deliberately detach outside that group cannot provide
uninterrupted persistent service. Generic Host execution, arbitrary PID kills
and Tunnel lifecycle control remain outside this CLI.

```sh
lazyterm projects start PROJECT-UUID COMMAND-UUID --yes
lazyterm projects status --json
lazyterm projects output PROJECT-UUID COMMAND-UUID --json
lazyterm projects stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes
```

`runtime stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes` is an alias for
stopping the identified Project run. It accepts managed or recognized local
external Project run IDs; it is not a PID kill command.

Use `--yes` only for the action the user authorized. Read the exact command,
working directory, and target before starting. Starting a stored Project command
executes its shell text; adding its definition does not authorize execution.
Start validates the directory and relies on the runner lock to prevent duplicate
managed starts. Recognizable external copies block start with a conflict;
unknown/ambiguous process matches cannot be guaranteed detectable.

Before stopping, query status and pass the returned run ID. The existing runner
protocol authenticates the run and verifies ownership; a replaced run ID fails
with a conflict rather than stopping the replacement. TERM/KILL behavior is the
same as the TUI Project supervisor. CLI-started processes survive CLI and TUI exit.
Agents must not rewrite command definitions to work around execution errors.

External status includes `source: "external"`, an opaque run ID and
`external_runs` when multiple copies match the same command. Stop accepts one
exact external run ID, rechecks PID/user/start-time/command/directory, and sends
TERM only to validated individual descendants. It never kills an external process
group or escalates external termination to KILL. External output is not captured:
`projects output` returns only a saved managed run, which may be an earlier run.

`runtime list` inspects local TCP listeners and provides command/path/origin
evidence when the OS exposes it. Managed persistent Project attribution includes
the Project UUID, command UUID, and run ID. Insufficient process permissions or
missing `lsof`/`ss` can limit inspection.

`runtime list --host ALIAS` explicitly connects to one existing Host and inspects
its TCP listeners using `lsof` or `ss` plus available `ps`/cwd evidence. BatchMode
and strict host-key verification prevent interactive prompts or silent first-use
trust. Ordinary inventory commands never connect. Remote listeners are never
matched against local process groups or exposed through arbitrary PID stop.

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

The Skill treats LazyTerm as the default inventory for this machine's Hosts,
Projects, web links, and local development services. Users can ask "有哪些主机",
"看看哪些服务在跑", "哪个程序占用 3000 端口", or "把这个项目的启动命令记下来"
without mentioning LazyTerm. An explicitly named tool or target machine takes
precedence. Inventory queries do not imply permission to start/stop services,
and missing saved entries do not prove that a server or process does not exist.

### Local Development

To test frequent code changes, pin a local Skill package to the checkout's
development executable instead of installing a separate copy:

```sh
cargo build
python3 scripts/package_skill.py \
  --binary "$PWD/target/debug/lazyterm" \
  --output /tmp/lazyterm-workbuddy-skill.zip
```

Import the ZIP into your Agent client. It contains the portable Skill plus
`references/local-runtime.md` identifying the absolute executable path.
Subsequent `cargo build` (or `cargo run`) updates that executable; no CLI
reinstallation or Skill reimport is needed for binary-only changes. Restart an
already running TUI to use the new build. Skill instruction changes require a
new package and reimport. Moving the checkout also requires repackaging.
The package contains no executable, application configuration, or credentials.
Do not distribute a locally pinned package to other machines.

Omit `--binary` to generate a portable package that uses `lazyterm` on PATH.
