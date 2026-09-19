# v0.3.0 F-Review Evidence

This record covers the F-review remediation after the prior final-F source run.
All Rust/Cargo execution in this record was performed on the authorized Linux
builder `root@192.168.20.199`; no local Rust/Cargo command was rerun.

## Scope

- Current TUI source revision: `9e399d9` (`test: validate real PTY and Spec 25 child paths`).
- Prior F-review remediation: `daa944a` (`test: add remote PTY and spec 25 evidence hooks`).
- Historical core source/test/snapshot baseline: `0aa64c5e4d9211351123db059547beddb15c2cce`.
- Current `src/scripts/tests/snapshots` manifest: 375 entries,
  SHA-256 `607a4b52d4b865b6210f8865473f3a8aa8126b15d374b604f050fc4ebb09ba00`;
  local and remote manifests match.
- Agent: `061743369459299e66be97bf97d2b27352a39914` / 0.5.0.
- Runtime: `6cd2bdbc634437dea925495c61c7eb0be10ba171` / 0.4.1.
- Protocol: v1 with the required capability set.
- Raw remote logs: `/root/minicore-tui-v03-refactor/logs/final-f-review-current/`.
- Current quality logs: `quality-snapshots-1.85.0.log` and
  `quality-snapshots-stable.log`.
- Current E2E/performance logs: `e2e-1.85.0.log`, `e2e-stable.log`,
  `performance-1.85.0.log`, and `performance-stable.log`.
- Current child probes: `child-targets-snapshot-1.85.0.log`,
  `child-targets-snapshot-stable.log`, and `pty-report-with-clipboard.json`.
- Current source manifest: `source-manifest.sha256`.

The documentation commits that follow this record are not substituted for the
source remediation commit or either pinned backend revision.

## Current Rust Validation

Rust 1.85.0 and stable both passed the current tree's remote quality run:

```text
830 passed, 0 failed, 53 ignored
fmt --check: passed
archive diff check: Not run (the rsync validation directory had no `.git`; the emitted `Not a git repository` warning is not evidence)
clippy --all-targets -- -D warnings: passed
rustdoc -- -D warnings: passed
```

The ignored count includes the real-PTY and Release performance probes. The
ignored tests are not counted as successful hosted/native/manual acceptance.

The fixed-backend CI job was reproduced in an isolated remote directory with a
fresh `CARGO_HOME`:

1. `cargo fetch --locked` for TUI dependencies;
2. locked fetches for Runtime and Agent dependencies;
3. offline Runtime and Agent builds;
4. exact revision checks for Agent and Runtime; and
5. the serial loopback Agent E2E.

The E2E passed **34/34** on each Rust 1.85.0 and stable run. The clean isolated Agent binary SHA-256 was
`867325ae6f599d89f3c7f3f476559c5f1ed0824de64f1142b84fe94291a27d8f`; this
hash is build-directory/toolchain evidence and does not replace the previously
recorded acceptance binary hash. No Agent or Runtime source was modified.

## Linux OS-PTY Validation

`scripts/pty_terminal_validation.py` uses `pty.openpty`, `fork`, `setsid`,
`TIOCSCTTY`, `dup2`, `TIOCSWINSZ`, and direct `termios` inspection on the
original parent-held slave FD. The final Rust 1.85.0 and stable reports passed
all cases:

| Case | Result |
|---|---:|
| alternate-screen enter/restore and unflushed marker | passed |
| editor suspend/resume | passed |
| raw mode across suspend/resume/restore | passed |
| inherited-PTY panic child | passed |
| key input, resize, Ctrl-C delivery | passed |
| real TUI input, resize, and shutdown | passed |
| real TUI idle for 30 seconds | passed |
| same-slave negative raw-mode fixture detected | passed (`cooked=false`) |
| production `run_commands` with real clipboard helper | passed |

Every PTY child exited 0; the captured unflushed marker was absent; the
negative fixture deliberately left the original slave raw and the harness
observed `ICANON/ECHO=false`, then repaired that slave before the next case.
All restore cases observed cooked mode on the same slave FD. The real TUI
interaction probe recorded 10 actual `Terminal::draw` calls. The 30-second idle
probe recorded 2 actual draw calls, with zero stable-history layout calls and
zero historical body clones. The same reports recorded Linux process CPU and
peak-RSS observations from Python `resource`; these are OS-process observations,
not allocator accounting.

