use super::*;
use crate::protocol::ToolDataStreamWire as Stream;
use crate::protocol::ToolProcessWire;
use crate::state::panels::{Focus, MainView, ToolTextLayout};
use crate::ui::testapp::{self, respond, take_requests};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

fn app() -> App {
    testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high")
}
fn key(call: &str) -> ToolKey {
    ToolKey::new("ses_1", "lup_1", 2, call)
}
fn read(key: &ToolKey, terminal: bool) -> Value {
    let name = if terminal {
        "tool-read-terminal.json"
    } else {
        "tool-read-running.json"
    };
    let mut value: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{name}")).unwrap(),
    )
    .unwrap();
    if value.get("result").is_some() {
        value = value["result"].take();
    }
    let reference = serde_json::to_value(crate::protocol::ToolRefWire::from(key)).unwrap();
    value["execution"]["tool_ref"] = reference.clone();
    if value.get("invocation").is_some() {
        value["invocation"]["tool_ref"] = reference;
    }
    value
}
fn page(key: &ToolKey, stream: Stream, start: u64, text: &str, eof: bool) -> Value {
    use base64::Engine;
    let (encoding, data) = match stream {
        Stream::Stdout | Stream::Stderr => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(text),
        ),
        Stream::Input => ("utf8_json", text.to_owned()),
        Stream::Output => ("utf8", text.to_owned()),
    };
    json!({"tool_ref": crate::protocol::ToolRefWire::from(key), "stream": stream, "encoding": encoding, "data": data, "base_offset": start, "next_offset": start + text.len() as u64, "observed_end": start + text.len() as u64, "eof": eof, "truncated": false, "availability": "available"})
}
fn press(app: &mut App, code: KeyCode) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    ))))
}
fn install_layout(app: &mut App) {
    let request = app
        .tool_layout_request(app.tool_body_area().width.saturating_sub(1).max(1))
        .unwrap();
    app.mark_tool_layout_pending(request.identity.clone());
    app.update(AppEvent::ToolLayoutPrepared(
        ToolTextLayout::build(request).unwrap(),
    ));
}

#[test]
fn closing_a_and_opening_b_keeps_real_slots_and_ignores_late_a() {
    let mut app = app();
    let a = take_requests(app.open_tool_detail(key("a"))).remove(0);
    assert!(press(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(app.queries.in_flight_len(), 1);
    let b = take_requests(app.open_tool_detail(key("b"))).remove(0);
    assert_eq!(app.queries.in_flight_len(), 2);
    assert!(respond(&mut app, &a, read(&key("a"), true)).is_empty());
    assert_eq!(app.queries.in_flight_len(), 1);
    assert_eq!(app.tool_detail().unwrap().key, key("b"));
    assert!(
        !app.active_view()
            .unwrap()
            .tool_presentations
            .contains_key(&key("a"))
    );
    let output = take_requests(respond(&mut app, &b, read(&key("b"), true)));
    assert_eq!(output.len(), 1);
    assert_eq!(output[0].method, "tool.output");
    assert_eq!(output[0].params["tool_call_id"], "b");
}

#[test]
fn reopening_the_same_tool_after_a_late_response_reissues_the_refresh() {
    let mut app = app();
    let old = take_requests(app.open_tool_detail(key("a"))).remove(0);
    assert!(press(&mut app, KeyCode::Esc).is_empty());

    // The old request still owns its real slot. Reopening the same ToolKey
    // records one coalesced refresh instead of losing the new detail's intent.
    assert!(app.open_tool_detail(key("a")).is_empty());
    assert_eq!(app.queries.in_flight_len(), 1);

    let retry = take_requests(respond(&mut app, &old, read(&key("a"), true)));
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].method, "tool.read");
    assert_eq!(app.tool_detail().unwrap().key, key("a"));
}

