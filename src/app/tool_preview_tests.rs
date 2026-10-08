//! Optional argument generation is presentation-only and identity fenced.
use super::*;
use crate::protocol::{ToolArgumentsPreviewStateWire as PreviewState, ToolRefWire};
use crate::state::view::SectionKind;
use crate::ui::{testapp, transcript};
use serde_json::{Value, json};

fn fixture() -> (App, ToolKey) {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let view = app.active_session_mut().unwrap();
    let live = view.live.as_mut().unwrap();
    for request in &mut live.requests {
        request.parts.clear();
        request.tools.clear();
    }
    Arc::make_mut(&mut view.tool_presentations).clear();
    view.arguments_preview_fence = None;
    app.terminal_size = (80, 24);
    (app, ToolKey::new("ses_1", "loop_live", 0, "write-call"))
}

fn event(key: &ToolKey, attempt: u64, revision: u64, state: &str, name: &str, body: &str) -> Value {
    json!({"type":"tool_arguments_preview","data":{
        "turn":{"session_id":key.session_id,"loop_id":key.loop_id},"request_index":key.request_index,
        "tool_call_id":key.tool_call_id,"tool_name":name,"attempt":attempt,"revision":revision,
        "state":state,"partial":false,"display":{"detail":"src/example.rs:4-8","expanded_input":body},
        "meta":{"session_id":key.session_id,"loop_id":key.loop_id,"dropped_before":0}}})
}

fn notify(app: &mut App, event: Value) -> Vec<AppCommand> {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(serde_json::from_value(event).unwrap()),
    ))))
}

fn preview(app: &mut App, key: &ToolKey, attempt: u64, revision: u64, state: &str, body: &str) {
    let commands = notify(app, event(key, attempt, revision, state, "write", body));
    assert_no_tool_reads(commands);
}

fn assert_no_tool_reads(commands: Vec<AppCommand>) {
    for request in testapp::take_requests(commands) {
        assert!(!matches!(request.method, "tool.read" | "tool.output"));
    }
}

fn text(app: &App, width: u16) -> String {
    transcript::prepare_conversation(app, width)
        .lines()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn count_tools(app: &App) -> usize {
    transcript::prepare_conversation(app, 80)
        .sections
        .iter()
        .filter(|section| section.id.kind == SectionKind::Tool)
        .count()
}

fn toggle(app: &mut App, key: &ToolKey) {
    assert_no_tool_reads(app.update(AppEvent::ToggleTool {
        session_id: key.session_id.clone(),
        loop_id: key.loop_id.clone(),
        request_index: key.request_index,
        tool_call_id: key.tool_call_id.clone(),
    }));
}

fn authority(key: &ToolKey, kind: &str) -> Value {
    let turn = json!({"session_id":key.session_id,"loop_id":key.loop_id});
    let meta = json!({"session_id":key.session_id,"dropped_before":0});
    match kind {
        "tool_invocation" => json!({"type":kind,"data":{"turn":turn,"meta":meta,"data":{
            "tool_ref":ToolRefWire::from(key),"name":"write","subject":{"kind":"file","path":"real.rs"},"subject_truncated":false,
            "input":{"total_bytes":2,"preview":"{}","truncated":false,"encoding":"utf8_json"}}}}),
        "tool_execution" => {
            let mut value: Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/agent-v1/tool-read-terminal.json"
            ))
            .unwrap();
            let mut execution = value["result"]["execution"].take();
            execution["tool_ref"] = json!(ToolRefWire::from(key));
            execution["name"] = json!("write");
            execution["state"] = json!("running");
            execution["outcome"] = Value::Null;
            json!({"type":kind,"data":{"turn":turn,"meta":meta,"data":execution}})
        }
        "tool_presentation" => {
            json!({"type":kind,"data":{"turn":turn,"meta":meta,"request_index":key.request_index,
            "tool_call_id":key.tool_call_id,"tool_name":"write","display":{"detail":"real.rs","expanded_input":"real body"}}})
        }
        _ => {
            json!({"type":"tool_started","data":{"turn":turn,"meta":meta,"request_index":key.request_index,
            "tool_call_id":key.tool_call_id,"tool_name":"write"}})
        }
    }
}

