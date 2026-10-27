//! Commands, selectors, the footer and the small features around them.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};

use super::harness::*;
use crate::app::templates::Template;
use crate::app::{Action, Overlay, Scroll};
use crate::protocol::ToolRef;
use crate::protocol::read::UserKind;
use crate::transcript::{Block, RequestKey, ToolCall};
use crate::ui::theme::Palette;

/// A turn that stays running once sent.
fn script_turn(h: &Harness) {
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
}

/// A booted app with `s1` open.
async fn running_app() -> Harness {
    let mut h = Harness::new();
    script_turn(&h);
    h.open().await;
    h
}

/// The texts sent as prompts after typing `text` and pressing Enter `enters` times.
async fn sent_after(text: &str, enters: usize) -> Vec<String> {
    let mut h = running_app().await;
    h.type_text(text);
    for _ in 0..enters {
        h.press(KeyCode::Enter);
    }
    h.settle().await;
    h.sends()
}

#[tokio::test(start_paused = true)]
async fn an_unknown_command_warns_and_only_a_second_enter_sends_it() {
    assert_eq!(sent_after("/modle", 1).await, Vec::<String>::new());
    assert_eq!(sent_after("/modle", 2).await, ["/modle"]);
    assert_eq!(
        sent_after("//foo", 1).await,
        ["/foo"],
        "// escapes the slash"
    );
    for prompt in [
        "/usr/bin",
        "/etc is where config lives",
        "/usr/local/bin/foo",
    ] {
        assert_eq!(
            sent_after(prompt, 1).await,
            [prompt],
            "{prompt} is a prompt"
        );
    }

    let mut h = running_app().await;
    h.type_text("/modle");
    h.press(KeyCode::Enter);
    assert_eq!(
        h.notices(),
        ["Unknown command /modle · did you mean /model? · enter again sends it as text"]
    );
    assert_eq!(
        h.app.composer.text(),
        "/modle",
        "the draft stays to be edited"
    );
    // Editing and coming back to the same text is a new draft, warned again.
    h.press(KeyCode::Backspace);
    h.type_text("e");
    h.press(KeyCode::Enter);
    assert!(h.sends().is_empty());
    assert!(!h.notices().is_empty());
}

fn two_models(h: &Harness) {
    h.fake.script(
        "model.list",
        json!({"models": [
            {"id": "luna", "model_ref": "cus/luna", "context_window": 400000,
             "supports_tools": true, "supported_reasoning": ["low", "medium"]},
            {"id": "mock", "model_ref": "mock/large", "context_window": 200000,
             "supports_tools": true, "supported_reasoning": ["low", "medium", "high"]},
        ]}),
    );
}

fn updated_to(model: &str) -> Value {
    let mut info = session_info("s1");
    info["model"] = json!(model);
    json!({"session": info})
}

/// An open session on `gpt-6.1-sol` at `xhigh`, with models that support other levels.
async fn xhigh_session() -> Harness {
    let mut h = Harness::new();
    h.fake.script(
        "model.list",
        json!({"models": [
            {"id": "gpt-6.1-sol", "supported_reasoning": ["low", "high", "xhigh"]},
            {"id": "gpt-6.1-luna", "supported_reasoning": ["high", "xhigh"]},
            {"id": "deepseek-v4.1-flash", "supported_reasoning": ["high", "max"]},
        ]}),
    );
    let mut info = session_info("s1");
    info["model"] = json!("gpt-6.1-sol");
    info["reasoning"] = json!("xhigh");
    h.fake.script("session.open", json!({"session": info}));
    h.open().await;
    h
}

