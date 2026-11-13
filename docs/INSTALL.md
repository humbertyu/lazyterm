# Installation and Supported Environments

The release candidate is `0.6.0-beta.1`. These instructions describe the prepared release layout; no public release or Homebrew tap is implied until the maintainer publishes them.

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
| Linux | Experimental source builds; no release package or full acceptance guarantee |
| Windows | Not supported by this Unix process/socket implementation; a credential adapter alone does not imply application support |

Local release checks run on macOS 15.8.1 Apple Silicon, including execution of the Intel package under Rosetta. The release workflow additionally builds and tests natively on macOS 15 Intel and Apple Silicon runners; those hosted runs must succeed before publishing. CJK glyph size depends on the terminal font and fallback; use a monospace font covering the selected language. Full localization, first-use host-key dialogs in background operations, and encrypted-key passphrase storage are not release guarantees. Trust a new host through system OpenSSH before using background workflows.

## Download a Binary

Download the universal archive and `SHA256SUMS` from the same release. Both Apple Silicon and Intel use the same download; macOS selects the native executable slice without Rosetta. Replace the version below when a newer release is published.

```sh
shasum -a 256 lazyterm-v0.6.0-beta.1-universal-apple-darwin.tar.gz
# Compare the complete digest with its line in SHA256SUMS.
tar -xzf lazyterm-v0.6.0-beta.1-universal-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 lazyterm-v0.6.0-beta.1-universal-apple-darwin/lazyterm "$HOME/.local/bin/lazyterm"
export PATH="$HOME/.local/bin:$PATH"
lazyterm --version
lazyterm --diagnostics
lazyterm
```

Add the PATH line to your shell profile for future terminals. If all assets are downloaded, `shasum -a 256 -c SHA256SUMS` verifies the universal archive and the Formula together. A checksum detects a changed download; it is not a publisher signature.

The current packages have ad-hoc code signatures for execution integrity, not Developer ID signatures or Apple notarization. macOS may block a quarantined download. After verifying its source and checksum, follow macOS's explicit approval flow in Privacy & Security. Do not disable Gatekeeper globally. A notarized distribution requires maintainer-owned Apple signing credentials and remains a separate release gate.

## Homebrew

The packaging command generates `dist/lazyterm.rb` with one universal download URL and its real SHA-256 value. Publish it as `Formula/lazyterm.rb` in a maintained `homebrew-lazyterm` repository and publish the matching binary asset first. Once that tap exists:

```sh
brew tap humbertyu/lazyterm
brew install humbertyu/lazyterm/lazyterm
brew test humbertyu/lazyterm/lazyterm
```

The tap is release infrastructure to provision; the commands are not a claim that it already exists. The generated Formula can also be placed in another organization's tap. It installs a prebuilt binary, so users do not need Rust. Uploading a Formula as a release asset alone does not register a tap or Homebrew/core package. Homebrew/core submission is not part of this beta.

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

## Upgrade, Backup, and Uninstall

Quit LazyTerm before replacing its binary. Stop or intentionally retain persistent Project services; existing independent runners continue using the executable they started with, so stop/restart them after upgrading. Do not run an older version against upgraded metadata. Back up both `~/.ssh` and `~/Library/Application Support/lazyterm/` before an upgrade. Settings configuration export excludes managed passwords and private keys; export encrypted credentials separately and retain private keys through your own secure backup.

Upgrade a tap installation with `brew update` and `brew upgrade humbertyu/lazyterm/lazyterm`; otherwise verify and replace the standalone binary. Uninstall with `brew uninstall humbertyu/lazyterm/lazyterm` or remove `~/.local/bin/lazyterm`. Neither command should remove your SSH configuration, private keys, LazyTerm metadata, or Keychain entries. Delete saved credentials in the application before removing local data if that is your intent. Removing the binary alone does not stop independent Project services; stop them first.

## Diagnose a Problem

`lazyterm --diagnostics` prints the application version, OS family, executable architecture, terminal-input availability and credential backend. It does not load SSH configuration, enumerate Hosts, read passwords, or include user paths/environment values. Include this report with a reproduction. Review screenshots and any command output separately: Project raw logs and user-provided Snippet commands/output are application data, not sanitized support bundles. Never attach raw configuration archives, Keychain exports, private keys or full logs to a public issue.
