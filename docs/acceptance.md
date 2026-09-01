# Acceptance Matrix

This matrix preserves every acceptance ID from MT-001 through MT-144. Each
row has one status and the evidence used for this delivery.

Status meanings:

- `PASS`: implementation and the cited repository/remote evidence support the
  criterion.
- `PARTIAL`: the criterion is implemented, but the requested execution matrix
  was not fully available.
- `NOT RUN`: no execution evidence is claimed.

The delivery evidence is Linux Rust 1.85 on an isolated remote builder,
portable Windows GNU clippy/no-run checks, committed snapshots, and the
loopback real-Agent E2E. GitHub Actions was not run. macOS tests were not run
because local Rust builds are prohibited and no macOS remote builder was
available.

## Architecture

| ID | Status | Evidence |
|---|---|---|
| MT-001 | PASS | `Cargo.toml` package name is `minicore-tui`; `README.md`. |
| MT-002 | PASS | `Cargo.toml` contains one package and one installable binary target; `cargo metadata` target evidence in `docs/verification.md`. |
| MT-003 | PASS | `Cargo.toml`, `src/lib.rs`; no `minicore-agent` Rust dependency. |
| MT-004 | PASS | `Cargo.toml`, `src/lib.rs`; no `minicore-runtime` Rust dependency. |
| MT-005 | PASS | `src/rpc.rs`, `src/protocol.rs`, `docs/rpc-contract.md`; stdio-only transport. |
| MT-006 | PASS | Dependency/source audit: no HTTP or WebSocket implementation. |
| MT-007 | PASS | `src/app.rs` contains frontend state only; Agent loop remains in the child. |
| MT-008 | PASS | `src/app.rs`, `src/ui/help.rs`; unsupported interactions become a notice, with no approval UI. |
| MT-009 | PASS | `src/command.rs`, `src/app.rs`; no steering or follow-up queue. |
| MT-010 | PASS | Source/dependency audit; no plugin registry or plugin transport. |

## Technical

| ID | Status | Evidence |
|---|---|---|
| MT-011 | PASS | Remote `cargo +1.85.0 test --locked --all-targets`, clippy, doc, and fmt gates. |
| MT-012 | PASS | `Cargo.toml` has `edition = "2024"`. |
| MT-013 | PASS | `Cargo.toml` sets `unsafe_code = "forbid"`; remote clippy passes. |
| MT-014 | PASS | `Cargo.toml` pins `ratatui = 0.29.0`; metadata confirms it. |
| MT-015 | PASS | `Cargo.toml` pins `crossterm = 0.28.1`; tree check confirms it. |
| MT-016 | PASS | Remote `cargo tree -p crossterm` reports one `crossterm v0.28.1`. |
| MT-017 | PASS | `Cargo.toml` pins `tui-textarea = 0.7.0`. |
| MT-018 | PARTIAL | Linux remote tests passed; Windows GNU clippy/no-run passed; macOS tests and GitHub Actions were not run. |

## Terminal

| ID | Status | Evidence |
|---|---|---|
| MT-019 | PASS | `src/terminal.rs` `EnterAlternateScreen`; ignored PTY path remains available. |
| MT-020 | PASS | `src/terminal.rs` raw-mode entry/rollback; terminal lifecycle tests and remote compilation pass. |
| MT-021 | PASS | `src/terminal.rs` enables/disables bracketed paste; ANSI sequence tests. |
| MT-022 | PASS | `src/main.rs` explicit restore on normal/error paths; `tests/terminal_restore.rs`. |
| MT-023 | PASS | `src/main.rs` handles RPC/child errors before terminal restore; `src/ui/error.rs`; fatal rendering tests. |
| MT-024 | PASS | Panic hook implementation and isolated child regression in `tests/terminal_restore.rs`; ordinary panic exit asserted. |
| MT-025 | PASS | TestBackend cursor tests `long_wrapped_line_cursor_is_visible_with_correct_hardware_position` and `cjk_cursor_hardware_position_uses_display_columns` in `src/ui/composer.rs`; shared display-width/wrap coverage in `src/markdown.rs`. |
| MT-026 | PASS | `src/main.rs` remeasures terminal geometry; snapshots cover multiple sizes and `AppEvent::Viewport` handles clamping. |
| MT-027 | PASS | `src/ui/layout.rs` small-terminal threshold and `small_50x10` snapshot/component test. |