#[tokio::test(start_paused = true)]
async fn a_model_without_the_current_reasoning_is_switched_to_with_the_nearest_level() {
    let mut h = xhigh_session().await;
    let mut info = session_info("s1");
    info["model"] = json!("deepseek-v4.1-flash");
    info["reasoning"] = json!("max");
    h.fake.script("session.update", json!({"session": info}));

    h.action(Action::SetModel("deepseek-v4.1-flash".into()));
    h.settle().await;
    let updates = h.calls("session.update");
    assert_eq!(updates.len(), 1, "one update carries both fields");
    assert_eq!(updates[0]["model"], "deepseek-v4.1-flash");
    assert_eq!(updates[0]["reasoning"], "max");
    assert_eq!(
        h.notices(),
        ["deepseek-v4.1-flash does not support reasoning xhigh; using max"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_model_that_supports_the_current_reasoning_is_switched_to_as_it_is() {
    let mut h = xhigh_session().await;
    let mut info = session_info("s1");
    info["model"] = json!("gpt-6.1-luna");
    info["reasoning"] = json!("xhigh");
    h.fake.script("session.update", json!({"session": info}));

    h.action(Action::SetModel("gpt-6.1-luna".into()));
    h.settle().await;
    let updates = h.calls("session.update");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["model"], "gpt-6.1-luna");
    assert!(updates[0].get("reasoning").is_none(), "{:?}", updates[0]);
    assert!(h.notices().is_empty(), "{:?}", h.notices());
}

#[tokio::test(start_paused = true)]
async fn model_takes_a_direct_argument_with_completion() {
    let mut h = Harness::new();
    two_models(&h);
    h.fake.script("session.update", updated_to("luna"));
    h.open().await;

    h.type_text("/model lu");
    assert!(matches!(h.app.overlay, Overlay::Palette { .. }));
    assert_eq!(h.app.palette_rows().len(), 1);
    assert_eq!(h.app.palette_rows()[0].0, "luna");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "/model luna");

    h.app.composer.clear();
    h.type_text("/model lu");
    h.press(KeyCode::Enter);
    h.settle().await;
    let updates = h.calls("session.update");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["model"], "luna");
    assert_eq!(h.app.session.as_ref().unwrap().info.model, "luna");

    h.type_text("/model zzz");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.notices(), ["Unknown model zzz"]);
    assert_eq!(h.calls("session.update").len(), 1, "nothing was switched");
}

#[tokio::test(start_paused = true)]
async fn ctrl_p_cycles_the_models_and_shift_ctrl_p_goes_back() {
    let mut h = Harness::new();
    two_models(&h);
    h.fake.script("session.update", updated_to("luna"));
    h.open().await;
    assert_eq!(h.app.session.as_ref().unwrap().info.model, "mock");

    h.ctrl(KeyCode::Char('p'));
    h.settle().await;
    h.key(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    h.settle().await;
    let models: Vec<Value> = h
        .calls("session.update")
        .iter()
        .map(|call| call["model"].clone())
        .collect();
    // Forward from `mock` wraps to `luna`; backward from `luna` wraps to `mock`.
    assert_eq!(models, [json!("luna"), json!("mock")]);
}

fn profiles(h: &Harness) {
    h.fake.script(
        "profile.list",
        json!({"profiles": [
            {"id": "coding", "model": "mock", "reasoning": "medium", "approval": "auto"},
            {"id": "ask", "model": "mock", "reasoning": "medium", "approval": "ask"},
        ]}),
    );
}

#[tokio::test(start_paused = true)]
async fn new_takes_a_profile_that_completes() {
    let mut h = Harness::new();
    profiles(&h);
    h.open().await;

    h.type_text("/new ");
    let names: Vec<String> = h.app.palette_rows().into_iter().map(|row| row.0).collect();
    assert_eq!(names, ["coding", "ask"]);
    h.type_text("a");
    assert_eq!(h.app.palette_rows()[0].0, "ask");
    assert_eq!(h.app.palette_rows()[0].1, "mock ask");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.app.launch.profile.as_deref(), Some("ask"));

    h.type_text("/new nope");
    h.press(KeyCode::Enter);
    assert_eq!(h.notices(), ["Unknown profile nope"]);
    assert_eq!(h.app.launch.profile.as_deref(), Some("ask"));
}

#[tokio::test(start_paused = true)]
async fn a_template_is_a_command_that_sends_its_expansion() {
    let mut h = running_app().await;
    h.app.templates = vec![Template::parse(
        "review".into(),
        "---\ndescription: Review code\n---\nReview $1 with ${2:-care}.",
    )];
    h.type_text("/rev");
    let rows = h.app.palette_rows();
    assert_eq!(rows, [("review".to_owned(), "Review code".to_owned())]);

    h.type_text("iew parser");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.sends(), ["Review parser with care."]);
}