#[test]
fn snapshots_create_without_start_and_replace_after_loss_or_reordering() {
    let (mut app, key) = fixture();
    app.active_session_mut()
        .unwrap()
        .live
        .as_mut()
        .unwrap()
        .requests
        .clear();
    preview(&mut app, &key, 4, 8, "generating", "first");
    assert_eq!(count_tools(&app), 1);
    preview(&mut app, &key, 4, 8, "generating", "duplicate wrong");
    preview(&mut app, &key, 4, 7, "generating", "old wrong");
    assert_eq!(
        app.active_view().unwrap().tool_presentations[&key]
            .display
            .expanded_input
            .as_deref(),
        Some("first")
    );
    let mut snapshot = event(&key, 4, 11, "generating", "write", "complete replacement");
    snapshot["data"]["meta"]["dropped_before"] = json!(7);
    assert_no_tool_reads(notify(&mut app, snapshot));
    assert_eq!(
        app.active_view().unwrap().tool_presentations[&key]
            .display
            .expanded_input
            .as_deref(),
        Some("complete replacement")
    );
    assert_eq!(count_tools(&app), 1);
    assert!(text(&app, 80).contains("Generating arguments"));
    let facts = &app.active_view().unwrap().tool_presentations[&key];
    assert_eq!(facts.status, ToolStatus::Pending);
    assert!(facts.invocation.is_none() && facts.execution.is_none() && facts.timing.is_none());
}

#[test]
fn generated_is_still_unvalidated_and_discard_is_sticky_within_attempt() {
    let (mut app, key) = fixture();
    preview(&mut app, &key, 1, 1, "generating", "half");
    preview(&mut app, &key, 1, 2, "generated", "final");
    assert!(text(&app, 80).contains("Arguments generated"));
    assert!(!text(&app, 80).contains("completed"));
    assert_eq!(
        app.active_view().unwrap().tool_presentations[&key].status,
        ToolStatus::Pending
    );
    preview(&mut app, &key, 1, 3, "generating", "late generating");
    assert_eq!(
        app.active_view().unwrap().tool_presentations[&key]
            .display
            .expanded_input
            .as_deref(),
        Some("final")
    );
    preview(&mut app, &key, 1, 4, "discarded", "");
    preview(&mut app, &key, 1, 5, "generated", "cannot revive");
    assert_eq!(count_tools(&app), 0);
    assert!(
        !app.active_view()
            .unwrap()
            .tool_presentations
            .contains_key(&key)
    );
    preview(&mut app, &key, 2, 1, "generating", "new attempt");
    assert_eq!(count_tools(&app), 1);
}

#[test]
fn new_attempt_retires_every_call_even_when_retry_uses_different_ids() {
    let (mut app, key) = fixture();
    let other = ToolKey::new(&key.session_id, &key.loop_id, 0, "another-call");
    preview(&mut app, &key, 9, 99, "generating", "old one");
    preview(&mut app, &other, 9, 99, "generated", "old two");
    let new = ToolKey::new(&key.session_id, &key.loop_id, 0, "retry-call");
    preview(&mut app, &new, 10, 1, "generating", "fresh");
    assert_eq!(count_tools(&app), 1);
    for old in [&key, &other] {
        assert!(
            !app.active_view()
                .unwrap()
                .tool_presentations
                .contains_key(old)
        );
        preview(&mut app, old, 9, 100, "generating", "late previous attempt");
    }
    assert_eq!(count_tools(&app), 1);
    assert_eq!(
        app.active_view()
            .unwrap()
            .arguments_preview_fence
            .as_ref()
            .unwrap()
            .calls
            .len(),
        1
    );
}

#[test]
fn every_authoritative_event_upgrades_in_place_and_rejects_later_previews() {
    for kind in [
        "tool_invocation",
        "tool_started",
        "tool_execution",
        "tool_presentation",
    ] {
        for before in [false, true] {
            let (mut app, key) = fixture();
            if before {
                notify(&mut app, authority(&key, kind));
            }
            preview(&mut app, &key, 1, 1, "generating", "SPECULATIVE BODY");
            if !before {
                notify(&mut app, authority(&key, kind));
            }
            preview(
                &mut app,
                &key,
                100,
                100,
                "generating",
                "LATE SPECULATIVE BODY",
            );
            let facts = &app.active_view().unwrap().tool_presentations[&key];
            assert!(facts.arguments_preview.is_none(), "{kind}");
            assert!(!text(&app, 80).contains("SPECULATIVE"), "{kind}");
            assert!(count_tools(&app) <= 1, "{kind}");
            if !before {
                assert_eq!(count_tools(&app), 1);
            }
        }
    }
}

