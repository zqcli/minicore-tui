use super::*;
use crate::protocol::read::SnapshotPin;
use crate::ui::testapp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde_json::{Value, json};

fn encoded(index: usize) -> String {
    json!({"item":{"type":"user","data":{"loop_id":format!("loop_{index}"),
        "kind":"prompt","input":{"text":format!("PROMPT-{index:03}")}}}})
    .to_string()
}

fn fixture(start: usize, total: usize, theme: ThemeKind) -> App {
    let mut app = testapp::open_empty(theme, "ses_1", None, "high");
    let view = app.active_session_mut().unwrap();
    view.transcript.window.install_pin(SnapshotPin {
        captured_end: 90_000,
        history_revision: "a".repeat(64),
        total,
    });
    for index in start..total {
        let data = encoded(index);
        let item = crate::protocol::read::decode_item(&data).unwrap();
        let owner = install_history_item(view, index, &item).unwrap();
        view.transcript
            .window
            .insert_owner(index, owner, 0, data.len());
    }
    view.transcript.sync_from_window();
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    prepare(&mut app);
    app
}

fn prepare(app: &mut App) {
    let screen = crate::ui::layout::screen_layout(app, Rect::new(0, 0, 80, 24));
    let prepared = crate::ui::transcript::prepare_conversation(app, screen.content.width);
    app.viewport = (prepared.total_rows(), screen.transcript.height as usize);
    app.install_conversation(prepared);
}

fn home(app: &mut App) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Home,
        KeyModifiers::CONTROL,
    ))))
}

fn page(app: &App, start: usize, end: usize) -> Value {
    let view = app.active_view().unwrap();
    json!({
        "session": view.info,
        "items": (start..end).map(|index| {
            let data = encoded(index);
            json!({"index":index,"offset":0,"total_bytes":data.len(),
                "encoding":"utf8_json","data":data,"complete":true})
        }).collect::<Vec<_>>(),
        "total":view.transcript.window.total(), "records":[], "records_truncated":false,
        "history_revision":"a".repeat(64), "captured_end":90_000,
        "trailing_incomplete":false,"next_cursor":{"item":end,"offset":0}
    })
}

fn request(commands: Vec<AppCommand>, expected: usize) -> crate::protocol::OutgoingRequest {
    let requests = testapp::take_requests(commands);
    assert_eq!(requests.len(), 1, "one user-driven history page");
    let request = requests.into_iter().next().unwrap();
    assert_eq!(request.method, "session.read");
    assert_eq!(
        request.params["cursor"],
        json!({"item":expected,"offset":0})
    );
    assert_eq!(request.params["limit"], 20);
    assert_eq!(request.params["max_bytes"], 262_144);
    assert_eq!(request.params["captured_end"], 90_000);
    assert_eq!(request.params["history_revision"], "a".repeat(64));
    request
}

#[test]
fn earlier_history_row_is_explicit_decorative_and_never_auto_reads() {
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut app = fixture(50, 250, theme);
        let prepared = app.prepared_conversation.as_ref().unwrap();
        assert!(
            prepared.header[0]
                .to_string()
                .contains("Earlier messages not loaded")
        );
        assert!(prepared.header[0].to_string().contains("Load earlier"));
        assert_eq!(prepared.header_rows(), 2);
        assert!(prepared.copy_row(0).is_none());
        assert!(prepared.section_at(0, 5).is_none());
        assert!(app.update(AppEvent::Tick).is_empty());
        assert!(!app.pending_history(&"ses_1".to_owned()));
    }
}

#[test]
fn earlier_history_home_reads_one_page_and_preserves_visible_content_anchor() {
    let mut app = fixture(50, 250, ThemeKind::Dark);
    let req = request(home(&mut app), 30);
    let anchor = app.active_view().unwrap().scroll.anchor.clone().unwrap();
    assert_eq!(anchor.section_id.history_index, Some(50));
    assert!(
        home(&mut app).is_empty(),
        "repeated input coalesces while in flight"
    );
    let result = page(&app, 30, 50);
    assert!(
        testapp::respond(&mut app, &req, result).is_empty(),
        "do not reread the resident 200-item suffix"
    );
    prepare(&mut app);
    let view = app.active_view().unwrap();
    let prepared = app.prepared_conversation.as_ref().unwrap();
    let row = prepared.row_for_scroll_anchor(&anchor).unwrap();
    assert_eq!(row - view.scroll.offset, anchor.screen_row);
    assert!(!view.scroll.follow_tail);
    assert_loaded_range(&view.transcript.window, 30..250);
    assert!(view.read_page.is_none());
    assert!(view.transcript.next_cursor.is_none());
}

