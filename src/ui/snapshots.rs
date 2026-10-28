//! Full-screen render tests. Each one builds an `App`, draws it into a
//! `TestBackend` and compares the text of the frame against a checked-in
//! snapshot, so a visual regression shows up as a diff instead of a screenshot.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use tokio::sync::mpsc;

use crate::app::{App, Msg, Overlay, Scroll, Session};
use crate::protocol::read::UserKind;
use crate::protocol::stamp::unix_millis;
use crate::protocol::{LoopOutcome, Reasoning, SessionInfo, ToolRef, TurnRef};
use crate::transcript::{Block, ChangeSummary, RequestKey, ToolCall, ToolState, Transcript};
use crate::ui::theme::{Palette, Styles};

/// A render fixture. The client talks to an Agent that closes at once, which no
/// render observes.
struct Fixture {
    app: App,
    _runtime: tokio::runtime::Runtime,
}

impl std::ops::Deref for Fixture {
    type Target = App;

    fn deref(&self) -> &App {
        &self.app
    }
}

impl std::ops::DerefMut for Fixture {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

fn app() -> Fixture {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (tx, _rx) = mpsc::unbounded_channel::<Msg>();
    let (incoming_tx, _incoming_rx) = mpsc::channel(4);
    let client = runtime.block_on(async {
        crate::rpc::Client::start(
            tokio::io::sink(),
            tokio::io::empty(),
            tokio::io::empty(),
            incoming_tx,
        )
    });
    let mut app = App::new(client, tx, PathBuf::from("/work/minicore-tui"));
    app.styles = Styles::of(Palette::dark());
    app.ready = true;
    let mut session = Session::new(info(), 1);
    session.transcript = Transcript::default();
    app.session = Some(session);
    Fixture {
        app,
        _runtime: runtime,
    }
}

fn info() -> SessionInfo {
    SessionInfo {
        session_id: "ses_0123456789abcdef".into(),
        title: Some("Cache the parser".into()),
        profile: "coding".into(),
        workspace: "/work/minicore-tui".into(),
        model: "gpt-6-luna".into(),
        reasoning: Reasoning::Max,
        loaded: true,
        updated_at: String::new(),
    }
}

fn tool() -> ToolCall {
    let mut call = ToolCall::new(
        ToolRef {
            session_id: "ses_0123456789abcdef".into(),
            loop_id: "loop_1".into(),
            request_index: 1,
            tool_call_id: "call_1".into(),
        },
        "bash",
    );
    call.detail = "$ cargo test --lib".into();
    call.state = ToolState::Succeeded;
    call.duration = Some(std::time::Duration::from_millis(4200));
    call.exit_code = Some(0);
    call.output = vec!["running 131 tests".into(), "test result: ok".into()];
    call.output_dropped = 14;
    call
}

fn conversation(app: &mut App) {
    let session = app.session.as_mut().expect("session");
    let transcript = &mut session.transcript;
    transcript.push(Block::User {
        text: "Why does the parser re-parse the whole message on every delta?".into(),
        kind: UserKind::Prompt,
        at: None,
    });
    transcript.push(Block::Assistant {
        key: RequestKey {
            loop_id: "loop_1".into(),
            request_index: 0,
        },
        reasoning: "The layout cache only keyed on the block revision...".into(),
        text: "It does not any more. The **streaming renderer** keeps the stable prefix and\nre-renders only the tail, so a delta costs one small parse.".into(),
        streaming: false,
        expanded: None,
    });
    transcript.push(Block::Tool(Box::new(tool())));
    transcript.push(Block::TurnEnd {
        loop_id: "loop_1".into(),
        outcome: LoopOutcome::Completed,
        duration: Some(std::time::Duration::from_secs(12)),
        requests: 2,
        tools: 1,
        usage: Default::default(),
        changes: Some(ChangeSummary {
            files: 2,
            lines: Some((40, 7)),
        }),
    });
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for row in 0..buffer.area.height {
        let mut line = String::new();
        for column in 0..buffer.area.width {
            line.push_str(buffer[(column, row)].symbol());
        }
        out.push_str(line.trim_end_matches(' '));
        out.push('\n');
    }
    out
}

/// Checked-in frames, reviewed with the code that draws them;
/// `UPDATE_SNAPSHOTS=1 cargo test` rewrites them.
const IDLE: &str = include_str!("../../tests/snapshots/idle.txt");
const STREAMING: &str = include_str!("../../tests/snapshots/streaming.txt");
const DRAFT: &str = include_str!("../../tests/snapshots/draft.txt");

fn snapshot(path: &str, expected: &str, rendered: String) {
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        std::fs::write(file, rendered).expect("write snapshot");
        return;
    }
    assert_eq!(expected, rendered, "frame differs from {path}");
}

#[test]
fn idle_screen_shows_the_rail_composer_and_footer() {
    let mut app = app();
    conversation(&mut app);
    snapshot("tests/snapshots/idle.txt", IDLE, render(&mut app, 70, 20));
}

#[test]
fn expanded_cards_show_hidden_lines_at_the_cut() {
    let mut app = app();
    let mut read = tool();
    read.name = "read".into();
    read.detail = "src/main.rs:1-40".into();
    read.duration = None;
    read.exit_code = None;
    read.output = vec!["1: fn main() {".into(), "2:     run();".into()];
    read.output_dropped = 0;
    read.output_lines = Some(40);
    let transcript = &mut app.session.as_mut().unwrap().transcript;
    transcript.push(Block::Tool(Box::new(tool())));
    transcript.push(Block::Tool(Box::new(read)));
    app.tools_expanded = true;
    snapshot(
        "tests/snapshots/expanded-cards.txt",
        include_str!("../../tests/snapshots/expanded-cards.txt"),
        render(&mut app, 70, 32),
    );
}

#[test]
fn the_draft_is_the_welcome_header_over_the_editor_and_the_footer() {
    let mut app = app();
    app.session = None;
    app.profiles = vec![
        serde_json::from_value(
            serde_json::json!({"id": "coding", "model": "gpt-6-luna", "reasoning": "max"}),
        )
        .expect("profile"),
    ];
    app.default_profile = Some("coding".into());
    let screen = render(&mut app, 70, 14);
    snapshot("tests/snapshots/draft.txt", DRAFT, screen.clone());
    assert!(!screen.contains("No session"), "a draft shows no status");

    app.ready = false;
    let screen = render(&mut app, 70, 16);
    assert!(screen.contains("minicore-tui v"), "{screen}");
    assert!(screen.contains("Connecting to minicore-agent…"), "{screen}");
}

