use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use serde_json::json;

use super::harness::*;
use crate::app::{Action, Overlay, Scroll};
use crate::protocol::ToolRef;
use crate::protocol::read::UserKind;
use crate::transcript::{Block, NoticeLevel, RequestKey, ToolCall};

/// `count` prompts, each followed by a three-line answer.
fn conversation(h: &mut Harness, count: usize) {
    let transcript = &mut h.app.session.as_mut().unwrap().transcript;
    for index in 0..count {
        transcript.push(Block::User {
            text: format!("prompt {index}"),
            kind: UserKind::Prompt,
        });
        transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: format!("l{index}"),
                request_index: 0,
            },
            reasoning: String::new(),
            text: format!("answer {index}\n\nline two\n\nline three"),
            streaming: false,
        });
    }
}

/// The first transcript row with text; an anchored block may start with the
/// blank row that separates it from the one above.
fn first_row(h: &mut Harness) -> String {
    let screen = h.screen(60, 24);
    // The scrollbar fills the right edge for a moment after a scroll.
    let bare = |row: &str| row.trim_end_matches(['│', '┃', ' ']).to_owned();
    screen
        .lines()
        .map(bare)
        .find(|row| !row.is_empty())
        .unwrap_or_default()
}

#[tokio::test(start_paused = true)]
async fn an_approval_prompt_owns_the_keys_and_answers_over_the_wire() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.fake.script("interaction.answer", json!({"ok": true}));
    h.submit("write a file");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "interaction_requested",
        json!({"turn": turn("l1"), "interaction": {
            "interaction_id": "i1", "tool_call_id": "c1", "tool_name": "bash",
            "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}}}),
    )
    .await;
    h.until(|app| app.has_interaction()).await;
    let screen = h.screen(80, 20);
    for text in [
        "Allow bash?",
        "Allow once",
        "Deny",
        "Waiting for approval",
        "esc to deny",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }

    h.type_text("oops");
    h.ctrl(KeyCode::Char('j'));
    h.app.on_paste("a whole pasted prompt");
    assert_eq!(
        h.app.composer.text(),
        "",
        "the draft behind the sheet stays empty"
    );

    h.press(KeyCode::Down);
    h.press(KeyCode::Enter);
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["interaction_id"], "i1");
    assert_eq!(answered[0]["answer"]["decision"], "deny");
    assert!(!h.app.has_interaction());
}

#[tokio::test(start_paused = true)]
async fn follow_ups_queue_behind_a_running_turn_and_alt_up_recalls_the_newest() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));

    h.app.composer.set_text("first line");
    h.key(KeyCode::Enter, KeyModifiers::SHIFT);
    h.type_text("xy");
    assert_eq!(h.app.composer.text(), "first line\nxy");
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.app.composer.text(), "");

    for text in ["one", "two"] {
        h.app.composer.set_text(text);
        h.key(KeyCode::Enter, KeyModifiers::ALT);
    }
    assert_eq!(h.queue(), ["one", "two"]);

    h.app.composer.set_text("half");
    h.key(KeyCode::Up, KeyModifiers::ALT);
    assert_eq!(
        h.app.composer.text(),
        "half",
        "a draft is never overwritten"
    );
    assert_eq!(h.queue(), ["one", "two"]);
    assert_eq!(
        h.notices(),
        ["Clear the draft before recalling a queued message"]
    );

    h.ctrl(KeyCode::Char('c'));
    h.key(KeyCode::Up, KeyModifiers::ALT);
    assert_eq!(h.app.composer.text(), "two");
    assert_eq!(h.queue(), ["one"]);
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_clears_a_draft_then_needs_two_presses_on_an_empty_composer() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("x");
    h.ctrl(KeyCode::Char('c'));
    assert_eq!(h.app.composer.text(), "");
    assert!(!h.app.quit);

    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "the first empty press only arms the quit");
    assert_eq!(h.notices(), ["Press Ctrl+C again to quit"]);
    h.type_text("x");
    h.press(KeyCode::Backspace);
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "typing disarmed the quit");
    h.ctrl(KeyCode::Char('c'));
    assert!(h.app.quit);
}

