# Installation and Supported Environments

The release candidate is `0.9.0-beta.4`. Download its Universal Binary from
[GitHub Releases](https://github.com/humbertyu/lazyterm/releases/tag/v0.9.0-beta.4)
or install from our [Homebrew tap](https://github.com/humbertyu/homebrew-brew).
See [beta testing and Agent/Skill setup](BETA-TESTING.md) for the quick start.

## Supported Environment

| Component | Release baseline |
| --- | --- |
| OS | macOS 15 or newer; older systems are not supported by these packages |
| CPU | One Universal Binary supports Apple Silicon and Intel natively |
| Terminal | Interactive UTF-8 terminal with ANSI colors; 100 x 30 or larger recommended |
| SSH | System OpenSSH (`ssh`, `sftp`, `ssh-keygen`, `ssh-add`), tested locally with 9.9p2 |
| Credentials | macOS Keychain, available and unlocked when saving/reading passwords |
| Runtime | System `ps`, `lsof`, and `kill` |
| Source builds | Rust 1.92+, Cargo, Xcode Command Line Tools; Python 3.9+ only for packaging |
| Linux | Experimental source builds; no release package or full acceptance guarantee. Formal support is planned for v1.0 after native acceptance. |
| Windows | Not supported by the current Unix process/socket implementation. Windows support is planned for v1.0 and requires native implementation and acceptance. |

Agent CLI remote Runtime queries require noninteractive SSH authentication and
existing Host-key trust, including any ProxyJump hops. Host-targeted CLI Project
runs additionally require Python 3, `sh` and `ps` on the POSIX remote; a local
runner holds the connection and cleans its remote group after disconnect.
Remote detached services and rediscovery are not supported. See [CLI.md](CLI.md).
OpenSSH 9.9p2 is the tested baseline, not a verified minimum version; older
OpenSSH versions have not passed the release acceptance matrix.

The current release workflow builds and tests natively on macOS 15 Intel and Apple Silicon runners; Linux and Windows are not currently release targets. v1.0 platform baselines and packages will be declared only after native CI and manual acceptance are complete. CJK glyph size depends on the terminal font and fallback; use a monospace font covering the selected language. Full localization, first-use host-key dialogs in background operations, and encrypted-key passphrase storage are not release guarantees. Trust a new host through system OpenSSH before using background workflows.

## Download a Binary

Download the universal archive and `SHA256SUMS` from the same release. Both Apple Silicon and Intel use the same download; macOS selects the native executable slice without Rosetta. Replace the version below when a newer release is published.

```sh
shasum -a 256 lazyterm-v0.9.0-beta.4-universal-apple-darwin.tar.gz
# Compare the complete digest with its line in SHA256SUMS.
tar -xzf lazyterm-v0.9.0-beta.4-universal-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 lazyterm-v0.9.0-beta.4-universal-apple-darwin/lazyterm "$HOME/.local/bin/lazyterm"
export PATH="$HOME/.local/bin:$PATH"
lazyterm --version
lazyterm --diagnostics
lazyterm
```

Add the PATH line to your shell profile for future terminals. If all assets are downloaded, `shasum -a 256 -c SHA256SUMS` verifies the universal archive and the Formula together. A checksum detects a changed download; it is not a publisher signature.

The current packages have ad-hoc code signatures for execution integrity, not Developer ID signatures or Apple notarization. macOS may block a quarantined download. After verifying its source and checksum, follow macOS's explicit approval flow in Privacy & Security. Do not disable Gatekeeper globally. A notarized distribution requires maintainer-owned Apple signing credentials and remains a separate release gate.

## Homebrew

The maintained tap installs the published Universal Binary, without Rust:

```sh
brew install humbertyu/brew/lazyterm
brew test lazyterm
```

The shared tap is hosted at `humbertyu/homebrew-brew` and can contain formulas
for multiple applications. For each release, maintainers
publish the binary first, then copy the generated `dist/lazyterm.rb` into the
tap's `Formula/lazyterm.rb`. Homebrew/core submission is not part of this beta.

The fully qualified formula name selects only LazyTerm and avoids a separate
tap command. If the installed Homebrew version explicitly requires a trust
command, review the [published formula](https://github.com/humbertyu/homebrew-brew/blob/main/Formula/lazyterm.rb)
and trust only `humbertyu/brew/lazyterm`; do not trust the whole tap or disable
trust checks. See the [Agent installation guide](AGENT-INSTALL.md) for details.

Upgrade with `brew upgrade lazyterm`. If another tap provides a formula with
the same name, use `humbertyu/brew/lazyterm` to select this one explicitly.

## Build From Source

```sh
xcode-select --install
cargo install --path . --locked --root "$HOME/.local"
"$HOME/.local/bin/lazyterm" --version
```

Use a reviewed release tag. Source builds do not automatically give an older macOS version tested support. Release binaries are packaged with:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
python3 scripts/release.py universal
(cd dist && shasum -a 256 -c SHA256SUMS)
```

For local uncommitted testing, use `python3 scripts/release.py universal --allow-dirty`. Those archives record `source_dirty: true` and are rejected by normal release assembly. This command builds each CPU slice, validates the inputs, combines them with `lipo`, signs the combined executable, and creates the universal archive, Formula and checksums. Individual `build --target ...` and `assemble` commands remain available for CI; architecture-specific archives are intermediate artifacts, not public release assets. Cross-building on Apple Silicon requires Rosetta to execute the Intel slice smoke check; end users of the universal package do not need it. The hosted workflow executes the final universal package natively on both CPUs before preparing a draft release. Archives include only the executable, license, selected public documentation, and build metadata. They never package the maintainer's home directory, configuration, keys, logs, or credentials.

The repository's `projects/lazyterm.json` records the local Projects workspace commands for builds, checks and packaging. Set the workspace base path to your checkout when registering it in LazyTerm. Run one packaging task at a time because they share `target/` and `dist/`. The local package command permits uncommitted changes; the release command requires a clean checkout. Packaging is an ordinary Task and stops if its owning LazyTerm exits.

## Experimental Linux Source Builds

Linux remains a development target, not a published release package. On
Debian/Ubuntu, install Rust 1.92+ and the build/runtime dependencies:

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config libssl-dev libwayland-dev \
  libxkbcommon-dev openssh-client procps lsof iproute2
cargo build --locked
./target/debug/lazyterm --diagnostics
```

Linux passwords use Secret Service through the session D-Bus. A desktop
session must provide an enabled Secret Service implementation, such as GNOME
Keyring or a compatible KWallet setup, and its password collection must be
unlocked. Installing a package alone does not ensure a working desktop session.
LazyTerm reports unavailable/denied storage operations without exposing the
backend's raw error details. Host passwords never fall back to configuration
files. An unavailable store does not mean that a saved password was deleted;
restore access to the original keyring or enter a temporary password without
saving. Temporary password entry is supported by the interactive TUI; CLI
operations retain their documented noninteractive authentication restrictions.

On a headless machine, use SSH keys or temporary interactive passwords unless
you deliberately configure a session bus and credential service. LazyTerm does
not create an unprotected keyring or start a credential daemon for the user.
GNOME Keyring and KWallet graphical unlock/cancel/retry have passed isolated
X11 probes. A new KDE account must initialize a password-protected default
wallet using Wallet Manager before saving. Full GNOME/Plasma login-session
and Wayland acceptance remains pending; see the
[v1.0 validation record](v1.0-acceptance.md).

Developers can test the backend without touching their real credential
collection:

```sh
sudo apt-get install openssh-server dbus-x11 gnome-keyring libglib2.0-bin
bash scripts/test-linux-credentials.sh
```

The fixture creates a temporary home, its own D-Bus session and a disposable
password-protected GNOME keyring. It tests unavailable storage, password
creation/update, a fresh process reading the same reference, deletion, and
locked storage without a graphical prompter. It deletes the temporary home
afterwards. This script refuses to run on macOS.

## Upgrade, Backup, and Uninstall

Quit LazyTerm before replacing its binary. Stop or intentionally retain persistent Project services; existing independent runners continue using the executable they started with, so stop/restart them after upgrading. Do not run an older version against upgraded metadata. Back up both `~/.ssh` and `~/Library/Application Support/lazyterm/` before an upgrade. Settings configuration export excludes managed passwords and private keys; export encrypted credentials separately and retain private keys through your own secure backup.

Metadata schema 3 uses UUID entity IDs. TUI startup migrates older supported
schemas with a backup; CLI reads do not migrate. See [DATA-MIGRATION.md](DATA-MIGRATION.md)
for exact backup locations, retention, write/failure boundaries and recovery.
The [Stable Core checklist](v0.9-acceptance.md) records outstanding acceptance.

Upgrade a tap installation with `brew update` and `brew upgrade humbertyu/brew/lazyterm`; otherwise verify and replace the standalone binary. Uninstall with `brew uninstall humbertyu/brew/lazyterm` or remove `~/.local/bin/lazyterm`. Neither command should remove your SSH configuration, private keys, LazyTerm metadata, or Keychain entries. Delete saved credentials in the application before removing local data if that is your intent. Removing the binary alone does not stop independent Project services; stop them first.

When switching from a manual install to Homebrew, use `type -a lazyterm` to
identify any old executable that could shadow Homebrew. If installed with the
manual binary instructions above, remove only that executable with
`rm "$HOME/.local/bin/lazyterm"`. If installed through Cargo with
`--root "$HOME/.local"`, use `cargo uninstall --root "$HOME/.local" lazyterm`
instead. A default Cargo installation uses `cargo uninstall lazyterm`.
Then install through Homebrew and open a new terminal, or run `rehash` in zsh.
The existing application data is reused.

## Diagnose a Problem

`lazyterm --diagnostics` prints the application version, OS family, executable architecture, terminal-input availability and credential backend. It does not load SSH configuration, enumerate Hosts, read passwords, or include user paths/environment values. Include this report with a reproduction. Review screenshots and any command output separately: Project raw logs and user-provided Snippet commands/output are application data, not sanitized support bundles. Never attach raw configuration archives, Keychain exports, private keys or full logs to a public issue.