#[test]
fn the_screen_shows_earlier_content_while_streaming() {
    let mut app = app();
    conversation(&mut app);
    let session = app.session.as_mut().expect("session");
    session.transcript.push(Block::User {
        text: "Now stream a longer answer.".into(),
        kind: UserKind::Prompt,
        at: unix_millis("2026-10-20T21:41:09Z"),
    });
    session.transcript.push(Block::Assistant {
        key: RequestKey {
            loop_id: "loop_2".into(),
            request_index: 0,
        },
        reasoning: "I should explain the anchor rule first, then show it.".into(),
        text: "An anchored view keeps its block and row, so\nnew output below never moves the text you\nare reading. Only scrolling to the end returns\nto following.".into(),
        streaming: true,
        expanded: None,
    });
    snapshot(
        "tests/snapshots/streaming.txt",
        STREAMING,
        render(&mut app, 70, 20),
    );
}

#[test]
fn an_anchored_view_does_not_move_when_content_grows() {
    let mut app = app();
    conversation(&mut app);
    // Scroll up: the anchor is the first user block.
    app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    let before = render(&mut app, 70, 20);
    let session = app.session.as_mut().expect("session");
    session.transcript.push(Block::Assistant {
        key: RequestKey {
            loop_id: "loop_3".into(),
            request_index: 0,
        },
        reasoning: String::new(),
        text: (0..60)
            .map(|index| format!("appended line {index}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        streaming: true,
        expanded: None,
    });
    let after = render(&mut app, 70, 20);
    let kept: Vec<&str> = before.lines().take(10).collect();
    assert!(
        kept.iter()
            .all(|line| after.lines().any(|new| new == *line)),
        "the anchored top must not be replaced:\n{before}\n---\n{after}"
    );
    assert!(
        after.contains("↓ Jump to latest message · End"),
        "the label marks content below an anchored view:\n{after}"
    );
    assert!(
        matches!(app.scroll, Scroll::Anchor { .. }),
        "{:?}",
        app.scroll
    );
}

#[test]
fn the_editor_grows_with_its_content_and_caps_at_twelve_rows() {
    let mut app = app();
    app.composer.set_text(
        &(0..20)
            .map(|index| format!("line {index} of a long draft"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let rendered = render(&mut app, 60, 60);
    let editor_rows = rendered
        .lines()
        .filter(|line| line.starts_with(" ▎"))
        .count();
    assert_eq!(
        editor_rows, 12,
        "the editor caps at twelve rows:\n{rendered}"
    );
    assert!(rendered.contains("line 19 of a long draft"), "{rendered}");
}

#[test]
fn the_footer_reports_model_and_reasoning_without_duplicate_status() {
    let mut app = app();
    app.composer.set_text("hello");
    let rendered = render(&mut app, 80, 12);
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(
        footer.starts_with(" ▸ minicore-tui · gpt 6 luna · max"),
        "{footer}"
    );
    assert!(
        !footer.contains('@'),
        "an unknown branch is left out: {footer}"
    );
    assert!(
        !footer.contains("ctx"),
        "no context before it is known: {footer}"
    );
    assert!(
        !footer.contains('↑'),
        "no token totals before any usage: {footer}"
    );
}

#[test]
fn the_footer_names_the_branch_and_right_aligns_the_totals() {
    let mut app = app();
    let session = app.session.as_mut().expect("session");
    session.presentation = Some(
        serde_json::from_value(serde_json::json!({
            "session_id": "ses_0123456789abcdef", "git_branch": "main",
            "context": {"tokens": 16400, "window": 40000, "percent": 41.0, "kind": "reported"}
        }))
        .unwrap(),
    );
    session.usage = crate::protocol::UsageTotals {
        input: 50_500,
        output: 1_800,
        cached: 33_800,
    };
    let rendered = render(&mut app, 100, 12);
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(
        footer.starts_with(" ▸ minicore-tui@main · gpt 6 luna"),
        "{footer}"
    );
    assert!(!footer.contains("Cache the parser"), "{footer}");
    assert!(
        footer.ends_with("↑50.5k ↓1.8k R33.8k · ctx 41.00%"),
        "{footer}"
    );
    assert_eq!(crate::wrap::width(footer), 100, "flush right: {footer}");
    // On a narrow terminal the directory is cut first, then dropped; the
    // model and level stay.
    for (columns, identity) in [(60_u16, " ▸ mini…"), (50, " gpt")] {
        let narrow = render(&mut app, columns, 12);
        let footer = narrow.lines().last().unwrap_or_default();
        assert!(footer.starts_with(identity), "{footer}");
        assert!(footer.contains("gpt 6 luna · max"), "{footer}");
        assert!(footer.ends_with("ctx 41.00%"), "{footer}");
        assert_eq!(crate::wrap::width(footer), usize::from(columns), "{footer}");
    }
}

/// The last transcript row never touches the stack below it, and the
/// composer's tint spans the whole row.
#[test]
fn a_blank_row_separates_the_transcript_and_the_composer_band_is_full_width() {
    let mut app = app();
    for index in 0..40 {
        app.session
            .as_mut()
            .expect("session")
            .transcript
            .push(Block::User {
                text: format!("prompt {index}"),
                kind: UserKind::Prompt,
                at: None,
            });
    }
    let backend = TestBackend::new(50, 14);
    let mut terminal = Terminal::new(backend).expect("terminal");
    crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, &mut app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let row = |y: u16| -> String {
        (0..50)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    let composer = terminal.backend().cursor_position().y;
    assert_eq!(row(composer), " ▎", "the empty editor has no hint");
    assert_eq!(row(composer - 2), "", "the gap row above the editor");
    assert_eq!(row(composer - 3).trim(), "▎", "the card's bottom pad row");
    assert!(
        row(composer - 4).contains("prompt 39"),
        "{}",
        row(composer - 4)
    );
    assert_eq!(
        buffer[(0u16, composer)].bg,
        Color::Reset,
        "the gutter is never painted"
    );
    assert_eq!(
        buffer[(49u16, composer)].bg,
        Palette::dark().composer_bg,
        "the band reaches the last column"
    );
}

#[test]
fn the_status_spinner_moves_with_time() {
    use std::time::Duration;
    let frames: std::collections::HashSet<&str> = (0..10)
        .map(|step| crate::ui::status::spinner(Duration::from_millis(step * 80)))
        .collect();
    assert_eq!(frames.len(), 10, "every tick shows the next frame");
    assert_eq!(
        crate::ui::status::spinner(Duration::from_millis(800)),
        crate::ui::status::spinner(Duration::ZERO)
    );
}

#[test]
fn a_fatal_error_shows_the_stderr_tail() {
    let mut app = app();
    app.fatal = Some(crate::app::Fatal {
        reason: "agent exited (1)".into(),
        stderr_tail: vec![
            "error: could not read config".into(),
            "error: giving up".into(),
        ],
    });
    let rendered = render(&mut app, 60, 14);
    assert!(rendered.contains("Fatal error"), "{rendered}");
    assert!(rendered.contains("agent exited (1)"), "{rendered}");
    assert!(rendered.contains("could not read config"), "{rendered}");
    assert!(
        rendered.to_lowercase().contains("press q"),
        "the fatal screen says how to quit:\n{rendered}"
    );
}

#[test]
fn no_color_renders_the_same_layout_without_color() {
    let mut app = app();
    conversation(&mut app);
    let colored = render(&mut app, 70, 20);
    app.styles = Styles::of(Palette::plain());
    let plain = render(&mut app, 70, 20);
    assert_eq!(colored, plain, "NO_COLOR changes colors, never layout");
}

#[test]
fn a_narrow_terminal_keeps_every_row_within_the_width() {
    let mut app = app();
    conversation(&mut app);
    let rendered = render(&mut app, 30, 24);
    for line in rendered.lines() {
        assert!(
            crate::wrap::width(line) <= 30,
            "a row exceeds the terminal width: {line:?}"
        );
    }
}

#[test]
fn typing_keeps_the_draft_visible_and_marked_with_the_rail() {
    let mut app = app();
    for character in "hello".chars() {
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        );
        assert_eq!(app.on_key(key), None);
    }
    let rendered = render(&mut app, 40, 10);
    assert!(rendered.contains(" ▎hello"), "{rendered}");
    assert!(
        !rendered.contains("Ask anything"),
        "the placeholder is gone once there is a draft:\n{rendered}"
    );
}

#[test]
fn an_open_overlay_is_not_drawn_over_the_composer() {
    let mut app = app();
    app.composer.set_text("draft");
    app.overlay = Overlay::Help { scroll: 0 };
    let rendered = render(&mut app, 40, 10);
    assert!(
        rendered.contains(" ▎draft"),
        "the composer stays visible under a view:\n{rendered}"
    );
}

#[test]
fn the_session_view_lists_the_tools_allowed_for_the_session_only_when_there_are_some() {
    let mut app = app();
    app.overlay = Overlay::SessionInfo { items: Some(4) };
    assert!(!render(&mut app, 80, 24).contains("allowed"));
    app.session
        .as_mut()
        .unwrap()
        .allowed_tools
        .extend(["edit".to_owned(), "bash".to_owned()]);
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains("allowed   bash, edit"), "{screen}");
}

#[test]
fn a_view_dims_the_composer_and_keys_typed_in_it_do_not_edit_the_draft() {
    let mut app = app();
    app.overlay = Overlay::Help { scroll: 0 };
    let rendered = render(&mut app, 60, 12);
    assert!(rendered.contains("view open · esc to return"), "{rendered}");
    assert!(!rendered.contains("Ask anything"), "{rendered}");
    app.on_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('x'),
    ));
    assert!(app.composer.is_empty(), "the view owns the keys");

    app.overlay = Overlay::None;
    let rendered = render(&mut app, 60, 12);
    assert!(!rendered.contains("Ask anything"), "{rendered}");
    assert!(!rendered.contains("view open"), "{rendered}");
}

fn queue(app: &mut App, messages: &[(&str, crate::app::QueuedKind)]) {
    let queue = &mut app.session.as_mut().expect("session").queue;
    for (text, kind) in messages {
        queue.push(crate::app::Queued {
            text: (*text).into(),
            kind: *kind,
            sent: false,
            at: None,
        });
    }
}

/// The dock's key hint row, trimmed.
fn hint_row(rendered: &str) -> Option<&str> {
    rendered
        .lines()
        .find(|line| line.contains("steer ·") || line.contains("edit queued"))
        .map(str::trim)
}

#[test]
fn the_dock_lists_sent_and_unsent_messages_in_send_order_with_the_keys() {
    use crate::app::QueuedKind::{Follow, Steer};
    let mut app = app();
    conversation(&mut app);
    running_bash(&mut app);
    queue(
        &mut app,
        &[
            ("focus on the parser first", Steer),
            ("then update the docs", Follow),
            ("and the changelog", Steer),
        ],
    );
    app.session.as_mut().unwrap().queue[0].sent = true;
    app.composer.set_text("one more thing");
    snapshot(
        "tests/snapshots/dock-queue.txt",
        include_str!("../../tests/snapshots/dock-queue.txt"),
        render(&mut app, 70, 20),
    );
}

#[test]
fn the_dock_hint_follows_what_the_keys_can_do() {
    use crate::app::QueuedKind::Follow;
    let mut app = app();
    assert_eq!(hint_row(&render(&mut app, 70, 12)), None, "idle");

    running_bash(&mut app);
    assert_eq!(
        hint_row(&render(&mut app, 70, 12)),
        None,
        "no draft, no queue"
    );

    app.composer.set_text("draft");
    assert_eq!(
        hint_row(&render(&mut app, 70, 12)),
        Some("enter steer · alt+enter follow-up")
    );

    queue(&mut app, &[("later", Follow)]);
    assert_eq!(
        hint_row(&render(&mut app, 70, 12)),
        Some("enter steer · alt+enter follow-up · alt+↑ edit queued")
    );

    app.composer.clear();
    assert_eq!(
        hint_row(&render(&mut app, 70, 12)),
        Some("alt+↑ edit queued")
    );
}

#[test]
fn a_long_queued_message_is_cut_to_the_dock_width() {
    use crate::app::QueuedKind::Follow;
    let mut app = app();
    queue(
        &mut app,
        &[("a very long follow-up message that cannot fit", Follow)],
    );
    let rendered = render(&mut app, 30, 12);
    assert!(
        rendered.contains("↳ follow-up 1  a very long…"),
        "{rendered}"
    );
}

#[test]
fn a_running_turn_keeps_elapsed_time_only_in_the_footer() {
    let mut app = app();
    let session = app.session.as_mut().expect("session");
    session.live = Some(crate::app::LiveTurn::new(
        TurnRef {
            session_id: "ses_0123456789abcdef".into(),
            loop_id: "loop_9".into(),
        },
        0,
    ));
    let rendered = render(&mut app, 70, 12);
    assert!(
        rendered.contains("Working (esc to interrupt)"),
        "{rendered}"
    );
    assert!(rendered.contains("Thinking"), "{rendered}");
    let footer = rendered.lines().last().unwrap();
    assert!(footer.contains("gpt 6 luna · max"), "{footer}");
    assert!(!footer.contains("working"), "{footer}");
    let working = rendered
        .lines()
        .find(|line| line.contains("Working (esc to interrupt)"))
        .expect("working row");
    assert!(working.ends_with(" · Thinking"), "{working}");
    assert!(
        rendered.lines().last().unwrap().ends_with(" · 0m"),
        "{rendered}"
    );
}

#[test]
fn a_long_running_turn_has_no_duplicate_timer_in_the_working_row() {
    let mut fixture = app();
    let app = &mut fixture.app;
    let session = app.session.as_mut().unwrap();
    let mut live = crate::app::LiveTurn::new(
        TurnRef {
            session_id: session.info.session_id.clone(),
            loop_id: "loop_9".into(),
        },
        0,
    );
    live.running_tool = Some(("read".into(), "src/main.rs".into()));
    live.requests = 3;
    session.live = Some(live);
    let rendered = render(app, 110, 12);
    let working = rendered
        .lines()
        .find(|line| line.contains("Working (esc to interrupt)"))
        .expect("working row");
    assert!(
        working.ends_with("Running read src/main.rs · request 3"),
        "{working}"
    );
    assert!(
        rendered.lines().last().unwrap().ends_with(" · 0m"),
        "{rendered}"
    );
}

/// A left press and release on one cell: a click, not a drag.
fn click(app: &mut App, column: u16, row: u16) {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
}

fn row_of(screen: &str, prefix: &str) -> u16 {
    screen
        .lines()
        .position(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starts with {prefix:?}:\n{screen}")) as u16
}

#[test]
fn clicking_a_hidden_lines_row_collapses_the_expanded_card_without_opening_an_overlay() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 30);
    let header = row_of(&folded, " ▎ bash");
    click(app, 10, header);
    let expanded = render(app, 70, 30);
    assert!(expanded.contains("… 14 earlier lines"), "{expanded}");
    assert!(!app.tools_expanded, "only that card changed");

    click(app, 10, row_of(&expanded, " ▎ … 14 earlier lines"));
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    let refolded = render(app, 70, 30);
    assert!(refolded.contains("ctrl+o to expand"), "{refolded}");
}

#[test]
fn clicking_the_body_or_the_footer_of_an_expanded_card_collapses_it() {
    for prefix in [" ▎ test result: ok", " ▎ Took 4.2s"] {
        let mut fixture = app();
        let _runtime = fixture._runtime.enter();
        let app = &mut fixture.app;
        conversation(app);
        let folded = render(app, 70, 34);
        click(app, 10, row_of(&folded, " ▎ bash"));
        let open = render(app, 70, 34);
        assert!(open.contains(" ▎ $ cargo test --lib"), "{open}");
        click(app, 10, row_of(&open, prefix));
        let collapsed = render(app, 70, 34);
        assert!(
            collapsed.contains("ctrl+o to expand"),
            "a click on {prefix:?} left the card open:\n{collapsed}"
        );
        assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    }
}

#[test]
fn a_drag_across_an_expanded_card_body_selects_and_leaves_it_open() {
    use crossterm::event::MouseEventKind::{Down, Drag, Up};
    let mut fixture = app();
    // The copy is queued on the runtime and never runs.
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 30);
    click(app, 10, row_of(&folded, " ▎ bash"));
    let open = render(app, 70, 30);
    let body = row_of(&open, " ▎ running 131 tests");
    left(app, Down, 4, body);
    left(app, Drag, 12, body + 1);
    left(app, Up, 12, body + 1);
    assert!(app.selection.is_some());
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    let after = render(app, 70, 30);
    assert!(after.contains(" ▎ $ cargo test --lib"), "{after}");
}

