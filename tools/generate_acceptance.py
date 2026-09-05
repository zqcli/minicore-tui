import os, sys

spec_file = "minicore-tui-v0.2-agent-v0.3-runtime-v0.4-migration-spec-r2.md"
with open(spec_file, "r", encoding="utf-8") as f:
    lines = f.readlines()

# Extract MIG-001..160 descriptions exactly from Spec 68
mig_spec = {}
for line in lines:
    line = line.strip()
    if line.startswith("| MIG-"):
        parts = [p.strip() for p in line.split("|")[1:-1]]
        if len(parts) >= 2:
            mig_spec[parts[0]] = parts[1]

assert len(mig_spec) == 160, f"Expected 160 specs, got {len(mig_spec)}"

# Mapping dictionary: (source_mapping, test_mapping, status)
# Status is a verification claim: PASS, NOT RUN, or an explicit PARTIAL reason.
matrix = {
    "MIG-001": ("docs/backend.md, Cargo.toml", "docs/backend.md (Source Audit & Provenance)", "PASS"),
    "MIG-002": ("docs/backend.md", "docs/backend.md (Source Audit & Provenance)", "PASS"),
    "MIG-003": ("src/protocol.rs:is_supported_agent_version", "src/protocol.rs:version_gate_is_exactly_0_3_major_minor", "PASS"),
    "MIG-004": ("src/protocol.rs:is_supported_agent_version", "src/protocol.rs:version_gate_is_exactly_0_3_major_minor", "PASS"),
    "MIG-005": ("src/protocol.rs:is_supported_agent_version", "src/protocol.rs:version_gate_is_exactly_0_3_major_minor", "PASS"),
    "MIG-006": ("Cargo.toml", "Cargo.lock (Source Audit)", "PASS"),
    "MIG-007": ("Cargo.toml", "Cargo.lock (Source Audit)", "PASS"),
    "MIG-008": ("src/protocol.rs, src/app.rs", "tests/protocol.rs:unknown_fields_and_usage_defaults_and_outcome_tolerance", "PASS"),
    "MIG-009": ("src/protocol.rs:TurnRef", "tests/protocol.rs:turn_wait_is_a_direct_turn_result_view", "PASS"),
    "MIG-010": ("src/protocol.rs:OutgoingRequest", "tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items", "PASS"),
    "MIG-011": ("src/protocol.rs:IndexedHistoryItemWire", "tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items", "PASS"),
    "MIG-012": ("src/protocol.rs:TurnResultViewWire", "tests/protocol.rs:turn_wait_is_a_direct_turn_result_view", "PASS"),
    "MIG-013": ("src/protocol.rs:SessionStatusWire", "tests/protocol.rs:session_state_uses_an_active_loop_object", "PASS"),
    "MIG-014": ("src/app.rs", "tests/app_flow.rs:regression_test_close_wait_correlation_and_guards", "PASS"),
    "MIG-015": ("src/ui/selector.rs, src/app.rs", "src/ui/selector.rs:model_line_marks_the_current_session_model", "PASS"),
    "MIG-016": ("src/app.rs:steer_turn", "src/app.rs:composer_routes_to_steer_turn_when_session_is_running", "PASS"),
    "MIG-017": ("src/ui/tool.rs", "src/ui/component_tests.rs:tool_preview_caps_chars_at_32k_and_lines_at_40", "PASS"),
    "MIG-018": ("src/protocol.rs:TurnRef", "tests/protocol.rs:turn_wait_is_a_direct_turn_result_view", "PASS"),
    "MIG-019": ("src/protocol.rs:SessionInfo", "tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes", "PASS"),
    "MIG-020": ("src/protocol.rs:SessionStatusWire", "tests/protocol.rs:session_state_uses_an_active_loop_object", "PASS"),
    "MIG-021": ("src/protocol.rs:LoopStateWire", "tests/protocol.rs:session_state_uses_an_active_loop_object", "PASS"),
    "MIG-022": ("src/protocol.rs:SessionUpdateResult", "tests/app_flow.rs:session_update_is_sent_for_an_active_session", "PASS"),
    "MIG-023": ("src/protocol.rs:RequestStartedDataWire", "tests/protocol.rs:event_fixtures_keep_request_index_and_tool_outcome", "PASS"),
    "MIG-024": ("src/protocol.rs:OutputDeltaDataWire", "tests/protocol.rs:event_fixtures_keep_request_index_and_tool_outcome", "PASS"),
    "MIG-025": ("src/protocol.rs:ToolStartedDataWire", "tests/protocol.rs:event_fixtures_keep_request_index_and_tool_outcome", "PASS"),
    "MIG-026": ("src/protocol.rs:HistoryPageWire", "tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items", "PASS"),
    "MIG-027": ("src/protocol.rs:HistoryItemWire", "tests/protocol.rs:history_fixture_decodes_contiguous_indexed_items", "PASS"),
    "MIG-028": ("src/protocol.rs:TurnPersistenceWire", "tests/protocol.rs:turn_wait_is_a_direct_turn_result_view", "PASS"),
    "MIG-029": ("src/protocol.rs:UsageWire", "tests/protocol.rs:unknown_fields_and_usage_defaults_and_outcome_tolerance", "PASS"),
    "MIG-030": ("src/protocol.rs:session_update", "tests/app_flow.rs:session_update_is_sent_for_an_active_session", "PASS"),
    "MIG-031": ("src/protocol.rs:steer_turn", "tests/app_flow.rs:steer_queue_full_retains_composer_input_and_shows_warning", "PASS"),
    "MIG-032": ("src/protocol.rs:RpcError", "src/rpc.rs:fake_unknown_method_reports_a_top_level_jsonrpc_error", "PASS"),
    "MIG-033": ("src/app.rs:continue_history_chain", "tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count", "PASS"),
    "MIG-034": ("src/app.rs:open_session", "tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count", "PASS"),
    "MIG-035": ("src/app.rs:continue_history_chain", "tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count", "PASS"),
    "MIG-036": ("src/ui/user.rs", "src/ui/component_tests.rs:user_card_uses_the_spec_background", "PASS"),
    "MIG-037": ("src/ui/user.rs", "src/ui/snapshots.rs:steering_dark_80x24", "PASS"),
    "MIG-038": ("src/ui/assistant.rs", "tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate", "PASS"),
    "MIG-039": ("src/ui/tool.rs", "src/ui/component_tests.rs:tool_preview_caps_chars_at_32k_and_lines_at_40", "PASS"),
    "MIG-040": ("src/app.rs:on_tool_finished", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-041": ("src/app.rs:on_tool_finished", "src/app.rs:history_merges_user_assistant_tool_summary_without_synthetic_terminals", "PASS"),
    "MIG-042": ("src/ui/transcript.rs", "src/app.rs:history_merges_user_assistant_tool_summary_without_synthetic_terminals", "PASS"),
    "MIG-043": ("src/app.rs:continue_history_chain", "src/app.rs:history_merges_user_assistant_tool_summary_without_synthetic_terminals", "PASS"),
    "MIG-044": ("src/app.rs:reconcile_after_wait", "tests/app_flow.rs:regression_scenario_b_wait_persisted_post_wait_history_and_no_infinite_retry", "PASS"),
    "MIG-045": ("src/app.rs:open_session, src/app.rs:activate_existing_session", "tests/app_flow.rs:loaded_running_session_reopen_reuses_view_after_state_failure", "PASS"),
    "MIG-046": ("src/state/session.rs:SessionView", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-047": ("src/state/turn.rs:LiveLoop", "tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate", "PASS"),
    "MIG-048": ("src/app.rs:on_request_started", "tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate", "PASS"),
    "MIG-049": ("src/app.rs:append_delta", "tests/app_flow.rs:request_index_keeps_multi_request_deltas_separate", "PASS"),
    "MIG-050": ("src/app.rs:on_tool_started", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-051": ("src/state/turn.rs:ensure_request_mut", "tests/app_flow.rs:tool_events_before_started_are_retained_and_mark_a_gap", "PASS"),
    "MIG-052": ("src/app.rs:on_agent_event", "tests/app_flow.rs:loop_events_can_bind_before_turn_send_response", "PASS"),
    "MIG-053": ("src/app.rs:on_send_response", "tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live", "PASS"),
    "MIG-054": ("src/app.rs:on_wait_response", "tests/app_flow.rs:regression_scenario_e_stale_wait_and_history_paging_idempotence", "PASS"),
    "MIG-055": ("src/ui/assistant.rs", "src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved", "PASS"),
    "MIG-056": ("src/app.rs:confirm_model_item", "tests/app_flow.rs:session_update_is_sent_for_an_active_session", "PASS"),
    "MIG-057": ("src/app.rs:confirm_model_item", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-058": ("src/ui/footer.rs", "src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved", "PASS"),
    "MIG-059": ("src/app.rs:on_request_started", "tests/app_flow.rs:update_request_started_before_update_response_confirms_applied", "PASS"),
    "MIG-060": ("src/app.rs:on_tool_started", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-061": ("src/app.rs:on_request_started", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-062": ("src/app.rs:on_update_session_response", "tests/app_flow.rs:session_update_is_sent_for_an_active_session", "PASS"),
    "MIG-063": ("src/app.rs:on_update_session_response", "tests/agent_e2e.rs:e2e_scenario_e2_update_single_request_then_next_turn", "PASS"),
    "MIG-064": ("src/state/session.rs", "tests/app_flow.rs:regression_test_pending_config_update_loop_scoping_and_no_rollback", "PASS"),
    "MIG-065": ("src/app.rs:confirm_model_item", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-066": ("src/app.rs:submit_composer", "src/app.rs:composer_routes_to_steer_turn_when_session_is_running", "PASS"),
    "MIG-067": ("src/app.rs:submit_composer", "src/app.rs:composer_routes_to_steer_turn_when_session_is_running", "PASS"),
    "MIG-068": ("src/app.rs:on_steer_response, src/state/composer.rs:editor_revision", "tests/app_flow.rs:steering_ack_only_clears_the_same_editor_revision", "PASS"),
    "MIG-069": ("src/app.rs:on_steer_response", "src/app.rs:composer_retains_text_on_steer_failure_and_shows_warning", "PASS"),
    "MIG-070": ("src/app.rs:on_steer_response", "tests/app_flow.rs:steer_queue_full_retains_composer_input_and_shows_warning", "PASS"),
    "MIG-071": ("src/state/turn.rs:LiveLoop", "tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation", "PASS"),
    "MIG-072": ("src/app.rs:on_request_started", "tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation", "PASS"),
    "MIG-073": ("src/app.rs:merge_history_items", "tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation", "PASS"),
    "MIG-074": ("src/app.rs:reconcile_after_wait", "tests/app_flow.rs:late_steer_ack_after_complete_history_marks_missing_steer_not_recorded", "PASS"),
    "MIG-075": ("src/app.rs:steer_turn", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-076": ("src/app.rs:steer_turn", "tests/app_flow.rs:regression_test_close_wait_correlation_and_guards", "PASS"),
    "MIG-077": ("src/app.rs:on_agent_event", "tests/agent_e2e.rs:e2e_scenario_d_steer_turn", "PASS"),
    "MIG-078": ("src/app.rs:on_wait_response", "src/app.rs:turn_wait_persistence_failure_latches_blocked_and_creates_unsaved_loop", "PASS"),
    "MIG-079": ("src/app.rs:reconcile_after_wait", "tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live", "PASS"),
    "MIG-080": ("src/app.rs:on_wait_response", "tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view", "PASS"),
    "MIG-081": ("src/app.rs:on_wait_response", "src/app.rs:turn_wait_persistence_failure_latches_blocked_and_creates_unsaved_loop", "PASS"),
    "MIG-082": ("src/ui/transcript.rs", "src/ui/snapshots.rs:unsaved_gap_dark_80x24", "PASS"),
    "MIG-083": ("src/app.rs:on_wait_response", "tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view", "PASS"),
    "MIG-084": ("src/app.rs:submit_composer", "src/app.rs:composer_rejects_submit_when_session_is_blocked", "PASS"),
    "MIG-085": ("src/app.rs:steer_turn", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-086": ("src/app.rs:confirm_model_item", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-087": ("src/app.rs:on_send_failed", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-088": ("src/app.rs:on_wait_response", "tests/app_flow.rs:regression_scenario_c_failed_wait_does_not_reconcile_and_idempotent", "PASS"),
    "MIG-089": ("src/ui/transcript.rs", "src/ui/snapshots.rs:finishing_dark_80x24", "PASS"),
    "MIG-090": ("src/app.rs:on_agent_event", "tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live", "PASS"),
    "MIG-091": ("src/app.rs", "tests/app_flow.rs:regression_test_close_wait_correlation_and_guards", "PASS"),
    "MIG-092": ("src/app.rs:request_shutdown", "tests/app_flow.rs:shutdown_drains_after_child_exit_until_rpc_channel_ends", "PASS"),
    "MIG-093": ("src/app.rs:connection_terminated", "src/ui/component_tests.rs:fatal_connection_renders_the_overlay", "PASS"),
    "MIG-094": ("src/app.rs:open_session", "tests/app_flow.rs:failed_close_reopen_keeps_retired_loop_fenced", "PASS"),
    "MIG-095": ("src/app.rs", "src/ui/snapshots.rs:store_error_dark_80x24", "PASS"),
    "MIG-096": ("src/app.rs", "tests/app_flow.rs:regression_test_close_agent_error_single_state_check_and_store_error", "PASS"),
    "MIG-097": ("src/ui/transcript.rs", "src/ui/mod.rs:dock_is_below_the_transcript_with_a_composer_border", "PASS"),
    "MIG-098": ("src/ui/user.rs", "src/ui/component_tests.rs:user_card_uses_the_spec_background", "PASS"),
    "MIG-099": ("src/ui/user.rs", "src/ui/snapshots.rs:steering_dark_80x24", "PASS"),
    "MIG-100": ("src/ui/assistant.rs", "src/markdown.rs:paragraphs_bold_italic_and_inline_code_are_styled", "PASS"),
    "MIG-101": ("src/ui/assistant.rs", "src/ui/component_tests.rs:reasoning_is_gray_and_italic_and_can_be_hidden", "PASS"),
    "MIG-102": ("src/ui/tool.rs", "src/ui/component_tests.rs:tool_cards_use_state_backgrounds_and_expanded_preview_bounds", "PASS"),
    "MIG-103": ("src/ui/transcript.rs", "src/ui/snapshots.rs:unsaved_gap_dark_80x24", "PASS"),
    "MIG-104": ("src/ui/assistant.rs", "src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved", "PASS"),
    "MIG-105": ("src/ui/composer.rs", "src/ui/component_tests.rs:composer_border_follows_the_reasoning_level", "PASS"),
    "MIG-106": ("src/ui/footer.rs", "src/ui/component_tests.rs:footer_waiting_boundary_shows_next_config_with_current_request_preserved", "PASS"),
    "MIG-107": ("src/ui/footer.rs", "src/ui/component_tests.rs:footer_is_one_row_on_short_terminals", "PASS"),
    "MIG-108": ("src/theme.rs", "src/theme.rs:dark_palette_matches_spec", "PASS"),
    "MIG-109": ("src/theme.rs", "src/theme.rs:light_base_palette_matches_spec", "PASS"),
    "MIG-110": ("src/ui/transcript.rs", "tests/render_snapshots.rs:empty_dark_scene_matches_the_committed_snapshot", "PASS"),
    "MIG-111": ("Cargo.toml", "Cargo.lock", "PASS"),
    "MIG-112": ("Cargo.toml", "Cargo.lock", "PASS"),
    "MIG-113": ("Cargo.toml", "Cargo.lock", "PASS"),
    "MIG-114": ("src/rpc.rs:RpcProcess", "src/rpc.rs:writer_emits_one_flushed_ndjson_line_per_request", "PASS"),
    "MIG-115": ("src/rpc.rs:RpcProcess", "src/rpc.rs:reader_emits_frames_in_arrival_order_with_preserved_ids", "PASS"),
    "MIG-116": ("src/rpc.rs:RpcProcess", "src/rpc.rs:stderr_reader_emits_bounded_utf8_safe_lines", "PASS"),
    "MIG-117": ("src/rpc.rs:MAX_RPC_FRAME_BYTES", "src/rpc.rs:reader_accepts_exactly_max_size_frames", "PASS"),
    "MIG-118": ("src/terminal.rs:TerminalGuard", "src/terminal.rs:restore_runs_commands_in_reverse_order", "PASS"),
    "MIG-119": ("src/terminal.rs:TerminalGuard::enter", "tests/terminal_restore.rs:panic_hook_normal_drop_and_nested_guards_restore_safely", "PASS"),
    "MIG-120": ("src/rpc.rs:terminate_with_observer", "src/main.rs:forced_shutdown_drains_gated_stderr_before_reporting", "PASS"),
    "MIG-121": ("tests/protocol.rs", "tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes", "PASS"),
    "MIG-122": ("src/app.rs", "tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live", "PASS"),
    "MIG-123": ("src/app.rs", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-124": ("src/app.rs", "tests/agent_e2e.rs:e2e_scenario_d_steer_turn", "PASS"),
    "MIG-125": ("src/app.rs", "tests/app_flow.rs:session_update_is_sent_for_an_active_session", "PASS"),
    "MIG-126": ("src/app.rs", "tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation", "PASS"),
    "MIG-127": ("src/app.rs:cancel_active_turn, src/command.rs:LocalCommand::Cancel", "tests/app_flow.rs:slash_cancel_sends_exact_turn_cancel_and_wait_reconciles", "PASS"),
    "MIG-128": ("src/app.rs", "tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view", "PASS"),
    "MIG-129": ("src/app.rs", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-130": ("src/app.rs", "tests/app_flow.rs:tool_events_before_started_are_retained_and_mark_a_gap", "PASS"),
    "MIG-131": ("src/app.rs", "src/app.rs:create_session_activates_and_pages_history", "PASS"),
    "MIG-132": ("src/ui/snapshots.rs", "src/ui/snapshots.rs:narrow_model_selector_dark_60x16", "PASS"),
    "MIG-133": ("src/ui/snapshots.rs", "src/ui/snapshots.rs:chat_dark_80x24", "PASS"),
    "MIG-134": ("src/ui/snapshots.rs", "src/ui/snapshots.rs:wide_120x40", "PASS"),
    "MIG-135": ("src/ui/snapshots.rs", "src/ui/snapshots.rs:cjk_80x24", "PASS"),
    "MIG-136": ("tests/agent_e2e.rs", "tests/agent_e2e.rs:e2e_scenario_a_discovery", "PASS"),
    "MIG-137": ("tests/agent_e2e.rs", "tests/agent_e2e.rs (#[ignore])", "PASS"),
    "MIG-138": (".github/workflows/ci.yml", "GitHub Actions Linux runner", "NOT RUN"),
    "MIG-139": (".github/workflows/ci.yml", "GitHub Actions macOS runner", "NOT RUN"),
    "MIG-140": (".github/workflows/ci.yml", "GitHub Actions Windows runner", "NOT RUN"),
    "MIG-141": ("docs/backend.md", "docs/backend.md (Source Audit & Provenance)", "PASS"),
    "MIG-142": ("src/protocol.rs:PingResult", "src/protocol.rs:version_gate_is_exactly_0_3_major_minor", "PASS"),
    "MIG-143": ("src/app.rs:on_send_failed", "tests/app_flow.rs:blocked_session_forbids_send_steer_update_and_retains_completion", "PASS"),
    "MIG-144": ("src/app.rs:on_wait_response", "tests/app_flow.rs:regression_scenario_c_failed_wait_does_not_reconcile_and_idempotent", "PASS"),
    "MIG-145": ("src/app.rs:on_wait_response", "tests/app_flow.rs:regression_scenario_a_wait_internal_error_does_not_loop_history_or_clear_gap", "PASS"),
    "MIG-146": ("src/app.rs:on_close_verify_state_response", "tests/app_flow.rs:close_verification_internal_or_malformed_retains_loaded_state", "PASS"),
    "MIG-147": ("src/app.rs:request_shutdown", "tests/app_flow.rs:shutdown_drains_after_child_exit_until_rpc_channel_ends", "PASS"),
    "MIG-148": ("src/protocol.rs:CancelledResult", "tests/agent_e2e.rs:e2e_scenario_f_shutdown_cancels_active_wait", "PASS"),
    "MIG-149": ("src/app.rs:shutdown_force_message, src/main.rs:force_kill_and_report, src/rpc.rs:terminate_with_observer", "src/main.rs:forced_shutdown_drains_gated_stderr_before_reporting", "PASS"),
    "MIG-150": ("src/ui/transcript.rs", "src/ui/snapshots.rs:unsaved_gap_dark_80x24", "PASS"),
    "MIG-151": ("src/app.rs:on_wait_response", "tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view", "PASS"),
    "MIG-152": ("src/app.rs", "tests/app_flow.rs:persistence_failure_blocks_without_losing_the_old_result_view", "PASS"),
    "MIG-153": ("src/app.rs:on_request_started", "tests/app_flow.rs:deterministic_same_loop_model_a_to_tool_to_model_b", "PASS"),
    "MIG-154": ("src/app.rs:on_request_started", "tests/app_flow.rs:update_request_started_before_update_response_confirms_applied", "PASS"),
    "MIG-155": ("src/app.rs:on_request_started", "tests/app_flow.rs:update_and_steer_fifo_duplicate_text_history_reconciliation", "PASS"),
    "MIG-156": ("src/app.rs:continue_history_chain", "tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count", "PASS"),
    "MIG-157": ("src/app.rs:on_session_response", "tests/app_flow.rs:regression_test_close_agent_error_single_state_check_and_store_error", "PASS"),
    "MIG-158": ("src/app.rs", "tests/protocol.rs:unknown_reasoning_is_rejected_but_unknown_read_only_fields_are_ignored", "PASS"),
    "MIG-159": ("tests/agent_e2e.rs, src/rpc.rs", "tests/agent_e2e.rs:e2e_scenario_f_shutdown_cancels_active_wait", "PASS"),
    "MIG-160": ("docs/acceptance.md, docs/verification.md", "docs/verification.md (final6 logs; GitHub Actions platform jobs explicitly not run)", "PASS"),
}

# All cited `src/app.rs` tests are ordinary `#[cfg(test)]` tests and are
# included in the remote all-targets result. Keep their executable status.
# Verify every single item in matrix
failed_symbols = []
for mig_id, (source_map, test_map, status) in matrix.items():
    for m in [source_map, test_map]:
        parts = [p.strip() for p in m.split(",") if p.strip()]
        for p in parts:
            if mig_id in ("MIG-138", "MIG-139", "MIG-140") and m == test_map:
                continue
            if "(Source Audit" in p:
                clean_path = p.split("(")[0].strip()
                if not os.path.exists(clean_path):
                    failed_symbols.append((mig_id, clean_path, "path not found"))
                continue
            if "Cargo.lock" in p or "Cargo.toml" in p or ".github" in p or "docs/" in p or "snapshots/" in p or "*.json" in p or "(#[ignore])" in p:
                clean_path = p.split(":")[0].split(" ")[0].strip()
                if not os.path.exists(clean_path) and not (
                    mig_id in {"MIG-138", "MIG-139", "MIG-140"}
                    and ".github/workflows/ci.yml" in clean_path
                ):
                    failed_symbols.append((mig_id, clean_path, "path not found"))
                continue
            if ":" in p:
                fpath, sym = p.split(":", 1)
                fpath = fpath.strip()
                sym = sym.strip().split(" ")[0]
                if not os.path.exists(fpath):
                    failed_symbols.append((mig_id, fpath, "path not found"))
                    continue
                with open(fpath, "r", encoding="utf-8") as f:
                    c = f.read()
                if sym not in c:
                    failed_symbols.append((mig_id, sym, f"symbol not found in {fpath}"))
            else:
                fpath = p.strip().split(" ")[0]
                if not os.path.exists(fpath):
                    failed_symbols.append((mig_id, fpath, "path not found"))

if failed_symbols:
    print(f"Validation FAILED on {len(failed_symbols)} items:")
    for item in failed_symbols:
        print(" ", item)
    sys.exit(1)

print("ALL 160 items mapped to existing source/spec entries successfully!")

# Write docs/acceptance.md
output = []
output.append("# r2 Acceptance Status & MIG-001..160 Verification Matrix\n\n")
output.append("This matrix maps every MIG-001..160 criterion to source and executable evidence.\n")
output.append("`PASS` requires the cited executable test or exact dependency/source check to have passed in the\n")
output.append("remote final6 runs. Pins, dependency absence, and evidence-recording requirements use source audits,\n")
output.append("not invented runtime tests. `NOT RUN` means the criterion was not executed; a remote Linux run never substitutes\n")
output.append("for an unrun GitHub Actions platform job. The implementer did not run local Cargo/Rust tools.\n\n")

output.append("## Remote Final6/Post-review Evidence\n\n")
output.append("All commands ran in `/root/minicore-tui-r2-01a06ec1/tui` on `192.168.20.199`; final6 logs are under\n")
output.append("`/root/minicore-tui-r2-01a06ec1/logs/final6-*`.\n\n")
output.append("- `cargo +1.85.0 test --locked --all-targets`: **273 passed, 0 failed, 8 ignored** (197 lib, 8 main, 49 app_flow, 8 protocol, 4 render_snapshots, 2 rpc_io, 5 terminal_restore; 7 ignored Agent E2E and 1 ignored real-PTY test).\n")
output.append("- `cargo +stable test --locked --all-targets`: **273 passed, 0 failed, 8 ignored**.\n")
output.append("- `MINICORE_AGENT_BIN=../agent/target/debug/minicore-agent cargo +1.85.0 test --locked --test agent_e2e -- --ignored`: **7 passed, 0 failed** (A, B, C, D, E, E2, F; Agent SHA recorded separately).\n")
output.append("- `MCT_UPDATE_SNAPSHOTS=1 cargo +stable test --lib ui::snapshots`: **47 passed, 0 failed**; generated snapshots were then checked by both full suites.\n")
output.append("- `script -q -e -c 'cargo +1.85.0 test --locked --test terminal_restore -- --ignored --nocapture'`: **1 passed, 0 failed** under a remote PTY.\n")
output.append("- `cargo +stable test --release --lib version_gate`: **2 passed, 0 failed**; the prerelease expectation is conditional on `cfg!(debug_assertions)`.\n")
output.append("- `cargo +stable fmt --all -- --check`, `cargo +1.85.0 fmt --all -- --check`, clippy with `-D warnings`, and rustdoc with `RUSTDOCFLAGS=-D warnings`: **all passed**.\n")
output.append("- `cargo +stable tree -d` and `cargo +stable tree -p crossterm`: **passed**; dependency tree shows ratatui 0.29.0 and crossterm 0.28.1.\n")
output.append("- GitHub Actions Linux, macOS, and Windows jobs were not run in final6; remote Linux and cross-target compilation are separate evidence and are not substituted for CI.\n\n")

from collections import Counter
counts = Counter(status for _, _, status in matrix.values())
partial_ids = [mig_id for mig_id, (_, _, status) in matrix.items() if status.startswith("PARTIAL")]
not_run_ids = [mig_id for mig_id, (_, _, status) in matrix.items() if status == "NOT RUN"]
output.append("## Status Breakdown\n\n")
output.append(f"- **PASS**: {counts['PASS']} criteria supported by final6 execution, exact dependency checks, or source/provenance audits appropriate to the criterion.\n")
output.append("- Source-audit criteria MIG-001, MIG-002, MIG-006, MIG-007, MIG-141, and MIG-160 were independently checked by the parent; these do not claim runtime SHA attestation or platform CI execution.\n")
output.append(f"- **NOT RUN**: {counts['NOT RUN']} criteria (`{', '.join(not_run_ids)}`); platform CI status is not substituted by remote Linux execution.\n\n")

output.append("## MIG-001..160 Acceptance Matrix\n\n")
output.append("| ID | Specification Item (Spec 68) | Source Mapping | Test Mapping | Verification Status |\n")
output.append("|---|---|---|---|---|\n")

for i in range(1, 161):
    mig_id = f"MIG-{i:03d}"
    spec_desc = mig_spec[mig_id]
    source_map, test_map, status = matrix[mig_id]
    output.append(f"| {mig_id} | {spec_desc} | `{source_map}` | `{test_map}` | **{status}** |\n")

with open("docs/acceptance.md", "w", encoding="utf-8") as f:
    f.writelines(output)

print("docs/acceptance.md generated successfully.")
