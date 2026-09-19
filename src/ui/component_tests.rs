//! Component-level rendering tests: exact colors, modifiers, preview bounds,
//! footer behavior, and cursor column math (development spec 15, 29, 31).

use std::sync::Arc;

use crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use serde_json::json;

use crate::app::App;
use crate::event::{AppEvent, RpcEvent};
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::{
    AssistantBlock, AssistantPart, ToolBlock, TranscriptBlock, UserBlock,
};
use crate::state::view::{ConversationSelection, SelectionGranularity, SelectionPoint};
use crate::theme::{Theme, ThemeKind};
use crate::ui::testapp;
use crate::ui::{
    assistant, footer, layout, panel, reasoning, render, selector, tool, transcript, user,
};

#[test]
fn shared_panel_layout_has_one_geometry_for_render_and_hit_testing() {
    let layout = panel::layout(Rect::new(3, 5, 50, 12), panel::PanelSpec::new(2, true, 1));
    assert_eq!(layout.outer, Rect::new(3, 5, 50, 12));
    assert_eq!(layout.inner, Rect::new(4, 6, 48, 10));
    assert_eq!(layout.title, Rect::new(4, 6, 48, 1));
    assert_eq!(layout.header, Rect::new(4, 7, 48, 2));
    assert_eq!(layout.query, Some(Rect::new(4, 9, 48, 1)));
    assert_eq!(layout.content, Rect::new(4, 10, 48, 5));
    assert_eq!(layout.footer, Rect::new(4, 15, 48, 1));
    assert_eq!(layout.content_row(12), Some(2));
    assert_eq!(layout.content_row(15), None);
    assert_eq!(panel::visible_window(&[2, 2, 2, 2], 2, 5), 2..4);
}

#[test]
fn prepared_tool_sections_keep_full_identity_and_mouse_toggle_uses_the_same_range() {
    let mut app = testapp::tools(ThemeKind::Dark);
    app.update(AppEvent::ToggleTools {
        session_id: "ses_1".to_owned(),
    });
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });

    let prepared = transcript::prepare_conversation(&app, 79);
    let tool_sections: Vec<_> = prepared
        .sections
        .iter()
        .filter(|section| section.id.kind == crate::state::view::SectionKind::Tool)
        .collect();
    assert_eq!(tool_sections.len(), 3);
    assert_eq!(
        tool_sections
            .iter()
            .map(|section| section.id.tool_call_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("call-1"), Some("call-2"), Some("call-3")]
    );
    assert!(
        prepared
            .copy_ranges
            .iter()
            .any(|range| range.text == "run the tools")
    );
    assert!(
        prepared
            .copy_ranges
            .iter()
            .all(|range| !range.text.starts_with('▎'))
    );
    assert!(
        tool_sections
            .windows(2)
            .all(|sections| sections[0].rows.end <= sections[1].rows.start)
    );

    let layout = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let total = transcript::total_lines(&app, layout.content.width);
    let offset = total.saturating_sub(layout.transcript.height as usize);
    let section = tool_sections
        .iter()
        .find(|section| section.rows.start >= offset)
        .expect("a collapsed tool card is visible in the tail");
    let row = layout.transcript.y + (section.rows.start - offset) as u16;
    let column = layout.content.x + 2;
    app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    })));
    app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    })));

    let view = app.active_view().unwrap();
    let key = crate::state::tool::ToolKey::new(
        "ses_1",
        section.id.loop_id.as_deref().unwrap(),
        section.id.request_index.unwrap(),
        section.id.tool_call_id.as_deref().unwrap(),
    );
    assert_eq!(
        view.tool_folds.get(&key),
        Some(&crate::state::view::FoldOverride::Expanded)
    );
    assert!(!view.scroll.follow_tail);

    // A per-tool collapse remains authoritative even after the global toggle
    // expands every Tool again.
    app.update(AppEvent::ToggleTools {
        session_id: "ses_1".to_owned(),
    });
    app.update(AppEvent::ToggleTool {
        session_id: "ses_1".to_owned(),
        loop_id: key.loop_id.clone(),
        request_index: key.request_index,
        tool_call_id: key.tool_call_id.clone(),
    });
    assert_eq!(
        app.active_view().unwrap().tool_folds.get(&key),
        Some(&crate::state::view::FoldOverride::Collapsed)
    );
    let prepared = transcript::prepare_conversation(&app, 79);
    let folded = prepared
        .sections
        .iter()
        .find(|section| {
            section.id.kind == crate::state::view::SectionKind::Tool
                && section.id.tool_call_id.as_deref() == Some(key.tool_call_id.as_str())
        })
        .is_some_and(|section| section.folded);
    assert!(folded, "the per-tool collapse overrides global expansion");
}

/// Spec §25.1 (structural): a cached durable layout is not rebuilt, the tool
/// projection resolves every call through the index (never a block scan), and
/// the cloned bytes are counted where they are actually cloned.
#[test]
fn stable_history_layout_is_cached_and_tool_projection_uses_the_index() {
    crate::perf::reset();
    let mut app = testapp::tools(ThemeKind::Dark);
    let first = transcript::prepare_conversation(&app, 79);
    let mid = crate::perf::snapshot();
    assert_eq!(mid.tool_linear_scans, 0, "no block scan in the projection");
    assert!(
        mid.tool_index_lookups >= 3,
        "the three tool calls resolved through the projection index"
    );
    assert!(mid.layout_calls >= 1, "the first durable layout was built");
    assert_eq!(
        mid.historical_text_bytes_cloned, 0,
        "C2: preparing a frame never copies the durable history"
    );
    assert_eq!(
        mid.viewport_rows_materialized, 0,
        "C2: preparing a frame materializes no rows; only the window does"
    );
    // The counters are not constants: the diagnostic full materialization and
    // one viewport window both report real work.
    let _all = first.lines();
    let after_all = crate::perf::snapshot();
    assert!(
        after_all.historical_text_bytes_cloned > 0,
        "the full-frame diagnostic clone is counted where it happens"
    );
    let window = first.window(0, 5);
    assert_eq!(window.len(), 5);
    let after_window = crate::perf::snapshot();
    assert!(
        after_window.viewport_rows_materialized >= 5,
        "the visible window rows are counted"
    );
    assert!(
        after_window.viewport_text_bytes_cloned > 0,
        "the visible window bytes are counted"
    );

    // Same view and width: the installed durable cache is authoritative, so a
    // second preparation must not rebuild the stable history.
    app.install_conversation(first);
    let second = transcript::prepare_conversation(&app, 79);
    let after = crate::perf::snapshot();
    assert_eq!(
        after.layout_calls, mid.layout_calls,
        "a cached durable layout is not rebuilt for a stable history"
    );
    assert_eq!(after.tool_linear_scans, 0);
    drop(second);
}

#[test]
fn live_tool_mouse_click_collapses_the_running_card() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });

    let prepared = transcript::prepare_conversation(&app, 79);
    let live_tool = prepared
        .sections
        .iter()
        .find(|section| {
            section.id.kind == crate::state::view::SectionKind::Tool
                && section.id.loop_id.as_deref() == Some("loop_live")
                && section.id.tool_call_id.as_deref() == Some("c1")
                && section.id.history_index.is_none()
        })
        .expect("running live Tool section");
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let offset = prepared
        .total_rows()
        .saturating_sub(screen.transcript.height as usize);
    assert!(live_tool.rows.start >= offset, "live Tool must be visible");
    let row = screen.transcript.y + (live_tool.rows.start - offset) as u16;
    let column = screen.content.x + live_tool.content_columns.start as u16;
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };

    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Up(MouseButton::Left)));

    let key = crate::state::tool::ToolKey::new("ses_1", "loop_live", 0, "c1");
    assert_eq!(
        app.active_view().unwrap().tool_folds.get(&key),
        Some(&crate::state::view::FoldOverride::Collapsed),
        "a plain click on a running live Tool must record a per-tool collapse"
    );
    let folded = transcript::prepare_conversation(&app, 79)
        .sections
        .into_iter()
        .find(|section| section.id == live_tool.id)
        .is_some_and(|section| section.folded);
    assert!(
        folded,
        "the running live Tool must render folded after the click"
    );
}

#[test]
fn stale_live_tool_loop_cannot_toggle_the_current_card() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let before = app
        .active_view()
        .and_then(|view| view.live.as_ref())
        .and_then(|live| live.requests.first())
        .and_then(|request| request.tools.first())
        .map(|tool| tool.expanded)
        .expect("live Tool fixture");
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .live
        .as_mut()
        .unwrap()
        .reference = Some(crate::protocol::TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_new".to_owned(),
    });
    app.update(AppEvent::ToggleTool {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_old".to_owned(),
        request_index: 0,
        tool_call_id: "c1".to_owned(),
    });
    let view = app.active_view().unwrap();
    assert_eq!(
        view.live
            .as_ref()
            .and_then(|live| live.requests.first())
            .and_then(|request| request.tools.first())
            .map(|tool| tool.expanded),
        Some(before),
        "an old loop event must not mutate the current live Tool"
    );
    assert!(
        view.tool_folds.is_empty(),
        "an old loop event must not create a fold override"
    );
}

#[test]
fn stale_durable_tool_toggle_cannot_mutate_the_current_live_card() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let view = app.sessions.known.get_mut("ses_1").unwrap();
    view.live.as_mut().unwrap().reference = Some(crate::protocol::TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_new".to_owned(),
    });
    view.live
        .as_mut()
        .unwrap()
        .requests
        .first_mut()
        .unwrap()
        .tools
        .first_mut()
        .unwrap()
        .expanded = true;
    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
        index: Some(99),
        loop_id: "loop_old".to_owned(),
        request_index: 0,
        tool_call_id: "c1".to_owned(),
        name: "read".to_owned(),
        result: None,
        outcome: None,
        live_status: None,
        progress: None,
        expanded: true,
    }));

    app.update(AppEvent::ToggleTool {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_old".to_owned(),
        request_index: 0,
        tool_call_id: "c1".to_owned(),
    });
    assert_eq!(
        app.active_view()
            .unwrap()
            .live
            .as_ref()
            .and_then(|live| live.requests.first())
            .and_then(|request| request.tools.first())
            .map(|tool| tool.expanded),
        Some(true),
        "a stale durable Tool with the same call id must not mutate the new live loop"
    );
}

#[test]
fn prepared_section_ids_survive_tool_result_updates() {
    let mut app = testapp::tools(ThemeKind::Dark);
    let before: Vec<_> = transcript::prepare_conversation(&app, 79)
        .sections
        .into_iter()
        .map(|section| section.id)
        .collect();
    let view = app.sessions.known.get_mut("ses_1").unwrap();
    for block in view.transcript.blocks_mut() {
        let block = std::sync::Arc::make_mut(block);
        if let TranscriptBlock::Tool(tool) = &mut *block {
            if tool.tool_call_id == "call-1" {
                tool.result = Some("a changed result\nwith another line".to_owned().into());
            }
        }
    }
    view.transcript.invalidate();
    let after: Vec<_> = transcript::prepare_conversation(&app, 79)
        .sections
        .into_iter()
        .map(|section| section.id)
        .collect();
    assert_eq!(before, after);
}

#[test]
fn selection_rebases_when_older_history_prepends_rows() {
    let mut app = testapp::chat(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let first = transcript::prepare_conversation(&app, 79);
    let section = first
        .sections
        .iter()
        .find(|section| section.id.kind == crate::state::view::SectionKind::User)
        .expect("user section");
    let section_id = section.id.clone();
    let section_row = 1.min(section.rows.len().saturating_sub(1));
    let point = SelectionPoint {
        row: section.rows.start + section_row,
        column: section.content_columns.start,
        section_id: Some(section_id.clone()),
        section_row,
    };
    app.selection = Some(ConversationSelection {
        session_id: "ses_1".to_owned(),
        anchor: point.clone(),
        focus: point,
        granularity: SelectionGranularity::Word,
        dragged: false,
    });
    app.update(AppEvent::ConversationPrepared(first));

    let view = app.sessions.known.get_mut("ses_1").unwrap();
    view.transcript.insert_block(
        0,
        TranscriptBlock::User(UserBlock {
            index: Some(99),
            loop_id: Some("loop_older".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "older history".to_owned(),
            pending: false,
        }),
    );
    view.transcript.invalidate();
    let second = transcript::prepare_conversation(&app, 79);
    let moved_start = second
        .sections
        .iter()
        .find(|section| section.id == section_id)
        .expect("original user section after prepend")
        .rows
        .start;
    app.update(AppEvent::ConversationPrepared(second));

    let selection = app.selection.as_ref().expect("selection survives prepend");
    assert_eq!(selection.anchor.section_id.as_ref(), Some(&section_id));
    assert_eq!(selection.anchor.row, moved_start + section_row);
    assert_eq!(selection.focus.row, moved_start + section_row);
}

#[test]
fn selection_rebases_when_a_live_section_grows_after_the_selected_row() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let first = transcript::prepare_conversation(&app, 79);
    let row = first
        .copy_ranges
        .iter()
        .find(|range| range.text == "more")
        .expect("live text row");
    let (section_id, section_start) = first
        .sections
        .iter()
        .find(|section| section.rows.contains(&row.row))
        .map(|section| (section.id.clone(), section.rows.start))
        .expect("live text section");
    let point = SelectionPoint {
        row: row.row,
        column: row.columns.start,
        section_id: Some(section_id.clone()),
        section_row: row.row - section_start,
    };
    app.selection = Some(ConversationSelection {
        session_id: "ses_1".to_owned(),
        anchor: point.clone(),
        focus: point,
        granularity: SelectionGranularity::Word,
        dragged: false,
    });
    app.update(AppEvent::ConversationPrepared(first));

    let event = serde_json::from_value(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "channel": "text",
            "delta": "\nnew output",
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    }))
    .expect("live output fixture parses");
    app.update(AppEvent::Rpc(RpcEvent::Frame(
        crate::protocol::IncomingFrame::Notification(crate::protocol::RpcNotification::AgentEvent(
            event,
        )),
    )));
    let second = transcript::prepare_conversation(&app, 79);
    app.update(AppEvent::ConversationPrepared(second.clone()));
    let selection = app
        .selection
        .as_ref()
        .expect("selection survives live growth");
    let moved = second
        .sections
        .iter()
        .find(|candidate| candidate.id == section_id)
        .expect("same live section identity");
    assert_eq!(selection.anchor.section_id.as_ref(), Some(&section_id));
    assert_eq!(
        selection.anchor.row,
        moved.rows.start + selection.anchor.section_row
    );
    assert_eq!(
        selection.focus.row,
        moved.rows.start + selection.focus.section_row
    );
}

