//! Native transcript boundaries: rendered cards, unread arrivals, and inline reads.
use super::*;
use crate::protocol::ToolRefWire;
use crate::protocol::read::SnapshotPin;
use crate::state::view::{SectionKind, SectionView};
use crate::ui::{testapp, transcript};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use serde_json::{Value, json};

fn screen(app: &App) -> crate::ui::layout::ScreenLayout {
    crate::ui::layout::screen_layout(
        app,
        Rect::new(0, 0, app.terminal_size.0, app.terminal_size.1),
    )
}

fn prepare(app: &mut App) -> Vec<AppCommand> {
    let screen = screen(app);
    let prepared = transcript::prepare_conversation(app, screen.content.width);
    let mut commands = app.update(AppEvent::ConversationPrepared(prepared));
    let total = app
        .prepared_conversation(screen.content.width)
        .unwrap()
        .total_rows();
    // main.rs publishes content-visible rows, not the raw transcript height.
    let visible = transcript::visible_rows(app, total, screen.transcript.height);
    commands.extend(app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: visible,
    }));
    commands
}

fn control_key(app: &mut App, code: KeyCode) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::CONTROL,
    ))))
}

fn notify(app: &mut App, value: Value) -> Vec<AppCommand> {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(serde_json::from_value(value).unwrap()),
    ))))
}

fn install_resident_items(app: &mut App, total: usize, entries: Vec<(usize, Value)>) {
    let view = app.active_session_mut().unwrap();
    view.transcript.window.install_pin(SnapshotPin {
        projection: None,
        captured_end: 90_000,
        history_revision: "a".repeat(64),
        total,
    });
    for (index, envelope) in entries {
        let data = envelope.to_string();
        let item = crate::protocol::read::decode_item(&data).unwrap();
        let owner = install_history_item(view, index, &item).unwrap();
        view.transcript
            .window
            .insert_owner(index, owner, 0, data.len());
    }
    view.transcript.sync_from_window();
}

fn tool_section(prepared: &PreparedConversation, key: &ToolKey) -> SectionView {
    prepared
        .sections
        .iter()
        .find(|section| {
            section.id.kind == SectionKind::Tool
                && section.id.session_id.as_ref() == key.session_id.as_str()
                && section.id.loop_id.as_deref() == Some(key.loop_id.as_str())
                && section.id.request_index == Some(key.request_index)
                && section.id.tool_call_id.as_deref() == Some(key.tool_call_id.as_str())
        })
        .expect("the renderer must expose the tool card")
}

fn folded(app: &App, key: &ToolKey) -> bool {
    let prepared = transcript::prepare_conversation(app, screen(app).content.width);
    tool_section(&prepared, key).folded
}

fn rendered_row(app: &App, row: u16) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(app.terminal_size.0, app.terminal_size.1)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .unwrap();
    let area = screen(app).content;
    (area.x..area.right())
        .map(|column| terminal.backend().buffer()[(column, row)].symbol())
        .collect::<String>()
}

fn historical_call() -> (App, ToolKey) {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let key = ToolKey::new("ses_1", "historical", 0, "write-call");
    // Item 1 is the matching result outside the resident window. The later
    // prompt makes this a historical card, not a currently executing tool.
    install_resident_items(
        &mut app,
        3,
        vec![
            (
                0,
                json!({"item":{"type":"assistant","data":{
            "loop_id":key.loop_id,"request_index":0,"model":"deep",
            "finish_reason":"tool_calls","content":[{"type":"tool_call","data":{
                "tool_call_id":key.tool_call_id,"name":"write","call_index":0,
                "arguments":{"path":"historical.rs","content":"saved input"}}}]}}}),
            ),
            (
                2,
                testapp::user_entry(2, "later", &"Later prompt paragraph.\n\n".repeat(40)),
            ),
        ],
    );
    let view = app.active_view().unwrap();
    assert!(view.live.is_none());
    assert!(view.transcript.window.item(1).is_none());
    assert!(
        view.transcript
            .blocks
            .iter()
            .all(|block| { !matches!(block.as_ref(), TranscriptBlock::Tool(_)) })
    );
    prepare(&mut app);
    control_key(&mut app, KeyCode::Home);
    prepare(&mut app);
    let prepared = app
        .prepared_conversation(screen(&app).content.width)
        .unwrap();
    let section = tool_section(prepared, &key);
    assert_eq!(section.id.history_index, Some(0));
    assert!(section.collapsible && section.folded);
    assert_eq!(app.viewport.1, screen(&app).transcript.height as usize - 1);
    (app, key)
}