#[tokio::test(start_paused = true)]
async fn the_palette_delivers_the_typed_argument_to_the_accepted_command() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut updated = session_info("s1");
    updated["reasoning"] = json!("max");
    h.fake.script("session.update", json!({"session": updated}));

    h.type_text("/rename a better title");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("session.rename")[0]["title"], "a better title");
    assert_eq!(h.app.composer.text(), "");

    // `/res` lists resume before reasoning; the argument belongs to the row
    // the user accepts.
    h.type_text("/res max");
    let screen = h.screen(80, 24);
    let resume = screen.find("resume").expect("resume row");
    let reasoning = screen.find("reasoning").expect("reasoning row");
    assert!(resume < reasoning, "{screen}");
    h.press(KeyCode::Down);
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("session.update")[0]["reasoning"], "max");
    assert_eq!(h.app.composer.text(), "");
    assert!(h.app.overlay.is_none(), "{:?}", h.app.overlay);
}

#[tokio::test(start_paused = true)]
async fn a_slash_line_that_names_no_command_is_a_prompt() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.fake
        .script("turn.steer", json!({"ok": true, "steer_index": 0}));

    h.type_text("/usr/bin/env is broken");
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.sends(), ["/usr/bin/env is broken"]);
    assert!(h.app.overlay.is_none());
    assert!(h.notices().is_empty());

    // Without the palette (a recalled or edited draft) the line still goes out.
    h.app.composer.set_text("/var/log is huge");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("turn.steer")[0]["text"], "/var/log is huge");
}

#[tokio::test(start_paused = true)]
async fn at_opens_the_file_picker_only_at_the_start_of_a_word() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("mail me@example.com");
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "mail me@example.com");

    h.type_text(" @");
    assert!(matches!(h.app.overlay, Overlay::Files { .. }));
    h.press(KeyCode::Esc);
    h.app.composer.clear();
    h.type_text("@");
    assert!(matches!(h.app.overlay, Overlay::Files { .. }));
}

#[tokio::test(start_paused = true)]
async fn ctrl_and_alt_move_and_delete_by_word() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        h.app.composer.set_text("one two three");
        h.key(KeyCode::Left, modifier);
        h.key(KeyCode::Left, modifier);
        h.type_text("[");
        h.key(KeyCode::Right, modifier);
        h.type_text("]");
        assert_eq!(h.app.composer.text(), "one [two ]three");

        h.press(KeyCode::End);
        h.key(KeyCode::Backspace, modifier);
        assert_eq!(h.app.composer.text(), "one [two ");
    }
    h.app.composer.set_text("one two");
    h.ctrl(KeyCode::Char('h'));
    assert_eq!(h.app.composer.text(), "one ", "legacy terminals send ^H");
    h.press(KeyCode::Backspace);
    assert_eq!(h.app.composer.text(), "one");
}

fn wheel(h: &mut Harness, kind: MouseEventKind) {
    h.app.on_mouse(MouseEvent {
        kind,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
}

#[tokio::test(start_paused = true)]
async fn the_wheel_scrolls_the_open_overlay_never_the_hidden_transcript() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);

    h.press(KeyCode::F(1));
    wheel(&mut h, MouseEventKind::ScrollDown);
    assert!(matches!(h.app.overlay, Overlay::Help { scroll: 3 }));
    wheel(&mut h, MouseEventKind::ScrollUp);
    assert!(matches!(h.app.overlay, Overlay::Help { scroll: 0 }));
    h.press(KeyCode::Esc);

    h.type_text("/");
    wheel(&mut h, MouseEventKind::ScrollDown);
    assert!(matches!(
        h.app.overlay,
        Overlay::Palette { selected: 1, .. }
    ));
    h.press(KeyCode::Esc);

    h.type_text("/context");
    h.press(KeyCode::Enter);
    wheel(&mut h, MouseEventKind::ScrollUp);
    assert!(matches!(h.app.overlay, Overlay::Context));
    assert_eq!(h.app.scroll, Scroll::Follow, "the transcript did not move");

    h.press(KeyCode::Esc);
    wheel(&mut h, MouseEventKind::ScrollUp);
    assert!(matches!(h.app.scroll, Scroll::Anchor { .. }));
}