#[test]
fn tool_queries_share_the_deferred_budget_with_execution_waits() {
    let mut app = app();
    for index in 0..15 {
        app.pending_requests.insert(
            crate::protocol::RequestId(10_000 + index),
            RequestKind::WaitTurn(TurnRef {
                session_id: "other".into(),
                loop_id: format!("loop_{index}"),
            }),
        );
    }
    let a = take_requests(app.open_tool_detail(key("a"))).remove(0);
    assert!(!app.deferred_admission_ok());
    assert!(app.open_tool_detail(key("b")).is_empty());
    assert_eq!(app.queries.in_flight_len(), 1);
    let b = take_requests(respond(&mut app, &a, read(&key("a"), true))).remove(0);
    assert_eq!(b.params["tool_call_id"], "b");
    assert_eq!(app.queries.in_flight_len(), 1);
}

#[test]
fn deleting_a_session_retains_query_accounting_until_the_real_response() {
    let mut app = app();
    let request = take_requests(app.open_tool_detail(key("a"))).remove(0);
    app.on_delete_session_response(
        &"ses_1".to_owned(),
        &RpcResponse {
            id: crate::protocol::RequestId(999),
            result: Some(json!({"ok":true})),
            error: None,
        },
    );
    assert_eq!(app.queries.in_flight_len(), 1);
    assert_eq!(
        app.pending_requests.get(&request.id),
        Some(&RequestKind::StaleRead)
    );
    respond(&mut app, &request, read(&key("a"), true));
    assert_eq!(app.queries.in_flight_len(), 0);
    assert!(app.tool_detail().is_none());
}

#[test]
fn terminal_conflict_uses_authoritative_read_and_recording_is_independent() {
    let mut app = app();
    let value = read(&key("a"), true);
    let mut execution: crate::protocol::ToolExecutionWire =
        serde_json::from_value(value["execution"].clone()).unwrap();
    execution.state = crate::protocol::ToolExecutionStateWire::Succeeded;
    execution.outcome = Some(ToolOutcomeWire::Success);
    execution.recording = crate::protocol::ToolRecordingStateWire::Failed;
    app.accept_tool_execution(execution.clone(), false);
    let request = take_requests(app.open_tool_detail(key("a"))).remove(0);
    assert_eq!(app.tool_facts().unwrap().status, ToolStatus::Succeeded);
    execution.state = crate::protocol::ToolExecutionStateWire::Failed;
    execution.outcome = Some(ToolOutcomeWire::Failed);
    app.accept_tool_execution(execution, false);
    assert!(app.tool_facts().unwrap().needs_read);
    assert_eq!(
        app.tool_facts().unwrap().outcome,
        Some(ToolOutcomeWire::Success)
    );
    respond(&mut app, &request, value);
    assert!(!app.tool_facts().unwrap().needs_read);
    let old: crate::protocol::ToolExecutionWire =
        serde_json::from_value(read(&key("a"), false)["execution"].clone()).unwrap();
    let outcome = app.tool_facts().unwrap().outcome;
    app.accept_tool_execution(old, false);
    assert_eq!(app.tool_facts().unwrap().outcome, outcome);
}

