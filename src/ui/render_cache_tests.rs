//! Regression and contract tests for conversation layout reuse, durable
//! history caching, and clone-free render paths.

use std::path::PathBuf;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde_json::json;

use crate::app::{App, ConnectionState};
use crate::event::{AppEvent, RpcEvent};
use crate::markdown::{parse_count, reset_parse_count};
use crate::protocol::{IncomingFrame, RpcNotification, TurnRef, UsageWire};
use crate::state::session::SessionView;
use crate::state::transcript::{AssistantBlock, AssistantPart, ToolBlock, TranscriptBlock};
use crate::state::turn::{
    AppliedSteer, LiveLoop, LocalSubmissionId, PendingSteer, PendingSteerState,
};
use crate::state::view::{ConversationSelection, SelectionGranularity, SelectionPoint};
use crate::theme::ThemeKind;
use crate::ui::transcript::{prepare_conversation, visible_rows};

const WIDTH: u16 = 77;
const HEIGHT: u16 = 24;
const MD: &str = "# Title\n\nSome **bold** and *italic* text with `code`.\n\n- item 1\n- item 2\n\n```rust\nfn test() {}\n```\n";

#[test]
fn pending_layout_replays_body_and_track_without_painting_gap_or_grown_dock() {
    let mut app = make_test_app(20);
    app.enable_async_layout();
    let prepared = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(prepared));
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let rendered = terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    app.remember_transcript_frame(rendered.buffer);
    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: screen.scrollbar.x,
            row: screen.transcript.y,
            modifiers: KeyModifiers::NONE,
        },
    )));
    assert!(app.scrollbar_active());
    let rendered = terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    app.remember_transcript_frame(rendered.buffer);
    let previous = terminal.backend().buffer().clone();
    app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: 1,
        dropped: 0,
    }));
    assert!(app.prepared_conversation(WIDTH).is_none());
    app.composer_mut().set_text(&"new draft row\n".repeat(8));
    let current = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    assert!(current.transcript.height < screen.transcript.height);
    assert!(
        app.transition_transcript_frame(current.transcript)
            .is_some()
    );
    terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for row in current.transcript.y..current.transcript.bottom() {
        for column in current.content.x..current.content.right() {
            assert_eq!(buffer[(column, row)], previous[(column, row)]);
        }
        assert_eq!(
            buffer[(current.scrollbar.x, row)],
            previous[(screen.scrollbar.x, row)]
        );
    }
    for row in 0..24 {
        assert_eq!(buffer[(current.right_gap.x, row)].symbol(), " ");
        assert_eq!(
            buffer[(current.right_gap.x, row)].bg,
            crate::theme::Theme::dark().page_bg
        );
        if row >= current.transcript.bottom() {
            assert_eq!(buffer[(current.scrollbar.x, row)].symbol(), " ");
            assert_eq!(
                buffer[(current.scrollbar.x, row)].bg,
                crate::theme::Theme::dark().page_bg
            );
        }
    }
}

#[test]
fn pending_width_resize_discards_body_and_independent_track_replay() {
    let mut app = make_test_app(20);
    app.enable_async_layout();
    let prepared = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(prepared));
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let rendered = terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    app.remember_transcript_frame(rendered.buffer);
    assert!(app.transition_transcript_frame(screen.transcript).is_some());
    assert!(app.transition_scrollbar_frame(screen.transcript).is_some());

    app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: 1,
        dropped: 0,
    }));
    assert!(app.prepared_conversation(WIDTH).is_none());
    app.update(AppEvent::TerminalSize {
        width: 100,
        height: 24,
    });
    let current = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 100, 24));
    assert_ne!(current.transcript, screen.transcript);
    // A width change reflows every body row and relocates the track, so neither
    // the old body nor the old right-edge column may be replayed at the new
    // geometry while the replacement layout is pending.
    assert!(
        app.transition_transcript_frame(current.transcript)
            .is_none()
    );
    assert!(app.transition_scrollbar_frame(current.transcript).is_none());
}

