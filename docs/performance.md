# Idle CPU Investigation

## macOS PTY Measurement - 2026-11-25

A report of approximately 70% CPU while idle prompted inspection of the TUI
loop and background workers. No running LazyTerm process or CPU sample from
that incident was available; its exact cause has not been established.

Confirmed sources of unnecessary work:

- The TUI polled all background state and rendered the entire page every
  100 ms. Every terminal event, including ignored mouse movement, could
  trigger another full poll and render sooner.
- Project discovery enumerated processes and listening ports every three
  seconds even on unrelated pages.
- Project discovery enumerated listening ports before determining whether
  any external process matched a configured command and working directory.

Changes:

- Background polling has its own 100 ms deadline. Terminal events cannot
  accelerate it.
- Idle pages redraw once per second. Visible transfers, running commands,
  and other pending operations use 100 ms redraws. Keyboard input, paste,
  left clicks, resize and focus return invalidate the display, with input
  redraws capped at approximately 30 frames per second.
- Ignored events do not invalidate the display.
- Automatic Project discovery runs on Projects and local Runtime pages.
  Explicit refresh and command preflight still work from other pages.
  Managed process polling, recovery and cleanup continue on every page.
- Listening ports are enumerated only after an external Project match
  has been found. Matching runs retain their descendant port details.

Measurement used the local debug binary in a 200-column by 50-row PTY,
with a temporary HOME and an empty SSH config. The user's three Project
definitions (18 commands) were copied to the disposable metadata file.
No commands were started, no remote connections were made, and the real
configuration and credential store were not modified.

| Scenario | Observation window | Cumulative CPU time |
| --- | --- | --- |
| Before, idle Hosts page | 12 seconds | 2.042 seconds |
| After, idle Hosts page | 12 seconds | 0.138 seconds |
| After, approximately 9,530 ignored mouse movements | 12 seconds | 0.195 seconds |

CPU time was measured with Python `resource.getrusage(RUSAGE_CHILDREN)` after
waiting for the PTY child; it includes CPU accounted to its reaped inspection
children. Before the change the child was terminated with TERM. Afterward it
exited normally with `q`, adding roughly 0.6 seconds of wall-clock teardown.
These are short local measurements, not release-build benchmarks or proof
that every possible source of the reported 70% CPU has been eliminated.

Regression tests cover simulated event floods, polling and redraw deadlines,
input responsiveness, hidden-page discovery suppression, remote Runtime,
and fast redraws while visible work is pending. Existing Project preflight,
external discovery, managed lifecycle and CLI/TUI refresh tests also pass.

Validation of the modified working tree:

- `LAZYTERM_SFTP_FIXTURE=1 cargo test --locked -- --test-threads=1`:
  300 unit/App tests and 9 CLI tests passed; 10 unit helpers/opt-in tests and
  one CLI helper were ignored. The disposable SSH/SFTP fixture was enabled;
  the separate password fixture and native credential-store probes were not.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`,
  `cargo fmt --check`, and `git diff --check` passed.
