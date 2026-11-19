---
name: lazyterm
description: "Manage this machine's host inventory, project commands, web links, and services through LazyTerm. Use for requests such as 有哪些主机、查看运行的服务、哪个程序占用端口、添加项目启动命令, even when the user does not mention LazyTerm. Query local or explicitly selected Host listeners, maintain definitions, and control authorized Project services."
---

# LazyTerm

## Default Workspace Context

Treat LazyTerm as this machine's default inventory and service-management source
for requests about saved Hosts, project commands, web entry points, listening
ports, and running development services. The user does not need to say
"in LazyTerm" or name this Skill. Respect an explicitly named alternative tool
or target machine. This default does not apply to unrelated project source-code
changes or general questions about processes.

Route ordinary requests by intent:

| User request | First lookup |
| --- | --- |
| 有哪些主机 / 服务器有哪些 | `hosts list` and, when useful, `groups list` |
| 有哪些项目 / 项目怎么启动 / 网页入口是什么 | `projects list`, then `projects get UUID` |
| 有哪些服务在跑 / 哪个程序占用 3000 端口 | `runtime list` and `projects status` |
| 把这个项目的启动命令记下来 | Query existing Projects, then create/update the matching definition |
| 有没有可参考的运维命令 | `snippets list` |

Answer in terms of the user's Hosts, Projects, and services without repeatedly
prefixing results with "configured in LazyTerm". Keep evidence limits visible:
saved Hosts are an inventory, not a scan of every server; Runtime defaults to
this local machine; query remote listeners only for a user-selected Host through
`runtime list --host ALIAS`. If a requested item is
missing, report that lookup result rather than claiming it does not exist.
Resolve service names against Project definitions before an authorized action;
ask only when the target or intended action remains ambiguous. Discovery does
not authorize starting or stopping services.

## Executable Selection

If this Skill includes `references/local-runtime.md`, read it before invoking
commands and use its absolute executable path. Otherwise use an absolute path
explicitly supplied by the user, or default to `lazyterm` on PATH. Replace
`lazyterm` in all examples below with the selected executable, quoting its path.
Keep that selection for the task; if an explicitly selected executable is missing
or fails, report the error rather than silently falling back to another install.
Do not install or build a replacement unless requested.

With subcommands the binary returns JSON; without
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
  retry a stale write. The TUI refreshes shared data automatically while no edit
  or confirmation dialog is open; `r` on Hosts remains a manual reload.
- Snippets are reference templates. For Host login, remote shell work, and file
  transfers, use your own SSH/SFTP tools and the discovered alias. LazyTerm has no
  Host execution or SFTP CLI endpoint. Do not request private keys or passwords.

## Inspect and Control Services

`runtime list` inspects local TCP listeners with available process/path/origin
evidence. `runtime list --host ALIAS` explicitly connects to that existing Host
using noninteractive SSH and strict known-host verification. If authentication
or trust is missing, report the failure; do not change SSH policy to bypass it.
`projects status` reports managed runs and recognizable local external Project
commands. External runs have `source: "external"` and `external_runs` lists;
`unobserved` and
`unavailable` are uncertain states, not proof that no process is running.
External processes and TUI app-scoped tasks may not appear in managed status.

Before an authorized start, read the Project and exact command, directory,
target, and lifecycle. CLI starts support Local/Host commands with either App/Persistent
lifecycle and Task/Service kind. Each CLI start uses an independent background
runner for that run, without modifying the saved lifecycle or kind:

```sh
lazyterm projects start PROJECT-UUID COMMAND-UUID --yes
lazyterm projects output PROJECT-UUID COMMAND-UUID --json
```

Host commands require noninteractive SSH key/agent authentication, existing
host-key trust, and remote Python 3, sh and ps. A local runner maintains the SSH
session; its remote process group is cleaned when that session/heartbeat ends.
This is not a remotely installed persistent service. Do not install missing
dependencies or rewrite a command/target to bypass a failure unless requested.

Creating a command definition does not authorize executing its shell text.
Starting a command does not authorize changing its definition. Never change
lifecycle, kind, command text, target, or groups merely to work around a start
error. Report a genuine unsupported target or error instead. A Task can be
started individually; it does not need to become a Service.
Use `--yes` only within the user's explicit requested execution/termination scope.
For stop, query status immediately and preserve the returned run ID exactly as a
opaque string (including the `external:` prefix for external runs):

```sh
lazyterm projects stop PROJECT-UUID COMMAND-UUID --run-id RUN-ID --yes
```

`runtime stop` takes the same identifiers. Managed runs use authenticated runner
control; external runs revalidate user/start-time/command/directory and signal
only that run's individual processes. External output is not captured; saved
managed output can belong to an earlier run, so inspect its run ID/source.
There is no arbitrary PID termination endpoint. A stale run ID must trigger
reinspection, not an attempt to kill its old PID. Do not auto-retry a failed or
timed-out start; inspect status and listeners first to avoid duplicate services.

Output is bounded and known credential patterns are redacted, but arbitrary
unlabelled application secrets may remain. Avoid collecting unrelated logs or
putting secrets into configuration. Saved credentials and private keys stay
outside this interface. MCP and private-key synchronization are not required.
