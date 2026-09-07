# TUI 0.2.6 — Debug Rendering Cost

Local uncommitted patch; Agent remains 0.3.2 and Runtime is unchanged.

## Cause And Change

The 0.2.5 history cache already retains prepared layout across busy ticks.
However, each `terminal.draw` still converts the visible lines into Ratatui Cells.
The no-wrap `Paragraph` path traverses graphemes and measures display widths;
terminal buffers also need to be populated and compared. Unoptimized dependency
code substantially amplifies this work in an ordinary Debug build.

`Cargo.toml` now sets `[profile.dev.package."*"] opt-level = 2`. The TUI package
remains at level 0 with full debug information and assertions. Dependency debug
information/assertions remain enabled, but optimization can inline dependency
functions or obscure their local variables. The first dependency rebuild is
more expensive; this is not a cost-free change to dependency debugging.

There are no application Rust source changes, dependency upgrades, new caches,
frame-rate changes, or Agent/Runtime execution changes. Eleven startup snapshots
only change `v0.2.5` to `v0.2.6`. The Release profile is unchanged.

## Measured Results

Synthetic 240×80, 200 mixed CJK/Emoji/Markdown history messages, 10,016 prepared
rows including the busy tail: busy frame wall time **12.4824 → 1.1832 ms** with
only dependency optimization. The local 3 ms/frame probe changes from expected
failure to pass. Both probe configurations keep the TUI crate at level 0.

Real iTerm2 3.6.11, actual **220×53**, synthetic history and a real Agent whose
loopback provider holds the next response. CPU percentages below are medians of
three 8-second process CPU-time windows, relative to one logical core:

| Build | Busy-Wait TUI CPU | Samples |
|---|---:|---|
| Preserved 0.2.5 Debug | 7.370% | 7.370%, 7.369%, 7.370% |
| 0.2.6 Debug | 1.250% | 1.250%, 1.373%, 1.249% |
| 0.2.6 Release | 1.000% | 2.125%, 1.000%, 0.875% |

The new Debug reduction is approximately 83%. Idle CPU was near zero; Agent
CPU increments during held responses were below the 0.01-second `ps` resolution.
A measured 0.00 seconds does not mean mathematically zero CPU. No native pixel
screenshot or universal workload guarantee is claimed.

## Limits And Verification

This addresses periodic drawing while waiting, not all streaming costs. Live
snapshot composition still copies cached history rows. The dense 10,042-row
probe's 1 KB live-text update remains **34.5923 ms** in dependency-optimized
Debug (44.7607 ms before); this is not a fully incremental transcript. Live
Assistant Markdown behavior remains unchanged. The render budget remains 30 FPS;
only busy timer ticks are approximately 10 Hz.

Stable and Rust 1.85: **412 passed / 17 ignored** each; **16/16** real-Agent
loopback E2E tests and real-TTY restoration passed separately. Formatting,
Clippy with warnings denied, rustdoc and both builds passed. Native interaction
checks passed for all three binaries, including Thinking boundaries, Steer FIFO,
Option+Up, 60×16, immediate Footer settings, input, scrolling and normal exit.
All 59 application/test Rust source hashes are unchanged for this patch.

Debug and Release binaries were rebuilt at their original paths. Existing live
processes were not restarted; finish the current task and restart the TUI normally
to use 0.2.6. No user config or session was read or changed for this verification.

See [verification and raw evidence](verification/0.2.6/README.md).