## RPC

| ID | Status | Evidence |
|---|---|---|
| MT-028 | PASS | `src/rpc.rs` child spawn; `tests/rpc_io.rs`, fake Agent process tests. |
| MT-029 | PASS | `src/rpc.rs` has one stdin writer task. |
| MT-030 | PASS | `src/rpc.rs` has one stdout reader task. |
| MT-031 | PASS | `tests/agent_process.rs`, `tests/app_flow.rs` event/response interleaving. |
| MT-032 | PASS | Out-of-order fake Agent mode and app correlation tests. |
| MT-033 | PASS | `App.pending_requests` and `RequestKind`; protocol/app tests correlate by ID. |
| MT-034 | PASS | `src/rpc.rs` `MAX_RPC_FRAME_BYTES`; duplex boundary tests. |
| MT-035 | PASS | `src/rpc.rs` captures stderr as bounded log events; no direct terminal forwarding. |
| MT-036 | PASS | `src/app.rs` fatal state/exit status; `src/ui/error.rs` tests. |
| MT-037 | PASS | Shutdown state tests in `tests/app_flow.rs` and real-Agent E2E. |
| MT-038 | PASS | `RpcProcess::terminate`/kill fallback and hanging-child tests in `src/rpc.rs`. |

## Startup

| ID | Status | Evidence |
|---|---|---|
| MT-039 | PASS | Bootstrap request and response tests in `tests/app_flow.rs`/`tests/protocol.rs`. |
| MT-040 | PASS | Model fixture and catalog bootstrap tests. |
| MT-041 | PASS | Profile fixture and catalog bootstrap tests. |
| MT-042 | PASS | Session fixture and catalog bootstrap tests. |
| MT-043 | PASS | `src/args.rs`, `src/main.rs`, and `tests/rpc_io.rs` spawn/config error coverage. |
| MT-044 | PASS | `src/ui/header.rs`, `src/ui/help.rs`, and empty snapshots show `/new`/resume guidance. |

## Session

| ID | Status | Evidence |
|---|---|---|
| MT-045 | PASS | New-session workspace field in `src/state/selection.rs`, UI tests, and app-flow tests. |
| MT-046 | PASS | Profile selector/form tests in `src/app.rs`, `src/ui/component_tests.rs`. |
| MT-047 | PASS | Model selector/form tests and model fixtures. |
| MT-048 | PASS | Reasoning selector/form tests and reasoning fixtures. |
| MT-049 | PASS | Agent error routing keeps the draft; create/open error tests in `src/app.rs`/`tests/app_flow.rs`. |
| MT-050 | PASS | `SessionInfo` updates `SessionView.info`; app-flow/session selector tests. |
| MT-051 | PASS | Open response requests `session.state`; app-flow tests. |
| MT-052 | PASS | Incremental `session.transcript` chain and pagination tests. |
| MT-053 | PASS | Session selector/open flow and snapshot coverage. |
| MT-054 | PASS | Background session event test in `tests/app_flow.rs`. |
| MT-055 | PASS | Model selector changes only a new-session draft; selector/app tests. |
| MT-056 | PASS | README/keybindings and selector tests explicitly state no hot swap. |

## Turn

