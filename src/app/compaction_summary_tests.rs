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
