# Testing

The repository's default test suite is offline. It uses desensitized JSON
fixtures, a non-installable fake Agent test target, deterministic App update
flows, Ratatui `TestBackend` snapshots, and terminal lifecycle checks. It does
not call a real provider, require an installed Agent, read a user config, or
enter an alternate screen during normal CI tests.

## Current 0.3.0 Verification Boundary

The current package is TUI **0.3.0**, paired with Agent **0.5.0** and Runtime
**0.4.1**. The current code/test/snapshot baseline is
`0aa64c5e4d9211351123db059547beddb15c2cce`; fixed backend revisions are
recorded in [`docs/backend.md`](backend.md). Phase-F review and the final
Tool Detail/history/export fixes pass the focused current local suite.

The default local commands remain offline after dependencies have been fetched:

```bash
cargo fmt --all -- --check
cargo test --locked --offline --all-targets
cargo clippy --locked --offline --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --offline --no-deps
```

The current local rustc 1.98.0 result is **828 passed, 0 failed, 43 ignored** across the
all-targets suite; `tests/app_flow.rs` is **137/137**, and the ignored release
performance suite is **6/6**. These checks were run against the current local
source baseline. The authorized remote Rust 1.85/stable checks, fixed
Agent/Runtime build, final-source serial Agent E2E, hosted CI, and native/manual
terminal checks remain separate acceptance evidence and are not claimed here.

## Historical Patch Verification

The historical same-version **TUI 0.2.8 / Agent 0.3.3** source pair is TUI `a604e55`
/ Agent `f1697f7`; see the [public reload correction](verification/reload-refresh/README.md).
Fresh committed source archives passed **508 TUI tests / 19 ignored** on stable
and Rust 1.85, plus all **18 real-Agent E2E tests** separately on both toolchains.
Stable fmt, strict Clippy and warning-denied rustdoc passed. The unchanged Agent
retains its **369 passed / 2 ignored** exact-source acceptance from the earlier
[Tool/reload/subagent follow-up](verification/followups/README.md); its Linux binary
was freshly built for the corrective E2E runs and its accepted macOS binaries reused.
macOS Debug/Release reran all six native feature, Session and streaming workflows,
plus real-TTY normal/panic restoration. Corrected TUI binaries/symbols are installed;
Agent installation bytes are unchanged. Accepted Rust work and delivery artifacts
came from the authorized Linux builder. The earlier local-Rust violation and
excluded results remain disclosed in the previous report; its 498-test TUI record
and hashes are historical, not relabeled. No new Windows/hosted-CI or pixel-parity
result is implied.

The original paired release remains documented in
[verification/0.2.8/README.md](verification/0.2.8/README.md). The previous
[Session-management acceptance](verification/session-management/README.md) reports
464 TUI tests / 18 ignored, 318 Agent tests / 2 ignored, and 17 E2E scenarios.
Its source/artifact hashes remain historical. Earlier panel stages are retained in
[Stage 3](verification/stage3-session-panel/README.md) and
[Stage 4](verification/stage4-session-panel/README.md). Existing MSRV strict-Clippy
diagnostics are not claimed as fixed or suppressed.

For the historical preceding **TUI 0.2.7 / Agent 0.3.2**, see
[verification/0.2.7/README.md](verification/0.2.7/README.md). All compilation was
performed remotely on Linux: stable and Rust 1.85 each passed 423 default tests
with 17 ignored; 16 real-Agent loopback E2E tests passed separately. Native macOS
ran the cross-built executables, including 10 writer tests, isolated-config
regression, real-TTY normal/panic restoration, and iTerm2 interaction checks.
Debug uses package level 1/dependency level 2, with debug info and assertions;
optimized stepping/local-variable tradeoffs are intentional.
The fixed-scroll experiment uses an actual 220×53 terminal, absolute 25 Hz
injection and recorded event timestamps. Each of three 8-second process-CPU
windows excludes profiling. The supplemental full 800-event CPU span includes
subsequent sampling and drain overhead. Neither is a streaming CPU ceiling.
No native pixel screenshot or new Windows result is claimed.
The older platform gates below remain historical.

## Default Linux Commands

Run these commands from the repository root:

```bash
cargo fmt --all -- --check
cargo test --locked --offline --all-targets
cargo clippy --locked --offline --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --offline --no-deps
cargo tree --locked --offline -p crossterm
cargo metadata --locked --offline --no-deps --format-version 1
```