#[test]
fn conversation_copy_skips_external_section_spacers_but_keeps_timestamp_content() {
    let app = testapp::chat(ThemeKind::Dark);
    let prepared = transcript::prepare_conversation(&app, 79);
    let user = prepared
        .sections
        .iter()
        .find(|section| section.id.kind == crate::state::view::SectionKind::User)
        .expect("user section");
    let assistant_row = prepared
        .copy_ranges
        .iter()
        .find(|range| range.text.contains("Heading"))
        .expect("assistant heading row");
    let user_row = prepared
        .copy_ranges
        .iter()
        .find(|range| range.text.contains("Hello"))
        .expect("user content row");
    let start = SelectionPoint {
        row: user_row.row,
        column: user_row.columns.start,
        section_id: Some(user.id.clone()),
        section_row: user_row.row - user.rows.start,
    };
    let assistant = prepared
        .sections
        .iter()
        .find(|section| section.rows.contains(&assistant_row.row))
        .expect("assistant section");
    let focus = SelectionPoint {
        row: assistant_row.row,
        column: assistant_row.columns.start + "Heading".chars().count() - 1,
        section_id: Some(assistant.id.clone()),
        section_row: assistant_row.row - assistant.rows.start,
    };
    let selection = ConversationSelection {
        session_id: "ses_1".to_owned(),
        anchor: start,
        focus,
        granularity: SelectionGranularity::Character,
        dragged: true,
    };
    let copied = transcript::selection_text(&prepared, &selection);
    assert!(copied.contains("Hello world with code."));
    assert!(copied.contains("time unavailable"));
    assert!(copied.contains("Heading"));
    assert!(
        !copied.contains("\n\n"),
        "external spacer leaked: {copied:?}"
    );
}

pub(crate) fn draw(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal
}

pub(crate) fn text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

pub(crate) fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let width = terminal.backend().buffer().area.width as usize;
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn any_cell_matching(
    terminal: &Terminal<TestBackend>,
    predicate: impl Fn(&ratatui::buffer::Cell) -> bool,
) -> bool {
    terminal.backend().buffer().content().iter().any(predicate)
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn is_blank(line: &ratatui::text::Line<'_>) -> bool {
    let text = line_text(line);
    text.strip_prefix('▎').unwrap_or(&text).trim().is_empty()
}

fn assert_section_is_vertically_padded(lines: &[ratatui::text::Line<'_>], label: &str) {
    assert!(!lines.is_empty(), "{label} needs at least one row");
    if lines.len() < 3 {
        assert!(
            lines.iter().any(|line| !is_blank(line)),
            "{label} needs visible content"
        );
        return;
    }
    assert!(is_blank(&lines[0]), "{label} needs one blank row above");
    if lines.len() >= 3 {
        assert!(
            !is_blank(&lines[1]),
            "{label} must start content immediately after the top row"
        );
        assert!(
            !is_blank(&lines[lines.len() - 2]),
            "{label} must end content immediately before the bottom row"
        );
        assert!(
            is_blank(&lines[lines.len() - 1]),
            "{label} needs one blank row below"
        );
    }
}

fn assert_no_adjacent_blank_rows(lines: &[ratatui::text::Line<'_>], label: &str) {
    for pair in lines.windows(2) {
        assert!(
            !(pair[0].spans.is_empty() && pair[1].spans.is_empty()),
            "{label} has duplicate boundary blank rows"
        );
    }
}

#[test]
fn message_and_tool_sections_have_symmetric_vertical_padding() {
    let theme = Theme::dark();

    let user_lines = user::lines(
        &theme,
        &UserBlock {
            index: Some(1),
            loop_id: Some("turn".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "user text".to_owned(),
            pending: false,
        },
        40,
    );
    assert_section_is_vertically_padded(&user_lines, "user message");

    let assistant_lines = assistant::lines(
        &theme,
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![AssistantPart::Text("assistant text".to_owned())],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    assert_section_is_vertically_padded(&assistant_lines, "assistant message");

    let visible_thinking = reasoning::visible_lines(&theme, "thinking text", 40);
    let hidden_thinking = reasoning::thinking_line(&theme);
    let live_thinking = reasoning::live_lines(&theme, "live thinking", 40, true);
    for (label, lines) in [
        ("visible thinking", visible_thinking),
        ("hidden thinking", hidden_thinking),
        ("live thinking", live_thinking),
    ] {
        assert_section_is_vertically_padded(&lines, label);
        assert!(
            lines.iter().any(|line| line_text(line).starts_with("▎")),
            "{label} content must use the shared Rail"
        );
    }

    let tool_lines = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "turn".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: Some("command result".to_owned().into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: true,
        },
        40,
        false,
    );
    assert_section_is_vertically_padded(&tool_lines, "durable tool call");

    let live_tool_lines = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            progress: Some("running command".to_owned()),
            display: None,
            result: None,
            result_truncated: false,
            expanded: false,
        },
        40,
    );
    assert_section_is_vertically_padded(&live_tool_lines, "live tool call");
}

#[test]
fn assistant_parts_keep_order_and_share_boundary_padding() {
    let lines = assistant::lines(
        &Theme::dark(),
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![
                AssistantPart::Text("first".to_owned()),
                AssistantPart::Reasoning("thinking".to_owned()),
                AssistantPart::Text("second".to_owned()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    let text: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(text, vec!["", " first", "", "▎thinking", "", " second", ""]);
    assert_no_adjacent_blank_rows(&lines, "assistant text/reasoning/text");
}

#[test]
fn adjacent_assistant_sections_share_one_boundary_row() {
    let theme = Theme::dark();
    let make = |text: &str, index| AssistantBlock {
        index,
        loop_id: "turn".to_owned(),
        request_index: 0,
        model: "model".to_owned(),
        reasoning_level: crate::protocol::Reasoning::Auto,
        parts: vec![AssistantPart::Text(text.to_owned())],
        tool_calls: vec![],
        usage: Default::default(),
        finish_reason: "stop".to_owned(),
        terminal_error: None,
    };
    let mut lines = Vec::new();
    layout::append_section(
        &mut lines,
        assistant::lines(&theme, &make("first", 2), 40, true),
    );
    layout::append_section(
        &mut lines,
        assistant::lines(&theme, &make("second", 3), 40, true),
    );
    assert_eq!(
        lines.iter().map(line_text).collect::<Vec<_>>(),
        vec!["", " first", "", " second", ""]
    );
    assert_no_adjacent_blank_rows(&lines, "adjacent assistant sections");
}

#[test]
fn empty_reasoning_renders_nothing_and_does_not_hide_the_next_run() {
    let theme = Theme::dark();
    assert!(reasoning::reasoning_lines(&theme, "", 40, false, false).is_empty());
    assert!(reasoning::live_lines(&theme, "", 40, false).is_empty());

    let lines = assistant::lines(
        &theme,
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![
                AssistantPart::Text("answer".to_owned()),
                AssistantPart::Reasoning(String::new()),
                AssistantPart::Reasoning("hidden".to_owned()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        false,
    );
    let text: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(text, vec!["", " answer", "", "▎ Thinking...", ""]);
    assert_no_adjacent_blank_rows(&lines, "empty reasoning followed by hidden reasoning");
}

#[test]
fn explicit_markdown_blank_lines_survive_section_padding() {
    let lines = assistant::lines(
        &Theme::dark(),
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![AssistantPart::Text("one\n\nthree".to_owned())],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    let text: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(text, vec!["", " one", "", " three", ""]);
}

#[test]
fn user_assistant_and_tool_boundaries_share_one_blank_row() {
    let theme = Theme::dark();
    let user_lines = user::lines(
        &theme,
        &UserBlock {
            index: Some(1),
            loop_id: Some("turn".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "user".to_owned(),
            pending: false,
        },
        40,
    );
    let assistant_lines = assistant::lines(
        &theme,
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![AssistantPart::Text("assistant".to_owned())],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    let tool_lines = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            progress: None,
            display: None,
            result: None,
            result_truncated: false,
            expanded: false,
        },
        40,
    );
    let mut lines = Vec::new();
    layout::append_section(&mut lines, user_lines);
    layout::append_section(&mut lines, assistant_lines);
    layout::append_section(&mut lines, tool_lines);
    assert_no_adjacent_blank_rows(&lines, "user/assistant/tool");
    assert_eq!(
        lines.iter().filter(|line| !is_blank(line)).count(),
        6,
        "surface sections expose their content, timestamp, and three-line tool preview"
    );
}

#[test]
fn cached_and_fallback_transcripts_have_identical_section_spacing() {
    let theme = Theme::dark();
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .transcript
        .blocks = std::sync::Arc::new(
        vec![
            TranscriptBlock::User(UserBlock {
                index: Some(1),
                loop_id: Some("turn".to_owned()),
                kind: crate::protocol::UserMessageKindWire::Prompt,
                text: "user".to_owned(),
                pending: false,
            }),
            TranscriptBlock::Assistant(AssistantBlock {
                index: 2,
                loop_id: "turn".to_owned(),
                request_index: 0,
                model: "model".to_owned(),
                reasoning_level: crate::protocol::Reasoning::Auto,
                parts: vec![
                    AssistantPart::Text("first".to_owned()),
                    AssistantPart::Reasoning("thinking".to_owned()),
                    AssistantPart::Text("second".to_owned()),
                ],
                tool_calls: vec![],
                usage: Default::default(),
                finish_reason: "stop".to_owned(),
                terminal_error: None,
            }),
            TranscriptBlock::Tool(ToolBlock {
                index: None,
                loop_id: "turn".to_owned(),
                request_index: 0,
                tool_call_id: "call".to_owned(),
                name: "bash".to_owned(),
                result: None,
                outcome: None,
                live_status: None,
                progress: None,
                expanded: false,
            }),
        ]
        .into_iter()
        .map(std::sync::Arc::new)
        .collect(),
    );

    let fallback = transcript::all_lines(&theme, &app, 80);
    let prepared = transcript::prepare_conversation(&app, 80);
    app.update(AppEvent::ConversationPrepared(prepared));
    let cached = transcript::all_lines(&theme, &app, 80);
    assert_eq!(cached, fallback);
    assert_no_adjacent_blank_rows(&cached, "cached transcript");
}

#[test]
fn durable_and_live_tool_sections_have_the_same_padding_shape() {
    let theme = Theme::dark();
    let durable = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "turn".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: None,
            outcome: None,
            live_status: None,
            progress: None,
            expanded: false,
        },
        40,
        false,
    );
    let live = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            progress: None,
            display: None,
            result: None,
            result_truncated: false,
            expanded: false,
        },
        40,
    );
    assert_eq!(durable.len(), live.len());
    assert_section_is_vertically_padded(&durable, "durable collapsed tool");
    assert_section_is_vertically_padded(&live, "live tool");
}

#[test]
fn user_card_uses_the_spec_background() {
    let app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(any_cell_matching(&terminal, |cell| cell.bg == Theme::dark().user_message_bg));
    assert!(text(&terminal).contains("hello"));
}

#[test]
fn assistant_text_has_no_background() {
    let app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let bg = Theme::dark().page_bg;
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == bg && cell.symbol() == "a"),
        "the assistant text sits on the page background"
    );
}

#[test]
fn reasoning_is_gray_and_italic_and_can_be_hidden() {
    let mut app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(
        any_cell_matching(&terminal, |cell| {
            cell.fg == Theme::dark().muted
                && cell.modifier.contains(Modifier::ITALIC)
                && cell.symbol() == "c"
        }),
        "reasoning text 'carefully' is gray italic"
    );

    app.update(AppEvent::ToggleReasoning);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Thinking..."));
    assert!(
        !content.contains("carefully"),
        "hidden reasoning text is gone"
    );
}

#[test]
fn composer_uses_a_fixed_blue_rail_without_a_rectangular_border() {
    let theme = Theme::dark();
    for reasoning in ["high", "low", "medium", "disabled"] {
        let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("t"), reasoning);
        let terminal = draw(&app, 80, 24);
        assert!(
            any_cell_matching(&terminal, |cell| {
                cell.symbol() == "▎" && cell.fg == theme.rail_editor
            }),
            "reasoning level {reasoning} keeps the editor rail blue"
        );
        assert!(!any_cell_matching(&terminal, |cell| cell.symbol() == "╭"));
    }
}

#[test]
fn durable_tool_fold_override_is_honored_by_the_prepared_transcript() {
    // regression: the durable renderer is called with all_expanded=false, but
    // effective_tool_block precomputes the resolved fold so an Expanded
    // override must still expose the full payload rows.
    let mut app = testapp::tools(ThemeKind::Dark); // assembled expanded
    app.update(AppEvent::ToggleTools {
        session_id: "ses_1".to_owned(),
    }); // collapse everything
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .tool_folds
        .insert(
            crate::state::tool::ToolKey::new("ses_1", "loop_1", 0, "call-1"),
            crate::state::view::FoldOverride::Expanded,
        );
    let prepared = crate::ui::transcript::prepare_conversation(&app, 79);
    let tool_sections: Vec<_> = prepared
        .sections
        .iter()
        .filter(|section| section.id.kind == crate::state::view::SectionKind::Tool)
        .collect();
    assert_eq!(tool_sections.len(), 3);
    let expanded_section = &tool_sections[0]; // call-1 has a 60-line result
    assert!(
        expanded_section.rows.len() > 10,
        "expanded durable card must expose full rows, got {:?}",
        expanded_section.rows
    );
    assert!(
        prepared
            .copy_ranges
            .iter()
            .any(|range| range.text.contains("line 59 of a long file")),
        "the tail of the expanded payload must be copy-visible"
    );
    // Without the override the same card defaults to folded (60-line result), so
    // the fold override alone must be the thing widening it.
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .tool_folds
        .clear();
    let prepared_collapsed = crate::ui::transcript::prepare_conversation(&app, 79);
    let read_section_collapsed = prepared_collapsed
        .sections
        .iter()
        .find(|section| section.id.kind == crate::state::view::SectionKind::Tool)
        .unwrap();
    assert!(
        read_section_collapsed.rows.len() <= 6,
        "without an override the 60-line read card is folded"
    );
}

#[test]
fn failed_tool_cards_keep_status_summary_hint_and_error_body() {
    let theme = Theme::dark();
    let display = crate::protocol::ToolDisplayWire {
        detail: "$ run command".to_owned(),
        expanded_input: Some("input body".to_owned()),
        input_line_count: Some(1),
        hidden_line_count: Some(2),
        truncated: false,
    };
    let collapsed = tool::durable_with_display(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "loop".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: Some("permission denied\nprivate diagnostic".to_owned().into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Failed),
            live_status: None,
            progress: None,
            expanded: false,
        },
        80,
        false,
        Some(&display),
    );
    let collapsed_text = collapsed
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(collapsed_text.contains("failed: permission denied"));
    assert!(collapsed_text.contains("ctrl+o to expand"));
    assert!(!collapsed_text.contains("private diagnostic"));

    let expanded = tool::durable_with_display(
        &theme,
        &ToolBlock {
            expanded: true,
            result: Some("permission denied\nprivate diagnostic".to_owned().into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Failed),
            ..ToolBlock {
                index: None,
                loop_id: "loop".to_owned(),
                request_index: 0,
                tool_call_id: "call".to_owned(),
                name: "bash".to_owned(),
                result: None,
                outcome: None,
                live_status: None,
                progress: None,
                expanded: false,
            }
        },
        80,
        false,
        Some(&display),
    );
    let expanded_text = expanded
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded_text.contains("input body"));
    assert!(expanded_text.contains("permission denied"));
    assert!(expanded_text.contains("private diagnostic"));
    assert!(!expanded_text.contains("ctrl+o to expand"));

    let denied = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "loop".to_owned(),
            request_index: 0,
            tool_call_id: "denied".to_owned(),
            name: "write".to_owned(),
            result: Some("not allowed".to_owned().into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Denied),
            live_status: None,
            progress: None,
            expanded: false,
        },
        80,
        false,
    );
    assert!(
        denied
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
            .contains("denied: not allowed")
    );

    let cancelled = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "cancelled".to_owned(),
            name: "read".to_owned(),
            status: ToolStatus::Cancelled,
            progress: None,
            display: None,
            result: Some("cancellation detail".to_owned().into()),
            result_truncated: false,
            expanded: false,
        },
        80,
    );
    let cancelled_text = cancelled
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(cancelled_text.contains("cancelled"));
    assert!(!cancelled_text.contains("cancellation detail"));

    let long_result = "x".repeat(400);
    let long_text = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "loop".to_owned(),
            request_index: 0,
            tool_call_id: "long".to_owned(),
            name: "bash".to_owned(),
            result: Some(long_result.into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Failed),
            live_status: None,
            progress: None,
            expanded: false,
        },
        80,
        false,
    )
    .iter()
    .map(line_text)
    .collect::<Vec<_>>()
    .join("\n");
    assert!(!long_text.contains(&"x".repeat(121)));

    let unknown = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "loop".to_owned(),
            request_index: 0,
            tool_call_id: "unknown".to_owned(),
            name: "read".to_owned(),
            result: None,
            outcome: Some(crate::protocol::ToolOutcomeWire::Unknown),
            live_status: None,
            progress: None,
            expanded: true,
        },
        80,
        false,
    );
    assert!(
        unknown
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
            .contains("outcome unknown: unconfirmed")
    );
}

