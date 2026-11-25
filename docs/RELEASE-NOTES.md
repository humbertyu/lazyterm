# LazyTerm 0.9.0 Beta 4

LazyTerm is a local-first terminal workspace for SSH Hosts, SFTP files,
Tunnels, Snippets, Runtime, Projects, Credentials and Settings. This is a
public testing build, not a stable release.

## Changes Since Beta 3

- Update the URL parser to 2.5.9 to address RUSTSEC-2026-0592.
- Reduce idle redraws from ten per second to one; input and visible work retain
  responsive updates.
- Stop ignored mouse movement from triggering full redraws or accelerating
  background polling.
- Run automatic Project discovery only on Projects and local Runtime pages,
  and enumerate listening ports only after finding a matching external command.
  Command preflight, process supervision and recovery remain active.
- In a local debug-build measurement, cumulative CPU time over a 12-second idle
  window fell from 2.04 seconds to 0.14 seconds. Results vary by workload and
  environment; see `docs/performance.md` in the source repository.

## Download and Install

- macOS 15 or later; Intel and Apple Silicon use the same
  `lazyterm-v0.9.0-beta.4-universal-apple-darwin.tar.gz`.
- Download `SHA256SUMS` and compare the archive's SHA-256 before installing.
- Extract it and install `lazyterm` to `~/.local/bin`; Rust is not required.
- The included `docs/BETA-TESTING.md` covers installation and test scenarios.
  [Read the guide online](https://github.com/humbertyu/lazyterm/blob/v0.9.0-beta.4/docs/BETA-TESTING.md).

## Agent and Skill

Download `lazyterm-v0.9.0-beta.4-skill.zip` from this release and install its
`lazyterm` folder using your Agent client's Skill loader or supported Skill
directory. The binary archive also includes `skills/lazyterm/SKILL.md`.
The guide includes a ready-to-use Agent installation prompt.

After loading it, ask "Which projects and services are running?" or
"Which program is using port 3000?" The CLI returns structured data without
opening the TUI. The Skill discovers Hosts, project commands and web links,
maintains definitions, and controls explicitly authorized Project runs.
Host login and file transfer use the Agent's own SSH/SFTP tools.

## Included

- OpenSSH config management, groups, password/SSH key workflows and shared
  authentication prompts; Host passwords stay in the system Keychain.
- Dual-pane Files with SFTP, transfer progress/history, rename and delete.
- Local/Remote/Dynamic Tunnels; reusable Snippet packages with full run output.
- Local/remote Runtime and grouped Project commands, service supervision,
  web links and process attribution.
- UUID-based configuration merge/import/export, separate encrypted credential
  archives, backups and stale-write protection.
- Agent CLI/portable Skill, consistent form editing, Unicode cursor movement,
  masked password fields and compact-terminal checks.

## Before Testing

Back up `~/.ssh` and `~/Library/Application Support/lazyterm/` before using an
existing profile. Private keys are excluded from configuration/credential
exports and need your own secure backup. Close old LazyTerm versions.

Git synchronization remains experimental and paused. Full translation,
Windows support, background first-use Host-key confirmation, encrypted-key
passphrase storage and detached remote service runners are not included.
Trust new Hosts through system OpenSSH first. Linux source builds are experimental.
Packages use ad-hoc signatures; they are not Apple-notarized.

## Feedback

[Report a beta issue](https://github.com/humbertyu/lazyterm/issues/new)
with the version, macOS/CPU, terminal, reproduction steps, expected/actual
behavior and `lazyterm --diagnostics`. Review screenshots before attaching them.
Never attach passwords, private keys, configuration/credential exports or raw logs.
