# LazyTerm

A lightweight, keyboard-driven workspace for remote hosts. LazyTerm aims to bring the useful parts of a Termius-style workflow to a local-first TUI: SSH host management, terminal access, SFTP, tunnels, and command snippets.

The project is at an early stage. The Hosts view reads aliases from `~/.ssh/config`, organizes them by local tags, and opens them with the system OpenSSH client. Basic creation, editing, and deletion are available for LazyTerm-managed hosts. Files has a local-first dual-pane browser with an embedded SFTP directory view, per-host connection reuse, recursive transfers, transfer history, and confirmed file mutations. Tunnels support Local, Remote, and Dynamic/SOCKS forwards with process supervision. Snippets are organized into Packages, can run against a Host or tagged Host group with bounded concurrency, and show per-host output and status. Runtime inspects local or remote TCP listeners through the same Host catalog. Projects organizes local commands by base directory, supervises their processes, and links their listening ports back from Runtime.

## Requirements

- Rust stable
- macOS currently targeted; OpenSSH is expected at `/usr/bin/ssh`

## Run

```sh
cargo run
```

The top tab bar switches between Hosts, Files, Tunnels, Snippets, Runtime, and Projects. Use `1`-`6` or `F1`-`F6` to jump directly, `Tab` / `Shift+Tab` or `Ctrl+Left` / `Ctrl+Right` to cycle, and backtick (`` ` ``) to toggle back to the previous tab. Plain arrow keys navigate within Hosts, not between tabs. Returning to Hosts preserves its group, filter, and selection.

Search captures typed characters, including digits, rather than switching tabs; `F1`-`F6` and `Ctrl+Left` / `Ctrl+Right` can still switch from search without clearing the filter. Add/edit and tag dialogs keep keyboard focus until saved or cancelled; `Tab` moves between form fields there. Terminal-reserved shortcuts may require terminal configuration; plain digit navigation is available outside text input.

In Hosts, use `↑`/`↓` to select, `Enter` to open a group or connect to a host, `/` to search, `r` to reload, `f` for Files, or `t` for Tunnels. Files always shows the local directory on the left. Its breadcrumb path is shown above the local list; click any visible path segment to jump directly to that directory, and LazyTerm remembers the last local path. Focus either pane and press `/` to filter its files and directories by name, `Enter` to keep the filter, or `Esc` to clear it. Hidden files and directories are hidden by default; press `h` in either pane to toggle them for both local and remote listings. The remote pane starts empty; focus it with `Tab` and press `Enter` or `s` to open a picker containing Groups and Hosts. Enter a group to see its hosts, then Enter a host to load its last remote directory, or its home directory the first time. In either pane, `↑`/`↓` selects, `Enter` opens a directory, `Backspace` returns to the parent, and `Tab` switches panes. Press `n` to create a directory, `m` to rename an entry, and `x` to delete it after confirmation. With a local file or directory selected, press `u` to upload it to the current remote directory; with a remote file or directory selected, press `d` to download it to the current local directory. Transfers run sequentially; the `TRANSFERS` area shows queued, active, completed, cancelled, and failed tasks with a progress bar and percentage. Use `[` / `]` to select a task, `c` to cancel the selected task, `C` to cancel all active and queued transfers, `R` to retry a failed or cancelled task, and `K` to clear terminal history. Transfers support ordinary files and recursive directories; interrupted recursive transfers remove empty directories created by that transfer. Existing transfer targets require overwrite confirmation. Remote `r` reloads and `o` opens the native `sftp <alias>` client. The embedded browser reuses the SSH alias and OpenSSH routing/config, shows remote size, permissions, owner/group IDs, and readable modification time when available, and requires a known host key plus key/agent authentication. Use native `sftp` for interactive password authentication or first-time host-key confirmation. Press `q` to quit. These shortcuts navigate LazyTerm's workspace; they do not switch SSH sessions while OpenSSH owns the terminal.

Hosts has a compact left browser with **Groups** cards above the **Hosts** list, and a separate details pane on the right. Each card shows its name and host count; at most two columns and two rows are visible, with scrolling for additional groups. Narrow terminals show only the browser. Arrow keys follow the grid: `←`/`→` move within a card row and `↑`/`↓` move between rows. From the last group row, `↓` enters the host list; from the first host, `↑` returns to the last group. Movement stops at the edges rather than wrapping. Hosts without tags appear in **Untagged**. Select a group and press `Enter` to see its hosts. Press `←`, `Esc`, or `Backspace` inside a group to return to the previously selected group card. Press `v` to return directly to the overview.

Select a host and press `g` to edit its comma-separated tags, then `Enter` to save or `Esc` to cancel. Tags work for both managed and existing OpenSSH hosts. A host with multiple tags appears in every corresponding group. Search matches aliases, destinations, and tags, and stays within the current group. New hosts created inside a tag group inherit that tag.

Tags are stored separately from SSH configuration in `~/Library/Application Support/lazyterm/hosts.json` on macOS, or `$XDG_DATA_HOME/lazyterm/hosts.json` (default `~/.local/share/lazyterm/hosts.json`) elsewhere. The legacy `view` property is retained for compatibility but does not switch between separate Groups and Hosts pages. Tag edits do not modify `~/.ssh/config`.

OpenSSH configuration (including discovered `Include` files) is the source of truth for which hosts exist. On startup and `r` refresh, LazyTerm removes metadata for aliases that no longer exist and saves the cleanup only when needed. A missing root config is treated as an empty catalog. Read or parse failures preserve metadata instead of treating hosts as deleted. An externally renamed alias is treated as deletion plus addition; renames made in LazyTerm preserve tags. If a group loses its last host, the view returns to the group list.

The v0.1.0 host-management scope is limited to common connection fields: alias, hostname, username, port, and identity file. LazyTerm will not store passwords. Advanced OpenSSH directives and complex configuration rules remain editable directly in `~/.ssh/config`.

In the Hosts view, use `a` to add a managed host, `e` to edit a LazyTerm-managed host, and `d` to delete one after confirmation. Existing unmanaged OpenSSH hosts remain read-only in LazyTerm. Press `Enter` to hand an SSH session to the system OpenSSH client.

Runtime shows TCP listeners on the local machine or on a selected Host. Press `s` to open the Groups/Hosts picker, `l` to return to Local, and `r` to refresh the inspection. `Enter` keeps the selected listener focused and its details visible. Entries are classified as `APP`, `SYSTEM`, or `UNKNOWN`, with application listeners shown first. Select a listener and press `k` to confirm sending `TERM` to its visible PID. Runtime also queries the full command line and working directory for the PID when the platform permits it, so an Agent-started `node`, `python`, or similar process can be traced back to its project directory and command. Runtime uses `lsof` when available and falls back to `ss`; process names, paths, working directories, and PIDs may be unavailable without sufficient permissions.

Runtime also marks launch origin separately from ownership: `CLI` (cyan) for terminal/shell ancestry, `AGENT` (gold) for recognized Agent ancestry, `DESKTOP` (gray-blue) for macOS app bundles, and `UNKNOWN` when no evidence is visible. CLI and Agent listeners appear first within the application category. The details pane shows the classification evidence. Mine still includes desktop applications owned by the same user; colors distinguish their origin. These are observations of the current parent chain, not a durable launch audit: detached or reparented processes can lose their origin.

Runtime categories are ordered `Mine`, `Apps`, `System`, `All`; use Left/Right to switch. `Mine` is selected by default and shows non-system listeners owned by the current local user or remote SSH account, determined by numeric UID. It includes potential Agent leftovers, but does not prove which tool launched a process. `Apps` includes application processes belonging to other users; `System` is a heuristic based on known service names and system paths. Unknown entries remain accessible in `All`. Inspect the working directory and command before terminating a process.

Commands launched from Projects have a green `PROJECT` origin in local Runtime, with the project and command names shown beside the listener. These appear first in Mine and Apps. Press `o` on a Project listener to open its command output. `k` explicitly confirms stopping the entire managed command group, rather than just one listening child. Entering Runtime refreshes the inspection; use `r` to refresh while staying there. Association uses the owned process group and current run identity, not matching directory names or remote PIDs.

Recognized externally started Project commands appear in cyan as `PROJECT / EXT`, with the same project/output link. Their confirmed stop targets the observed command and its descendants individually, preserving unrelated processes in a shared terminal process group.

## Projects

Press `6` / `F6` for Projects. Use `a` to add a project with a name and absolute base directory (`~/...` is accepted), `e` to edit, and `d` to delete after confirmation. Enter a project to see its named commands; `a/e/d` then operate on commands. Each command contains a shell command and an optional working directory, relative to the project base or absolute. The directory is validated on save and before execution.

Select a command and press `s` to start, `x` to stop, or `r` to restart. `A` starts all currently stopped commands in the selected project; `X` stops all of them. Enter opens output without starting anything. Use Up/Down to scroll, Home/End for the beginning/live tail, `[` / `]` to switch commands, and Esc to return. Switching tabs preserves running tasks and workspace selection. Stop commands before editing or deleting their definitions.

Projects also checks for externally started commands asynchronously every three seconds. Recognition requires the current user's process, the configured working directory, and a matching command; common npm argument rewriting is normalized and descendant listening ports are shown. Directory or port matches alone do not establish a running command, and ambiguous definitions are left unassociated. Missing process metadata or rewritten process ancestry can prevent recognition. Running here describes process presence, not application health.

Current status is separate from the last managed run: an externally running server can coexist with a previous `exit 1`. Start and start-all inspect again before launching and skip recognized running commands. Stopping or restarting external commands requires confirmation and fresh identity/directory checks. External stop sends TERM to the observed processes without automatic KILL escalation; a stop that times out cancels restart. A successful restart launches a new managed run. External commands are never stopped on LazyTerm exit, and their existing stdout/stderr cannot be captured; the output view identifies any retained logs as the last managed run.

Each run uses `sh -c` in its own process group, inherits LazyTerm's environment, and has no interactive stdin or PTY. Use foreground commands such as `npm run dev` or `cargo test`; commands that deliberately daemonize or escape their process group are outside this supervisor's lifecycle. Stop sends TERM to the group and escalates to KILL after two seconds. Restart waits for the previous group and output readers to finish. Normal LazyTerm exit kills remaining managed groups; forced termination of LazyTerm itself is not yet recoverable.

The output view shows a scrolling 256 KiB tail with terminal escape sequences removed and stderr marked. Complete raw stdout and stderr are saved separately for each run in `project-logs/` beside `hosts.json`; their paths appear in command details. Log files use mode `0600`, the log directory uses `0700`, and logs remain across restarts and definition deletion. There is no automatic log rotation yet. Project definitions are stored in the existing version-1 `hosts.json` as an additive `projects` field, using the same atomic writes and `hosts.config.lazyterm.bak` backup. Existing files load with an empty project list. Projects survive Host reconciliation and remain separate from Snippet templates.

## Direction

- Reuse OpenSSH for interactive sessions, host key verification, and ProxyJump.
- Keep credentials in the operating system keychain; never persist plaintext passwords.
- Keep SSH sessions, the TUI, and tunnels on explicit, independent lifecycles.
- Build SFTP and tunnel management around shared host and credential models.
- Defer embedded terminal rendering, sync, and team features.

See [docs/architecture.md](docs/architecture.md) for the initial architecture and milestones.
See [ROADMAP.md](ROADMAP.md) for planned releases and [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution workflow.

The v0.1.0 release gate is not yet met. See the roadmap audit for remaining favorites, SSH acceptance testing, and configuration-safety work; Files, Tunnels, and Snippets are not part of this milestone.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

The real SSH/SFTP fixture test is opt-in and requires `sshd`, `ssh-keygen`, and `ssh-keyscan`:

```sh
LAZYTERM_SFTP_FIXTURE=1 cargo test sftp_fixture_end_to_end -- --nocapture
```

It starts a temporary local `sshd`, uses temporary keys and configuration, tests the embedded SFTP operations, and removes the fixture when the test exits.

Normal `cargo test` also runs disposable Project process tests, including a temporary listener on an OS-assigned loopback port, concurrent commands, full output logs, TERM/KILL cleanup, restart, and the Projects -> Runtime -> output -> confirmed stop flow. These require local `ps` and `lsof` (or `ss`) and use no personal SSH configuration. The ignored `disposable_listener` test is a subprocess helper launched by these tests, not an untested acceptance gate.

External Project tests cover command/directory matching, ambiguous definitions, stale scan rejection, duplicate-start prevention, current status versus previous exit status, Runtime links, canceled stops, confirmed restart into managed execution, and preserving unrelated shared-group processes.

## License

MIT. See [LICENSE](LICENSE).
