# Delivery Verification

## Current Delivery: 0.2.1

The current TUI package is **0.2.1**, with the same Agent `0.3.x` protocol and
unchanged backend pins. Its reasoning Markdown/order fixes, remote verification
(**277 passed, 0 failed, 8 ignored** on both MSRV and stable), and macOS artifact
evidence are recorded in [release-0.2.1.md](release-0.2.1.md).

## Migration Baseline: 0.2.0

The remainder records the historical final6 verification of the r2 migration. The
implementer did not run local Cargo/Rust commands; all Rust compilation,
formatting, linting, documentation, tests, and PTY validation were executed on
the remote builder `192.168.20.199` in
`/root/minicore-tui-r2-01a06ec1/tui`.

## Source & Binary Pins

- TUI source baseline: `3ecb509e353a666711b11bfdee7f50bdc92fe674`
  (worktree intentionally uncommitted).
- Agent source: `b2e23938d073ab21c2775faa623561ba929a5ed1`, built as
  `/root/minicore-tui-r2-01a06ec1/agent/target/debug/minicore-agent`.
- Runtime source: `87f3cf92b9b5980b0f468174a319cf53427d858e`, pinned by the
  Agent dependency lock.
- Migration baseline package: `0.2.0`; supported wire range: Agent `0.3.x` only.

## Final6 & Post-review Results

| Check | Evidence | Result |
|---|---|---|
| MSRV all-target suite | `cargo +1.85.0 test --locked --all-targets` | **273 passed, 0 failed, 8 ignored** |
| Stable all-target suite | `cargo +stable test --locked --all-targets` | **273 passed, 0 failed, 8 ignored** |
| Release version gate | `cargo +stable test --release --lib version_gate` | **2 passed, 0 failed** |
| Stable/MSRV rustfmt | `cargo +stable fmt --all -- --check`; `cargo +1.85.0 fmt --all -- --check` | **passed** |
| Stable clippy | `cargo +stable clippy --locked --all-targets -- -D warnings` | **passed, no warnings** |
| Rustdoc | `RUSTDOCFLAGS="-D warnings" cargo +stable doc --locked --no-deps` | **passed** |
| Snapshots | `MCT_UPDATE_SNAPSHOTS=1 cargo +stable test --lib ui::snapshots` | **47 passed, 0 failed** |
| Real PTY restore | `script -q -e -c 'cargo +1.85.0 test --locked --test terminal_restore -- --ignored --nocapture'` | **1 passed, 0 failed** |
| Real Agent loopback E2E | `MINICORE_AGENT_BIN=../agent/target/debug/minicore-agent cargo +1.85.0 test --locked --test agent_e2e -- --ignored` | **7 passed, 0 failed** |
| Dependency tree | `cargo +stable tree -d`; `cargo +stable tree -p crossterm` | **passed; ratatui 0.29.0, crossterm 0.28.1** |
| Windows cross-check | Parent final6: `cargo +stable check --locked --offline --all-targets --target x86_64-pc-windows-gnu` | **passed; not native execution or CI** |
| macOS cross-check | Parent final6: `cargo +stable check --locked --offline --all-targets --target x86_64-apple-darwin` | **passed; not native execution or CI** |
| Backend dependency absence | Parent: Linux-filtered `cargo +1.85.0 metadata --locked --offline --format-version 1` | **passed; neither minicore-agent nor minicore-runtime is linked** |
| Source/provenance audit | Parent checked all three actual HEADs, Agent archive SHA, Runtime lock revision, and unchanged backend tracked trees | **passed; see backend.md** |

The all-target count includes 197 library tests, 8 main tests, 49 app-flow
tests, 8 protocol tests, 4 render-snapshot tests, 2 RPC I/O tests, 5
terminal-restore tests, and 8 ignored tests (7 Agent E2E tests plus 1 ignored
real-PTY test). The real Agent run explicitly exercised discovery, a basic
turn, tool execution, steering, same-loop update, next-turn update, and
shutdown cancellation.

Final6 added slash command entry and restricted-state Composer regressions to the prior reviewer evidence:

- `late_completed_loop_events_cannot_bind_a_new_prompt`: a completed L1's late
  request, delta, and state events cannot bind the pending L2 prompt.
- `reopen_invalidates_old_wait_persisted_response`,
  `reopen_invalidates_old_wait_failed_response`,
  `loaded_running_session_reopen_reuses_view_after_state_failure`, and
  `failed_close_reopen_keeps_retired_loop_fenced`: loaded sessions reuse their
  existing view, while close→reopen fences old requests/events and preserves
  the fence after an open failure.