#[test]
fn process_command_is_retained_before_any_read_and_cancel_is_not_confirmation() {
    let mut app = app();
    let value = read(&key("a"), false);
    let mut command: crate::protocol::CommandResultWire =
        serde_json::from_value(value["execution"]["command"].clone()).unwrap();
    command.status = crate::protocol::CommandStatusWire::Cancelling;
    app.accept_tool_process(ToolProcessWire {
        tool_ref: (&key("a")).into(),
        chunk: None,
        command: Some(command.clone()),
    });
    app.open_tool_detail(key("a"));
    assert_eq!(app.tool_facts().unwrap().command.as_deref(), Some(&command));
    assert!(!command.termination_confirmed && !command.output_complete);
    assert!(command.exit_code.is_none());
    assert!(app.tool_tabs().contains(&Stream::Stdout));
}
#[test]
fn switch_tab_holds_old_slot_and_old_response_cannot_change_current_tab() {
    let mut app = app();
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    let output = take_requests(respond(&mut app, &req, read(&key("a"), true))).remove(0);
    assert_eq!(app.tool_detail().unwrap().tab, Stream::Stdout);
    assert!(press(&mut app, KeyCode::Tab).is_empty());
    assert_eq!(app.queries.in_flight_len(), 1);
    let next = take_requests(respond(
        &mut app,
        &output,
        page(&key("a"), Stream::Stdout, 0, "old", true),
    ))
    .remove(0);
    assert_eq!(next.params["stream"], "stderr");
    assert_eq!(
        app.tool_detail().unwrap().streams[Stream::Stdout.index()].retained_bytes,
        0
    );
}
#[test]
fn terminal_command_drains_until_real_empty_eof_then_stops() {
    let mut app = app();
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    let output = take_requests(respond(&mut app, &req, read(&key("a"), true))).remove(0);
    let next = take_requests(respond(
        &mut app,
        &output,
        page(&key("a"), Stream::Stdout, 0, "tail", false),
    ))
    .remove(0);
    assert_eq!(next.params["offset"], 4);
    assert!(
        respond(
            &mut app,
            &next,
            page(&key("a"), Stream::Stdout, 4, "", true)
        )
        .is_empty()
    );
    assert!(app.tool_detail().unwrap().stream().eof);
    assert!(app.update(AppEvent::Tick).is_empty());
}
#[test]
fn resource_exhausted_stops_pagination_until_explicit_retry() {
    let mut app = app();
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    testapp::respond_rpc_error(&mut app, &req, -32020, "resource_exhausted");
    assert_eq!(app.queries.in_flight_len(), 0);
    assert!(
        app.tool_detail()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("retry")
            || app
                .tool_detail()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("重试")
    );
    for _ in 0..20 {
        assert!(app.update(AppEvent::Tick).is_empty());
    }
    assert_eq!(take_requests(press(&mut app, KeyCode::F(5))).len(), 1);
}
#[test]
fn f6_escape_preserve_draft_anchor_and_never_send_or_cancel_from_main() {
    let mut app = app();
    app.composer.set_text("draft remains");
    app.active_session_mut().unwrap().scroll.follow_tail = false;
    app.open_tool_detail(key("a"));
    for code in [KeyCode::Enter, KeyCode::Char('q'), KeyCode::Char('x')] {
        assert!(press(&mut app, code).is_empty());
    }
    assert_eq!(app.composer.content(), "draft remains");
    assert_eq!(app.focused_region(), Focus::Main);
    press(&mut app, KeyCode::F(6));
    assert_eq!(app.focused_region(), Focus::Editor);
    press(&mut app, KeyCode::Char('!'));
    assert!(press(&mut app, KeyCode::Esc).is_empty());
    assert!(app.tool_detail().is_none());
    assert_eq!(app.composer.content(), "draft remains!");
    assert!(!app.active_view().unwrap().scroll.follow_tail);
    assert_eq!(app.queries.in_flight_len(), 1);
    for _ in 0..5 {
        assert!(app.update(AppEvent::Tick).is_empty());
    }
}
#[test]
fn polling_uses_500ms_fallback_and_250ms_event_hint() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let mut app = app();
    let elapsed = Arc::new(AtomicU64::new(0));
    let clock = elapsed.clone();
    let base = Instant::now();
    app.monotonic_now =
        Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    let output = take_requests(respond(&mut app, &req, read(&key("a"), false))).remove(0);
    respond(
        &mut app,
        &output,
        page(&key("a"), Stream::Stdout, 0, "", false),
    );
    elapsed.store(499, Ordering::Relaxed);
    assert!(app.update(AppEvent::Tick).is_empty());
    elapsed.store(500, Ordering::Relaxed);
    let req = take_requests(app.update(AppEvent::Tick)).remove(0);
    assert_eq!(req.method, "tool.read");
    let output = take_requests(respond(&mut app, &req, read(&key("a"), false))).remove(0);
    respond(
        &mut app,
        &output,
        page(&key("a"), Stream::Stdout, 0, "", false),
    );
    app.accept_tool_process(ToolProcessWire {
        tool_ref: (&key("a")).into(),
        chunk: None,
        command: None,
    });
    assert_eq!(
        app.tool_detail().unwrap().due,
        Some(base + Duration::from_millis(750))
    );
}
#[test]
fn detail_scroll_stops_follow_and_end_resumes_only_detail() {
    let mut app = app();
    app.terminal_size = (80, 24);
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    let output = take_requests(respond(&mut app, &req, read(&key("a"), true))).remove(0);
    respond(
        &mut app,
        &output,
        page(&key("a"), Stream::Stdout, 0, &"hello\n".repeat(90), true),
    );
    install_layout(&mut app);
    let before = app.active_view().unwrap().scroll.offset;
    press(&mut app, KeyCode::PageUp);
    assert!(!app.tool_detail().unwrap().follow[2]);
    press(&mut app, KeyCode::End);
    assert!(app.tool_detail().unwrap().follow[2]);
    assert_eq!(app.active_view().unwrap().scroll.offset, before);
}