#[test]
fn historical_assistant_only_card_click_expands_rendered_section() {
    let (mut app, tool_key) = historical_call();
    let screen = screen(&app);
    let prepared = app.prepared_conversation(screen.content.width).unwrap();
    let section = tool_section(prepared, &tool_key);
    let target = prepared
        .window(section.rows.start, section.rows.len())
        .iter()
        .position(|line| line.to_string().contains("historical.rs"))
        .expect("the historical target must be rendered");
    let position = transcript::scroll_position(
        &app,
        prepared.total_rows(),
        screen.transcript.height as usize,
    );
    let logical_row = section.rows.start + target;
    assert!((position.offset..position.offset + position.visible_rows).contains(&logical_row));
    let row = screen.transcript.y + (logical_row - position.offset) as u16;
    assert!(rendered_row(&app, row).contains("historical.rs"));
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column: screen.content.x + section.content_columns.start as u16,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }
    assert!(
        !folded(&app, &tool_key),
        "a rendered historical call must respond to a click"
    );
}

#[test]
fn historical_assistant_only_card_ctrl_o_expands_then_collapses() {
    let (mut app, tool_key) = historical_call();
    control_key(&mut app, KeyCode::Char('o'));
    assert!(
        !folded(&app, &tool_key),
        "first Ctrl+O must expand the card"
    );
    control_key(&mut app, KeyCode::Char('o'));
    assert!(
        folded(&app, &tool_key),
        "second Ctrl+O must collapse the same card"
    );
}

fn deferred_write(key: &ToolKey) -> Value {
    json!({"display":true,"item":{"type":"tool_result","data":{
        "loop_id":key.loop_id,"request_index":key.request_index,
        "call_id":key.tool_call_id,"tool_name":"write","outcome":"success"}},
        "tool_summaries":[{"tool_ref":ToolRefWire::from(key),
            "tool_call_id":key.tool_call_id,"name":"write",
            "display":{"detail":"a.rs","input_line_count":2},
            "output_line_count":1,"output_truncated":false,"count_state":"exact"}]})
}

#[test]
fn inline_read_includes_last_visible_card_above_marker_but_not_offscreen_card() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let visible_key = ToolKey::new("ses_1", "historical", 0, "visible-write");
    let offscreen_key = ToolKey::new("ses_1", "later", 0, "offscreen-write");
    install_resident_items(
        &mut app,
        5,
        vec![
            (
                0,
                testapp::user_entry(0, "historical", &"Earlier paragraph.\n\n".repeat(40)),
            ),
            (
                1,
                json!({"item":{"type":"tool_result","data":{
            "loop_id":"historical","request_index":0,"call_id":"read-before",
            "tool_name":"read","outcome":"success","output":{"content":"done"}}}}),
            ),
            (2, deferred_write(&visible_key)),
            (
                3,
                testapp::user_entry(3, "later", &"Later paragraph.\n\n".repeat(40)),
            ),
            (4, deferred_write(&offscreen_key)),
        ],
    );
    let screen = screen(&app);
    let prepared = transcript::prepare_conversation(&app, screen.content.width);
    let total = prepared.total_rows();
    let section = tool_section(&prepared, &visible_key);
    let offscreen = tool_section(&prepared, &offscreen_key);
    assert!(section.folded && offscreen.folded);
    for key in [&visible_key, &offscreen_key] {
        let facts = &app.active_view().unwrap().tool_presentations[key];
        assert!(facts.body_deferred && facts.input_text().is_none());
        assert!(facts.inline.is_none());
    }
    // Consecutive tool cards share blank padding, so the first visible row
    // here is the write title itself, not an invisible leading spacer.
    assert!(
        prepared.window(section.rows.start, 1)[0]
            .to_string()
            .contains("write")
    );
    app.install_conversation(prepared);
    let content_height = screen.transcript.height as usize - 1;
    app.set_transcript_offset(
        section.rows.start + 1 - content_height,
        total,
        screen.transcript.height as usize,
    );
    let position = transcript::scroll_position(&app, total, screen.transcript.height as usize);
    assert!(position.marker);
    assert_eq!(position.visible_rows, content_height);
    assert_eq!(
        section.rows.start,
        position.offset + position.visible_rows - 1
    );
    assert!(offscreen.rows.start >= position.offset + position.visible_rows);
    let last_content_row = screen.transcript.y + position.visible_rows as u16 - 1;
    assert!(rendered_row(&app, last_content_row).contains("write"));
    assert!(rendered_row(&app, screen.transcript.bottom() - 1).contains("scroll position"));
    // Publish exactly the measurement main.rs sends before polling, rather
    // than compensating for the bug by storing the raw height in the fixture.
    let commands = app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: transcript::visible_rows(&app, total, screen.transcript.height),
    });
    assert_eq!(app.viewport.1, content_height);
    let requests = testapp::take_requests(commands)
        .into_iter()
        .filter(|request| matches!(request.method, "tool.read" | "tool.output"))
        .collect::<Vec<_>>();
    assert_eq!(
        requests.len(),
        1,
        "the last rendered write must recover its deferred input"
    );
    assert_eq!(requests[0].method, "tool.read");
    let requested: ToolRefWire = serde_json::from_value(requests[0].params.clone()).unwrap();
    assert_eq!(requested, ToolRefWire::from(&visible_key));
    assert_eq!(requests[0].params["display"], true);
}