#[test]
fn cancelled_calls_use_the_dedicated_surface_in_live_and_durable_cards() {
    let theme = Theme::dark();
    // The same card identity takes the dedicated cancelled surface both while
    // it is live (cancelled in-flight) and once it is durable (stored cancel).
    let live = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "c".to_owned(),
            name: "read".to_owned(),
            status: ToolStatus::Cancelled,
            progress: None,
            display: None,
            result: None,
            result_truncated: false,
            expanded: true,
        },
        40,
    );
    let durable = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "t".to_owned(),
            request_index: 0,
            tool_call_id: "c".to_owned(),
            name: "read".to_owned(),
            result: None,
            outcome: Some(crate::protocol::ToolOutcomeWire::Cancelled),
            live_status: None,
            progress: None,
            expanded: true,
        },
        40,
        false,
    );
    for (label, lines) in [("live", &live), ("durable", &durable)] {
        let header = &lines[1];
        assert!(
            header
                .spans
                .iter()
                .any(|span| span.style.bg == Some(theme.tool_cancelled_bg)),
            "{label} cancelled header must carry the spec card background"
        );
        assert_eq!(
            header.spans[0].style.fg,
            Some(theme.tool_cancelled_rail),
            "{label} cancelled rail must carry the spec rail colour"
        );
    }
}

#[test]
fn tool_cards_use_state_backgrounds_and_expanded_preview_bounds() {
    let theme = Theme::dark();
    let make = |outcome: Option<crate::protocol::ToolOutcomeWire>| ToolBlock {
        index: None,
        loop_id: "t".into(),
        request_index: 0,
        tool_call_id: "c".into(),
        name: "read".into(),
        result: Some(Arc::<str>::from("data")),
        outcome,
        live_status: None,
        progress: None,
        expanded: true,
    };
    // Exact card backgrounds per state, asserted at the line level so the
    // viewport cannot hide them.
    for (outcome, expected) in [
        (
            Some(crate::protocol::ToolOutcomeWire::Success),
            theme.tool_success_bg,
        ),
        (
            Some(crate::protocol::ToolOutcomeWire::Denied),
            theme.tool_error_bg,
        ),
        (
            Some(crate::protocol::ToolOutcomeWire::Failed),
            theme.tool_error_bg,
        ),
        (None, theme.tool_pending_bg),
    ] {
        let lines = tool::durable(&theme, &make(outcome), 40, false);
        let header = &lines[1];
        let has_bg = header
            .spans
            .iter()
            .any(|span| span.style.bg == Some(expected));
        assert!(
            has_bg,
            "outcome {outcome:?} should use the {expected:?} card background"
        );
    }

    // Viewport level: global expansion exposes the complete bounded result;
    // there is no fixed 40-line renderer cap.
    let app = testapp::tools(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(!content.contains("more lines"));
    assert!(content.contains("line 59"));
}

#[test]
fn tool_expanded_preview_remains_available_without_a_fixed_renderer_cap() {
    let theme = Theme::dark();
    let make = |result: &str| ToolBlock {
        index: None,
        loop_id: "t".into(),
        request_index: 0,
        tool_call_id: "c".into(),
        name: "bash".into(),
        result: Some(result.to_owned().into()),
        outcome: Some(crate::protocol::ToolOutcomeWire::Success),
        live_status: None,
        progress: None,
        expanded: true,
    };
    let single_line = "x".repeat(40_000);
    let lines = tool::durable(&theme, &make(&single_line), 120, false);
    let joined: String = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    // The full payload survives: nothing is clipped and every row is wrapped
    // to the content width (a single 40_000-char row would hold every char
    // and prove wrapping never ran).
    assert_eq!(
        joined.chars().filter(|&c| c == 'x').count(),
        40_000,
        "the full payload must be preserved"
    );
    let longest_row = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    assert!(
        lines.len() > 300 && longest_row <= 120,
        "long results must wrap into width-bounded rows, not clip"
    );

    let many_lines = (0..60)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = tool::durable(&theme, &make(&many_lines), 120, false);
    let joined: String = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    assert!(joined.contains("line 0"));
    assert!(joined.contains("line 59"));
    assert!(!joined.contains("more lines"));
}

#[test]
fn workspace_shortening_replaces_the_home_prefix() {
    let home = std::path::Path::new("/home/user");
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user/project"), Some(home)),
        "~/project"
    );
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user"), Some(home)),
        "~"
    );
    // A sibling directory must not be shortened by a prefix match.
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user2/project"), Some(home)),
        "/home/user2/project"
    );
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/srv/other"), Some(home)),
        "/srv/other"
    );
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user/project"), None),
        "/home/user/project"
    );
}

#[test]
fn footer_hides_secondary_info_below_80_columns() {
    let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let narrow = draw(&app, 70, 24);
    let narrow_text = text(&narrow);
    assert!(narrow_text.contains("deep"), "model stays visible");
    assert!(narrow_text.contains("high"), "reasoning stays visible");
    assert!(narrow_text.contains("ctx ?"), "unknown context is explicit");

    let wide = draw(&app, 120, 40);
    let wide_text = text(&wide);
    assert!(wide_text.contains("project"));
    assert!(
        !wide_text.contains("Task"),
        "title is not a default footer field"
    );
}

#[test]
fn footer_is_one_row_on_short_terminals() {
    let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    // The footer is one row at every terminal height.
    let terminal = draw(&app, 80, 23);
    let content = text(&terminal);
    assert!(content.contains("ready"));
    assert!(content.contains("deep"));
    assert!(content.contains("high"));
}

#[test]
fn running_live_turn_shows_gap_footer_and_status_spinner() {
    let app = testapp::live_turn(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(
        content.contains("⚠ incomplete"),
        "event gap shows in the footer"
    );
    assert!(
        content.contains("Running read"),
        "running tool in the status row"
    );
}

#[test]
fn last_result_renders_outcome_and_persistence_in_status_and_transcript() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let turn = |loop_id: &str| crate::protocol::TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: loop_id.to_owned(),
    };

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_done"),
            outcome: crate::protocol::LoopOutcomeWire::Completed,
            persistence: Some(crate::protocol::TurnPersistenceWire::Persisted),
            usage: Some(Default::default()),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            accepted_at: None,
            completed_at: None,
        });
    }
    let content = text(&draw(&app, 120, 40));
    assert!(!content.contains("completed · persisted"));
    assert!(!content.contains("Turn completed"));

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_cancelled"),
            outcome: crate::protocol::LoopOutcomeWire::Cancelled {
                reason: crate::protocol::CancelReasonWire::Unknown("sandbox_evicted".to_owned()),
            },
            persistence: Some(crate::protocol::TurnPersistenceWire::Persisted),
            usage: Some(Default::default()),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            accepted_at: None,
            completed_at: None,
        });
    }
    let content = text(&draw(&app, 80, 24));
    assert!(content.contains("cancelled (sandbox_evicted)"));
    assert!(content.contains("persisted"));

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_shutdown"),
            outcome: crate::protocol::LoopOutcomeWire::Cancelled {
                reason: crate::protocol::CancelReasonWire::Shutdown,
            },
            persistence: Some(crate::protocol::TurnPersistenceWire::Persisted),
            usage: Some(Default::default()),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            accepted_at: None,
            completed_at: None,
        });
    }
    let content = text(&draw(&app, 80, 24));
    assert!(content.contains("cancelled (shutdown) · persisted"));

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_failed"),
            outcome: crate::protocol::LoopOutcomeWire::Failed {
                kind: "model_error".to_owned(),
                model_error: Some(crate::protocol::ModelErrorWire {
                    kind: "rate_limit".to_owned(),
                    delivery: "upstream".to_owned(),
                    retryable: true,
                    retry_after_millis: None,
                }),
            },
            persistence: Some(crate::protocol::TurnPersistenceWire::Failed),
            usage: Some(Default::default()),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            accepted_at: None,
            completed_at: None,
        });
        view.state.as_mut().unwrap().status = crate::protocol::SessionStatusWire::Blocked;
        view.state.as_mut().unwrap().block_reason =
            Some(crate::protocol::SessionBlockReasonWire::Persistence);
    }
    let content = text(&draw(&app, 120, 40));
    assert!(content.contains("failed: model_error: rate_limit"));
    assert!(content.contains("persistence failed"));
    assert!(content.contains("Blocked · persistence"));
}

#[test]
fn live_request_without_model_is_explicitly_unknown() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .live
        .as_mut()
        .unwrap()
        .requests[0]
        .model
        .clear();
    let content = text(&draw(&app, 120, 40));
    assert!(!content.contains("Request #0 · config unknown"));
    assert!(content.contains("working") || content.contains("incomplete"));
}

#[test]
fn footer_waiting_boundary_shows_next_config_with_current_request_preserved() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    // live_turn has request 0 with model "deep", reasoning High, rev 0
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.config_update = Some(crate::state::session::PendingConfigUpdate {
            loop_id: Some("loop_live".to_string()),
            model: Some("fast".to_string()),
            reasoning: Some(crate::protocol::Reasoning::Low),
            revision: Some(2),
            state: crate::state::session::ConfigUpdateState::WaitingBoundary,
        });
    }
    let terminal = draw(&app, 120, 40);
    let content = text(&terminal);
    assert!(content.contains("project"));
    assert!(content.contains("deep"));
    assert!(content.contains("high"));
    assert!(!content.contains("request 0 · deep · high · rev 0"));
    assert!(!content.contains("next: fast"));
}

#[test]
fn fatal_connection_renders_the_overlay() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "unfinished".into(),
    });
    app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: "latest agent stderr".len(),
        dropped: 0,
    }));
    app.update(AppEvent::Rpc(RpcEvent::Exited(None)));
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Fatal error"));
    assert!(content.contains("Exit status: unavailable"));
    assert!(content.contains("result/save status unconfirmed"));
    assert!(content.contains("Tool side effects may already exist"));
    assert!(content.contains("agent stderr: 19 bytes"), "{content}");
    assert!(content.contains("Press q to quit"));
}

