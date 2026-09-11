# Stage 3 Session Panel Verification

**Historical checkpoint, not final acceptance.** Subsequent review and native
verification found stale close-state, event-gap, late-response resurrection and
filtered-selection gaps in the claims below. Those PASS labels describe the
then-selected tests, not proof of the whole safety requirement. The fixes,
nonzero regressions and current verdict are in
[Session Management Acceptance](../session-management/README.md).

This report records the Stage 2 review follow-up for the shared TUI. The Agent
source and its dependencies were not modified. The TUI↔Agent boundary remains
opaque to the TUI; prompt/config ownership stays in the Agent.

## Requirement Matrix

| Requirement | Evidence | Status |
|---|---|---|
| Unknown or pending session state is re-read before close/delete; stale state is not treated as safe | `src/ui/component_tests.rs:session_delete_requires_close_then_second_confirmation_and_tombstones_id`; `tests/app_flow.rs:regression_test_close_agent_error_single_state_check_and_store_error` | PASS |
| A close error followed by `SESSION_NOT_LOADED` converges to the closed state | `src/app.rs:mark_session_closed`; `tests/app_flow.rs:regression_test_close_agent_error_single_state_check_and_store_error` | PASS |
| Busy, blocked, finishing, unsaved, result-unconfirmed, and closing sessions are protected from direct deletion | `tests/app_flow.rs:session_close_and_delete_command_lifecycle`; Session-panel guard tests | PASS |
| Loaded deletion requires close, then a separate permanent-delete confirmation whose default is Cancel | `src/ui/component_tests.rs:session_delete_requires_close_then_second_confirmation_and_tombstones_id` | PASS |
| Refresh, reorder, filtering, late responses, pending deletes, and tombstones cannot retarget or resurrect a session | `src/ui/component_tests.rs:session_selector_refresh_preserves_selected_id_after_reorder`; `session_double_click_release_rechecks_the_current_target` | PASS |
| Session selector opening refreshes the catalog; selector/footer mouse hit-testing shares render geometry | `src/ui/testapp.rs:open_session_selector`; `src/ui/component_tests.rs:session_panel_mouse_selection_and_double_click_use_shared_content_rect` | PASS |
| Compact 60×16 layouts keep fields, actions, confirmations, and footer controls reachable; paging uses geometry | `src/ui/panel.rs`; selector/new-session/help/logs snapshots and component tests | PASS |
| Rename waits for a complete ACK, preserves the draft on error, and uses the stable SessionId | `src/ui/component_tests.rs:session_panel_rename_uses_id_and_waits_for_complete_ack` | PASS |
| The real TUI/App path works against the current Agent for rename and delete | `tests/agent_e2e.rs:e2e_session_panel_rename_and_delete_against_current_agent` | PASS |
| Snapshot comparisons remain literal and only affected panel snapshots changed | `snapshots/session_selector_dark_80x24.txt`; `snapshots/session_selector_light_120x40.txt`; all snapshot tests | PASS |

Live Assistant Markdown remains intentionally deferred.

## Validation

All Rust commands ran on the authorized remote Linux builder. The local machine
was used only for file synchronization and artifact inspection.

- Stable `cargo test --locked --all-targets -- --test-threads=1`: **447 passed, 0 failed, 18 ignored** across lib, bin, integration targets, and the ignored-by-default target listings. The `agent_process` harness target has no libtest result line.
- MSRV Rust 1.85 `cargo +1.85.0-x86_64-unknown-linux-gnu test --locked --all-targets -- --test-threads=1`: **447 passed, 0 failed, 18 ignored**.
- Stable ignored real-Agent E2E: **17 passed, 0 failed**.
- MSRV ignored real-Agent E2E: **17 passed, 0 failed**.
- Stable strict Clippy (`-D warnings`): **passed**.
- Stable/MSRV rustdoc with `RUSTDOCFLAGS="-D warnings"`: **passed**.
- Stable/MSRV formatter checks: **passed**.
- `git diff --check`: **passed**.
- MSRV strict Clippy remains blocked by four pre-existing diagnostics in unmodified files: `src/ui/editor_layout.rs:283-284`, `src/ui/rail.rs:177`, and `src/markdown.rs:817`. The two diagnostics introduced by the Session panel were fixed; no unrelated cleanup was authorized.
- The real-PTY restore test remains ignored outside a TTY. Hosted CI and native cross-machine pointer timing were not run.

## Preserved Evidence

Local logs are retained under `/tmp/minicore-session-panels.M0rGv9/logs/` with
`stage3-*` names. Copies are retained in the remote TUI mirror at
`/root/minicore-session-panels.H0sLzv/source/minicore-tui/logs/`.

Important artifacts include:

- `stage3-tui-stable-all-targets-final.log`
- `stage3-tui-msrv-all-targets-final.log`
- `stage3-tui-agent-e2e-stable-final.log`
- `stage3-tui-agent-e2e-msrv-final.log`
- `stage3-clippy-stable-final-2.log`
- `stage3-clippy-msrv-final-2.log`
- `stage3-doc-stable-final.log`
- `stage3-doc-msrv-final.log`
- `stage3-session-panel-agent-e2e-2.log`
- `stage3-snapshot-update.log`

No commit, push, reset, cleanup, dependency change, lockfile change, Agent
source change, or Runtime change was made.
