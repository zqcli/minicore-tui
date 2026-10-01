use super::*;
use crate::protocol::workspace::*;
use crate::state::{
    panels::{Focus, MainView},
    workspace::*,
};
use crate::ui::testapp::{self, respond, take_requests};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
fn app() -> App {
    testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high")
}
fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code, modifiers,
    ))))
}
fn fixture(name: &str) -> Value {
    serde_json::from_str::<Value>(
        &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{name}.json")).unwrap(),
    )
    .unwrap()["result"]
        .clone()
}
fn files(paths: &[&str], cursor: Value) -> Value {
    json!({"directory":"","entries": paths.iter().map(|p| json!({"path":p,"kind":"file","size":10})).collect::<Vec<_>>(),"next_cursor":cursor,"truncated":!cursor.is_null(),"scan_complete":cursor.is_null(),"stopped_by":if cursor.is_null(){"end"}else{"page"},"skipped_count":0,"consistency":"live","observed_at_unix_ms":1})
}
fn page(path: &str, text: &str, line: u32, next: Value, revision: &str) -> Value {
    json!({"path":path,"content":text,"start_line":line,"returned_lines":1,"revision":revision,"truncated":!next.is_null(),"line_truncated":!next.is_null(),"next_range":next,"encoding":"utf8","status":"ok","file_bytes":500,"file_modified_unix_ms":null})
}
fn clock(app: &mut App) -> Arc<AtomicU64> {
    let elapsed = Arc::new(AtomicU64::new(0));
    let c = elapsed.clone();
    let base = Instant::now();
    app.monotonic_now = Arc::new(move || base + Duration::from_millis(c.load(Ordering::Relaxed)));
    elapsed
}
fn file(app: &mut App, path: &str) -> Vec<AppCommand> {
    app.open_file_preview(path.into(), None, ReturnTarget::Conversation)
}
fn layout(app: &mut App) {
    let req = app
        .file_layout_request(app.main_body_area().width.saturating_sub(9).max(1))
        .unwrap();
    app.mark_file_layout_pending(req.identity.clone());
    app.update(AppEvent::FileLayoutPrepared(
        FileLayout::build(req).unwrap(),
    ));
}
#[test]
fn candidates_debounce_one_inflight_and_generation_fences_opaque_cursor() {
    let mut a = app();
    let time = clock(&mut a);
    a.open_workspace_browser(BrowserKind::Files, "a".into(), false);
    time.store(149, Ordering::Relaxed);
    assert!(a.update(AppEvent::Tick).is_empty());
    time.store(150, Ordering::Relaxed);
    let old = take_requests(a.update(AppEvent::Tick)).remove(0);
    assert_eq!(old.params["recursive"], true);
    assert_eq!(old.params["limit"], 100);
    assert_eq!(old.params["max_bytes"], 65536);
    a.workspace_edit(Some("b"), false, false, false);
    time.store(300, Ordering::Relaxed);
    assert!(a.update(AppEvent::Tick).is_empty());
    assert_eq!(a.queries.in_flight_len(), 1);
    let current =
        take_requests(respond(&mut a, &old, files(&["old-secret"], Value::Null))).remove(0);
    assert_eq!(current.params["query"], "ab");
    assert!(a.workspace_browser().unwrap().files.is_empty());
    let cursor = json!({"entry":9,"scope":"opaque","additive":[1,"unchanged"]});
    respond(&mut a, &current, files(&["z", "a"], cursor.clone()));
    assert_eq!(a.workspace_browser().unwrap().files[0].path, "a");
    let next = take_requests(a.workspace_more(false)).remove(0);
    assert_eq!(next.params["cursor"], cursor);
    a.workspace_edit(Some("c"), false, false, false);
    time.store(450, Ordering::Relaxed);
    let changed = take_requests(respond(&mut a, &next, files(&["stale"], Value::Null))).remove(0);
    assert!(changed.params["cursor"].is_null());
    assert!(a.workspace_browser().unwrap().files.is_empty());
    assert!(press(&mut a, KeyCode::Esc, KeyModifiers::NONE).is_empty());
    assert_eq!(a.queries.in_flight_len(), 1);
    respond(&mut a, &changed, files(&["late"], Value::Null));
    assert!(a.workspace_browser().is_none());
    assert!(a.update(AppEvent::Tick).is_empty());
}
#[test]
fn deadline_and_query_errors_stop_without_implicit_rescan() {
    let mut a = app();
    a.open_workspace_browser(BrowserKind::Files, String::new(), false);
    let r = take_requests(a.workspace_more(true)).remove(0);
    respond(&mut a, &r, fixture("workspace-files-deadline"));
    assert_eq!(
        a.workspace_browser().unwrap().stopped_by,
        Some(ScanStop::Deadline)
    );
    assert!(a.workspace_more(false).is_empty());
    assert!(a.update(AppEvent::Tick).is_empty());
    let r = take_requests(a.workspace_more(true)).remove(0);
    testapp::respond_rpc_error(&mut a, &r, -32020, "secret query diagnostic");
    assert!(a.workspace_browser().unwrap().error.is_some());
    assert!(a.update(AppEvent::Tick).is_empty());
    assert!(!format!("{:?}", a.dock).contains("secret"));
}
#[test]
fn late_file_a_keeps_its_slot_and_cannot_change_b() {
    let mut a = app();
    let r = take_requests(file(&mut a, "a")).remove(0);
    assert!(file(&mut a, "b").is_empty());
    assert_eq!(a.queries.in_flight_len(), 1);
    let b = take_requests(respond(
        &mut a,
        &r,
        page("a", "secret a", 1, Value::Null, "r"),
    ))
    .remove(0);
    assert_eq!(b.params["path"], "b");
    assert_eq!(a.file_preview().unwrap().content.bytes, 0);
    assert_eq!(a.queries.in_flight_len(), 1);
    respond(&mut a, &b, page("b", "correct b", 1, Value::Null, "r"));
    assert_eq!(
        a.file_preview().unwrap().content.chunks[0].as_ref(),
        "correct b"
    );
    assert!(press(&mut a, KeyCode::Esc, KeyModifiers::NONE).is_empty());
    assert!(a.update(AppEvent::Tick).is_empty());
}
#[test]
fn same_line_utf8_cursor_crlf_and_no_eol_are_byte_exact() {
    let mut a = app();
    let first = take_requests(file(&mut a, "x")).remove(0);
    assert_eq!(first.params["start_line"], 1);
    assert_eq!(first.params["line_byte_offset"], 0);
    assert_eq!(first.params["max_lines"], 400);
    assert_eq!(first.params["max_bytes"], 65536);
    respond(
        &mut a,
        &first,
        page(
            "x",
            "中",
            1,
            json!({"start_line":1,"line_byte_offset":3}),
            "stable",
        ),
    );
    let second = take_requests(a.file_more(false)).remove(0);
    assert_eq!(second.params["if_revision"], "stable");
    assert_eq!(second.params["start_line"], 1);
    assert_eq!(second.params["line_byte_offset"], 3);
    respond(
        &mut a,
        &second,
        page(
            "x",
            "🙂\r",
            1,
            json!({"start_line":1,"line_byte_offset":8}),
            "stable",
        ),
    );
    let third = take_requests(a.file_more(false)).remove(0);
    assert_eq!(third.params["line_byte_offset"], 8);
    respond(&mut a, &third, page("x", "\nx", 1, Value::Null, "stable"));
    layout(&mut a);
    assert_eq!(
        a.file_preview()
            .unwrap()
            .layout
            .as_ref()
            .unwrap()
            .copy_text
            .as_ref(),
        "中🙂\r\nx"
    );
    let copy = a.copy_file();
    assert!(matches!(&copy[0],AppCommand::CopySelection(text) if text.as_str()=="中🙂\r\nx"));
    assert!(a.file_more(false).is_empty());
}
#[test]
fn changed_stops_mixed_pages_and_binary_too_large_unavailable_are_explicit() {
    let mut a = app();
    let r = take_requests(file(&mut a, "x")).remove(0);
    respond(
        &mut a,
        &r,
        page(
            "x",
            "old",
            1,
            json!({"start_line":1,"line_byte_offset":3}),
            "v1",
        ),
    );
    let r = take_requests(a.file_more(false)).remove(0);
    let mut changed = page("x", "", 1, Value::Null, "v2");
    changed["status"] = "changed".into();
    respond(&mut a, &r, changed);
    assert_eq!(a.file_preview().unwrap().content.chunks[0].as_ref(), "old");
    assert_eq!(a.file_preview().unwrap().revision.as_deref(), Some("v1"));
    assert!(a.file_more(false).is_empty());
    assert!(a.update(AppEvent::Tick).is_empty());
    for status in ["binary", "too_large"] {
        let r = take_requests(a.file_more(true)).remove(0);
        assert!(r.params["if_revision"].is_null());
        let mut p = page("x", "", 1, Value::Null, "v3");
        p["status"] = status.into();
        p["encoding"] = "unknown".into();
        p["revision"] = Value::Null;
        respond(&mut a, &r, p);
        assert_ne!(a.file_preview().unwrap().status, Some(FileStatus::Ok));
        assert_eq!(a.file_preview().unwrap().content.bytes, 0);
    }
    let r = take_requests(a.file_more(true)).remove(0);
    testapp::respond_rpc_error(&mut a, &r, -32010, "workspace unavailable");
    assert!(a.file_preview().unwrap().error.is_some());
    assert!(a.update(AppEvent::Tick).is_empty());
}
#[test]
fn closed_session_workspace_entries_require_explicit_continue() {
    let mut a = app();
    a.sessions.known.get_mut("ses_1").unwrap().info.loaded = false;
    assert!(
        a.open_workspace_browser(BrowserKind::Files, String::new(), false)
            .is_empty()
    );
    assert!(a.workspace_browser().is_none());
    assert!(file(&mut a, "anything").is_empty());
    assert!(a.file_preview().is_none());
    assert!(a.notices.back().unwrap().text.contains("Continue"));
}
#[test]
fn path_only_reference_preview_return_and_native_undo_preserve_draft() {
    let mut a = app();
    a.composer_mut().type_text("prefix ");
    press(&mut a, KeyCode::Char('@'), KeyModifiers::NONE);
    let r = take_requests(a.workspace_more(true)).remove(0);
    let path = "dir 空 格/\"file\".txt";
    respond(&mut a, &r, files(&[path], Value::Null));
    let preview = take_requests(a.workspace_select(true)).remove(0);
    respond(
        &mut a,
        &preview,
        page(path, "DO NOT ATTACH", 1, Value::Null, "r"),
    );
    assert_eq!(a.composer.content(), "prefix @");
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    assert!(a.workspace_browser().is_some());
    assert!(a.workspace_select(false).is_empty());
    assert_eq!(
        a.composer.content(),
        format!("prefix {}", reference_token(path))
    );
    assert_eq!(a.composer.file_reference_at_cursor(), Some(path));
    assert_eq!(a.active_view().unwrap().transcript.total, 0);
    assert!(!a.composer.content().contains("DO NOT ATTACH"));
    a.composer_mut().undo();
    assert_eq!(a.composer.content(), "prefix @");
    assert!(a.composer.file_reference_at_cursor().is_none());
}
#[test]
fn token_insertion_after_a_large_paste_never_jumps_to_a_u16_column() {
    let mut a = app();
    a.composer_mut().insert_paste(&"x".repeat(70000));
    a.composer_mut().type_text(" ");
    press(&mut a, KeyCode::Char('@'), KeyModifiers::NONE);
    let r = take_requests(a.workspace_more(true)).remove(0);
    respond(&mut a, &r, files(&["space name"], Value::Null));
    a.workspace_select(false);
    assert!(a.composer.content().ends_with(" @\"space name\""));
    assert!(a.composer.content().starts_with(&"x".repeat(70000)));
}
#[test]
fn ordinary_email_paste_and_main_enter_do_not_open_send_or_cancel() {
    let mut a = app();
    a.composer_mut().type_text("name");
    press(&mut a, KeyCode::Char('@'), KeyModifiers::NONE);
    assert!(a.workspace_browser().is_none());
    a.update(AppEvent::Terminal(CrosstermEvent::Paste(" @pasted".into())));
    assert!(a.workspace_browser().is_none());
    let draft = a.composer.content();
    file(&mut a, "a");
    for code in [KeyCode::Enter, KeyCode::Char('q'), KeyCode::Char('x')] {
        assert!(press(&mut a, code, KeyModifiers::NONE).is_empty());
    }
    assert_eq!(a.composer.content(), draft);
    press(&mut a, KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(a.focus, Focus::Editor);
    press(&mut a, KeyCode::Char('!'), KeyModifiers::NONE);
    assert!(a.composer.content().ends_with('!'));
}
#[test]
fn grep_scope_case_generation_and_selected_file_use_real_source_position() {
    let mut a = app();
    a.open_workspace_browser(BrowserKind::Grep, "needle".into(), false);
    if let Dock::Workspace(b) = &mut a.dock {
        b.scope = "[\"dir 空格\",\"another\"]".into();
    }
    let r = take_requests(a.workspace_more(true)).remove(0);
    assert_eq!(r.params["paths"], json!(["dir 空格", "another"]));
    assert_eq!(r.params["max_matches"], 100);
    let mut result = fixture("workspace-search");
    result["matches"][0]["line_number"] = 5.into();
    result["matches"][0]["line_text_byte_offset"] = 0.into();
    respond(&mut a, &r, result);
    let p = take_requests(a.workspace_select(false)).remove(0);
    assert_eq!(p.params["start_line"], 1);
    assert_eq!(a.file_preview().unwrap().target.unwrap().start_line, 5);
    let next = take_requests(respond(
        &mut a,
        &p,
        page(
            "needles.txt",
            "a\nb\nc\nd\n",
            1,
            json!({"start_line":5,"line_byte_offset":0}),
            "r",
        ),
    ))
    .remove(0);
    assert_eq!(next.params["start_line"], 5);
    assert_eq!(next.params["if_revision"], "r");
    respond(
        &mut a,
        &next,
        page("needles.txt", "let needle = 1;", 5, Value::Null, "r"),
    );
    layout(&mut a);
    assert_eq!(a.file_preview().unwrap().offset, 4);
    assert!(a.file_preview().unwrap().target.is_none());
    press(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(a.workspace_browser().unwrap().matches.len(), 2);
    let old_generation = a.workspace_browser().unwrap().generation;
    a.workspace_edit(None, false, false, true);
    assert!(a.workspace_browser().unwrap().generation > old_generation);
    assert!(a.workspace_browser().unwrap().matches.is_empty());
    assert!(a.workspace_browser().unwrap().cursor.is_none());
}
#[test]
fn file_tool_layout_identities_and_close_routes_do_not_cross_variants() {
    let mut a = app();
    let r = take_requests(file(&mut a, "a")).remove(0);
    respond(&mut a, &r, page("a", "file secret", 1, Value::Null, "r"));
    let req = a.file_layout_request(50).unwrap();
    a.mark_file_layout_pending(req.identity.clone());
    let old = FileLayout::build(req).unwrap();
    a.close_tool_detail();
    assert!(a.file_preview().is_some());
    let key = ToolKey::new("ses_1", "loop", 1, "call");
    a.open_tool_detail(key);
    a.update(AppEvent::FileLayoutPrepared(old));
    assert!(a.tool_detail().is_some());
    assert!(a.file_preview().is_none());
    let req = a.tool_layout_request(50).unwrap();
    a.mark_tool_layout_pending(req.identity.clone());
    let old = crate::state::panels::ToolTextLayout::build(req).unwrap();
    file(&mut a, "b");
    a.update(AppEvent::ToolLayoutPrepared(old));
    assert!(a.file_preview().unwrap().layout.is_none());
    assert!(matches!(a.main_view, MainView::FilePreview(_)));
}

#[test]
fn file_preview_width_reflow_preserves_selected_source_position() {
    let mut a = app();
    a.terminal_size = (160, 48);
    let target = FileRange {
        start_line: 75,
        line_byte_offset: 0,
    };
    let r = take_requests(a.open_file_preview(
        "numbered-cjk.txt".into(),
        Some(target),
        ReturnTarget::Conversation,
    ))
    .remove(0);
    let text: String = (1..=80)
        .map(|i| format!("ROW-{i:03} {}\n", "甲".repeat(95)))
        .collect();
    respond(
        &mut a,
        &r,
        page("numbered-cjk.txt", &text, 1, Value::Null, "r"),
    );
    layout(&mut a);
    assert!(a.file_preview().unwrap().target.is_none());
    for size in [(60, 16), (160, 48)] {
        a.update(AppEvent::TerminalSize {
            width: size.0,
            height: size.1,
        });
        layout(&mut a);
        let preview = a.file_preview().unwrap();
        let source = preview.layout.as_ref().unwrap().rows[preview.offset].source;
        assert_eq!(
            source, target,
            "width reflow must preserve the selected source"
        );
        assert!(!preview.follow);
    }
}

#[test]
fn file_preview_width_reflow_preserves_same_line_byte_anchor() {
    let mut a = app();
    a.terminal_size = (160, 48);
    let r = take_requests(a.open_file_preview(
        "long-cjk.txt".into(),
        Some(FileRange {
            start_line: 1,
            line_byte_offset: 300,
        }),
        ReturnTarget::Conversation,
    ))
    .remove(0);
    respond(
        &mut a,
        &r,
        page("long-cjk.txt", &"甲".repeat(1000), 1, Value::Null, "r"),
    );
    layout(&mut a);
    let preview = a.file_preview().unwrap();
    let anchor = preview.layout.as_ref().unwrap().rows[preview.offset].source;
    assert!(anchor.line_byte_offset > 0);
    a.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    layout(&mut a);
    let preview = a.file_preview().unwrap();
    let rows = &preview.layout.as_ref().unwrap().rows;
    assert_eq!(rows[preview.offset].source.start_line, anchor.start_line);
    assert!(rows[preview.offset].source.line_byte_offset <= anchor.line_byte_offset);
    assert!(rows[preview.offset + 1].source.line_byte_offset > anchor.line_byte_offset);
}

#[test]
fn file_preview_width_reflow_keeps_following_loaded_tail() {
    let mut a = app();
    a.terminal_size = (160, 48);
    let r = take_requests(file(&mut a, "long-cjk.txt")).remove(0);
    respond(
        &mut a,
        &r,
        page("long-cjk.txt", &"甲".repeat(1000), 1, Value::Null, "r"),
    );
    layout(&mut a);
    press(&mut a, KeyCode::End, KeyModifiers::NONE);
    a.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    layout(&mut a);
    let preview = a.file_preview().unwrap();
    assert!(preview.follow);
    let height = a.main_body_area().height as usize;
    assert_eq!(
        preview.scroll_offset(height),
        preview
            .layout
            .as_ref()
            .unwrap()
            .rows
            .len()
            .saturating_sub(height)
    );
}

#[test]
fn delayed_file_layout_cannot_consume_unreached_grep_target() {
    let mut a = app();
    let target = FileRange {
        start_line: 1,
        line_byte_offset: 600,
    };
    let first = take_requests(a.open_file_preview(
        "long-line.txt".into(),
        Some(target),
        ReturnTarget::Conversation,
    ))
    .remove(0);
    let second = take_requests(respond(
        &mut a,
        &first,
        page(
            "long-line.txt",
            &"甲".repeat(20),
            1,
            json!({"start_line":1,"line_byte_offset":60}),
            "r",
        ),
    ))
    .remove(0);
    let request = a.file_layout_request(10).unwrap();
    a.mark_file_layout_pending(request.identity.clone());
    let delayed = FileLayout::build(request).unwrap();
    assert_eq!(delayed.identity.revision, 1);
    assert_eq!(delayed.copy_text.len(), 60);
    respond(
        &mut a,
        &second,
        page(
            "long-line.txt",
            &format!("{}TARGET", "甲".repeat(180)),
            1,
            Value::Null,
            "r",
        ),
    );
    assert_eq!(a.file_preview().unwrap().content_revision, 2);
    assert!(a.file_preview().unwrap().next.is_none());
    a.update(AppEvent::FileLayoutPrepared(delayed));
    assert_eq!(
        a.file_preview().unwrap().target,
        Some(target),
        "an earlier content revision cannot prove the match was reached"
    );
    let request = a.file_layout_request(10).unwrap();
    a.mark_file_layout_pending(request.identity.clone());
    a.update(AppEvent::FileLayoutPrepared(
        FileLayout::build(request).unwrap(),
    ));
    let preview = a.file_preview().unwrap();
    assert_eq!(
        preview.layout.as_ref().unwrap().rows[preview.offset].source,
        target
    );
    assert!(preview.target.is_none());
}
#[test]
fn file_and_browser_render_safely_at_all_supported_geometries() {
    use ratatui::{Terminal, backend::TestBackend};
    for size in [(60, 16), (80, 24), (120, 40)] {
        let mut a = app();
        a.terminal_size = size;
        a.composer_mut().type_text("saved draft");
        let r = take_requests(file(&mut a, "file\x1b]52;x\x07")).remove(0);
        respond(
            &mut a,
            &r,
            page(
                "file\x1b]52;x\x07",
                "中🙂\t\x1b]52;x\x07\r\nnoeol",
                1,
                Value::Null,
                "r",
            ),
        );
        layout(&mut a);
        let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
        term.draw(|f| crate::ui::render(f, &a)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains('\x1b'));
        assert!(text.contains("saved draft"));
        let rect =
            crate::ui::layout::screen_layout(&a, ratatui::layout::Rect::new(0, 0, size.0, size.1));
        assert_eq!(rect.footer.height, 1);
        a.open_workspace_browser(BrowserKind::Grep, "query".into(), false);
        term.draw(|f| crate::ui::render(f, &a)).unwrap();
    }
}

#[test]
fn workspace_shares_two_slots_with_tools_and_releases_only_on_response() {
    let mut a = app();
    let old_file = take_requests(file(&mut a, "a")).remove(0);
    a.open_tool_detail(ToolKey::new("ses_1", "loop", 1, "call"));
    assert_eq!(a.queries.in_flight_len(), 2);
    a.open_workspace_browser(BrowserKind::Files, String::new(), false);
    assert!(a.workspace_more(true).is_empty());
    let commands = take_requests(respond(
        &mut a,
        &old_file,
        page("a", "late", 1, Value::Null, "r"),
    ));
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].method, "workspace.files");
    assert_eq!(a.queries.in_flight_len(), 2);
    assert!(press(&mut a, KeyCode::Esc, KeyModifiers::NONE).is_empty());
    assert!(
        a.tool_detail().is_some(),
        "Dock closes before the underlying main detail"
    );
    assert!(press(&mut a, KeyCode::Esc, KeyModifiers::NONE).is_empty());
    assert!(!a.has_main_detail());
    assert_eq!(a.queries.in_flight_len(), 2);
}

