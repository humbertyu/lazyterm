---
name: lazyterm
description: Discover and maintain LazyTerm Hosts, groups, Projects, Snippet templates, and Tunnel definitions through its CLI, and inspect or control approved local Project services.
---

# LazyTerm

Use the installed `lazyterm` binary. With subcommands it returns JSON; without
arguments it opens an interactive TUI. Begin with `lazyterm schema --json` for
current writable fields, examples, and capability limits. Parse stdout as
`{schema_version, ok, data}` or `{schema_version, ok, error}`; diagnostics use
stderr. Use `--input FILE` or `--input -` for structured configuration writes.

## Discover and Maintain

- Query `hosts list`, `groups list`, `projects list`, `snippets list`, and
  `tunnels list`. Query an individual entity with `get ID`; Hosts use aliases,
  while Projects/packages/Tunnels and their children use UUIDs.
- Create/update through the CLI rather than rewriting SSH config or metadata.
  Host standard fields go into SSH config; `hosts tag ALIAS` sets its groups.
- Partial updates preserve omitted fields. Nested commands, links, and Snippets
  merge by UUID. Reuse a child's UUID when changing it; omitting it adds an item.
  Empty child arrays do not delete items. Deletion and Host alias renaming are
  currently TUI operations.
- On exit code 5, re-read state and reconsider the intended change. Do not blindly
  retry a stale write. A running TUI can refresh shared data with `r` on Hosts.
- Snippets are reference templates. For Host login, remote shell work, and file
  transfers, use your own SSH/SFTP tools and the discovered alias. LazyTerm has no
  Host execution or SFTP CLI endpoint. Do not request private keys or passwords.

## Inspect and Control Services

`runtime list` inspects local TCP listeners with available process/path/origin
evidence. `projects status` reports managed persistent runs; `unobserved` and
`unavailable` are uncertain states, not proof that no process is running.
External processes and TUI app-scoped tasks may not appear in managed status.

Before an authorized start, read the Project and exact command, directory,
target, and lifecycle. Current CLI starts require Local + Persistent:

```sh
lazyterm projects start PROJECT-UUID COMMAND-UUID --yes
lazyterm projects output PROJECT-UUID COMMAND-UUID --json
```

Creating a command definition does not authorize executing its shell text.
Use `--yes` only within the user's explicit requested execution/termination scope.
For stop, query status immediately and preserve the returned run ID exactly as a
decimal string:

```sh
lazyterm projects stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes
```

`runtime stop` takes the same identifiers and stops only the authenticated managed
run. There is no arbitrary PID termination endpoint. A stale run ID must trigger
reinspection, not an attempt to kill its old PID. Do not auto-retry a failed or
timed-out start; inspect status and listeners first to avoid duplicate services.

Output is bounded and known credential patterns are redacted, but arbitrary
unlabelled application secrets may remain. Avoid collecting unrelated logs or
putting secrets into configuration. Saved credentials and private keys stay
outside this interface. MCP and private-key synchronization are not required.
