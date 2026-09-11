# Stage 4 Session Panel Verification

**Historical checkpoint, not final acceptance.** Later stages tightened
history-gap request revisions, and native verification found filtered-selection
and dialog-target gaps. Current results and installation evidence are in
[Session Management Acceptance](../session-management/README.md); counts and
claims below are retained as stage-local history.

This report records the Session-panel safety and coverage follow-up. Only the
TUI was changed in Stage 4. The Agent, prompt-file implementation, RPC
versions, dependencies, `Cargo.toml`, and `Cargo.lock` were not changed.

## Requirement Matrix

| Requirement | Evidence | Status |
|---|---|---|
| Close verification writes the returned state back and preserves running/blocked safety evidence | `src/app.rs:on_close_verify_state_response`; `tests/app_flow.rs:close_verification_running_state_is_written_without_cancelling_the_turn`; `close_verification_blocked_state_remains_unsafe_for_delete` | PASS |
| Internal, malformed, and transport-unknown close outcomes cannot reuse the old Idle state; the next explicit close confirmation rereads state | `tests/app_flow.rs:close_verification_internal_or_malformed_retains_loaded_state`; `close_verification_transport_failure_requires_a_fresh_state_read` | PASS |
| A close verification reply for a running session does not cancel the active turn or change the active SessionId | `tests/app_flow.rs:close_verification_running_state_is_written_without_cancelling_the_turn` | PASS |
| `event_gap` blocks direct and Session-panel Close/Delete until reconciliation, while Rename remains available | `src/app.rs:session_action_safety`; `tests/app_flow.rs:event_gap_blocks_close_and_delete_for_loaded_and_closed_sessions`; `src/ui/component_tests.rs:event_gap_blocks_session_panel_close_and_delete_but_allows_rename` | PASS |
| Successful deletion tombstones the ID, retires its ID-scoped lifecycle requests, and rejects late open/rename/state/history/event inputs without resurrection, new RPCs, or active-session changes | `src/app.rs:on_delete_session_response`, response/event guards; `tests/app_flow.rs:deleted_session_id_rejects_late_lifecycle_responses_and_events` | PASS |
| Help and Logs body Home/End/Page operations use panel scroll only and leave transcript content/scroll unchanged | `src/ui/component_tests.rs:help_and_logs_body_paging_does_not_scroll_the_transcript` | PASS |
| Generic selector mouse selection/confirmation uses the rendered selector geometry | `src/ui/component_tests.rs:generic_selector_mouse_selects_and_confirms_the_hit_item`; existing session selector mouse coverage | PASS |
| Session footer mouse hit-testing dispatches a real footer action from its shared action rectangle | `src/ui/component_tests.rs:session_footer_mouse_refresh_uses_the_action_hit_rect` | PASS |
| Help, Logs, and a multi-row Session selector all move PageDown at 60×16 | `src/ui/component_tests.rs:short_panels_page_down_moves_help_logs_and_session_selector` | PASS |
| Real Agent E2E verifies default Cancel + Enter does not send `session.delete`, then verifies explicit Delete | `tests/agent_e2e.rs:e2e_session_panel_rename_and_delete_against_current_agent` | PASS |
| Literal snapshots and all-target behavior remain green | `stage4-tui-stable-all-targets-final.log`; `stage4-tui-msrv-all-targets-final.log`; snapshot integration tests | PASS |

Live Assistant Markdown remains intentionally deferred.

## Validation

All Rust commands ran on the authorized remote Linux builder. No user store,
credential, or external service was read. The real-Agent tests use the isolated
loopback mock Responses API and the current Agent binary.

- Stable `cargo test --locked --all-targets -- --test-threads=1`: **457 passed, 0 failed**; 17 Agent E2E tests and 1 real-PTY test are ignored in this target run.
- Rust 1.85.0 MSRV `cargo +1.85.0-x86_64-unknown-linux-gnu test --locked --all-targets -- --test-threads=1`: **457 passed, 0 failed**; the same 18 tests are ignored.
- Stable ignored real-Agent E2E: **17 passed, 0 failed**.
- MSRV ignored real-Agent E2E: **17 passed, 0 failed**.
- Stable strict Clippy (`-D warnings`): **passed**.
- Stable/MSRV rustdoc with `RUSTDOCFLAGS="-D warnings"`: **passed**.
- Stable formatter check: **passed**.
- `git diff --check`: **passed**.
- MSRV strict Clippy remains blocked only by four pre-existing diagnostics in unmodified files: `src/ui/editor_layout.rs:283-284`, `src/ui/rail.rs:177`, and `src/markdown.rs:817`. No unrelated cleanup was authorized.
- `real_pty_enter_and_restore_round_trip` remains ignored outside a TTY. Hosted CI and native cross-machine pointer timing were not run.

