//! Wire-shaped Summary/display history fixtures, not provider-generation E2E tests.
use super::*;
use crate::protocol::{RequestId, RpcResponse};
use crate::state::session::ManualCompactState;
use crate::state::view::FoldOverride;
use serde_json::json;

const SESSION: &str = "ses_summary";

fn app() -> App {
    let mut app = App::new(std::path::PathBuf::from("/project"));
    app.connection = ConnectionState::Ready;
    let info = serde_json::from_value(json!({
        "session_id": SESSION, "title": null, "profile": "coding",
        "workspace": "/project", "model": "deep", "reasoning": "high",
        "loaded": true, "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    app.sessions
        .known
        .insert(SESSION.to_owned(), SessionView::new(info));
    app.sessions.active = Some(SESSION.to_owned());
    app
}

fn read_request(app: &mut App, commands: Vec<AppCommand>) -> ReadRequest {
    commands
        .into_iter()
        .find_map(|command| {
            let AppCommand::Rpc(request) = command else {
                return None;
            };
            if request.method != "session.read" {
                return None;
            }
            // These fixtures apply the page directly rather than passing an
            // RPC response through App::update. Mirror its query-slot release
            // as well as removing RequestKind, or repeated reads exhaust the
            // admission budget before they ever reach history projection.
            assert!(app.queries.on_query_finished(request.id).is_some());
            match app.pending_requests.remove(&request.id) {
                Some(RequestKind::History { read, .. }) => Some(read),
                _ => None,
            }
        })
        .expect("authoritative session.read")
}

fn summary_page(app: &App, text: &str, revision: char) -> crate::protocol::ReadSessionResult {
    let data = json!({"item": {"type": "summary", "data": {"content": text}}}).to_string();
    serde_json::from_value(json!({
        "session": app.sessions.known[SESSION].info,
        "items": [{"index": 0, "offset": 0, "total_bytes": data.len(),
            "encoding": "utf8_json", "data": data, "complete": true}],
        "total": 1, "records": [], "records_truncated": false,
        "history_revision": revision.to_string().repeat(64),
        "captured_end": 1, "trailing_incomplete": false
    }))
    .unwrap()
}

fn begin_history_read(app: &mut App) -> ReadRequest {
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.history_read.begin(HistoryTrigger::Refresh);
    let command = app
        .request_history(&SESSION.to_owned())
        .expect("history refresh");
    read_request(app, vec![command])
}

fn install_page(app: &mut App, text: &str, revision: char) {
    let read = begin_history_read(app);
    let page = summary_page(app, text, revision);
    app.continue_read_chain(&SESSION.to_owned(), &read, &page);
}

#[test]
fn compaction_summary_read_revision_reuse_and_reopen_default_to_collapsed() {
    let mut app = app();
    install_page(&mut app, "old summary", 'a');
    assert_summary_owner(&app, "old summary");
    for choice in [FoldOverride::Expanded, FoldOverride::Collapsed] {
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        Arc::make_mut(&mut view.summary_folds).insert(0, choice);
        for _ in 0..3 {
            install_page(&mut app, "old summary", 'a');
            assert_eq!(
                app.sessions.known[SESSION].summary_folds.get(&0),
                Some(&choice)
            );
            assert_summary_owner(&app, "old summary");
        }
    }
    install_page(&mut app, "new summary", 'b');
    assert_summary_owner(&app, "new summary");
    let view = &app.sessions.known[SESSION];
    assert!(view.summary_folds.is_empty());
    assert_eq!(view.transcript.blocks.len(), 1);
    assert!(
        matches!(view.transcript.blocks[0].as_ref(), TranscriptBlock::Summary(summary) if summary.content == "new summary")
    );
    let snapshot = crate::ui::transcript::DurableLayoutSnapshot::from_view(view);
    assert!(Arc::ptr_eq(&snapshot.blocks, &view.transcript.blocks));
    assert!(Arc::ptr_eq(&snapshot.summary_folds, &view.summary_folds));
    let info = view.info.clone();
    app.sessions
        .known
        .insert(SESSION.to_owned(), SessionView::new(info));
    install_page(&mut app, "new summary", 'b');
    assert!(app.sessions.known[SESSION].summary_folds.is_empty());
    assert_summary_owner(&app, "new summary");
}

fn assert_summary_owner(app: &App, content: &str) {
    let view = &app.sessions.known[SESSION];
    assert_eq!(view.transcript.blocks.len(), 1);
    let block = &view.transcript.blocks[0];
    assert!(
        matches!(block.as_ref(), TranscriptBlock::Summary(summary) if summary.content == content)
    );
    let resident = view
        .transcript
        .window
        .item(0)
        .expect("resident summary owner");
    assert!(Arc::ptr_eq(block, resident));
    assert_eq!(
        view.summary_history_revision.as_deref(),
        view.transcript.window.pin().map(|pin| pin
            .projection
            .as_ref()
            .map_or(pin.history_revision.as_str(), |projection| projection
                .revision
                .as_str()))
    );
    assert!(view.transcript.complete);
}

#[test]
fn compaction_summary_first_authority_rebuilds_an_existing_owner_without_losing_body() {
    let mut app = app();
    install_page(&mut app, "resident summary", 'a');
    // A preexisting projection without a summary authority must not retain a
    // local index's fold choice. The fresh probe also resets resident owners,
    // so decode dedup cannot leave the summary missing from the projection.
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.summary_history_revision = None;
    Arc::make_mut(&mut view.summary_folds).insert(0, FoldOverride::Expanded);
    install_page(&mut app, "resident summary", 'a');
    assert!(app.sessions.known[SESSION].summary_folds.is_empty());
    assert_summary_owner(&app, "resident summary");
}

#[test]
fn compaction_summary_disappears_when_new_revision_reuses_index_for_non_summary() {
    let mut app = app();
    install_page(&mut app, "old summary", 'a');
    let read = begin_history_read(&mut app);
    let mut page = summary_page(&app, "unused", 'b');
    let data = json!({"item": {"type": "user", "data": {
        "loop_id": "loop-new", "kind": "prompt", "input": {"text": "new prompt"}
    }}})
    .to_string();
    page.items[0].total_bytes = data.len();
    page.items[0].data = data;
    app.continue_read_chain(&SESSION.to_owned(), &read, &page);
    assert!(
        app.sessions.known[SESSION]
            .transcript
            .blocks
            .iter()
            .all(|block| !matches!(block.as_ref(), TranscriptBlock::Summary(_)))
    );
}

#[test]
fn compaction_summary_manual_receipts_wait_for_context_and_never_synthesize_body() {
    for (status, failure) in [
        ("compacted", None),
        ("noop", None),
        ("failed", Some("cancelled")),
        ("failed", Some("store")),
        ("unknown_write", Some("write_outcome_unknown")),
    ] {
        for has_summary in [false, true] {
            for read_inflight in [false, true] {
                let mut app = app();
                app.composer.type_text("preserve compaction draft");
                if has_summary {
                    install_page(&mut app, "existing history summary", 'a');
                }
                let pending_read = read_inflight.then(|| begin_history_read(&mut app));
                let view = &app.sessions.known[SESSION];
                let blocks = Arc::clone(&view.transcript.blocks);
                let generation = view.history_query_generation;
                app.sessions.known.get_mut(SESSION).unwrap().manual_compact =
                    Some(ManualCompactState {
                        operation_id: "summary-op".to_owned(),
                        cancel_requested: false,
                        result: None,
                        state_refresh_confirmed: false,
                        context_refresh_confirmed: false,
                    });
                let response = RpcResponse {
                    id: RequestId(1),
                    error: None,
                    result: Some(json!({"operation_id": "summary-op", "status": status,
                        "covered_loop_count": 1, "covered_item_count": 2, "retained_item_count": 0,
                        "failure_kind": failure})),
                };
                let commands =
                    app.on_compact_response(&SESSION.to_owned(), "summary-op", &response);
                assert_eq!(app.composer.content(), "preserve compaction draft");
                let view = &app.sessions.known[SESSION];
                assert_eq!(view.compaction_feedback.len(), 1);
                assert_eq!(view.compaction_feedback[0].origin_label(), "manual");
                let feedback = view.compaction_feedback.clone();
                assert!(Arc::ptr_eq(&blocks, &view.transcript.blocks), "{status}");
                assert_eq!(view.history_query_generation, generation, "{status}");
                assert!(!view.history_read.post_wait_pending(), "{status}");
                assert!(
                    !commands.iter().any(|command| matches!(command,
                    AppCommand::Rpc(request) if request.method == "session.read")),
                    "{status}"
                );
                if let Some(read) = pending_read {
                    let page = summary_page(&app, "existing history summary", 'a');
                    let commands = app.continue_read_chain(&SESSION.to_owned(), &read, &page);
                    assert!(
                        !commands.iter().any(|command| matches!(command,
                        AppCommand::Rpc(request) if request.method == "session.read")),
                        "no deferred compaction reread: {status}"
                    );
                    assert_summary_owner(&app, "existing history summary");
                    assert_eq!(app.sessions.known[SESSION].compaction_feedback, feedback);
                }
            }
        }
    }
}

#[test]
fn compaction_summary_existing_post_turn_reconciliation_projects_history_items() {
    let mut app = app();
    let turn = TurnRef {
        session_id: SESSION.to_owned(),
        loop_id: "loop-auto".to_owned(),
    };
    let commands = app.reconcile_after_wait(&turn);
    let read = read_request(&mut app, commands);
    let page = summary_page(&app, "synthetic history summary", 'a');
    app.continue_read_chain(&SESSION.to_owned(), &read, &page);
    assert!(matches!(
        app.sessions.known[SESSION].transcript.blocks[0].as_ref(),
        TranscriptBlock::Summary(_)
    ));
    assert!(app.sessions.known[SESSION].summary_folds.is_empty());
}

#[tokio::test]
async fn compaction_summary_body_is_installed_only_after_owned_decode_worker_completes() {
    let mut app = app();
    app.enable_async_decode();
    install_page(&mut app, "worker-owned summary", 'a');
    assert!(app.sessions.known[SESSION].transcript.blocks.is_empty());
    let request = app.pending_decode_request().expect("decode handoff");
    let mut jobs = crate::jobs::LocalJobs::new();
    assert!(jobs.try_schedule_decode(request));
    app.mark_decode_scheduled();
    app.update(jobs.events().recv().await.unwrap());
    assert!(
        matches!(app.sessions.known[SESSION].transcript.blocks[0].as_ref(), TranscriptBlock::Summary(summary) if summary.content == "worker-owned summary")
    );
    jobs.shutdown().await;
}

fn summary_match() -> crate::state::search::SearchMatch {
    crate::state::search::SearchMatch {
        index: Some(0),
        source: crate::state::search::SearchSource::Summary,
        loop_id: None,
        request_index: None,
        ordinal: 0,
        tool_call_id: None,
        preview: "needle".to_owned(),
        source_offset: 0,
        byte_range: 0..6,
    }
}

#[test]
fn compaction_summary_search_expands_then_restores_in_the_owning_session() {
    let mut app = app();
    install_page(&mut app, "needle", 'a');
    app.jump_to_match(&summary_match());
    assert_eq!(
        app.sessions.known[SESSION].summary_folds.get(&0),
        Some(&FoldOverride::Expanded)
    );
    let mut other = SessionView::new(app.sessions.known[SESSION].info.clone());
    other.info.session_id = "other".to_owned();
    Arc::make_mut(&mut other.summary_folds).insert(0, FoldOverride::Expanded);
    app.sessions.known.insert("other".to_owned(), other);
    app.sessions.active = Some("other".to_owned());
    app.close_search();
    assert!(app.sessions.known[SESSION].summary_folds.is_empty());
    assert_eq!(
        app.sessions.known["other"].summary_folds.get(&0),
        Some(&FoldOverride::Expanded)
    );
}

#[test]
fn compaction_summary_search_restore_cannot_resurrect_old_revision_fold() {
    let mut app = app();
    install_page(&mut app, "needle", 'a');
    Arc::make_mut(&mut app.sessions.known.get_mut(SESSION).unwrap().summary_folds)
        .insert(0, FoldOverride::Collapsed);
    app.jump_to_match(&summary_match());
    install_page(&mut app, "replacement summary", 'b');
    app.close_search();
    assert!(app.sessions.known[SESSION].summary_folds.is_empty());
}

#[test]
fn compaction_summary_mouse_click_expands_and_collapses() {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut app = app();
    app.terminal_size = (100, 40);
    install_page(&mut app, "# Details\n\nSummary body", 'a');
    for expected in [FoldOverride::Expanded, FoldOverride::Collapsed] {
        app.last_click = None; // Two ordinary clicks, not a word-selection double click.
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 100, 40));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let section = prepared
            .sections
            .iter()
            .find(|section| section.id.kind == crate::state::view::SectionKind::Summary)
            .unwrap();
        let position = crate::ui::transcript::scroll_position(
            &app,
            prepared.total_rows(),
            screen.transcript.height as usize,
        );
        let row = screen.transcript.y + (section.rows.start + 1 - position.offset) as u16;
        let column = screen.content.x + 4;
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.update(crate::event::AppEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })));
        }
        assert_eq!(
            app.sessions.known[SESSION].summary_folds.get(&0),
            Some(&expected)
        );
    }
}

