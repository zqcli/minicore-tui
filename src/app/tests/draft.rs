//! The draft: the screen before the first message creates a session.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::json;

use super::harness::*;
use crate::app::{Action, LaunchSettings, Overlay};
use crate::protocol::Reasoning;

const NO_SESSION: &str = "No session yet: send a message first";

/// Scripts what the first message of a draft needs: a session and a running turn.
fn script_first_message(h: &Harness) {
    h.fake
        .script("session.create", json!({"session": session_info("s1")}));
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
}

/// A booted app with no session.
async fn draft() -> Harness {
    let mut h = Harness::new();
    script_first_message(&h);
    h.boot().await;
    h
}

#[tokio::test(start_paused = true)]
async fn the_first_submit_creates_the_session_with_the_draft_settings_then_sends_the_text() {
    let mut h = Harness::new();
    script_first_message(&h);
    h.app.launch = LaunchSettings {
        profile: Some("coding".into()),
        model: Some("mock".into()),
        reasoning: Some(Reasoning::High),
    };
    h.boot().await;
    h.settle().await;
    assert!(
        h.calls("session.create").is_empty(),
        "booting creates nothing"
    );
    assert!(h.app.session.is_none());

    h.submit("fix the greeting");
    assert_eq!(h.app.composer.text(), "", "the editor gave the text up");
    h.until(|app| app.is_busy()).await;
    h.settle().await;

    let create = h.calls("session.create");
    assert_eq!(create.len(), 1);
    assert_eq!(create[0]["profile"], "coding");
    assert_eq!(create[0]["model"], "mock");
    assert_eq!(create[0]["reasoning"], "high");
    assert_eq!(create[0]["workspace"], json!(h.app.workspace));
    assert_eq!(h.sends(), ["fix the greeting"]);
    assert_eq!(h.texts(), ["fix the greeting"]);
    assert!(h.sent_at("fix the greeting").is_some());
    assert_eq!(
        h.calls("session.rename")[0]["title"],
        "fix the greeting",
        "the first prompt titles the session as usual"
    );
    assert_eq!(
        h.app.launch,
        LaunchSettings::default(),
        "the session took the settings"
    );
    h.press(KeyCode::Up);
    assert_eq!(
        h.app.composer.text(),
        "fix the greeting",
        "recorded in the prompt history"
    );
}

const HEADER: &str = concat!("minicore-tui v", env!("CARGO_PKG_VERSION"));