#[test]
fn fatal_overlay_retains_a_known_result_summary() {
    let mut app = testapp::shutdown_cancel_result(ThemeKind::Dark);
    app.update(AppEvent::Rpc(RpcEvent::Exited(None)));
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Known turn result:"));
    assert!(content.contains("cancelled (shutdown) · persisted"));
}

#[test]
fn recent_notices_render_above_the_composer() {
    let mut app = testapp::fresh(ThemeKind::Dark);
    app.update(AppEvent::SubmitTurn {
        session_id: "none".into(),
        text: "hi".into(),
    });
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("unavailable"));
}

#[test]
fn cursor_sits_at_the_composer_caret() {
    // The hardware cursor is placed by `composer::render` through
    // `frame.set_cursor_position` using `unicode-width` column math; the
    // per-character column rules are unit-tested in `markdown`.
    let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let mut terminal = draw(&app, 80, 24);
    let position = terminal.backend_mut().get_cursor_position().unwrap();
    assert!(position.x >= 2, "cursor starts after the gutter and rail");
    assert!(position.y < 24);
}

#[test]
fn light_theme_renders_identically_shaped_content() {
    let app = testapp::chat_with_reasoning(ThemeKind::Light);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("hello"));
    assert!(any_cell_matching(&terminal, |cell| cell.bg == Theme::light().user_message_bg));
}

#[test]
fn conversation_drag_copies_across_blocks_without_the_rail_or_padding() {
    let mut app = testapp::chat(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let total = prepared.total_rows();
    let offset = total.saturating_sub(screen.transcript.height as usize);
    let start = prepared
        .copy_ranges
        .iter()
        .find(|range| range.row >= offset && range.text.contains("quoted wisdom"))
        .expect("visible quote row");
    let end = prepared
        .copy_ranges
        .iter()
        .find(|range| range.row >= offset && range.text.contains("fn hello()"))
        .expect("visible code row");
    let start_row = start.row;
    let end_row = end.row;
    let start_text_at = start.text.find("quoted wisdom").unwrap();
    let end_text_at = end.text.find("fn hello()").unwrap();
    let start_column =
        start.columns.start + unicode_width::UnicodeWidthStr::width(&start.text[..start_text_at]);
    let end_column = end.columns.start
        + unicode_width::UnicodeWidthStr::width(&end.text[..end_text_at])
        + "fn hello()".len()
        - 1;
    app.update(AppEvent::ConversationPrepared(prepared));

    let mouse = |kind, column, row| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: screen.content.x + column as u16,
            row: screen.transcript.y + (row - offset) as u16,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };
    app.update(mouse(
        MouseEventKind::Down(MouseButton::Left),
        start_column,
        start_row,
    ));
    app.update(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        end_column,
        end_row,
    ));
    let commands = app.update(mouse(
        MouseEventKind::Up(MouseButton::Left),
        end_column,
        end_row,
    ));
    let copied = commands.into_iter().find_map(|command| match command {
        crate::command::AppCommand::CopySelection(text) => Some(text),
        _ => None,
    });
    let copied = copied.expect("drag release produces a copy command");
    assert!(
        copied.as_str().contains("quoted wisdom"),
        "copied text: {:?}",
        copied.as_str()
    );
    assert!(copied.as_str().contains("fn hello()"));
    assert!(!copied.as_str().contains('▎'));
}

#[test]
fn scrollbar_drag_body_follows_before_release() {
    let mut app = testapp::tools(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let total = prepared.total_rows();
    let visible = transcript::visible_rows(&app, total, screen.transcript.height);
    let current = total.saturating_sub(visible);
    let geometry = crate::ui::scrollbar::geometry(screen.transcript, total, current)
        .expect("tool transcript overflows");
    let view_offset_before = app.active_view().unwrap().scroll.offset;
    assert!(app.active_view().unwrap().scroll.follow_tail);
    let down = |kind, row| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: geometry.column as u16,
            row: row as u16,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };

    app.update(down(
        MouseEventKind::Down(MouseButton::Left),
        geometry.thumb_top,
    ));
    let preview_row = geometry.track_top;
    app.update(down(MouseEventKind::Drag(MouseButton::Left), preview_row));
    let pending = app
        .scrollbar_preview_offset("ses_1")
        .expect("pending offset");
    assert_eq!(pending, 0);
    assert_eq!(app.active_view().unwrap().scroll.offset, view_offset_before);
    assert!(!app.active_view().unwrap().scroll.follow_tail);
    assert!(
        buffer_lines(&draw(&app, 80, 24))
            .iter()
            .any(|row| row.contains("line 00")),
        "the transcript body must follow the pending scrollbar offset before release"
    );

    app.update(down(MouseEventKind::Up(MouseButton::Left), preview_row));
    assert!(app.scrollbar_preview_offset("ses_1").is_none());
    let view = app.active_view().unwrap();
    assert!(!view.scroll.follow_tail);
    assert_eq!(view.scroll.offset, pending);
}

#[test]
fn double_click_selects_one_unicode_word() {
    let mut app = testapp::chat(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let total = prepared.total_rows();
    let offset = total.saturating_sub(screen.transcript.height as usize);
    let row = prepared
        .copy_ranges
        .iter()
        .find(|range| range.row >= offset && range.text.contains("quoted wisdom"))
        .expect("visible quote row");
    let row_number = row.row;
    let text_at = row.text.find("quoted").unwrap();
    let column =
        row.columns.start + unicode_width::UnicodeWidthStr::width(&row.text[..text_at]) + 2;
    app.update(AppEvent::ConversationPrepared(prepared));
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: screen.content.x + column as u16,
            row: screen.transcript.y + (row_number - offset) as u16,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Up(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    let commands = app.update(mouse(MouseEventKind::Up(MouseButton::Left)));
    let copied = commands.into_iter().find_map(|command| match command {
        crate::command::AppCommand::CopySelection(text) => Some(text),
        _ => None,
    });
    assert_eq!(
        copied.expect("double click copies a word").as_str(),
        "quoted"
    );
}

#[test]
fn conversation_selection_uses_grapheme_boundaries_for_cjk_and_emoji() {
    let mut app = testapp::cjk(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let row = prepared
        .copy_ranges
        .iter()
        .find(|range| range.text.contains("中文"))
        .expect("CJK content row");
    let cjk_at = row.text.find("中文").expect("CJK word");
    let cjk_column = row.columns.start + unicode_width::UnicodeWidthStr::width(&row.text[..cjk_at]);
    let row_number = row.row;
    let offset = prepared
        .total_rows()
        .saturating_sub(screen.transcript.height as usize);
    app.update(AppEvent::ConversationPrepared(prepared));
    let mouse = |kind, column| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: screen.content.x + column as u16,
            row: screen.transcript.y + (row_number - offset) as u16,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };
    app.update(mouse(MouseEventKind::Down(MouseButton::Left), cjk_column));
    let copied = app.update(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        cjk_column + 1,
    ));
    assert!(copied.is_empty());
    let copied = app.update(mouse(MouseEventKind::Up(MouseButton::Left), cjk_column + 1));
    let copied = copied.into_iter().find_map(|command| match command {
        crate::command::AppCommand::CopySelection(text) => Some(text),
        _ => None,
    });
    assert_eq!(
        copied
            .expect("wide CJK grapheme copies as one unit")
            .as_str(),
        "中"
    );

    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let emoji_row = prepared
        .copy_ranges
        .iter()
        .find(|range| range.text.contains('😀'))
        .expect("emoji content row");
    let emoji_at = emoji_row.text.find('😀').expect("emoji");
    let emoji_start = emoji_row.columns.start
        + unicode_width::UnicodeWidthStr::width(&emoji_row.text[..emoji_at]);
    let emoji_section = prepared
        .sections
        .iter()
        .find(|section| section.rows.contains(&emoji_row.row))
        .expect("emoji section");
    let emoji_selection = ConversationSelection {
        session_id: "ses_1".to_owned(),
        anchor: SelectionPoint {
            row: emoji_row.row,
            column: emoji_start + 1,
            section_id: Some(emoji_section.id.clone()),
            section_row: emoji_row.row - emoji_section.rows.start,
        },
        focus: SelectionPoint {
            row: emoji_row.row,
            column: emoji_start + 1,
            section_id: Some(emoji_section.id.clone()),
            section_row: emoji_row.row - emoji_section.rows.start,
        },
        granularity: SelectionGranularity::Word,
        dragged: false,
    };
    assert_eq!(
        transcript::selection_text(&prepared, &emoji_selection),
        "😀"
    );

    let mut word_app = testapp::cjk(ThemeKind::Dark);
    word_app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let word_screen = layout::screen_layout(&word_app, Rect::new(0, 0, 80, 24));
    let word_prepared = transcript::prepare_conversation(&word_app, word_screen.content.width);
    let word_row = word_prepared
        .copy_ranges
        .iter()
        .find(|range| range.text.contains("中文"))
        .expect("CJK word row");
    let word_at = word_row.text.find("中文").unwrap();
    let word_column =
        word_row.columns.start + unicode_width::UnicodeWidthStr::width(&word_row.text[..word_at]);
    let word_row_number = word_row.row;
    let word_offset = word_prepared
        .total_rows()
        .saturating_sub(word_screen.transcript.height as usize);
    word_app.update(AppEvent::ConversationPrepared(word_prepared));
    let word_mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: word_screen.content.x + word_column as u16,
            row: word_screen.transcript.y + (word_row_number - word_offset) as u16,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };
    word_app.update(word_mouse(MouseEventKind::Down(MouseButton::Left)));
    word_app.update(word_mouse(MouseEventKind::Up(MouseButton::Left)));
    word_app.update(word_mouse(MouseEventKind::Down(MouseButton::Left)));
    let copied = word_app.update(word_mouse(MouseEventKind::Up(MouseButton::Left)));
    let copied = copied.into_iter().find_map(|command| match command {
        crate::command::AppCommand::CopySelection(text) => Some(text),
        _ => None,
    });
    assert_eq!(
        copied.expect("CJK double click copies a word").as_str(),
        "中文"
    );
}

#[test]
fn word_selection_drag_extends_from_the_original_word_range() {
    let mut app = testapp::chat(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let row = prepared
        .copy_ranges
        .iter()
        .find(|range| range.text.contains("quoted wisdom"))
        .expect("quote row");
    let quoted_at = row.text.find("quoted").unwrap();
    let wisdom_at = row.text.find("wisdom").unwrap();
    let quoted_column =
        row.columns.start + unicode_width::UnicodeWidthStr::width(&row.text[..quoted_at]) + 2;
    let wisdom_column = row.columns.start
        + unicode_width::UnicodeWidthStr::width(&row.text[..wisdom_at])
        + "wisdom".chars().count()
        - 1;
    let row_number = row.row;
    let offset = prepared
        .total_rows()
        .saturating_sub(screen.transcript.height as usize);
    app.update(AppEvent::ConversationPrepared(prepared));
    let mouse = |kind, column| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: screen.content.x + column as u16,
            row: screen.transcript.y + (row_number - offset) as u16,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };
    app.update(mouse(
        MouseEventKind::Down(MouseButton::Left),
        quoted_column,
    ));
    app.update(mouse(MouseEventKind::Up(MouseButton::Left), quoted_column));
    app.update(mouse(
        MouseEventKind::Down(MouseButton::Left),
        quoted_column,
    ));
    app.update(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        wisdom_column,
    ));
    let commands = app.update(mouse(MouseEventKind::Up(MouseButton::Left), wisdom_column));
    let copied = commands.into_iter().find_map(|command| match command {
        crate::command::AppCommand::CopySelection(text) => Some(text),
        _ => None,
    });
    assert_eq!(
        copied
            .expect("word drag copies the extended range")
            .as_str(),
        "quoted wisdom"
    );
}

// ---- Phase 4: selectors and the new-session form -----------------------

#[test]
fn selector_panel_replaces_the_composer_and_keeps_the_transcript_visible() {
    let app = testapp::model_selector(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    // The transcript stays visible above the dock (spec 24.1).
    assert!(content.contains("hello") || content.contains("Select model"));
    assert!(content.contains("Select model"));
    assert!(content.contains("Model applies at the next model request."));
    assert!(content.contains("128k context"));
    assert!(content.contains("✓ tools"));
    assert!(content.contains("— tools"));
    assert!(content.contains("✓ current"));
    let dark = Theme::dark();
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == dark.selected_bg),
        "the selected row uses selected_bg"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.symbol() == "→"
            && cell.fg == dark.accent),
        "the selection arrow is accent"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.symbol() == "┌"
            && cell.fg == dark.border_accent),
        "the panel border is accent"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.symbol() == "✓"
            && cell.fg == dark.success),
        "the current marker is success colored"
    );
}