#[test]
fn compaction_summary_copy_message_uses_complete_source_without_header() {
    let mut app = app();
    app.terminal_size = (100, 40);
    app.viewport = (100, 30);
    let source = "# Decisions\n\nKeep **this**.\n\n```rust\nlet x = 1;\n```";
    install_page(&mut app, source, 'a');
    for folded in [FoldOverride::Collapsed, FoldOverride::Expanded] {
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        Arc::make_mut(&mut view.summary_folds).insert(0, folded);
        view.transcript.invalidate();
        let commands = app.copy_command(crate::command::CopyTarget::Message);
        let text = commands
            .iter()
            .find_map(|command| match command {
                AppCommand::CopySelection(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("summary source copy");
        assert_eq!(text, source);
    }
}

fn compact_context(app: &mut App, status: &str, operation: &str) -> Vec<AppCommand> {
    let mut context: serde_json::Value = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string("tests/fixtures/agent-v1/session-context-idle.json").unwrap(),
    )
    .unwrap()["result"]
        .clone();
    context["session_id"] = SESSION.into();
    context["last_result"] =
        json!({"operation_id": operation, "status": status, "origin": "automatic"});
    let generation = app.sessions.known[SESSION].context_query_generation;
    app.on_session_context_response(
        &SESSION.to_owned(),
        generation,
        ContextQueryOwner::Explicit,
        &RpcResponse {
            id: RequestId(99),
            error: None,
            result: Some(context),
        },
    )
}

#[test]
fn compaction_summary_new_compacted_context_re_pins_once_non_success_never_reads() {
    for status in ["compacted", "noop", "failed", "unknown_write"] {
        let mut app = app();
        install_page(&mut app, "old summary", 'a');
        let commands = compact_context(&mut app, status, "op-1");
        let reads = commands
            .iter()
            .filter(|command| {
                matches!(command,
            AppCommand::Rpc(request) if request.method == "session.read")
            })
            .count();
        assert_eq!(reads, usize::from(status == "compacted"), "{status}");
        assert_summary_owner(&app, "old summary"); // Receipts never become body.
        let repeated = compact_context(&mut app, status, "op-1");
        assert!(!repeated.iter().any(|command| matches!(command,
            AppCommand::Rpc(request) if request.method == "session.read")));
        if status == "compacted" {
            // Metadata refinement of the same terminal operation is not a
            // second compaction and must not queue another history refresh.
            let mut context = app.sessions.known[SESSION].context.clone().unwrap();
            context.last_result.as_mut().unwrap().after_tokens = Some(123);
            let generation = app.sessions.known[SESSION].context_query_generation;
            let repeated = app.on_session_context_response(
                &SESSION.to_owned(),
                generation,
                ContextQueryOwner::Explicit,
                &RpcResponse {
                    id: RequestId(99),
                    error: None,
                    result: Some(serde_json::to_value(context).unwrap()),
                },
            );
            assert!(!repeated.iter().any(|command| matches!(command,
                AppCommand::Rpc(request) if request.method == "session.read")));
            let read = read_request(&mut app, commands);
            assert!(read.probe && read.pin.is_none());
            let page = summary_page(&app, "new summary", 'b');
            app.continue_read_chain(&SESSION.to_owned(), &read, &page);
            assert_summary_owner(&app, "new summary");
            assert!(app.sessions.known[SESSION].summary_folds.is_empty());
        }
    }
}

#[test]
fn compaction_summary_refresh_queues_behind_existing_chain_and_preserves_stronger_triggers() {
    for active in [
        HistoryTrigger::Refresh,
        HistoryTrigger::Gap,
        HistoryTrigger::PostWait,
    ] {
        for pending in [
            None,
            Some(HistoryTrigger::Gap),
            Some(HistoryTrigger::PostWait),
        ] {
            let mut app = app();
            install_page(&mut app, "old", 'a');
            let read = begin_history_read(&mut app);
            let view = app.sessions.known.get_mut(SESSION).unwrap();
            view.history_read.begin(active);
            if let Some(pending) = pending {
                view.history_read.defer(pending);
            }
            let generation = view.history_query_generation;
            assert!(app.refresh_history_view(&SESSION.to_owned()).is_empty());
            assert_eq!(
                app.sessions.known[SESSION].history_query_generation,
                generation
            );
            let page = summary_page(&app, "old", 'a');
            let commands = app.continue_read_chain(&SESSION.to_owned(), &read, &page);
            let next = read_request(&mut app, commands);
            assert!(next.probe && next.pin.is_none());
            assert_eq!(next.cursor, ReadCursor::start());
            let view = &app.sessions.known[SESSION];
            assert_eq!(view.history_read.is_reconciling(), pending.is_some());
            let page = summary_page(&app, "fresh", 'b');
            assert!(
                app.continue_read_chain(&SESSION.to_owned(), &next, &page)
                    .is_empty()
            );
            assert_summary_owner(&app, "fresh");
        }
    }
}

#[test]
fn compaction_summary_projection_revision_resets_folds_and_search_restore() {
    let mut app = app();
    for (revision, body) in [('b', "needle old"), ('c', "needle fresh")] {
        let read = begin_history_read(&mut app);
        let mut page = summary_page(&app, body, 'a'); // Raw archive unchanged.
        let data = json!({"display": true, "derived_summary": true,
            "item": {"type": "summary", "data": {"content": body}}})
        .to_string();
        page.items[0].total_bytes = data.len();
        page.items[0].data = data;
        page.projection = Some(serde_json::from_value(json!({
            "revision": revision.to_string().repeat(64), "first_item": 0, "covered_item_count": 1,
            "covered_usage": {"usage": {}, "loop_count": 0, "last_loop_id": null, "partial": false}
        })).unwrap());
        app.continue_read_chain(&SESSION.to_owned(), &read, &page);
        assert_summary_owner(&app, body);
        assert!(app.sessions.known[SESSION].summary_folds.is_empty());
        if revision == 'b' {
            app.jump_to_match(&summary_match());
        }
    }
    app.close_search();
    assert!(app.sessions.known[SESSION].summary_folds.is_empty());
}

#[test]
fn compaction_summary_ctrl_o_includes_summary_and_preserves_draft() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    app.terminal_size = (100, 40);
    install_page(&mut app, "body", 'a');
    app.composer.type_text("keep draft");
    for expected in [FoldOverride::Expanded, FoldOverride::Collapsed] {
        app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(
            app.sessions.known[SESSION].summary_folds.get(&0),
            Some(&expected)
        );
        assert_eq!(app.composer.content(), "keep draft");
    }
}

#[test]
fn compaction_summary_refresh_failure_hints_at_conversation_retry_and_keeps_old_body() {
    let mut app = app();
    install_page(&mut app, "old", 'a');
    let commands = app.refresh_history_view(&SESSION.to_owned());
    let read = read_request(&mut app, commands);
    app.on_history_response(
        &SESSION.to_owned(),
        &read,
        &RpcResponse {
            id: RequestId(99),
            result: Some(json!({})),
            error: None,
        },
    );
    assert!(app.notices.back().unwrap().text.contains("/refresh"));
    assert!(
        matches!(app.sessions.known[SESSION].transcript.blocks[0].as_ref(),
        TranscriptBlock::Summary(summary) if summary.content == "old")
    );
    let commands = app.refresh_history_view(&SESSION.to_owned());
    let retry = read_request(&mut app, commands);
    assert!(retry.probe && retry.pin.is_none());
}

fn idle(app: &mut App) {
    app.sessions.known.get_mut(SESSION).unwrap().state = Some(
        serde_json::from_value(json!({
            "session_id": SESSION, "status": "idle", "active_loop": null, "block_reason": null
        }))
        .unwrap(),
    );
}

fn history_page(
    app: &App,
    total: usize,
    start: usize,
    count: usize,
    display: bool,
) -> crate::protocol::ReadSessionResult {
    let end = (start + count).min(total);
    let chunks = (start..end).map(|index| {
        let data = json!({"item": {"type": "user", "data": {
            "loop_id": format!("loop-{index}"), "kind": "prompt", "input": {"text": "history"}
        }}}).to_string();
        json!({"index": index, "offset": 0, "total_bytes": data.len(), "encoding": "utf8_json", "data": data, "complete": true})
    }).collect::<Vec<_>>();
    let mut page = json!({"session": app.sessions.known[SESSION].info, "total": total,
        "items": chunks, "records": [], "records_truncated": false,
        "history_revision": "a".repeat(64), "captured_end": total, "trailing_incomplete": false,
        "next_cursor": (end < total).then(|| json!({"item": end, "offset": 0}))});
    if display {
        page["projection"] = json!({"revision": "b".repeat(64), "first_item": 0,
            "covered_item_count": 0, "covered_usage": {"usage": {}, "loop_count": 0,
                "last_loop_id": null, "partial": false}});
    }
    serde_json::from_value(page).unwrap()
}

#[test]
fn manual_compact_settled_tail_uses_actual_pagination_without_loading_missing_prefix() {
    for total in [0, 199, 200, 201, 227] {
        for display in [false, true] {
            // No summary, including covered_count=0 display.
            let mut app = app();
            idle(&mut app);
            let mut read = begin_history_read(&mut app);
            let mut cursors = Vec::new();
            loop {
                cursors.push(read.cursor.item);
                assert!(!app.can_manual_compact(), "read in progress: {total}");
                let count = if read.probe {
                    1
                } else {
                    crate::protocol::READ_PAGE_LIMIT
                };
                let page = history_page(&app, total, read.cursor.item, count, display);
                let commands = app.continue_read_chain(&SESSION.to_owned(), &read, &page);
                if commands.is_empty() {
                    break;
                }
                read = read_request(&mut app, commands);
            }
            let view = &app.sessions.known[SESSION];
            assert_eq!(view.transcript.complete, total <= 200);
            assert!(view.transcript.next_cursor.is_none());
            if total > 200 {
                assert_eq!(cursors[1], total - 200);
                assert_eq!(view.transcript.window.loaded_ranges().len(), 1);
                assert_eq!(
                    view.transcript.window.loaded_ranges()[0],
                    total - 200..total
                );
            }
            assert!(
                app.can_manual_compact(),
                "settled tail: {total} display={display}"
            );
            let commands = app.start_manual_compact();
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| matches!(command,
                AppCommand::Rpc(request) if request.method == "session.compact"))
                    .count(),
                1
            );
            assert!(app.start_manual_compact().is_empty());
        }
    }
}