#[test]
fn whitelist_identity_and_cross_request_boundaries_are_enforced() {
    let (mut app, key) = fixture();
    for name in ["bash", "unknown", "apply_patch"] {
        notify(
            &mut app,
            event(&key, 1, 1, "generating", name, "DO NOT DISPLAY"),
        );
    }
    assert_eq!(count_tools(&app), 0);
    for field in ["session_id", "loop_id"] {
        let mut invalid = event(&key, 1, 1, "generating", "write", "DO NOT DISPLAY");
        invalid["data"]["meta"][field] = json!("different");
        notify(&mut app, invalid);
    }
    assert_eq!(count_tools(&app), 0);
    preview(&mut app, &key, 1, 1, "generating", "old request");
    let next = ToolKey::new(&key.session_id, &key.loop_id, 1, &key.tool_call_id);
    preview(&mut app, &next, 2, 1, "generating", "new request");
    preview(
        &mut app,
        &key,
        99,
        99,
        "generating",
        "old request cannot return",
    );
    assert_eq!(count_tools(&app), 1);
    assert!(
        !app.active_view()
            .unwrap()
            .tool_presentations
            .contains_key(&key)
    );
    assert!(text(&app, 80).contains("new request"));
}

#[test]
fn read_and_edit_display_only_the_path_and_range() {
    for name in ["read", "edit"] {
        let (mut app, key) = fixture();
        notify(
            &mut app,
            event(
                &key,
                1,
                1,
                "generating",
                name,
                "{half JSON or speculative diff}",
            ),
        );
        assert!(text(&app, 80).contains("src/example.rs:4-8"));
        assert!(!text(&app, 80).contains("half JSON"));
        assert!(
            app.active_view().unwrap().tool_presentations[&key]
                .display
                .expanded_input
                .is_none()
        );
        toggle(&mut app, &key);
        assert!(!text(&app, 80).contains("half JSON"));
    }
}

#[test]
fn write_shows_first_ten_logical_lines_and_expands_only_retained_body() {
    let (mut app, key) = fixture();
    let body = (1..=14)
        .map(|n| format!("body-{n:02} {}", "x".repeat(85)))
        .collect::<Vec<_>>()
        .join("\n");
    preview(&mut app, &key, 1, 1, "generating", &body);
    for width in [40, 80, 120] {
        let rendered = text(&app, width);
        assert!(rendered.contains("body-01"));
        assert!(rendered.contains("body-10"));
        assert!(!rendered.contains("body-11"));
        assert!(rendered.contains("4 more lines"));
    }
    toggle(&mut app, &key);
    assert!(text(&app, 80).contains("body-14"));
    let mut snapshot = event(&key, 1, 2, "generated", "write", "retained only");
    snapshot["data"]["display"]["body_truncated"] = json!(true);
    snapshot["data"]["partial"] = json!(true);
    notify(&mut app, snapshot);
    assert!(text(&app, 80).contains("retained only"));
    assert!(text(&app, 80).contains("preview truncated"));
    assert!(!text(&app, 80).contains("body-14"));
    assert_no_tool_reads(app.open_tool_detail(key.clone()));
    assert!(app.tool_detail().is_none());
    assert_no_tool_reads(app.poll_inline_tools());
    assert_no_tool_reads(app.poll_bash_timing_reads());
}

#[test]
fn preview_copy_preserves_source_soft_wraps_and_excludes_status_or_hints() {
    let (mut app, key) = fixture();
    let body = format!("{}\nsecond line", "abcdef 中".repeat(20));
    preview(&mut app, &key, 1, 1, "generating", &body);
    let prepared = transcript::prepare_conversation(&app, 40);
    let section = prepared
        .sections
        .iter()
        .find(|section| section.id.kind == SectionKind::Tool)
        .unwrap();
    let selection = ConversationSelection {
        session_id: key.session_id.clone(),
        anchor: SelectionPoint {
            row: section.rows.start,
            column: 0,
            section_id: Some(section.id.clone()),
            section_row: 0,
        },
        focus: SelectionPoint {
            row: section.rows.end - 1,
            column: 40,
            section_id: Some(section.id.clone()),
            section_row: section.rows.end - section.rows.start - 1,
        },
        granularity: crate::state::view::SelectionGranularity::Character,
        dragged: true,
    };
    let copied = transcript::selection_text(&prepared, &selection);
    assert!(copied.contains(&body), "{copied:?}");
    assert!(!copied.contains("Generating arguments"));
    assert!(!copied.contains("ctrl+o"));
}

