# Data Format and Upgrade Policy

This policy describes the current implementation. A development milestone or
application version does not imply a new data format or a stable public release.

## Sources of Truth

OpenSSH configuration (`~/.ssh/config` and its Includes) owns Host connection
fields. LazyTerm edits supported directives in the original file and preserves
unrelated options and comments. It refuses ambiguous edits to duplicated or
shared Host blocks. Advanced OpenSSH grammar is not reconstructed from the
Host list.

LazyTerm metadata lives in `~/Library/Application Support/lazyterm/hosts.json`
on macOS. It owns groups, credentials references, Projects, Snippets, Tunnels,
Settings and saved browser locations. Standard SSH configuration is not copied
into this file as an authoritative second Host database.

Current metadata schema: **3**. Entity IDs are UUIDs; Host references use SSH
aliases. The configuration export envelope has its own `format_version`,
independent of the application version and metadata schema. CLI JSON envelopes
have `schema_version: 1`; run IDs are opaque strings, not entity UUIDs.

## Reading and Migrating

- Missing metadata on a fresh installation starts with empty defaults.
- TUI startup upgrades schema 1/2 documents to schema 3. A missing version means
  schema 1. Numeric entity IDs are replaced with UUIDs and supported references
  are rewritten together, including a local mapping for legacy runner records.
- Migration uses the ordinary locked, backed-up metadata writer. A stale disk
  snapshot aborts the save rather than overwriting another writer's data.
- Invalid JSON, invalid versions, version 0 and future schemas are rejected.
  Existing running clients retain loaded data and block saves after a missing
  or invalid metadata file; disappearance is not treated as an empty project list.
- CLI queries do not migrate or reconcile configuration. A CLI version error
  requires opening the matching TUI once, rather than a read silently saving data.
- Unknown top-level metadata fields are retained. This is not a guarantee that
  an old binary understands new nested fields or new execution semantics.

Import uses the in-app validator and merge transaction, not archive extraction
over the live data directory. Matching UUIDs take precedence across the full
collection; when no UUID matches, same-name records are used as a fallback
(Tunnel definitions use their label and credentials also require matching kind).
Incoming supported fields win while omitted local fields and child records are
retained. Host conflicts merge by alias with the incoming definition winning.
Password archives are separately encrypted and explicitly confirmed. Neither
normal configuration import nor export moves private-key files.

## Backups and Failure Boundaries

Before replacing existing metadata, the writer stores its exact previous bytes
under `hosts.json.backups/`. It retains the latest **30** timestamped JSON
snapshots. `hosts.json.writes.jsonl` records write attempts, outcomes, caller and
record counts; it is a diagnostic journal, not a replayable transaction log.
The atomic writer also keeps a `config.lazyterm.bak` extension on a sibling
backup (for example `hosts.config.lazyterm.bak` for `hosts.json`).
Do not rely on either local backup as your only durable backup.

Configuration import additionally keeps pre-import files in `import-backups/`
and rolls back its failed writes. Credential import has a separate encrypted
backup. Keychain secrets are not restored by copying metadata or SSH files;
use the credential import flow. Plaintext Web Link passwords, when explicitly
selected by the user, also exist in private local metadata backups.

Individual metadata and SSH-file replacements are atomic, with locks shared by
LazyTerm writers. They are not one atomic transaction spanning every SSH file,
metadata, the OS Keychain and runner records. External editors do not participate
in LazyTerm locks; inspect disk state after a reported partial operation.

## Upgrade and Recovery

1. Stop or deliberately retain persistent Project runs. Quit the TUI and finish
   Agent CLI writes before replacing the executable.
2. Back up the complete LazyTerm data directory and `~/.ssh` privately. Export
   encrypted credentials separately if a credential restore may be needed.
3. Install the reviewed candidate and open it once to perform any migration.
   Check Hosts, Project groups/commands/links, credential associations and runs.
4. If migration fails, preserve the error and backup copies. Do not delete the
   live file or replace it with `{}` to make startup succeed.

For manual recovery, close all writers first, preserve the broken current file,
then restore a reviewed snapshot to `hosts.json` and reopen the matching version.
Restore SSH files from the same backup when the operation changed both. A
runner record is evidence to inspect, never authorization to signal a saved PID.

Downgrading a migrated file in place is unsupported. Rollback requires the old
binary and a complete pre-upgrade configuration backup; separately inspect
credentials and running services. Future incompatible schema changes must add
explicit validation/migration, reference-preservation tests, backup/failure tests,
and an updated policy before being declared ready.