#[test]
fn manual_compact_rejects_unfinished_large_item_but_allows_complete_placeholder() {
    for complete in [false, true] {
        let mut app = app();
        idle(&mut app);
        let read = begin_history_read(&mut app);
        let bytes = crate::protocol::read::MAX_AUTO_ITEM_BYTES + 1;
        let data = if complete {
            "x".repeat(bytes)
        } else {
            "x".to_owned()
        };
        let page = serde_json::from_value(json!({
            "session": app.sessions.known[SESSION].info, "total": 1,
            "items": [{"index": 0, "offset": 0, "total_bytes": bytes,
                "encoding": "utf8_json", "data": data, "complete": complete}],
            "history_revision": "a".repeat(64), "captured_end": 1, "trailing_incomplete": false,
            "next_cursor": (!complete).then(|| json!({"item": 0, "offset": 1}))
        }))
        .unwrap();
        assert!(
            app.continue_read_chain(&SESSION.to_owned(), &read, &page)
                .is_empty()
        );
        assert!(!app.sessions.known[SESSION].history_read.is_loading());
        assert_eq!(app.can_manual_compact(), complete);
        if !complete {
            let commands = app.refresh_history_view(&SESSION.to_owned());
            let retry = read_request(&mut app, commands);
            assert!(retry.probe && retry.pin.is_none());
        }
    }
}