#[test]
fn earlier_history_wheel_and_click_load_the_same_pinned_page() {
    for click in [false, true] {
        let mut app = fixture(50, 250, ThemeKind::Dark);
        let (total, visible) = app.transcript_scroll_extent();
        app.set_transcript_offset(0, total, visible);
        let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
        let commands = app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind: if click {
                MouseEventKind::Down(MouseButton::Left)
            } else {
                MouseEventKind::ScrollUp
            },
            column: screen.content.x + 3,
            row: screen.transcript.y,
            modifiers: KeyModifiers::NONE,
        })));
        request(commands, 30);
        assert!(app.selection.is_none());
    }
}

#[test]
fn earlier_history_user_pages_are_not_limited_by_search_jump_retry_budget() {
    let mut app = fixture(150, 350, ThemeKind::Dark);
    for start in [130, 110, 90, 70, 50, 30] {
        let req = request(home(&mut app), start);
        let result = page(&app, start, start + 20);
        assert!(testapp::respond(&mut app, &req, result).is_empty());
        prepare(&mut app);
    }
    assert_eq!(app.search_jump_attempts, 0);
    assert_loaded_range(&app.active_view().unwrap().transcript.window, 30..350);
}

#[test]
fn earlier_history_affordance_disappears_after_prefix_is_loaded() {
    for retained_welcome in [false, true] {
        let mut app = fixture(20, 220, ThemeKind::Dark);
        // Cover a cold restored session and a session whose empty beginning
        // was seen here before its history grew and the prefix was evicted.
        app.active_session_mut().unwrap().welcome_retained = retained_welcome;
        let req = request(home(&mut app), 0);
        let result = page(&app, 0, 20);
        assert!(testapp::respond(&mut app, &req, result).is_empty());
        prepare(&mut app);
        assert!(crate::ui::header::earlier_history_start(&app).is_none());
        let header = &app.prepared_conversation.as_ref().unwrap().header;
        assert!(
            header
                .iter()
                .all(|line| !line.to_string().contains("Earlier messages"))
        );
        assert_eq!(
            header
                .iter()
                .filter(|line| line.to_string().contains("MINICORE"))
                .count(),
            usize::from(retained_welcome)
        );
        assert_eq!(header.len(), if retained_welcome { 5 } else { 0 });
        assert!(app.active_view().unwrap().transcript.complete);
        assert!(home(&mut app).is_empty());
    }
}

#[tokio::test]
async fn earlier_history_async_decode_stops_at_resident_suffix() {
    let mut app = fixture(50, 250, ThemeKind::Dark);
    app.enable_async_decode();
    let req = request(home(&mut app), 30);
    let result = page(&app, 30, 50);
    assert!(testapp::respond(&mut app, &req, result).is_empty());
    let mut jobs = crate::jobs::LocalJobs::new();
    let mut decoded = 0;
    while let Some(decode) = app.pending_decode_request() {
        assert!(jobs.try_schedule_decode(decode));
        app.mark_decode_scheduled();
        assert!(app.update(jobs.events().recv().await.unwrap()).is_empty());
        decoded += 1;
    }
    jobs.shutdown().await;
    assert_eq!(decoded, 20);
    let view = app.active_view().unwrap();
    assert_loaded_range(&view.transcript.window, 30..250);
    assert!(view.read_page.is_none());
    assert!(view.transcript.next_cursor.is_none());
}

#[tokio::test]
async fn earlier_history_single_item_async_jump_finishes_before_resident_suffix() {
    let mut app = fixture(50, 250, ThemeKind::Dark);
    app.enable_async_decode();
    let target = crate::state::search::SearchMatch {
        index: Some(49),
        source: crate::state::search::SearchSource::Prompt,
        loop_id: Some("loop_49".to_owned()),
        request_index: None,
        ordinal: 0,
        tool_call_id: None,
        preview: "PROMPT-049".to_owned(),
        source_offset: 0,
        byte_range: 0..10,
    };
    let req = request(app.jump_to_match(&target), 49);
    let result = page(&app, 49, 50);
    assert!(testapp::respond(&mut app, &req, result).is_empty());
    assert!(app.pending_search_jump.is_some());
    let mut jobs = crate::jobs::LocalJobs::new();
    let decode = app.pending_decode_request().unwrap();
    assert!(jobs.try_schedule_decode(decode));
    app.mark_decode_scheduled();
    assert!(app.update(jobs.events().recv().await.unwrap()).is_empty());
    jobs.shutdown().await;
    assert!(app.pending_search_jump.is_none());
    assert_eq!(app.active_view().unwrap().scroll.prompt_cursor, Some(49));
    assert_eq!(
        app.active_view()
            .unwrap()
            .scroll
            .anchor
            .as_ref()
            .unwrap()
            .section_id
            .history_index,
        Some(49)
    );
    assert!(app.active_view().unwrap().read_page.is_none());
}

fn assert_loaded_range(
    window: &crate::app::history::HistoryWindow,
    expected: std::ops::Range<usize>,
) {
    assert_eq!(window.loaded_ranges().len(), 1);
    assert_eq!(window.loaded_ranges()[0], expected);
}
