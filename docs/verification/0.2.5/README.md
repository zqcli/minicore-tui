# 0.2.5 Performance Verification

Local macOS 15.7.3 / Intel i5-12400 evidence. All results concern the current
uncommitted TUI build; no new cross-platform or native pixel-parity claim.
Agent source/config/store and Runtime were not changed for this patch.

## Gates

| Gate | Result | Evidence |
| --- | --- | --- |
| Stable all targets | 412 passed / 17 default-ignored | `stable.log` |
| Rust 1.85 all targets | 412 passed / 17 default-ignored | `msrv.log` |
| Real Agent, isolated loopback | 16/16, explicitly enabled | `e2e.log` |
| Real TTY restoration | 1 passed; both streams TTY, no skip | `iterm/native-pty.log` |
| fmt / Clippy / docs | Passed, warnings denied | Corresponding logs |
| Original-path Debug and Release | Both report 0.2.5 | `builds.txt` |

The 17 ignored tests are the 16 E2E scenarios and the one real-TTY test, all run
separately. `cache-red.log` records the four initial failures; `cache-green.log`
records the repaired no-op input invariant. The stable suite includes all nine
new cache tests and both new frame-preparation tests. Gemini 3.8 Flash high
assisted with the initial cache tests and read-only review; the parent completed
implementation and independent verification.

## Component Benchmark

`perf/probe.rs` invokes actual App::update, prepare_conversation and ui::render,
with in-memory fixtures and a 100×40 TestBackend. There is no provider or Store.
Each message is 1,068 bytes; 10/50/200 messages yield 421/2,101/8,401 rows.
Each regular measurement uses a warmup and five samples, reporting the median.
Debug and Release measurements ran sequentially. The external crate's lockfile
was seeded from the project lockfile and dependency versions matched.

`perf/debug-before.csv`, `release-before.csv`, `debug-after.csv` and
`release-after.csv` retain the measurements. Release uses normal optimization
with symbol information for profiling. The benchmark intentionally calls a
frame even for a hover event; the production main loop now avoids that draw.
It excludes real terminal I/O and native pixel work, and does not claim that
live full-snapshot composition is independent of history size.

## Native iTerm2

`iterm/driver.py` ran in iTerm2 3.6.11 and drove real TUI/Agent subprocesses with
synthetic loopback responses and a gated real bash tool. Every run created a
fresh absolute temporary config, data directory and workspace. Native evidence
is screen text/input/protocol assertions, not pixels. Current-row extraction
excludes resize scrollback.

Three final runs passed:

- `iterm/debug`: new 0.2.5 Debug.
- `iterm/release`: new 0.2.5 Release.
- `iterm/before-release`: a preserved 0.2.4 Release executable, run now as the
  performance control. This is not relabeled historical evidence or a 0.2.5 binary.

The shared scenario verifies Thinking summaries, rapid Alpha/Beta steering,
Option+Up withdrawal/re-admission, queue placement above Working, 60×16 layout,
separate model-request inclusion and single final User cards, immediate Footer
max, a long historical answer, input after 200 hover events, 100 wheel events,
scrollbar drag to the beginning and clean exit. Input markers placed after event
batches prove the input stream has drained before recording CPU time.

CPU time uses `ps time` for the TUI identified through the Agent child with the
unique temporary config path. Resolution is 0.01 seconds: a reported 0.00 means
below this resolution, not literally zero CPU cost. Native wall times include
AppleScript and screen-read overhead. Individual timings are single-run evidence;
component timings are medians. Result JSON contains executable hashes, which
were unchanged throughout each run.

The first native attempt failed only at PID discovery after reaching the long
history. Full-width ps output and exact owned-Agent parent identification fixed
the driver; final runs exercised all assertions. No product code changed after
binary freeze. No screenshot was attempted or reconstructed as a replacement.

See [release-0.2.5.md](../../release-0.2.5.md) for the results table and remaining
scope. `source-hashes.txt` and `builds.txt` identify the delivered inputs/artifacts.