#[test]
fn manual_compact_shared_safety_and_unsettled_reads_remain_fenced() {
    for condition in [
        "postwait",
        "gap",
        "closing",
        "unknown",
        "blocked",
        "loading",
        "decode",
        "unloaded",
        "preparing",
    ] {
        let mut app = app();
        idle(&mut app);
        install_page(&mut app, "body", 'a');
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        match condition {
            "postwait" => view.history_read.defer(HistoryTrigger::PostWait),
            "gap" => view.event_gap = true,
            "closing" => view.closing = true,
            "unknown" => view.result_confirmation = ResultConfirmation::Unknown,
            "live" => view.live = Some(LiveLoop::new(LocalSubmissionId(1), "live".into())),
            "unsaved" => {
                view.unsaved_loop = Some(UnsavedLoop {
                    turn: TurnRef {
                        session_id: SESSION.into(),
                        loop_id: "unsaved".into(),
                    },
                    user_text: "keep".into(),
                    requests: vec![],
                    result: None,
                    event_gap: false,
                })
            }
            "blocked" => view.state.as_mut().unwrap().status = SessionStatusWire::Blocked,
            "loading" => view.history_read.begin(HistoryTrigger::Refresh),
            "unloaded" => view.info.loaded = false,
            "preparing" => {
                view.manual_compact = Some(ManualCompactState {
                    operation_id: "active".into(),
                    cancel_requested: false,
                    result: None,
                    state_refresh_confirmed: false,
                    context_refresh_confirmed: false,
                })
            }
            "decode" => {
                app.enable_async_decode();
                install_page(&mut app, "decoding", 'b');
            }
            _ => unreachable!(),
        }
        assert!(!app.can_manual_compact(), "{condition}");
        assert!(app.start_manual_compact().is_empty(), "{condition}");
    }
}

