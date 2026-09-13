# Pi Fullscreen Scrollbar Verification

Date: 2026-09-13. TUI remains 0.2.8; Agent remains 0.3.3. This is a TUI-only
change, independent of the paused Agent compaction draft and incident cleanup.
The exact tested source is bound by `source-sha256.json`; implementation and
installation commits are recorded in `../../verification.md`.

## Reference and Scope

Pi's latest published release at verification was **0.85.1**, revision
`d981de1229ef899957bbe968bc8dcda02a21f477` (published 2026-09-05).
Official release: <https://github.com/earendil-works/pi/releases/tag/v0.85.1>.
The installed package's layout, ScrollView and fullscreen TUI JavaScript were
byte-compared with the official npm distribution. `tools/scrollbar_oracle.mjs`
executes those implementations; it does not reimplement the reference formulas.
The generated fixture records their hashes and both built-in theme hashes.

Coverage: **144 geometry cases, 1152 pointer mappings, 18 visibility/scroll trace
steps and 12 actual Pi renderings** (dark/light × active/inactive × top/mid/bottom).
Repeated oracle generation was byte-identical. The three historical 0.84.4
scrollbar fixtures remain unchanged, but their current Rust comparison is
superseded by the new versioned renderings. Other Rail fixtures retain their
original reference. Thirteen UI snapshots changed for auto visibility and the
non-reserving marker overlay; unrelated expectations were not removed.

The default auto bar overlays one column: `│` track, `┃` thumb, `█` active thumb,
1000 ms hide delay, hover/drag activity, Pi round/clamp geometry, centered track
click and immediate Session offsets. Release does not remap. Wheel is one row,
Alt-wheel five, and page scrolling overlaps four rows. Resize/content growth and
wheel/keyboard input retain capture; focus/session/modal transitions and fitting
content end it. The current prepared geometry is reused; when absent, the last
measurement remains valid like Pi's last published `currentLayout`.

The bottom hint overlays its own centered rectangle and keeps MiniCore's existing
status labels. This is scrollbar parity, not an entire theme/keybinding rewrite.
Wide-character overlay edges, background preservation, selection clipping and
text/fold/link hit arbitration have dedicated regressions.

## Gates

All Rust execution, including formatting, occurred on `root@192.168.20.199` under
`/root/minicore-scrollbar.1ulnCV`. Source transfers used an explicit allowlist or
Git archives. No private config/Store was transferred. No local Rust fallback.

| Gate | Result |
|---|---|
| Stable all targets | 525 passed / 20 ignored |
| Rust 1.85 all targets | 525 passed / 20 ignored |
| Stable fmt, strict Clippy, warning-denied rustdoc, Linux build | PASS |
| Real Agent E2E, stable / MSRV | 18 passed each |
| macOS Debug / Release, LLVM 19 + Rust 1.98 | PASS |
| Mach-O layout, minos 11.0, strict signature checks | PASS |
| Debug binary / dSYM UUID | `4C4C445A-5555-3144-A105-9C2CE9C62379` |
| Independent read-only source review | APPROVE |

The 20 default ignores are 18 separately executed Agent E2E cases, one manual
benchmark and the existing real-PTY-only test. Agent E2E uses a fresh Linux build
of accepted Agent `f1697f7`; native runs use its unchanged installed Debug binary.
No new Agent full-suite acceptance is claimed.

`logs/render-red.log`, `review-red.log` and `reference-tests-2.log` preserve real
track, completion/interaction and wide-background failures. Compile/setup errors
and outdated fixture assumptions remain separately recorded and are not product
REDs. Existing selection timing assertions remain; their setup now supplies
sufficient actual content rather than inconsistent synthetic viewport counts.

## Matched Performance

Same remote host, stable optimized test profile (package opt-level 1, dependencies
2), same benchmark, 200 history messages and 5000 wheel-down events at the tail.
Baseline is `6ecd736` plus only the identical benchmark instrumentation. Separate
executables were preserved and three baseline/candidate runs alternated.

| Metric | Baseline | Candidate |
|---|---:|---:|
| Repainted frames | 5000 | 0 |
| Median event workload | 1245.082 ms | 7.141 ms |
| Markdown parses | 0 | 0 |

This measures avoided work for **unchanged boundary input**. It is not an overall
FPS, CPU, streaming-throughput or pixel-parity claim. No new full-history cache
architecture was added; live composition can still copy history rows.

## Native and Artifacts

Both candidates passed real iTerm2 workflows in fresh synthetic workspaces,
using a loopback provider and exactly one model request each. Captures verify
auto hide, hover/active glyphs, full-height track, 1/5-row wheel movement, page
step, live track/drag updates, release-coordinate immunity and resize during
capture. Exact `stty -g` state was restored on normal exit. Input was injected
as terminal SGR/key sequences; universal pixel parity and hardware-pointer races
are not claimed. Existing unrelated iTerm windows and user processes were not
closed or restarted. No new panic-exit or real-provider run is claimed.

| Artifact | SHA-256 |
|---|---|
| TUI Debug | `f1a9eb3516e5ed1f9632c1ef4b1fe1cc622aa8545878a30141cc7e9b61e1917e` |
| TUI Release | `c4d97f442290e5c3c58496ce46719d3138cfbabb17e6ad87a3d53364bacbf019` |

Raw logs, native text captures, TTY metadata and exact execution scripts are
retained here. Binaries/dSYM and complete local evidence are under
`/tmp/minicore-scrollbar.jB2v84`; remote artifacts and benchmark binaries remain
under `/root/minicore-scrollbar.1ulnCV`. Historical verification packages,
rejected artifacts, Runtime work and the paused compaction evidence are retained.