#[test]
fn a_card_opens_downward_from_its_header_by_click_or_ctrl_o() {
    for by_click in [true, false] {
        let mut fixture = app();
        let app = &mut fixture.app;
        conversation(app);
        let session = app.session.as_mut().unwrap();
        let mut call = tool();
        call.state = ToolState::Running;
        call.duration = None;
        call.exit_code = None;
        call.output = (1..=12).map(|index| format!("item {index}")).collect();
        call.output_dropped = 0;
        session.transcript.push(Block::Tool(Box::new(call)));
        let folded = render(app, 70, 20);
        let header = |screen: &str| {
            let rows: Vec<&str> = screen.lines().collect();
            rows.iter().rposition(|row| row.starts_with(" ▎ bash"))
        };
        let before = header(&folded).expect("running card");
        if by_click {
            click(app, 10, before as u16);
        } else {
            app.on_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('o'),
                crossterm::event::KeyModifiers::CONTROL,
            ));
        }
        let expanded = render(app, 70, 20);
        // The first row carries the Ctrl+O flash.
        let top = |screen: &str| {
            screen
                .lines()
                .skip(1)
                .take(3)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(top(&expanded), top(&folded), "{expanded}");
        if by_click {
            let title = expanded.lines().nth(before + 1).unwrap_or_default();
            assert!(title.contains("$ cargo test --lib"), "{expanded}");
        }
    }
}

