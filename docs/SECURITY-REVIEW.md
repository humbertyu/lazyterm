# Beta Security and Diagnostics Review

Review date: 2026-11-13. This is an internal source review with regression tests, not an independent security audit or a guarantee that arbitrary command output is secret-free.

## Boundaries Reviewed

- Host passwords use native Keychain storage, scoped askpass sessions, private local sockets and exact prompt routes. Password bytes are not put into process arguments, environment values or OpenSSH config. Opaque authentication tokens are local capabilities, not passwords.
- Configuration import validates versions and paths, stages SSH validation privately, uses backups and rolls back failed file writes. Export intentionally preserves raw SSH config and user-authored command text; those may contain user-embedded secrets and must be treated as private documents.
- Encrypted credential archives use age passphrase encryption. Ordinary configuration exports omit managed password fields and private key files. Import takes one explicit confirmation before Keychain changes.
- Key generation reserves a new private directory, refuses overwrite, and keeps private files local. Public-key installation is confirmed and preserves existing authorized keys. New unencrypted private keys still need filesystem protection and secure backup.
- Metadata, credentials exports, migration backups, Project logs, runner records and control directories use the existing private-write boundaries. Metadata uses locks and snapshot comparison to prevent stale-process overwrite.
- Process cleanup uses live guard ownership, not a PID read from configuration; unknown live groups are not killed. Application commands are explicitly executable content, not sandboxed scripts.

## Diagnostic Controls

Diagnostic text masks password/passphrase/token/API-key fields, quoted values, Authorization and Cookie headers, URI passwords, command-line secret flags, and private-key blocks. Streaming diagnostic readers operate on bounded complete lines so read boundaries cannot expose a split secret marker. Oversized lines are omitted. Runtime inspection and public-key installation errors use the same redaction boundary. Project Web Link Debug output masks its optional plaintext password.

Authentication success is read from a private mode-0600 OpenSSH `-E` client log, not remote command stderr. Each ProxyJump client receives that same local logging boundary with explicit quoting and resolved usernames. The log worker filters client debug output before surfacing bounded redacted errors; the temporary log is removed when the trace closes normally. A regression test verifies that a forged authentication-success line in remote stderr cannot authenticate a connection or commit a password. Like other local logs, a force-killed owner may leave its private temporary client log behind; it contains connection diagnostics, not supplied password bytes.

The support report intentionally avoids configuration, key paths, Host aliases, process command lines and environment values. Tests cover repeated fields, Unicode, quoted spaces, bearer headers, URI query tokens, key blocks and one-byte stream fragmentation.

## Data That Remains Sensitive

Project raw stdout/stderr logs, Project/Snippet command definitions and output, custom SSH directives, filenames, usernames, addresses, and configuration backups can contain confidential information supplied by users or remote programs. They are not a sanitized telemetry channel. Automatic redaction cannot identify an unlabeled arbitrary secret reliably. No diagnostic upload or telemetry service is enabled.

Config-file Web Link passwords are deliberately plaintext after an explicit UI choice and also appear in private local backups. Host passwords always remain in Keychain. Clipboard expiry is best effort: a force-killed TUI cannot promise to clear a password already copied to the system clipboard. System clipboard managers may retain copies.

## Release Gates

Both architecture slices must pass build, signature integrity, version/diagnostic smoke checks and archive validation before `lipo` combines them into a Universal Binary. The combined executable is signed again, both slices and checksums are verified, and the same final archive must run natively on both hosted CPUs. Native hosted architecture tests and the manual acceptance checklist remain required before publication. Packages currently use ad-hoc signatures; Developer ID signing/notarization needs maintainer credentials. The generated Homebrew Formula needs a published tap and matching release assets. No stable release claim is made by preparing these files.

## Dependency Advisory Result

`cargo-audit 0.23.0` used RustSec database commit `af7b3ec33460b8e2de8d422e16d628518891a519`. `rustix` was upgraded from 1.1.5 to 1.1.6 to address RUSTSEC-2026-0486 (unsound extended-attribute buffer initialization). The complete lockfile scan also reports RUSTSEC-2026-0387 (`crypto-bigint` 0.5.5) and RUSTSEC-2023-0071 (`rsa` 0.9.10). Neither package is reachable in the default arm64 or Intel macOS build tree; optional crypto features still retain them in Cargo.lock. They are not silently ignored: `scripts/audit.py` reports dormant findings and blocks on any vulnerability/unsoundness reachable in either actual macOS release graph. Enabling new features requires rerunning this gate.

Reachable build-time dependencies `basic-toml` and `proc-macro-error2` have unmaintained advisories through age's localization macros. They are reported as warnings, not misrepresented as patched or absent. Linux-only D-Bus dependency maintenance notices are also retained in the full scan; Linux is not a supported binary release target. Dependencies and platform credential behavior need ongoing review; one successful scan is not a permanent safety claim.