#[test]
fn cleanup_on_terminal_cancel_disconnect_switch_and_new_loop_never_resurrects() {
    for mode in [
        "finished",
        "cancel",
        "wait",
        "disconnect",
        "switch",
        "new_loop",
    ] {
        let (mut app, key) = fixture();
        preview(&mut app, &key, 1, 1, "generating", "discard me");
        match mode {
            "finished" => {
                notify(
                    &mut app,
                    json!({"type":"turn_finished","data":{
                "turn":{"session_id":key.session_id,"loop_id":key.loop_id},"outcome":{"type":"completed"},"persistence":"persisted",
                "meta":{"session_id":key.session_id,"dropped_before":0}}}),
                );
            }
            "cancel" => {
                app.active_session_mut()
                    .unwrap()
                    .live
                    .as_mut()
                    .unwrap()
                    .cancel_requested = true;
            }
            "wait" => {
                app.active_session_mut()
                    .unwrap()
                    .live
                    .as_mut()
                    .unwrap()
                    .waiting = true;
            }
            "disconnect" => {
                app.update(AppEvent::Rpc(RpcEvent::ConnectionClosed));
            }
            "switch" => {
                app.retire_session_operations(&key.session_id);
                app.sessions.active = None;
            }
            "new_loop" => {
                app.active_session_mut()
                    .unwrap()
                    .live
                    .as_mut()
                    .unwrap()
                    .reference
                    .as_mut()
                    .unwrap()
                    .loop_id = "new-loop".into();
            }
            _ => unreachable!(),
        }
        app.reconcile_arguments_previews();
        assert!(
            !app.sessions.known[&key.session_id]
                .tool_presentations
                .contains_key(&key),
            "{mode}"
        );
        preview(&mut app, &key, 99, 99, "generated", "late old snapshot");
        assert!(
            !app.sessions.known[&key.session_id]
                .tool_presentations
                .contains_key(&key),
            "{mode}"
        );
    }
}

#[test]
fn terminal_before_first_preview_closes_the_whole_loop() {
    let (mut app, key) = fixture();
    notify(
        &mut app,
        json!({"type":"turn_finished","data":{
        "turn":{"session_id":key.session_id,"loop_id":key.loop_id},"outcome":{"type":"completed"},"persistence":"persisted",
        "meta":{"session_id":key.session_id,"dropped_before":0}}}),
    );
    let later = ToolKey::new(&key.session_id, &key.loop_id, 8, "never-started");
    preview(&mut app, &later, 99, 1, "generated", "must remain absent");
    assert_eq!(count_tools(&app), 0);
}

#[test]
fn request_start_or_missing_start_output_cleans_older_preview_cards() {
    for kind in ["request_started", "output_delta"] {
        let (mut app, key) = fixture();
        preview(&mut app, &key, 1, 1, "generating", "previous request");
        let data = if kind == "request_started" {
            json!({"turn":{"session_id":key.session_id,"loop_id":key.loop_id},"request_index":1,"config_revision":1,"model":"fake","reasoning":"auto","meta":{"session_id":key.session_id,"dropped_before":0}})
        } else {
            json!({"turn":{"session_id":key.session_id,"loop_id":key.loop_id},"request_index":1,"channel":"text","delta":"next request text","meta":{"session_id":key.session_id,"dropped_before":0}})
        };
        notify(&mut app, json!({"type":kind,"data":data}));
        assert_eq!(count_tools(&app), 0);
        preview(&mut app, &key, 99, 99, "generating", "cannot reappear");
        assert_eq!(count_tools(&app), 0);
    }
}

