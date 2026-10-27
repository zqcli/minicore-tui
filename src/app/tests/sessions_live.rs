//! Sessions left for another keep running in the background and come back as
//! they were.

use crossterm::event::KeyCode;
use serde_json::{Value, json};

use super::harness::*;
use super::remote::{config, phone, request, synced_state};
use crate::app::{Action, LiveStatus, Msg, Overlay, Scroll, Session, SessionOp};
use crate::transcript::Block;

fn turn_in(session: &str, loop_id: &str) -> Value {
    json!({"session_id": session, "loop_id": loop_id})
}

fn waited_in(session: &str, loop_id: &str) -> Value {
    json!({"turn": turn_in(session, loop_id), "outcome": completed()})
}

/// `s1` ("First") open with turn `l1` running; the Agent can open `s2`
/// ("Second") and run turn `l2` in it.
async fn running_first() -> Harness {
    running_first_ending(completed()).await
}

/// `running_first` where the first session's turn ends with `outcome`.
async fn running_first_ending(outcome: Value) -> Harness {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut first = session_info("s1");
    first["title"] = json!("First");
    h.fake
        .script("session.rename@s1", json!({"session": first}));
    h.fake
        .script("turn.send@s1", json!({"turn": turn_in("s1", "l1")}));
    h.fake.script_held(
        "turn.wait@s1",
        json!({"turn": turn_in("s1", "l1"), "outcome": outcome}),
    );
    let mut second = session_info("s2");
    second["title"] = json!("Second");
    h.fake.script("session.open@s2", json!({"session": second}));
    h.fake
        .script("turn.send@s2", json!({"turn": turn_in("s2", "l2")}));
    h.fake.script_held("turn.wait@s2", waited_in("s2", "l2"));
    h.submit("work in one");
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    h
}

/// Shows `id`, live or not, and lets its reads settle.
async fn show(h: &mut Harness, id: &str) {
    h.action(Action::OpenSession(id.into()));
    h.until(|app| {
        app.session
            .as_ref()
            .is_some_and(|s| s.info.session_id == id)
    })
    .await;
    h.settle().await;
}

fn first(h: &Harness) -> &Session {
    h.app.live("s1").expect("s1 is live")
}

fn texts_of(session: &Session) -> Vec<String> {
    session
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } | Block::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn two_sessions_run_at_once_and_switching_back_needs_no_reload() {
    let mut h = running_first().await;
    h.app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    show(&mut h, "s2").await;
    assert_eq!(first(&h).live_status(), LiveStatus::Running);
    h.submit("work in two");
    h.until(|app| app.is_busy()).await;
    let sent: Vec<Value> = h
        .calls("turn.send")
        .iter()
        .map(|call| call["session_id"].clone())
        .collect();
    assert_eq!(sent, [json!("s1"), json!("s2")]);

    h.emit(
        "output_delta",
        json!({"turn": turn_in("s1", "l1"), "request_index": 0, "channel": "text",
               "delta": "Answer from one."}),
    )
    .await;
    h.emit(
        "tool_started",
        json!({"turn": turn_in("s1", "l1"), "request_index": 0, "tool_call_id": "c1",
               "tool_name": "bash"}),
    )
    .await;
    h.settle().await;
    assert!(texts_of(first(&h)).contains(&"Answer from one.".to_owned()));
    assert!(!h.texts().contains(&"Answer from one.".to_owned()));

    let opens = h.calls("session.open").len();
    let reads = h.calls("session.read").len();
    show(&mut h, "s1").await;
    assert!(h.texts().contains(&"Answer from one.".to_owned()));
    assert!(
        h.blocks()
            .iter()
            .any(|block| matches!(block, Block::Tool(_)))
    );
    assert_eq!(h.calls("session.open").len(), opens);
    assert_eq!(h.calls("session.read").len(), reads);
    assert_eq!(
        h.app.scroll,
        Scroll::Anchor {
            block: 0,
            offset: 0
        }
    );
    assert_eq!(h.app.background[0].info.session_id, "s2");
}

#[tokio::test(start_paused = true)]
async fn a_background_turn_ends_there_sends_its_follow_up_and_is_marked_new() {
    let mut h = running_first().await;
    h.app
        .session
        .as_mut()
        .unwrap()
        .push_follow_up("next for one".into());
    show(&mut h, "s2").await;
    h.fake.release("turn.wait@s1");
    // The follow-up's own turn keeps running.
    h.fake
        .script("turn.send@s1", json!({"turn": turn_in("s1", "l3")}));
    h.fake.script_held("turn.wait@s1", waited_in("s1", "l3"));
    h.until(|app| app.live("s1").is_some_and(|s| s.unseen))
        .await;
    h.settle().await;
    let ended = first(&h)
        .transcript
        .blocks()
        .iter()
        .any(|block| matches!(block, Block::TurnEnd { .. }));
    assert!(ended);
    assert!(
        h.notices()
            .iter()
            .any(|notice| notice.starts_with("\"First\" finished in")),
        "{:?}",
        h.notices()
    );
    assert_eq!(h.sends(), ["work in one", "next for one"]);
    assert_eq!(h.calls("turn.send")[1]["session_id"], "s1");
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s2");
    show(&mut h, "s1").await;
    assert!(!h.app.session.as_ref().unwrap().unseen);
}