fn max_reasoning_flow_selects_and_ships_the_literal_max_level() -> crate::app::App {
    let mut app = testapp::luna_session(ThemeKind::Dark);
    // Open the active-session reasoning selector; cursor starts at `high`
    // (index 4 of the full ladder) and moves to `max` (index 6).
    app.update(AppEvent::OpenReasoningSelector);
    app.update(AppEvent::MoveSelector { delta: 2 });
    let commands = testapp::take_requests(app.update(AppEvent::ConfirmDock));
    let update = commands
        .iter()
        .find(|request| request.method == "session.update")
        .expect("session.update must be issued");
    assert_eq!(
        update.params["reasoning"],
        serde_json::json!("max"),
        "the wire must carry the literal `max` value, got: {}",
        update.params
    );
    let request = update.clone();
    testapp::respond(
        &mut app,
        &request,
        serde_json::json!({
            "session": {
                "session_id": "ses_main",
                "title": null,
                "profile": "coding",
                "workspace": "/work/cli",
                "model": "luna",
                "reasoning": "max",
                "loaded": true,
                "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            },
            "active_revision": null
        }),
    );
    app
}

#[test]
fn reasoning_selector_offers_max_only_when_the_model_advertises_it() {
    let mut app = testapp::luna_session(ThemeKind::Dark);
    app.update(AppEvent::OpenReasoningSelector);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(
        content.contains("Maximum reasoning"),
        "max level is listed for luna"
    );
    assert!(
        content.contains("Extra-deep reasoning"),
        "xhigh level is listed"
    );
    assert!(content.contains("Ultra reasoning"), "ultra level is listed");
    // deep still stops at high: it must not offer max.
    let app = testapp::reasoning_selector(ThemeKind::Dark);
    let content = text(&draw(&app, 80, 24));
    assert!(!content.contains("Maximum reasoning"));
    assert!(!content.contains("Ultra reasoning"));
}

#[test]
fn selecting_max_updates_the_session_and_footer_shows_max() {
    let app = max_reasoning_flow_selects_and_ships_the_literal_max_level();
    // Idle (no live loop): the durable session setting is the footer authority.
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(
        content.contains("max") && !content.contains("· deep · high ·"),
        "footer must show the newly selected max level, got: {content}"
    );
}

#[test]
fn running_request_metadata_drives_footer_until_the_later_request_uses_max() {
    let mut app = testapp::luna_session(ThemeKind::Dark);
    let agent_event = |app: &mut App, value: serde_json::Value| {
        app.update(AppEvent::Rpc(RpcEvent::Frame(
            crate::protocol::IncomingFrame::Notification(
                crate::protocol::RpcNotification::AgentEvent(
                    serde_json::from_value(value).unwrap(),
                ),
            ),
        )));
    };
    // Start a live loop whose request 0 metadata is the historical `high`.
    let commands = testapp::take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_main".to_owned(),
        text: "stream me".to_owned(),
    }));
    assert_eq!(commands.len(), 1);
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "turn_started",
            "data": {"turn": {"session_id": "ses_main", "loop_id": "loop_live"},
                     "meta": {"session_id": "ses_main", "dropped_before": 0}}
        }),
    );
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "request_started",
            "data": {
                "turn": {"session_id": "ses_main", "loop_id": "loop_live"},
                "request_index": 0,
                "config_revision": 0,
                "model": "luna",
                "reasoning": "high",
                "meta": {"session_id": "ses_main", "dropped_before": 0}
            }
        }),
    );
    // Session updated to max while this request is in flight; the ack is the
    // truth, so the footer shows the acknowledged max immediately (0.2.3).
    app.update(AppEvent::OpenReasoningSelector);
    app.update(AppEvent::MoveSelector { delta: 2 });
    let commands = testapp::take_requests(app.update(AppEvent::ConfirmDock));
    let update = commands
        .iter()
        .find(|request| request.method == "session.update")
        .expect("session.update during the loop");
    testapp::respond(
        &mut app,
        update,
        serde_json::json!({
            "session": {
                "session_id": "ses_main", "title": null, "profile": "coding",
                "workspace": "/work/cli", "model": "luna", "reasoning": "max",
                "loaded": true, "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            },
            "active_revision": null
        }),
    );
    // Footer shows the acknowledged session max at once; the in-flight request
    // metadata stays `high` and is never rewritten.
    let content = text(&draw(&app, 80, 24));
    assert!(
        content.contains(" · max · "),
        "footer must show the acknowledged max immediately: {content}"
    );
    let live = app.sessions.known["ses_main"].live.as_ref().unwrap();
    assert_eq!(
        live.requests[0].reasoning,
        crate::protocol::Reasoning::High,
        "running request reasoning must not be rewritten to the session max"
    );
    // The later request uses the updated level in its own metadata.
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "request_started",
            "data": {
                "turn": {"session_id": "ses_main", "loop_id": "loop_live"},
                "request_index": 1,
                "config_revision": 1,
                "model": "luna",
                "reasoning": "max",
                "meta": {"session_id": "ses_main", "dropped_before": 0}
            }
        }),
    );
    let live = app.sessions.known["ses_main"].live.as_ref().unwrap();
    assert_eq!(
        live.requests[1].reasoning,
        crate::protocol::Reasoning::Max,
        "the later request uses the updated level"
    );
}

#[test]
fn reasoning_selector_lists_only_supported_levels_with_the_current_session_header() {
    let app = testapp::reasoning_selector(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Select reasoning"));
    assert!(content.contains("Current session: —"));
    assert!(content.contains("New session setting: high"));
    assert!(content.contains("Provider default"));
    assert!(content.contains("Deep reasoning"));
    // deep supports auto/low/medium/high; disabled is not listed.
    assert!(content.contains("Moderate reasoning"));
    assert!(!content.contains("No reasoning"));
    assert!(
        any_cell_matching(&terminal, |cell| cell.fg == Theme::dark().thinking_high),
        "reasoning rows use the thinking colors"
    );
}

#[test]
fn session_selector_sorts_newest_first_and_marks_running_loaded_idle() {
    let app = testapp::session_selector_all(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    // updated_at descending: ses_main (5m) then ses_recent (15m) then ses_old (1d).
    assert!(content.find("ses_main") < content.find("Web app"));
    assert!(content.find("Web app") < content.find("Rust port"));
    for marker in ["◉", "●", "○"] {
        assert!(content.contains(marker), "missing status marker {marker}");
    }
    assert!(content.contains("5m"));
    assert!(content.contains("15m"));
    assert!(content.contains("1d"));
    assert!(content.contains("/work/web"));
}

#[test]
fn session_panel_mouse_selection_and_double_click_use_shared_content_rect() {
    let (models, profiles, sessions) = testapp::standard_catalog();
    let mut app = testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions.clone());
    testapp::open_session_selector_all(&mut app, sessions);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let state = match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state,
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let panel = crate::ui::selector::session_panel_layout(screen.panel, state);
    let row = panel.content.y + 2;
    let column = panel.content.x + 2;
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(
        match &app.dock {
            crate::state::selection::Dock::SessionSelector(state) => {
                state.selected_session_id.as_deref()
            }
            _ => None,
        },
        Some("ses_recent")
    );

    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    let requests = testapp::take_requests(app.update(mouse(MouseEventKind::Up(MouseButton::Left))));
    assert!(
        requests
            .iter()
            .any(|request| request.method == "session.open")
    );
}

#[test]
fn generic_selector_mouse_selects_and_confirms_the_hit_item() {
    let mut app = testapp::model_selector(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let state = match &app.dock {
        crate::state::selection::Dock::ModelSelector(state) => state,
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let panel = selector::catalog_panel_layout(screen.panel, &app, state);
    let row = panel.content.y + 1;
    let column = panel.content.x + 2;
    let hit = selector::selector_item_at(&app, screen.panel, state, column, row)
        .expect("second model row is hit-testable");
    assert_eq!(hit.key, "fast");
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Up(MouseButton::Left)));
    assert_eq!(
        match &app.dock {
            crate::state::selection::Dock::ModelSelector(state) => state.cursor,
            _ => usize::MAX,
        },
        1
    );
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    let requests = testapp::take_requests(app.update(mouse(MouseEventKind::Up(MouseButton::Left))));
    assert!(requests.is_empty());
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::ReasoningSelector(state)
            if state.model_context.as_deref() == Some("fast")
    ));
}

#[test]
fn session_footer_mouse_refresh_uses_the_action_hit_rect() {
    let (models, profiles, sessions) = testapp::standard_catalog();
    let mut app = testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions.clone());
    testapp::open_session_selector(&mut app, sessions);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let state = match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state,
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let panel = selector::session_panel_layout(screen.panel, state);
    let (column, row) = (panel.footer.x..panel.footer.right())
        .flat_map(|column| (panel.footer.y..panel.footer.bottom()).map(move |row| (column, row)))
        .find(|&(column, row)| {
            selector::session_action_at(&app, screen.panel, state, column, row)
                == Some(crate::state::selection::SessionPanelAction::Refresh)
        })
        .expect("Refresh footer action is hit-testable");
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    let requests = testapp::take_requests(app.update(mouse(MouseEventKind::Up(MouseButton::Left))));
    assert!(
        requests
            .iter()
            .any(|request| request.method == "session.list")
    );
}

#[test]
fn event_gap_blocks_session_panel_close_and_delete_but_allows_rename() {
    let (models, profiles, sessions) = testapp::standard_catalog();
    let mut app = testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions.clone());
    testapp::open_session_selector_all(&mut app, sessions);
    let view = app.sessions.known.get_mut("ses_main").unwrap();
    view.event_gap = true;
    view.state = Some(crate::protocol::SessionStateWire {
        session_id: "ses_main".to_owned(),
        status: crate::protocol::SessionStatusWire::Idle,
        active_loop: None,
        block_reason: None,
        compaction: None,
    });

    let key =
        |code, modifiers| AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(code, modifiers)));
    app.update(key(KeyCode::F(2), KeyModifiers::empty()));
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::SessionSelector(state)
            if matches!(&state.mode, crate::state::selection::SessionPanelMode::Rename { .. })
    ));
    app.update(AppEvent::Terminal(CrosstermEvent::Paste(
        "renamed".to_owned(),
    )));
    let rename = testapp::take_requests(app.update(key(KeyCode::Enter, KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.rename")
        .expect("event_gap must not block rename");
    testapp::respond(
        &mut app,
        &rename,
        json!({
            "session": {
                "session_id": "ses_main",
                "title": "renamed",
                "profile": "coding",
                "workspace": "/work/cli",
                "model": "deep",
                "reasoning": "high",
                "loaded": true,
                "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            }
        }),
    );
    app.sessions.known.get_mut("ses_main").unwrap().event_gap = true;
    app.update(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
    app.update(key(KeyCode::Delete, KeyModifiers::empty()));
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::SessionSelector(state)
            if matches!(&state.mode, crate::state::selection::SessionPanelMode::Browse)
    ));
    assert!(app.pending_requests.values().all(|request| {
        !matches!(
            request,
            crate::app::RequestKind::CloseSession { .. }
                | crate::app::RequestKind::CloseVerifyState { .. }
                | crate::app::RequestKind::DeleteSession { .. }
        )
    }));
    assert!(
        app.pending_requests
            .values()
            .all(|request| { !matches!(request, crate::app::RequestKind::SessionState { .. }) })
    );
}

#[test]
fn session_double_click_release_rechecks_the_current_target() {
    let (models, profiles, sessions) = testapp::standard_catalog();
    let mut app = testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions.clone());
    testapp::open_session_selector(&mut app, sessions);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let state = match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state,
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let panel = crate::ui::selector::session_panel_layout(screen.panel, state);
    let column = panel.content.x + 2;
    let row = panel.content.y;
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }))
    };

    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Up(MouseButton::Left)));
    app.update(mouse(MouseEventKind::Down(MouseButton::Left)));

    let refresh = testapp::take_requests(app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(5),
            crossterm::event::KeyModifiers::empty(),
        ),
    ))))
    .into_iter()
    .find(|request| request.method == "session.list")
    .expect("refresh request");
    testapp::respond(
        &mut app,
        &refresh,
        json!({
            "sessions": [
                {"session_id":"ses_recent","title":"Web app","profile":"review","workspace":"/work/web","model":"fast","reasoning":"medium","loaded":false,"created_at":"2027-01-15T07:45:00Z","updated_at":"2027-01-15T07:45:00Z"},
                {"session_id":"ses_old","title":"Rust port","profile":"coding","workspace":"/work/rust","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-14T08:00:00Z","updated_at":"2027-01-14T08:00:00Z"}
            ]
        }),
    );

    let commands = app.update(mouse(MouseEventKind::Up(MouseButton::Left)));
    assert!(
        testapp::take_requests(commands)
            .iter()
            .all(|request| request.method != "session.open"),
        "a stale second click must not open the removed session"
    );
    assert_ne!(
        app.sessions.active.as_deref(),
        Some("ses_main"),
        "a stale second click must not activate the removed session"
    );
}

#[test]
fn new_session_mouse_click_selects_field_and_places_unicode_cursor() {
    let mut app = testapp::new_session(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let panel =
        crate::ui::panel::layout(screen.panel, crate::ui::panel::PanelSpec::new(0, false, 1));
    let row = panel.content.y + 4;
    let column = panel.content.x + 15;
    app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::empty(),
    })));
    let draft = app.new_session().expect("new-session draft");
    assert_eq!(draft.field, crate::state::selection::NewSessionField::Title);
    assert_eq!(draft.field_cursor, 0);
}