This is Linux kernel-PTY evidence. It is not real iTerm2/manual IME use, native
macOS/Windows execution, or hosted CI; those remain `Not run`.

### Harness Notes

A direct SSH-pipe invocation of the production `run_commands` probe correctly
failed its explicit PTY precondition; the same test then passed through this
external kernel-PTY driver. A later shell quoting error affected only log
summarization after Cargo had completed; the result lines were read directly
from the remote logs. Neither harness event is counted as product evidence.

## Spec 25 Evidence

The deterministic migration gate uses the production App input path after a
near-256 KiB paste and asserts zero ordinary-edit full joins, cached byte-length
tracking, and retained-capacity admission. A separate deterministic job test
keeps a clipboard worker blocked while input, scroll, resize, and an RPC stderr
observation are reduced. The remote acceptance adds a real `xclip` child selected
through a temporary PATH: it does not read stdin, sleeps for two seconds, and
is observed as the direct child by PID/PPid while App input, scroll, resize,
exact turn cancellation, RPC admission, and the Composer draft continue.
The owned timeout/kill/wait test verifies the helper is gone and not a zombie.

Rust 1.85 Release results on the fixed remote builder:

```text
production App draft edit: 4096 edits, Rust 1.85 P95=700 us, P99=755 us,
  draft_bytes=258048, retained_capacity_estimate=5402688,
  composer_full_joins_delta=0
same direct Composer workload: Rust 1.85 P95=192 us, P99=220 us,
  draft_bytes=258048, retained_capacity_estimate=5402688,
  composer_full_joins_delta=0
C2c 120x40: Rust 1.85 p95_us=3571, p99_us=4367, durable_rows=43870,
  layout_calls=0, history_bytes_cloned=0, viewport_bytes=4396336
full current performance ignored set: 9 passed, 0 failed
stable repeat: App p95_us=662 p99_us=676; direct Composer p95_us=238 p99_us=243;
  C2c p95_us=3565 p99_us=4295; full set 9 passed, 0 failed
```

The 256 KiB target is a local edit-processing target. These numbers are not
terminal input-to-frame latency. The draw counter is an explicit production
scheduling counter, not a timing surrogate.

The exact Spec §25 scenarios are now directly covered. The real OS clipboard
helper uses the production `NativeClipboard` path and a direct child PID; the
real harness=false Agent child pauses stdin while producing 72 valid roughly
1 MiB stdout frames. The transport test reaches the bounded 64 MiB wire budget,
keeps 28 normal plus four control admissions synchronous, drains the flood,
verifies exact FIFO response IDs, and shuts down with the child reaped. This is
Linux OS-child evidence, not an external Provider or native desktop claim.

## Independent Baseline

Commit `9d11ee6` was exported with `git archive`, transferred to the remote
builder, and built in an independent directory. Archive SHA-256:

```text
32c307491d7aa6cedce26c28fade6ad2296645ebc029ae68953e75ec2e6d12d5
```

The baseline package is TUI 0.2.8. The same direct 256 KiB Composer workload
recorded:

```text
P95=1492 us, P99=1907 us, bytes=258048
```

A temporary, explicitly labeled draw-counter instrumented copy of that archive
recorded 10 draws during the same interaction probe and 2 draws during the
30-second idle probe. The original archive was not modified. This makes the
idle scheduling comparison reproducible without claiming that the unmodified
historical binary exposed a counter.

The builder did not contain `/usr/bin/time`; therefore no `time -v` CPU/RSS
record was fabricated. Python `resource` values are recorded only for the PTY
process probes. Exact allocator capacity and RSS qualification remain
`Not run`.

## Acceptance Boundary

The hosted GitHub Actions matrix has configuration coverage and the fixed job
was reproduced remotely, but no hosted run exists for this branch. Native
macOS/Windows, manual iTerm2/IME, external-provider access, and generation of
a real Agent history item above 8 MiB remain `Not run`.

Phase F once violated the original remote-only Rust/Cargo requirement by
running locally. Those local results are excluded from acceptance; the affected
validation was rerun remotely on Rust 1.85.0 and stable and was not substituted
back from the local run.
