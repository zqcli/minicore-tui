//! Full-screen render tests. Each one builds an `App`, draws it into a
//! `TestBackend` and compares the text of the frame against a checked-in
//! snapshot, so a visual regression shows up as a diff instead of a screenshot.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use crate::app::{App, Msg, Overlay, Scroll, Session};
use crate::protocol::read::UserKind;
use crate::protocol::{LoopOutcome, Reasoning, SessionInfo, ToolRef, TurnRef};
use crate::theme::{Palette, ThemeKind};
use crate::transcript::block::{ToolState, Transcript};
use crate::transcript::{Block, ChangeSummary, RequestKey, ToolCall};

/// A render fixture. The client needs a child process to exist, so it points at
/// `/bin/sleep`: the child exits at once with an argument error, which a render
/// never observes. Nothing is written, so parallel tests cannot race.
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
    let client = runtime
        .block_on(async {
            crate::rpc::Client::spawn(
                std::path::Path::new("/bin/sleep"),
                std::path::Path::new("/dev/null"),
                incoming_tx,
            )
        })
        .expect("client");
    let mut app = App::new(client, tx, PathBuf::from("/work/minicore-tui"));
    app.palette = Palette::for_kind(ThemeKind::Dark);
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
        created_at: String::new(),
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
    });
    transcript.push(Block::Assistant {
        key: RequestKey {
            loop_id: "loop_1".into(),
            request_index: 0,
        },
        reasoning: "The layout cache only keyed on the block revision...".into(),
        text: "It does not any more. The **streaming renderer** keeps the stable prefix and\nre-renders only the tail, so a delta costs one small parse.".into(),
        streaming: false,
    });
    transcript.push(Block::Tool(Box::new(tool())));
    transcript.push(Block::TurnEnd {
        loop_id: "loop_1".into(),
        outcome: LoopOutcome::Completed,
        duration: Some(std::time::Duration::from_secs(12)),
        requests: 2,
        tool_rounds: 1,
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
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .expect("draw");
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

/// The checked-in frame. Snapshots live in the source so the expected output is
/// reviewed with the code that produces it; `cargo test` diffs against it, and
/// `UPDATE_SNAPSHOTS=1 cargo test` rewrites it.
const IDLE: &str = include_str!("../../tests/snapshots/idle.txt");
const STREAMING: &str = include_str!("../../tests/snapshots/streaming.txt");

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
fn the_screen_shows_earlier_content_while_streaming() {
    let mut app = app();
    conversation(&mut app);
    let session = app.session.as_mut().expect("session");
    session.transcript.push(Block::User {
        text: "Now stream a longer answer.".into(),
        kind: UserKind::Prompt,
    });
    session.transcript.push(Block::Assistant {
        key: RequestKey {
            loop_id: "loop_2".into(),
            request_index: 0,
        },
        reasoning: "I should explain the anchor rule first, then show it.".into(),
        text: "An anchored view keeps its block and row, so\nnew output below never moves the text you\nare reading. Only scrolling to the end returns\nto following.".into(),
        streaming: true,
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
    });
    let after = render(&mut app, 70, 20);
    let kept: Vec<&str> = before.lines().take(10).collect();
    assert!(
        kept.iter()
            .all(|line| after.lines().any(|new| new == *line)),
        "the anchored top must not be replaced:\n{before}\n---\n{after}"
    );
    assert!(
        after.contains("↓ new output"),
        "the pill marks content below an anchored view:\n{after}"
    );
}

#[test]
fn the_composer_grows_with_its_content_and_caps_at_ten_rows() {
    let mut app = app();
    app.composer.set_text(
        &(0..20)
            .map(|index| format!("line {index} of a long draft"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let rendered = render(&mut app, 60, 30);
    let composer_rows = rendered
        .lines()
        .filter(|line| line.starts_with('▎'))
        .count();
    assert_eq!(
        composer_rows, 10,
        "the composer caps at ten rows:\n{rendered}"
    );
}

#[test]
fn the_footer_reports_the_state_and_context() {
    let mut app = app();
    app.composer.set_text("hello");
    let rendered = render(&mut app, 80, 12);
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(footer.starts_with("minicore-tui · "), "{footer}");
    assert!(
        !footer.contains('@'),
        "an unknown branch is left out: {footer}"
    );
    assert!(footer.contains("gpt-6-luna"), "{footer}");
    assert!(footer.contains("max"), "{footer}");
    assert!(footer.contains("● ready"), "{footer}");
    assert!(footer.ends_with("ctx ?"), "{footer}");
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
    assert!(footer.starts_with("minicore-tui@main · "), "{footer}");
    assert!(footer.contains("Cache the parser"), "{footer}");
    assert!(
        footer.ends_with("↑50.5k ↓1.8k R33.8k · ctx 41%"),
        "{footer}"
    );
    assert_eq!(crate::ui::wrap::width(footer), 100, "flush right: {footer}");
    // On a narrow terminal the title gives way first.
    let narrow = render(&mut app, 70, 12);
    let footer = narrow.lines().last().unwrap_or_default();
    assert!(!footer.contains("Cache the parser"), "{footer}");
    assert!(footer.ends_with("ctx 41%"), "{footer}");
}

/// The last transcript row never touches the stack below it, and the
/// composer's tint spans the whole row like `reference.png`.
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
            });
    }
    let backend = TestBackend::new(50, 14);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let row = |y: u16| -> String {
        (0..50)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    let composer = (0..14)
        .find(|y| row(*y).starts_with("▎ Ask anything"))
        .expect("composer row");
    assert_eq!(row(composer - 1), "", "the gap row above the stack");
    assert!(
        row(composer - 2).contains("prompt 39"),
        "{}",
        row(composer - 2)
    );
    assert_eq!(
        buffer[(49u16, composer)].bg,
        app.palette.composer_bg,
        "the band reaches the last column"
    );
}

#[test]
fn the_status_spinner_moves_with_time() {
    use std::time::Duration;
    let frames: std::collections::HashSet<&str> = (0..10)
        .map(|step| crate::ui::status::spinner(Duration::from_millis(step * 100)))
        .collect();
    assert_eq!(frames.len(), 10, "every tick shows the next frame");
    assert_eq!(
        crate::ui::status::spinner(Duration::from_millis(1_000)),
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
    app.palette = Palette::plain();
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
            crate::ui::wrap::width(line) <= 30,
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
    assert!(rendered.contains("▎ hello"), "{rendered}");
    assert!(
        !rendered.contains("Ask anything"),
        "the placeholder is gone once there is a draft:\n{rendered}"
    );
}

#[test]
fn an_open_overlay_is_not_drawn_over_the_composer() {
    let mut app = app();
    app.composer.set_text("draft");
    app.overlay = Overlay::Help;
    let rendered = render(&mut app, 40, 10);
    assert!(
        rendered.contains("▎ draft"),
        "the composer stays visible under a view:\n{rendered}"
    );
}

#[test]
fn a_queued_steer_is_shown_above_the_composer() {
    let mut app = app();
    app.session
        .as_mut()
        .expect("session")
        .queue
        .push(crate::app::Queued {
            text: "focus on the parser first".into(),
            kind: crate::app::QueuedKind::Steer,
        });
    let rendered = render(&mut app, 60, 12);
    assert!(
        rendered.contains("↳ steer · focus on the parser first"),
        "{rendered}"
    );
}

#[test]
fn a_running_turn_shows_the_status_line_with_elapsed_time() {
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
    assert!(rendered.contains("Thinking"), "{rendered}");
    assert!(rendered.contains("esc to interrupt"), "{rendered}");
    assert!(rendered.contains("● working"), "{rendered}");
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
fn clicking_a_card_header_toggles_that_card_and_its_hint_opens_the_tool_view() {
    let mut fixture = app();
    // Opening a view starts a read; it only needs a runtime to be queued on.
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 20);
    let header = row_of(&folded, "▎ ✓ bash");
    click(app, 10, header);
    let expanded = render(app, 70, 20);
    assert!(
        expanded.contains("… 14 earlier lines · click for full output"),
        "{expanded}"
    );
    assert!(!app.tools_expanded, "only that card changed");

    click(app, 10, row_of(&expanded, "▎   … 14 earlier lines"));
    assert!(matches!(app.overlay, Overlay::Tool(_)), "{:?}", app.overlay);

    app.overlay = Overlay::None;
    render(app, 70, 20);
    click(app, 10, header);
    let refolded = render(app, 70, 20);
    assert!(refolded.contains("ctrl+o to expand"), "{refolded}");
}

#[test]
fn clicking_a_turn_end_line_opens_the_turn_diff() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    click(app, 5, row_of(&screen, "── completed"));
    assert!(
        matches!(
            app.overlay,
            Overlay::Diff {
                scope: crate::app::DiffScope::Turn,
                ..
            }
        ),
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
    let header = row_of(&screen, "▎ ✓ bash");
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
