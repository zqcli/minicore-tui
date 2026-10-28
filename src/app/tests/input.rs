use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use serde_json::json;

use super::harness::*;
use crate::app::{FLASH_TTL, Msg, NoticeLevel, Overlay, Scroll};
use crate::protocol::read::UserKind;
use crate::transcript::{Block, RequestKey};

/// `count` prompts, each followed by a three-line answer.
fn conversation(h: &mut Harness, count: usize) {
    let transcript = &mut h.app.session.as_mut().unwrap().transcript;
    for index in 0..count {
        transcript.push(Block::User {
            text: format!("prompt {index}"),
            kind: UserKind::Prompt,
            at: None,
        });
        transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: format!("l{index}"),
                request_index: 0,
            },
            reasoning: String::new(),
            text: format!("answer {index}\n\nline two\n\nline three"),
            streaming: false,
            expanded: None,
        });
    }
}

/// The first transcript row with text; an anchored block may start with the
/// blank row that separates it from the one above, or a card's pad row.
fn first_row(h: &mut Harness) -> String {
    let screen = h.screen(60, 24);
    // The scrollbar fills the right edge for a moment after a scroll.
    let bare = |row: &str| row.trim_end_matches(['│', '┃', ' ']).to_owned();
    screen
        .lines()
        .map(bare)
        .find(|row| !matches!(row.trim(), "" | "▎"))
        .unwrap_or_default()
}

/// A running turn that has just asked to run `bash`.
async fn asking() -> Harness {
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
    h
}

#[tokio::test(start_paused = true)]
async fn an_approval_prompt_owns_the_keys_and_answers_over_the_wire() {
    let mut h = asking().await;
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
    arm_prompt().await;
    h.press(KeyCode::Enter);
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["interaction_id"], "i1");
    assert_eq!(answered[0]["answer"]["decision"], "deny");
    assert!(!h.app.has_interaction());
}

#[tokio::test(start_paused = true)]
async fn answer_keys_pressed_as_the_prompt_appears_do_nothing_until_it_is_armed() {
    let mut h = asking().await;
    for key in [KeyCode::Char('y'), KeyCode::Char('a'), KeyCode::Char('n')] {
        h.press(key);
    }
    h.press(KeyCode::Enter);
    h.run_for(Duration::from_millis(50)).await;
    assert!(h.calls("interaction.answer").is_empty());
    assert!(h.app.has_interaction());

    arm_prompt().await;
    h.press(KeyCode::Char('y'));
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["decision"], "allow_once");
}

#[tokio::test(start_paused = true)]
async fn typing_on_keeps_a_prompt_unarmed_until_a_pause() {
    let mut h = asking().await;
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(400)).await;
        h.type_text("a sentence");
    }
    h.press(KeyCode::Enter);
    h.run_for(Duration::from_millis(50)).await;
    assert!(h.calls("interaction.answer").is_empty());

    arm_prompt().await;
    h.press(KeyCode::Char('n'));
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["decision"], "deny");
}

#[tokio::test(start_paused = true)]
async fn escape_denies_a_prompt_that_is_not_armed_yet() {
    let mut h = asking().await;
    h.press(KeyCode::Esc);
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["decision"], "deny");
}

#[tokio::test(start_paused = true)]
async fn a_shifted_end_does_not_select_in_the_draft_behind_a_prompt() {
    let mut h = asking().await;
    h.app.composer.set_text("draft");
    h.app.composer.move_home();
    h.key(KeyCode::End, KeyModifiers::SHIFT);
    assert!(h.app.composer.selection().is_none());
}

#[tokio::test(start_paused = true)]
async fn the_transcript_scrolls_while_a_prompt_waits() {
    let mut h = asking().await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    h.press(KeyCode::PageUp);
    assert_ne!(h.app.scroll, Scroll::Follow);
    h.press(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow);
    h.screen(60, 24);
    h.ctrl(KeyCode::Up);
    assert_ne!(h.app.scroll, Scroll::Follow);
    let expanded = h.app.tools_expanded;
    h.ctrl(KeyCode::Char('o'));
    assert_ne!(h.app.tools_expanded, expanded);
    assert!(h.app.has_interaction(), "the prompt stays");
    assert!(h.calls("interaction.answer").is_empty());
}

#[tokio::test(start_paused = true)]
async fn shift_enter_adds_a_line_and_alt_enter_queues_follow_ups_behind_a_running_turn() {
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
}