#[tokio::test(start_paused = true)]
async fn escape_closes_a_picker_and_takes_only_its_token() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "workspace.files",
        json!({"entries": [{"path": "src/main.rs", "kind": "file"},
                           {"path": "README.md", "kind": "file"}],
               "truncated": false, "scan_complete": true}),
    );
    h.type_text("/di");
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "", "the lone command goes with it");
    h.type_text("/");
    assert_eq!(h.app.composer.text(), "/", "a fresh command, not `//`");
    h.press(KeyCode::Esc);

    h.app.composer.set_text("fix the bug in ");
    h.type_text("@ap");
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "fix the bug in ");

    h.type_text("@zzz");
    h.settle().await;
    h.press(KeyCode::Enter);
    assert!(!h.app.overlay.is_none(), "no match keeps the picker open");
    h.press(KeyCode::Esc);
    h.type_text("@main");
    h.settle().await;
    h.press(KeyCode::Enter);
    assert_eq!(h.app.composer.text(), "fix the bug in @src/main.rs");
}

#[tokio::test(start_paused = true)]
async fn paging_keeps_four_rows_and_ctrl_arrows_jump_between_prompts() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    let before = h.screen(60, 24);
    let rows = h.viewport();
    assert!(before.contains("line three"), "{before}");

    h.press(KeyCode::PageUp);
    let after = h.screen(60, 24);
    let bare = |row: &str| row.trim_end_matches(['│', '┃', ' ']).to_owned();
    let old: Vec<String> = before.lines().take(rows).map(bare).collect();
    let new: Vec<String> = after.lines().take(rows).map(bare).collect();
    assert_eq!(
        new[rows - 4],
        old[0],
        "four rows of the last page stay:\n{after}"
    );
    h.press(KeyCode::PageDown);
    assert_eq!(h.app.scroll, Scroll::Follow);

    h.ctrl(KeyCode::Up);
    let top = first_row(&mut h);
    let index: usize = top
        .rsplit(' ')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("a prompt at the top: {top:?}"));
    h.ctrl(KeyCode::Up);
    assert!(first_row(&mut h).ends_with(&format!("prompt {}", index - 1)));
    h.ctrl(KeyCode::Down);
    assert!(first_row(&mut h).ends_with(&format!("prompt {index}")));
    for _ in index..12 {
        h.ctrl(KeyCode::Down);
    }
    assert_eq!(
        h.app.scroll,
        Scroll::Follow,
        "past the last prompt the view follows"
    );

    h.ctrl(KeyCode::Home);
    assert!(
        first_row(&mut h).ends_with("minicore-tui v0.4.0"),
        "the header tops the transcript"
    );
    assert!(h.screen(60, 24).contains("prompt 0"));
    h.ctrl(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow);
    h.press(KeyCode::Home);
    assert!(
        first_row(&mut h).ends_with("minicore-tui v0.4.0"),
        "home with no draft goes to the top"
    );
    h.press(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow, "end follows the tail again");
}

#[tokio::test(start_paused = true)]
async fn the_find_bar_moves_the_view_with_every_step() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    h.ctrl(KeyCode::Char('f'));
    h.type_text("prompt 1");
    let screen = h.screen(60, 24);
    assert!(screen.contains("1 of 3"), "prompt 1, 10 and 11:\n{screen}");
    assert!(
        first_row(&mut h).ends_with("prompt 1"),
        "typing shows match 1"
    );
    h.press(KeyCode::Down);
    assert!(first_row(&mut h).ends_with("prompt 10"));
    h.press(KeyCode::Tab);
    assert!(first_row(&mut h).ends_with("prompt 11"));
    h.press(KeyCode::Up);
    assert!(first_row(&mut h).ends_with("prompt 10"));
    h.press(KeyCode::Enter);
    assert!(first_row(&mut h).ends_with("prompt 11"));
}

