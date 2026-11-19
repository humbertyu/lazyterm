# LazyTerm 0.9.0 Beta 3

LazyTerm is a local-first terminal workspace for SSH Hosts, SFTP files,
Tunnels, Snippets, Runtime, Projects, Credentials and Settings. This is a
public testing build, not a stable release.

## Changes Since Beta 2

- Fix interactive SSH shells that accepted `exit` but ignored typed commands;
  keyboard input now works through the system OpenSSH client.
- Return to the Hosts view faster after an SSH session ends, while retaining
  forced cleanup for leftover SSH and ProxyJump processes.

## Download and Install

- macOS 15 or later; Intel and Apple Silicon use the same
  `lazyterm-v0.9.0-beta.3-universal-apple-darwin.tar.gz`.
- Download `SHA256SUMS` and compare the archive's SHA-256 before installing.
- Extract it and install `lazyterm` to `~/.local/bin`; Rust is not required.
- The included `docs/BETA-TESTING.md` covers installation and test scenarios.
  [Read the guide online](https://github.com/humbertyu/lazyterm/blob/v0.9.0-beta.3/docs/BETA-TESTING.md).

## Agent and Skill

Download `lazyterm-v0.9.0-beta.3-skill.zip` from this release and install its
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