#[test]
fn session_panel_rename_uses_id_and_waits_for_complete_ack() {
    let mut app = testapp::session_selector_all(ThemeKind::Dark);
    app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(2),
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    app.update(AppEvent::Terminal(CrosstermEvent::Paste(
        "新しい".to_owned(),
    )));
    app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    let draft = match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => match &state.mode {
            crate::state::selection::SessionPanelMode::Rename { draft, cursor, .. } => {
                assert_eq!(draft, "新し");
                assert_eq!(*cursor, 2);
                draft.clone()
            }
            mode => panic!("unexpected session mode: {mode:?}"),
        },
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let before = app.sessions.list[0].title.clone();
    let commands = app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    let request = testapp::take_requests(commands)
        .into_iter()
        .find(|request| request.method == "session.rename")
        .expect("session.rename request");
    assert_eq!(request.params["session_id"], "ses_main");
    assert_eq!(request.params["title"], draft);
    assert_eq!(app.sessions.list[0].title, before);
    app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::SessionSelector(state)
            if matches!(
                &state.mode,
                crate::state::selection::SessionPanelMode::Rename {
                    submitting: true,
                    ..
                }
            ) && state.selected_session_id.as_deref() == Some("ses_main")
    ));

    testapp::respond_rpc_error(
        &mut app,
        &request,
        crate::protocol::INVALID_PARAMS,
        "invalid title",
    );
    match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => match &state.mode {
            crate::state::selection::SessionPanelMode::Rename {
                draft, submitting, ..
            } => {
                assert_eq!(draft, "新し");
                assert!(!submitting);
                assert!(
                    state
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("invalid"))
                );
            }
            mode => panic!("rename error left mode: {mode:?}"),
        },
        dock => panic!("unexpected dock after rename error: {dock:?}"),
    }

    let commands = app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    let request = testapp::take_requests(commands)
        .into_iter()
        .find(|request| request.method == "session.rename")
        .expect("retry is explicit, not automatic");
    testapp::respond(
        &mut app,
        &request,
        json!({
            "session": {
                "session_id": "ses_main", "title": "新し", "profile": "coding",
                "workspace": "/work/cli", "model": "deep", "reasoning": "high",
                "loaded": true, "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T08:00:00Z"
            }
        }),
    );
    assert_eq!(
        app.sessions.known["ses_main"].info.title.as_deref(),
        Some("新し")
    );
    assert!(matches!(
        app.dock,
        crate::state::selection::Dock::SessionSelector(
            crate::state::selection::SessionSelectorState {
                mode: crate::state::selection::SessionPanelMode::Browse,
                ..
            }
        )
    ));

    // A refresh issued AFTER the rename is a new catalog authority: the old
    // permanent title override is gone, so the Agent row is applied as-is.
    let commands = app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(5),
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    let refresh = testapp::take_requests(commands)
        .into_iter()
        .find(|request| request.method == "session.list")
        .expect("refresh request after rename");
    testapp::respond(
        &mut app,
        &refresh,
        json!({
            "sessions": [
                {"session_id":"ses_main","title":"stale","profile":"coding","workspace":"/work/cli","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-15T07:55:00Z","updated_at":"2027-01-15T07:55:00Z"}
            ]
        }),
    );
    assert_eq!(
        app.sessions.known["ses_main"].info.title.as_deref(),
        Some("stale"),
        "a list issued after the rename is the newer authority"
    );
}

#[test]
fn session_selector_refresh_preserves_selected_id_after_reorder() {
    let mut app = testapp::session_selector_all(ThemeKind::Dark);
    app.update(AppEvent::MoveSelector { delta: 1 });
    let selected_before = match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => {
            state.selected_session_id.clone().expect("selected session")
        }
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let commands = app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(5),
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    let request = testapp::take_requests(commands)
        .into_iter()
        .find(|request| request.method == "session.list")
        .expect("refresh request");
    testapp::respond(
        &mut app,
        &request,
        json!({
            "sessions": [
                {"session_id":"ses_old","title":"Rust port","profile":"coding","workspace":"/work/rust","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-14T08:00:00Z","updated_at":"2027-01-15T08:10:00Z"},
                {"session_id":"ses_recent","title":"Web app","profile":"review","workspace":"/work/web","model":"fast","reasoning":"medium","loaded":false,"created_at":"2027-01-15T07:45:00Z","updated_at":"2027-01-15T07:45:00Z"},
                {"session_id":"ses_main","title":null,"profile":"coding","workspace":"/work/cli","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-15T07:55:00Z","updated_at":"2027-01-15T07:55:00Z"}
            ]
        }),
    );
    match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => {
            assert_eq!(
                state.selected_session_id.as_deref(),
                Some(selected_before.as_str())
            );
        }
        dock => panic!("unexpected dock after refresh: {dock:?}"),
    }
}

