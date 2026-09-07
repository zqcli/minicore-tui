# minicore-tui 0.2.5

Local performance patch. Debug and Release binaries were rebuilt at their existing
paths. Agent remains 0.3.2 and Runtime is unchanged. No commit or tag was created.

## Changes

- Retain the shared conversation snapshot across scrolling, selection, spinner
  ticks, viewport feedback and ordinary editor input. No-op mouse movement and
  identical viewport feedback do not mark the App dirty.
- Cache durable history rows, section/copy/link metadata per session using
  `(render_revision, width, theme, reasoning_visible, tools_expanded)`.
  Content/display mutations invalidate history; live text and steer receipts
  reuse it. The cache is immutable and installed through App-owned updates.
- Borrow the prepared snapshot during painting and hit testing. Clone only
  visible rows for painting. A mouse event prepares a missing snapshot once
  before its helpers run. Copy text is generated once per row during preparation.
- Coalesce expensive layout and real viewport measurement inside the draw
  budget, rather than preparing before every event-loop wait. Small-terminal
  warning screens do not prepare hidden conversation content.
- Preserve required invalidation for history, folds, theme, width, acceptance
  timestamps and late tool presentation/result data. Keep the same copy,
  selection, link and scroll geometry and all existing Steer/RPC semantics.

The live Assistant Markdown display behavior is deliberately unchanged in this
performance-only patch. Reasoning still renders Markdown. The combined live
snapshot still copies cached history rows when live content changes; this is not
an incremental-parser framework or a fully virtualized transcript.

## Measured Results

Same host and locked dependencies, 100×40 TestBackend, five-sample medians.
200 synthetic messages of 1,068 bytes each produce 8,401 prepared rows.
Timings exclude terminal I/O and native font/pixel rendering.

| Operation | 0.2.4 Debug | 0.2.5 Debug | 0.2.4 Release | 0.2.5 Release |
| --- | ---: | ---: | ---: | ---: |
| Thumb drag + prepare + render | 159.69 ms | 2.38 ms | 28.46 ms | 0.20 ms |
| Cached render | 11.67 ms | 2.57 ms | 4.36 ms | 0.20 ms |
| ~1KB live text update with 200 historical messages | 160.53 ms | 8.67 ms | 28.43 ms | 2.87 ms |

Scroll/hover/paint cost remained approximately flat from 10 to 200 historical
messages. Live updates no longer parse history, but still incur history copying
when composing a new full snapshot.

Real iTerm2 3.6.11 also ran the same isolated long-history interaction against
an archived 0.2.4 Release binary, the new Debug binary, and the new Release binary.
All three completed the functional checks. Process CPU time, including an input
marker after each event batch, was:

| Batch | 0.2.4 Release | 0.2.5 Debug | 0.2.5 Release |
| --- | ---: | ---: | ---: |
| 200 hover events then editor input | 1.62 s | 0.01 s | below 0.01 s resolution |
| 100 wheel events then editor input | 1.02 s | 0.01 s | 0.01 s |

These are bounded synthetic measurements, not a promise about every workload or
an estimate of the user's ongoing process CPU percentage. End-to-end native
wall times include AppleScript automation and screen-read overhead.

## Verification

- Stable and Rust 1.85: **412 passed / 17 default-ignored** each.
- All **16 real-Agent loopback E2E scenarios** passed separately.
- The remaining real-TTY enter/restore test passed in iTerm2 with terminal
  stdin/stdout verified; no skip branch was used.
- Formatting, all-target Clippy and docs with warnings denied passed.
- Native checks preserved Thinking part boundaries, rapid FIFO Steer input,
  queue placement, withdrawal/re-admission, 60×16 layout, single final history
  entries, immediate Footer max updates, long-history scrolling/dragging and
  clean shell restoration. Binary hashes were unchanged throughout.
- Nine cache regression tests plus two main-loop preparation tests were added.
  The initial cache suite reproduced four failures before the fix. Existing
  snapshots changed only their visible package version where applicable.

No native pixel screenshot is claimed. Tests used fresh absolute temporary
storage and a synthetic loopback provider, not user config or user sessions.
See [verification/0.2.5](verification/0.2.5/README.md) for logs and evidence.