#[test]
fn durable_assistant_replaces_previews_and_prevents_recreation() {
    let (mut app, key) = fixture();
    preview(&mut app, &key, 1, 1, "generated", "SPECULATIVE BODY");
    let orphan = ToolKey::new(&key.session_id, &key.loop_id, 0, "orphan-call");
    preview(&mut app, &orphan, 1, 1, "generated", "ORPHAN BODY");
    let item = crate::protocol::read::decode_item(&json!({"display":true,"item":{"type":"assistant","data":{
        "loop_id":key.loop_id,"request_index":0,"model":"fake","finish_reason":"tool_calls","content":[{"type":"tool_call","data":{"tool_call_id":key.tool_call_id,"name":"write","arguments":{"path":"durable.rs","content":"durable body"},"call_index":0}}]}}}).to_string()).unwrap();
    install_history_item(app.active_session_mut().unwrap(), 1, &item).unwrap();
    assert!(
        !app.active_view()
            .unwrap()
            .tool_presentations
            .contains_key(&orphan)
    );
    assert!(
        app.active_view().unwrap().tool_presentations[&key]
            .arguments_preview
            .is_none()
    );
    preview(&mut app, &key, 100, 1, "generated", "LATE SPECULATIVE BODY");
    assert!(!text(&app, 80).contains("SPECULATIVE"));
    assert_eq!(count_tools(&app), 1);
}

#[test]
fn preview_budgets_bound_bodies_calls_ids_and_repeated_attempt_watermarks() {
    let (mut app, key) = fixture();
    let huge = "🦀".repeat(crate::limits::TOOL_ARGUMENT_PREVIEW_BYTES);
    for n in 0..32 {
        let call = ToolKey::new(&key.session_id, &key.loop_id, 0, &format!("call-{n}"));
        preview(&mut app, &call, 1, 1, "generating", &huge);
    }
    let view = app.active_view().unwrap();
    assert!(view.tool_presentations.len() <= crate::limits::TOOL_ARGUMENT_PREVIEW_CALLS);
    assert!(
        view.tool_presentations
            .values()
            .map(|f| f.retained_bytes())
            .sum::<usize>()
            <= crate::limits::TOOL_ARGUMENT_PREVIEW_TOTAL_BYTES
    );
    assert!(view.tool_presentations.values().all(|f| f.retained_bytes()
        <= crate::limits::TOOL_ARGUMENT_PREVIEW_BYTES
        && f.display.body_truncated));
    assert_eq!(
        view.arguments_preview_fence.as_ref().unwrap().calls.len(),
        crate::limits::TOOL_ARGUMENT_PREVIEW_CALLS
    );
    for attempt in 2..200 {
        let call = ToolKey::new(
            &key.session_id,
            &key.loop_id,
            0,
            &format!("retry-{attempt}"),
        );
        preview(&mut app, &call, attempt, 1, "generating", "tiny");
        preview(&mut app, &call, attempt, 2, "discarded", "");
    }
    let view = app.active_view().unwrap();
    assert!(view.tool_presentations.is_empty());
    assert_eq!(
        view.arguments_preview_fence.as_ref().unwrap().calls.len(),
        1
    );
    assert_eq!(
        view.arguments_preview_fence
            .as_ref()
            .unwrap()
            .calls
            .values()
            .next()
            .unwrap()
            .state,
        PreviewState::Discarded
    );
    let invalid = ToolKey::new(
        &key.session_id,
        &key.loop_id,
        0,
        &"x".repeat(crate::limits::TOOL_ARGUMENT_PREVIEW_ID_BYTES + 1),
    );
    preview(
        &mut app,
        &invalid,
        999,
        1,
        "generating",
        "must not retain id",
    );
    assert!(app.active_view().unwrap().tool_presentations.is_empty());
}

#[test]
fn explicit_budget_eviction_keeps_tombstone_until_a_new_attempt() {
    let (mut app, key) = fixture();
    preview(&mut app, &key, 1, 1, "generating", "evict this");
    App::remove_arguments_preview(app.active_session_mut().unwrap(), &key);
    preview(&mut app, &key, 1, 2, "generated", "stale restore");
    assert_eq!(count_tools(&app), 0);
    preview(&mut app, &key, 2, 1, "generating", "new attempt");
    assert_eq!(count_tools(&app), 1);
}