#[test]
fn collapsing_a_card_while_following_leaves_no_blank_rows_under_the_content() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 16);
    click(app, 10, row_of(&folded, " ▎ bash"));
    let open = render(app, 70, 16);
    assert!(open.contains(" ▎ $ cargo test --lib"), "{open}");
    // Following the longer transcript, the collapsing click pins a top that
    // the shorter one no longer reaches.
    app.scroll_to(usize::MAX);
    let open = render(app, 70, 16);
    click(app, 10, row_of(&open, " ▎ $ cargo test --lib"));
    let collapsed = render(app, 70, 16);
    let end = |screen: &str| row_of(screen, "   completed in 12s");
    assert_eq!(end(&collapsed), end(&folded), "{collapsed}");
    assert_eq!(app.scroll, Scroll::Follow);
    assert!(!collapsed.contains("Jump to latest"), "{collapsed}");
}

/// A prompt and its turn end, which `outcome` made a failure.
fn failed_turn(app: &mut App, outcome: serde_json::Value) {
    let transcript = &mut app.session.as_mut().expect("session").transcript;
    transcript.push(Block::User {
        text: "Summarise the changes".into(),
        kind: UserKind::Prompt,
        at: None,
    });
    transcript.push(Block::TurnEnd {
        loop_id: "loop_1".into(),
        outcome: serde_json::from_value(outcome).expect("outcome"),
        duration: Some(std::time::Duration::from_secs(47)),
        requests: 1,
        tools: 2,
        usage: Default::default(),
        changes: None,
    });
}