#[test]
#[ignore = "manual matched scrollbar event benchmark"]
fn scrollbar_noop_event_benchmark() {
    let mut app = make_test_app(200);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    app.update(AppEvent::Rendered);
    reset_parse_count();
    let start = std::time::Instant::now();
    let mut frames = 0;
    for _ in 0..5000 {
        app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 4,
                row: 1,
                modifiers: KeyModifiers::empty(),
            },
        )));
        if app.dirty {
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            app.update(AppEvent::Rendered);
            frames += 1;
        }
    }
    println!(
        "scrollbar_noop events=5000 history_messages=200 frames={frames} elapsed_us={} markdown_parses={}",
        start.elapsed().as_micros(),
        parse_count()
    );
    assert_eq!(parse_count(), 0);
}

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
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
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
        width: WIDTH + 3,
        height: HEIGHT,
    });
    let prepared = prepare_conversation(&app, WIDTH);
    let screen =
        crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, WIDTH + 3, HEIGHT));
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
            .lines()
            .iter()
            .any(|line| line.to_string().contains("streaming delta"))
    );
    assert!(next_prepared.total_rows() > 0);

    // Only the updated live section is parsed. The two durable Markdown
    // blocks retain their immutable layout and must not be re-parsed.
    assert_eq!(parse_count(), 1, "only live Markdown should be parsed");
    assert!(std::sync::Arc::ptr_eq(
        next_prepared.durable.as_ref().unwrap(),
        app.active_view()
            .unwrap()
            .transcript
            .render_cache
            .as_ref()
            .unwrap()
    ));
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
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
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
    let area = ratatui::layout::Rect::new(0, 0, WIDTH + 3, HEIGHT);
    let screen = crate::ui::layout::screen_layout(&app, area);
    let vis = visible_rows(&app, total, screen.transcript.height);
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: vis,
    });

    // Start scrollbar drag directly
    let geo = crate::ui::scrollbar::geometry(
        screen.scrollbar_for(screen.transcript),
        total,
        total.saturating_sub(vis),
    )
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
fn unrelated_rpc_reprepare_keeps_scrollbar_drag_until_release() {
    let mut app = make_test_app(5);
    let prepared = prepare_conversation(&app, WIDTH);
    let total = prepared.total_rows();
    let area = ratatui::layout::Rect::new(0, 0, WIDTH + 3, HEIGHT);
    let screen = crate::ui::layout::screen_layout(&app, area);
    let visible = visible_rows(&app, total, screen.transcript.height);
    let geometry = crate::ui::scrollbar::geometry(
        screen.scrollbar_for(screen.transcript),
        total,
        total - visible,
    )
    .expect("overflowing transcript has a scrollbar");

    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: geometry.column as u16,
            row: geometry.thumb_top as u16,
            modifiers: KeyModifiers::empty(),
        },
    )));
    assert!(app.scrollbar_preview_offset("ses_test").is_some());

    app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: "unrelated stderr".len(),
        dropped: 0,
    }));
    assert!(app.prepared_conversation(WIDTH).is_none());
    let reparsed = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(reparsed));
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: visible,
    });
    assert!(
        app.scrollbar_preview_offset("ses_test").is_some(),
        "an unrelated RPC reprepare and identical viewport must not cancel drag"
    );

    let target_row = geometry.track_top + geometry.max_thumb_start / 2;
    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: geometry.column as u16,
            row: target_row as u16,
            modifiers: KeyModifiers::empty(),
        },
    )));
    let pending = app
        .scrollbar_preview_offset("ses_test")
        .expect("drag remains active after reprepare");
    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: geometry.column as u16,
            row: target_row as u16,
            modifiers: KeyModifiers::empty(),
        },
    )));
    assert!(app.scrollbar_preview_offset("ses_test").is_none());
    assert_eq!(app.active_view().unwrap().scroll.offset, pending);
}

#[test]
fn cached_render_and_mouse_hit_testing_do_not_parse_history() {
    let mut app = make_test_app(50);
    reset_parse_count();
    let pointer = app.prepared_conversation(WIDTH).unwrap().history_ptr();
    for _ in 0..20 {
        app.update(AppEvent::Tick);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(WIDTH + 3, HEIGHT)).unwrap();
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
        app.prepared_conversation(WIDTH).unwrap().history_ptr(),
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
    assert_eq!(
        parse_count(),
        0,
        "changing reasoning visibility must not reparse text-only sections"
    );
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
            .lines()
            .iter()
            .any(|line| line.to_string().contains("UPDATED_PRESENTATION"))
    );
}

