# Project Metadata Loss Investigation

## Evidence

- On November 9, 2026, the live `hosts.json` and its rotating backup contained no Projects. The snapshot preserved before credential recovery was already empty (file modification time: 14:59:14 Asia/Shanghai). The subsequent two credential recoveries modified only `credentials`; they did not restore Projects.
- Manual backups from November 8 retain `blackbox-rockbottom` and six commands, but no Web Links. The two saved Web Link password records still exist in LazyTerm's macOS Keychain service. Their account references do not identify the original URL, username or project.
- A LazyTerm process started on November 6 was still running during the investigation. It predates Project support and cannot preserve fields introduced later.
- Before this fix, every metadata mutation serialized the entire in-memory document and unconditionally replaced `hosts.json`. No file revision check, lock or writer journal existed.
- Before the earlier test-isolation fix, App tests without a temporary save path fell back to the production metadata path. The disposable SFTP UI test also constructed default-empty metadata and persisted a remote path through `poll_sftp`.

## Reproduction And Attribution Limits

The `legacy_unconditional_save_reproduces_project_loss_in_a_temporary_file` regression reproduces the previous save implementation: an empty stale snapshot erases a newer Project; a second save replaces the single backup with the empty snapshot too. This explains why a rotating backup alone does not guarantee recovery.

There are two concrete unsafe writer paths: stale running applications and unisolated tests. The historical file has no writer journal, so the exact PID, operation and chronological sequence responsible for the original overwrite cannot be established from the remaining evidence. Do not report either path as the proven sole cause. The default test suite now rejects writes without an explicit fixture path.

## Fix

- OS lock and exact snapshot comparison before saving; reject stale, externally deleted or externally corrupted files.
- Preserve unknown top-level fields when the new binary reads and writes metadata.
- Timestamped private backups retaining 30 versions, independent of the rotating backup.
- Private writer journal containing timestamp, PID, caller, outcome and counts, without passwords, commands or references.
- Compile production `Metadata::save` out of unit tests and require an explicit fixture save path.
- Give the real SSH/SFTP UI fixture its own metadata path.
- Regression tests for the legacy failure, stale snapshots, concurrent writers, external file deletion/corruption, unknown fields and secret-free writer records.

Already-running old executables cannot be retroactively protected. They must be closed before resuming edits with the fixed build. Backups and locks written by the new binary cannot prevent an old non-cooperating executable from replacing the file.

On November 9, the user authorized closing the old process. PID 9859 was verified as the November 6 TUI attached to `ttys010`, sent SIGTERM, and confirmed gone. The persistent Project runner was identified by its `--project-runner` arguments and retained. A startup instance lock now prevents duplicate TUIs for the same data directory, with PID/terminal diagnostics and OS-managed release after exit or crash. Processes started before this change must still be restarted to participate.

## Recovery Boundaries

Restored the six backed-up commands and reconstructed the Home and buy links from the user's previously supplied URLs. Lost command groups were left empty; command type and lifecycle use the defaults represented by that backup. The two recovered Keychain references remain separate; no password was assigned to either link and no usernames were invented. This reconstructed state is not an exact restoration of all lost fields.

On the subsequent grouping check, the live file and retained versions all had empty groups. This was an incomplete recovery, not evidence of another loss after recovery. Following the user's report, the current configuration was backed up to `hosts.json.before-group-rebuild-20261109.bak` and command groups were reconstructed by purpose: Services (`start`), Checks (`check-port-3000`, `check-http`, `project-info`), and Tests (`test-long-running`, `test-exit-1`). These names are a reconstruction, not recovered historical names. Command text, execution type, target, lifecycle, links and password references were preserved.

## Validation

The full suite passes with 155 tests passed and 3 ignored. Clippy, build, formatting and diff checks pass. The disposable SSH/SFTP integration fixture also passes with real SSH enabled, and the live user configuration SHA-256 is unchanged before and after that fixture. Following recovery, another full test run leaves the live configuration containing one project, six commands, two Web Links and two credential records.