#[test]
fn live_loop_keeps_running_when_file_or_dock_closes_and_anchor_is_restored() {
    let mut a = testapp::live_turn(ThemeKind::Dark);
    a.composer_mut().type_text("未发送 draft");
    a.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .scroll
        .follow_tail = false;
    a.sessions.known.get_mut("ses_1").unwrap().scroll.offset = 42;
    let cursor = a.composer.cursor();
    let before = a
        .active_view()
        .unwrap()
        .live
        .as_ref()
        .unwrap()
        .reference
        .clone();
    file(&mut a, "still running");
    a.open_workspace_browser(BrowserKind::Files, String::new(), false);
    assert!(press(&mut a, KeyCode::Esc, KeyModifiers::NONE).is_empty());
    assert!(a.file_preview().is_some());
    assert!(press(&mut a, KeyCode::Esc, KeyModifiers::NONE).is_empty());
    assert_eq!(a.active_view().unwrap().scroll.offset, 42);
    assert!(!a.active_view().unwrap().scroll.follow_tail);
    assert_eq!(a.composer.cursor(), cursor);
    assert_eq!(a.composer.content(), "未发送 draft");
    assert_eq!(
        a.active_view().unwrap().live.as_ref().unwrap().reference,
        before
    );
    assert!(
        !a.pending_requests
            .values()
            .any(|r| matches!(r, RequestKind::CancelTurn(_)))
    );
}