#[test]
fn compaction_summary_queued_fresh_read_keeps_generation_and_gate_until_admitted() {
    use crate::app::queries::{QueryAdmission, QueryKey, QuerySlots};
    for (backlog_full, trigger) in [
        (false, HistoryTrigger::Refresh),
        (true, HistoryTrigger::PostWait),
        (true, HistoryTrigger::Gap),
    ] {
        let mut app = app();
        idle(&mut app);
        install_page(&mut app, "old", 'a');
        for index in 0..QuerySlots::CAPACITY {
            assert_eq!(
                app.queries.request_query(
                    QueryKey::Changes {
                        session_id: format!("busy-{index}")
                    },
                    RequestId(900 + index as u64)
                ),
                QueryAdmission::Admitted
            );
        }
        if backlog_full {
            for index in 0..QuerySlots::MAX_WAITING {
                assert_eq!(
                    app.queries.request_query(
                        QueryKey::Changes {
                            session_id: format!("queued-{index}")
                        },
                        RequestId(1000 + index as u64)
                    ),
                    QueryAdmission::Busy
                );
            }
        }
        if backlog_full {
            app.sessions
                .known
                .get_mut(SESSION)
                .unwrap()
                .history_read
                .defer(trigger);
        }
        assert!(app.refresh_history_view(&SESSION.to_owned()).is_empty());
        let generation = app.sessions.known[SESSION].history_query_generation;
        if backlog_full {
            assert_eq!(
                app.sessions.known[SESSION].history_read.post_wait_pending(),
                trigger == HistoryTrigger::PostWait
            );
            assert_eq!(
                app.sessions.known[SESSION].history_read.is_reconciling(),
                trigger == HistoryTrigger::Gap
            );
            assert!(!app.can_manual_compact());
            assert!(!app.sessions.known[SESSION].history_read.is_loading());
            // A declined intent remains explicitly retryable once the backlog clears.
            app.queries = QuerySlots::new();
            assert!(!app.refresh_history_view(&SESSION.to_owned()).is_empty());
            assert!(app.sessions.known[SESSION].history_read.is_reconciling());
        } else {
            assert!(!app.can_manual_compact());
            app.free_query_slot(RequestId(900));
            let mut commands = Vec::new();
            app.drain_query_followups(&mut commands, false);
            let read = read_request(&mut app, commands);
            assert_eq!(
                app.sessions.known[SESSION].history_query_generation,
                generation
            );
            assert!(read.probe && read.pin.is_none());
            let page = summary_page(&app, "fresh", 'b');
            app.continue_read_chain(&SESSION.to_owned(), &read, &page);
            assert!(app.can_manual_compact());
        }
    }
}

#[test]
fn compaction_summary_ctrl_o_mixes_summary_reasoning_and_tools_with_local_overrides() {
    for with_tools in [false, true] {
        let mut app = if with_tools {
            crate::ui::testapp::tools(ThemeKind::Dark)
        } else {
            crate::ui::testapp::chat_with_reasoning(ThemeKind::Dark)
        };
        app.terminal_size = (100, 40);
        let mut source = self::app();
        install_page(&mut source, "summary body", 'a');
        let summary = Arc::clone(&source.sessions.known[SESSION].transcript.blocks[0]);
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        if !with_tools {
            for block in view.transcript.blocks_mut() {
                if let TranscriptBlock::Assistant(assistant) = Arc::make_mut(block) {
                    for part in &mut assistant.parts {
                        if let AssistantPart::Reasoning(text) = part {
                            *text = "long reasoning line\n".repeat(10);
                        }
                    }
                }
            }
        }
        view.transcript.blocks_mut().insert(0, summary);
        Arc::make_mut(&mut view.summary_folds).insert(0, FoldOverride::Expanded);
        view.transcript.invalidate();
        for _ in 0..2 {
            app.update(AppEvent::ToggleTools {
                session_id: "ses_1".into(),
            });
            let prepared = crate::ui::transcript::prepare_conversation(&app, 100);
            let details = prepared
                .sections
                .iter()
                .filter(|section| section.collapsible)
                .collect::<Vec<_>>();
            assert!(
                details.len() >= 2,
                "with_tools={with_tools}: {:?}",
                prepared.sections
            );
            assert!(
                details
                    .iter()
                    .all(|section| section.folded == details[0].folded)
            );
        }
    }
}

#[test]
fn compaction_summary_pending_post_wait_re_pins_even_if_finished_loop_is_already_resident() {
    let mut app = app();
    idle(&mut app);
    let read = begin_history_read(&mut app);
    let mut live = LiveLoop::new(LocalSubmissionId(1), "history".into());
    live.reference = Some(TurnRef {
        session_id: SESSION.into(),
        loop_id: "loop-0".into(),
    });
    live.waiting = true;
    live.last_result = Some(
        serde_json::from_value(json!({
            "turn": {"session_id": SESSION, "loop_id": "loop-0"},
            "outcome": {"type": "completed"}, "persistence": "persisted"
        }))
        .unwrap(),
    );
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.live = Some(live);
    view.history_read.defer(HistoryTrigger::PostWait);
    assert!(app.refresh_history_view(&SESSION.to_owned()).is_empty());
    let page = history_page(&app, 1, 0, 1, false);
    let commands = app.continue_read_chain(&SESSION.to_owned(), &read, &page);
    assert!(app.sessions.known[SESSION].live.is_none());
    let next = read_request(&mut app, commands);
    assert!(next.probe && next.pin.is_none());
}

#[tokio::test]
async fn compaction_summary_refresh_waits_for_owned_decode_before_one_fresh_probe() {
    let mut app = app();
    idle(&mut app);
    app.enable_async_decode();
    install_page(&mut app, "old decoding", 'a');
    let generation = app.sessions.known[SESSION].history_query_generation;
    assert!(app.refresh_history_view(&SESSION.to_owned()).is_empty());
    assert!(!app.can_manual_compact());
    let request = app.pending_decode_request().unwrap();
    let old_identity = request.identity.clone();
    let mut jobs = crate::jobs::LocalJobs::new();
    assert!(jobs.try_schedule_decode(request));
    app.mark_decode_scheduled();
    let commands = app.update(jobs.events().recv().await.unwrap());
    let read = read_request(&mut app, commands);
    assert!(read.probe && read.pin.is_none());
    assert!(app.sessions.known[SESSION].history_query_generation > generation);
    assert_ne!(app.decode_in_flight.as_ref(), Some(&old_identity));
    jobs.shutdown().await;
}

#[test]
fn compaction_summary_failed_context_after_manual_success_has_explicit_retry_hint() {
    let mut app = app();
    install_page(&mut app, "old", 'a');
    app.sessions.known.get_mut(SESSION).unwrap().manual_compact = Some(ManualCompactState {
        operation_id: "op".into(),
        cancel_requested: false,
        result: None,
        state_refresh_confirmed: false,
        context_refresh_confirmed: false,
    });
    let commands = app.on_compact_response(
        &SESSION.to_owned(),
        "op",
        &RpcResponse {
            id: RequestId(1),
            error: None,
            result: Some(json!({"operation_id": "op", "status": "compacted"})),
        },
    );
    let request = crate::ui::testapp::take_requests(commands)
        .into_iter()
        .find(|request| request.method == "session.context")
        .unwrap();
    crate::ui::testapp::respond_rpc_error(&mut app, &request, -32000, "unavailable");
    assert!(
        app.notices
            .back()
            .unwrap()
            .text
            .contains("summary view not refreshed")
    );
    assert!(
        app.notices
            .back()
            .unwrap()
            .text
            .contains("conversation and /refresh")
    );
    assert_summary_owner(&app, "old");
}

