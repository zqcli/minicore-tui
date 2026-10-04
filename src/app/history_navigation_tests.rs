use super::*;
use crate::protocol::read::{RawHistoryItem, SnapshotPin};
use crate::ui::testapp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

fn item(index: usize) -> RawHistoryItem {
    let loop_id = format!("loop_{}", index / 2);
    let value = if index % 2 == 0 {
        json!({"item":{"type":"user","data":{"loop_id":loop_id,"kind":"prompt","input":{"text":format!("PROMPT-{index:03}")}}}})
    } else {
        json!({"item":{"type":"assistant","data":{"loop_id":loop_id,"request_index":0,"model":"deep","reasoning":"high","content":[{"type":"text","data":format!("REPLY-{index:03}")}],"usage":{},"finish_reason":"stop"}}})
    };
    crate::protocol::read::decode_item(&value.to_string()).unwrap()
}

fn install(app: &mut App, index: usize) {
    let view = app.active_session_mut().unwrap();
    let owner = install_history_item(view, index, &item(index)).unwrap();
    view.transcript
        .window
        .insert_owner(index, owner, index as u64, 100);
    view.transcript.sync_from_window();
}

fn fixture(total: usize) -> App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.active_session_mut()
        .unwrap()
        .transcript
        .window
        .install_pin(SnapshotPin {
            projection: None,
            captured_end: total as u64,
            history_revision: "a".repeat(64),
            total,
        });
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    app
}

fn prepare(app: &mut App) {
    let prepared = crate::ui::transcript::prepare_conversation(app, 80);
    app.viewport = (prepared.total_rows(), 19);
    app.install_conversation(prepared);
}

fn slash(app: &mut App, command: &str) -> Vec<AppCommand> {
    app.composer_mut().set_text(command);
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ))))
}

#[test]
fn history_backfill_keeps_canonical_order_and_reuses_duplicate_owners() {
    let mut app = fixture(250);
    for index in (50..250).chain(0..50) {
        install(&mut app, index);
    }
    let view = app.active_view().unwrap();
    assert_eq!(
        view.transcript
            .blocks
            .iter()
            .filter_map(|block| block.index())
            .collect::<Vec<_>>(),
        (0..250).collect::<Vec<_>>()
    );
    let before = view.transcript.window.item(12).unwrap().clone();
    install(&mut app, 12);
    let view = app.active_view().unwrap();
    assert_eq!(view.transcript.blocks.len(), 250);
    assert!(Arc::ptr_eq(
        &before,
        view.transcript.window.item(12).unwrap()
    ));
    prepare(&mut app);
    let lines = app.prepared_conversation.as_ref().unwrap().lines();
    let text = lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<String>();
    assert!(text.find("PROMPT-000").unwrap() < text.find("PROMPT-050").unwrap());
    assert!(text.rfind("REPLY-249").unwrap() > text.find("REPLY-049").unwrap());
}

#[test]
fn history_backfill_keeps_large_placeholders_in_order_before_pending_drafts() {
    let mut app = fixture(8);
    install(&mut app, 6);
    let view = app.active_session_mut().unwrap();
    view.transcript.push_block(TranscriptBlock::User(UserBlock {
        index: None,
        loop_id: None,
        kind: UserMessageKindWire::Prompt,
        text: "pending prompt".into(),
        pending: true,
    }));
    install_history_placeholder(view, 4, 9 * 1024 * 1024);
    install_history_placeholder(view, 2, 9 * 1024 * 1024);
    install_history_placeholder(view, 2, 9 * 1024 * 1024);
    assert_eq!(
        view.transcript
            .blocks
            .iter()
            .map(|block| block.index())
            .collect::<Vec<_>>(),
        vec![Some(2), Some(4), Some(6), None]
    );
}

#[test]
fn consecutive_prompt_navigation_advances_after_real_layout_and_anchor_capture() {
    let mut app = fixture(16);
    for index in 0..16 {
        install(&mut app, index);
    }
    prepare(&mut app);
    app.active_session_mut().unwrap().scroll.anchor = None;
    for expected in [0, 2, 4, 6] {
        assert!(slash(&mut app, "/next").is_empty());
        assert_eq!(
            app.active_view()
                .unwrap()
                .scroll
                .anchor
                .as_ref()
                .unwrap()
                .section_id
                .history_index,
            Some(expected)
        );
        prepare(&mut app);
    }
    for expected in [4, 2, 0] {
        assert!(slash(&mut app, "/prev").is_empty());
        assert_eq!(
            app.active_view()
                .unwrap()
                .scroll
                .anchor
                .as_ref()
                .unwrap()
                .section_id
                .history_index,
            Some(expected)
        );
        prepare(&mut app);
    }
    assert!(slash(&mut app, "/latest").is_empty());
    assert_eq!(
        app.active_view()
            .unwrap()
            .scroll
            .anchor
            .as_ref()
            .unwrap()
            .section_id
            .history_index,
        Some(14)
    );
}

#[test]
fn history_backfill_orders_tool_and_summary_owners_and_promotes_pending_once() {
    let mut app = fixture(8);
    install(&mut app, 6);
    let view = app.active_session_mut().unwrap();
    view.transcript.push_block(TranscriptBlock::User(UserBlock {
        index: None,
        loop_id: Some("loop_0".into()),
        kind: UserMessageKindWire::Prompt,
        text: "PROMPT-000".into(),
        pending: true,
    }));
    let tool = crate::protocol::read::decode_item(&json!({"item":{"type":"tool_result","data":{
        "loop_id":"loop_1","request_index":0,"call_id":"call_1","tool_name":"read","outcome":"success","output":{"content":"tool result"}
    }}}).to_string()).unwrap();
    let summary = crate::protocol::read::decode_item(
        &json!({"item":{"type":"summary","data":{"content":"summary"}}}).to_string(),
    )
    .unwrap();
    install_history_item(view, 5, &tool).unwrap();
    install_history_item(view, 3, &summary).unwrap();
    install_history_item(view, 0, &item(0)).unwrap();
    install_history_item(view, 5, &tool).unwrap();
    assert_eq!(
        view.transcript
            .blocks
            .iter()
            .filter_map(|block| block.index())
            .collect::<Vec<_>>(),
        vec![0, 3, 5, 6]
    );
    assert!(
        !view
            .transcript
            .blocks
            .iter()
            .any(|block| matches!(block.as_ref(), TranscriptBlock::User(user) if user.pending))
    );
}

#[test]
fn direct_scrolling_releases_explicit_prompt_navigation_cursor() {
    let mut app = fixture(16);
    for index in 0..16 {
        install(&mut app, index);
    }
    prepare(&mut app);
    slash(&mut app, "/next");
    prepare(&mut app);
    assert_eq!(app.active_view().unwrap().scroll.prompt_cursor, Some(0));
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::PageDown,
        KeyModifiers::NONE,
    ))));
    assert_eq!(app.active_view().unwrap().scroll.prompt_cursor, None);
}