#[tokio::test(start_paused = true)]
async fn alt_enter_and_ctrl_enter_queue_follow_ups_while_enter_steers() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake
        .script("turn.steer", json!({"ok": true, "steer_index": 0}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;

    h.key(KeyCode::Enter, KeyModifiers::ALT);
    assert!(h.queue().is_empty(), "an empty draft queues nothing");

    h.type_text("by alt enter");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.type_text("by ctrl enter");
    h.key(KeyCode::Enter, KeyModifiers::CONTROL);
    assert_eq!(h.queue(), ["by alt enter", "by ctrl enter"]);
    assert!(
        h.calls("turn.steer").is_empty(),
        "a follow-up is not a steer"
    );

    h.type_text("now");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert!(
        h.calls("turn.steer").is_empty(),
        "the steer waits behind both follow-ups"
    );
    assert_eq!(h.queue(), ["by alt enter", "by ctrl enter", "now"]);
    assert_eq!(
        h.app.session.as_ref().unwrap().queue[2].kind,
        crate::app::QueuedKind::Steer
    );
}

#[tokio::test(start_paused = true)]
async fn tab_does_nothing_in_the_composer() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.type_text("draft");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "draft");
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;

    h.type_text("busy");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "busy");
    assert!(h.queue().is_empty());
}

#[tokio::test(start_paused = true)]
async fn tab_with_a_completion_list_open_completes() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/mode");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "/model ");
}

#[tokio::test(start_paused = true)]
async fn a_follow_up_waits_in_the_queue_while_a_compaction_runs() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.emit(
        "session_state",
        json!({"state": {"session_id": "s1", "status": "idle",
                         "compaction": {"operation_id": "auto-boundary-l1-1", "phase": "summarizing",
                                        "covered_item_count": 8, "retained_item_count": 2}}}),
    )
    .await;
    h.until(|app| app.compacting()).await;
    h.app.composer.set_text("later");
    h.action(crate::app::Action::QueueFollowUp);
    h.settle().await;
    assert_eq!(h.queue(), ["later"]);
    assert!(h.sends().is_empty());

    h.emit(
        "session_state",
        json!({"state": {"session_id": "s1", "status": "idle"}}),
    )
    .await;
    h.settle().await;
    assert_eq!(h.sends(), ["later"]);
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_clears_a_draft_and_a_second_press_within_half_a_second_quits() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("x");
    h.ctrl(KeyCode::Char('c'));
    assert_eq!(h.app.composer.text(), "");
    assert!(!h.app.quit);
    h.ctrl(KeyCode::Char('c'));
    assert!(h.app.quit, "the second press is inside the window");

    let mut h = Harness::opened(display_read(0, 0)).await;
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "the first press only arms the quit");
    assert_eq!(h.notices(), ["Press Ctrl+C again to quit"]);
    tokio::time::advance(std::time::Duration::from_millis(600)).await;
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "a late press arms again");
    h.type_text("x");
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
async fn slash_tool_is_an_unknown_command() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    assert!(crate::app::commands::execute("tool", "").is_none());
    assert!(crate::app::commands::filter("tool", &[]).is_empty());
    h.type_text("/tool");
    h.press(KeyCode::Enter);
    assert_eq!(
        h.notices(),
        ["Unknown command /tool · enter again sends it as text"]
    );
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "/tool");
    h.settle().await;
    assert!(h.calls("tool.read").is_empty());
    assert!(h.sends().is_empty());
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

#[tokio::test(start_paused = true)]
async fn alt_d_and_alt_delete_delete_the_next_word_and_ctrl_x_copies_the_answer() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("one two three");
    h.press(KeyCode::Home);
    h.key(KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(
        h.app.composer.text(),
        " two three",
        "typed nothing, deleted a word"
    );
    h.key(KeyCode::Delete, KeyModifiers::ALT);
    assert_eq!(h.app.composer.text(), " three");

    h.ctrl(KeyCode::Char('x'));
    assert_eq!(h.notices(), ["No answer to copy yet"]);
    assert_eq!(
        h.app.composer.text(),
        " three",
        "Ctrl+X leaves the draft alone"
    );
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
    assert!(matches!(h.app.overlay, Overlay::Palette { selected: 1 }));
    h.press(KeyCode::Esc);
    h.app.composer.clear();

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
    assert_eq!(h.app.composer.text(), "/di", "the draft stays");

    h.app.composer.clear();
    h.type_text("/usr/local/bin/foo is a path");
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "/usr/local/bin/foo is a path");

    h.app.composer.set_text("fix the bug in ");
    h.type_text("@ap");
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "fix the bug in @ap");

    h.app.composer.set_text("fix the bug in ");
    h.type_text("@zzz");
    h.settle().await;
    h.press(KeyCode::Enter);
    assert!(!h.app.overlay.is_none(), "no match keeps the picker open");
    h.press(KeyCode::Esc);
    h.app.composer.set_text("fix the bug in ");
    h.type_text("@main");
    h.settle().await;
    h.press(KeyCode::Enter);
    assert_eq!(h.app.composer.text(), "fix the bug in @src/main.rs ");
}