#[test]
fn live_tool_fold_survives_presentation_finish_wait_and_history_replacement() {
    let mut app = super::testapp::live_turn(ThemeKind::Dark);
    for request in &mut app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .live
        .as_mut()
        .unwrap()
        .requests
    {
        request
            .parts
            .retain(|part| !matches!(part, crate::state::turn::LivePart::Reasoning(_)));
    }
    {
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index: 0,
                loop_id: "old_loop".to_owned(),
                request_index: 0,
                model: "deep".to_owned(),
                reasoning_level: crate::protocol::Reasoning::High,
                parts: vec![AssistantPart::Text("cached **history**".to_owned())],
                tool_calls: vec![],
                usage: UsageWire::default(),
                finish_reason: "stop".to_owned(),
                terminal_error: None,
            }));
        view.transcript.complete = true;
        view.transcript.invalidate();
    }
    let prepared = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(prepared));

    let key = crate::state::tool::ToolKey::new("ses_1", "loop_live", 0, "c1");
    app.update(AppEvent::ToggleTool {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_live".to_owned(),
        request_index: 0,
        tool_call_id: "c1".to_owned(),
    });
    assert_eq!(
        app.active_view().unwrap().tool_folds.get(&key),
        Some(&crate::state::view::FoldOverride::Collapsed)
    );
    let folded = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(folded));
    reset_parse_count();

    let presentation = serde_json::from_value(json!({
        "type": "tool_presentation",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "tool_call_id": "c1",
            "tool_name": "read",
            "display": {
                "detail": "LIVE_PRESENTATION",
                "expanded_input": "input",
                "truncated": false
            },
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    }))
    .unwrap();
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(presentation),
    ))));
    let prepared = prepare_conversation(&app, WIDTH);
    app.update(AppEvent::ConversationPrepared(prepared));
    reset_parse_count();
    let prepared = prepare_conversation(&app, WIDTH);
    assert_eq!(
        parse_count(),
        2,
        "only the two live text sections are parsed"
    );
    assert_eq!(
        prepared.history_ptr(),
        app.prepared_conversation(WIDTH).unwrap().history_ptr(),
        "repreparing after live presentation must reuse the cached durable markdown"
    );

    {
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        let live = view.live.as_mut().unwrap();
        live.waiting = true;
        live.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: crate::protocol::TurnRef {
                session_id: "ses_1".to_owned(),
                loop_id: "loop_live".to_owned(),
            },
            outcome: crate::protocol::LoopOutcomeWire::Completed,
            persistence: Some(crate::protocol::TurnPersistenceWire::Persisted),
            usage: Some(UsageWire::default()),
            requests: Some(1),
            tool_rounds: Some(1),
            final_config_revision: Some(0),
            accepted_at: None,
            completed_at: None,
        });
        view.live = None;
        view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
            index: Some(1),
            loop_id: "loop_live".to_owned(),
            request_index: 0,
            tool_call_id: "c1".to_owned(),
            name: "read".to_owned(),
            result: Some("history result".to_owned().into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Success),
            live_status: Some(crate::state::tool::ToolStatus::Succeeded),
            progress: None,
            expanded: true,
        }));
        view.transcript.complete = true;
        view.transcript.invalidate();
        let history_tool = view
            .transcript
            .blocks
            .iter()
            .find_map(|block| match block.as_ref() {
                TranscriptBlock::Tool(tool) if tool.loop_id == "loop_live" => Some(tool),
                _ => None,
            })
            .expect("history Tool replacement");
        assert!(
            !crate::ui::transcript::effective_tool_expanded(view, history_tool),
            "history Tool replacement resolves the existing fold override"
        );
    }
    let prepared = prepare_conversation(&app, WIDTH);
    let section = prepared
        .sections
        .iter()
        .find(|section| {
            section.id.kind == crate::state::view::SectionKind::Tool
                && section.id.loop_id.as_deref() == Some("loop_live")
                && section.id.request_index == Some(0)
                && section.id.tool_call_id.as_deref() == Some("c1")
        })
        .expect("history replacement keeps the loop identity");
    assert!(
        section.folded,
        "the live fold override survives history replacement: section={section:?}, folds={:?}",
        app.active_view().unwrap().tool_folds
    );
    assert_eq!(
        app.active_view().unwrap().tool_folds.get(&key),
        Some(&crate::state::view::FoldOverride::Collapsed)
    );
}

#[test]
fn ordinary_editor_input_reuses_prepared_rows() {
    let mut app = make_test_app(5);
    let pointer = app.prepared_conversation(WIDTH).unwrap().history_ptr();
    for character in "hello world".chars() {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                KeyModifiers::NONE,
            ),
        )));
        assert_eq!(
            app.prepared_conversation(WIDTH).unwrap().history_ptr(),
            pointer
        );
    }
}

fn user_gap_app(second_kind: &str) -> App {
    crate::ui::testapp::open_with(
        ThemeKind::Dark,
        "ses_1",
        None,
        "high",
        vec![
            crate::ui::testapp::user_entry(0, "loop_1", "first prompt"),
            json!({
                "index": 1,
                "item": {"type": "user", "data": {
                    "loop_id": "loop_2", "kind": second_kind,
                    "input": {"text": "second message"}
                }}
            }),
        ],
    )
}