#[tokio::test(start_paused = true)]
async fn session_shows_the_facts_and_the_item_count() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("session.read", display_read(42, 0));
    h.type_text("/session");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert!(matches!(
        h.app.overlay,
        Overlay::SessionInfo { items: Some(42) }
    ));
    let screen = h.screen(100, 30);
    let facts = [
        ("id", "s1"),
        ("workspace", "/tmp/ws"),
        ("profile", "coding"),
        ("model", "mock · medium"),
        ("items", "42"),
        ("tokens", "none yet"),
    ];
    for (name, value) in facts {
        let row = format!("{name:<10}{value}");
        assert!(screen.contains(&row), "{row}:\n{screen}");
    }
    assert!(screen.contains("enter copy id"), "{screen}");
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
}

#[tokio::test(start_paused = true)]
async fn context_opens_as_a_sheet_below_the_transcript() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    push_blocks(&mut h, vec![assistant(0, "", "Transcript stays visible")]);
    h.type_text("/context");
    h.press(KeyCode::Enter);
    h.settle().await;

    let screen = h.screen(100, 30);
    assert!(
        screen.contains("r refresh · c copy · esc close"),
        "{screen}"
    );
    assert!(screen.contains("Context: 1.2k of 200k tokens"), "{screen}");
    assert!(!screen.contains("← Context"), "{screen}");
    let transcript = screen.find("Transcript stays visible").expect(&screen);
    let context = screen.find("Context: 1.2k").expect(&screen);
    assert!(transcript < context, "{screen}");
}

async fn ask(h: &mut Harness, id: &str, tool: &str) {
    h.emit(
        "interaction_requested",
        json!({"turn": turn("l1"), "interaction": {
            "interaction_id": id, "tool_call_id": format!("c-{id}"), "tool_name": tool,
            "kind": {"type": "approval", "data": {"prompt": format!("Allow {tool}?")}}}}),
    )
    .await;
    h.run_for(Duration::from_millis(50)).await;
}

