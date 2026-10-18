//! Full-screen render tests. Each one builds an `App`, draws it into a
//! `TestBackend` and compares the text of the frame against a checked-in
//! snapshot, so a visual regression shows up as a diff instead of a screenshot.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use crate::app::keys::Input;
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
    let mut session = Session::new(info());
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
    call.output_lines = Some(16);
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
            added: 40,
            removed: 7,
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
/// reviewed with the code that produces it; `cargo test` diffs against it.
const IDLE: &str = include_str!("../../tests/snapshots/idle.txt");
const STREAMING: &str = include_str!("../../tests/snapshots/streaming.txt");

fn snapshot(path: &str, expected: &str, rendered: String) {
    let _ = path;
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
    app.composer = (0..20)
        .map(|index| format!("line {index} of a long draft"))
        .collect::<Vec<_>>()
        .join("\n");
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
    app.composer = "hello".into();
    let rendered = render(&mut app, 80, 12);
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(footer.contains("minicore-tui@-"), "{footer}");
    assert!(footer.contains("gpt-6-luna"), "{footer}");
    assert!(footer.contains("max"), "{footer}");
    assert!(footer.contains("● ready"), "{footer}");
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
        assert_eq!(
            app.on_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )),
            Input::Edited
        );
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
    app.composer = "draft".into();
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