#[test]
fn first_snapshot_binds_pending_turn_without_any_start_event() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "auto");
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "synthetic request".into(),
    });
    assert!(
        app.active_view()
            .unwrap()
            .live
            .as_ref()
            .unwrap()
            .reference
            .is_none()
    );
    let key = ToolKey::new("ses_1", "loop_no_start", 0, "write-call");
    preview(
        &mut app,
        &key,
        5,
        3,
        "generating",
        "replacement without start",
    );
    assert_eq!(count_tools(&app), 1);
    assert_eq!(
        app.active_view()
            .unwrap()
            .live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id,
        key.loop_id
    );
}

#[test]
fn tool_budget_pressure_releases_preview_owners_and_keeps_bounded_tombstone() {
    let (mut app, key) = fixture();
    preview(&mut app, &key, 1, 1, "generating", "speculative body");
    let block: Arc<str> = Arc::from("x".repeat(crate::limits::TOOL_STREAM_BYTES));
    let view = app.active_session_mut().unwrap();
    for i in 0..17 {
        let mut facts = crate::state::tool::ToolFacts::new("write");
        facts.result = Some(Arc::clone(&block));
        Arc::make_mut(&mut view.tool_presentations)
            .insert(ToolKey::new("ses_1", "settled", i, "real"), Arc::new(facts));
    }
    app.enforce_tool_budget();
    assert!(
        !app.active_view()
            .unwrap()
            .tool_presentations
            .contains_key(&key)
    );
    assert_eq!(count_tools(&app), 0);
    preview(&mut app, &key, 1, 2, "generated", "must stay evicted");
    assert!(
        !app.active_view()
            .unwrap()
            .tool_presentations
            .contains_key(&key)
    );
    assert_eq!(
        app.active_view()
            .unwrap()
            .arguments_preview_fence
            .as_ref()
            .unwrap()
            .calls
            .len(),
        1
    );
}

#[test]
fn durable_summary_upgrade_releases_the_live_preview_body_arc() {
    let (mut app, key) = fixture();
    preview(&mut app, &key, 1, 1, "generated", "temporary body");
    let old_display = Arc::downgrade(&app.active_view().unwrap().tool_presentations[&key].display);
    let item = crate::protocol::read::decode_item(&json!({"display":true,"item":{"type":"tool_result","data":{
        "loop_id":key.loop_id,"request_index":0,"call_id":key.tool_call_id,"tool_name":"write","outcome":"success"}},
        "tool_summaries":[{"tool_ref":ToolRefWire::from(&key),"tool_call_id":key.tool_call_id,"name":"write",
            "display":{"detail":"durable.rs","input_line_count":1},"output_line_count":0,"output_truncated":false,"count_state":"exact"}]}).to_string()).unwrap();
    install_history_item(app.active_session_mut().unwrap(), 1, &item).unwrap();
    assert!(old_display.upgrade().is_none());
    assert!(
        app.active_view().unwrap().tool_presentations[&key]
            .arguments_preview
            .is_none()
    );
}

#[test]
fn matching_session_terminal_notices_clear_previews_but_stale_loop_notices_do_not() {
    for status in ["closed", "idle", "blocked", "finishing"] {
        let (mut app, key) = fixture();
        preview(&mut app, &key, 1, 1, "generating", "retire this body");
        let notification = |loop_id: &str| {
            if status == "closed" {
                json!({"type":"session_closed","data":{"session_id":key.session_id,"meta":{"session_id":key.session_id,"loop_id":loop_id,"dropped_before":0}}})
            } else {
                json!({"type":"session_state","data":{"state":{"session_id":key.session_id,"status":status,
                    "active_loop":if status == "finishing" { json!({"loop_id":key.loop_id,"status":"finishing","request_index":0,"config_revision":0,"model":null,"pending_interaction":null}) } else { Value::Null },
                    "block_reason":Value::Null},"meta":{"session_id":key.session_id,"loop_id":loop_id,"dropped_before":0}}})
            }
        };
        notify(&mut app, notification("older-loop"));
        assert!(
            app.active_view()
                .unwrap()
                .tool_presentations
                .contains_key(&key),
            "{status}"
        );
        notify(&mut app, notification(&key.loop_id));
        assert!(
            !app.active_view()
                .unwrap()
                .tool_presentations
                .contains_key(&key),
            "{status}"
        );
        preview(
            &mut app,
            &key,
            2,
            1,
            "generated",
            "must not revive after session stop",
        );
        assert!(
            !app.active_view()
                .unwrap()
                .tool_presentations
                .contains_key(&key)
        );
    }
}
