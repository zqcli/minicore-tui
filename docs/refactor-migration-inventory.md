# v0.3 Refactor Migration Inventory (Historical Stage A)

This file preserves the frozen stage-A inventory of user-visible commands, E2E
scenarios, and core behaviors that every later stage had to migrate rather than
drop (Spec §22.2, §28.2). It is a historical baseline, not the current release
matrix. The current 0.3.0 migration guide is
[`migration-0.2-to-v0.3.md`](migration-0.2-to-v0.3.md), and the current
acceptance statuses are in [`refactor-acceptance.md`](refactor-acceptance.md).

The delivered v0.3 surface now includes E1 tool details, E2 workspace/file
references and preview, E3 Changes/Diff/Context, D3 settings and external
editing, and phase-F query-scope invalidation. Stage B–F retained the source
and behavior boundaries below; the original Stage-A counts are not relabeled.

## Stage-A Slash Commands (Historical)

Parser: [`src/command.rs`](../src/command.rs). The list below is the frozen
Stage-A command set. The delivered v0.3 surface additionally includes
`/rename /search /copy /export /files /grep /diff /tool /context /compact
/editor /settings /refresh`; the live command table and key behavior are in
[`docs/keybindings.md`](keybindings.md).

| Command | Args | Effect | Current test |
|---|---|---|---|
| `/new` | none | open new-session form | `command::tests::parses_every_implemented_command` |
| `/resume` | none | open session selector | same |
| `/sessions` | none | open session selector | same |
| `/model` | none | open model selector | same |
| `/reasoning` | none | open reasoning selector | same |
| `/theme` | `dark\|light` | switch palette | same |
| `/clear` | none | clear local view + reload | same |
| `/help` | none | open help | same |
| `/logs` | none | open agent log panel | same |
| `/cancel` | none | `turn.cancel` on the active loop | `app_flow::slash_cancel_sends_exact_turn_cancel_and_wait_reconciles` |
| `/reload` | none | `agent.reload` + metadata refresh | `app_flow` reload tests, `agent_e2e::e2e_configuration_reload_refreshes_catalogs_and_active_session` |
| `/quit` | none | `agent.shutdown` | `app_flow` shutdown tests |
| `/close` | `[confirm]` | close active session | `app_flow::session_close_and_delete_command_lifecycle` |
| `/delete` | `[confirm]` | delete a closed session | same |

Keys: [`src/keymap.rs`](../src/keymap.rs) (F1 help, F2 rename, F5 refresh
sessions, F6 Main/Editor focus toggle, Ctrl+N/R/L/O/T, Esc/PageUp/PageDown/
Home/End, Ctrl+C/Ctrl+D).

## Stage-A Real-Agent E2E Scenarios (18, Historical)

File: [`tests/agent_e2e.rs`](../tests/agent_e2e.rs). Run with
`MINICORE_AGENT_BIN=… cargo test --locked --test agent_e2e -- --ignored
--test-threads=1`. Each starts a loopback Responses mock with a synthetic
workspace/data dir. The current target has 34 scenarios; the 18 rows below are
preserved as the Stage-A migration inventory and are not the final count.

| # | Scenario | Must keep |
|---|---|---|
| 1 | `e2e_configuration_reload_refreshes_catalogs_and_active_session` | reload refresh without history reinstall |
| 2 | `e2e_scenario_a_discovery` | ping/models/profiles/sessions bootstrap |
| 3 | `e2e_session_panel_rename_and_delete_against_current_agent` | rename/delete semantics |
| 4 | `e2e_scenario_b_basic_turn` | send → stream → wait → durable replace |
| 5 | `e2e_max_reasoning_ships_literal_max_and_provider_body_carries_it` | extended reasoning not downgraded |
| 6 | `e2e_scenario_c_tool_execution` | tool call + result card |
| 7 | `e2e_scenario_d_steer_turn` | steer reaches provider |
| 8 | `e2e_reasoning_summary_item_boundaries_survive_to_the_tui` | reasoning part boundaries |
| 9 | `e2e_two_consecutive_steers_both_reach_the_provider` | steer FIFO |
| 10 | `e2e_fifo_steers_are_paced_until_receipt` | steer ACK/receipt pacing |
| 11 | `e2e_fifo_duplicate_texts_are_paced_and_both_persist` | duplicate-text steer identity |
| 12 | `e2e_scenario_e_same_loop_update` | model update next request |
| 13 | `e2e_scenario_e2_update_single_request_then_next_turn` | update single-request/next-turn |
| 14 | `e2e_scenario_f_shutdown_cancels_active_wait` | shutdown cancels active wait |
| 15 | `e2e_stress_six_loops_ten_requests_no_repeated_final_text` | multi-loop integrity |
| 16 | `e2e_stress_second_tool_expansion_survives_background_generation` | tool fold across generations |
| 17 | `e2e_live_request_usage_shows_during_loop_and_persisted_total_replaces_it` | live vs persisted usage |
| 18 | `e2e_stress_session_switch_preserves_tool_fold` | session switch keeps folds |