#[test]
fn file_scrollbar_drag_is_local_and_focus_loss_releases_it() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut a = app();
    a.terminal_size = (80, 24);
    let r = take_requests(file(&mut a, "rows")).remove(0);
    respond(
        &mut a,
        &r,
        page("rows", &"row\n".repeat(100), 1, Value::Null, "r"),
    );
    layout(&mut a);
    let body = a.main_body_area();
    for (kind, row) in [
        (MouseEventKind::Down(MouseButton::Left), body.y),
        (MouseEventKind::Drag(MouseButton::Left), body.bottom() - 1),
    ] {
        a.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: body.right() - 1,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }
    assert!(a.file_preview().unwrap().offset > 0);
    assert!(a.file_preview().unwrap().scrollbar_grab.is_some());
    assert_eq!(a.active_view().unwrap().scroll.offset, 0);
    a.update(AppEvent::Terminal(CrosstermEvent::FocusLost));
    assert!(a.file_preview().unwrap().scrollbar_grab.is_none());
}

#[test]
fn workspace_dock_owns_focus_and_places_ime_caret_by_visible_cells() {
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
        layout::Rect,
    };
    let mut a = app();
    a.terminal_size = (80, 24);
    let r = take_requests(file(&mut a, "source")).remove(0);
    respond(&mut a, &r, page("source", "data", 1, Value::Null, "r"));
    layout(&mut a);
    for kind in [BrowserKind::Files, BrowserKind::Grep] {
        a.open_workspace_browser(kind, "中🙂".into(), false);
        assert_eq!(
            a.focused_region(),
            if kind == BrowserKind::Files {
                Focus::Dock
            } else {
                Focus::Search
            }
        );
        let screen = crate::ui::layout::screen_layout(&a, Rect::new(0, 0, 80, 24));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &a)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!(cursor.x, screen.panel.x + 13); // nine prefix cells + two wide glyphs
        assert_eq!(cursor.y, screen.panel.y + 1);
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Dock"));
        a.workspace_edit(None, false, true, false);
        a.workspace_edit(Some("路径"), false, false, false);
        terminal.draw(|f| crate::ui::render(f, &a)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!(cursor.y, screen.panel.y + 2);
        assert_eq!(
            cursor.x,
            screen.panel.x + if kind == BrowserKind::Files { 17 } else { 13 }
        );
    }
}

