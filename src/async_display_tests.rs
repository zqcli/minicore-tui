//! Regression coverage through the production scheduler, worker and draw path.
//! Current-thread tests deliberately do not yield between toggle and draw: the
//! real worker cannot finish until we explicitly receive its completion events.
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use minicore_tui::protocol::{Reasoning, SessionInfo, ToolOutcomeWire, UserMessageKindWire};
use minicore_tui::state::session::SessionView;
use minicore_tui::state::transcript::{
    AssistantBlock, AssistantPart, ToolBlock, TranscriptBlock, UserBlock,
};
use minicore_tui::state::view::{
    ConversationSelection, SectionKind, SelectionGranularity, SelectionPoint,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use std::fmt::Write as _;
use std::sync::Arc;

const AREA: Rect = Rect::new(0, 0, 100, 40);

fn fixture() -> App {
    fixture_with_app(App::new(PathBuf::from("/synthetic")))
}

fn fixture_with_app(mut app: App) -> App {
    let info: SessionInfo = serde_json::from_value(serde_json::json!({
        "session_id": "display", "title": null, "profile": "coding",
        "workspace": "/synthetic", "model": "deep", "reasoning": "high",
        "loaded": true, "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let mut view = SessionView::new(info);
    view.transcript.push_block(TranscriptBlock::User(UserBlock {
        index: Some(0),
        loop_id: Some("loop".into()),
        kind: UserMessageKindWire::Prompt,
        text: "retained-user-content".into(),
        pending: false,
    }));
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 1,
            loop_id: "loop".into(),
            request_index: 0,
            model: "deep".into(),
            reasoning_level: Reasoning::High,
            parts: vec![
                // Only runs with more than three raw lines are collapsible.
                AssistantPart::Reasoning("thinking first\nthinking second\nthinking third\nthinking fourth\nthinking fifth\nthinking sixth".into()),
                AssistantPart::Text("retained-assistant-content".into()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".into(),
            terminal_error: None,
        }));
    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
        index: Some(2),
        loop_id: "loop".into(),
        request_index: 0,
        tool_call_id: "call".into(),
        name: "read".into(),
        result: Some(Arc::from((0..60).fold(String::new(), |mut body, n| {
            writeln!(body, "tool result line {n}").unwrap();
            body
        }))),
        outcome: Some(ToolOutcomeWire::Success),
        live_status: None,
        progress: None,
        expanded: true,
    }));
    view.transcript.complete = true;
    view.scroll.follow_tail = false;
    app.sessions.known.insert("display".into(), view);
    app.sessions.active = Some("display".into());
    app.enable_async_layout();
    app.update(AppEvent::TerminalSize {
        width: AREA.width,
        height: AREA.height,
    });
    app
}

fn toggle_tool() -> AppEvent {
    AppEvent::ToggleTool {
        session_id: "display".into(),
        loop_id: "loop".into(),
        request_index: 0,
        tool_call_id: "call".into(),
    }
}

fn toggle_thinking() -> AppEvent {
    AppEvent::ToggleReasoningSection {
        session_id: "display".into(),
        loop_id: "loop".into(),
        request_index: 0,
        ordinal: 0,
    }
}

fn draw(app: &mut App, jobs: &mut LocalJobs, terminal: &mut Terminal<TestBackend>, area: Rect) {
    prepare_frame_with_jobs(app, jobs, area);
    draw_frame(terminal, app).unwrap();
}

/// Find an actual visible content row, not a section's leading padding.
fn section_hit(app: &App, kind: SectionKind, needle: &str) -> (u16, u16) {
    let screen = ui::layout::screen_layout(app, AREA);
    let prepared = app.prepared_conversation(screen.content.width).unwrap();
    let section = prepared
        .sections
        .iter()
        .find(|section| section.id.kind == kind)
        .unwrap();
    assert!(
        section.collapsible,
        "fixture must exercise a real collapsible section"
    );
    // These fixtures keep the target at the top. Assert that precondition
    // rather than duplicating the private production scroll-position math.
    let view = app.active_view().unwrap();
    assert_eq!(view.scroll.offset, 0);
    assert!(!view.scroll.follow_tail || prepared.total_rows() <= screen.transcript.height as usize);
    let row = section
        .rows
        .clone()
        .find(|row| {
            prepared
                .window(*row, 1)
                .iter()
                .any(|line| line.to_string().contains(needle))
        })
        .expect("visible section content, not blank padding");
    assert!(
        row < ui::transcript::visible_rows(app, prepared.total_rows(), screen.transcript.height)
    );
    (
        screen.transcript.x + section.content_columns.start as u16,
        screen.transcript.y + row as u16,
    )
}