| ID | Status | Evidence |
|---|---|---|
| MT-057 | PASS | Composer submit path and app-flow turn tests. |
| MT-058 | PASS | `on_send_response` registers `WaitTurn` in the same update; explicit assertions in app tests. |
| MT-059 | PASS | Send failure restores composer text; `tests/app_flow.rs`. |
| MT-060 | PASS | `turn_started` before send response tests in `src/app.rs`/`tests/app_flow.rs`. |
| MT-061 | PASS | Text output-delta routing and live snapshot. |
| MT-062 | PASS | Reasoning output-delta routing and reasoning component tests. |
| MT-063 | PASS | Live tool-start routing and running-tool snapshot. |
| MT-064 | PASS | Live tool-progress routing and app tests. |
| MT-065 | PASS | Live tool-finished routing and component tests. |
| MT-066 | PASS | Exact `TurnRef` cancellation path and keymap/app tests. |
| MT-067 | PASS | Cancel response does not replace the required wait; cancellation tests. |
| MT-068 | PASS | Wait response starts state/transcript reconciliation; app-flow tests. |
| MT-069 | PASS | Two-turn durable reconciliation in `tests/app_flow.rs` and E2E. |
| MT-070 | PASS | Background two-session flow in `tests/app_flow.rs`. |

## Event Reliability

| ID | Status | Evidence |
|---|---|---|
| MT-071 | PASS | `docs/architecture.md`, `src/app.rs`; live events never fabricate durable blocks. |
| MT-072 | PASS | `mark_gap` and dropped-event tests. |
| MT-073 | PASS | Footer warning and `running_gap_80x24` snapshot/component test. |
| MT-074 | PASS | Reconcile chain clears the gap; app-flow tests. |
| MT-075 | PASS | Wait authority tests do not require `turn_finished`. |
| MT-076 | PASS | Interleaved output/wait handling in app-flow and fake Agent tests. |
| MT-077 | PASS | No ACK/replay implementation; source/dependency audit. |

## Pi Visuals

| ID | Status | Evidence |
|---|---|---|
| MT-078 | PASS | `src/ui/mod.rs`, fullscreen snapshots at 60×16/80×24/120×40. |
| MT-079 | PASS | `src/ui/user.rs`, exact user background component test. |
| MT-080 | PASS | `src/ui/assistant.rs`, no-background component test. |
| MT-081 | PASS | `src/ui/reasoning.rs`, gray italic component test. |
| MT-082 | PASS | Toggle reasoning tests and hidden-reasoning snapshot. |
| MT-083 | PASS | Tool pending background test. |
| MT-084 | PASS | Tool success background test. |
| MT-085 | PASS | Tool error/denied background test. |
| MT-086 | PASS | Composer reasoning-color component test. |
| MT-087 | PASS | `src/ui/status.rs`, spinner and live snapshot tests. |
| MT-088 | PASS | Footer renderer and wide-layout snapshots. |
| MT-089 | PASS | Selector accent/selected-background component test. |
| MT-090 | PASS | `src/theme.rs` dark palette unit test. |
| MT-091 | PASS | `src/theme.rs` light palette unit test and light snapshots. |
| MT-092 | PASS | `src/ui/header.rs`, README; no Pi logo/brand asset. |

## Composer

| ID | Status | Evidence |
|---|---|---|
| MT-093 | PASS | `tui-textarea` composer and multiline tests. |
| MT-094 | PASS | Enter keymap and submit tests. |
| MT-095 | PASS | Ctrl+J keymap/composer tests. |
| MT-096 | PASS | Shift+Enter keymap test. |
| MT-097 | PASS | Crossterm paste handling and multiline composer snapshot. |
| MT-098 | PASS | Unicode width/CJK tests and CJK snapshot. |
| MT-099 | PASS | Emoji width/cursor tests. |
| MT-100 | PASS | Bounded in-process history tests. |
| MT-101 | PASS | Running composer is frozen; keymap/app tests. |
| MT-102 | PASS | Ctrl+C clear/double-press lifecycle tests. |

## Selection And Commands