fn rate_limited() -> serde_json::Value {
    serde_json::json!({"type": "failed", "kind": "model", "model_error": {
        "kind": "rate_limited", "delivery": "not_started", "retry_after_millis": 12000,
        "provider": {"status": 429, "code": "rate_limit_exceeded", "type": "requests",
            "message": "Rate limit reached for gpt-6-luna in organization org-7Zq on tokens per min (TPM): Limit 30000, Used 29250, Requested 4012.\nPlease try again in 8.524s. Visit https://platform.example/account/rate-limits to learn more."}}})
}

#[test]
fn a_failed_turn_names_the_provider_status_the_hint_and_clips_a_long_message() {
    let mut app = app();
    failed_turn(&mut app, rate_limited());
    snapshot(
        "tests/snapshots/failed-provider.txt",
        include_str!("../../tests/snapshots/failed-provider.txt"),
        render(&mut app, 80, 16),
    );
}

#[test]
fn ctrl_o_opens_the_whole_provider_message_and_closes_it_again() {
    let mut app = app();
    failed_turn(&mut app, rate_limited());
    app.tools_expanded = true;
    let open = render(&mut app, 80, 16);
    snapshot(
        "tests/snapshots/failed-provider-expanded.txt",
        include_str!("../../tests/snapshots/failed-provider-expanded.txt"),
        open,
    );
    app.tools_expanded = false;
    let shut = render(&mut app, 80, 16);
    assert!(shut.contains("full message"), "{shut}");
    assert!(!shut.contains("to learn more"), "{shut}");
}

#[test]
fn a_failure_without_provider_details_shows_its_hint_and_nothing_quoted() {
    let mut app = app();
    failed_turn(
        &mut app,
        serde_json::json!({"type": "failed", "kind": "model", "model_error": {"kind": "timeout"}}),
    );
    failed_turn(
        &mut app,
        serde_json::json!({"type": "failed", "kind": "output_limit"}),
    );
    snapshot(
        "tests/snapshots/failed-plain.txt",
        include_str!("../../tests/snapshots/failed-plain.txt"),
        render(&mut app, 80, 18),
    );
}

#[test]
fn a_drag_over_a_failed_turn_takes_the_message_without_rails_hints_or_ellipsis() {
    let mut app = app();
    failed_turn(&mut app, rate_limited());
    let screen = render(&mut app, 80, 16);
    let first = row_of(&screen, "   failed");
    let drag = crate::app::selection::Drag {
        from: (0, first),
        to: (79, first + 6),
    };
    let text = app.selected_text(drag).expect("text");
    assert_eq!(
        text,
        "Rate limit reached for gpt-6-luna in organization org-7Zq on tokens per\n\
         min (TPM): Limit 30000, Used 29250, Requested 4012.\n\
         Please try again in 8.524s. Visit"
    );
}

#[test]
fn clicking_a_turn_end_line_opens_the_git_diff() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    click(app, 5, row_of(&screen, "   completed"));
    assert!(
        matches!(&app.overlay, Overlay::Diff(_)),
        "{:?}",
        app.overlay
    );
}

#[test]
fn a_drag_over_a_card_header_selects_instead_of_toggling() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut fixture = app();
    // The copy is queued on the runtime and never runs.
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let header = row_of(&screen, " ▎ bash");
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 2),
        (MouseEventKind::Drag(MouseButton::Left), 8),
        (MouseEventKind::Up(MouseButton::Left), 8),
    ] {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row: header,
            modifiers: KeyModifiers::NONE,
        });
    }
    let after = render(app, 70, 20);
    assert!(after.contains("ctrl+o to expand"), "still folded:\n{after}");
}

/// Feeds one Agent event through the reducer, as the live loop does.
fn emit(app: &mut App, kind: &str, mut data: serde_json::Value) {
    data["meta"] = serde_json::json!({"session_id": "ses_0123456789abcdef", "dropped_before": 0});
    let event = serde_json::from_value(serde_json::json!({"type": kind, "data": data})).unwrap();
    app.session.as_mut().unwrap().apply_event(event);
}

/// A live turn whose bash call is announced by a preview, the way the Agent does.
fn previewed_bash(app: &mut App) -> ToolRef {
    let turn = TurnRef::new("ses_0123456789abcdef", "loop_1");
    app.session.as_mut().unwrap().live = Some(crate::app::LiveTurn::new(turn.clone(), 0));
    emit(
        app,
        "tool_arguments_preview",
        serde_json::json!({
            "turn": turn, "request_index": 1, "tool_call_id": "call_1", "tool_name": "bash",
            "state": "generating", "display": {"detail": "$ cargo test --lib"},
        }),
    );
    tool().key
}

#[test]
fn a_running_card_without_output_shows_running_until_output_arrives() {
    let mut fixture = app();
    let app = &mut fixture.app;
    let key = previewed_bash(app);
    assert!(!render(app, 70, 30).contains("running…"));
    emit(
        app,
        "tool_started",
        serde_json::json!({
            "turn": TurnRef::new("ses_0123456789abcdef", "loop_1"), "request_index": 1,
            "tool_call_id": "call_1", "tool_name": "bash",
        }),
    );
    let folded = render(app, 70, 30);
    let header = row_of(&folded, " ▎ bash");
    assert_eq!(row_of(&folded, " ▎ running…"), header + 2);
    let drag = crate::app::selection::Drag {
        from: (1, header),
        to: (69, header + 2),
    };
    assert_eq!(
        app.selected_text(drag).as_deref(),
        Some("bash\n$ cargo test --lib")
    );

    click(app, 10, header + 2);
    let open = render(app, 70, 30);
    assert!(open.contains(" ▎ $ cargo test --lib"), "{open}");
    assert!(open.contains(" ▎ running…"), "{open}");
    click(app, 10, row_of(&open, " ▎ running…"));
    assert!(render(app, 70, 30).contains(" ▎ running…"));

    let session = app.session.as_mut().unwrap();
    session
        .transcript
        .find_tool_mut(&key)
        .unwrap()
        .push_output("Compiling\n");
    let streaming = render(app, 70, 30);
    assert!(
        streaming.contains("(1 more line, click or ctrl+o to expand)"),
        "{streaming}"
    );
    assert!(!streaming.contains("running…"), "{streaming}");

    let session = app.session.as_mut().unwrap();
    session
        .transcript
        .find_tool_mut(&key)
        .unwrap()
        .output
        .clear();
    emit(
        app,
        "tool_finished",
        serde_json::json!({
            "turn": TurnRef::new("ses_0123456789abcdef", "loop_1"), "request_index": 1,
            "tool_call_id": "call_1", "result": {"outcome": "success", "content_bytes": 0},
        }),
    );
    assert!(!render(app, 70, 30).contains("running…"));
}