#[test]
fn copy_uses_the_visible_snapshot_while_new_process_bytes_await_layout() {
    use base64::Engine;
    let mut app = app();
    app.terminal_size = (80, 24);
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    let output = take_requests(respond(&mut app, &req, read(&key("a"), false))).remove(0);
    respond(
        &mut app,
        &output,
        page(&key("a"), Stream::Stdout, 0, "visible", false),
    );
    install_layout(&mut app);
    app.accept_tool_process(ToolProcessWire {
        tool_ref: (&key("a")).into(),
        command: None,
        chunk: Some(crate::protocol::ToolProcessChunkWire {
            stream: Stream::Stdout,
            encoding: "base64".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b" pending"),
            base_offset: 7,
            next_offset: 15,
            observed_end: 15,
            dropped: false,
            expired: false,
        }),
    });
    let commands = app.copy_tool_detail();
    assert!(
        matches!(commands.as_slice(), [AppCommand::CopySelection(text)] if text.as_str() == "visible")
    );
    assert_eq!(
        app.tool_detail().unwrap().stream().display_text(),
        "visible pending"
    );
    assert!(app.notices.back().unwrap().text.contains("不完整"));
}

#[test]
fn input_tab_survives_preview_eviction_when_authoritative_input_is_available() {
    let mut app = app();
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    respond(&mut app, &req, read(&key("a"), true));
    app.detail_tab(-1);
    assert_eq!(app.tool_detail().unwrap().tab, Stream::Input);
    let view = app.active_session_mut().unwrap();
    let facts = Arc::make_mut(&mut view.tool_presentations)
        .get_mut(&key("a"))
        .unwrap();
    Arc::make_mut(facts).truncate_to_bytes(0);
    assert!(app.tool_facts().unwrap().invocation.is_none());
    assert!(app.tool_tabs().contains(&Stream::Input));
}
#[test]
fn detail_render_is_safe_narrow_and_keeps_the_existing_editor_footer_geometry() {
    let mut app = app();
    let req = take_requests(app.open_tool_detail(key("a"))).remove(0);
    let output = take_requests(respond(&mut app, &req, read(&key("a"), true))).remove(0);
    respond(
        &mut app,
        &output,
        page(
            &key("a"),
            Stream::Stdout,
            0,
            "A\x1b]52;c;bad\x07\r中\n",
            true,
        ),
    );
    for (width, height) in [(60, 16), (80, 24), (120, 40)] {
        app.terminal_size = (width, height);
        install_layout(&mut app);
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        let screen = crate::ui::layout::screen_layout(&app, area);
        assert_eq!(screen.footer.height, 1);
        assert!(screen.panel.height >= 4 && screen.panel.height <= 12);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains('\x1b'));
        assert!(!text.contains('\x07'));
        assert!(text.contains("EOF=true"));
        assert!(text.contains('▎'));
    }
}
#[test]
fn title_detail_hit_does_not_replace_the_original_tool_fold() {
    let mut app = testapp::tools(ThemeKind::Dark);
    app.terminal_size = (80, 24);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let screen = crate::ui::layout::screen_layout(&app, area);
    let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
    let position = crate::ui::transcript::scroll_position(
        &app,
        prepared.total_rows(),
        screen.transcript.height as usize,
    );
    let (hit, key) = crate::ui::tool_detail::detail_hits(
        &prepared,
        screen.transcript,
        position.offset,
        position.visible_rows,
    )
    .remove(0);
    app.update(AppEvent::ConversationPrepared(prepared));
    let commands = app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        },
    )));
    assert!(matches!(app.main_view, MainView::ToolDetail(_)));
    assert_eq!(app.tool_detail().unwrap().key, key);
    assert_eq!(
        take_requests(commands)[0].params["tool_call_id"],
        key.tool_call_id
    );
}