The historical E3 delivery verification used Rust `1.85.0` and stable on an
isolated remote Linux builder; see [verification/v03-e3/README.md](verification/v03-e3/README.md)
for those recorded commands and results. GitHub Actions Linux, macOS, and
Windows jobs were not run for that record. Suggested command time limits are
120 seconds for fmt, metadata, and dependency-tree checks, and 900 seconds for
test, clippy, and doc checks.
Credentials and private workspace paths are never recorded in test artifacts.

## Reproducible macOS Artifact

From the repository root, put LLVM's `clang` and `ld64.lld` on `PATH` and set
`SDKROOT` to a macOS SDK. `scripts/build-macos-x86_64.sh` checks those prerequisites,
fixes `MACOSX_DEPLOYMENT_TARGET=11.0`, clears external Rust flag/target-directory
overrides, and runs the locked Rust 1.85 Darwin release build with ad-hoc signing.
The 0.2.7 delivery binaries use Rust 1.98/LLVM 19 to match the comparison compiler;
the canonical Rust 1.85 build was separately exercised. Zig 0.13-linked probes
crashed during native exception unwinding; LLVM-linked probes passed. The precise
internal cause of the Zig-path crash was not established. The target-specific `.cargo/config.toml` reserves
`0x4000` bytes of Mach-O header padding without affecting Linux or Windows
targets; the script checks and prints the relative and absolute artifact path.
On macOS, `scripts/verify-macos-binary.sh [binary]` locates the
`__TEXT,__text` section, checks load-command bounds and `minos 11.0`, runs
`file`, `nm`, `size`, and `dwarfdump --uuid`, and fail-closes on malformed
`LC_CODE_SIGNATURE` ranges before strict codesign verification. Its
`scripts/verify-macos-binary.sh --self-test` mode runs the same metadata parser
against portable fixtures, so it also runs on Linux without macOS tools.

## Counting Targets And Tests

`cargo metadata --no-deps --format-version 1` is the source of truth for Cargo
targets. The `agent_process` target has `harness = false`, so it is an executable
fake-Agent harness and intentionally has no libtest `test result` line. For the
other targets, count the `passed`, `failed`, and `ignored` fields from each
`test result: ok` line in the unabridged `cargo test` output. The current local
all-target run totals **828 passed, 0 failed, 43 ignored**. Do not count compile
messages or the harness-free executable as tests.

## Snapshots

Committed text snapshots live in [`../snapshots/`](../snapshots/). They are
captured through the production `ui::render` path using Ratatui
`TestBackend`, with dark/light themes, 60×16, 80×24, and 120×40 layouts,
selectors, new-session forms, tools, reasoning, scrolling, CJK, help/logs,
and small-terminal scenes.

`src/ui/snapshots.rs` compares the 27 committed snapshot files; it can update
them only when `MCT_UPDATE_SNAPSHOTS=1` is explicitly set. The integration
target `tests/render_snapshots.rs` independently compares representative
80×24 scenes. Snapshot drift is therefore covered by the default all-targets
test command. This repository does not depend on `insta`; the committed text
comparison is deterministic and works without a review tool.

## RPC And App Tests

- `tests/protocol.rs` parses every fixture through production protocol code.
- The unit tests in `src/rpc.rs` exercise the public `RpcProcess` boundary.
- `tests/agent_process.rs` drives the production RPC process against a fake
  Agent for serve, ordering, events-before-response, crash, hang, oversized
  request, and full-contract cases.
- `tests/app_flow.rs` covers bootstrap, session creation/opening, pagination,
  multi-request reconciliation, persistence failure and duplicate wait,
  generation-staged configuration reload, shutdown drain, internal exact-turn
  refresh, and active-session updates. The reducer tests in `src/app.rs` also
  pin reload admission: no queue advancement or new `turn.send`/`turn.steer`
  at reload begin/mid/end/failure, composer and admitted-queue retention,
  stale-read gap preservation, state-read failure fencing, lifecycle admission,
  recovery after late lifecycle/create ACKs, and channel-end fencing during an
  incomplete reload. Reload-created Steer fences require a matching fresh
  Running state response; an ordinary History gap alone does not create that
  fence. Coverage includes late TurnRef binding, Idle notifications during state
  recovery, dropped close-verification reads, and independent History/state
  reconciliation. The terminal FIFO regression verifies that a completed,
  persisted, history-settled Idle loop can hand off queued input exactly once
  without sending another Steer to the old loop.
- `src/ui/transcript.rs` tests durable cache preparation/install, revision and
  key invalidation, stale preparation rejection, session-local caches, live
  delta isolation, and parse-count cache hits.
- `src/markdown.rs` tests style-run coalescing, style boundaries, Unicode,
  CJK, emoji, combining marks, Markdown blocks, and plain streaming wrapping.

## Terminal Tests