fn add_live(view: &mut SessionView, local_id: u64) {
    view.live = Some(LiveLoop::new(
        LocalSubmissionId(local_id),
        "second message".to_owned(),
    ));
    view.live.as_mut().unwrap().reference = Some(TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_2".to_owned(),
    });
    view.transcript.invalidate();
}

fn assert_user_gap(app: &App, expected: &str) {
    let prepared = prepare_conversation(app, WIDTH);
    let users: Vec<_> = prepared
        .sections
        .iter()
        .filter(|section| section.id.kind == crate::state::view::SectionKind::User)
        .collect();
    assert_eq!(users.len(), 2);
    let gap = users[1].rows.start.saturating_sub(users[0].rows.end);
    assert_eq!(gap, 1, "one transparent User separator");
    let gap_row = users[0].rows.end;
    assert!(prepared.row(gap_row).unwrap().spans.is_empty());
    let durable = prepared.durable.as_ref().expect("durable frame");
    assert!(
        durable
            .layout
            .sections
            .iter()
            .all(|section| { section.layout.link_cells.len() == section.layout.rows.len() })
    );
    assert!(prepared.links_at(gap_row).is_empty());
    assert!(prepared.sections.iter().all(|s| !s.rows.contains(&gap_row)));
    assert!(
        prepared
            .section_at(gap_row, users[0].content_columns.start)
            .is_none()
    );
    assert!(prepared.copy_ranges.iter().all(|r| r.row != gap_row));

    let selection = ConversationSelection {
        session_id: "ses_1".to_owned(),
        anchor: SelectionPoint {
            row: users[0].rows.start + 1,
            column: users[0].content_columns.start,
            section_id: Some(users[0].id.clone()),
            section_row: 1,
        },
        focus: SelectionPoint {
            row: users[1].rows.start + 1,
            column: users[1].content_columns.end.saturating_sub(1),
            section_id: Some(users[1].id.clone()),
            section_row: 1,
        },
        granularity: SelectionGranularity::Character,
        dragged: true,
    };
    let copied = crate::ui::transcript::selection_text(&prepared, &selection);
    assert!(copied.contains("first prompt") && copied.contains(expected));
    assert!(!copied.contains("\n\n"));

    let screen = crate::ui::layout::screen_layout(app, Rect::new(0, 0, WIDTH + 3, HEIGHT));
    let position = crate::ui::transcript::scroll_position(
        app,
        prepared.total_rows(),
        screen.transcript.height as usize,
    );
    assert!(gap_row >= position.offset && gap_row < position.offset + position.visible_rows);
    let rows = crate::ui::component_tests::buffer_lines(&crate::ui::component_tests::draw(
        app,
        WIDTH + 3,
        HEIGHT,
    ));
    assert!(
        rows[screen.transcript.y as usize + gap_row - position.offset][1..]
            .trim()
            .is_empty()
    );
}

#[test]
fn consecutive_user_cards_keep_one_transparent_gap_across_all_user_paths() {
    for kind in ["prompt", "steering"] {
        assert_user_gap(&user_gap_app(kind), "second message");
    }

    let mut live = user_gap_app("prompt");
    let view = live.sessions.known.get_mut("ses_1").unwrap();
    view.transcript.blocks_mut().pop();
    add_live(view, 1);
    view.applied_steers.push(AppliedSteer {
        local_id: 1,
        text: "second steering".to_owned(),
        accepted_at: None,
        request_index: 0,
    });
    assert_user_gap(&live, "second steering");

    let mut queued = user_gap_app("prompt");
    let view = queued.sessions.known.get_mut("ses_1").unwrap();
    add_live(view, 2);
    view.live
        .as_mut()
        .unwrap()
        .pending_steers
        .push(PendingSteer {
            local_id: 2,
            text: "queued steering".to_owned(),
            state: PendingSteerState::Queued,
            accepted_at: None,
            steer_index: None,
        });
    assert_user_gap(&queued, "second message");
    let prepared = prepare_conversation(&queued, WIDTH);
    assert_eq!(
        prepared
            .sections
            .iter()
            .filter(|section| section.id.kind == crate::state::view::SectionKind::User)
            .count(),
        2
    );
    let rows = crate::ui::component_tests::buffer_lines(&crate::ui::component_tests::draw(
        &queued,
        WIDTH + 3,
        HEIGHT,
    ));
    assert!(
        rows.iter()
            .any(|row| row.contains("Steering (accepted): queued steering"))
    );
}