fn click(app: &mut App, column: u16, row: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        let commands = app.update(AppEvent::Terminal(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
        assert!(
            commands.is_empty(),
            "single fold click must not open detail or copy: {commands:?}"
        );
    }
}

fn cells(buffer: &Buffer, area: Rect) -> Buffer {
    let mut result = Buffer::empty(area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            result[(x, y)] = buffer[(x, y)].clone();
        }
    }
    result
}

fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn assert_no_old_content(terminal: &Terminal<TestBackend>) {
    let screen = text(terminal.backend().buffer());
    for marker in [
        "retained-user-content",
        "retained-assistant-content",
        "tool result line",
        "thinking first",
    ] {
        assert!(
            !screen.contains(marker),
            "old transcript leaked across a display barrier: {marker}"
        );
    }
}

async fn finish(
    app: &mut App,
    jobs: &mut LocalJobs,
    terminal: &mut Terminal<TestBackend>,
    area: Rect,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            draw(app, jobs, terminal, area);
            if app
                .prepared_conversation(ui::layout::screen_layout(app, area).content.width)
                .is_some()
            {
                break;
            }
            app.update(
                jobs.events()
                    .recv()
                    .await
                    .expect("owned layout worker event"),
            );
        }
    })
    .await
    .expect("current layout must complete");
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_delayed_folds_keep_cells_and_editor_live() {
    let mut app = fixture();
    let mut jobs = LocalJobs::new();
    let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    let screen = ui::layout::screen_layout(&app, AREA);
    assert!(text(terminal.backend().buffer()).contains("retained-user-content"));

    for toggle in [
        toggle_tool(),
        toggle_tool(),
        toggle_thinking(),
        toggle_thinking(),
    ] {
        let view = app.sessions.known.get_mut("display").unwrap();
        view.scroll.follow_tail = false;
        view.scroll.offset = 0;
        draw(&mut app, &mut jobs, &mut terminal, AREA);
        let before = cells(terminal.backend().buffer(), screen.transcript);
        let viewport = app.viewport;
        let old_layout = Arc::downgrade(
            &app.prepared_conversation(screen.content.width)
                .unwrap()
                .durable
                .as_ref()
                .unwrap()
                .layout,
        );
        app.update(toggle);
        assert!(app.prepared_conversation(screen.content.width).is_none());
        assert!(
            old_layout.upgrade().is_none(),
            "screen cells must not pin an invalidated layout"
        );
        for _ in 0..3 {
            draw(&mut app, &mut jobs, &mut terminal, AREA);
            assert_eq!(
                cells(terminal.backend().buffer(), screen.transcript),
                before
            );
            assert_eq!(
                app.viewport, viewport,
                "pending work is not a zero-row conversation"
            );
        }
        let footer_before = cells(terminal.backend().buffer(), screen.footer);
        app.sessions
            .known
            .get_mut("display")
            .unwrap()
            .info
            .model
            .push('x');
        app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::NONE,
        ))));
        draw(&mut app, &mut jobs, &mut terminal, AREA);
        assert_ne!(
            cells(terminal.backend().buffer(), screen.footer),
            footer_before
        );
        assert_eq!(
            cells(terminal.backend().buffer(), screen.transcript),
            before
        );
        assert!(text(&cells(terminal.backend().buffer(), screen.panel)).contains('z'));
        finish(&mut app, &mut jobs, &mut terminal, AREA).await;
        assert_ne!(
            cells(terminal.backend().buffer(), screen.transcript),
            before,
            "the completed fold must replace, not permanently freeze, the screen"
        );
    }
    jobs.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_rejects_stale_completions_and_stale_mouse_hits() {
    let mut app = fixture();
    let mut jobs = LocalJobs::new();
    let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    let screen = ui::layout::screen_layout(&app, AREA);
    let (mouse_column, mouse_row) = section_hit(&app, SectionKind::Thinking, "thinking first");
    let before = cells(terminal.backend().buffer(), screen.transcript);
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, AREA);

    // Let the actual worker finish, but hold its entire result stream outside
    // the reducer. Two more toggles make these completions stale.
    let mut delayed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), jobs.events().recv())
            .await
            .unwrap()
            .unwrap();
        let complete = matches!(&event, AppEvent::DurableLayoutPrepared(result) if result.complete);
        delayed.push(event);
        if complete {
            break;
        }
    }
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    let revision = app.active_view().unwrap().transcript.render_revision;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::ScrollDown,
    ] {
        assert!(
            app.update(AppEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column: mouse_column,
                row: mouse_row,
                modifiers: KeyModifiers::NONE,
            })))
            .is_empty()
        );
    }
    assert_eq!(
        app.active_view().unwrap().transcript.render_revision,
        revision
    );
    assert!(app.selection.is_none());
    for event in delayed {
        app.update(event);
        draw(&mut app, &mut jobs, &mut terminal, AREA);
        assert!(app.prepared_conversation(screen.content.width).is_none());
        assert_eq!(
            cells(terminal.backend().buffer(), screen.transcript),
            before
        );
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            prepare_frame_with_jobs(&mut app, &mut jobs, AREA);
            if app.prepared_conversation(screen.content.width).is_some() {
                break;
            }
            app.update(jobs.events().recv().await.unwrap());
        }
    })
    .await
    .unwrap();
    // Even a completed layout is not hittable until it has actually replaced
    // the old cells on the terminal (the production loop throttles draws).
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.update(AppEvent::Terminal(Event::Mouse(MouseEvent {
            kind,
            column: mouse_column,
            row: mouse_row,
            modifiers: KeyModifiers::NONE,
        })));
    }
    assert_eq!(
        app.active_view().unwrap().transcript.render_revision,
        revision
    );
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    assert_ne!(
        cells(terminal.backend().buffer(), screen.transcript),
        before
    );
    let before_click = cells(terminal.backend().buffer(), screen.transcript);
    // A successful draw restores hit testing, not only display.
    let revision = app.active_view().unwrap().transcript.render_revision;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.update(AppEvent::Terminal(Event::Mouse(MouseEvent {
            kind,
            column: mouse_column,
            row: mouse_row,
            modifiers: KeyModifiers::NONE,
        })));
    }
    assert!(app.active_view().unwrap().transcript.render_revision > revision);
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    assert_eq!(
        cells(terminal.backend().buffer(), screen.transcript),
        before_click
    );
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    jobs.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_tool_mouse_cycles_resume_after_each_draw() {
    let start = Instant::now();
    let mut app = fixture_with_app(App::with_monotonic_clock(
        PathBuf::from("/synthetic"),
        move || start,
    ));
    let mut jobs = LocalJobs::new();
    let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    let screen = ui::layout::screen_layout(&app, AREA);
    for expected_folded in [true, false, true, false] {
        // Keep the clock frozen: a fresh draw must immediately restore hits.
        // Alternate real content rows, so valid rapid single clicks do not
        // become the existing same-word double-click selection gesture.
        let needle = if expected_folded {
            "tool result line 0"
        } else {
            "read"
        };
        let (column, row) = section_hit(&app, SectionKind::Tool, needle);
        let before = cells(terminal.backend().buffer(), screen.transcript);
        let revision = app.active_view().unwrap().transcript.render_revision;
        let old_section = app
            .prepared_conversation(screen.content.width)
            .unwrap()
            .sections
            .iter()
            .find(|section| section.id.kind == SectionKind::Tool)
            .unwrap();
        assert_eq!(old_section.folded, !expected_folded);
        click(&mut app, column, row);
        let toggled_revision = app.active_view().unwrap().transcript.render_revision;
        assert!(
            toggled_revision > revision,
            "a normal click on an already drawn frame must toggle"
        );
        assert!(app.prepared_conversation(screen.content.width).is_none());
        draw(&mut app, &mut jobs, &mut terminal, AREA);
        for _ in 0..3 {
            click(&mut app, column, row);
            draw(&mut app, &mut jobs, &mut terminal, AREA);
            assert_eq!(
                app.active_view().unwrap().transcript.render_revision,
                toggled_revision,
                "rapid pending clicks cannot resolve stale screen cells"
            );
            assert_eq!(
                cells(terminal.backend().buffer(), screen.transcript),
                before
            );
        }
        finish(&mut app, &mut jobs, &mut terminal, AREA).await;
        let section = app
            .prepared_conversation(screen.content.width)
            .unwrap()
            .sections
            .iter()
            .find(|section| section.id.kind == SectionKind::Tool)
            .unwrap();
        assert_eq!(section.folded, expected_folded);
        assert_ne!(
            cells(terminal.backend().buffer(), screen.transcript),
            before
        );
    }
    jobs.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_geometry_session_theme_and_epoch_barriers() {
    let mut app = fixture();
    let mut jobs = LocalJobs::new();
    let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    let resized = Rect::new(0, 0, AREA.width, AREA.height - 1);
    // No TerminalSize event first: the size can change between select and draw.
    terminal.backend_mut().resize(resized.width, resized.height);
    terminal.resize(resized).unwrap();
    draw(&mut app, &mut jobs, &mut terminal, resized);
    assert_no_old_content(&terminal);
    finish(&mut app, &mut jobs, &mut terminal, resized).await;
    assert!(text(terminal.backend().buffer()).contains("tool result line"));

    app.update(AppEvent::SetTheme(minicore_tui::theme::ThemeKind::Light));
    draw(&mut app, &mut jobs, &mut terminal, resized);
    assert_no_old_content(&terminal);
    finish(&mut app, &mut jobs, &mut terminal, resized).await;

    let mut other = app.active_view().unwrap().info.clone();
    other.session_id = "other".into();
    app.sessions
        .known
        .insert("other".into(), SessionView::new(other));
    app.sessions.active = Some("other".into());
    app.update(AppEvent::Tick);
    draw(&mut app, &mut jobs, &mut terminal, resized);
    assert_no_old_content(&terminal);
    // Returning before either worker result arrives must not resurrect cells.
    app.sessions.active = Some("display".into());
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, resized);
    assert_no_old_content(&terminal);
    finish(&mut app, &mut jobs, &mut terminal, resized).await;
    app.sessions.known.get_mut("display").unwrap().session_epoch += 1;
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, resized);
    assert_no_old_content(&terminal);
    finish(&mut app, &mut jobs, &mut terminal, resized).await;
    jobs.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_preserves_selection_and_scroll_anchor_until_commit() {
    let mut app = fixture();
    let mut jobs = LocalJobs::new();
    let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    let screen = ui::layout::screen_layout(&app, AREA);
    let prepared = app.prepared_conversation(screen.content.width).unwrap();
    let section = prepared
        .sections
        .iter()
        .find(|section| section.id.kind == SectionKind::User)
        .unwrap();
    let point = SelectionPoint {
        row: section.rows.start,
        column: section.content_columns.start,
        section_id: Some(section.id.clone()),
        section_row: 0,
    };
    app.selection = Some(ConversationSelection {
        session_id: "display".into(),
        anchor: point.clone(),
        focus: SelectionPoint {
            column: point.column + 3,
            ..point
        },
        granularity: SelectionGranularity::Character,
        dragged: true,
    });
    let view = app.sessions.known.get_mut("display").unwrap();
    view.scroll.follow_tail = false;
    view.scroll.offset = 1;
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    let selection = app.selection.clone();
    let before = cells(terminal.backend().buffer(), screen.transcript);
    app.update(toggle_thinking());
    let anchor = app
        .active_view()
        .unwrap()
        .scroll
        .anchor
        .clone()
        .expect("source anchor captured");
    for _ in 0..3 {
        draw(&mut app, &mut jobs, &mut terminal, AREA);
        assert_eq!(app.active_view().unwrap().scroll.offset, 1);
        assert_eq!(app.selection, selection);
        assert_eq!(
            cells(terminal.backend().buffer(), screen.transcript),
            before
        );
    }
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    assert_eq!(
        app.active_view().unwrap().scroll.anchor.as_ref(),
        Some(&anchor)
    );
    assert_eq!(
        app.selection.as_ref().unwrap().anchor.section_id,
        selection.unwrap().anchor.section_id
    );
    jobs.shutdown().await;
}