| ID | Status | Evidence |
|---|---|---|
| MT-103 | PASS | Session filter/sort tests and selector snapshots. |
| MT-104 | PASS | Model filter tests and search snapshot. |
| MT-105 | PASS | Supported reasoning filter and selector tests. |
| MT-106 | PASS | Dock selector renderer/component tests. |
| MT-107 | PASS | `/new` parser/app tests. |
| MT-108 | PASS | `/resume` parser/app tests. |
| MT-109 | PASS | `/model` parser/app tests. |
| MT-110 | PASS | `/reasoning` parser/app tests. |
| MT-111 | PASS | `/theme dark|light` parser/theme tests. |
| MT-112 | PASS | `/clear` local reload tests. |
| MT-113 | PASS | `/help` parser/render tests. |
| MT-114 | PASS | `/logs` parser/log-panel tests. |
| MT-115 | PASS | `/quit` parser/shutdown tests. |
| MT-116 | PASS | Unknown command tests assert no RPC command. |

## Scrolling And Performance

| ID | Status | Evidence |
|---|---|---|
| MT-117 | PASS | Scroll state and follow-tail snapshots/app tests. |
| MT-118 | PASS | Upward scroll disables follow; app-flow/UI tests. |
| MT-119 | PASS | New-output marker snapshot/component test. |
| MT-120 | PASS | End restores follow; keymap/app tests. |
| MT-121 | PASS | Mouse-wheel mapping and scroll tests. |
| MT-122 | PASS | `src/main.rs` pure `render_deadline` test; `RENDER_INTERVAL = 33ms`. |
| MT-123 | PASS | Idle `render_deadline == None`, dirty/Rendered tests, and main-loop guard. |
| MT-124 | PASS | Durable cache parse-count test: one prepare, repeated measure/render cache hit. |
| MT-125 | PASS | Live text/reasoning use `wrap_plain`; cache tests prove live delta leaves durable revision unchanged. |
| MT-126 | PASS | Tool preview 40-line/32KiB bounds tests. |

## Security

| ID | Status | Evidence |
|---|---|---|
| MT-127 | PASS | Source/dependency audit; TUI has no shell execution path. |
| MT-128 | PASS | `src/rpc.rs` owns Agent stdout reader; only parsed frames reach App. |
| MT-129 | PASS | 4096-byte line cap and 200-line log ring tests. |
| MT-130 | PASS | `src/main.rs` debug metadata-only logger, E2E config checks, and source audit. |
| MT-131 | PASS | Help component test contains “Tools run automatically.” |
| MT-132 | PASS | Help component test contains “Bash is not sandboxed.” |
| MT-133 | PASS | Fatal overlay component test and `src/ui/error.rs`; no raw frame rendering. |

## Tests

| ID | Status | Evidence |
|---|---|---|
| MT-134 | PASS | Remote full `cargo test --locked --all-targets`; RPC duplex tests in `src/rpc.rs`. |
| MT-135 | PASS | Remote protocol fixture tests in `tests/protocol.rs`. |
| MT-136 | PASS | Remote `tests/app_flow.rs` result. |
| MT-137 | PASS | `small_50x10` and 60×16 selector snapshot. |
| MT-138 | PASS | 80×24 snapshot inventory and remote snapshot tests. |
| MT-139 | PASS | 120×40 snapshot inventory and remote snapshot tests. |
| MT-140 | PASS | Dark snapshot inventory and remote snapshot tests. |
| MT-141 | PASS | Light snapshot inventory and remote snapshot tests. |
| MT-142 | PASS | `cjk_80x24` snapshot and Unicode tests. |
| MT-143 | PASS | `tests/agent_e2e.rs` is ignored by default; loopback E2E passed remotely. |
| MT-144 | PASS | Default all-targets run passed without real provider/network; E2E remains ignored. |

## Non-PASS Items

Only `MT-018` is not a full PASS: the Linux remote and portable Windows GNU
checks are green, but macOS execution and GitHub Actions execution were not
available for this delivery. No claim is made that the CI workflow ran. The
ignored real-PTY test is also not included in the default count; its status is
recorded as a known runtime limitation rather than an invented platform pass.