#[tokio::test(start_paused = true)]
async fn the_command_list_filters_wraps_and_applies() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/");
    assert!(matches!(h.app.overlay, Overlay::Palette { selected: 0 }));
    h.press(KeyCode::Up);
    let last = crate::app::commands::COMMANDS.len() - 1;
    assert!(matches!(h.app.overlay, Overlay::Palette { selected } if selected == last));
    h.press(KeyCode::Down);
    assert!(matches!(h.app.overlay, Overlay::Palette { selected: 0 }));

    h.press(KeyCode::Backspace);
    assert!(h.app.overlay.is_none(), "backspace past the slash closes");
    assert_eq!(h.app.composer.text(), "");

    h.type_text("/mo");
    h.press(KeyCode::Tab);
    assert!(h.app.overlay.is_none());
    assert_eq!(
        h.app.composer.text(),
        "/model ",
        "tab completes without running"
    );

    h.app.composer.clear();
    h.type_text("/mo");
    h.press(KeyCode::Enter);
    assert!(matches!(h.app.overlay, Overlay::Models { .. }));
    assert_eq!(h.app.composer.text(), "");
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
        first_row(&mut h).ends_with(concat!("minicore-tui v", env!("CARGO_PKG_VERSION"))),
        "the header tops the transcript"
    );
    assert!(h.screen(60, 24).contains("prompt 0"));
    h.ctrl(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow);
    h.press(KeyCode::Home);
    assert!(
        first_row(&mut h).ends_with(concat!("minicore-tui v", env!("CARGO_PKG_VERSION"))),
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
    // The last match cannot reach the top: the view stops at the end.
    h.press(KeyCode::Tab);
    let screen = h.screen(60, 24);
    assert!(
        screen.contains("3 of 3") && screen.contains("prompt 11"),
        "{screen}"
    );
    h.press(KeyCode::Up);
    assert!(first_row(&mut h).ends_with("prompt 10"));
    h.press(KeyCode::Enter);
    let screen = h.screen(60, 24);
    assert!(
        screen.contains("3 of 3") && screen.contains("prompt 11"),
        "{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_match_in_block_0_is_shown_below_the_welcome_header() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    h.ctrl(KeyCode::Char('f'));
    h.type_text("prompt 0");
    assert!(
        first_row(&mut h).ends_with("prompt 0"),
        "the view lands on the match, not on the header:\n{}",
        h.screen(60, 24)
    );
}

#[tokio::test(start_paused = true)]
async fn errors_stay_until_a_key_and_info_notices_expire() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.notice(NoticeLevel::Error, "boom");
    tokio::time::advance(Duration::from_secs(60)).await;
    h.app.tick();
    assert_eq!(h.notices(), ["boom"]);
    assert!(
        h.app.next_expiry().is_none(),
        "an error alone does not keep the clock running"
    );
    h.press(KeyCode::Left);
    assert!(h.notices().is_empty());

    h.app.notice(NoticeLevel::Info, "copied");
    tokio::time::advance(Duration::from_secs(6)).await;
    h.app.tick();
    assert!(h.notices().is_empty());
    assert!(h.app.next_expiry().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_flash_and_the_scrollbar_wake_the_clock_once_at_their_end() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let flashed = tokio::time::Instant::now();
    h.app.on_msg(Msg::Flash("Copied!".into()));
    let step = Duration::from_millis(300);
    tokio::time::advance(step).await;
    h.app.scrolled_at = Some(tokio::time::Instant::now());
    assert!(!h.app.animating(), "nothing moves on every frame");
    assert_eq!(h.app.next_expiry(), Some(flashed + FLASH_TTL));

    tokio::time::advance(FLASH_TTL - step).await;
    h.app.tick();
    assert_eq!(h.app.next_expiry(), Some(flashed + step + FLASH_TTL));
    tokio::time::advance(step).await;
    h.app.tick();
    assert!(h.app.next_expiry().is_none() && !h.app.scrollbar_shown());
}

#[tokio::test(start_paused = true)]
async fn r_in_the_context_sheet_reads_the_ledger_again() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/context");
    h.press(KeyCode::Enter);
    h.settle().await;
    let before = h.calls("session.context").len();
    h.press(KeyCode::Char('r'));
    h.settle().await;
    assert_eq!(h.calls("session.context").len(), before + 1);
    assert!(h.screen(80, 20).contains("r refresh · c copy · esc close"));
}

#[tokio::test(start_paused = true)]
async fn pointer_motion_asks_for_a_redraw_only_over_the_scrollbar() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    let moved = |h: &mut Harness, column| {
        h.app.on_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        })
    };
    assert!(!moved(&mut h, 20), "motion over text changes nothing");
    assert!(moved(&mut h, 59), "motion over the scrollbar shows it");
}
