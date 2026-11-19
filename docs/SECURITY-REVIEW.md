# Beta Security and Diagnostics Review

Review date: 2026-11-15. This is an internal source review with regression tests, not an independent security audit or a guarantee that arbitrary command output is secret-free.

## Stable Core Follow-Up

The 2026-11-14 follow-up covers the Agent CLI and Host write boundaries introduced
or checked since that baseline. It is not a new dependency advisory scan or final
v0.9 release approval.

- Setting a Host's SSH key now re-reads the catalog under the shared SSH writer
  lock and rejects changed or removed Host connection fields before assigning
  IdentityFile. Duplicate definitions across Includes remain refused; repeated
  symlink Includes are resolved to one physical source. External editors can
  still race a write because they do not share this lock.
- Agent CLI responses allowlist configuration fields and redact recognized
  secret patterns. Saved credentials, runner control sockets/tokens and private
  key contents remain outside its response schema. Process actions require an
  explicitly authorized saved Project command and an observed run identity.
- Remote Runtime and CLI Project operations require explicit Host selection,
  noninteractive authentication and strict known-host checking on jump routes.
  They do not fetch Keychain passwords or offer first-use trust bypasses.
- The remote Project guard runs over the existing SSH session without installing
  a server daemon. Its anchored remote process group is cleaned after EOF or a
  20-second missing heartbeat; TERM escalates after two seconds. A deliberately
  detached process or remote reboot is outside that lifecycle guarantee.
- Configuration refresh retains loaded data after invalid/missing metadata and
  defers updates under open forms; stale writes fail instead of replacing the
  CLI writer's changes. The CLI/TUI PTY fixture exercises that boundary.

Candidate-specific advisory results, manual OS Keychain permissions,
native CPU release runs and user acceptance remain pending in
[v0.9-acceptance.md](v0.9-acceptance.md).

### 2026-11-15 Data and File-Permission Follow-Up

- Metadata saves now enforce mode `0700` on their containing data directory and
  `0600` on the live document, lock, write journal, timestamped backups, and
  atomic sibling backup. Existing permissive metadata files are tightened on
  save. Metadata writes refuse symlinked lock/journal paths; the atomic
  replacement does not follow a symlink at the destination.
- SSH config writes reject symlinked live and backup destinations. The exact
  sibling backup is written atomically; private metadata recovery writes force
  mode `0600`, while ordinary SSH config writes preserve the existing file
  mode.
- UUID identity is now searched across the full collection before falling back
  to a same-name match during import. Previously, an earlier same-name record
  could take precedence over a later exact UUID match and misdirect child
  records or credential references. Regression coverage exercises Projects,
  commands, links, credentials and imported secret-reference mapping.
- Isolated recovery coverage migrates a schema-2 Project with groups, command,
  Web Link, local plaintext credential and unknown fields; imports the same
  bundle twice; rejects a stale import after a concurrent write; and restores
  exact metadata and SSH bytes from backups. No user profile was used.
- A fresh RustSec checkout resolved the earlier incomplete database. The
  2026-11-15 scan passed the default macOS dependency-graph gate with database
  commit `4ef2df8e806a65e2f1ecc14f56f51249062b23d6`. Maintenance warnings and
  dormant vulnerabilities remain reported below; this does not accept a future
  changed lockfile, enabled feature set, or final release candidate.

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

On 2026-11-15, `cargo-audit 0.23.0` used RustSec database commit `4ef2df8e806a65e2f1ecc14f56f51249062b23d6` through `python3 scripts/audit.py --database /private/tmp/lazyterm-v09-advisory-db`. The gate passed for both default macOS build graphs; the current working tree was uncommitted. `rustix` was previously upgraded from 1.1.5 to 1.1.6 to address RUSTSEC-2026-0486 (unsound extended-attribute buffer initialization). The complete lockfile scan still reports RUSTSEC-2026-0387 (`crypto-bigint` 0.5.5) and RUSTSEC-2023-0071 (`rsa` 0.9.10). Neither package is reachable in the default arm64 or Intel macOS build tree; optional crypto features still retain them in Cargo.lock. They are not silently ignored: `scripts/audit.py` reports dormant findings and blocks on any vulnerability/unsoundness reachable in either actual macOS release graph. Enabling new features requires rerunning this gate.

Reachable build-time dependencies `basic-toml` and `proc-macro-error2` have unmaintained advisories through age's localization macros. They are reported as warnings, not misrepresented as patched or absent. Linux-only D-Bus dependency maintenance notices are also retained in the full scan; Linux is not a supported binary release target. Dependencies and platform credential behavior need ongoing review; one successful scan is not a permanent safety claim.