#[test]
fn compaction_summary_queued_tail_after_probe_keeps_cursor_pin_and_generation() {
    use crate::app::queries::{QueryAdmission, QueryKey, QuerySlots};
    for backlog_full in [false, true] {
        let mut app = app();
        idle(&mut app);
        let read = begin_history_read(&mut app);
        let generation = app.sessions.known[SESSION].history_query_generation;
        for index in 0..2 {
            assert_eq!(
                app.queries.request_query(
                    QueryKey::Changes {
                        session_id: format!("busy-{index}")
                    },
                    RequestId(900 + index)
                ),
                QueryAdmission::Admitted
            );
        }
        if backlog_full {
            for index in 0..QuerySlots::MAX_WAITING {
                assert_eq!(
                    app.queries.request_query(
                        QueryKey::Changes {
                            session_id: format!("queued-{index}")
                        },
                        RequestId(1000 + index as u64)
                    ),
                    QueryAdmission::Busy
                );
            }
        }
        let page = history_page(&app, 227, 0, 1, true);
        assert!(
            app.continue_read_chain(&SESSION.to_owned(), &read, &page)
                .is_empty()
        );
        assert!(!app.can_manual_compact());
        if backlog_full {
            assert!(!app.sessions.known[SESSION].history_read.is_loading());
            app.queries = QuerySlots::new();
            let commands = app.refresh_history_view(&SESSION.to_owned());
            let retry = read_request(&mut app, commands);
            assert!(retry.probe && retry.pin.is_none());
        } else {
            app.free_query_slot(RequestId(900));
            let mut commands = Vec::new();
            app.drain_query_followups(&mut commands, false);
            let tail = read_request(&mut app, commands);
            assert_eq!(tail.cursor.item, 27);
            assert_eq!(tail.window_start, 27);
            assert!(!tail.probe && !tail.replacement && tail.pin.is_some());
            assert_eq!(
                app.sessions.known[SESSION].history_query_generation,
                generation
            );
        }
    }
}

const COVERED_TERMINAL_LOOP: &str = "loop-covered-terminal";

fn covered_terminal_result() -> crate::protocol::TurnResultViewWire {
    serde_json::from_value(json!({
        "turn":{"session_id":SESSION,"loop_id":COVERED_TERMINAL_LOOP},
        "outcome":{"type":"completed"},"persistence":"persisted",
        "usage":{"input_tokens":100,"output_tokens":20},"requests":52,"tool_rounds":51
    }))
    .unwrap()
}

fn covered_terminal_page(app: &App, partial: bool) -> serde_json::Value {
    let data = json!({"display":true,"derived_summary":true,
        "item":{"type":"summary","data":{"content":"Settled summary"}}})
    .to_string();
    json!({"session":app.sessions.known[SESSION].info,"items":[{
        "index":171,"offset":0,"total_bytes":data.len(),"encoding":"utf8_json",
        "data":data,"complete":true}],"total":172,"records":[],"records_truncated":false,
        "history_revision":"a".repeat(64),"captured_end":1312539,"trailing_incomplete":false,
        "projection":{"revision":"b".repeat(64),"first_item":171,"covered_item_count":172,
            "covered_usage":{"loop_count":1,"last_loop_id":COVERED_TERMINAL_LOOP,
                "usage":{"input_tokens":100,"output_tokens":20},"partial":partial}}})
}

fn attach_covered_terminal_live(app: &mut App) {
    let result = covered_terminal_result();
    let mut live = LiveLoop::new(LocalSubmissionId(77), "finished prompt".into());
    live.reference = Some(result.turn.clone());
    live.waiting = true;
    live.last_result = Some(result.clone());
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.live = Some(live);
    view.last_result = Some(result);
}

fn covered_terminal_fixture() -> (App, ReadRequest) {
    let mut app = app();
    idle(&mut app);
    let read = begin_history_read(&mut app);
    let page = serde_json::from_value(covered_terminal_page(&app, false)).unwrap();
    app.continue_read_chain(&SESSION.to_owned(), &read, &page);
    attach_covered_terminal_live(&mut app);
    (app, read)
}

fn covered_terminal_wire_fixture() -> (App, crate::protocol::OutgoingRequest) {
    use crate::ui::testapp::{respond, take_requests};
    let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, SESSION, None, "high");
    let result = covered_terminal_result();
    app.composer.set_text("finished prompt");
    let send = take_requests(app.submit_composer()).remove(0);
    assert_eq!(send.method, "turn.send");
    let wait = take_requests(respond(
        &mut app,
        &send,
        json!({
            "turn":result.turn,"accepted_at":"2026-01-02T03:04:05Z"
        }),
    ))
    .into_iter()
    .find(|request| request.method == "turn.wait")
    .unwrap();
    let commands = respond(&mut app, &wait, serde_json::to_value(result).unwrap());
    let mut pending = std::collections::VecDeque::from(take_requests(commands));
    let mut history = None;
    while let Some(request) = pending.pop_front() {
        let value = match request.method {
            "session.read" => {
                assert!(history.replace(request).is_none());
                continue;
            }
            "session.state" => json!({"session_id":SESSION,"status":"idle","active_loop":null}),
            "session.context" => json!({"session_id":SESSION,"current_operation":null,
                "last_result":null,"coverage":{"covered_loop_count":0,"covered_item_count":0,"retained_item_count":0},"budget":{},"automatic":{"current":null,"last":null}}),
            "session.presentation" => json!({"session_id":SESSION,"context":{"kind":"unknown"}}),
            other => panic!("unexpected terminal request: {other}"),
        };
        pending.extend(take_requests(respond(&mut app, &request, value)));
    }
    (app, history.expect("post-wait display read"))
}

#[test]
fn summary_only_terminal_wire_read_retires_live_and_admits_a_genuinely_new_turn() {
    use crate::ui::testapp::{respond, take_requests};
    for (partial, unknown_usage) in [(false, false), (true, false), (true, true)] {
        let (mut app, read) = covered_terminal_wire_fixture();
        assert!(app.active_view().unwrap().live.as_ref().unwrap().waiting);
        let mut page = covered_terminal_page(&app, partial);
        if unknown_usage {
            page["projection"]["covered_usage"]["usage"] = json!({});
        }
        assert!(take_requests(respond(&mut app, &read, page)).is_empty());
        let view = app.active_view().unwrap();
        assert!(view.live.is_none());
        assert!(view.transcript.complete);
        assert_eq!(
            view.usage_projection.usage.input_tokens,
            (!unknown_usage).then_some(100)
        );
        assert_eq!(
            view.usage_projection.usage.output_tokens,
            (!unknown_usage).then_some(20)
        );
        assert_eq!(
            crate::ui::footer::footer_view(&app).status,
            crate::ui::footer::FooterStatus::Ready
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 8)).unwrap();
        terminal
            .draw(|frame| {
                crate::ui::composer::render(frame, frame.area(), &app, &ThemeKind::Dark.theme())
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains("Unsupported interaction"));
        assert!(
            !app.notices
                .iter()
                .any(|notice| notice.text.contains("not contained"))
        );
        app.composer.set_text("a genuinely new prompt");
        let requests = take_requests(app.submit_composer());
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "turn.send")
                .count(),
            1
        );
        assert!(
            requests
                .iter()
                .all(|request| request.method != "turn.steer")
        );
    }
}