#[tokio::test(start_paused = true)]
async fn a_background_turn_that_failed_names_the_reason_and_the_provider_status() {
    let mut h = running_first_ending(json!({"type": "failed", "kind": "model",
        "model_error": {"kind": "rate_limited", "provider": {"status": 429}}}))
    .await;
    show(&mut h, "s2").await;
    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| s.unseen))
        .await;
    h.settle().await;
    assert!(
        h.notices()
            .contains(&"\"First\" failed · rate limited (429)".to_owned()),
        "{:?}",
        h.notices()
    );
}

fn approval_for(session: &str, id: &str) -> Value {
    json!({"turn": turn_in(session, "l1"), "interaction": {
        "interaction_id": id, "tool_call_id": format!("c-{id}"), "tool_name": "bash",
        "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}}})
}

#[tokio::test(start_paused = true)]
async fn a_background_approval_notifies_and_is_answered_only_once_shown() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    h.emit("interaction_requested", approval_for("s1", "i1"))
        .await;
    h.settle().await;
    assert!(
        h.notices()
            .contains(&"\"First\" needs approval · ctrl+r".to_owned()),
        "{:?}",
        h.notices()
    );
    assert!(h.screen(100, 20).contains("1 needs approval"));
    let terminal = h.app.terminal_bytes();
    assert!(terminal.contains("approval"), "{terminal:?}");
    assert!(terminal.ends_with('\x07'), "the bell rings: {terminal:?}");

    h.press(KeyCode::Char('y'));
    h.settle().await;
    assert!(h.calls("interaction.answer").is_empty());
    assert_eq!(h.app.composer.text(), "y");

    show(&mut h, "s1").await;
    assert!(h.screen(100, 24).contains("Allow once"));
    h.press(KeyCode::Char('y'));
    h.settle().await;
    let answers = h.calls("interaction.answer");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["session_id"], "s1");
    assert_eq!(answers[0]["interaction_id"], "i1");

    show(&mut h, "s2").await;
    assert_eq!(h.app.composer.text(), "y", "each session keeps its draft");
    h.app.background[0].allowed_tools.insert("bash".into());
    h.emit("interaction_requested", approval_for("s1", "i2"))
        .await;
    h.settle().await;
    let answers = h.calls("interaction.answer");
    assert_eq!(
        answers.len(),
        2,
        "an allowed tool is answered in the background"
    );
    assert_eq!(answers[1]["interaction_id"], "i2");
    assert!(
        !h.notices().iter().any(|text| text.contains("Allowed")),
        "{:?}",
        h.notices()
    );
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s2");
}

#[tokio::test(start_paused = true)]
async fn refused_background_text_and_drafts_return_with_their_session() {
    let mut h = running_first().await;
    h.app
        .session
        .as_mut()
        .unwrap()
        .push_follow_up("next for one".into());
    h.app.composer.set_text("draft in one");
    show(&mut h, "s2").await;
    assert_eq!(h.app.composer.text(), "");
    h.app.composer.set_text("draft in two");
    h.fake
        .script_error("turn.send@s1", -32000, "refused", "invalid_state");
    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| !s.returned.is_empty()))
        .await;
    assert_eq!(h.app.composer.text(), "draft in two");
    show(&mut h, "s1").await;
    assert_eq!(h.app.composer.text(), "draft in one\n\nnext for one");
    show(&mut h, "s2").await;
    assert_eq!(h.app.composer.text(), "draft in two");
}

#[tokio::test(start_paused = true)]
async fn the_draft_keeps_its_text_while_a_session_is_shown() {
    let mut h = Harness::new();
    h.boot().await;
    h.app.composer.set_text("draft words");
    show(&mut h, "s1").await;
    assert_eq!(h.app.composer.text(), "");
    h.action(Action::NewSession);
    assert!(h.app.session.is_none());
    assert_eq!(h.app.composer.text(), "draft words");
    assert_eq!(h.app.background[0].info.session_id, "s1");
}

#[tokio::test(start_paused = true)]
async fn an_older_page_for_a_background_session_leaves_the_screen_alone() {
    let mut h = Harness::opened(page(80, 0, vec![assistant(79, "l9", "latest")])).await;
    h.fake
        .script("session.open@s2", json!({"session": session_info("s2")}));
    show(&mut h, "s2").await;
    let anchored = Scroll::Anchor {
        block: 1,
        offset: 0,
    };
    h.app.scroll = anchored;
    h.app.overlay = Overlay::Find {
        query: "first".into(),
        matches: Vec::new(),
        current: 0,
    };
    h.fake
        .script("session.read@s1", page(80, 0, vec![user(0, "l0", "first")]));
    h.app.with_background(0, |app| app.load_older());
    h.settle().await;
    assert_eq!(texts_of(first(&h)), ["first", "latest"]);
    assert_eq!(h.app.scroll, anchored);
    assert!(
        matches!(&h.app.overlay, Overlay::Find { matches, .. } if matches.is_empty()),
        "the foreground's find is not run over the background transcript"
    );
}