#[test]
fn excessive_paths_malformed_ranges_and_candidate_retention_stop_explicitly() {
    let mut a = app();
    a.open_workspace_browser(BrowserKind::Grep, "needle".into(), false);
    if let Dock::Workspace(b) = &mut a.dock {
        b.scope = serde_json::to_string(&vec!["x"; 33]).unwrap();
    }
    assert!(a.workspace_more(true).is_empty());
    assert!(a.workspace_browser().unwrap().error.is_some());
    if let Dock::Workspace(b) = &mut a.dock {
        b.scope.clear();
    }
    let r = take_requests(a.workspace_more(true)).remove(0);
    let mut malformed = fixture("workspace-search");
    malformed["matches"][0]["line_text"] = "中".into();
    respond(&mut a, &r, malformed);
    assert!(a.workspace_browser().unwrap().error.is_some());
    assert!(a.workspace_browser().unwrap().matches.is_empty());
    a.open_workspace_browser(BrowserKind::Files, String::new(), false);
    for page_no in 0..5 {
        let r = take_requests(if page_no == 0 {
            a.workspace_more(true)
        } else {
            a.workspace_more(false)
        })
        .remove(0);
        let paths: Vec<_> = (0..100).map(|i| format!("p-{page_no}-{i}")).collect();
        let refs: Vec<_> = paths.iter().map(String::as_str).collect();
        respond(
            &mut a,
            &r,
            files(&refs, json!({"entry":page_no+1,"scope":"opaque"})),
        );
    }
    let b = a.workspace_browser().unwrap();
    assert_eq!(b.len(), 500);
    assert!(b.limited);
    assert!(a.workspace_more(false).is_empty());
}