#[test]
fn summary_only_terminal_proof_preserves_all_identity_and_completion_fences() {
    for invalid in [
        "result_session",
        "result_loop",
        "projection_loop",
        "missing_persistence",
        "failed_persistence",
        "missing_result",
        "needs_read",
        "unknown_result",
        "blocked",
        "closing",
        "unknown_close",
        "unsaved",
        "stale_gap",
        "stale_summary_revision",
        "stale_pin",
        "cursor",
        "zero_loops",
        "zero_items",
        "wrong_boundary",
        "overflow_boundary",
        "outside_pin",
        "absent_summary",
    ] {
        let (mut app, mut read) = covered_terminal_fixture();
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        let pending = Arc::new(TranscriptBlock::User(UserBlock {
            index: None,
            loop_id: Some(COVERED_TERMINAL_LOOP.into()),
            kind: UserMessageKindWire::Prompt,
            text: "uncertain accepted prompt".into(),
            pending: true,
        }));
        view.transcript.blocks_mut().push(pending.clone());
        let revision = view.transcript.render_revision;
        let mut pin = view.transcript.window.pin().unwrap().clone();
        match invalid {
            "result_session" => {
                view.live
                    .as_mut()
                    .unwrap()
                    .last_result
                    .as_mut()
                    .unwrap()
                    .turn
                    .session_id = "other".into()
            }
            "result_loop" => {
                view.live
                    .as_mut()
                    .unwrap()
                    .last_result
                    .as_mut()
                    .unwrap()
                    .turn
                    .loop_id = "other".into()
            }
            "projection_loop" => {
                pin.projection.as_mut().unwrap().covered_usage.last_loop_id = Some("other".into())
            }
            "missing_persistence" => {
                view.live
                    .as_mut()
                    .unwrap()
                    .last_result
                    .as_mut()
                    .unwrap()
                    .persistence = None
            }
            "failed_persistence" => {
                view.live
                    .as_mut()
                    .unwrap()
                    .last_result
                    .as_mut()
                    .unwrap()
                    .persistence = Some(TurnPersistenceWire::Failed)
            }
            "missing_result" => view.live.as_mut().unwrap().last_result = None,
            "needs_read" => view.result_confirmation = ResultConfirmation::NeedsRead,
            "unknown_result" => view.result_confirmation = ResultConfirmation::Unknown,
            "blocked" => view.state.as_mut().unwrap().status = SessionStatusWire::Blocked,
            "closing" => view.closing = true,
            "unknown_close" => view.close_verification_unknown = true,
            "unsaved" => {
                let result = covered_terminal_result();
                view.unsaved_loop = Some(crate::state::turn::UnsavedLoop {
                    turn: result.turn.clone(),
                    user_text: String::new(),
                    requests: Vec::new(),
                    result: Some(result),
                    event_gap: false,
                });
            }
            "stale_gap" => read.gap_revision = view.gap_revision.wrapping_add(1),
            "stale_summary_revision" => view.summary_history_revision = Some("c".repeat(64)),
            "stale_pin" => {
                let mut old = pin.clone();
                old.history_revision = "c".repeat(64);
                read.pin = Some(old);
            }
            "cursor" => {
                view.transcript.next_cursor = Some(ReadCursor {
                    item: 172,
                    offset: 0,
                })
            }
            "zero_loops" => pin.projection.as_mut().unwrap().covered_usage.loop_count = 0,
            "zero_items" => pin.projection.as_mut().unwrap().covered_item_count = 0,
            "wrong_boundary" => pin.projection.as_mut().unwrap().covered_item_count = 171,
            "overflow_boundary" => {
                let p = pin.projection.as_mut().unwrap();
                p.first_item = usize::MAX;
                p.covered_item_count = 0;
            }
            "outside_pin" => pin.total = 171,
            "absent_summary" => view.transcript.window.replace_pin(pin.clone()),
            _ => unreachable!(),
        }
        view.transcript.window.install_pin(pin);
        assert!(
            !App::display_summary_confirms_live_turn(view, &SESSION.to_owned(), &read),
            "{invalid}"
        );
        App::finish_read_chain(view, &SESSION.to_owned(), &read);
        assert!(view.live.is_some(), "{invalid}");
        assert!(
            view.transcript
                .blocks
                .iter()
                .any(|block| Arc::ptr_eq(block, &pending)),
            "{invalid}"
        );
        assert_eq!(view.transcript.render_revision, revision, "{invalid}");
    }
}

#[test]
fn summary_only_terminal_coverage_does_not_invent_steering_outcomes() {
    use crate::state::turn::PendingSteer;
    let (mut app, read) = covered_terminal_fixture();
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    for (index, state) in [
        PendingSteerState::Sending,
        PendingSteerState::Queued,
        PendingSteerState::Unconfirmed,
        PendingSteerState::Persisted,
        PendingSteerState::NotRecorded,
    ]
    .into_iter()
    .enumerate()
    {
        view.live
            .as_mut()
            .unwrap()
            .pending_steers
            .push(PendingSteer {
                local_id: index as u64,
                text: format!("steer-{index}"),
                state,
                accepted_at: None,
                steer_index: None,
            });
    }
    App::finish_read_chain(view, &SESSION.to_owned(), &read);
    assert!(view.live.is_none());
    assert_eq!(
        view.completed_steers
            .iter()
            .map(|steer| steer.state.clone())
            .collect::<Vec<_>>(),
        vec![
            PendingSteerState::Unconfirmed,
            PendingSteerState::Unconfirmed,
            PendingSteerState::Unconfirmed,
            PendingSteerState::Persisted,
            PendingSteerState::NotRecorded,
        ]
    );
}

#[tokio::test]
async fn summary_only_terminal_waits_for_complete_page_and_owned_decode() {
    use crate::ui::testapp::{respond, take_requests};
    let (mut app, read) = covered_terminal_wire_fixture();
    let mut page = covered_terminal_page(&app, false);
    let data = page["items"][0]["data"].as_str().unwrap().to_owned();
    let split = data.len() / 2;
    page["items"][0]["data"] = json!(&data[..split]);
    page["items"][0]["complete"] = json!(false);
    page["next_cursor"] = json!({"item":171,"offset":split});
    let requests = take_requests(respond(&mut app, &read, page));
    assert!(app.active_view().unwrap().live.is_some());
    assert!(app.active_view().unwrap().transcript.blocks.iter().any(
        |block| matches!(block.as_ref(),TranscriptBlock::User(user) if user.pending
            && user.loop_id.as_deref()==Some(COVERED_TERMINAL_LOOP))
    ));
    let read = requests
        .iter()
        .find(|request| request.method == "session.read")
        .unwrap();
    app.enable_async_decode();
    let mut page = covered_terminal_page(&app, false);
    page["items"][0]["offset"] = json!(split);
    page["items"][0]["data"] = json!(&data[split..]);
    respond(&mut app, read, page);
    assert!(
        app.active_view().unwrap().live.is_some(),
        "decode still owns the summary"
    );
    let request = app.pending_decode_request().expect("summary decode");
    let mut jobs = crate::jobs::LocalJobs::new();
    assert!(jobs.try_schedule_decode(request));
    app.mark_decode_scheduled();
    app.update(jobs.events().recv().await.unwrap());
    assert!(app.active_view().unwrap().live.is_none());
    assert!(app.active_view().unwrap().transcript.blocks.iter().all(
        |block| !matches!(block.as_ref(),TranscriptBlock::User(user) if user.pending
            && user.loop_id.as_deref()==Some(COVERED_TERMINAL_LOOP))
    ));
    jobs.shutdown().await;
}