`tests/terminal_restore.rs` contains the normal offline tests and the ignored
real-PTY round trip. The panic-hook regression launches the same test binary
with `--exact child_test`, redirects the silent status run to null stdio, and
uses a ten-second parent timeout. A second invocation captures output to check
that no recursive destructor-panic diagnostic appears. The child status must
be non-success with ordinary panic exit code 101; on Unix it must not be
signal-terminated. The parent never installs a test-global panic hook.

Run the ignored PTY check only from an actual terminal:

```bash
cargo test --locked --test terminal_restore -- --ignored --nocapture
```

The PTY check is not part of the default offline suite because a non-TTY
cannot safely exercise terminal modes.

## Real-Agent E2E

The E2E tests are ignored by default:

```bash
MINICORE_AGENT_BIN=/path/to/minicore-agent \
cargo test --locked --offline --test agent_e2e -- --ignored --test-threads=1 --nocapture
```

The test harness creates an isolated configuration, data directory, workspace,
and loopback mock model endpoint. The current ignored target contains 34
scenarios covering discovery, turns, steering, updates, configuration reload,
shutdown, Tool/file/workspace/Changes/Context workflows, and editor/background
lifecycle behavior. No provider key or real user data is used.

A delivery run should wrap this command in a 300-second timeout and a cleanup
trap. The trap must kill/reap only processes created by the run and remove its
temporary root. The official serial command is `--ignored --test-threads=1`.
The historical E3 remote run passed 34/34 on both Linux toolchains; the final
source-baseline run is pending. This is loopback evidence against the real
Agent binary, not external-provider coverage.


## Stage 7 PTY Evidence

`scripts/stage7_xtermjs.py` drives the real TUI and Agent through a PTY with an
isolated loopback Responses server and temporary Git workspace. Actual PTY
bytes feed pinned xterm.js in headless Edge; Playwright screenshots the
terminal element after a renderer-flush barrier. It records raw bytes, input
sequences, terminal sizes, binary hashes, and exact final-sentence assertions.
No desktop screenshot or synthetic cell image is used for the final evidence.

```bash
cargo build --locked --offline
(cd ../minicore-agent && cargo build --locked --offline)
npm --prefix tools/stage7-xtermjs ci
python3 scripts/stage7_xtermjs.py \
  --tui-bin target/debug/minicore-tui \
  --agent-bin ../minicore-agent/target/debug/minicore-agent
```

The development-only driver requires its Python dependencies and the browser
channel specified in `tools/stage7-xtermjs/driver.mjs`; neither is required by
default Rust tests. The final archive is
`docs/verification/rail/capture-20260906T062517Z/`: four mandated 80×24
screenshots, two 120×40 scenes, and one 62×18 scene, plus raw PTY and compact
provenance. `artifacts/` is ignored regenerable scratch. The earlier iTerm/OCR
prototype `stage7_pty.py` remains available but is not the final capture path.
This evidence uses a mock provider with real Agent/tools, not an external LLM,
and does not substitute for hosted CI or exhaustive source-cell equivalence.

## Windows Cross-Check

The Linux builder performs these portable cross-target compile checks when
needed:

```bash
cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings
cargo test --locked --target x86_64-pc-windows-gnu --all-targets --no-run
```

The Windows GNU toolchain commands are compile/clippy cross-checks, not native
Windows execution or GitHub Actions CI evidence. Windows-specific code paths are
kept under `cfg(windows)` and must remain warning-free. On the frozen source
this pair passed on the Linux builder (cross-clippy `-D warnings` and all-target
`--no-run` link); the final logs are archived under `docs/verification/rail/final-r5/`
(`tui-windows-clippy.log`, `tui-windows-build.log`).

## CI Workflow

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) defines:

- Ubuntu stable quality: fmt, Clippy with denied warnings, rustdoc, and one
  Crossterm version;
- locked offline tests on Ubuntu, macOS, and Windows for Rust 1.85.0 and stable;
- a separate pinned Agent/Runtime source checkout and offline build;
- a serial ignored real-Agent E2E job using only the in-test loopback provider
  and no provider credentials.

The workflow first fetches locked dependencies, then uses `--offline` for the
actual checks. Hosted jobs have not been run for this release branch yet, so
this file is CI configuration rather than execution evidence. Remote Linux
records and local focused tests remain separate evidence.

## Secret Hygiene

Do not put API keys, bearer tokens, provider credentials, raw Agent frames,
user messages, reasoning, tool arguments, tool output, or real workspace paths
in fixtures, snapshots, debug logs, E2E configs, or documentation. The debug
log records only request metadata and is bounded to 200 lines / 4096 bytes per
line. The E2E safety checker exists to enforce
this boundary for its temporary config.
