# r2 Acceptance Status & MIG-001..160 Verification Matrix

This matrix maps every MIG-001..160 criterion to source and executable evidence.
`PASS` requires the cited executable test or exact dependency/source check to have passed in the
remote final6 runs. Pins, dependency absence, and evidence-recording requirements use source audits,
not invented runtime tests. `NOT RUN` means the criterion was not executed; a remote Linux run never substitutes
for an unrun GitHub Actions platform job. The implementer did not run local Cargo/Rust tools.

## Remote Final6/Post-review Evidence

All commands ran in `/root/minicore-tui-r2-01a06ec1/tui` on `192.168.20.199`; final6 logs are under
`/root/minicore-tui-r2-01a06ec1/logs/final6-*`.

- `cargo +1.85.0 test --locked --all-targets`: **273 passed, 0 failed, 8 ignored** (197 lib, 8 main, 49 app_flow, 8 protocol, 4 render_snapshots, 2 rpc_io, 5 terminal_restore; 7 ignored Agent E2E and 1 ignored real-PTY test).
- `cargo +stable test --locked --all-targets`: **273 passed, 0 failed, 8 ignored**.
- `MINICORE_AGENT_BIN=../agent/target/debug/minicore-agent cargo +1.85.0 test --locked --test agent_e2e -- --ignored`: **7 passed, 0 failed** (A, B, C, D, E, E2, F; Agent SHA recorded separately).
- `MCT_UPDATE_SNAPSHOTS=1 cargo +stable test --lib ui::snapshots`: **47 passed, 0 failed**; generated snapshots were then checked by both full suites.
- `script -q -e -c 'cargo +1.85.0 test --locked --test terminal_restore -- --ignored --nocapture'`: **1 passed, 0 failed** under a remote PTY.
- `cargo +stable test --release --lib version_gate`: **2 passed, 0 failed**; the prerelease expectation is conditional on `cfg!(debug_assertions)`.
- `cargo +stable fmt --all -- --check`, `cargo +1.85.0 fmt --all -- --check`, clippy with `-D warnings`, and rustdoc with `RUSTDOCFLAGS=-D warnings`: **all passed**.
- `cargo +stable tree -d` and `cargo +stable tree -p crossterm`: **passed**; dependency tree shows ratatui 0.29.0 and crossterm 0.28.1.
- GitHub Actions Linux, macOS, and Windows jobs were not run in final6; remote Linux and cross-target compilation are separate evidence and are not substituted for CI.

## Status Breakdown

- **PASS**: 157 criteria supported by final6 execution, exact dependency checks, or source/provenance audits appropriate to the criterion.
- Source-audit criteria MIG-001, MIG-002, MIG-006, MIG-007, MIG-141, and MIG-160 were independently checked by the parent; these do not claim runtime SHA attestation or platform CI execution.
- **NOT RUN**: 3 criteria (`MIG-138, MIG-139, MIG-140`); platform CI status is not substituted by remote Linux execution.

## MIG-001..160 Acceptance Matrix