/// Opens the picker and presses Ctrl+D on `id`'s row.
fn delete_from_picker(h: &mut Harness, id: &str) {
    h.ctrl(KeyCode::Char('r'));
    let Overlay::Sessions { rows, .. } = &h.app.overlay else {
        panic!("the picker is open");
    };
    let at = rows.iter().position(|row| row.session_id == id).unwrap();
    for _ in 0..at {
        h.press(KeyCode::Down);
    }
    h.ctrl(KeyCode::Char('d'));
}

#[tokio::test(start_paused = true)]
async fn a_working_background_session_cannot_be_deleted_and_an_idle_one_can() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    delete_from_picker(&mut h, "s1");
    assert_eq!(h.notices(), ["Stop \"First\" first: open it and press Esc"]);
    h.settle().await;
    assert!(h.calls("session.delete").is_empty());

    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| !s.is_busy()))
        .await;
    h.settle().await;
    let epoch = first(&h).epoch;
    h.press(KeyCode::Esc);
    delete_from_picker(&mut h, "s1");
    h.ctrl(KeyCode::Char('d'));
    h.settle().await;
    assert_eq!(h.calls("session.delete")[0]["session_id"], "s1");
    assert!(h.app.live("s1").is_none());
    assert!(h.app.background.is_empty());
    // A late result for the deleted session goes nowhere.
    h.app.on_msg(Msg::Session(
        epoch,
        Box::new(SessionOp::Tail(Ok(crate::app::tasks::History {
            items: Vec::new(),
            from: 0,
            floor: 0,
            has_more: false,
            canonical: false,
            records: Vec::new(),
            covered_usage: None,
        }))),
    ));
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s2");
}

#[tokio::test(start_paused = true)]
async fn the_picker_lists_live_sessions_first() {
    let mut h = running_first().await;
    let mut other = session_info("s3");
    other["updated_at"] = json!("2030-01-01T00:00:00Z");
    h.app.sessions.push(serde_json::from_value(other).unwrap());
    show(&mut h, "s2").await;
    h.ctrl(KeyCode::Char('r'));
    let Overlay::Sessions { rows, .. } = &h.app.overlay else {
        panic!("the picker is open");
    };
    let order: Vec<&str> = rows.iter().map(|row| row.session_id.as_str()).collect();
    assert_eq!(order, ["s2", "s1", "s3"]);
    let screen = h.screen(100, 24);
    assert!(screen.contains("running 0m"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn the_phone_sees_live_sessions_and_switches_to_one_at_once() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    h.app.remote.config = Some(config(std::path::PathBuf::from("/tmp/ws")));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    let state = synced_state(&mut h, &mut output).expect("a snapshot");
    let listed = |state: &Value, id: &str| {
        state["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"] == id)
            .cloned()
            .unwrap()
    };
    let one = listed(&state, "s1");
    assert_eq!(
        (&one["live"], &one["status"], &one["unseen"]),
        (&json!(true), &json!("running"), &json!(false))
    );
    assert!(one["started_ms"].as_u64().is_some());
    assert_eq!(listed(&state, "s2")["status"], "idle");
    assert_eq!(state["background"], json!({"running": 1, "approval": 0}));

    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert!(
        synced_state(&mut h, &mut output).is_none(),
        "time alone sends nothing"
    );

    h.emit("interaction_requested", approval_for("s1", "i1"))
        .await;
    h.settle().await;
    let state = synced_state(&mut h, &mut output).expect("a new state");
    assert_eq!(listed(&state, "s1")["status"], "approval");
    assert_eq!(state["background"], json!({"running": 0, "approval": 1}));

    let opens = h.calls("session.open").len();
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"}));
    while let Ok(message) = output.try_recv() {
        if message["t"] == "ack" {
            assert_eq!(message["ok"], true);
        }
    }
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s1");
    assert_eq!(h.calls("session.open").len(), opens);
}

#[tokio::test(start_paused = true)]
async fn many_large_background_sessions_add_no_work_to_a_frame() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    for n in 0..20 {
        let id = format!("b{n}");
        let info: crate::protocol::SessionInfo = serde_json::from_value(session_info(&id)).unwrap();
        let mut session = Session::new(info.clone(), 100 + n);
        for i in 0..500 {
            session.transcript.push(Block::User {
                text: format!("message {i}"),
                kind: crate::protocol::read::UserKind::Prompt,
                at: None,
            });
        }
        h.app.background.push(session);
        h.app.sessions.push(info);
    }
    h.app.catalog_generation += 1;
    h.app.remote.config = Some(config(std::path::PathBuf::from("/tmp/ws")));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    let snapshot = synced_state(&mut h, &mut output).expect("a snapshot");
    assert_eq!(snapshot["background"], json!({"running": 0, "approval": 0}));
    h.screen(80, 24);
    assert!(
        h.app.layout.total_rows() < 50,
        "only the foreground is laid out"
    );
    h.app.sync_remote();
    assert!(
        output.try_recv().is_err(),
        "an unchanged frame sends nothing"
    );
}