## Hash Binding

The complete before/after SHA-256 manifest for every file under `src/`,
`tests/`, and `snapshots/`, plus `Cargo.toml` and `Cargo.lock`, is recorded in
`/tmp/minicore-session-panels.M0rGv9/logs/stage4-source-hashes-final.log`.
The Stage4-before status and baseline hashes are retained in
`/tmp/minicore-session-panels.M0rGv9/logs/stage4-source-before.log`.
The selected Stage4 source hashes at completion were:

| File | SHA-256 |
|---|---|
| `src/app.rs` | `dc68fffdcbf151a46f0939b245676311a696e5524b428ab30b3aec162213c565` |
| `src/state/session.rs` | `b898bbeabbbfa391a8b9f723f103dbd5c0a632e7cf3a58bc37c3727fb1cf0342` |
| `src/ui/component_tests.rs` | `dbc91e1bcdd964071590ee36b49905f33f37b0f2398b10bde340f6ff4a5404ca` |
| `src/ui/testapp.rs` | `360f54a11e342a873f268ed1d0bbe94452f76d3deae25ccb3320c40032813bc9` |
| `tests/app_flow.rs` | `a13392d1a1ef7e81d5e2a190ecfd433c5b547303769d19437f659f122f4ac07d` |
| `tests/agent_e2e.rs` | `caad603e17a4f3aa90b66b1b7313b029fa6d087ebc016d25ada38a83d46f5a2f` |
| `Cargo.toml` | `5666ff82804b88e8a98221a7f3a78aa736d53db2c946b605f04d8c150b8d3a33` |
| `Cargo.lock` | `40a638f2ef3b3192ee1731796c77a6dcf018d95f66f817ef191eb3b6182e01de` |

The manifest also records unchanged snapshot hashes and all other source/test
hashes, so snapshot and Cargo drift is independently auditable.

## Preserved Evidence

The failed RED runs remain in `/tmp/minicore-session-panels.M0rGv9/logs/`,
including:

- `stage4-red-session-safety.log` — initial running-state and tombstone failures.
- `stage4-red-close-transport-final.log` — transport failure without the unknown-state guard.
- `stage4-red-event-gap-final.log` — direct Close/Delete failure without the event-gap guard.
- `stage4-red-event-gap-ui-final.log` — corrected Session-panel event-gap failure.

GREEN and final gate logs include:

- `stage4-green-session-safety.log`
- `stage4-green-targeted.log`
- `stage4-green-event-gap-final.log`
- `stage4-green-tombstone-final.log`
- `stage4-green-core-final.log`
- `stage4-tui-stable-all-targets-final.log` / `stage4-tui-stable-all-targets-final2.log`
- `stage4-tui-msrv-all-targets-final.log` / `stage4-tui-msrv-all-targets-final2.log`
- `stage4-agent-e2e-stable-final.log` / `stage4-agent-e2e-final2.log`
- `stage4-agent-e2e-msrv-final.log` / `stage4-agent-e2e-final2.log`
- `stage4-clippy-stable-final.log`
- `stage4-clippy-msrv-final.log` / `stage4-clippy-msrv-final2.log`
- `stage4-doc-stable.log`
- `stage4-doc-msrv.log` / `stage4-doc-final2.log`
- `stage4-source-hashes-final.log`

All `stage4-*` logs were also copied to the remote TUI mirror under
`/root/minicore-session-panels.H0sLzv/source/minicore-tui/logs/`. Earlier
Stage 1/2/3 diffs, logs, snapshots, and stream-interaction artifacts remain
untouched.

No commit, push, reset, cleanup, dependency change, lockfile change, Agent
source change, or Runtime change was made.