fn answers(h: &Harness) -> Vec<(String, String)> {
    h.calls("interaction.answer")
        .iter()
        .map(|call| {
            (
                call["interaction_id"].as_str().unwrap().to_owned(),
                call["answer"]["decision"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn allowing_a_tool_for_the_session_answers_its_later_approvals_itself() {
    let mut h = running_app().await;
    h.fake.script("interaction.answer", json!({"ok": true}));
    h.submit("work");
    h.until(|app| app.is_busy()).await;

    ask(&mut h, "i1", "bash").await;
    assert!(h.app.has_interaction());
    h.type_text("a");
    h.run_for(Duration::from_millis(50)).await;
    assert_eq!(answers(&h), [("i1".to_owned(), "allow_once".to_owned())]);

    ask(&mut h, "i2", "bash").await;
    assert!(
        !h.app.has_interaction(),
        "bash is answered without the sheet"
    );
    assert_eq!(answers(&h)[1], ("i2".to_owned(), "allow_once".to_owned()));
    assert!(
        h.notices().is_empty(),
        "a granted tool is not announced again"
    );
    assert!(
        h.screen(100, 24).contains("allow bash"),
        "the footer says so"
    );
    assert_eq!(footer_fg(&mut h, "allow bash"), Palette::dark().amber);

    ask(&mut h, "i3", "write").await;
    assert!(h.app.has_interaction(), "another tool still asks");
    h.press(KeyCode::Esc);
    h.run_for(Duration::from_millis(50)).await;
    assert_eq!(answers(&h)[2], ("i3".to_owned(), "deny".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn the_third_row_allows_the_tool_for_the_session() {
    let mut h = running_app().await;
    h.fake.script("interaction.answer", json!({"ok": true}));
    h.submit("work");
    h.until(|app| app.is_busy()).await;
    ask(&mut h, "i1", "edit").await;
    let screen = h.screen(100, 24);
    assert!(screen.contains("Allow edit this session"), "{screen}");
    h.press(KeyCode::Down);
    h.press(KeyCode::Down);
    h.press(KeyCode::Enter);
    h.run_for(Duration::from_millis(50)).await;
    assert_eq!(answers(&h), [("i1".to_owned(), "allow_once".to_owned())]);
    let allowed = &h.app.session.as_ref().unwrap().allowed_tools;
    assert!(allowed.contains("edit"));
}

fn buffer(h: &mut Harness, width: u16, height: u16) -> Buffer {
    let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
    crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, &mut h.app)).unwrap();
    terminal.backend().buffer().clone()
}

/// The foreground of the first cell of `word` on the last row.
fn footer_fg(h: &mut Harness, word: &str) -> Color {
    let buffer = buffer(h, 100, 30);
    let row: String = (0..100).map(|x| buffer[(x, 29)].symbol()).collect();
    let at = row
        .find(word)
        .unwrap_or_else(|| panic!("{word} not in {row}"));
    buffer[(at as u16, 29)].fg
}

#[tokio::test(start_paused = true)]
async fn profile_and_approval_stay_out_of_the_footer_but_approval_is_visible() {
    let mut h = Harness::new();
    script_turn(&h);
    h.fake.script("interaction.answer", json!({"ok": true}));
    let mut info = session_info("s1");
    info["profile"] = json!("ask");
    h.fake
        .script("session.open", json!({"session": info.clone()}));
    // The auto-title rename answers with the session as the Agent has it.
    h.fake.script("session.rename", json!({"session": info}));
    profiles(&h);
    h.open().await;
    h.submit("work");
    h.until(|app| app.is_busy()).await;
    let footer = h.screen(100, 30).lines().last().unwrap().to_owned();
    assert!(footer.contains("mock · medium"), "{footer}");
    assert!(
        !footer.contains("ask") && !footer.contains("working"),
        "{footer}"
    );

    ask(&mut h, "i1", "bash").await;
    let screen = h.screen(100, 30);
    let footer = screen.lines().last().unwrap();
    assert!(footer.contains("mock · medium"), "{footer}");
    assert!(
        !footer.contains("ask") && !footer.contains("approval"),
        "{footer}"
    );
    assert!(!footer.contains("working"), "{footer}");
    assert!(screen.contains("Waiting for approval"), "{screen}");
    assert!(screen.contains("Allow bash?"), "{screen}");
    assert!(h.app.has_interaction(), "approval is still required");
    assert_eq!(h.app.session.as_ref().unwrap().info.profile, "ask");
}

#[tokio::test(start_paused = true)]
async fn an_auto_profile_leaves_the_footer_as_it_was() {
    let mut h = Harness::new();
    profiles(&h);
    h.open().await;
    let footer = h.screen(100, 30).lines().last().unwrap().to_owned();
    assert!(footer.contains("mock · medium"), "{footer}");
    assert!(
        !footer.contains("coding") && !footer.contains("auto"),
        "{footer}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_narrow_footer_cuts_the_directory_and_keeps_the_model_and_level() {
    let mut h = running_app().await;
    h.app.workspace = "/w/live-ws-minicore-tui".into();
    h.submit("work");
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    let wide = h.screen(100, 24).lines().last().unwrap().to_owned();
    assert!(wide.contains("▸ live-ws-minicore-tui@main"), "{wide}");
    for columns in [60, 50, 40] {
        let screen = h.screen(columns, 24);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("mock · medium"), "{columns}: {footer}");
        assert!(!footer.contains("working"), "{columns}: {footer}");
    }
    let screen = h.screen(40, 24);
    let footer = screen.lines().last().unwrap();
    assert!(footer.contains("▸ live-"), "{footer}");
    assert!(footer.contains('…'), "{footer}");
}

fn sessions_listing(h: &Harness) {
    let titled = |id: &str, title: &str| {
        let mut info = session_info(id);
        info["title"] = json!(title);
        info
    };
    h.fake.script(
        "session.list",
        json!({"sessions": [
            titled("s1", "Same title"),
            titled("s2", "Same title"),
            titled("s3", "Other work"),
        ]}),
    );
    h.fake.script(
        "session.open",
        json!({"session": titled("s1", "Same title")}),
    );
}

#[tokio::test(start_paused = true)]
async fn the_sessions_list_marks_the_current_one_and_tells_equal_titles_apart() {
    let mut h = Harness::new();
    sessions_listing(&h);
    h.app.workspace = "/tmp/ws".into();
    h.open().await;
    h.ctrl(KeyCode::Char('r'));
    let screen = h.screen(100, 30);
    let rows: Vec<&str> = screen
        .lines()
        .filter(|row| row.contains("Same title"))
        .collect();
    assert_eq!(rows.len(), 2, "{screen}");
    assert!(rows[0].contains("current · s1"), "{}", rows[0]);
    assert!(
        rows[1].contains("s2") && !rows[1].contains("current"),
        "{}",
        rows[1]
    );
    let other = screen
        .lines()
        .find(|row| row.contains("Other work"))
        .unwrap();
    assert!(!other.contains("s3"), "a unique title needs no id: {other}");
    assert!(
        !rows.iter().chain([&other]).any(|row| row.contains("mock")),
        "the model column is gone:\n{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_session_is_renamed_from_the_picker_without_opening_it() {
    let mut h = Harness::new();
    sessions_listing(&h);
    let mut renamed = session_info("s2");
    renamed["title"] = json!("Renamed");
    h.fake.script("session.rename", json!({"session": renamed}));
    h.open().await;

    h.ctrl(KeyCode::Char('r'));
    h.press(KeyCode::Down);
    h.ctrl(KeyCode::Char('n'));
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("Rename Session") && screen.contains("> Same title"),
        "{screen}"
    );

    // Esc cancels the rename and leaves the picker open.
    h.press(KeyCode::Esc);
    assert!(matches!(
        h.app.overlay,
        Overlay::Sessions { renaming: None, .. }
    ));
    h.ctrl(KeyCode::Char('n'));
    for _ in 0.."Same title".len() {
        h.press(KeyCode::Backspace);
    }
    h.type_text("Renamed");
    h.press(KeyCode::Enter);
    h.settle().await;

    let calls = h.calls("session.rename");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["session_id"], "s2");
    assert_eq!(calls[0]["title"], "Renamed");
    let screen = h.screen(100, 30);
    assert!(screen.contains("Renamed"), "{screen}");
    assert!(matches!(h.app.overlay, Overlay::Sessions { .. }));
    let resident = h.app.session.as_ref().unwrap();
    assert_eq!(
        resident.info.title.as_deref(),
        Some("Same title"),
        "s1 is untouched"
    );
}

fn push_blocks(h: &mut Harness, blocks: Vec<Block>) {
    let transcript = &mut h.app.session.as_mut().unwrap().transcript;
    for block in blocks {
        transcript.push(block);
    }
}

fn assistant(index: usize, reasoning: &str, text: &str) -> Block {
    Block::Assistant {
        key: RequestKey {
            loop_id: format!("l{index}"),
            request_index: 0,
        },
        reasoning: reasoning.into(),
        text: text.into(),
        streaming: false,
        expanded: None,
    }
}

/// Opens find on `query` the way typing it would, in one step.
fn open_find(h: &mut Harness, query: &str) {
    let blocks = h.app.transcript_blocks();
    let matches = crate::app::find::find_matches(blocks, query, true);
    h.app.overlay = Overlay::Find {
        query: query.to_owned(),
        matches,
        current: 0,
    };
    h.app.show_match();
}

fn find_status(h: &Harness) -> (usize, usize) {
    match &h.app.overlay {
        Overlay::Find {
            matches, current, ..
        } => (*current + 1, matches.len()),
        _ => panic!("find is not open"),
    }
}

#[tokio::test(start_paused = true)]
async fn find_counts_matches_and_highlights_them_on_screen() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    push_blocks(
        &mut h,
        vec![
            Block::User {
                text: "the parser bug".into(),
                kind: UserKind::Prompt,
                at: None,
            },
            assistant(1, "", "parser here and parser there"),
        ],
    );
    h.ctrl(KeyCode::Char('f'));
    h.type_text("parser");
    assert_eq!(find_status(&h), (1, 3), "three matches in two blocks");
    h.press(KeyCode::Enter);
    h.press(KeyCode::Enter);
    assert_eq!(find_status(&h), (3, 3));
    h.press(KeyCode::Enter);
    assert_eq!(find_status(&h), (1, 3), "wraps");

    let buffer = buffer(&mut h, 80, 24);
    let marked: String = (0..24)
        .flat_map(|y| (0..80).map(move |x| (x, y)))
        .filter(|&(x, y)| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
        .map(|(x, y)| buffer[(x, y)].symbol().to_owned())
        .collect();
    assert_eq!(marked, "parserparserparser");
}

#[tokio::test(start_paused = true)]
async fn find_lands_on_the_matching_row_not_the_top_of_the_block() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let paragraphs: Vec<String> = (0..60)
        .map(|n| match n {
            45 => "the needle is in this paragraph".to_owned(),
            n => format!("paragraph {n}"),
        })
        .collect();
    push_blocks(&mut h, vec![assistant(1, "", &paragraphs.join("\n\n"))]);
    h.screen(80, 24);
    h.ctrl(KeyCode::Char('f'));
    h.type_text("needle");
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("the needle is in this paragraph"),
        "{screen}"
    );
    let Scroll::Anchor { offset, .. } = h.app.scroll else {
        panic!("the view is anchored on the match");
    };
    assert!(
        offset > 40,
        "landed inside the block, not at its top: {offset}"
    );
}

#[tokio::test(start_paused = true)]
async fn find_opens_folded_thinking_and_tool_output_that_hold_the_match() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let thinking: Vec<String> = (0..30).map(|n| format!("thought {n}")).collect();
    let mut call = ToolCall::new(
        ToolRef {
            session_id: "s1".into(),
            loop_id: "l2".into(),
            request_index: 0,
            tool_call_id: "c1".into(),
        },
        "bash",
    );
    call.detail = "$ cargo test".into();
    call.output = (0..30)
        .map(|n| match n {
            10 => "test deepneedle ... ok".to_owned(),
            n => format!("line {n}"),
        })
        .collect();
    push_blocks(
        &mut h,
        vec![
            assistant(1, &thinking.join("\n"), "done"),
            Block::Tool(Box::new(call)),
        ],
    );
    for hidden in ["thought 22", "test deepneedle"] {
        assert!(!h.screen(80, 24).contains(hidden), "{hidden} starts folded");
    }

    open_find(&mut h, "thought 22");
    let screen = h.screen(80, 24);
    assert!(screen.contains("thought 22"), "{screen}");

    open_find(&mut h, "deepneedle");
    let screen = h.screen(80, 24);
    assert!(screen.contains("test deepneedle ... ok"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn the_title_follows_the_session_and_the_exit_prints_the_resume_command() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.workspace = "/w/proj".into();
    let first = h.app.terminal_bytes();
    assert!(first.contains("\x1b]0;proj · idle\x07"), "{first:?}");
    assert_eq!(h.app.terminal_bytes(), "", "unchanged state writes nothing");
    assert_eq!(
        h.app.exit_output(),
        "\x1b[23;0t",
        "an empty session has nothing to resume"
    );

    h.app.session.as_mut().unwrap().info.title = Some("Fix parser".into());
    assert!(h.app.terminal_bytes().contains("proj · Fix parser · idle"));
    push_blocks(
        &mut h,
        vec![Block::User {
            text: "hi".into(),
            kind: UserKind::Prompt,
            at: None,
        }],
    );
    assert_eq!(
        h.app.exit_output(),
        "\x1b[23;0tResume: minicore-tui --session s1\n"
    );
}

#[tokio::test(start_paused = true)]
async fn an_edit_approval_reads_the_card_and_shows_the_diff_in_colour() {
    let mut h = running_app().await;
    h.fake.script(
        "tool.read",
        json!({"display": {
            "detail": "notes.txt", "input_line_count": 5,
            "expanded_input": "--- before\n+++ after\n@@\n-hello\n+goodbye\n context\n",
        }}),
    );
    h.submit("work");
    h.until(|app| app.is_busy()).await;
    // The card exists, as the invocation event makes it, but has no input yet.
    let mut call = ToolCall::new(
        ToolRef {
            session_id: "s1".into(),
            loop_id: "l1".into(),
            request_index: 0,
            tool_call_id: "c-i1".into(),
        },
        "edit",
    );
    call.detail = "notes.txt".into();
    push_blocks(&mut h, vec![Block::Tool(Box::new(call))]);
    ask(&mut h, "i1", "edit").await;

    assert_eq!(h.calls("tool.read").len(), 1);
    let screen = h.screen(100, 30);
    assert!(screen.contains("edit  notes.txt"), "{screen}");
    assert!(
        !screen.contains("--- before"),
        "headers are left out:\n{screen}"
    );
    let palette = Palette::dark();
    assert_eq!(h.fg_of("-hello"), palette.failure);
    assert_eq!(h.fg_of("+goodbye"), palette.success);
}
