# Install LazyTerm with an Agent

Share this page with your Agent:

`https://github.com/humbertyu/lazyterm/blob/v0.9-stable-core/docs/AGENT-INSTALL.md`

This guide installs the LazyTerm CLI/TUI and its Agent Skill on macOS 15 or
later. The current beta is `0.9.0-beta.2`.

## Recommended: Homebrew

If Homebrew is installed, use the fully qualified formula name:

```sh
brew install humbertyu/brew/lazyterm
lazyterm --version
lazyterm --diagnostics
```

This is one command and does not require a GitHub account, GitHub token, or
separate `brew tap` step. The fully qualified name selects and trusts only the
LazyTerm formula, not every package in the tap.

Homebrew's trust permission allows the selected formula code to run with the
user's privileges. It is not a security certification. Before approving an
unexpected trust prompt, inspect the published formula:

<https://github.com/humbertyu/homebrew-brew/blob/main/Formula/lazyterm.rb>

Do not trust the whole tap or disable Homebrew's trust checks just to install
LazyTerm. If the installed Homebrew version explicitly requires a separate
trust command, use the narrow formula scope and then retry:

```sh
brew trust --formula humbertyu/brew/lazyterm
brew install humbertyu/brew/lazyterm
```

## Install the Skill

Installing the program does not automatically install or enable the Skill in
an Agent client. Download the Skill ZIP from the pinned release:

<https://github.com/humbertyu/lazyterm/releases/tag/v0.9.0-beta.2>

The release asset is `lazyterm-v0.9.0-beta.2-skill.zip`; `SHA256SUMS` in the
same release contains its checksum. Compare the downloaded ZIP's SHA-256 value
with that file before importing it. This detects a damaged or mismatched
download; because both files come from the same GitHub release, it is not an
independent publisher signature. The ZIP contains the `lazyterm/` Skill
directory and `SKILL.md`.

Ask the Agent to install that directory using the Skill loader or installation
directory documented by its own client. There is no universal Skill install
location shared by all Agent clients. If the client has no supported Skill
loader, the CLI remains usable, but the Skill cannot be considered enabled.
After installation, start a fresh Agent session if the client requires it and
ask the Agent to confirm it can read the LazyTerm Skill instructions.

## Verify the Install

```sh
lazyterm --version
lazyterm schema --json
```

The binary is both the terminal application and the CLI. The Skill gives an
Agent instructions for using the local LazyTerm CLI; it does not grant access
to passwords or private keys and does not authorize starting or stopping
processes. Review any proposed write or process-control action before
approving it.

For a direct binary installation, supported macOS versions, checksum
verification, and upgrade instructions, see [INSTALL.md](INSTALL.md). The
release archive is a macOS Universal Binary for Intel and Apple Silicon.

## Give This to an Agent

```text
Install LazyTerm 0.9.0-beta.2 on this Mac using the instructions at
https://github.com/humbertyu/lazyterm/blob/v0.9-stable-core/docs/AGENT-INSTALL.md.
Prefer the fully qualified Homebrew formula. Before approving any trust prompt,
explain exactly what code is being trusted and inspect the formula; do not
trust the whole tap or disable trust checks. Install the matching Skill ZIP
from the pinned GitHub Release, verify it against SHA256SUMS, and load it using
this Agent client's supported Skill mechanism. Do not guess the Skill directory.
Verify the CLI version and schema, then report the program path and whether the
Skill is actually enabled. Do not modify my LazyTerm configuration, access
secrets, connect to Hosts, or start/stop processes.
```