| ID | Specification Item (Spec 68) | Source Mapping | Test Mapping | Verification Status |
|---|---|---|---|---|
| MIG-001 | 固定Agent实际HEAD | `docs/backend.md, Cargo.toml` | `docs/backend.md (Source Audit & Provenance)` | **PASS** |
| MIG-002 | 固定Runtime实际HEAD | `docs/backend.md` | `docs/backend.md (Source Audit & Provenance)` | **PASS** |
| MIG-003 | Agent 0.3.x version gate | `src/protocol.rs:is_supported_agent_version` | `src/protocol.rs:version_gate_is_exactly_0_3_major_minor` | **PASS** |
| MIG-004 | Agent 0.2拒绝 | `src/protocol.rs:is_supported_agent_version` | `src/protocol.rs:version_gate_is_exactly_0_3_major_minor` | **PASS** |
| MIG-005 | Agent 0.4拒绝 | `src/protocol.rs:is_supported_agent_version` | `src/protocol.rs:version_gate_is_exactly_0_3_major_minor` | **PASS** |
| MIG-006 | 不依赖Agent Rust crate | `Cargo.toml` | `Cargo.lock (Source Audit)` | **PASS** |
| MIG-007 | 不依赖Runtime Rust crate | `Cargo.toml` | `Cargo.lock (Source Audit)` | **PASS** |
| MIG-008 | 无双协议兼容层 | `src/protocol.rs, src/app.rs` | `tests/protocol.rs:unknown_fields_and_usage_defaults_and_outcome_tolerance` | **PASS** |
| MIG-009 | 无instance_id | `src/protocol.rs:TurnRef` | `tests/protocol.rs:turn_wait_is_a_direct_turn_result_view` | **PASS** |
| MIG-010 | 无session.transcript | `src/protocol.rs:OutgoingRequest` | `tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items` | **PASS** |
| MIG-011 | 无ConversationSeq | `src/protocol.rs:IndexedHistoryItemWire` | `tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items` | **PASS** |
| MIG-012 | 无durable TurnTerminal DTO | `src/protocol.rs:TurnResultViewWire` | `tests/protocol.rs:turn_wait_is_a_direct_turn_result_view` | **PASS** |
| MIG-013 | 无SessionStatus Closing | `src/protocol.rs:SessionStatusWire` | `tests/protocol.rs:session_state_uses_an_active_loop_object` | **PASS** |
| MIG-014 | 无unfinished Turn repair文案 | `src/app.rs` | `tests/app_flow.rs:regression_test_close_wait_correlation_and_guards` | **PASS** |
| MIG-015 | 无Model不可变文案 | `src/ui/selector.rs, src/app.rs` | `src/ui/selector.rs:model_line_marks_the_current_session_model` | **PASS** |
| MIG-016 | 无Steering unsupported文案 | `src/app.rs:steer_turn` | `src/app.rs:composer_routes_to_steer_turn_when_session_is_running` | **PASS** |
| MIG-017 | 无Tool argument推断 | `src/ui/tool.rs` | `src/ui/component_tests.rs:tool_preview_caps_chars_at_32k_and_lines_at_40` | **PASS** |
| MIG-018 | TurnRef使用loop_id | `src/protocol.rs:TurnRef` | `tests/protocol.rs:turn_wait_is_a_direct_turn_result_view` | **PASS** |
| MIG-019 | SessionInfo新shape | `src/protocol.rs:SessionInfo` | `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | **PASS** |
| MIG-020 | SessionState五状态 | `src/protocol.rs:SessionStatusWire` | `tests/protocol.rs:session_state_uses_an_active_loop_object` | **PASS** |
| MIG-021 | LoopState解析 | `src/protocol.rs:LoopStateWire` | `tests/protocol.rs:session_state_uses_an_active_loop_object` | **PASS** |
| MIG-022 | ConfigRevision解析 | `src/protocol.rs:SessionUpdateResult` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | **PASS** |
| MIG-023 | RequestStarted解析 | `src/protocol.rs:RequestStartedDataWire` | `tests/protocol.rs:event_fixtures_keep_request_index_and_tool_outcome` | **PASS** |
| MIG-024 | OutputDelta request_index | `src/protocol.rs:OutputDeltaDataWire` | `tests/protocol.rs:event_fixtures_keep_request_index_and_tool_outcome` | **PASS** |
| MIG-025 | Tool Event request_index | `src/protocol.rs:ToolStartedDataWire` | `tests/protocol.rs:event_fixtures_keep_request_index_and_tool_outcome` | **PASS** |
| MIG-026 | History Page解析 | `src/protocol.rs:HistoryPageWire` | `tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items` | **PASS** |
| MIG-027 | History四种Item | `src/protocol.rs:HistoryItemWire` | `tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items` | **PASS** |
| MIG-028 | TurnResult persistence | `src/protocol.rs:TurnPersistenceWire` | `tests/protocol.rs:turn_wait_is_a_direct_turn_result_view` | **PASS** |
| MIG-029 | Usage optional字段 | `src/protocol.rs:UsageWire` | `tests/protocol.rs:unknown_fields_and_usage_defaults_and_outcome_tolerance` | **PASS** |
| MIG-030 | session.update | `src/protocol.rs:session_update` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | **PASS** |
| MIG-031 | turn.steer | `src/protocol.rs:steer_turn` | `tests/app_flow.rs:steer_queue_full_retains_composer_input_and_shows_warning` | **PASS** |
| MIG-032 | 新错误码解析 | `src/protocol.rs:RpcError` | `src/rpc.rs:fake_unknown_method_reports_a_top_level_jsonrpc_error` | **PASS** |
| MIG-033 | offset分页 | `src/app.rs:continue_history_chain` | `tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count` | **PASS** |
| MIG-034 | 每页20 | `src/app.rs:open_session` | `tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count` | **PASS** |
| MIG-035 | index连续校验 | `src/app.rs:continue_history_chain` | `tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count` | **PASS** |
| MIG-036 | Prompt User Card | `src/ui/user.rs` | `src/ui/component_tests.rs:user_card_uses_the_spec_background` | **PASS** |
| MIG-037 | Steering Card | `src/ui/user.rs` | `src/ui/snapshots.rs:steering_dark_80x24` | **PASS** |
| MIG-038 | Assistant按request分组 | `src/ui/assistant.rs` | `tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate` | **PASS** |
| MIG-039 | ToolCall无arguments可渲染 | `src/ui/tool.rs` | `src/ui/component_tests.rs:tool_preview_caps_chars_at_32k_and_lines_at_40` | **PASS** |
| MIG-040 | ToolResult关联 | `src/app.rs:on_tool_finished` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-041 | Orphan ToolResult不丢 | `src/app.rs:on_tool_finished` | `src/app.rs:history_merges_user_assistant_tool_summary_without_synthetic_terminals` | **PASS** |
| MIG-042 | Summary可渲染 | `src/ui/transcript.rs` | `src/app.rs:history_merges_user_assistant_tool_summary_without_synthetic_terminals` | **PASS** |
| MIG-043 | 不创建fake Terminal | `src/app.rs:continue_history_chain` | `src/app.rs:history_merges_user_assistant_tool_summary_without_synthetic_terminals` | **PASS** |
| MIG-044 | Persisted后增量对齐 | `src/app.rs:reconcile_after_wait` | `tests/app_flow.rs:regression_scenario_b_wait_persisted_post_wait_history_and_no_infinite_retry` | **PASS** |
| MIG-045 | Reopen加载History | `src/app.rs:open_session, src/app.rs:activate_existing_session` | `tests/app_flow.rs:loaded_running_session_reopen_reuses_view_after_state_failure` | **PASS** |
| MIG-046 | 一个Session最多一个LiveLoop | `src/state/session.rs:SessionView` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-047 | LiveLoop多个LiveRequest | `src/state/turn.rs:LiveLoop` | `tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate` | **PASS** |
| MIG-048 | RequestStarted创建Request | `src/app.rs:on_request_started` | `tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate` | **PASS** |
| MIG-049 | Delta按request_index | `src/app.rs:append_delta` | `tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate` | **PASS** |
| MIG-050 | Tool按request_index | `src/app.rs:on_tool_started` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-051 | RequestStarted丢失时lazy request | `src/state/turn.rs:ensure_request_mut` | `tests/app_flow.rs:tool_events_before_started_are_retained_and_mark_a_gap` | **PASS** |
| MIG-052 | Event早于send response | `src/app.rs:on_agent_event` | `tests/app_flow.rs:loop_events_can_bind_before_turn_send_response` | **PASS** |
| MIG-053 | send后立即wait | `src/app.rs:on_send_response` | `tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live` | **PASS** |
| MIG-054 | wait乱序Response | `src/app.rs:on_wait_response` | `tests/app_flow.rs:regression_scenario_e_stale_wait_and_history_paging_idempotence` | **PASS** |
| MIG-055 | current request model/reasoning显示 | `src/ui/assistant.rs` | `src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved` | **PASS** |
| MIG-056 | Idle session.update | `src/app.rs:confirm_model_item` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | **PASS** |
| MIG-057 | Active session.update | `src/app.rs:confirm_model_item` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-058 | active_revision显示 | `src/ui/footer.rs` | `src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved` | **PASS** |
| MIG-059 | 同Loop RequestStarted确认实际配置，支持事件先于update响应 | `src/app.rs:on_request_started` | `tests/app_flow.rs:update_request_started_before_update_response_confirms_applied` | **PASS** |
| MIG-060 | current Tool保持旧revision | `src/app.rs:on_tool_started` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-061 | 下一Request使用新revision | `src/app.rs:on_request_started` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-062 | active_revision null解释正确 | `src/app.rs:on_update_session_response` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | **PASS** |
| MIG-063 | Update不延长Loop | `src/app.rs:on_update_session_response` | `tests/agent_e2e.rs:e2e_scenario_e2_update_single_request_then_next_turn` | **PASS** |
| MIG-064 | 无静默Reasoning降级 | `src/state/session.rs` | `tests/app_flow.rs:regression_test_pending_config_update_loop_scoping_and_no_rollback` | **PASS** |
| MIG-065 | Update blocked时错误 | `src/app.rs:confirm_model_item` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-066 | Running Composer为Steer | `src/app.rs:submit_composer` | `src/app.rs:composer_routes_to_steer_turn_when_session_is_running` | **PASS** |
| MIG-067 | Idle Composer为Prompt | `src/app.rs:submit_composer` | `src/app.rs:composer_routes_to_steer_turn_when_session_is_running` | **PASS** |
| MIG-068 | Steer成功清空Composer | `src/app.rs:on_steer_response, src/state/composer.rs:editor_revision` | `tests/app_flow.rs:steering_ack_only_clears_the_same_editor_revision` | **PASS** |
| MIG-069 | Steer失败保留Composer | `src/app.rs:on_steer_response` | `src/app.rs:composer_retains_text_on_steer_failure_and_shows_warning` | **PASS** |
| MIG-070 | QueueFull可见 | `src/app.rs:on_steer_response` | `tests/app_flow.rs:steer_queue_full_retains_composer_input_and_shows_warning` | **PASS** |
| MIG-071 | 多Steer FIFO显示 | `src/state/turn.rs:LiveLoop` | `tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation` | **PASS** |
| MIG-072 | 后续RequestStarted不冒充逐条Steer applied回执 | `src/app.rs:on_request_started` | `tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation` | **PASS** |
| MIG-073 | Persisted History含Steering | `src/app.rs:merge_history_items` | `tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation` | **PASS** |
| MIG-074 | 区分History未记录与保存未确认，不自动重发Steer | `src/app.rs:reconcile_after_wait` | `tests/app_flow.rs:late_steer_ack_after_complete_history_marks_missing_steer_not_recorded` | **PASS** |
| MIG-075 | WaitingForInput不自动steer | `src/app.rs:steer_turn` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-076 | Finishing不发送steer | `src/app.rs:steer_turn` | `tests/app_flow.rs:regression_test_close_wait_correlation_and_guards` | **PASS** |
| MIG-077 | Steer可使final Loop继续 | `src/app.rs:on_agent_event` | `tests/agent_e2e.rs:e2e_scenario_d_steer_turn` | **PASS** |
| MIG-078 | turn.wait检查persistence | `src/app.rs:on_wait_response` | `src/app.rs:turn_wait_persistence_failure_latches_blocked_and_creates_unsaved_loop` | **PASS** |
| MIG-079 | Persisted后对齐History；未完成前保留Live | `src/app.rs:reconcile_after_wait` | `tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live` | **PASS** |
| MIG-080 | Failed不假设本Loop已在内存History，也不推断磁盘必为空 | `src/app.rs:on_wait_response` | `tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view` | **PASS** |
| MIG-081 | Failed保留UnsavedLoop | `src/app.rs:on_wait_response` | `src/app.rs:turn_wait_persistence_failure_latches_blocked_and_creates_unsaved_loop` | **PASS** |
| MIG-082 | Failed显示event gap风险 | `src/ui/transcript.rs` | `src/ui/snapshots.rs:unsaved_gap_dark_80x24` | **PASS** |
| MIG-083 | Failed后Session Blocked | `src/app.rs:on_wait_response` | `tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view` | **PASS** |
| MIG-084 | Blocked禁用send | `src/app.rs:submit_composer` | `src/app.rs:composer_rejects_submit_when_session_is_blocked` | **PASS** |
| MIG-085 | Blocked禁用steer | `src/app.rs:steer_turn` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-086 | Blocked禁用update | `src/app.rs:confirm_model_item` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-087 | Blocked保留结果，显式close/open不保证数据一定丢失或恢复 | `src/app.rs:on_send_failed` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-088 | 不自动重试persist | `src/app.rs:on_wait_response` | `tests/app_flow.rs:regression_scenario_c_failed_wait_does_not_reconcile_and_idempotent` | **PASS** |
| MIG-089 | Finishing显示Saving | `src/ui/transcript.rs` | `src/ui/snapshots.rs:finishing_dark_80x24` | **PASS** |
| MIG-090 | TurnFinished event非权威 | `src/app.rs:on_agent_event` | `tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live` | **PASS** |
| MIG-091 | Active Loop不宣称恢复 | `src/app.rs` | `tests/app_flow.rs:regression_test_close_wait_correlation_and_guards` | **PASS** |
| MIG-092 | Graceful shutdown排空wait，取消reason=user合法 | `src/app.rs:request_shutdown` | `tests/app_flow.rs:shutdown_drains_after_child_exit_until_rpc_channel_ends` | **PASS** |
| MIG-093 | Agent crash Fatal | `src/app.rs:connection_terminated` | `src/ui/component_tests.rs:fatal_connection_renders_the_overlay` | **PASS** |
| MIG-094 | Reopen只见persisted History | `src/app.rs:open_session` | `tests/app_flow.rs:failed_close_reopen_keeps_retired_loop_fenced` | **PASS** |
| MIG-095 | Store错误可理解且不根据通用kind断定旧格式 | `src/app.rs` | `src/ui/snapshots.rs:store_error_dark_80x24` | **PASS** |
| MIG-096 | TUI不直接迁移Store | `src/app.rs` | `tests/app_flow.rs:regression_test_close_agent_error_single_state_check_and_store_error` | **PASS** |
| MIG-097 | Fullscreen Transcript+Dock | `src/ui/transcript.rs` | `src/ui/mod.rs:dock_is_below_the_transcript_with_a_composer_border` | **PASS** |
| MIG-098 | User背景卡 | `src/ui/user.rs` | `src/ui/component_tests.rs:user_card_uses_the_spec_background` | **PASS** |
| MIG-099 | Steering特殊卡 | `src/ui/user.rs` | `src/ui/snapshots.rs:steering_dark_80x24` | **PASS** |
| MIG-100 | Assistant Markdown | `src/ui/assistant.rs` | `src/markdown.rs:paragraphs_bold_italic_and_inline_code_are_styled` | **PASS** |
| MIG-101 | Reasoning灰色斜体 | `src/ui/assistant.rs` | `src/ui/component_tests.rs:reasoning_is_gray_and_italic_and_can_be_hidden` | **PASS** |
| MIG-102 | Tool三状态背景 | `src/ui/tool.rs` | `src/ui/component_tests.rs:tool_cards_use_state_backgrounds_and_expanded_preview_bounds` | **PASS** |
| MIG-103 | Unsaved红色卡 | `src/ui/transcript.rs` | `src/ui/snapshots.rs:unsaved_gap_dark_80x24` | **PASS** |
| MIG-104 | Request配置dim显示 | `src/ui/assistant.rs` | `src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved` | **PASS** |
| MIG-105 | Composer reasoning边框 | `src/ui/composer.rs` | `src/ui/component_tests.rs:composer_border_follows_the_reasoning_level` | **PASS** |
| MIG-106 | Pending update显示 | `src/ui/footer.rs` | `src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved` | **PASS** |
| MIG-107 | Footer双行 | `src/ui/footer.rs` | `src/ui/component_tests.rs:footer_is_one_row_on_short_terminals` | **PASS** |
| MIG-108 | Dark主题 | `src/theme.rs` | `src/theme.rs:dark_palette_matches_spec` | **PASS** |
| MIG-109 | Light主题 | `src/theme.rs` | `src/theme.rs:light_base_palette_matches_spec` | **PASS** |
| MIG-110 | 无Pi品牌资产 | `src/ui/transcript.rs` | `tests/render_snapshots.rs:empty_dark_scene_matches_the_committed_snapshot` | **PASS** |
| MIG-111 | Ratatui 0.29 | `Cargo.toml` | `Cargo.lock` | **PASS** |
| MIG-112 | Crossterm 0.28 | `Cargo.toml` | `Cargo.lock` | **PASS** |
| MIG-113 | Rust 1.85 | `Cargo.toml` | `Cargo.lock` | **PASS** |
| MIG-114 | 单stdin writer | `src/rpc.rs:RpcProcess` | `src/rpc.rs:writer_emits_one_flushed_ndjson_line_per_request` | **PASS** |
| MIG-115 | 单stdout reader | `src/rpc.rs:RpcProcess` | `src/rpc.rs:reader_emits_frames_in_arrival_order_with_preserved_ids` | **PASS** |
| MIG-116 | stderr有界 | `src/rpc.rs:RpcProcess` | `src/rpc.rs:stderr_reader_emits_bounded_utf8_safe_lines` | **PASS** |
| MIG-117 | 32MiB frame上限 | `src/rpc.rs:MAX_RPC_FRAME_BYTES` | `src/rpc.rs:reader_accepts_exactly_max_size_frames` | **PASS** |
| MIG-118 | Terminal正常恢复 | `src/terminal.rs:TerminalGuard` | `src/terminal.rs:restore_runs_commands_in_reverse_order` | **PASS** |
| MIG-119 | Panic best-effort恢复 | `src/terminal.rs:TerminalGuard::enter` | `tests/terminal_restore.rs:panic_hook_normal_drop_and_nested_guards_restore_safely` | **PASS** |
| MIG-120 | Agent shutdown超时kill | `src/rpc.rs:terminate_with_observer` | `src/main.rs:forced_shutdown_drains_gated_stderr_before_reporting` | **PASS** |
| MIG-121 | Protocol fixtures通过 | `tests/protocol.rs` | `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | **PASS** |
| MIG-122 | App send flow | `src/app.rs` | `tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live` | **PASS** |
| MIG-123 | Multi-request Tool flow | `src/app.rs` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-124 | Steer flow | `src/app.rs` | `tests/agent_e2e.rs:e2e_scenario_d_steer_turn` | **PASS** |
| MIG-125 | Update flow | `src/app.rs` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | **PASS** |
| MIG-126 | Update+Steer flow | `src/app.rs` | `tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation` | **PASS** |
| MIG-127 | Cancel flow | `src/app.rs:cancel_active_turn, src/command.rs:LocalCommand::Cancel` | `tests/app_flow.rs:slash_cancel_sends_exact_turn_cancel_and_wait_reconciles` | **PASS** |
| MIG-128 | Persistence failed flow | `src/app.rs` | `tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view` | **PASS** |
| MIG-129 | Blocked flow | `src/app.rs` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-130 | Event gap flow | `src/app.rs` | `tests/app_flow.rs:tool_events_before_started_are_retained_and_mark_a_gap` | **PASS** |
| MIG-131 | Reopen flow | `src/app.rs` | `src/app.rs:create_session_activates_and_pages_history` | **PASS** |
| MIG-132 | 60x16 snapshot | `src/ui/snapshots.rs` | `src/ui/snapshots.rs:narrow_model_selector_dark_60x16` | **PASS** |
| MIG-133 | 80x24 snapshot | `src/ui/snapshots.rs` | `src/ui/snapshots.rs:chat_dark_80x24` | **PASS** |
| MIG-134 | 120x40 snapshot | `src/ui/snapshots.rs` | `src/ui/snapshots.rs:wide_120x40` | **PASS** |
| MIG-135 | CJK snapshot | `src/ui/snapshots.rs` | `src/ui/snapshots.rs:cjk_80x24` | **PASS** |
| MIG-136 | Agent 0.3 E2E存在 | `tests/agent_e2e.rs` | `tests/agent_e2e.rs:e2e_scenario_a_discovery` | **PASS** |
| MIG-137 | 默认测试离线 | `tests/agent_e2e.rs` | `tests/agent_e2e.rs (#[ignore])` | **PASS** |
| MIG-138 | Linux CI | `.github/workflows/ci.yml` | `GitHub Actions Linux runner` | **NOT RUN** |
| MIG-139 | macOS CI | `.github/workflows/ci.yml` | `GitHub Actions macOS runner` | **NOT RUN** |
| MIG-140 | Windows CI | `.github/workflows/ci.yml` | `GitHub Actions Windows runner` | **NOT RUN** |
| MIG-141 | Agent基线更新至b2e2393…；Runtime仍为87f3cf9…；docs/backend记录实际二进制来源 | `docs/backend.md` | `docs/backend.md (Source Audit & Provenance)` | **PASS** |
| MIG-142 | 版本仍0.3.0，TUI不把ping当成补丁SHA证明，不新增RPC字段 | `src/protocol.rs:PingResult` | `src/protocol.rs:version_gate_is_exactly_0_3_major_minor` | **PASS** |
| MIG-143 | blocked/send2错误不清除L1 TurnRef、wait或临时输出 | `src/app.rs:on_send_failed` | `tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion` | **PASS** |
| MIG-144 | 对保留的blocked Turn重复wait幂等，不重复卡片、Usage或History副作用 | `src/app.rs:on_wait_response` | `tests/app_flow.rs:regression_scenario_c_failed_wait_does_not_reconcile_and_idempotent` | **PASS** |
| MIG-145 | blocked/internal的wait RPC错误不被伪造为正常persistence failed结果 | `src/app.rs:on_wait_response` | `tests/app_flow.rs:regression_scenario_a_wait_internal_error_does_not_loop_history_or_clear_gap` | **PASS** |
| MIG-146 | close返回Internal但已卸载的情形可单次读取核对，不无限重试 | `src/app.rs:on_close_verify_state_response` | `tests/app_flow.rs:close_verification_internal_or_malformed_retains_loaded_state` | **PASS** |
| MIG-147 | shutdown期间持续读取；之前注册wait结果在最终响应前得到处理 | `src/app.rs:request_shutdown` | `tests/app_flow.rs:shutdown_drains_after_child_exit_until_rpc_channel_ends` | **PASS** |
| MIG-148 | close/shutdown取消reason=user被接受，不要求独立shutdown原因 | `src/protocol.rs:CancelledResult` | `tests/agent_e2e.rs:e2e_scenario_f_shutdown_cancels_active_wait` | **PASS** |
| MIG-149 | shutdown ok不掩盖已知persistence failed；强杀标记未确认 | `src/app.rs:shutdown_force_message, src/main.rs:force_kill_and_report, src/rpc.rs:terminate_with_observer` | `src/main.rs:forced_shutdown_drains_gated_stderr_before_reporting` | **PASS** |
| MIG-150 | persisted文案不承诺事务、fsync、掉电或端到端崩溃持久性 | `src/ui/transcript.rs` | `src/ui/snapshots.rs:unsaved_gap_dark_80x24` | **PASS** |
| MIG-151 | failed追加的文件可能无行/片段/完整行；TUI不作固定假设 | `src/app.rs:on_wait_response` | `tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view` | **PASS** |
| MIG-152 | 不完整尾行修复与Active Loop恢复严格区分，TUI不修写Store | `src/app.rs` | `tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view` | **PASS** |
| MIG-153 | 同Loop Model A→Tool→Model B保持session_id/loop_id与requests=2、tool_rounds=1 | `src/app.rs:on_request_started` | `tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b` | **PASS** |
| MIG-154 | RequestStarted先于update响应仍正确确认；旧Request不被改标 | `src/app.rs:on_request_started` | `tests/app_flow.rs:update_request_started_before_update_response_confirms_applied` | **PASS** |
| MIG-155 | Steer无逐条应用回执；后续RequestStarted不统一标全部applied | `src/app.rs:on_request_started` | `tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation` | **PASS** |
| MIG-156 | History从本地连续加载末尾推进，不从尚未加载的total跳页 | `src/app.rs:continue_history_chain` | `tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count` | **PASS** |
| MIG-157 | 坏record列表跳过、显式open报错；健康Session继续可用 | `src/app.rs:on_session_response` | `tests/app_flow.rs:regression_test_close_agent_error_single_state_check_and_store_error` | **PASS** |
| MIG-158 | store_error不被固定诊断成旧格式；不自动补默认值/删Tool/改文件 | `src/app.rs` | `tests/protocol.rs:unknown_reasoning_is_rejected_but_unknown_read_only_fields_are_ignored` | **PASS** |
| MIG-159 | 所有gate与子进程等待有测试超时和回收；不复制上游私有故障RPC | `tests/agent_e2e.rs, src/rpc.rs` | `tests/agent_e2e.rs:e2e_scenario_f_shutdown_cancels_active_wait` | **PASS** |
| MIG-160 | 最新Agent CI和TUI实际测试分别记录；未执行的E2E/Live验证不得写通过 | `docs/acceptance.md, docs/verification.md` | `docs/verification.md (final6 logs; GitHub Actions platform jobs explicitly not run)` | **PASS** |
