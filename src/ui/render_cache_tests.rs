//! Regression and contract tests for conversation layout reuse, durable
//! history caching, and clone-free render paths.

use std::path::PathBuf;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::json;

use crate::app::{App, ConnectionState};
use crate::event::{AppEvent, RpcEvent};
use crate::markdown::{parse_count, reset_parse_count};
use crate::protocol::{IncomingFrame, RpcNotification, UsageWire};
use crate::state::session::SessionView;
use crate::state::transcript::{AssistantBlock, AssistantPart, TranscriptBlock};
use crate::theme::ThemeKind;
use crate::ui::transcript::{prepare_conversation, visible_rows};

const WIDTH: u16 = 79;
const HEIGHT: u16 = 24;
const MD: &str = "# Title\n\nSome **bold** and *italic* text with `code`.\n\n- item 1\n- item 2\n\n```rust\nfn test() {}\n```\n";

fn make_test_app(item_count: usize) -> App {
    let mut app = App::new(PathBuf::from("/project"));
    app.connection = ConnectionState::Ready;
    app.update(AppEvent::SetTheme(ThemeKind::Dark));
    let info = serde_json::from_value(json!({
        "session_id": "ses_test",
        "title": "Performance Test",
        "profile": "coding",
        "workspace": "/project",
        "model": "deep",
        "reasoning": "high",
        "loaded": true,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let mut view = SessionView::new(info);
    view.state = Some(
        serde_json::from_value(json!({
            "session_id": "ses_test",
            "status": "idle",
            "active_loop": null,
            "block_reason": null
        }))
        .unwrap(),
    );
    for i in 0..item_count {
        view.transcript
            .blocks
            .push(TranscriptBlock::Assistant(AssistantBlock {
                index: i,
                loop_id: format!("loop_{i}"),
                request_index: 0,
                model: "deep".into(),
                reasoning_level: crate::protocol::Reasoning::High,
                parts: vec![AssistantPart::Text(MD.into())],
                tool_calls: vec![],
                usage: UsageWire::default(),
                finish_reason: "stop".into(),
                terminal_error: None,
            }));
    }
    view.transcript.complete = true;
    view.transcript.invalidate();
    app.sessions.known.insert("ses_test".into(), view);
    app.sessions.active = Some("ses_test".into());
    app.update(AppEvent::TerminalSize {
        width: WIDTH + 1,
        height: HEIGHT,
    });
    let prepared = prepare_conversation(&app, WIDTH);
    let screen =
        crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, WIDTH + 1, HEIGHT));
    let vis = visible_rows(&app, prepared.total_rows(), screen.transcript.height);
    app.update(AppEvent::Viewport {
        total_lines: prepared.total_rows(),
        visible_rows: vis,
    });
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Rendered);
    app
}

#[test]
fn no_op_mouse_moved_retains_prepared_conversation_and_does_not_dirty() {
    let mut app = make_test_app(1);
    assert!(app.prepared_conversation(WIDTH).is_some());
    assert!(!app.dirty);

    let commands = app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::empty(),
        },
    )));

    assert!(commands.is_empty());
    assert!(
        app.prepared_conversation(WIDTH).is_some(),
        "RED: no-op mouse move must retain prepared conversation"
    );
    assert!(!app.dirty, "RED: no-op mouse move must not set dirty flag");
}

#[test]
fn scroll_and_tick_and_viewport_retain_prepared_conversation() {
    let mut app = make_test_app(2);
    assert!(app.prepared_conversation(WIDTH).is_some());

    // Tick updates frame_count for status spinner, but should NOT drop transcript layout
    app.update(AppEvent::Tick);
    assert!(
        app.prepared_conversation(WIDTH).is_some(),
        "RED: Tick must retain prepared conversation"
    );

    // ScrollUp changes scroll offset, but should NOT drop transcript layout
    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::empty(),
        },
    )));
    assert!(
        app.prepared_conversation(WIDTH).is_some(),
        "RED: ScrollUp must retain prepared conversation"
    );

    // Viewport with identical geometry must retain layout and be idempotent
    let (total, vis) = app.viewport;
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: vis,
    });
    assert!(
        app.prepared_conversation(WIDTH).is_some(),
        "RED: Viewport with identical geometry must retain prepared conversation"
    );
}

#[test]
fn live_delta_reuses_durable_markdown_without_reparsing() {
    let mut app = make_test_app(2);
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_test".into(),
        text: "Live test".into(),
    });
    let notify = |app: &mut App, value| {
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(serde_json::from_value(value).unwrap()),
        ))));
    };
    let turn = json!({"session_id":"ses_test", "loop_id":"live_1"});
    let meta = json!({"session_id":"ses_test", "dropped_before":0});
    notify(
        &mut app,
        json!({"type":"turn_started", "data":{"turn":turn, "meta":meta}}),
    );
    notify(
        &mut app,
        json!({"type":"request_started", "data":{
            "turn":turn, "meta":meta, "request_index":0, "config_revision":0, "model":"deep", "reasoning":"high"
        }}),
    );
    let p = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(p));
    reset_parse_count();
    notify(
        &mut app,
        json!({"type":"output_delta", "data":{
            "turn":turn, "meta":meta, "request_index":0, "channel":"text", "delta":"streaming delta"
        }}),
    );
    let next_prepared = prepare_conversation(&app, WIDTH);
    assert!(
        next_prepared
            .lines
            .iter()
            .any(|line| line.to_string().contains("streaming delta"))
    );
    assert!(next_prepared.total_rows() > 0);

    // Durable history has 2 blocks of markdown. They MUST NOT be re-parsed!
    assert_eq!(
        parse_count(),
        0,
        "RED: live delta preparation must reuse durable layout and not re-parse historical markdown"
    );
}