#[tokio::test(start_paused = true)]
async fn errors_stay_until_a_key_and_info_notices_expire() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.notice(NoticeLevel::Error, "boom");
    tokio::time::advance(Duration::from_secs(60)).await;
    h.app.tick();
    assert_eq!(h.notices(), ["boom"]);
    assert!(
        !h.app.wants_tick(),
        "an error alone does not keep the clock running"
    );
    h.press(KeyCode::Left);
    assert!(h.notices().is_empty());

    h.app.notice(NoticeLevel::Info, "copied");
    assert!(h.app.wants_tick());
    tokio::time::advance(Duration::from_secs(6)).await;
    h.app.tick();
    assert!(h.notices().is_empty());
    assert!(!h.app.wants_tick());
}

#[tokio::test(start_paused = true)]
async fn r_in_the_context_view_reads_the_ledger_again() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/context");
    h.press(KeyCode::Enter);
    h.settle().await;
    let before = h.calls("session.context").len();
    h.press(KeyCode::Char('r'));
    h.settle().await;
    assert_eq!(h.calls("session.context").len(), before + 1);
    assert!(h.screen(80, 20).starts_with(" ← Context"));
}

fn card(id: &str, name: &str) -> Block {
    Block::Tool(Box::new(ToolCall::new(
        ToolRef {
            session_id: "s1".into(),
            loop_id: "l1".into(),
            request_index: 0,
            tool_call_id: id.into(),
        },
        name,
    )))
}

fn title(h: &mut Harness) -> String {
    h.screen(80, 20)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(start_paused = true)]
async fn slash_tool_opens_a_card_and_brackets_step_between_cards() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "tool.output",
        json!({"stream": "stdout", "encoding": "base64", "base_offset": 0, "next_offset": 6,
               "observed_end": 6, "eof": true, "truncated": false, "availability": "available",
               "data": "aGVsbG8K"}),
    );
    let transcript = &mut h.app.session.as_mut().unwrap().transcript;
    transcript.push(Block::User {
        text: "go".into(),
        kind: UserKind::Prompt,
    });
    transcript.push(card("c1", "bash"));
    transcript.push(card("c2", "read"));

    h.type_text("/tool");
    h.press(KeyCode::Enter);
    assert!(title(&mut h).starts_with(" ← read"), "the newest card");
    assert!(title(&mut h).contains("tool 2 of 2"));
    h.settle().await;
    let screen = h.screen(80, 20);
    assert!(screen.contains("hello"), "{screen}");
    assert!(
        !screen.contains("── "),
        "one stream needs no section: {screen}"
    );

    h.press(KeyCode::Char('['));
    assert!(title(&mut h).contains("tool 1 of 2"));
    h.settle().await;
    let screen = h.screen(80, 20);
    assert!(
        screen.contains("── stdout ──") && screen.contains("── stderr ──"),
        "{screen}"
    );
    let streams: Vec<_> = h
        .calls("tool.output")
        .iter()
        .map(|params| params["stream"].clone())
        .collect();
    assert_eq!(streams, ["output", "stdout", "stderr"]);
    h.press(KeyCode::Char('['));
    assert!(title(&mut h).contains("tool 1 of 2"), "stops at the first");
    h.press(KeyCode::Right);
    assert!(title(&mut h).contains("tool 2 of 2"));

    h.press(KeyCode::Esc);
    h.app.scroll = Scroll::Anchor {
        block: 1,
        offset: 0,
    };
    h.action(Action::ShowTool);
    assert!(
        title(&mut h).contains("tool 1 of 2"),
        "the card at the top of the view"
    );
}