#[test]
fn a_card_waiting_for_approval_does_not_say_running() {
    let mut fixture = app();
    let app = &mut fixture.app;
    let key = previewed_bash(app);
    emit(
        app,
        "tool_execution",
        serde_json::json!({
            "turn": TurnRef::new("ses_0123456789abcdef", "loop_1"),
            "data": {"tool_ref": key, "state": "awaiting_policy", "result_truncated": false},
        }),
    );
    let call = app.session.as_ref().unwrap().transcript.find_tool(&key);
    assert_eq!(call.unwrap().state, ToolState::Running);
    assert!(!render(app, 70, 20).contains("running…"));
}

#[test]
fn a_drag_across_a_card_takes_its_text_without_rails_or_hints() {
    let mut fixture = app();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let header = row_of(&screen, " ▎ bash");
    let drag = crate::app::selection::Drag {
        from: (1, header),
        to: (69, header + 2),
    };
    assert_eq!(
        app.selected_text(drag).as_deref(),
        Some("bash\n$ cargo test --lib")
    );
}

#[test]
fn the_gutter_column_stays_blank_and_ignores_clicks() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let area = app.layout.viewport.area();
    assert_eq!(area.x, 1);
    for line in screen.lines().take(area.height as usize) {
        assert!(!line.starts_with(|c: char| c != ' '), "{line:?}\n{screen}");
    }
    app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    let top = render(app, 70, 20);
    assert!(
        top.contains(concat!("minicore-tui v", env!("CARGO_PKG_VERSION"))),
        "{top}"
    );
    assert!(
        top.contains("esc interrupt · ctrl+c clear · / commands"),
        "{top}"
    );
    assert!(top.contains("/work/minicore-tui"), "{top}");
    app.scroll = Scroll::Follow;
    let header = row_of(&screen, " ▎ bash");
    click(app, 0, header);
    assert!(
        render(app, 70, 20).contains("ctrl+o to expand"),
        "still folded"
    );
}

/// A transcript whose block 0 is the card, so the welcome header sits in front of it.
fn card_first(app: &mut App) {
    let session = app.session.as_mut().expect("session");
    session.transcript.push(Block::Tool(Box::new(tool())));
}

#[test]
fn a_click_on_a_card_in_block_0_maps_below_the_welcome_header() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    card_first(app);
    let folded = render(app, 70, 30);
    click(app, 10, row_of(&folded, " ▎ bash"));
    let expanded = render(app, 70, 30);
    assert!(expanded.contains(" ▎ $ cargo test --lib"), "{expanded}");

    click(app, 10, row_of(&expanded, " ▎ … 14 earlier lines"));
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    let refolded = render(app, 70, 30);
    assert!(refolded.contains("ctrl+o to expand"), "{refolded}");
}

#[test]
fn a_click_on_a_welcome_header_row_does_nothing() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    card_first(app);
    let folded = render(app, 70, 30);
    let rows = app.layout.state.header_rows as u16;
    assert!(rows > 0, "the header is cached in front of block 0");
    for row in 0..rows {
        click(app, 10, row);
    }
    assert_eq!(render(app, 70, 30), folded);
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
}

