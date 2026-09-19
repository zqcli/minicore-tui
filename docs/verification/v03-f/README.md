# v0.3.0 F-Review Evidence

This record covers the F-review remediation after the prior final-F source run.
All Rust/Cargo execution in this record was performed on the authorized Linux
builder `root@192.168.20.199`; no local Rust/Cargo command was rerun.

## Scope

- TUI remediation commit: `daa944a` (`test: add remote PTY and spec 25 evidence hooks`).
- Core source/test/snapshot baseline: `0aa64c5e4d9211351123db059547beddb15c2cce`.
- Agent: `061743369459299e66be97bf97d2b27352a39914` / 0.5.0.
- Runtime: `6cd2bdbc634437dea925495c61c7eb0be10ba171` / 0.4.1.
- Protocol: v1 with the required capability set.
- Raw remote logs: `/root/minicore-tui-v03-refactor/logs/final-f-review/`.

The documentation commits that follow this record are not substituted for the
source remediation commit or either pinned backend revision.

## Current Rust Validation

Rust 1.85.0 and stable both passed the current tree's remote quality run:

```text
830 passed, 0 failed, 48 ignored
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

The E2E passed **34/34**. The clean isolated Agent binary SHA-256 was
`867325ae6f599d89f3c7f3f476559c5f1ed0824de64f1142b84fe94291a27d8f`; this
hash is build-directory/toolchain evidence and does not replace the previously
recorded acceptance binary hash. No Agent or Runtime source was modified.

## Linux OS-PTY Validation

`scripts/pty_terminal_validation.py` uses `pty.fork`, `TIOCSWINSZ`, and direct
`termios` inspection. The final report passed all cases:

| Case | Result |
|---|---:|
| alternate-screen enter/restore and unflushed marker | passed |
| editor suspend/resume | passed |
| raw mode across suspend/resume/restore | passed |
| inherited-PTY panic child | passed |
| key input, resize, Ctrl-C delivery | passed |
| real TUI input, resize, and shutdown | passed |
| real TUI idle for 30 seconds | passed |

Every PTY child exited 0; the captured unflushed marker was absent; the
post-exit PTY had `ICANON` and `ECHO` restored. The real TUI interaction probe
recorded 10 actual `Terminal::draw` calls. The 30-second idle probe recorded 2
actual draw calls, with zero stable-history layout calls and zero historical
body clones. The same report recorded Linux process CPU and peak-RSS
observations from Python `resource`; these are OS-process observations, not
allocator accounting.

This is Linux kernel-PTY evidence. It is not real iTerm2/manual IME use, native
macOS/Windows execution, or hosted CI; those remain `Not run`.

## Spec 25 Evidence

The deterministic migration gate uses the production App input path after a
near-256 KiB paste and asserts zero ordinary-edit full joins, cached byte-length
tracking, and retained-capacity admission. A separate deterministic job test
keeps a clipboard worker blocked while input, scroll, resize, and an RPC stderr
observation are reduced.

Rust 1.85 Release results on the fixed remote builder:

```text
production App draft edit: 4096 edits, P95=761 us, P99=868 us,
  draft_bytes=258048, retained_capacity_estimate=5402688,
  composer_full_joins_delta=0
same direct Composer workload: P95=237 us, P99=238 us,
  draft_bytes=258048, retained_capacity_estimate=5402688,
  composer_full_joins_delta=0
C2c 120x40: p95_us=3380, p99_us=3617, durable_rows=43870,
  layout_calls=0, history_bytes_cloned=0, viewport_bytes=4396336
full current performance ignored set: 7 passed, 0 failed
```

The 256 KiB target is a local edit-processing target. These numbers are not
terminal input-to-frame latency. The draw counter is an explicit production
scheduling counter, not a timing surrogate.

Two exact Spec §25 scenarios remain unrun: cancellation of a real OS clipboard
helper that hangs for two seconds, and a fixed Agent paused on stdin while
producing oversized stdout. The injected blocked-clipboard worker and existing
RPC backpressure tests cover the same nonblocking ownership/backpressure
properties, but they are not relabeled as those exact OS/Agent scenarios.

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

The previous local Rust 1.98.0 run is retained only as an execution deviation
in the current documentation. It is not part of the current acceptance
counts, does not substitute for the remote runs above, and was not rerun.
