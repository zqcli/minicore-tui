# Testing

The repository's default test suite is offline. It uses desensitized JSON
fixtures, a non-installable fake Agent test target, deterministic App update
flows, Ratatui `TestBackend` snapshots, and terminal lifecycle checks. It does
not call a real provider, require an installed Agent, read a user config, or
enter an alternate screen during normal CI tests.

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

The delivery verification used Rust `1.85.0` on an isolated Linux builder.
The command time limits used for that verification were 120 seconds for fmt,
metadata, and dependency-tree checks, and 900 seconds for test, clippy, and
doc checks. The command itself is more important than the builder location;
credentials and private workspace paths are never recorded in test artifacts.

## Reproducible macOS Artifact

From the repository root, `scripts/build-macos-x86_64.sh` checks for
`cargo-zigbuild` and `zig`, fixes `MACOSX_DEPLOYMENT_TARGET=11.0`, clears
external Rust flag/target-directory overrides, and runs the locked Rust 1.85
Darwin release build. The target-specific `.cargo/config.toml` reserves
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
- `tests/rpc_io.rs` exercises the public `RpcProcess` boundary.
- `tests/agent_process.rs` drives the production RPC process against a fake
  Agent for serve, ordering, events-before-response, crash, hang, oversized
  request, and full-contract cases.
- `tests/app_flow.rs` covers bootstrap, session creation/opening, pagination,
  multi-turn reconciliation, event gaps, cancellation, background sessions,
  shutdown, and fatal state.
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
Agent data directory/workspace. `tests/agent_e2e.rs` rejects non-loopback
URLs, hardcoded credential fields, userinfo, unsupported schemes, and paths
outside the E2E-owned temporary root. The run covers ping/catalog discovery,
session creation, two turns with durable reconciliation, transcript authority,
shutdown response, and child exit. No provider key or real user data is used.

The delivery run wraps this command in a 300-second timeout and a cleanup
trap. The trap kills/reaps only processes created by the run and removes its
temporary root.

## Windows Cross-Check

The Linux builder performs the portable cross-target compile checks:

```bash
cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings
cargo test --locked --target x86_64-pc-windows-gnu --all-targets --no-run
```

The Windows GNU toolchain is a compile/clippy cross-check, not a claim that a
Windows kernel executed the suite. Windows-specific code paths are kept under
`cfg(windows)` and must remain warning-free.

## CI Workflow

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) defines:

- Ubuntu quality: fmt, clippy with denied warnings, rustdoc, and one
  Crossterm version;
- locked tests on Ubuntu, macOS, and Windows;
- locked tests with Rust 1.85.0 on Ubuntu.

The workflow is configured but was not executed as part of this delivery;
there is no claim of a GitHub Actions run. See
[verification.md](verification.md) for the exact delivery evidence.

## Secret Hygiene

Do not put API keys, bearer tokens, provider credentials, raw Agent frames,
user messages, reasoning, tool arguments, tool output, or real workspace paths
in fixtures, snapshots, debug logs, E2E configs, or documentation. The debug
log records only request metadata. The E2E safety checker exists to enforce
this boundary for its temporary config.
