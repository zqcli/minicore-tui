# 0.2.6 Verification

Scope: dependency-only Debug optimization. Product Rust source and Agent/Runtime
are unchanged. Version and 11 startup snapshot labels become 0.2.6. No commit or
tag was created. All provider requests and stored sessions in these tests are
synthetic, with fresh absolute temporary workspaces/stores, loopback endpoints
and dummy credentials. No user Store/configuration was used or changed.

## Correctness And Build Gates

| Gate | Result | Evidence |
|---|---|---|
| Stable all targets | 412 passed, 17 ignored | `stable.log` |
| Rust 1.85 all targets | 412 passed, 17 ignored | `msrv.log` |
| Real-Agent loopback E2E | 16 passed | `e2e.log` |
| Real TTY | 1 passed; all stdio handles TTY; stty unchanged | `iterm/native-pty.log` |
| Formatting | PASS | `fmt.log` |
| All-target Clippy, warnings denied | PASS | `clippy.log` |
| Rustdoc, warnings denied | PASS | `doc.log` |
| Debug / Release builds | PASS; both report 0.2.6 | `build-debug.log`, `build-release.log`, `builds.txt` |
| Product source preservation | 59/59 hashes unchanged | `product-source-before.txt`, `source-check.log` |

The 17 default ignored tests are the 16 E2E cases and the real-TTY case, run
separately above. The fake-Agent harness without libtest output is not counted.
`compiler-profiles.txt` extracts real compiler flags from the verbose test build:
TUI level 0 and debuginfo 2; Ratatui/Unicode level 2, debuginfo 2 and assertions on.
Gemini 3.8 Flash high performed a read-only scope/evidence review with no blocker
reported; the parent independently ran the measurements and checks. Older
cross-platform and pixel-verification gaps are not relabeled as new evidence.

## Red-Capable Component Probe

`perf/src/main.rs` uses synthetic state and actual `App::update`, conversation
preparation, `ui::render` and Ratatui `Terminal<TestBackend>::draw`. No Agent or
filesystem-backed user state is involved. Viewport is 240×80 with dense Chinese,
combining accents, skin-tone/ZWJ Emoji, flags and Markdown.

Busy test: 200 historical messages, 10,016 prepared rows, a warmup batch of 50
frames, then seven timed batches of 50. Report is median batch time / 50.
The 3 ms/frame threshold is a local diagnostic target, not a CI timing assertion.

| Configuration | Busy Frame Wall Time | Local Gate |
|---|---:|---|
| Default Debug | 12.4824 ms | expected failure, exit 101 |
| Dependency-optimized Debug | 1.1832 ms | PASS |

`busy-debug.log` preserves the intentionally red assertion; it is not a TUI
runtime panic. `busy-deps.log` preserves green. Full `debug.csv`/`deps.csv`
include 10/50/200-message controls. Cached paint/tick stays approximately flat
with history length; live composition still grows with history.

The `deps` profile optimizes non-workspace packages but explicitly overrides
`minicore-tui` to level 0, because the TUI is a dependency of this probe. Without
that override the comparison would unintentionally optimize the application.
Initial measurements used the same pre-version-bump 0.2.5 source in both groups;
archive lockfile now references 0.2.6 for reruns. The 59 product/test Rust files
are byte-for-byte unchanged. Profile settings come from the probe root.

```sh
CARGO_TARGET_DIR=/tmp/minicore-026-probe-rerun cargo run --locked --offline \
  --manifest-path docs/verification/0.2.6/perf/Cargo.toml \
  -- --busy-frames --assert-budget
CARGO_TARGET_DIR=/tmp/minicore-026-probe-rerun cargo run --locked --offline \
  --manifest-path docs/verification/0.2.6/perf/Cargo.toml --profile deps \
  -- --busy-frames --assert-budget
```

The printed 10 FPS CPU estimate is an extrapolation from wall time; it is not
measured process CPU. Native measurements below are separate and authoritative
for that terminal workload. Component tests exclude terminal I/O/font rendering.

## Native iTerm2

`iterm/driver.py` creates an owned iTerm2 window, a fresh temporary store/workspace,
a loopback server and a real Agent process. It identifies only its own TUI through
the uniquely configured Agent child's parent PID. The user processes are not
sampled, stopped or restarted. Native pixel screenshots remain **UNVERIFIED**.

All three runs passed: Thinking boundaries, FIFO/Steer receipts, Option+Up,
60×16 queue visibility, immediate Footer max, long-history hover/wheel/drag,
input during a held response and normal shell restoration. Current-screen text
excludes resize scrollback. Requested 220×55 was actually **220×53** in every run.

After an 8-second idle control, the provider holds one request with no output.
The Working display continues changing. Each run measures three 8-second windows
using process `ps time`, then separately samples only that owned TUI for 3 seconds.
No compilation or component benchmarks ran during CPU windows.

| Run | Idle CPU Increment / ~8s | Busy CPU % Samples | Median |
|---|---:|---|---:|
| `iterm/before-debug` — 0.2.5 | 0.01s | 7.370, 7.369, 7.370 | 7.370% |
| `iterm/debug` — 0.2.6 | below 0.01s | 1.250, 1.373, 1.249 | 1.250% |
| `iterm/release` — 0.2.6 | 0.01s | 2.125, 1.000, 0.875 | 1.000% |

Each directory contains `result.json`, `busy-sample.txt`, current-screen text and
owned-process metadata. Keep the Release first-window outlier; the table does not
substitute a lower run. `ps time` resolution is 0.01s, not zero CPU. Agent busy
increments were below that resolution in all nine windows. These percentages
refer to one logical core, not the entire machine. This is one run per binary,
with repeated windows, not a population-level performance guarantee.

The preserved old Debug binary is `/tmp/minicore-026.cJPXXO/before-debug-minicore-tui`,
SHA-256 `3f44892105bf913fe9821872ef70dc76ed74fc08c787a519a49b628ba67a5b49`.
Current binary hashes are in `builds.txt` and matched each run's frozen hashes.
All owned native windows/processes exited normally. User processes 15561/15562
remained running and were observed at 0% CPU at the end.

## Remaining Costs

Ratatui still measures and paints visible graphemes each frame. No viewport Cell
cache was added; double buffering reduces output but does not eliminate raster
work. The render budget remains 30 FPS and the busy tick remains 100 ms. Held
response measurements must not be presented as a streaming CPU cap.

Large live snapshots still copy historical rows: the 10,042-row, 1 KB live-text
update probe improves from 44.7607 to 34.5923 ms, leaving substantial Debug
composition cost. Streaming Assistant Markdown is still plain. Dependency
optimization preserves debug information/assertions but may affect stepping and
local-variable visibility inside dependencies; the initial dependency rebuild
also costs more. Release profile and application execution semantics are unchanged.