- `close_verification_internal_or_malformed_retains_loaded_state`: only
  `SESSION_NOT_LOADED` proves unload; internal and malformed checks remain
  unknown.
- `agent_exit_marks_live_result_unconfirmed_without_overwriting_known_result`
  and the fatal overlay tests: unknown live results and known results remain
  distinguishable after connection loss.
- `late_steer_ack_after_complete_history_marks_missing_steer_not_recorded`,
  `late_steer_ack_respects_recorded_and_uncertain_history`, and
  `steering_ack_only_clears_the_same_editor_revision`: late steering acks use
  the submission editor revision; retyping the same text does not clear it,
  unchanged text does, and direct `AppEvent::SteerTurn` cannot clear it.
  Complete persisted
  History can move a missing steer to `NotRecorded`, while recorded and
  failed/uncertain History keep their conservative state and new composer text
  survives the late acknowledgement.
- `late_session_events_cannot_overwrite_info_or_clear_a_new_loop` and
  `session_opened_event_initializes_unknown_view_and_reads_running_state`:
  `session_opened` does not overwrite initialized SessionInfo, no-loop idle
  cannot regress a current loop, and first-open RunningLoop placeholders accept
  normal subsequent events.
- `slash_cancel_sends_exact_turn_cancel_and_wait_reconciles` verifies the
  actual Composer key path, exact `turn.cancel` fields, preservation of the
  original `turn.wait`, and cancelled/persisted History reconciliation.
- `slash_refresh_and_restricted_commands_remain_usable` verifies one-shot
  retained-TurnRef refresh, duplicate-wait suppression, blocked/finishing/no-
  session command access, `/close confirm`, and rejection of ordinary
  prompt/steer/update operations.
- `forced_shutdown_message_combines_unknown_known_failure_and_stderr`,
  `forced_shutdown_timeout_report_keeps_unknown_and_known_failure_facts`, and
  `forced_shutdown_drains_gated_stderr_before_reporting`: forced shutdown
  evidence combines unknown/known result facts with captured stderr; the gated
  fake agent leaves both a late failed `turn.wait` result and stderr unread
  before kill, and the child is killed and reaped. The ordinary
  `fake_hanging_agent_stderr_survives_until_kill_and_reap` test remains coverage
  for the unchanged `terminate()` behavior.

## Deliberate Non-Execution

- GitHub Actions Linux, macOS, and Windows jobs were not run; no CI jobs were
  triggered. No native macOS/Windows remote runner was available, and local Rust
  builds were prohibited. Linux tests and cross-checks are not native platform CI.
- External LLM provider execution was intentionally replaced by the isolated
  loopback mock. Production Store append-failure/worker-panic injection was not
  added to the binary; TUI fixtures and the unchanged Agent's targeted tests
  cover those contracts without a debug RPC. No power-loss durability is claimed.
- Unfiltered offline Cargo metadata failed on uncached Redox-only
  `redox_syscall 0.5.18`; Linux-filtered metadata then passed. No Redox build is
  claimed and no network download was performed to satisfy that unused target.
- Unsupported capabilities remain intentionally out of scope: Approval,
  Compaction, Plugin, MCP, Subagent, reconnect, automatic restart, and local
  Store migration.

## Artifacts

The parent independently reran the final source after the last review. Local
copies of `delivery-msrv.log`, `delivery-stable.log`, `delivery-e2e.log`,
`delivery-fmt.log`, `delivery-clippy.log`, `delivery-doc.log`,
`delivery-release.log`, `delivery-windows.log`, `delivery-macos.log`, and
`delivery-pty.log` are under `docs/verification/`. They confirm the 273-test
suites, seven E2E scenarios, quality gates, two release checks, both cross-target
checks, and the real PTY round trip without relying on subagent summaries.

Raw final2 through final6 post-review logs are preserved on the remote
builder under `/root/minicore-tui-r2-01a06ec1/logs/final[23456]-*`, including
`final3-pty.log`, `final3-agent-e2e.log`, `final3-postreview.log`,
`final3-snapshots.log`, `final3-dependencies.log`, `final3-docs.log`, and the
corresponding `final4-*`, `final5-*`, and `final6-*` logs. The acceptance matrix is generated by
`tools/generate_acceptance.py`; its statuses are conservative and distinguish
executable evidence, source/provenance evidence, and unrun platform CI.

The final parent audit accepts MIG-001/002/006/007/141/160 by their applicable
source/provenance/documentation checks. Final status: **157 PASS, 3 NOT RUN**
(MIG-138/139/140). No incomplete runtime behavior is hidden in a source-audit
label. Both independent review roles finished with no remaining findings.