#[test]
fn session_selector_query_refresh_and_footer_actions_keep_filtered_target() {
    let sessions = || {
        vec![
            json!({
                "session_id": "ses_original",
                "title": "Renamed During Work 中文",
                "profile": "coding",
                "workspace": "/work/original",
                "model": "deep",
                "reasoning": "high",
                "loaded": true,
                "created_at": "2027-01-15T07:54:00Z",
                "updated_at": "2027-01-15T07:56:00Z"
            }),
            json!({
                "session_id": "ses_survivor",
                "title": "Native Survivor",
                "profile": "coding",
                "workspace": "/work/survivor",
                "model": "deep",
                "reasoning": "high",
                "loaded": true,
                "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            }),
        ]
    };
    let make_app = || {
        let (models, profiles, _) = testapp::standard_catalog();
        let session_list = sessions();
        let mut app =
            testapp::ready_catalog(ThemeKind::Dark, models, profiles, session_list.clone());
        for session_id in ["ses_original", "ses_survivor"] {
            app.sessions.known.get_mut(session_id).unwrap().state =
                Some(crate::protocol::SessionStateWire {
                    session_id: session_id.to_owned(),
                    status: crate::protocol::SessionStatusWire::Idle,
                    active_loop: None,
                    block_reason: None,
                    compaction: None,
                });
        }
        app.sessions.active = Some("ses_survivor".to_owned());
        testapp::open_session_selector_all(&mut app, session_list);
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        app
    };
    let selected = |app: &App| match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state.selected_session_id.clone(),
        _ => None,
    };
    let key = |code| {
        AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::empty(),
        )))
    };
    let mouse = |column, row, kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    let footer_point = |app: &App, action| {
        let state = match &app.dock {
            crate::state::selection::Dock::SessionSelector(state) => state,
            dock => panic!("unexpected dock: {dock:?}"),
        };
        let screen = layout::screen_layout(app, Rect::new(0, 0, 80, 24));
        let panel = selector::session_panel_layout(screen.panel, state);
        (panel.footer.x..panel.footer.right())
            .flat_map(|column| {
                (panel.footer.y..panel.footer.bottom()).map(move |row| (column, row))
            })
            .find(|&(column, row)| {
                selector::session_action_at(app, screen.panel, state, column, row) == Some(action)
            })
            .expect("session footer action is hit-testable")
    };

    let mut app = make_app();
    for character in "Renamed During Work".chars() {
        app.update(key(KeyCode::Char(character)));
    }
    assert_eq!(
        selected(&app).as_deref(),
        Some("ses_original"),
        "typing a query must move selection into the filtered list"
    );
    let refresh = testapp::take_requests(app.update(key(KeyCode::F(5))))
        .into_iter()
        .find(|request| request.method == "session.list")
        .expect("F5 refresh request");
    testapp::respond(
        &mut app,
        &refresh,
        json!({"sessions": [sessions()[1].clone(), sessions()[0].clone()]}),
    );
    assert_eq!(
        selected(&app).as_deref(),
        Some("ses_original"),
        "refresh reordering must preserve the visible stable ID"
    );

    let (close_column, close_row) =
        footer_point(&app, crate::state::selection::SessionPanelAction::Close);
    app.update(mouse(
        close_column,
        close_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert!(
        testapp::take_requests(app.update(mouse(
            close_column,
            close_row,
            MouseEventKind::Up(MouseButton::Left),
        )))
        .is_empty()
    );
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::SessionSelector(state)
            if matches!(
                &state.mode,
                crate::state::selection::SessionPanelMode::ConfirmClose
            ) && state.selected_session_id.as_deref() == Some("ses_original")
    ));
    let close_screen = text(&draw(&app, 80, 24));
    assert!(close_screen.contains("Renamed During Work"));
    assert!(!close_screen.contains("Native Survivor"));
    let close = testapp::take_requests(app.update(key(KeyCode::Enter)))
        .into_iter()
        .find(|request| request.method == "session.close")
        .expect("close targets the filtered session");
    assert_eq!(close.params["session_id"], "ses_original");
    testapp::respond(&mut app, &close, json!({"ok": true}));

    let (delete_column, delete_row) =
        footer_point(&app, crate::state::selection::SessionPanelAction::Delete);
    app.update(mouse(
        delete_column,
        delete_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert!(
        testapp::take_requests(app.update(mouse(
            delete_column,
            delete_row,
            MouseEventKind::Up(MouseButton::Left),
        )))
        .is_empty()
    );
    let delete_screen = text(&draw(&app, 80, 24));
    assert!(delete_screen.contains("Renamed During Work"));
    assert!(!delete_screen.contains("Native Survivor"));
    app.update(key(KeyCode::Tab));
    let delete = testapp::take_requests(app.update(key(KeyCode::Enter)))
        .into_iter()
        .find(|request| request.method == "session.delete")
        .expect("delete targets the filtered session");
    assert_eq!(delete.params["session_id"], "ses_original");

    let mut no_match = make_app();
    for character in "no matching session".chars() {
        no_match.update(key(KeyCode::Char(character)));
    }
    assert_eq!(
        selected(&no_match),
        None,
        "a query with no matches must clear the stable selection"
    );
    let (close_column, close_row) = footer_point(
        &no_match,
        crate::state::selection::SessionPanelAction::Close,
    );
    no_match.update(mouse(
        close_column,
        close_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert!(
        testapp::take_requests(no_match.update(mouse(
            close_column,
            close_row,
            MouseEventKind::Up(MouseButton::Left),
        )))
        .is_empty(),
        "no selected session must not emit close RPC"
    );
    let (delete_column, delete_row) = footer_point(
        &no_match,
        crate::state::selection::SessionPanelAction::Delete,
    );
    no_match.update(mouse(
        delete_column,
        delete_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert!(
        testapp::take_requests(no_match.update(mouse(
            delete_column,
            delete_row,
            MouseEventKind::Up(MouseButton::Left),
        )))
        .is_empty(),
        "no selected session must not emit delete RPC"
    );
}

#[test]
fn session_rename_dialog_freezes_target_and_reconciles_before_footer_actions() {
    let sessions = || {
        vec![
            json!({
                "session_id": "ses_alpha",
                "title": "Original Alpha",
                "profile": "coding",
                "workspace": "/work/alpha",
                "model": "deep",
                "reasoning": "high",
                "loaded": true,
                "created_at": "2027-01-15T07:54:00Z",
                "updated_at": "2027-01-15T07:56:00Z"
            }),
            json!({
                "session_id": "ses_beta",
                "title": "Original Beta",
                "profile": "coding",
                "workspace": "/work/beta",
                "model": "deep",
                "reasoning": "high",
                "loaded": true,
                "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            }),
        ]
    };
    let mut app = {
        let (models, profiles, _) = testapp::standard_catalog();
        let session_list = sessions();
        let mut app =
            testapp::ready_catalog(ThemeKind::Dark, models, profiles, session_list.clone());
        for session_id in ["ses_alpha", "ses_beta"] {
            app.sessions.known.get_mut(session_id).unwrap().state =
                Some(crate::protocol::SessionStateWire {
                    session_id: session_id.to_owned(),
                    status: crate::protocol::SessionStatusWire::Idle,
                    active_loop: None,
                    block_reason: None,
                    compaction: None,
                });
        }
        testapp::open_session_selector_all(&mut app, session_list);
        app.update(AppEvent::SetSelectorQuery {
            query: "Original".to_owned(),
        });
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        app
    };
    let key =
        |code, modifiers| AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(code, modifiers)));
    let selected = |app: &App| match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state.selected_session_id.clone(),
        dock => panic!("unexpected dock: {dock:?}"),
    };
    assert_eq!(selected(&app).as_deref(), Some("ses_alpha"));

    let refresh = testapp::take_requests(app.update(key(KeyCode::F(5), KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.list")
        .expect("refresh request");
    app.update(key(KeyCode::F(2), KeyModifiers::empty()));
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::SessionSelector(state)
            if matches!(
                &state.mode,
                crate::state::selection::SessionPanelMode::Rename { .. }
            ) && state.selected_session_id.as_deref() == Some("ses_alpha")
    ));
    testapp::respond(
        &mut app,
        &refresh,
        json!({
            "sessions": [
                {"session_id":"ses_alpha","title":"Refresh Renamed","profile":"coding","workspace":"/work/alpha","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-15T07:54:00Z","updated_at":"2027-01-15T08:01:00Z"},
                {"session_id":"ses_beta","title":"Original Beta","profile":"coding","workspace":"/work/beta","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-15T07:55:00Z","updated_at":"2027-01-15T07:55:00Z"}
            ]
        }),
    );
    assert_eq!(
        selected(&app).as_deref(),
        Some("ses_alpha"),
        "a refresh must not retarget the Rename dialog"
    );
    let event = serde_json::from_value(json!({
        "type": "session_opened",
        "data": {
            "session": {
                "session_id": "ses_gamma", "title": "Gamma", "profile": "coding",
                "workspace": "/work/gamma", "model": "deep", "reasoning": "high",
                "loaded": true, "created_at": "2027-01-15T07:57:00Z",
                "updated_at": "2027-01-15T08:03:00Z"
            },
            "meta": {"session_id": "ses_gamma", "loop_id": null, "dropped_before": 0}
        }
    }))
    .expect("session_opened fixture parses");
    let event_requests = testapp::take_requests(app.update(AppEvent::Rpc(RpcEvent::Frame(
        crate::protocol::IncomingFrame::Notification(crate::protocol::RpcNotification::AgentEvent(
            event,
        )),
    ))));
    if let Some(state_request) = event_requests
        .iter()
        .find(|request| request.method == "session.state")
    {
        testapp::respond(
            &mut app,
            state_request,
            json!({
                "session_id": "ses_gamma", "status": "idle",
                "active_loop": null, "block_reason": null
            }),
        );
    }
    assert_eq!(
        selected(&app).as_deref(),
        Some("ses_alpha"),
        "a SessionOpened upsert must not retarget the Rename dialog"
    );

    app.update(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    app.update(AppEvent::Terminal(CrosstermEvent::Paste(
        "Renamed Alpha".to_owned(),
    )));
    let rename = testapp::take_requests(app.update(key(KeyCode::Enter, KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.rename")
        .expect("rename request");
    assert_eq!(rename.params["session_id"], "ses_alpha");
    testapp::respond(
        &mut app,
        &rename,
        json!({
            "session": {
                "session_id": "ses_alpha", "title": "Renamed Alpha", "profile": "coding",
                "workspace": "/work/alpha", "model": "deep", "reasoning": "high",
                "loaded": true, "created_at": "2027-01-15T07:54:00Z",
                "updated_at": "2027-01-15T08:02:00Z"
            }
        }),
    );
    assert_eq!(
        selected(&app).as_deref(),
        Some("ses_beta"),
        "after ACK Browse must select the first remaining filtered session"
    );

    let footer_point = |app: &App, action| {
        let state = match &app.dock {
            crate::state::selection::Dock::SessionSelector(state) => state,
            dock => panic!("unexpected dock: {dock:?}"),
        };
        let screen = layout::screen_layout(app, Rect::new(0, 0, 80, 24));
        let panel = selector::session_panel_layout(screen.panel, state);
        (panel.footer.x..panel.footer.right())
            .flat_map(|column| {
                (panel.footer.y..panel.footer.bottom()).map(move |row| (column, row))
            })
            .find(|&(column, row)| {
                selector::session_action_at(app, screen.panel, state, column, row) == Some(action)
            })
            .expect("session footer action is hit-testable")
    };
    let mouse = |column, row, kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };

    let (close_column, close_row) =
        footer_point(&app, crate::state::selection::SessionPanelAction::Close);
    app.update(mouse(
        close_column,
        close_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert!(
        testapp::take_requests(app.update(mouse(
            close_column,
            close_row,
            MouseEventKind::Up(MouseButton::Left),
        )))
        .is_empty()
    );
    assert!(matches!(
        &app.dock,
        crate::state::selection::Dock::SessionSelector(state)
            if matches!(
                &state.mode,
                crate::state::selection::SessionPanelMode::ConfirmClose
            ) && state.selected_session_id.as_deref() == Some("ses_beta")
    ));
    let close = testapp::take_requests(app.update(key(KeyCode::Enter, KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.close")
        .expect("footer Close request");
    assert_eq!(close.params["session_id"], "ses_beta");
    testapp::respond(&mut app, &close, json!({"ok": true}));

    let (delete_column, delete_row) =
        footer_point(&app, crate::state::selection::SessionPanelAction::Delete);
    app.update(mouse(
        delete_column,
        delete_row,
        MouseEventKind::Down(MouseButton::Left),
    ));
    assert!(
        testapp::take_requests(app.update(mouse(
            delete_column,
            delete_row,
            MouseEventKind::Up(MouseButton::Left),
        )))
        .is_empty()
    );
    app.update(key(KeyCode::Tab, KeyModifiers::empty()));
    let delete = testapp::take_requests(app.update(key(KeyCode::Enter, KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.delete")
        .expect("footer Delete request");
    assert_eq!(delete.params["session_id"], "ses_beta");
}

#[test]
fn session_rename_ack_clears_hidden_target_when_query_has_no_matches() {
    let (models, profiles, _) = testapp::standard_catalog();
    let sessions = vec![
        json!({
            "session_id": "ses_alpha",
            "title": "Original Alpha",
            "profile": "coding",
            "workspace": "/work/alpha",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "created_at": "2027-01-15T07:54:00Z",
            "updated_at": "2027-01-15T07:56:00Z"
        }),
        json!({
            "session_id": "ses_beta",
            "title": "Other Beta",
            "profile": "coding",
            "workspace": "/work/beta",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "created_at": "2027-01-15T07:55:00Z",
            "updated_at": "2027-01-15T07:55:00Z"
        }),
    ];
    let mut app = testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions.clone());
    for session_id in ["ses_alpha", "ses_beta"] {
        app.sessions.known.get_mut(session_id).unwrap().state =
            Some(crate::protocol::SessionStateWire {
                session_id: session_id.to_owned(),
                status: crate::protocol::SessionStatusWire::Idle,
                active_loop: None,
                block_reason: None,
                compaction: None,
            });
    }
    testapp::open_session_selector_all(&mut app, sessions);
    app.update(AppEvent::SetSelectorQuery {
        query: "Original".to_owned(),
    });
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let key =
        |code, modifiers| AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(code, modifiers)));
    let selected = |app: &App| match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state.selected_session_id.clone(),
        dock => panic!("unexpected dock: {dock:?}"),
    };
    assert_eq!(selected(&app).as_deref(), Some("ses_alpha"));
    app.update(key(KeyCode::F(2), KeyModifiers::empty()));
    app.update(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    app.update(AppEvent::Terminal(CrosstermEvent::Paste(
        "Renamed Alpha".to_owned(),
    )));
    let rename = testapp::take_requests(app.update(key(KeyCode::Enter, KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.rename")
        .expect("rename request");
    assert_eq!(rename.params["session_id"], "ses_alpha");
    testapp::respond(
        &mut app,
        &rename,
        json!({
            "session": {
                "session_id": "ses_alpha", "title": "Renamed Alpha", "profile": "coding",
                "workspace": "/work/alpha", "model": "deep", "reasoning": "high",
                "loaded": true, "created_at": "2027-01-15T07:54:00Z",
                "updated_at": "2027-01-15T08:02:00Z"
            }
        }),
    );
    assert_eq!(
        selected(&app),
        None,
        "after ACK Browse must clear a selection with no filtered matches"
    );

    let mouse = |column, row, kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    let footer_point = |app: &App, action| {
        let state = match &app.dock {
            crate::state::selection::Dock::SessionSelector(state) => state,
            dock => panic!("unexpected dock: {dock:?}"),
        };
        let screen = layout::screen_layout(app, Rect::new(0, 0, 80, 24));
        let panel = selector::session_panel_layout(screen.panel, state);
        (panel.footer.x..panel.footer.right())
            .flat_map(|column| {
                (panel.footer.y..panel.footer.bottom()).map(move |row| (column, row))
            })
            .find(|&(column, row)| {
                selector::session_action_at(app, screen.panel, state, column, row) == Some(action)
            })
            .expect("session footer action is hit-testable")
    };
    for action in [
        crate::state::selection::SessionPanelAction::Close,
        crate::state::selection::SessionPanelAction::Delete,
    ] {
        let (column, row) = footer_point(&app, action);
        app.update(mouse(column, row, MouseEventKind::Down(MouseButton::Left)));
        assert!(
            testapp::take_requests(app.update(mouse(
                column,
                row,
                MouseEventKind::Up(MouseButton::Left),
            )))
            .is_empty(),
            "footer action with no selected session must not emit an RPC"
        );
    }
}

#[test]
fn session_panel_cancel_reconciles_after_frozen_target_updates() {
    let sessions = |alpha_loaded| {
        vec![
            json!({
                "session_id": "ses_alpha",
                "title": "Original Alpha",
                "profile": "coding",
                "workspace": "/work/alpha",
                "model": "deep",
                "reasoning": "high",
                "loaded": alpha_loaded,
                "created_at": "2027-01-15T07:54:00Z",
                "updated_at": "2027-01-15T07:56:00Z"
            }),
            json!({
                "session_id": "ses_beta",
                "title": "Original Beta",
                "profile": "coding",
                "workspace": "/work/beta",
                "model": "deep",
                "reasoning": "high",
                "loaded": false,
                "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            }),
        ]
    };
    let setup = |alpha_loaded| {
        let (models, profiles, _) = testapp::standard_catalog();
        let session_list = sessions(alpha_loaded);
        let mut app =
            testapp::ready_catalog(ThemeKind::Dark, models, profiles, session_list.clone());
        for session_id in ["ses_alpha", "ses_beta"] {
            app.sessions.known.get_mut(session_id).unwrap().state =
                Some(crate::protocol::SessionStateWire {
                    session_id: session_id.to_owned(),
                    status: crate::protocol::SessionStatusWire::Idle,
                    active_loop: None,
                    block_reason: None,
                    compaction: None,
                });
        }
        testapp::open_session_selector_all(&mut app, session_list);
        app.update(AppEvent::SetSelectorQuery {
            query: "Original".to_owned(),
        });
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        app
    };
    let key =
        |code, modifiers| AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(code, modifiers)));
    let selected = |app: &App| match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => state.selected_session_id.clone(),
        dock => panic!("unexpected dock: {dock:?}"),
    };
    let mouse_event = |column, row, kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    let cancel = |app: &mut App, mouse_cancel: bool| {
        if !mouse_cancel {
            app.update(key(KeyCode::Enter, KeyModifiers::empty()));
            return;
        }
        let state = match &app.dock {
            crate::state::selection::Dock::SessionSelector(state) => state,
            dock => panic!("unexpected dock: {dock:?}"),
        };
        let screen = layout::screen_layout(app, Rect::new(0, 0, 80, 24));
        let panel = selector::session_panel_layout(screen.panel, state);
        let (column, row) = (panel.content.x..panel.content.right())
            .flat_map(|column| {
                (panel.content.y..panel.content.bottom()).map(move |row| (column, row))
            })
            .find(|&(column, row)| {
                selector::session_action_at(app, screen.panel, state, column, row)
                    == Some(crate::state::selection::SessionPanelAction::Cancel)
            })
            .expect("ConfirmDelete Cancel button is hit-testable");
        app.update(mouse_event(
            column,
            row,
            MouseEventKind::Down(MouseButton::Left),
        ));
        app.update(mouse_event(
            column,
            row,
            MouseEventKind::Up(MouseButton::Left),
        ));
    };

    let mut rename = setup(true);
    let refresh = testapp::take_requests(rename.update(key(KeyCode::F(5), KeyModifiers::empty())))
        .into_iter()
        .find(|request| request.method == "session.list")
        .expect("refresh request");
    rename.update(key(KeyCode::F(2), KeyModifiers::empty()));
    testapp::respond(
        &mut rename,
        &refresh,
        json!({
            "sessions": [
                {"session_id":"ses_alpha","title":"Renamed During Edit","profile":"coding","workspace":"/work/alpha","model":"deep","reasoning":"high","loaded":true,"created_at":"2027-01-15T07:54:00Z","updated_at":"2027-01-15T08:01:00Z"},
                {"session_id":"ses_beta","title":"Original Beta","profile":"coding","workspace":"/work/beta","model":"deep","reasoning":"high","loaded":false,"created_at":"2027-01-15T07:55:00Z","updated_at":"2027-01-15T07:55:00Z"}
            ]
        }),
    );
    assert_eq!(
        selected(&rename).as_deref(),
        Some("ses_alpha"),
        "refresh must not change the Rename target"
    );
    rename.update(key(KeyCode::Esc, KeyModifiers::empty()));
    assert_eq!(
        selected(&rename).as_deref(),
        Some("ses_beta"),
        "Esc must reconcile Browse selection after Rename cancellation"
    );

    for mouse_cancel in [false, true] {
        let mut app = setup(false);
        app.update(key(KeyCode::Delete, KeyModifiers::empty()));
        assert!(matches!(
            &app.dock,
            crate::state::selection::Dock::SessionSelector(state)
                if matches!(
                    &state.mode,
                    crate::state::selection::SessionPanelMode::ConfirmDelete {
                        choice: crate::state::selection::SessionConfirmChoice::Cancel,
                        ..
                    }
                ) && state.selected_session_id.as_deref() == Some("ses_alpha")
        ));
        let open = testapp::take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_alpha".to_owned(),
        }))
        .into_iter()
        .find(|request| request.method == "session.open")
        .expect("async session update request");
        testapp::respond(
            &mut app,
            &open,
            json!({
                "session": {
                    "session_id": "ses_alpha", "title": "Renamed During Confirm",
                    "profile": "coding", "workspace": "/work/alpha", "model": "deep",
                    "reasoning": "high", "loaded": true,
                    "created_at": "2027-01-15T07:54:00Z",
                    "updated_at": "2027-01-15T08:02:00Z"
                }
            }),
        );
        assert_eq!(
            selected(&app).as_deref(),
            Some("ses_alpha"),
            "confirmation target must remain frozen during session sync"
        );
        cancel(&mut app, mouse_cancel);
        assert_eq!(
            selected(&app).as_deref(),
            Some("ses_beta"),
            "cancel must reconcile to the remaining filtered session"
        );
        assert!(matches!(
            &app.dock,
            crate::state::selection::Dock::SessionSelector(state)
                if matches!(&state.mode, crate::state::selection::SessionPanelMode::Browse)
        ));
    }
}

#[test]
fn session_delete_requires_close_then_second_confirmation_and_tombstones_id() {
    let (models, profiles, sessions) = testapp::standard_catalog();
    let mut app = testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions.clone());
    testapp::open_session_selector_all(&mut app, sessions);
    let press = |code| {
        AppEvent::Terminal(CrosstermEvent::Key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::empty(),
        )))
    };
    let state_request =
        testapp::take_requests(app.update(press(crossterm::event::KeyCode::Delete)))
            .into_iter()
            .find(|request| request.method == "session.state")
            .expect("unknown loaded state is reread before delete");
    assert!(matches!(
        app.dock,
        crate::state::selection::Dock::SessionSelector(
            crate::state::selection::SessionSelectorState {
                mode: crate::state::selection::SessionPanelMode::Browse,
                ..
            }
        )
    ));
    testapp::respond(
        &mut app,
        &state_request,
        json!({
            "session_id": "ses_main",
            "status": "idle",
            "active_loop": null,
            "block_reason": null
        }),
    );
    app.update(press(crossterm::event::KeyCode::Delete));
    assert!(matches!(
        app.dock,
        crate::state::selection::Dock::SessionSelector(
            crate::state::selection::SessionSelectorState {
                mode: crate::state::selection::SessionPanelMode::ConfirmCloseForDelete,
                ..
            }
        )
    ));
    let close = testapp::take_requests(app.update(press(crossterm::event::KeyCode::Enter)))
        .into_iter()
        .find(|request| request.method == "session.close")
        .expect("close before delete");
    testapp::respond(&mut app, &close, json!({"ok": true}));
    assert!(matches!(
        app.dock,
        crate::state::selection::Dock::SessionSelector(
            crate::state::selection::SessionSelectorState {
                mode: crate::state::selection::SessionPanelMode::ConfirmDelete {
                    choice: crate::state::selection::SessionConfirmChoice::Cancel,
                    submitting: false,
                },
                ..
            }
        )
    ));
    let cancelled = testapp::take_requests(app.update(press(crossterm::event::KeyCode::Enter)));
    assert!(
        cancelled
            .iter()
            .all(|request| request.method != "session.delete")
    );
    assert!(matches!(
        app.dock,
        crate::state::selection::Dock::SessionSelector(
            crate::state::selection::SessionSelectorState {
                mode: crate::state::selection::SessionPanelMode::Browse,
                ..
            }
        )
    ));
    app.update(press(crossterm::event::KeyCode::Delete));
    assert!(matches!(
        app.dock,
        crate::state::selection::Dock::SessionSelector(
            crate::state::selection::SessionSelectorState {
                mode: crate::state::selection::SessionPanelMode::ConfirmDelete {
                    choice: crate::state::selection::SessionConfirmChoice::Cancel,
                    submitting: false,
                },
                ..
            }
        )
    ));
    app.update(press(crossterm::event::KeyCode::Tab));
    let delete = testapp::take_requests(app.update(press(crossterm::event::KeyCode::Enter)))
        .into_iter()
        .find(|request| request.method == "session.delete")
        .expect("dedicated delete confirmation");
    assert_eq!(delete.params["session_id"], "ses_main");
    testapp::respond(&mut app, &delete, json!({"ok": true}));
    assert!(!app.sessions.list.iter().any(|s| s.session_id == "ses_main"));
    assert!(
        !app.sessions.known.contains_key("ses_main")
            && !app
                .sessions
                .list
                .iter()
                .any(|session| session.session_id == "ses_main"),
        "a deleted session is absent from the catalog"
    );
}

#[test]
fn new_session_form_shows_all_fields_and_the_active_field_background() {
    let app = testapp::new_session(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("New session"));
    assert!(content.contains("workspace"));
    assert!(content.contains("/project"));
    assert!(content.contains("profile"));
    assert!(content.contains("coding"));
    assert!(content.contains("model"));
    assert!(content.contains("deep"));
    assert!(content.contains("reasoning"));
    assert!(content.contains("high"));
    assert!(content.contains("title"));
    assert!(content.contains("Create session"));
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == Theme::dark().selected_bg),
        "the active field row is highlighted"
    );
}

#[test]
fn empty_selector_search_shows_no_matching_items() {
    let app = testapp::empty_model_search(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("No matching items"));
}

#[test]
fn short_terminal_renders_an_8_row_selector_panel() {
    let app = testapp::narrow_selector(ThemeKind::Dark);
    let terminal = draw(&app, 60, 16);
    let content = text(&terminal);
    assert!(content.contains("Select model"));
    assert!(
        content.contains("Select model"),
        "the selector remains visible"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == Theme::dark().selected_bg),
        "the moved selection is highlighted"
    );
}

#[test]
fn selectors_render_on_both_themes_without_panicking() {
    let fixtures: [fn(ThemeKind) -> App; 5] = [
        testapp::new_session,
        testapp::model_selector,
        testapp::reasoning_selector,
        testapp::session_selector,
        testapp::profile_selector,
    ];
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        for fixture in fixtures {
            let _ = draw(&fixture(kind), 120, 40);
        }
    }
}

// ---- Phase 5: input rendering -------------------------------------------

/// Every rendered page of the Help panel, concatenated. The command table is
/// long enough that a single top/bottom pair no longer covers it, so the test
/// pages to the end instead of assuming a fixed height.
fn accumulated_help_text(app: &mut App, width: u16, height: u16) -> String {
    let mut all = text(&draw(app, width, height));
    for _ in 0..80 {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::PageDown,
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
        let page = text(&draw(app, width, height));
        if all.ends_with(&page) {
            break;
        }
        all.push_str(&page);
    }
    all
}

#[test]
fn help_panel_lists_keys_and_safety_notes() {
    let app = testapp::help(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Help"));
    assert!(content.contains("Ctrl+R"));
    assert!(any_cell_matching(&terminal, |cell| cell.symbol() == "┌"
        && cell.fg == Theme::dark().border_accent));

    // A wide terminal shows the single command table without wrapping; every
    // page is accumulated because the table grew with the D2 commands.
    let mut wide = testapp::help(ThemeKind::Dark);
    let wide_content = accumulated_help_text(&mut wide, 140, 50);
    assert!(wide_content.contains("Slash commands"));
    assert!(wide_content.contains("/cancel"));
    assert!(wide_content.contains("/reload"));
    assert!(wide_content.contains("/refresh"));
    assert!(wide_content.contains("Tools run automatically."));
    assert!(wide_content.contains("Bash is not sandboxed."));
    assert!(wide_content.contains("No approval UI"));
}

/// D1d (spec §10.5): the help panel renders the same static command table the
/// parser uses, so help can never advertise an unimplemented command.
#[test]
fn help_panel_lists_every_table_command() {
    let mut app = testapp::help(ThemeKind::Dark);
    let content = accumulated_help_text(&mut app, 120, 60);
    for spec in crate::command::COMMANDS {
        assert!(
            content.contains(spec.name),
            "help is missing the {} entry",
            spec.name
        );
    }
    assert!(content.contains("/refresh"));
    assert!(content.contains("/rename"));
}

#[test]
fn logs_panel_shows_bounded_agent_stderr_without_raw_frames() {
    let app = testapp::logs(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Agent logs"));
    assert!(content.contains("agent stderr: 37 bytes"));
    assert!(content.contains("Recent stderr activity"));
    assert!(!content.contains("jsonrpc"), "no raw RPC frames in logs");
}

#[test]
fn help_and_logs_body_paging_does_not_scroll_the_transcript() {
    let key = |code| {
        AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::empty(),
        )))
    };
    let transcript_signature = |app: &App| {
        let view = app.active_view().expect("active transcript");
        (
            view.scroll.offset,
            view.scroll.follow_tail,
            view.scroll.new_content,
            (view.transcript.window.len(), view.transcript.window.total()),
            view.transcript.blocks.clone(),
        )
    };

    let mut help = testapp::chat(ThemeKind::Dark);
    let before = transcript_signature(&help);
    help.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::F(1),
        KeyModifiers::empty(),
    ))));
    help.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    help.update(key(KeyCode::PageDown));
    assert!(help.panel_scroll > 0, "Help PageDown must scroll its body");
    help.update(key(KeyCode::Home));
    assert_eq!(help.panel_scroll, 0);
    help.update(key(KeyCode::End));
    assert!(help.panel_scroll > 0, "Help End must reach the body tail");
    assert_eq!(transcript_signature(&help), before);

    let mut logs = testapp::chat(ThemeKind::Dark);
    for index in 0..32 {
        logs.update(AppEvent::Rpc(RpcEvent::AgentStderr {
            bytes: format!("agent line {index:02}").len(),
            dropped: 0,
        }));
    }
    for character in "/logs".chars() {
        logs.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::empty(),
        ))));
    }
    logs.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
    let before = transcript_signature(&logs);
    logs.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    logs.update(key(KeyCode::PageDown));
    assert!(logs.panel_scroll > 0, "Logs PageDown must scroll its body");
    logs.update(key(KeyCode::Home));
    assert_eq!(logs.panel_scroll, 0);
    logs.update(key(KeyCode::End));
    assert!(logs.panel_scroll > 0, "Logs End must reach the body tail");
    assert_eq!(transcript_signature(&logs), before);
}

