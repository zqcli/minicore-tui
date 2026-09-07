# Testing

The repository's default test suite is offline. It uses desensitized JSON
fixtures, a non-installable fake Agent test target, deterministic App update
flows, Ratatui `TestBackend` snapshots, and terminal lifecycle checks. It does
not call a real provider, require an installed Agent, read a user config, or
enter an alternate screen during normal CI tests.

## Current Patch Verification

The paired **TUI 0.2.8 / Agent 0.3.3** release is documented in
[verification/0.2.8/README.md](verification/0.2.8/README.md). Its source behavior
matches the preceding local builds; version-bearing snapshots were updated.
No local compilation is used for this release.

For the preceding **TUI 0.2.7 / Agent 0.3.2**, see
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
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo tree -p crossterm
cargo metadata --locked --no-deps --format-version 1
```

Final6 delivery verification used Rust `1.85.0` and stable on an isolated
remote Linux builder; see [verification.md](verification.md) for the recorded
commands and results. GitHub Actions Linux, macOS, and Windows jobs were not
run in final6.
Suggested command time limits are 120 seconds for fmt, metadata, and
dependency-tree checks, and 900 seconds for test, clippy, and doc checks.
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
targets. The `agent_process` target has `harness = false`, so it is an
executable fake-Agent harness and intentionally has no libtest `test result`
line. For the other targets, count the `passed`, `failed`, and `ignored`
fields from each `test result: ok` line in the unabridged `cargo test` output.
Do not count compile messages or the harness-free executable as tests.

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
  shutdown drain, and active-session updates.
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

The E2E test is ignored by default:

```bash
MINICORE_AGENT_BIN=/path/to/minicore-agent \
MINICORE_AGENT_CONFIG=/path/to/loopback-agent.toml \
cargo test --locked --test agent_e2e -- --ignored --nocapture
```

The configuration must use a loopback mock model endpoint and an isolated
Agent data directory/workspace. The ignored smoke flow covers ping/catalog
discovery, session creation, one turn with durable reconciliation, and
shutdown/child cleanup. The caller is responsible for supplying the isolated,
desensitized configuration; no provider key or real user data is used.

A delivery run should wrap this command in a 300-second timeout and a cleanup
trap. The trap must kill/reap only processes created by the run and remove its
temporary root. The official serial command is `--ignored --test-threads=1`
(eleven scenarios, deterministic ~1 s; the final-source r5 archive shows
0.52 s on the Linux builder). This is loopback evidence against the real Agent
binary, not external-provider coverage.


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

- Ubuntu quality: fmt, clippy with denied warnings, rustdoc, and one
  Crossterm version;
- locked tests on Ubuntu, macOS, and Windows;
- locked tests with Rust 1.85.0 on Ubuntu.

GitHub Actions Linux, macOS, and Windows jobs were not run in final6. Remote
Linux tests and cross-target checks are separate evidence, not a substitute for
those platform jobs. See [verification.md](verification.md) for the exact
delivery evidence.

## Secret Hygiene

Do not put API keys, bearer tokens, provider credentials, raw Agent frames,
user messages, reasoning, tool arguments, tool output, or real workspace paths
in fixtures, snapshots, debug logs, E2E configs, or documentation. The debug
log records only request metadata. The E2E safety checker exists to enforce
this boundary for its temporary config.