## Core behavior migration list

Each row is a behavior with existing evidence that later stages must keep
(updated to the new data path) or explicitly re-scope with a disclosed
replacement test.

| Behavior | Current evidence | Stage |
|---|---|---|
| Protocol v1 handshake + capability check | `tests/baseline_defects.rs::baseline_bootstrap_rejects_agent_0_5_protocol_v1` (RED for 0.5), stage B adds positive | B |
| Chunked raw-item read + cross-page UTF-8 | `tests/agent_v1_fixtures.rs::read_pages_reconstruct_exactly_with_continuation_cursors` | B |
| Turn completion + retained result | `tests/agent_v1_fixtures.rs::turn_result_availability_covers_pending_and_stored` | B |
| Tool facts by full ToolRef | `tests/agent_v1_fixtures.rs::tool_read_distinguishes_awaiting_policy_running_and_terminal` | B |
| Tool stream raw offsets/base64 | `tests/agent_v1_fixtures.rs::tool_output_streams_use_raw_byte_offsets` | B |
| Workspace read statuses | `tests/agent_v1_fixtures.rs::workspace_read_statuses_are_distinct` | B/E |
| Workspace files/search cursors | `tests/agent_v1_fixtures.rs::workspace_files_and_search_report_partial_pages` | B/E |
| Workspace status detached/unavailable | `tests/agent_v1_fixtures.rs::workspace_status_covers_repo_detached_and_unavailable` | B/E |
| Changes opaque refs + hunks | `tests/agent_v1_fixtures.rs::changes_list_and_diff_keep_opaque_refs_and_structured_hunks` | B/E |
| Sending admission never blocks UI | `tests/backpressure_baseline.rs::baseline_send_blocks_when_the_outbound_queue_is_full` (RED), stage B adds `try_send` | B |
| Preparing/cancel routing | `tests/preparing_baseline.rs::baseline_cannot_cancel_a_preparing_submission_before_turn_ref` (RED) | B |
| Stable prefix + viewport-only composition | `tests/performance.rs::baseline_all_lines_materializes_full_transcript` (RED) | C |
| Clipboard/export/editor off the UI loop | `tests/baseline_defects.rs::baseline_run_commands_awaits_send_and_clipboard` (RED) | C |
| Reload does not replace history | `tests/baseline_defects.rs::baseline_reload_stages_a_full_history_replacement` (RED) | C |
| Rail visual baseline | `src/ui/snapshots.rs` (27 files), `tests/rail_fixtures.rs` | frozen |
| CJK/IME/mouse/scrollbar/Terminal restore | existing `ui::*` and `terminal_restore` tests | frozen |
| Steer accept/apply/record separation | `tests/app_flow.rs` steer tests, E2E 7/9/10/11 | frozen |
| Model update next-request semantics | E2E 12/13 | frozen |
| Background sessions + folds | E2E 18 | frozen |

## Frozen visual baseline

The Rail snapshots under [`snapshots/`](../snapshots/) were verified unchanged
on the stage-A baseline; no expectation image was adjusted. Any stage that
changes a snapshot must justify it as an intentional visual change, not accept
a regression (Spec §22.3).