#[test]
fn durable_history_mutation_invalidates_and_reparses() {
    let mut app = make_test_app(1);
    let p = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(p));

    reset_parse_count();

    // Mutate durable history
    let view = app.sessions.known.get_mut("ses_test").unwrap();
    view.transcript
        .blocks
        .push(TranscriptBlock::Assistant(AssistantBlock {
            index: 1,
            loop_id: "loop_1".into(),
            request_index: 0,
            model: "deep".into(),
            reasoning_level: crate::protocol::Reasoning::High,
            parts: vec![AssistantPart::Text(MD.into())],
            tool_calls: vec![],
            usage: UsageWire::default(),
            finish_reason: "stop".into(),
            terminal_error: None,
        }));
    view.transcript.invalidate();

    let next_prepared = prepare_conversation(&app, WIDTH);
    assert!(next_prepared.total_rows() > 0);
    assert!(
        parse_count() > 0,
        "Durable history mutation must re-parse markdown"
    );
}

#[test]
fn viewport_duplicate_is_idempotent_and_does_not_cancel_drag() {
    let mut app = make_test_app(5);
    let prepared = prepare_conversation(&app, WIDTH);
    let total = prepared.total_rows();
    let area = ratatui::layout::Rect::new(0, 0, WIDTH + 1, HEIGHT);
    let screen = crate::ui::layout::screen_layout(&app, area);
    let vis = visible_rows(&app, total, screen.transcript.height);
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: vis,
    });

    // Start scrollbar drag directly
    let geo =
        crate::ui::scrollbar::geometry(screen.transcript, total, vis, total.saturating_sub(vis))
            .unwrap();
    let drag_column = geo.column as u16;
    let drag_row = geo.thumb_top as u16;

    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: drag_column,
            row: drag_row,
            modifiers: KeyModifiers::empty(),
        },
    )));

    assert!(
        app.scrollbar_preview_offset("ses_test").is_some(),
        "Scrollbar drag must be active"
    );

    // Duplicate Viewport with identical geometry MUST NOT cancel drag
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: vis,
    });

    assert!(
        app.scrollbar_preview_offset("ses_test").is_some(),
        "RED: Idempotent Viewport must not cancel active scrollbar drag"
    );
}

#[test]
fn cached_render_and_mouse_hit_testing_do_not_parse_history() {
    let mut app = make_test_app(50);
    reset_parse_count();
    let pointer = app.prepared_conversation(WIDTH).unwrap().lines.as_ptr();
    for _ in 0..20 {
        app.update(AppEvent::Tick);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(WIDTH + 1, HEIGHT)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        app.update(AppEvent::Rendered);
    }
    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 3,
            modifiers: KeyModifiers::empty(),
        },
    )));
    assert_eq!(parse_count(), 0);
    assert_eq!(
        app.prepared_conversation(WIDTH).unwrap().lines.as_ptr(),
        pointer
    );
}

#[test]
fn theme_width_and_visibility_reject_stale_cache() {
    let mut app = make_test_app(2);
    let dark = app.prepared_conversation(WIDTH).unwrap().clone();
    reset_parse_count();
    app.update(AppEvent::SetTheme(ThemeKind::Light));
    assert!(app.prepared_conversation(WIDTH).is_none());
    // A preparation from the previous theme must not be installed later.
    app.update(AppEvent::ConversationPrepared(dark));
    assert!(app.prepared_conversation(WIDTH).is_none());
    let light = prepare_conversation(&app, WIDTH);
    assert!(parse_count() > 0);
    app.update(AppEvent::ConversationPrepared(light));
    reset_parse_count();
    let narrow = prepare_conversation(&app, WIDTH - 20);
    assert!(parse_count() > 0);
    app.update(AppEvent::ConversationPrepared(narrow));
    reset_parse_count();
    app.update(AppEvent::ToggleReasoning);
    assert!(app.prepared_conversation(WIDTH - 20).is_none());
    let _ = prepare_conversation(&app, WIDTH - 20);
    assert!(parse_count() > 0);
}

#[test]
fn tool_presentation_refresh_invalidates_already_durable_tool_rows() {
    let mut app = super::testapp::tools(ThemeKind::Dark);
    let view = app.sessions.known.get_mut("ses_1").unwrap();
    let mut live =
        crate::state::turn::LiveLoop::new(crate::state::turn::LocalSubmissionId(1), String::new());
    live.reference = Some(crate::protocol::TurnRef {
        session_id: "ses_1".into(),
        loop_id: "loop_1".into(),
    });
    view.live = Some(live);
    let prepared = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(prepared));
    reset_parse_count();
    let event = serde_json::from_value(json!({"type":"tool_presentation", "data":{
        "turn":{"session_id":"ses_1", "loop_id":"loop_1"}, "request_index":0,
        "tool_call_id":"call-1", "tool_name":"read",
        "display":{"detail":"UPDATED_PRESENTATION", "expanded_input":"new input", "truncated":false},
        "meta":{"session_id":"ses_1", "dropped_before":0}
    }})).unwrap();
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(event),
    ))));
    let prepared = prepare_conversation(&app, WIDTH);
    assert!(parse_count() > 0);
    assert!(
        prepared
            .lines
            .iter()
            .any(|line| line.to_string().contains("UPDATED_PRESENTATION"))
    );
}

#[test]
fn ordinary_editor_input_reuses_prepared_rows() {
    let mut app = make_test_app(5);
    let pointer = app.prepared_conversation(WIDTH).unwrap().lines.as_ptr();
    for character in "hello world".chars() {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                KeyModifiers::NONE,
            ),
        )));
        assert_eq!(
            app.prepared_conversation(WIDTH).unwrap().lines.as_ptr(),
            pointer
        );
    }
}
