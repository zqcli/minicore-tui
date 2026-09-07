# TUI 0.2.7 — Scrolling Output Cost

Local, uncommitted delivery. Agent remains 0.3.2 and Runtime is unchanged.
All compilation and formatting for this patch occurred on the authorized Linux
builder; macOS only executed, inspected and installed the resulting artifacts.

## Changes

- Debug package optimization is now level 1; dependency optimization remains
  level 2. Debug information, assertions and overflow checks remain enabled.
  Optimization can affect stepping, breakpoint placement and local variables.
- `TerminalWriter<Stdout>` batches small ANSI writes through a 64 KiB buffer.
  Full buffers and explicit flushes preserve bytes/order and propagate errors.
  This does not make arbitrarily large frames atomic, remove Stdout's internal
  lock, or change terminal backpressure behavior.
- Pending bytes are discarded on writer Drop and before terminal restoration or
  clear-failure rollback. A zero-capacity forwarding writer handles teardown,
  because Ratatui's own Drop can show the cursor and flush.
- No new Cell cache, render-rate change, geometry change, Agent/Runtime change,
  or live Assistant Markdown change. The 30 FPS budget and 100 ms busy tick remain.
- Two pre-existing recovered-composer let-chains were expressed in Rust 1.85
  syntax. A test-only RPC fixture race now uses independently owned directories,
  atomically created without reusing an existing directory.
- macOS cross-linking uses Clang/LLD with a macOS SDK. The tested Zig 0.13 path
  produced native SIGSEGV during even a standalone `catch_unwind`; its precise
  internal cause was not established. Rejected artifacts were never installed.

## Native Measurements

Real iTerm2, actual 220×53, synthetic long multilingual history, absolute 25 Hz
wheel injection with per-event timestamps, 800 events reversing every 80 events.
The old/new comparison uses Rust 1.98, Clang/LLD 19 and the same SDK. CPU is process
CPU relative to one logical core, not a component wall-time estimate.

| Variant | Three 8-second CPU windows | Median |
|---|---|---:|
| 0.2.6 Debug, matched build | 5.000%, 5.000%, 4.998% | 5.000% |
| Buffer only, package opt0 | 4.997%, 4.998%, 4.873% | 4.997% |
| Old code, package opt1 | 3.999%, 4.124%, 3.998% | 3.999% |
| Buffer + opt1 experiment | 3.498%, 3.250%, 3.374% | 3.374% |
| Final 0.2.7 Debug | 3.000%, 3.000%, 3.122% | 3.000% |
| Final 0.2.7 Release | 2.624%, 2.747%, 2.624% | 2.624% |

The combined Debug change reduced measured scrolling CPU by roughly one third
to two fifths across the experiment and final run. Buffering alone did not show
a meaningful CPU improvement at opt0. Early variable-rate injection and the
original locally compiled 0.2.6 binary are retained as diagnostic data, not used
for this causal comparison.

The three primary windows exclude sampling. The supplementary 800-event span
was 1.60s CPU for the matched baseline, 1.08s for the combined experiment and
0.97s for final Debug; it includes subsequent 3-second sampling and drain work,
so it is not a pure per-event benchmark. `ps time` has 0.01s resolution; zero
Agent increments mean below that resolution. These are controlled local runs,
not CPU ceilings for arbitrary sizes, scrolling rates or streaming workloads.

## Verification

- Stable and Rust 1.85: **423 passed / 17 ignored each**; **16/16** real-Agent
  loopback E2E passed separately. Fmt, Clippy with warnings denied, docs and builds passed.
- Native execution of cross-built tests: **10 writer tests** plus fixture
  isolation passed. Identical 6,751 ANSI bytes required 3,801 versus 1 underlying
  mock writes. Short/interrupted writes, full/large buffers and error/panic paths are covered.
- Real TTY: normal exit 0, panic exit 101, all stdio handles were TTY, `stty`
  matched and the unflushed marker was absent on both paths.
- Final Debug/Release iTerm2 checks passed for Thinking boundaries, Steer FIFO,
  withdrawal, narrow layout, Footer settings, long-history input and shell restoration.
- Mach-O x86_64, minimum deployment header 11.0, signatures, parser checks and
  binary/dSYM UUID agreement passed. This is not a native macOS 11 certification.
- Native pixel screenshots and new Windows acceptance remain unverified.

## Artifacts And Cleanup

The verified executables are installed at `target/debug/minicore-tui` and
`target/release/minicore-tui`. Previous executable inodes are preserved under
`target/preserved-0.2.6-RXdxEP/`; no user process was restarted or terminated.
Agent's executable hash is unchanged.

Explicit Cargo-cache cleanup preserved executable formats, executable files,
dynamic libraries and symlinks. Local cleanup removed 239,576 files and verified
1,490 retained binary/library hashes; remote cleanup removed 40,815 files and
verified 778. Project `target` fell from about 20 GB to about 3 GB including the
preserved executables and new debug symbols. User stores/configurations were not
cleaned. Remote caches subsequently grew during the authorized new builds.

`target/debug/minicore-tui.dSYM` contains the remotely generated debug symbols.
For source-level LLDB debugging of this cross-build, map the remote source root:

```text
settings set target.source-map /root/minicore-tui-027-RXdxEP/minicore-tui /Users/zzq/Develops/minicore-tui
```

See [verification/0.2.7](verification/0.2.7/README.md) for raw measurements, hashes,
failed experiments, reproducible sources and build logs. No commit/tag was created.