#[tokio::test(start_paused = true)]
async fn the_draft_shows_the_welcome_header_and_creates_no_session() {
    let mut h = draft().await;
    h.settle().await;
    let screen = h.screen(70, 14);
    assert!(screen.contains(HEADER), "{screen}");
    for status in ["Connecting", "Creating", "Opening", "No session"] {
        assert!(!screen.contains(status), "no status line:\n{screen}");
    }
    assert!(h.app.session.is_none());
    assert!(h.calls("session.create").is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_header_stays_at_the_top_when_the_first_message_creates_the_session() {
    let mut h = draft().await;
    h.submit("hello there");
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    let screen = h.screen(70, 30);
    let row = |text: &str| {
        screen
            .lines()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no {text:?}:\n{screen}"))
    };
    assert!(row(HEADER) < row("hello there"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn alt_enter_in_the_draft_starts_the_session_like_enter() {
    let mut h = draft().await;
    h.app.composer.set_text("hello");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.sends(), ["hello"]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_create_puts_the_text_back_and_enter_tries_again() {
    let mut h = Harness::new();
    h.fake.script_error(
        "session.create",
        -32008,
        "profile not found",
        "profile_not_found",
    );
    h.boot().await;
    h.submit("hello");
    h.settle().await;

    assert_eq!(h.app.composer.text(), "hello");
    assert_eq!(h.notices(), ["profile not found"]);
    assert!(h.app.session.is_none());
    assert!(h.sends().is_empty());

    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("session.create").len(), 2);
}

#[tokio::test(start_paused = true)]
async fn enter_while_the_session_is_being_created_keeps_the_text_in_the_editor() {
    let mut h = draft().await;
    h.fake
        .script_held("session.create", json!({"session": session_info("s1")}));
    h.submit("first");
    h.run_for(Duration::from_millis(50)).await;
    assert!(h.app.session.is_none());
    let screen = h.screen(60, 20);
    assert!(screen.contains(HEADER), "{screen}");
    assert!(screen.contains("Creating the session…"), "{screen}");

    h.app.composer.set_text("second");
    h.press(KeyCode::Enter);
    h.run_for(Duration::from_millis(50)).await;
    assert_eq!(h.calls("session.create").len(), 1, "no second session");
    assert_eq!(
        h.app.composer.text(),
        "second",
        "the text waits in the editor"
    );

    h.fake.release("session.create");
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    assert_eq!(h.calls("session.create").len(), 1);
    assert_eq!(h.sends(), ["first"]);
    assert_eq!(h.app.composer.text(), "second");
}

#[tokio::test(start_paused = true)]
async fn new_returns_to_the_draft_without_creating_and_records_the_profile() {
    let mut h = Harness::new();
    h.fake.script(
        "profile.list",
        json!({"profiles": [
            {"id": "coding", "model": "mock", "reasoning": "medium"},
            {"id": "ask", "model": "mock", "reasoning": "medium"},
        ]}),
    );
    h.fake.script(
        "session.read",
        page(1, 0, vec![user(0, "l0", "an old question")]),
    );
    h.open().await;
    assert_eq!(h.texts(), ["an old question"]);
    let old = h.app.session.as_ref().unwrap().epoch;
    script_first_message(&h);

    h.type_text("/new ask");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert!(h.app.session.is_none());
    assert!(h.calls("session.create").is_empty());
    assert_eq!(h.app.launch.profile.as_deref(), Some("ask"));
    let screen = h.screen(60, 12);
    assert!(!screen.contains("an old question"), "{screen}");
    assert!(screen.contains(HEADER), "{screen}");

    h.action(Action::NewSession);
    assert_eq!(
        h.app.launch.profile.as_deref(),
        Some("ask"),
        "a plain /new keeps the draft's settings"
    );

    h.submit("new question");
    h.until(|app| app.is_busy()).await;
    let create = h.calls("session.create");
    assert_eq!(create.len(), 1);
    assert_eq!(create[0]["profile"], "ask");
    assert_ne!(
        h.app.session.as_ref().unwrap().epoch,
        old,
        "results for the old session cannot reach the new one"
    );
}

#[tokio::test(start_paused = true)]
async fn the_model_and_reasoning_picked_in_the_draft_reach_session_create() {
    let mut h = Harness::new();
    h.fake.script(
        "model.list",
        json!({"models": [
            {"id": "mock", "supported_reasoning": ["low", "medium", "high"]},
            {"id": "luna", "supported_reasoning": ["low", "medium", "high"]},
        ]}),
    );
    script_first_message(&h);
    h.boot().await;
    h.app.default_profile = Some("coding".into());
    let screen = h.screen(80, 12);
    assert!(screen.contains("mock · medium"), "{screen}");

    h.action(Action::CycleModel { forward: true });
    h.action(Action::SetReasoning(Reasoning::High));
    assert_eq!(
        h.notices(),
        ["New session: model luna", "New session: reasoning high"]
    );
    let screen = h.screen(80, 12);
    assert!(screen.contains("luna · high"), "{screen}");
    h.action(Action::SwitchModel("mock".into()));
    h.action(Action::SetModel("luna".into()));
    assert!(h.calls("session.update").is_empty(), "no session to update");

    h.submit("go");
    h.until(|app| app.is_busy()).await;
    let create = h.calls("session.create").remove(0);
    assert_eq!(create["model"], "luna");
    assert_eq!(create["reasoning"], "high");
    assert!(
        create.get("profile").is_none(),
        "the Agent's default applies"
    );
}

#[tokio::test(start_paused = true)]
async fn a_draft_model_without_the_reasoning_level_adjusts_the_one_session_create_gets() {
    let mut h = Harness::new();
    h.fake.script(
        "model.list",
        json!({"models": [
            {"id": "gpt-6.1-sol", "supported_reasoning": ["low", "high", "xhigh"]},
            {"id": "deepseek-v4.1-flash", "supported_reasoning": ["high", "max"]},
        ]}),
    );
    script_first_message(&h);
    h.app.launch = LaunchSettings {
        profile: None,
        model: Some("gpt-6.1-sol".into()),
        reasoning: Some(Reasoning::XHigh),
    };
    h.boot().await;

    h.action(Action::SetModel("deepseek-v4.1-flash".into()));
    assert_eq!(h.app.launch.reasoning, Some(Reasoning::Max));
    assert_eq!(
        h.notices().last().map(String::as_str),
        Some("deepseek-v4.1-flash does not support reasoning xhigh; using max")
    );

    h.submit("go");
    h.until(|app| app.is_busy()).await;
    let create = h.calls("session.create").remove(0);
    assert_eq!(create["model"], "deepseek-v4.1-flash");
    assert_eq!(create["reasoning"], "max");
}

#[tokio::test(start_paused = true)]
async fn commands_that_need_a_session_say_so_in_the_draft() {
    let mut h = draft().await;
    for action in [
        Action::Compact,
        Action::Rename("title".into()),
        Action::Export(None),
        Action::CopyLastAnswer,
        Action::LoadOlder,
    ] {
        h.app.notices.clear();
        h.action(action.clone());
        assert_eq!(h.notices(), [NO_SESSION], "{action:?}");
    }
    for command in ["/context", "/session", "/diff", "/diff workspace"] {
        h.app.notices.clear();
        h.type_text(command);
        h.press(KeyCode::Enter);
        assert_eq!(h.notices(), [NO_SESSION], "{command}");
        assert!(matches!(h.app.overlay, Overlay::None), "{command}");
    }
    h.settle().await;
    for method in [
        "session.compact",
        "session.rename",
        "session.context",
        "session.read",
        "changes.list",
    ] {
        assert!(h.calls(method).is_empty(), "{method}");
    }
}

#[tokio::test(start_paused = true)]
async fn at_files_are_unavailable_in_the_draft() {
    let mut h = draft().await;
    h.type_text("@");
    assert!(matches!(h.app.overlay, Overlay::None));
    assert_eq!(h.app.composer.text(), "@", "the character is still text");
    assert_eq!(
        h.notices(),
        ["@ file completion needs a session: send a message first"]
    );
    h.settle().await;
    assert!(h.calls("workspace.files").is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_sessions_picker_opens_an_existing_session_from_the_draft() {
    let mut h = draft().await;
    h.ctrl(KeyCode::Char('r'));
    h.press(KeyCode::Enter);
    h.until(|app| app.session.is_some()).await;
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s1");
    assert!(h.calls("session.create").is_empty());
}

#[tokio::test(start_paused = true)]
async fn quitting_the_draft_creates_nothing() {
    let mut h = draft().await;
    h.ctrl(KeyCode::Char('d'));
    assert!(h.app.quit);
    h.settle().await;
    assert!(h.calls("session.create").is_empty());
    assert!(h.app.exit_output().is_empty());
}