fn displayed_tool_button(app: &App) -> (u16, u16) {
    let screen = ui::layout::screen_layout(app, AREA);
    let prepared = app.prepared_conversation(screen.content.width).unwrap();
    let section = prepared
        .sections
        .iter()
        .find(|s| s.id.kind == minicore_tui::state::view::SectionKind::Tool)
        .unwrap();
    (
        screen.transcript.right().saturating_sub(9),
        screen.transcript.y + section.rows.start as u16 + 1,
    )
}

fn press_tool_button(app: &mut App, point: (u16, u16)) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: point.0,
        row: point.1,
        modifiers: KeyModifiers::NONE,
    })))
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_has_no_detail_hit_during_reprepare() {
    let mut app = fixture();
    let mut jobs = LocalJobs::new();
    let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    finish(&mut app, &mut jobs, &mut terminal, AREA).await;
    let point = displayed_tool_button(&app);
    let screen = ui::layout::screen_layout(&app, AREA);
    let before = cells(terminal.backend().buffer(), screen.transcript);
    app.update(toggle_thinking());
    draw(&mut app, &mut jobs, &mut terminal, AREA);
    assert!(app.prepared_conversation(screen.content.width).is_none());
    assert_eq!(
        cells(terminal.backend().buffer(), screen.transcript),
        before
    );

    press_tool_button(&mut app, point);
    assert!(
        app.tool_detail().is_none(),
        "removed detail label leaves no invisible hit"
    );
    jobs.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn production_async_display_tool_button_rejects_navigation_geometry_and_missing_identity() {
    for barrier in [
        "resize", "theme", "epoch", "session", "help", "scroll", "removed", "composer", "fatal",
        "shutdown",
    ] {
        let mut app = fixture();
        let mut jobs = LocalJobs::new();
        let mut terminal = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
        finish(&mut app, &mut jobs, &mut terminal, AREA).await;
        let point = displayed_tool_button(&app);
        app.update(toggle_thinking());
        draw(&mut app, &mut jobs, &mut terminal, AREA);
        match barrier {
            "resize" => {
                app.update(AppEvent::TerminalSize {
                    width: AREA.width,
                    height: AREA.height + 1,
                });
                app.update(AppEvent::TerminalSize {
                    width: AREA.width,
                    height: AREA.height,
                });
            }
            "theme" => {
                app.update(AppEvent::SetTheme(minicore_tui::theme::ThemeKind::Light));
            }
            "epoch" => {
                app.sessions.known.get_mut("display").unwrap().session_epoch += 1;
            }
            "session" => {
                app.sessions.active = Some("other".into());
            }
            "help" => {
                app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::F(1),
                    KeyModifiers::NONE,
                ))));
                app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                ))));
            }
            "scroll" => {
                app.sessions.known.get_mut("display").unwrap().scroll.offset = 1;
                app.update(AppEvent::Tick);
                app.sessions.known.get_mut("display").unwrap().scroll.offset = 0;
            }
            "removed" => {
                Arc::make_mut(
                    &mut app
                        .sessions
                        .known
                        .get_mut("display")
                        .unwrap()
                        .transcript
                        .blocks,
                )
                .retain(|block| !matches!(block.as_ref(), TranscriptBlock::Tool(_)));
            }
            "fatal" | "shutdown" => {
                app.connection = if barrier == "fatal" {
                    minicore_tui::app::ConnectionState::Failed("synthetic".into())
                } else {
                    minicore_tui::app::ConnectionState::ShuttingDown
                };
                app.update(AppEvent::Tick);
                app.connection = minicore_tui::app::ConnectionState::Ready;
            }
            "composer" => {
                app.composer_mut()
                    .set_text("one\ntwo\nthree\nfour\nfive\nsix");
            }
            _ => unreachable!(),
        }
        press_tool_button(&mut app, point);
        assert!(
            app.tool_detail().is_none(),
            "stale tool hit crossed {barrier} barrier"
        );
        jobs.shutdown().await;
    }
}