fn covered_prompt_summary_cells(app: &App) -> Vec<ratatui::buffer::Cell> {
    let prepared = crate::ui::transcript::prepare_conversation(app, 80);
    assert!(prepared.total_rows() <= 16);
    let rows = prepared
        .sections
        .iter()
        .find(|section| section.id.kind == crate::state::view::SectionKind::Summary)
        .unwrap()
        .rows
        .clone();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 16)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::transcript::render(
                frame,
                ratatui::layout::Rect::new(0, 0, 80, 16),
                ratatui::layout::Rect::default(),
                app,
                &app.theme.theme(),
            );
        })
        .unwrap();
    // Welcome chrome may coalesce the section's leading blank spacer. The
    // collapsed summary must have exactly one nonblank rendered row in either
    // view; compare every cell in that row, including style and padding.
    let content_rows = terminal.backend().buffer().content[rows.start * 80..rows.end * 80]
        .chunks(80)
        .filter(|row| row.iter().any(|cell| !cell.symbol().trim().is_empty()))
        .collect::<Vec<_>>();
    assert_eq!(content_rows.len(), 1);
    content_rows[0].to_vec()
}

#[test]
fn covered_pending_prompt_retirement_matches_cold_view_and_keeps_next_ack_owner() {
    use crate::ui::testapp::{respond, take_requests};
    let (mut live_app, read) = covered_terminal_wire_fixture();
    let before = live_app.active_view().unwrap();
    assert!(
        before
            .transcript
            .blocks
            .iter()
            .any(|block| matches!(block.as_ref(),
        TranscriptBlock::User(user) if user.pending && user.index.is_none()
            && user.kind==UserMessageKindWire::Prompt && user.text=="finished prompt"
            && user.loop_id.as_deref()==Some(COVERED_TERMINAL_LOOP)))
    );
    assert_eq!(
        before.live_user_timestamp.as_deref(),
        Some("2026-01-02T03:04:05Z")
    );
    let page = covered_terminal_page(&live_app, false);
    assert!(take_requests(respond(&mut live_app, &read, page)).is_empty());
    let view = live_app.active_view().unwrap();
    assert!(view.live.is_none());
    assert_eq!(
        view.transcript.blocks.len(),
        1,
        "only the authoritative summary remains"
    );
    assert!(matches!(
        view.transcript.blocks[0].as_ref(),
        TranscriptBlock::Summary(_)
    ));

    let mut cold = app();
    idle(&mut cold);
    let read = begin_history_read(&mut cold);
    let page = serde_json::from_value(covered_terminal_page(&cold, false)).unwrap();
    cold.continue_read_chain(&SESSION.to_owned(), &read, &page);
    assert_eq!(
        live_app.active_view().unwrap().transcript.blocks,
        cold.active_view().unwrap().transcript.blocks
    );
    assert_eq!(
        covered_prompt_summary_cells(&live_app),
        covered_prompt_summary_cells(&cold)
    );

    live_app.composer.set_text("second accepted prompt");
    let send = take_requests(live_app.submit_composer()).remove(0);
    assert_eq!(send.method, "turn.send");
    let requests = take_requests(respond(
        &mut live_app,
        &send,
        json!({
            "turn":{"session_id":SESSION,"loop_id":"loop-next-accepted"},
            "accepted_at":"2026-01-02T04:05:06Z"
        }),
    ));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "turn.wait")
            .count(),
        1
    );
    let view = live_app.active_view().unwrap();
    let prompts = view
        .transcript
        .blocks
        .iter()
        .filter_map(|block| match block.as_ref() {
            TranscriptBlock::User(user) => Some(user),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].text, "second accepted prompt");
    assert_eq!(prompts[0].loop_id.as_deref(), Some("loop-next-accepted"));
    assert!(prompts[0].pending && prompts[0].index.is_none());
    assert_eq!(
        view.live_user_timestamp.as_deref(),
        Some("2026-01-02T04:05:06Z")
    );
    assert!(view.live_user_time_accepted);
    assert!(live_app.composer.is_empty());
}

#[test]
fn covered_pending_prompt_cleanup_preserves_all_other_cards_drafts_and_queue() {
    use crate::state::turn::{SteerQueueItem, SteerQueueState};
    for include_owned_prompt in [false, true] {
        let (mut app, read) = covered_terminal_fixture();
        app.composer.set_text("do not submit this draft");
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        // These deliberately unbound synthetic cards never pass through a new
        // send/ACK. Generic first-binding behavior is outside this regression.
        let kept = [
            (None, None, true, UserMessageKindWire::Prompt),
            (None, Some("future-loop"), true, UserMessageKindWire::Prompt),
            (
                Some(171),
                Some(COVERED_TERMINAL_LOOP),
                false,
                UserMessageKindWire::Prompt,
            ),
            (
                Some(171),
                Some(COVERED_TERMINAL_LOOP),
                true,
                UserMessageKindWire::Prompt,
            ),
            (
                None,
                Some(COVERED_TERMINAL_LOOP),
                false,
                UserMessageKindWire::Prompt,
            ),
            (
                None,
                Some(COVERED_TERMINAL_LOOP),
                true,
                UserMessageKindWire::Steering,
            ),
        ]
        .into_iter()
        .map(|(index, loop_id, pending, kind)| {
            Arc::new(TranscriptBlock::User(UserBlock {
                index,
                loop_id: loop_id.map(str::to_owned),
                pending,
                kind,
                text: "identical text".into(),
            }))
        })
        .collect::<Vec<_>>();
        view.transcript.blocks_mut().extend(kept.iter().cloned());
        if include_owned_prompt {
            view.transcript.push_block(TranscriptBlock::User(UserBlock {
                index: None,
                loop_id: Some(COVERED_TERMINAL_LOOP.into()),
                pending: true,
                kind: UserMessageKindWire::Prompt,
                text: "identical text".into(),
            }));
        }
        view.steer_queue.push(SteerQueueItem {
            local_id: 900,
            text: "queued text".into(),
            state: SteerQueueState::Unsent,
            editor_revision: None,
            handoff: false,
        });
        view.steer_queue_paused = true;
        let revision = view.transcript.render_revision;
        App::finish_read_chain(view, &SESSION.to_owned(), &read);
        assert!(view.live.is_none());
        assert_eq!(view.transcript.blocks.len(), kept.len() + 1);
        for block in &kept {
            assert!(
                view.transcript
                    .blocks
                    .iter()
                    .any(|current| Arc::ptr_eq(current, block))
            );
        }
        assert_eq!(
            view.transcript.render_revision,
            revision + u64::from(include_owned_prompt)
        );
        assert_eq!(view.steer_queue.len(), 1);
        assert_eq!(view.steer_queue[0].text, "queued text");
        assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
        assert!(view.steer_queue_paused);
        assert_eq!(app.composer.content(), "do not submit this draft");
    }
}