fn left(
    app: &mut App,
    kind: fn(crossterm::event::MouseButton) -> crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
) {
    app.on_mouse(crossterm::event::MouseEvent {
        kind: kind(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
}

#[test]
fn a_double_click_selects_a_word_and_a_triple_click_the_row() {
    use crossterm::event::MouseEventKind::{Down, Up};
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let row = screen
        .lines()
        .position(|line| line.contains("re-renders only the tail"))
        .expect("answer row") as u16;
    let column = screen
        .lines()
        .nth(row as usize)
        .unwrap()
        .find("re-renders")
        .unwrap() as u16
        + 3;
    let selected = |app: &App| app.selection.and_then(|drag| app.selected_text(drag));
    for _ in 0..2 {
        left(app, Down, column, row);
        left(app, Up, column, row);
    }
    assert_eq!(selected(app).as_deref(), Some("re-renders"));
    left(app, Down, column, row);
    left(app, Up, column, row);
    assert_eq!(
        selected(app).as_deref(),
        Some("prefix and re-renders only the tail, so a delta costs one small"),
        "the row without its indent"
    );
    left(app, Down, column, row);
    assert_eq!(app.selection, None, "a fourth press starts over");
}

#[test]
fn a_detached_view_shows_the_jump_label_and_clicking_it_follows() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    conversation(app);
    let screen = render(app, 70, 20);
    assert!(!screen.contains("Jump to latest"), "following:\n{screen}");
    app.scroll_by(-3);
    let screen = render(app, 70, 20);
    let area = app.layout.viewport.area();
    let last = screen.lines().nth(area.bottom() as usize - 1).unwrap();
    assert!(last.contains("↓ Jump to latest message · End"), "{screen}");
    let column = last.find("Jump").unwrap() as u16;
    click(app, column, area.bottom() - 1);
    assert_eq!(app.scroll, Scroll::Follow);
}

#[test]
fn the_scrollbar_shows_after_a_scroll_and_its_track_jumps() {
    use crossterm::event::MouseEventKind::{Down, Drag, Up};
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    for _ in 0..3 {
        conversation(app);
    }
    let screen = render(app, 70, 20);
    assert!(!screen.contains('┃'), "hidden until a scroll:\n{screen}");
    app.scroll_by(-1);
    let screen = render(app, 70, 20);
    let area = app.layout.viewport.area();
    let bar: String = screen
        .lines()
        .take(area.height as usize)
        .map(|line| line.chars().nth(69).unwrap_or(' '))
        .collect();
    assert!(
        bar.ends_with('┃') && bar.starts_with('│'),
        "thumb at the bottom: {bar:?}"
    );
    left(app, Down, 69, area.y);
    let view = app.layout.viewport;
    let (_, len) =
        crate::app::scrollbar::thumb(view.height, app.layout.total_rows(), view.top).unwrap();
    assert_eq!(
        app.scrollbar_grab,
        Some(len / 2),
        "a track press grabs the thumb's centre"
    );
    render(app, 70, 20);
    assert_eq!(app.layout.viewport.top, 0, "and jumps it to the top");
    left(app, Drag, 69, area.bottom() - 1);
    left(app, Up, 69, area.bottom() - 1);
    assert_eq!(app.scroll, Scroll::Follow, "dragged to the end");
    assert_eq!(app.selection, None, "the bar never selects");
}
/// The approval sheet says what would run, from the card, and the status line
/// says what Esc does while it waits.
#[test]
fn an_approval_shows_the_tool_its_target_and_its_input() {
    let mut app = app();
    let session = app.session.as_mut().expect("session");
    let turn = TurnRef {
        session_id: "ses_0123456789abcdef".into(),
        loop_id: "loop_9".into(),
    };
    session.live = Some(crate::app::LiveTurn::new(turn.clone(), 0));
    let mut call = ToolCall::new(
        ToolRef {
            session_id: "ses_0123456789abcdef".into(),
            loop_id: "loop_9".into(),
            request_index: 0,
            tool_call_id: "call_w".into(),
        },
        "write",
    );
    call.detail = "notes/approval.txt".into();
    call.expanded_input = Some("Approval fixture executed.\nsecond line".into());
    session.transcript.push(Block::Tool(Box::new(call)));
    session.interaction = Some(crate::app::PendingInteraction {
        turn,
        interaction_id: "i1".into(),
        tool_call_id: "call_w".into(),
        tool_name: "write".into(),
        prompt: "Allow tool `write` for this call?".into(),
        kind: crate::app::InteractionKind::Approval,
        selected: 0,
    });
    let rendered = render(&mut app, 80, 24);
    assert!(rendered.contains("write  notes/approval.txt"), "{rendered}");
    assert!(
        rendered.contains("  Approval fixture executed."),
        "{rendered}"
    );
    assert!(rendered.contains("Allow once"), "{rendered}");
    assert!(
        rendered.contains("Waiting for approval") && rendered.contains("esc to deny"),
        "{rendered}"
    );
}

/// Remote control listening at a fixed address with a fixed password.
fn listening(app: &mut App) {
    let dir = tempfile::tempdir().expect("dir");
    let mut auth = crate::remote::auth::Auth::load(dir.path().join("remote.json")).expect("auth");
    auth.set_password(Some("k7mq-x2pa-n9dt")).expect("password");
    app.remote.auth = Some(std::sync::Arc::new(std::sync::Mutex::new(auth)));
    app.remote.url = Some("http://192.168.20.207:8765".into());
    app.remote.listening = vec![
        "0.0.0.0:8765".parse().expect("address"),
        "[::]:8765".parse().expect("address"),
    ];
}

/// Admits phone `name`, connected since `at` (UTC).
fn admit(app: &mut App, name: &str, at: &str) {
    let (outbound, _output) = mpsc::channel(1);
    let (close, _closing) = tokio::sync::watch::channel(None);
    app.remote.phones.push(crate::remote::Phone {
        connection: crate::remote::Connection {
            id: app.remote.phones.len() as u64 + 1,
            generation: 0,
            device: crate::remote::auth::Device {
                name: name.into(),
                token_sha256: name.into(),
                paired_at: String::new(),
            },
            outbound,
            close,
        },
        view: crate::remote::PhoneView::foreground(app),
        projection: Default::default(),
        connected_at: at.parse().expect("time"),
        queries: 0,
    });
}

/// A phone connected since 14:02.
fn connected(app: &mut App) {
    listening(app);
    admit(app, "iPhone Safari", "2026-10-25T14:02:00Z");
}

/// A turn running `bash` in the resident session, its spinner on its first frame.
fn running_bash(app: &mut App) {
    let session = app.session.as_mut().expect("session");
    let mut live = crate::app::LiveTurn::new(
        TurnRef {
            session_id: "ses_0123456789abcdef".into(),
            loop_id: "loop_2".into(),
        },
        session.transcript.len(),
    );
    live.running_tool = Some(("bash".into(), "cargo test --workspace".into()));
    session.live = Some(live);
    app.started = std::time::Instant::now();
}

#[test]
fn the_remote_status_screen_waits_for_a_phone_on_a_draft() {
    let mut app = app();
    app.session = None;
    app.profiles = vec![
        serde_json::from_value(
            serde_json::json!({"id": "coding", "model": "gpt-6-luna", "reasoning": "max"}),
        )
        .expect("profile"),
    ];
    app.default_profile = Some("coding".into());
    listening(&mut app);
    snapshot(
        "tests/snapshots/remote-waiting.txt",
        include_str!("../../tests/snapshots/remote-waiting.txt"),
        render(&mut app, 80, 24),
    );
}

#[test]
fn the_remote_status_screen_shows_the_phone_the_work_the_queue_and_refused_sign_ins() {
    let mut app = app();
    conversation(&mut app);
    connected(&mut app);
    running_bash(&mut app);
    let session = app.session.as_mut().expect("session");
    for text in ["then update the docs", "and the changelog"] {
        session.queue.push(crate::app::Queued {
            text: text.into(),
            kind: crate::app::QueuedKind::Follow,
            sent: false,
            at: None,
        });
    }
    app.remote.failed_logins = 3;
    app.remote.paused_until =
        Some(tokio::time::Instant::now() + std::time::Duration::from_secs(30));
    snapshot(
        "tests/snapshots/remote-busy.txt",
        include_str!("../../tests/snapshots/remote-busy.txt"),
        render(&mut app, 80, 24),
    );
}

#[test]
fn the_remote_status_screen_marks_listening_addresses_served_over_tls() {
    let mut app = app();
    listening(&mut app);
    app.remote.tls = true;
    let screen = render(&mut app, 80, 24);
    assert!(
        screen
            .lines()
            .any(|line| line == "    Listening  0.0.0.0:8765 · [::]:8765 · TLS"),
        "{screen}"
    );
}

#[test]
fn a_narrow_remote_status_screen_cuts_values_and_keeps_labels() {
    let mut app = app();
    conversation(&mut app);
    listening(&mut app);
    let screen = render(&mut app, 44, 24);
    for label in [
        "Address",
        "Listening",
        "Password",
        "Phones",
        "Session",
        "Workspace",
        "Model",
        "Now",
    ] {
        assert!(
            screen
                .lines()
                .any(|line| line.starts_with(&format!("    {label}"))),
            "{label}:\n{screen}"
        );
    }
    let phone = screen
        .lines()
        .find(|line| line.contains("Phones"))
        .expect("phone row");
    assert!(
        phone.contains("none yet · open the") && phone.ends_with('…'),
        "{screen}"
    );
}

#[test]
fn the_remote_status_screen_and_footer_count_several_phones_and_list_each() {
    let mut app = app();
    conversation(&mut app);
    connected(&mut app);
    admit(&mut app, "Mac Edge", "2026-10-25T14:30:00Z");
    let draft = crate::remote::PhoneView::Draft(crate::remote::DraftSettings {
        workspace: app.workspace.clone(),
        launch: Default::default(),
    });
    app.remote.phones[1].set_view(draft);
    let screen = render(&mut app, 100, 24);
    assert!(screen.contains("● 2 phones connected"), "{screen}");
    assert!(
        screen.contains("iPhone Safari · Cache the parser · connected at 14:02\n               Mac Edge · new session · connected at 14:30\n"),
        "{screen}"
    );
    assert!(screen.contains("remote · 2 phones"), "{screen}");
}

#[test]
fn a_remote_approval_waits_on_the_phone() {
    let mut app = app();
    listening(&mut app);
    let session = app.session.as_mut().expect("session");
    let mut call = tool();
    call.key.tool_call_id = "call_rm".into();
    call.detail = "rm -rf build".into();
    call.state = ToolState::Pending;
    session.transcript.push(Block::Tool(Box::new(call)));
    session.interaction = Some(crate::app::PendingInteraction {
        turn: TurnRef {
            session_id: "ses_0123456789abcdef".into(),
            loop_id: "loop_9".into(),
        },
        interaction_id: "i1".into(),
        tool_call_id: "call_rm".into(),
        tool_name: "bash".into(),
        prompt: "Allow tool `bash` for this call?".into(),
        kind: crate::app::InteractionKind::Approval,
        selected: 0,
    });
    let screen = render(&mut app, 80, 24);
    assert!(
        screen.contains("Waiting for approval on the phone · bash · rm -rf build"),
        "{screen}"
    );
    assert!(!screen.contains("Allow once"), "{screen}");
    assert!(!screen.contains("esc to deny"), "{screen}");
}

#[test]
fn an_idle_remote_status_screen_reports_the_last_turn() {
    let mut app = app();
    conversation(&mut app);
    listening(&mut app);
    let screen = render(&mut app, 80, 24);
    assert!(
        screen.contains("Now        Idle · last turn completed in 12s"),
        "{screen}"
    );
}

#[test]
fn the_remote_status_screen_counts_busy_background_sessions() {
    let mut app = app();
    listening(&mut app);
    let other = SessionInfo {
        session_id: "ses_other".into(),
        ..info()
    };
    let mut session = Session::new(other, 2);
    session.live = Some(crate::app::LiveTurn::new(
        TurnRef {
            session_id: "ses_other".into(),
            loop_id: "loop_1".into(),
        },
        0,
    ));
    app.background.push(session);
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains("    Others     1 running"), "{screen}");
}

#[test]
fn a_full_screen_view_draws_over_the_remote_status_screen() {
    let mut app = app();
    listening(&mut app);
    app.overlay = Overlay::Help { scroll: 0 };
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains("← Help"), "{screen}");
    assert!(!screen.contains("Password"), "{screen}");
}