#[test]
fn short_panels_page_down_moves_help_logs_and_session_selector() {
    let key = |code| {
        AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::empty(),
        )))
    };

    let mut help = testapp::help(ThemeKind::Dark);
    help.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    help.update(key(KeyCode::PageDown));
    assert!(help.panel_scroll > 0);

    let mut logs = testapp::logs(ThemeKind::Dark);
    for index in 0..32 {
        logs.update(AppEvent::Rpc(RpcEvent::AgentStderr {
            bytes: format!("agent line {index:02}").len(),
            dropped: 0,
        }));
    }
    logs.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    logs.update(key(KeyCode::PageDown));
    assert!(logs.panel_scroll > 0);

    let (models, profiles, sessions) = testapp::standard_catalog();
    let mut many_sessions = sessions.clone();
    for index in 0..12 {
        many_sessions.push(json!({
            "session_id": format!("ses_extra_{index}"),
            "title": format!("Extra {index}"),
            "profile": "coding",
            "workspace": "/work/extra",
            "model": "deep",
            "reasoning": "high",
            "loaded": false,
            "created_at": "2027-01-01T00:00:00Z",
            "updated_at": format!("2027-01-01T00:{index:02}:00Z")
        }));
    }
    let mut selector =
        testapp::ready_catalog(ThemeKind::Dark, models, profiles, many_sessions.clone());
    testapp::open_session_selector_all(&mut selector, many_sessions);
    selector.update(AppEvent::TerminalSize {
        width: 60,
        height: 16,
    });
    let before = match &selector.dock {
        crate::state::selection::Dock::SessionSelector(state) => state.selected_session_id.clone(),
        dock => panic!("unexpected dock: {dock:?}"),
    };
    selector.update(key(KeyCode::PageDown));
    let after = match &selector.dock {
        crate::state::selection::Dock::SessionSelector(state) => state.selected_session_id.clone(),
        dock => panic!("unexpected dock: {dock:?}"),
    };
    assert_ne!(after, before, "session PageDown must move at 60x16");
}

#[test]
fn multiline_composer_grows_the_panel_and_wraps_cjk() {
    let app = testapp::multiline_composer(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("line one"));
    assert!(content.contains("line two"));
    assert!(
        content.contains("你"),
        "CJK renders (2 columns per char in the buffer)"
    );
    assert!(content.contains("line six"));
    // Rail has no border rows; the content is capped by the 32% responsive
    // editor height while remaining above the four-row minimum.
    let height = crate::ui::layout::composer_height_phase5(&app, 80, 24, false);
    assert_eq!(height, 6, "the composer follows the shared editor geometry");
}

#[test]
fn selector_search_query_is_visible() {
    let app = testapp::search_query(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("> fast"));
}

#[test]
fn new_output_marker_renders_when_scrolled_away() {
    let app = testapp::new_output_marker(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("↓ new output"));
}

#[test]
fn new_output_marker_overlays_without_reducing_viewport() {
    let mut app = testapp::scrolled(ThemeKind::Dark);
    let all = transcript::all_lines(&Theme::dark(), &app, 80);
    let total = all.len();
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: 16,
    });
    let terminal = draw(&app, 80, 24);
    let rows = buffer_lines(&terminal);
    let marker_row = rows
        .iter()
        .position(|row| row.contains("↓ new output"))
        .expect("scrolled transcript shows the new-output marker");
    assert!(rows[marker_row].contains("↓ new output"));
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let position = transcript::scroll_position(
        &app,
        prepared.total_rows(),
        screen.transcript.height as usize,
    );
    let mut underlay = Terminal::new(TestBackend::new(80, 24)).unwrap();
    underlay
        .draw(|frame| {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(
                    prepared
                        .lines()
                        .iter()
                        .skip(position.offset)
                        .take(position.visible_rows)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                screen.transcript,
            )
        })
        .unwrap();
    let overlay = transcript::marker_area(screen.transcript, "↓ new output", false);
    for column in screen.transcript.x..screen.transcript.right() {
        if !overlay.contains((column, marker_row as u16).into()) {
            assert_eq!(
                terminal
                    .backend()
                    .buffer()
                    .cell((column, marker_row as u16))
                    .unwrap()
                    .symbol(),
                underlay
                    .backend()
                    .buffer()
                    .cell((column, marker_row as u16))
                    .unwrap()
                    .symbol()
            );
        }
    }

    assert_eq!(transcript::total_lines(&app, 80), total);
    assert_eq!(
        transcript::visible_rows(&app, total, 17),
        17,
        "the marker must not change scroll geometry"
    );
    assert!(
        all.iter()
            .any(|line| line_text(line).contains("quoted wisdom")),
        "the covered transcript body remains available to scrolling"
    );
    let marker_area = transcript::marker_area(
        Rect::new(1, marker_row as u16, 79, 1),
        "↓ new output",
        false,
    );
    let marker_cells = &terminal.backend().buffer().content()
        [marker_row * 80 + marker_area.x as usize..marker_row * 80 + marker_area.right() as usize];
    assert!(
        marker_cells
            .iter()
            .all(|cell| cell.bg == Theme::dark().page_bg),
        "only the indicator cells replace the transcript background"
    );

    for _ in 0..3 {
        app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
    }
    assert!(
        buffer_lines(&draw(&app, 80, 24))
            .iter()
            .any(|row| row.contains("wisdom")),
        "scrolling down exposes the transcript row below the marker window"
    );
}

#[test]
fn end_resumes_follow_with_marker_overlay() {
    let mut app = testapp::scrolled(ThemeKind::Dark);
    let total = transcript::total_lines(&app, 80);
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: 16,
    });
    assert!(!app.active_view().unwrap().scroll.follow_tail);

    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::End,
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )));
    assert!(app.active_view().unwrap().scroll.follow_tail);
    assert_eq!(transcript::visible_rows(&app, total, 17), 17);
    assert!(!text(&draw(&app, 80, 24)).contains("↓ new output"));
}

#[test]
fn empty_user_sections_do_not_add_spacer_rows() {
    let theme = Theme::dark();
    let make = |text: &str| UserBlock {
        index: None,
        loop_id: None,
        kind: crate::protocol::UserMessageKindWire::Prompt,
        text: text.to_owned(),
        pending: true,
    };
    for body in ["", " \n\t"] {
        assert!(
            user::lines(&theme, &make(body), 40).is_empty(),
            "empty user body {body:?} is an empty section"
        );
    }

    let normal = user::lines(&theme, &make("pending message"), 40);
    assert_section_is_vertically_padded(&normal, "non-empty pending user");
}