#[test]
fn preview_only_growth_while_scrolled_marks_new_output_without_row_growth() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let tool_key = ToolKey::new("ses_1", "loop_live", 0, "write-preview");
    notify(
        &mut app,
        json!({"type":"output_delta","data":{
        "turn":{"session_id":"ses_1","loop_id":"loop_live"},"request_index":0,
        "channel":"text","delta":"Earlier output paragraph.\n\n".repeat(40),
        "meta":{"session_id":"ses_1","loop_id":"loop_live","dropped_before":0}}}),
    );
    let preview = |revision: u64, body: &str| {
        json!({"type":"tool_arguments_preview","data":{
        "turn":{"session_id":tool_key.session_id,"loop_id":tool_key.loop_id},
        "request_index":tool_key.request_index,"tool_call_id":tool_key.tool_call_id,
        "tool_name":"write","attempt":1,"revision":revision,"state":"generating",
        "partial":false,"display":{"detail":"preview.rs","expanded_input":body},
        "meta":{"session_id":tool_key.session_id,"loop_id":tool_key.loop_id,"dropped_before":0}}})
    };
    notify(&mut app, preview(1, "first"));
    assert!(
        !app.active_view().unwrap().scroll.new_content,
        "following the tail has no unread output"
    );
    prepare(&mut app);
    control_key(&mut app, KeyCode::Home);
    prepare(&mut app);
    let screen = screen(&app);
    let prepared = app.prepared_conversation(screen.content.width).unwrap();
    let total = prepared.total_rows();
    assert!(transcript::scroll_position(&app, total, screen.transcript.height as usize).marker);
    assert_eq!(app.viewport.1, screen.transcript.height as usize - 1);
    assert!(!app.active_view().unwrap().scroll.follow_tail);
    assert!(!app.active_view().unwrap().scroll.new_content);
    for revision in [1, 0] {
        notify(&mut app, preview(revision, "ignored stale body"));
        assert!(
            !app.active_view().unwrap().scroll.new_content,
            "ignored snapshots are not arrivals"
        );
        assert_eq!(
            app.active_view().unwrap().tool_presentations[&tool_key]
                .display
                .expanded_input
                .as_deref(),
            Some("first")
        );
    }
    let commands = notify(&mut app, preview(2, "first grows"));
    assert!(
        testapp::take_requests(commands)
            .iter()
            .all(|request| { !matches!(request.method, "tool.read" | "tool.output") })
    );
    let facts = &app.active_view().unwrap().tool_presentations[&tool_key];
    assert_eq!(facts.arguments_preview.as_ref().unwrap().revision, 2);
    assert_eq!(facts.display.expanded_input.as_deref(), Some("first grows"));
    assert_eq!(
        transcript::prepare_conversation(&app, screen.content.width).total_rows(),
        total
    );
    assert!(!app.active_view().unwrap().scroll.follow_tail);
    assert!(
        app.active_view().unwrap().scroll.new_content,
        "accepted preview-only growth is new output even without a new transcript row"
    );
    assert!(rendered_row(&app, screen.transcript.bottom() - 1).contains("↓ new output"));
}
