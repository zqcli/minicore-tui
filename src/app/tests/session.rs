use std::time::Duration;

use crossterm::event::KeyCode;
use serde_json::json;

use super::harness::*;
use crate::app::{Action, LaunchSettings};
use crate::protocol::Reasoning;

#[tokio::test(start_paused = true)]
async fn a_new_session_carries_only_the_launch_flags_given() {
    let cases = [
        (LaunchSettings::default(), json!({})),
        (
            LaunchSettings {
                profile: Some("coding".into()),
                model: Some("deep".into()),
                reasoning: Some(Reasoning::High),
            },
            json!({"profile": "coding", "model": "deep", "reasoning": "high"}),
        ),
    ];
    for (launch, expected) in cases {
        let mut h = Harness::new();
        h.fake
            .script("session.create", json!({"session": session_info("s1")}));
        h.fake.script("input.push", started("l1"));
        h.app.launch = launch;
        h.boot().await;
        let workspace = h.app.workspace.clone();
        h.submit("hello");
        h.until(|app| app.session.is_some()).await;
        let create = h.calls("session.create").remove(0);
        assert_eq!(create["workspace"], json!(workspace));
        for field in ["profile", "model", "reasoning"] {
            assert_eq!(create.get(field), expected.get(field), "{field}: {create}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn launch_flags_apply_to_an_opened_session_once() {
    let mut h = Harness::new();
    h.app.launch = LaunchSettings {
        profile: None,
        model: Some("deep".into()),
        reasoning: Some(Reasoning::Max),
    };
    let mut updated = session_info("s1");
    updated["model"] = json!("deep");
    updated["reasoning"] = json!("max");
    h.fake.script("session.update", json!({"session": updated}));
    h.open().await;
    let update = h.calls("session.update");
    assert_eq!(update.len(), 1);
    assert_eq!(update[0]["model"], "deep");
    assert_eq!(update[0]["reasoning"], "max");
    assert_eq!(h.app.session.as_ref().unwrap().info.model, "deep");
}

#[tokio::test(start_paused = true)]
async fn a_closed_agent_shows_the_fatal_screen() {
    let mut h = Harness::new();
    h.boot().await;
    h.fake.close();
    h.until(|app| app.fatal.is_some()).await;
    let screen = h.screen(60, 12);
    assert!(screen.contains("Fatal error"), "{screen}");
    assert!(screen.contains("Press q to quit"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn a_reply_for_a_session_that_is_no_longer_open_is_dropped() {
    let mut h = Harness::new();
    h.fake.script_sequence(
        "session.open",
        vec![
            json!({"session": session_info("s1")}),
            json!({"session": session_info("s2")}),
        ],
    );
    h.fake.script_sequence_later(
        "session.read",
        vec![
            (display_read(1, 0), Duration::from_secs(1)),
            (display_read(1, 0), Duration::ZERO),
            (
                page(1, 0, vec![user(0, "l9", "second session")]),
                Duration::ZERO,
            ),
        ],
    );
    h.fake.script(
        "session.read",
        page(1, 0, vec![user(0, "l0", "first session")]),
    );
    h.boot().await;
    h.action(Action::OpenSession("s1".into()));
    h.until(|app| app.session.is_some()).await;
    h.action(Action::OpenSession("s2".into()));
    h.until(|app| {
        app.session
            .as_ref()
            .is_some_and(|session| session.info.session_id == "s2")
    })
    .await;
    h.settle().await;
    assert_eq!(h.texts(), ["second session"]);
}

#[tokio::test(start_paused = true)]
async fn copy_never_takes_the_answer_of_the_previous_session() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("question");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 0, "channel": "text", "delta": "an answer"}),
    )
    .await;
    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;

    h.fake
        .script("session.create", json!({"session": session_info("s2")}));
    h.fake.script("input.push", started("l2"));
    h.fake.script_held("turn.wait", waited("l2", completed()));
    h.action(Action::NewSession);
    h.submit("a new question");
    h.until(|app| {
        app.session
            .as_ref()
            .is_some_and(|session| session.info.session_id == "s2")
    })
    .await;
    h.action(Action::CopyLastAnswer);
    assert_eq!(h.notices(), ["No answer to copy yet"]);
}

#[tokio::test(start_paused = true)]
async fn the_sessions_picker_names_and_filters_by_title_or_first_prompt() {
    let mut h = Harness::new();
    let mut closed = session_info("ses_abcdef0123456789");
    closed["loaded"] = json!(false);
    h.fake.script(
        "session.list",
        json!({"sessions": [session_info("s1"), closed]}),
    );
    h.fake.script(
        "session.read",
        page(
            1,
            0,
            vec![user(0, "l0", "fix the greeting\nand run the tests")],
        ),
    );
    h.open().await;

    h.ctrl(KeyCode::Char('r'));
    let screen = h.screen(80, 20);
    assert!(screen.contains("› fix the greeting"), "{screen}");
    assert!(screen.contains("ses_abcdef "), "{screen}");
    h.type_text("greet");
    let screen = h.screen(80, 20);
    assert!(screen.contains("fix the greeting"), "{screen}");
    assert!(!screen.contains("ses_abcdef"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn ctrl_d_twice_deletes_the_row_the_filtered_picker_shows() {
    let mut h = Harness::new();
    let titled = |id: &str, title: &str| {
        let mut info = session_info(id);
        info["title"] = json!(title);
        info["loaded"] = json!(false);
        info
    };
    h.fake.script(
        "session.list",
        json!({"sessions": [session_info("s1"), titled("s_alpha", "alpha"),
                            titled("s_beta", "beta")]}),
    );
    h.fake.script("session.close", json!({"ok": true}));
    h.fake.script("session.delete", json!({"ok": true}));
    h.open().await;

    h.ctrl(KeyCode::Char('r'));
    h.type_text("beta");
    h.ctrl(KeyCode::Char('d'));
    assert_eq!(
        h.notices(),
        ["Press Ctrl+D again to delete \"beta\" permanently"]
    );
    h.ctrl(KeyCode::Char('d'));
    h.settle().await;
    let deleted = h.calls("session.delete");
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0]["session_id"], "s_beta");
    assert!(h.notices().is_empty(), "the row leaving the picker says it");
    let screen = h.screen(80, 20);
    assert_eq!(
        screen.matches("beta").count(),
        1,
        "only the typed filter is left:\n{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_sessions_picker_lists_this_workspace_first_then_newest() {
    let mut h = Harness::new();
    let titled = |id: &str, title: &str, workspace: &str, updated: &str| {
        let mut info = session_info(id);
        info["title"] = json!(title);
        info["workspace"] = json!(workspace);
        info["updated_at"] = json!(updated);
        info
    };
    let here = session_info("s1")["workspace"].as_str().unwrap().to_owned();
    h.fake.script(
        "session.list",
        json!({"sessions": [
            session_info("s1"),
            titled("s_new_elsewhere", "elsewhere new", "/other", "2025-01-02T00:00:00Z"),
            titled("s_old_here", "here old", &here, "2025-01-01T00:00:00Z"),
            titled("s_new_here", "here new", &here, "2025-01-03T00:00:00Z"),
        ]}),
    );
    h.open().await;

    h.ctrl(KeyCode::Char('r'));
    let screen = h.screen(80, 20);
    let order: Vec<usize> = ["here new", "here old", "elsewhere new"]
        .iter()
        .map(|title| {
            screen
                .find(title)
                .unwrap_or_else(|| panic!("{title}:\n{screen}"))
        })
        .collect();
    assert!(order.is_sorted(), "{screen}");
}