#[test]
fn remote_control_hides_the_transcript_until_it_is_turned_off() {
    let mut app = app();
    conversation(&mut app);
    listening(&mut app);
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains("Remote control"), "{screen}");
    assert!(!screen.contains("re-parse the whole message"), "{screen}");
    app.remote.stop("remote_off");
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains("re-parse the whole message"), "{screen}");
    assert!(!screen.contains("Password"), "{screen}");
}

#[test]
fn the_remote_footer_mark_gives_way_before_the_rest() {
    let mut app = app();
    conversation(&mut app);
    listening(&mut app);
    let footer = |app: &mut App, width| {
        let screen = render(app, width, 20);
        screen.lines().last().expect("footer").to_owned()
    };
    assert!(footer(&mut app, 80).contains("remote"), "waiting is marked");
    let narrow = footer(&mut app, 30);
    assert!(!narrow.contains("remote"), "{narrow}");
    assert!(
        narrow.contains("max"),
        "the reasoning level stays: {narrow}"
    );
}

#[test]
fn the_sessions_picker_shows_what_live_sessions_are_doing() {
    let mut app = app();
    let catalog = |id: &str, title: &str| SessionInfo {
        session_id: id.into(),
        title: Some(title.into()),
        updated_at: String::new(),
        ..info()
    };
    let turn = |id: &str| TurnRef {
        session_id: id.into(),
        loop_id: "loop_1".into(),
    };
    let mut running = Session::new(catalog("ses_running", "Port the lexer"), 2);
    running.live = Some(crate::app::LiveTurn::new(turn("ses_running"), 0));
    let mut asking = Session::new(catalog("ses_asking", "Clean the build"), 3);
    asking.live = Some(crate::app::LiveTurn::new(turn("ses_asking"), 0));
    asking.interaction = Some(crate::app::PendingInteraction {
        turn: turn("ses_asking"),
        interaction_id: "i1".into(),
        tool_call_id: "call_1".into(),
        tool_name: "bash".into(),
        prompt: "Allow tool `bash` for this call?".into(),
        kind: crate::app::InteractionKind::Approval,
        selected: 0,
    });
    let mut done = Session::new(catalog("ses_done", "Write the docs"), 4);
    done.unseen = true;
    let mut failed = Session::new(catalog("ses_failed", "Fix the flaky test"), 5);
    failed.unseen = true;
    failed.transcript.push(Block::TurnEnd {
        loop_id: "loop_1".into(),
        outcome: LoopOutcome::Failed {
            kind: "model".into(),
            model_error: None,
        },
        duration: None,
        requests: 1,
        tools: 0,
        usage: Default::default(),
        changes: None,
    });
    app.sessions = vec![
        catalog("ses_old", "An old session"),
        done.info.clone(),
        failed.info.clone(),
        asking.info.clone(),
        running.info.clone(),
        info(),
    ];
    app.background = vec![running, asking, done, failed];
    app.open_panel(crate::app::Panel::Sessions);
    snapshot(
        "tests/snapshots/sessions-picker-live.txt",
        include_str!("../../tests/snapshots/sessions-picker-live.txt"),
        render(&mut app, 80, 20),
    );
}
