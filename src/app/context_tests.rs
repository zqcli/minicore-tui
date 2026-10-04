use super::*;
use crate::{
    state::panels::MainView,
    ui::testapp::{self, respond, take_requests},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
fn app() -> App {
    testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high")
}
fn fixture(name: &str) -> Value {
    let mut v = serde_json::from_str::<Value>(
        &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{name}.json")).unwrap(),
    )
    .unwrap()["result"]
        .clone();
    v["session_id"] = "ses_1".into();
    v
}
fn clock(a: &mut App) -> Arc<AtomicU64> {
    let elapsed = Arc::new(AtomicU64::new(0));
    let copy = elapsed.clone();
    let base = Instant::now();
    a.monotonic_now = Arc::new(move || base + Duration::from_millis(copy.load(Ordering::Relaxed)));
    elapsed
}
#[test]
fn context_idle_and_closed_panel_stay_silent_and_late_only_releases_slot() {
    let mut a = app();
    let time = clock(&mut a);
    let r = take_requests(a.open_context()).remove(0);
    respond(&mut a, &r, fixture("session-context-idle"));
    time.store(5000, Ordering::Relaxed);
    assert!(a.update(AppEvent::Tick).is_empty());
    let saved = a.active_view().unwrap().context.clone();
    let r = take_requests(a.refresh_context_panel()).remove(0);
    a.close_main_detail();
    assert_eq!(a.queries.in_flight_len(), 1);
    respond(&mut a, &r, fixture("session-context-preparing"));
    assert_eq!(a.active_view().unwrap().context, saved);
    assert!(a.queries.is_empty());
    assert!(a.context_reads.is_empty());
    let r = take_requests(a.open_context()).remove(0);
    assert!(respond(&mut a, &r, fixture("session-context-preparing")).is_empty());
    assert!(a.context_reads.is_empty());
    let operation = a.context_cancel_target().unwrap();
    a.close_main_detail();
    assert!(
        a.context_reads.is_empty(),
        "an observed operation retains its identity without recurring read intent"
    );
    assert!(a.active_view().unwrap().context.is_some());
    assert!(a.active_view().unwrap().is_preparing());
    for elapsed in [5500, 7000, 60_000] {
        time.store(elapsed, Ordering::Relaxed);
        assert!(a.update(AppEvent::Tick).is_empty());
        assert!(a.context_reads.is_empty());
        assert!(a.active_view().unwrap().is_preparing());
    }
    // A missed final event followed by silence does not repair itself. A
    // deliberate reopen reads once and keeps exact cancellation available.
    let context = take_requests(a.open_context()).remove(0);
    assert_eq!(context.method, "session.context");
    assert_eq!(a.context_cancel_target(), Some(operation));
    respond(&mut a, &context, fixture("session-context-idle"));
    assert!(a.context_reads.is_empty());
    assert!(!a.active_view().unwrap().is_preparing());
}
#[test]
fn context_active_foreground_and_background_ticks_stay_silent_until_refresh() {
    let mut a = app();
    let time = clock(&mut a);
    let r = take_requests(a.open_context()).remove(0);
    respond(&mut a, &r, fixture("session-context-idle"));
    let requests = take_requests(a.start_manual_compact());
    let compact = requests
        .iter()
        .find(|r| r.method == "session.compact")
        .unwrap();
    let context = requests
        .iter()
        .find(|r| r.method == "session.context")
        .unwrap();
    let mut preparing = fixture("session-context-preparing");
    preparing["current_operation"]["operation_id"] = compact.params["operation_id"].clone();
    assert!(respond(&mut a, context, preparing.clone()).is_empty());
    assert!(a.context_reads.is_empty());
    for elapsed in [499, 500, 2000, 60_000] {
        time.store(elapsed, Ordering::Relaxed);
        assert!(a.update(AppEvent::Tick).is_empty());
        assert!(a.context_reads.is_empty());
    }
    a.set_active_session(None);
    for elapsed in [60_499, 60_500, 61_999, 62_000, 120_000] {
        time.store(elapsed, Ordering::Relaxed);
        assert!(a.update(AppEvent::Tick).is_empty());
        assert!(a.context_reads.is_empty());
    }
    assert!(a.sessions.known.get("ses_1").unwrap().is_preparing());
    a.set_active_session(Some("ses_1".into()));
    let reopened = take_requests(a.open_context());
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].method, "session.context");
    assert!(respond(&mut a, &reopened[0], preparing.clone()).is_empty());
    let refreshed = take_requests(a.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(5),
            crossterm::event::KeyModifiers::NONE,
        ),
    ))));
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].method, "session.context");
    assert!(respond(&mut a, &refreshed[0], preparing).is_empty());
    assert!(a.context_reads.is_empty());
    let refresh = take_requests(respond(
        &mut a,
        compact,
        json!({"operation_id":compact.params["operation_id"],"status":"noop"}),
    ))
    .remove(0);
    respond(&mut a, &refresh, fixture("session-context-idle"));
    assert!(a.context_reads.is_empty());
}
#[test]
fn context_compact_outcomes_preserve_history_and_confirm_unknown_write() {
    for status in ["compacted", "noop", "failed", "unknown_write"] {
        let mut a = app();
        let r = take_requests(a.open_context()).remove(0);
        respond(&mut a, &r, fixture("session-context-idle"));
        let revision = a.active_view().unwrap().transcript.render_revision;
        if let MainView::Context(c) = &mut a.main_view {
            c.action = 1;
        }
        let rs = take_requests(a.context_action());
        let compact = rs.iter().find(|r| r.method == "session.compact").unwrap();
        let context = rs.iter().find(|r| r.method == "session.context").unwrap();
        let op = compact.params["operation_id"].clone();
        respond(&mut a, context, fixture("session-context-idle"));
        let rs = take_requests(respond(
            &mut a,
            compact,
            json!({"operation_id":op,"status":status,"before_tokens":null,"after_tokens":null,"utility_usage":null}),
        ));
        assert!(rs.iter().all(|r| r.method != "session.read"));
        assert_eq!(
            a.active_view().unwrap().transcript.render_revision,
            revision
        );
        if status == "unknown_write" {
            assert!(!a.can_manual_compact());
            assert!(a.open_context().is_empty());
            a.close_main_detail(); // B's confirmation read must outlive this view.
            let state = rs.iter().find(|r| r.method == "session.state").unwrap();
            respond(
                &mut a,
                state,
                json!({"session_id":"ses_1","status":"idle","active_loop":null,"block_reason":null}),
            );
            assert!(a.active_view().unwrap().manual_compact.is_some());
        }
        let context = rs.iter().find(|r| r.method == "session.context").unwrap();
        let next = take_requests(respond(&mut a, context, fixture("session-context-idle")));
        if status == "unknown_write" {
            let compact = a.active_view().unwrap().manual_compact.as_ref().unwrap();
            assert!(compact.state_refresh_confirmed);
            assert!(
                !compact.context_refresh_confirmed,
                "reopening Context makes the older read stale, not a fresh confirmation"
            );
            assert!(!a.can_manual_compact());
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].method, "session.context");
            assert_ne!(next[0].id, context.id);
            assert!(respond(&mut a, &next[0], fixture("session-context-idle")).is_empty());
        } else {
            assert!(next.is_empty());
        }
        assert!(a.context_reads.is_empty());
        assert!(a.can_manual_compact());
        if status == "unknown_write" {
            assert!(a.active_view().unwrap().manual_compact.is_none());
        }
    }
}
#[test]
fn context_cancel_is_exact_and_closing_does_not_cancel() {
    let mut a = app();
    let r = take_requests(a.open_context()).remove(0);
    respond(&mut a, &r, fixture("session-context-idle"));
    if let MainView::Context(c) = &mut a.main_view {
        c.action = 1;
    }
    let rs = take_requests(a.context_action());
    let op = rs
        .iter()
        .find(|r| r.method == "session.compact")
        .unwrap()
        .params["operation_id"]
        .clone();
    let context = rs.iter().find(|r| r.method == "session.context").unwrap();
    let mut preparing = fixture("session-context-preparing");
    preparing["current_operation"]["operation_id"] = op.clone();
    assert!(respond(&mut a, context, preparing.clone()).is_empty());
    assert!(a.context_reads.is_empty());
    if let MainView::Context(c) = &mut a.main_view {
        c.action = 2;
    }
    let cancel = take_requests(a.context_action());
    assert_eq!(cancel.len(), 1);
    assert_eq!(cancel[0].method, "session.compact.cancel");
    assert_eq!(cancel[0].params["operation_id"], op);
    assert_eq!(
        a.active_view()
            .unwrap()
            .compaction_cancel_requested
            .as_deref(),
        op.as_str()
    );
    let confirmation = take_requests(respond(&mut a, &cancel[0], json!({"cancelled":true})));
    assert_eq!(confirmation.len(), 1);
    assert_eq!(confirmation[0].method, "session.context");
    let commands = take_requests(respond(&mut a, &confirmation[0], preparing));
    assert!(
        commands.is_empty(),
        "an accepted manual cancel with a still-active snapshot must not resend cancel or context"
    );
    assert!(a.context_reads.is_empty());
    assert_eq!(
        a.active_view()
            .unwrap()
            .compaction_cancel_requested
            .as_deref(),
        op.as_str()
    );
    a.close_main_detail();
    assert!(a.update(AppEvent::Tick).is_empty());
    assert!(
        a.active_view()
            .unwrap()
            .manual_compact
            .as_ref()
            .unwrap()
            .result
            .is_none()
    );
}
#[test]
fn context_cancel_intent_outlives_its_one_shot_read_and_panel() {
    let mut a = app();
    let time = clock(&mut a);
    let context = take_requests(a.open_context()).remove(0);
    let preparing = fixture("session-context-preparing");
    let operation = preparing["current_operation"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(respond(&mut a, &context, preparing.clone()).is_empty());
    assert!(a.context_reads.is_empty());
    if let MainView::Context(c) = &mut a.main_view {
        c.action = 2;
    }
    let cancel = take_requests(a.context_action()).remove(0);
    assert_eq!(cancel.method, "session.compact.cancel");
    assert_eq!(cancel.params["operation_id"], operation);
    assert_eq!(
        a.active_view()
            .unwrap()
            .compaction_cancel_requested
            .as_deref(),
        Some(operation.as_str())
    );
    let confirmation = take_requests(respond(&mut a, &cancel, json!({"cancelled":true})));
    assert_eq!(confirmation.len(), 1);
    assert_eq!(confirmation[0].method, "session.context");
    assert!(respond(&mut a, &confirmation[0], preparing).is_empty());
    assert!(a.context_reads.is_empty());
    assert!(a.compaction_cancelling("ses_1", &operation));
    a.close_main_detail();
    time.store(60_000, Ordering::Relaxed);
    assert!(a.update(AppEvent::Tick).is_empty());
    assert!(a.compaction_cancelling("ses_1", &operation));
    assert!(a.active_view().unwrap().is_preparing());
    let refreshed = take_requests(a.open_context()).remove(0);
    respond(&mut a, &refreshed, fixture("session-context-idle"));
    assert!(a.context_reads.is_empty());
    assert!(!a.compaction_cancelling("ses_1", &operation));
    assert!(!a.active_view().unwrap().is_preparing());
}
#[test]
fn context_method_not_found_disables_only_unsupported_actions_without_fallback() {
    let mut a = app();
    let r = take_requests(a.open_context()).remove(0);
    testapp::respond_rpc_error(&mut a, &r, -32601, "no method");
    assert!(!a.context_supported);
    assert!(!a.can_manual_compact());
    assert!(a.refresh_context_panel().is_empty());
    assert!(a.start_manual_compact().is_empty());
    assert!(a.context_reads.is_empty());
    let mut a = app();
    let rs = take_requests(a.start_manual_compact());
    let r = rs.iter().find(|r| r.method == "session.compact").unwrap();
    testapp::respond_rpc_error(&mut a, r, -32601, "no method");
    assert!(!a.compact_supported);
    assert!(a.context_supported);
    assert!(a.start_manual_compact().is_empty());
}
#[test]
fn context_metadata_unknowns_do_not_become_zero_or_summary_prompt_text() {
    let mut a = app();
    a.composer.type_text("draft");
    let r = take_requests(a.open_context()).remove(0);
    let mut value = fixture("session-context-idle");
    value["budget"]["estimated_history_tokens"] = Value::Null;
    value["summary"] = "PRIVATE SUMMARY NEVER DISPLAY".into();
    respond(&mut a, &r, value);
    let text = crate::ui::context::rows(&a).join("\n");
    assert!(text.contains("history tail tokens:unknown"));
    assert!(!text.contains("PRIVATE SUMMARY"));
    assert_eq!(a.composer.content(), "draft");
    a.sessions.known.get_mut("ses_1").unwrap().info.loaded = false;
    a.update(AppEvent::Tick);
    assert!(a.context_panel().is_none());
}

#[test]
fn context_focus_scrollbar_and_exact_automatic_cancel_keep_editor_and_operation() {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    let mut a = app();
    a.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    a.composer.type_text("draft");
    let r = take_requests(a.open_context()).remove(0);
    let value = fixture("session-context-preparing");
    let id = value["current_operation"]["operation_id"].clone();
    respond(&mut a, &r, value);
    let body = a.main_body_area();
    let scrollbar = crate::ui::layout::screen_layout(&a, ratatui::layout::Rect::new(0, 0, 60, 16))
        .scrollbar_for(body);
    a.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: scrollbar.x,
        row: body.bottom() - 1,
        modifiers: KeyModifiers::NONE,
    })));
    assert!(a.context_panel().unwrap().scrollbar_grab.is_some());
    a.update(AppEvent::Terminal(CrosstermEvent::FocusLost));
    assert!(a.context_panel().unwrap().scrollbar_grab.is_none());
    a.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::F(6),
        KeyModifiers::NONE,
    ))));
    assert_eq!(a.focus, crate::state::panels::Focus::Editor);
    a.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    ))));
    assert_eq!(a.composer.content(), "draftx");
    if let MainView::Context(c) = &mut a.main_view {
        c.action = 2;
    }
    let r = take_requests(a.context_action()).remove(0);
    assert_eq!(r.method, "session.compact.cancel");
    assert_eq!(r.params["operation_id"], id);
    testapp::respond_rpc_error(&mut a, &r, -32601, "unsupported cancel");
    assert!(!a.compact_cancel_supported);
    assert!(a.context_supported);
    assert!(a.context_cancel_target().is_none());
    assert!(a.detail_escape().is_empty());
    assert!(a.context_panel().is_none());
    assert_eq!(a.composer.content(), "draftx");
}

