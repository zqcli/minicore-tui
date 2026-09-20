//! UI coverage for synthetic RuntimeItem::Summary history pages. Agent 0.5
//! compaction bodies are not exposed by session.read; these are not real
//! compaction-generation E2E tests.
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
        view.transcript
            .window
            .pin()
            .map(|pin| pin.history_revision.as_str())
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
fn compaction_summary_manual_outcomes_never_read_history_or_synthesize_body() {
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
                let view = &app.sessions.known[SESSION];
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
