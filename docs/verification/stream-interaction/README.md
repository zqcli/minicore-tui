# Streaming Interaction Verification

Uncommitted source follow-up to TUI `248a748` (0.2.8), tested with Agent
`7d3700f` (0.3.3). Separate `gpt-5.6-luna:max` sessions performed implementation
and independent review; the parent resolved findings and ran final acceptance.
No package version, dependency, Agent source, Runtime pin or RPC contract changed.

## Changes

- Live Tool mouse folding uses the same full identity and effective expansion
  policy as rendering. Overrides survive presentation, completion and durable
  history reconciliation; live-only folds reuse durable Markdown preparation.
- Busy ticks request 33 ms rather than 100 ms. A latched absolute deadline and
  overdue dispatch prevent continuous RPC/input from postponing animation. The
  render cap remains 30 FPS; this is not a guarantee of 30 displayed frames/s.
  Selection auto-scroll retains a separate 50 ms monotonic deadline.
- Scrollbar dragging updates body and thumb through one pending scroll
  projection, with no separate 90 ms thumb animation. Mouse release commits only
  after verifying current geometry. Unrelated RPC/repreparation preserves a
  drag; resize, real geometry/lifecycle changes and explicit scrolling cancel it.
  Marker/visible-row budget is frozen during dragging and restored on release.
- Marker rows cannot activate off-screen links, selections or folds. A stationary
  scrollbar drag has no timer of its own. Idle disarms animation completely.
- Live Assistant Markdown remains deferred. Existing plain live text and
  Markdown reasoning/durable text are unchanged; large live snapshots still
  copy cached history rows. No new renderer/cache framework was introduced.

## Remote Gates

All Rust compilation, tests and formatting ran on the authorized Linux builder.
Only downloaded executables and read-only binary checks ran locally. Source
hashes in `source/` match the remote files used for the accepted builds.

- Stable Rust 1.97.1 and MSRV 1.85.0: **438 passed, 17 ignored each** across all
  targets. The ignored tests are 16 opt-in Agent E2E cases and one real-TTY case.
- Those **16 E2E tests were then explicitly run and all passed** with the freshly
  built real Agent and loopback provider (`remote/final-e2e.log`).
- Stable strict Clippy, format check and warning-denied rustdoc passed. MSRV
  Clippy is not claimed: existing diagnostics in unchanged files remain.
- Genuine behavioral RED/GREEN logs cover live folding, timer starvation,
  drag/reprepare, stale Loop identity, marker hits, stationary dragging,
  live-growth-before-release and selection edge re-entry. The initial
  `revision2-red-3.log` is a compile error, not a valid behavioral RED.
- Real iTerm2/PTTY normal exit 0 and panic exit 101 passed: stdio were TTYs,
  `stty` was restored, and pending frame bytes did not leak after restoration.

macOS comparison and accepted artifacts use Rust 1.98.0, Clang/LLD 19.1.7,
SDK 26.2, x86_64 and deployment header 11.0. Both Debug builds use package opt1 /
dependency opt2. Signatures, Mach-O metadata, parser self-tests and matching
Debug/dSYM UUID `4C4C4468-5555-3144-A141-41C71040F0CB` were checked locally.
Header checks do not certify execution on macOS 11 or real upstream TLS.

## Native Results

Native runs used iTerm2 3.6.11, fresh absolute temporary stores/workspaces, dummy
keys and a loopback provider. No user conversation/configuration or external
provider was accessed. The same Agent hash was used throughout.

| Behavior | Baseline Debug | Fixed Debug | Fixed Release |
| --- | --- | --- | --- |
| Live Tool click collapses | No | Yes | Yes |
| Body follows before mouse release | No | Yes | Yes |
| Spinner glyph states observed during ~2 s RPC flood | 1 | 36 | 39 |
| Distinct body windows during continuous drag (12 samples) | 1 | 9 | 11 |

The baseline native run intentionally has `expect_fixed=false`: PASS means the
control workflow completed, not that it meets the new behavior. Raw drag-window
counts included the right-edge thumb (baseline count 5); the separately retained
`drag-body-corrected.json` excludes the thumb/padding and yields the table above.
The source driver was corrected after measurement, and raw observations remain.
Glyph observation is screen-text sampling, not a precise FPS measurement.

CPU values below are percentages of one core, from three 8-second windows:

| Workload | Baseline Debug samples | Fixed Debug samples | Fixed Release samples |
| --- | --- | --- | --- |
| Held Working, 220×53 | 1.249 / 1.374 / 1.249 | 3.497 / 3.496 / 3.496 | 3.246 / 3.373 / 3.371 |
| Wheel 25 Hz, 220×53 | 3.500 / 3.621 / 3.498 | 3.372 / 3.500 / 3.499 | 3.247 / 3.249 / 3.247 |
| Scrollbar drag 25 Hz, 100×32 | 2.123 / 1.998 / 1.998 | 1.498 / 1.624 / 1.498 | 1.499 / 1.374 / 1.498 |

Held Working CPU **increased** with the more frequent animation; wheel CPU is
essentially unchanged. Drag now paints moving content and has lower measured CPU
in this fixture. Input timestamps are retained (200–201 events/window). Sampling
and screen observation were outside the primary CPU windows. Idle and stationary
drag accumulated less than `ps`'s 0.01-second CPU resolution; they are not proven
zero-cost. These are controlled fixtures, not universal CPU/latency ceilings.

Native checks also retained Thinking boundaries, FIFO/withdrawal, acknowledged
reasoning Footer settings, 60×16 layout, long-history interaction and clean exit.
Pixel screenshots remain **UNVERIFIED**; screen text is not pixel evidence.
Two early native harness failures (Rail-prefixed title locator and follow-tail
assumption after tool clicking) are retained separately, not product failures.

## Component Costs

`perf/` contains the reused synthetic 240×80 dense CJK/emoji/Markdown probe,
manifest and raw CSVs. Run it from a remote sibling `perf/` directory pointing to
`../baseline-tui` or `../minicore-tui`; the archived manifest is not an instruction
to build locally. These are TestBackend wall times, not process CPU. Most rows
use one warmup and a median of five samples (scrollbar-only uses twenty).

For 200 messages / 10,001 prepared rows: Tick 2.7101→2.4176 ms, wheel
3.0157→2.6420 ms and drag 2.5594→2.6265 ms. Live text 64 KiB without history
was 10.5197→9.7931 ms; ~1 KiB with long history was 22.8044→22.5529 ms.
Not all results improved: the 10-message prepare and cached-render samples got
slower. Raw CSVs retain that variation. No new live-Markdown performance claim
is made; its extra parsing/geometry work was deliberately left out of this fix.

## Cleanup And Installation

Before building, only five explicit prior Cargo target roots were cleaned.
56,391 regenerable files were removed; all 688 retained executable/library hashes
passed verification. Remote free space grew from about 85 GB to 102 GB before
new builds. An unrelated Cargo process/directory was excluded. See `cleanup/`.

Accepted TUI binaries were installed by staged rename at the original Debug and
Release paths, preserving old executable inodes and the old Debug dSYM in
`target/preserved-before-stream-EmJatp/`. `installed-builds.txt` records hashes.
The Agent binary was not replaced, and no user process was restarted. Existing
running TUI processes retain their old image until restarted by the user.

The version string remains 0.2.8; use the recorded binary hashes to distinguish
these fixes. Initial verification and installation preceded the source commit;
the user subsequently requested separate commits for completed features. No push,
tag or new GitHub CI run accompanied this verification: the checks above were
remote-builder/native acceptance.