#[test]
fn context_confirmation_resumes_when_retired_read_releases_its_actual_slot() {
    let mut a = app();
    let requests = take_requests(a.start_manual_compact());
    let compact = requests
        .iter()
        .find(|r| r.method == "session.compact")
        .unwrap();
    let old = requests
        .iter()
        .find(|r| r.method == "session.context")
        .unwrap();
    assert!(
        respond(
            &mut a,
            compact,
            json!({"operation_id":compact.params["operation_id"],"status":"failed"})
        )
        .is_empty()
    );
    assert_eq!(a.queries.in_flight_len(), 1);
    assert!(
        a.next_tick().is_none_or(|wait| !wait.is_zero()),
        "an occupied read must not cause a zero-deadline spin"
    );
    let next = take_requests(respond(&mut a, old, fixture("session-context-preparing")));
    assert_eq!(
        next.len(),
        1,
        "the pending one-shot confirmation must resume after the old read actually finishes"
    );
    assert_eq!(next[0].method, "session.context");
    assert_ne!(next[0].id, old.id);
    respond(&mut a, &next[0], fixture("session-context-idle"));
    assert!(a.context_reads.is_empty());
    assert!(!a.active_view().unwrap().is_preparing());
}

#[test]
fn explicit_refresh_targets_the_current_main_view_without_repinning_history() {
    let mut a = app();
    let r = take_requests(a.open_context()).remove(0);
    respond(&mut a, &r, fixture("session-context-idle"));
    let requests = take_requests(a.run_command("/refresh"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "session.context");
    respond(&mut a, &requests[0], fixture("session-context-idle"));
    let requests = take_requests(a.open_changes(crate::protocol::changes::ChangeScope::Workspace));
    for r in requests {
        let value = fixture(if r.method == "workspace.status" {
            "workspace-status"
        } else {
            "changes-list-workspace"
        });
        respond(&mut a, &r, value);
    }
    let requests = take_requests(a.run_command("/refresh"));
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| matches!(r.method, "changes.list" | "workspace.status"))
    );
}
