//! Sessions left for another keep running in the background and come back as
//! they were.

use std::time::Duration;

use crossterm::event::KeyCode;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::harness::*;
use super::phone::{connect_phone, enable_remote, request, synced_state};
use crate::app::{Action, LiveStatus, Msg, Overlay, Scroll, Session, SessionOp};
use crate::protocol::read::UserKind;
use crate::title::LONG_TURN;
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
    running_first_with(
        json!({"turn": turn_in("s1", "l1"), "outcome": outcome}),
        crate::testing::snapshot("s1", crate::testing::LOAD, 100),
    )
    .await
}

/// `running_first` where the first session's turn ends with the `turn.wait` answer `wait`.
async fn running_first_with(wait: Value, finished: Value) -> Harness {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut first = session_info("s1");
    first["title"] = json!("First");
    h.fake
        .script("session.rename@s1", json!({"session": first}));
    h.fake
        .script("input.push@s1", starting_reply("l1", h.next_revision()));
    h.fake
        .script_held("turn.wait@s1", after_snapshot(wait, finished));
    let mut second = session_info("s2");
    second["title"] = json!("Second");
    h.fake.script("session.open@s2", opened_session(second));
    h.fake.script(
        "input.push@s2",
        after_snapshot(
            accepted("s2", "i1"),
            crate::testing::snapshot_running(
                "s2",
                crate::testing::LOAD,
                2,
                "l2",
                entry("i1", "steer", "$text"),
            ),
        )
        .with_request_text(),
    );
    h.fake.script_held("turn.wait@s2", waited_in("s2", "l2"));
    h.submit("work in one");
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    h
}

/// Shows `id`, live or not, and lets its reads settle.
async fn show(h: &mut Harness, id: &str) {
    if h.app.live(id).is_none() {
        let mut read = display_read(0, 0);
        read["session"] = session_info(id);
        h.fake.script(&format!("session.read@{id}"), history(read));
        h.fake.script(
            &format!("session.state@{id}"),
            state_response(crate::testing::snapshot(id, crate::testing::LOAD, 1)),
        );
        h.fake
            .script(&format!("session.context@{id}"), json!({"session_id":id}));
    }
    h.action(Action::OpenSession(id.into()));
    h.until(|app| {
        app.resident
            .current()
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
async fn two_global_recovery_slots_rotate_busy_residents_when_snapshot_availability_changes() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    for id in ["s2", "s3"] {
        h.fake.script(
            &format!("session.open@{id}"),
            opened_session(session_info(id)),
        );
        h.fake
            .script(&format!("session.read@{id}"), history(display_read(0, 0)));
        show(&mut h, id).await;
    }
    for id in ["s1", "s2", "s3"] {
        h.fake.script(
            &format!("session.rename@{id}"),
            json!({"session":session_info(id)}),
        );
    }
    let snapshot_for = |id: &str, revision: u64, pressure: bool| {
        let mut state = crate::testing::snapshot_running(
            id,
            crate::testing::LOAD,
            revision,
            &format!("{id}-running"),
            entry(
                &format!("{id}-input"),
                "steer",
                &format!("{id} still working"),
            ),
        );
        state["completions"] = json!({"through_seq":4,"retained_from_seq":1,"truncated":false,"entries":(1..=4).map(|seq|json!({"seq":seq,"loop_id":format!("{id}-finished-{seq}"),"completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":if pressure && id=="s2" && seq==1 {"unavailable"}else{"live"},"unavailable_reason":if pressure && id=="s2" && seq==1 {json!("resource_limit")}else{Value::Null}})).collect::<Vec<_>>()});
        state
    };
    for id in ["s1", "s2", "s3"] {
        h.fake.script_held(
            &format!("turn.wait@{id}"),
            waited_in(id, &format!("{id}-running")),
        );
        for seq in 1..=4 {
            let loop_id = format!("{id}-finished-{seq}");
            let response = if id == "s2" && seq == 1 {
                json!({"turn":turn_in(id,&loop_id),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":"resource_limit","persistence":"failed","items":[],"total":0,"next_cursor":null})
            } else {
                let mut result = turn_result(
                    &loop_id,
                    vec![
                        user(0, &loop_id, &format!("{id} prompt {seq}")),
                        assistant(1, &loop_id, &format!("{id} answer {seq}")),
                    ],
                );
                result["turn"] = turn_in(id, &loop_id);
                result["outcome"] = completed();
                result["persistence"] = json!("failed");
                result["availability"] = json!("live");
                result
            };
            h.fake
                .script_held(&format!("turn.result@{id}/{loop_id}"), response);
        }
        h.snapshot_event(snapshot_for(id, 100, false)).await;
        h.until(|app| app.live(id).unwrap().recovery.revision == 100)
            .await;
    }
    h.fake.wait_for_calls("turn.result", 2).await;
    assert_eq!(
        h.calls("turn.result")
            .iter()
            .map(|call| call["session_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["s1", "s1"]
    );
    h.snapshot_event(snapshot_for("s2", 101, true)).await;
    h.until(|app| app.live("s2").unwrap().recovery.revision == 101)
        .await;
    for seq in 1..=2 {
        h.fake.release(&format!("turn.result@s1/s1-finished-{seq}"));
    }
    h.until(|app| app.live("s1").unwrap().recovery.settled_through >= 2)
        .await;
    h.fake.wait_for_calls("turn.result", 4).await;
    let admitted = h
        .calls("turn.result")
        .iter()
        .skip(2)
        .map(|call| call["session_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        admitted,
        ["s2", "s3"],
        "a busy resident's backlog cannot take both freed slots ahead of another resident"
    );
    for revision in 102..=110 {
        for id in ["s1", "s2", "s3"] {
            h.snapshot_event(snapshot_for(id, revision, true)).await;
        }
        h.until(|app| {
            ["s1", "s2", "s3"]
                .iter()
                .all(|id| app.live(id).unwrap().recovery.revision == revision)
        })
        .await;
        let sessions = h.app.resident.iter().collect::<Vec<_>>();
        assert!(sessions.iter().all(|session| session.is_busy()));
        assert!(
            sessions
                .iter()
                .map(|session| session.recovery_tasks.len())
                .sum::<usize>()
                <= 2
        );
        let settled = sessions
            .iter()
            .map(|session| session.recovery.settled_through)
            .sum::<u64>();
        if settled == 12 {
            break;
        }
        for call in h.calls("turn.result") {
            h.fake.release(&format!(
                "turn.result@{}/{}",
                call["session_id"].as_str().unwrap(),
                call["loop_id"].as_str().unwrap()
            ));
        }
        h.until(|app| {
            app.resident
                .iter()
                .map(|session| session.recovery.settled_through)
                .sum::<u64>()
                > settled
        })
        .await;
    }
    h.until(|app| {
        ["s1", "s2", "s3"]
            .iter()
            .all(|id| app.live(id).unwrap().recovery.settled_through == 4)
    })
    .await;
    assert_eq!(h.calls("turn.result").len(), 12);
    assert_eq!(
        h.app.live("s2").unwrap().body_recovery[0].status,
        crate::app::recovery::RecoveryStatus::Unavailable
    );
    for id in ["s1", "s2", "s3"] {
        assert_eq!(h.app.live(id).unwrap().recovery_tasks.len(), 0);
    }
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
        .calls("input.push")
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
    assert_eq!(
        h.app.resident.others().next().unwrap().info.session_id,
        "s2"
    );
}

#[tokio::test(start_paused = true)]
async fn a_background_turn_ends_there_and_waits_for_the_successor_the_agent_started() {
    let successor = crate::testing::snapshot_running(
        "s1",
        crate::testing::LOAD,
        100,
        "l3",
        entry("n1", "follow_up", "next for one"),
    );
    let mut h = running_first_with(waited_in("s1", "l1"), successor).await;
    h.mirror(vec![entry("f1", "follow_up", "next for one")], false);
    show(&mut h, "s2").await;
    h.fake.release("turn.wait@s1");
    // The Agent's successor turn keeps running.
    h.fake.script_held("turn.wait@s1", waited_in("s1", "l3"));
    h.until(|app| app.live("s1").is_some_and(|s| s.unseen))
        .await;
    h.settle().await;
    let shape: Vec<&str> = first(&h)
        .transcript
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User { .. } => "user",
            Block::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["user", "end", "user"]);
    assert_eq!(texts_of(first(&h)), ["work in one", "next for one"]);
    let running = first(&h).live.as_ref().expect("the successor runs in s1");
    assert_eq!(running.turn.loop_id, "l3");
    let waits: Vec<(Value, Value)> = h
        .calls("turn.wait")
        .iter()
        .map(|call| (call["session_id"].clone(), call["loop_id"].clone()))
        .collect();
    assert_eq!(
        waits,
        [(json!("s1"), json!("l1")), (json!("s1"), json!("l3"))]
    );
    assert!(
        h.notices()
            .iter()
            .any(|notice| notice.starts_with("\"First\" finished in")),
        "{:?}",
        h.notices()
    );
    assert_eq!(
        h.sends(),
        ["work in one"],
        "the Agent started it, not the TUI"
    );
    assert_eq!(h.session().info.session_id, "s2");
    show(&mut h, "s1").await;
    assert!(!h.session().unseen);
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

async fn ask_approval(h: &mut Harness, asked: Value) {
    let id = asked["turn"]["session_id"].as_str().unwrap();
    let loop_id = asked["turn"]["loop_id"].as_str().unwrap();
    let revision = h.app.live(id).unwrap().recovery.revision + 1;
    let mut running = crate::testing::snapshot_running(
        id,
        crate::testing::LOAD,
        revision,
        loop_id,
        entry(
            "i1",
            "steer",
            if id == "s1" {
                "work in one"
            } else {
                "work in two"
            },
        ),
    );
    let mut resumed = running.clone();
    resumed["revision"] = json!(revision + 1);
    h.fake.script(
        &format!("interaction.answer@{id}"),
        after_snapshot(json!({}), resumed),
    );
    let mut pending = asked["interaction"].clone();
    pending["loop_id"] = json!(loop_id);
    pending["request_index"] = json!(0);
    running["pending_interaction"] = pending;
    h.fake.script(
        &format!("session.state@{id}"),
        state_response(running.clone()),
    );
    h.snapshot_event(running).await;
}

#[tokio::test(start_paused = true)]
async fn a_background_approval_notifies_and_is_answered_only_once_shown() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    ask_approval(&mut h, approval_for("s1", "i1")).await;
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
    h.app
        .resident
        .iter_mut()
        .find(|session| session.info.session_id == "s1")
        .unwrap()
        .allowed_tools
        .insert("bash".into());
    ask_approval(&mut h, approval_for("s1", "i2")).await;
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
    assert_eq!(h.session().info.session_id, "s2");
}

#[tokio::test(start_paused = true)]
async fn switching_away_from_a_long_turn_is_silent_and_its_end_rings_once() {
    let mut h = running_first().await;
    tokio::time::advance(LONG_TURN).await;
    h.bells();
    show(&mut h, "s2").await;
    assert_eq!(h.bells(), 0, "nothing ended");
    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| s.unseen))
        .await;
    assert_eq!(h.bells(), 1);
    assert_eq!(h.bells(), 0);
}

/// The bells when the foreground turn of `running_first` ends `run` later.
async fn bells_when_the_foreground_turn_ends_after(run: Duration) -> usize {
    let mut h = running_first().await;
    tokio::time::advance(run).await;
    h.bells();
    h.fake.release("turn.wait@s1");
    h.until(|app| {
        !app.is_busy() && app.transcript_blocks().iter().any(|block| {
        matches!(block, Block::TurnEnd { loop_id, duration: Some(_), .. } if loop_id == "l1")
    })
    })
    .await;
    h.bells()
}

#[tokio::test(start_paused = true)]
async fn a_foreground_turn_rings_when_it_ends_only_after_a_long_run() {
    assert_eq!(
        bells_when_the_foreground_turn_ends_after(LONG_TURN).await,
        1
    );
    assert_eq!(
        bells_when_the_foreground_turn_ends_after(Duration::ZERO).await,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn a_long_turn_asking_for_approval_rings_while_a_background_approval_waits() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    h.submit("work in two");
    h.until(|app| app.is_busy()).await;
    ask_approval(&mut h, approval_for("s1", "i1")).await;
    h.settle().await;
    assert_eq!(h.bells(), 1, "the background approval rings");

    tokio::time::advance(LONG_TURN).await;
    let mut asked = approval_for("s2", "i2");
    asked["turn"] = turn_in("s2", "l2");
    asked["meta"] = json!({"session_id": "s2", "dropped_before": 0});
    ask_approval(&mut h, asked).await;
    h.settle().await;
    assert_eq!(h.bells(), 1, "the foreground approval rings too");
}

#[tokio::test(start_paused = true)]
async fn a_young_turn_asking_for_approval_does_not_ring() {
    let mut h = running_first().await;
    ask_approval(&mut h, approval_for("s1", "i1")).await;
    h.settle().await;
    assert!(h.app.has_interaction());
    assert_eq!(h.bells(), 0);
}

#[tokio::test(start_paused = true)]
async fn text_refused_after_leaving_a_session_is_retained_without_overwriting_either_draft() {
    let mut h = running_first().await;
    h.fake
        .script_error_held("input.push@s1", -32000, "refused", "invalid_state");
    h.submit("next for one");
    h.app.composer.set_text("draft in one");
    show(&mut h, "s2").await;
    assert_eq!(h.app.composer.text(), "");
    h.app.composer.set_text("draft in two");
    h.fake.release("input.push@s1");
    h.until(|app| {
        app.local_inputs.iter().any(|input| {
            input.target.session_id == "s1"
                && input.status == crate::app::unconfirmed::Status::Rejected
        })
    })
    .await;
    assert_eq!(h.app.composer.text(), "draft in two");
    show(&mut h, "s1").await;
    assert_eq!(h.app.composer.text(), "draft in one");
    let rejected = h
        .app
        .local_inputs
        .iter()
        .find(|input| input.target.session_id == "s1")
        .unwrap();
    assert_eq!(rejected.entry.text(), "next for one");
    assert_eq!(rejected.status, crate::app::unconfirmed::Status::Rejected);
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
    assert!(h.app.resident.current().is_none());
    assert_eq!(h.app.composer.text(), "draft words");
    assert_eq!(
        h.app.resident.others().next().unwrap().info.session_id,
        "s1"
    );
}

#[tokio::test(start_paused = true)]
async fn an_older_page_for_a_background_session_leaves_the_screen_alone() {
    let mut h = Harness::opened(page(80, 0, vec![assistant(79, "l9", "latest")])).await;
    h.fake
        .script("session.open@s2", opened_session(session_info("s2")));
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
    h.fake.script(
        "session.read@s1",
        history(page(80, 0, vec![user(0, "l0", "first")])),
    );
    let epoch = h.app.resident.others().next().unwrap().epoch;
    h.app.load_older_for(epoch);
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
async fn a_working_background_session_cannot_be_deleted() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    delete_from_picker(&mut h, "s1");
    assert_eq!(h.notices(), ["Stop \"First\" first: open it and press Esc"]);
    h.settle().await;
    assert!(h.calls("session.delete").is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_idle_background_session_with_queued_messages_cannot_be_deleted() {
    let queued = queue(vec![entry("i2", "follow_up", "after the stop")], true);
    let mut finished = crate::testing::snapshot("s1", crate::testing::LOAD, 100);
    finished["queue"] = queued.clone();
    let mut h = running_first_with(waited("l1", cancelled()), finished).await;
    h.fake.script("turn.result", turn_result("l1", vec![]));
    show(&mut h, "s2").await;
    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| !s.is_busy()))
        .await;
    h.settle().await;
    assert_eq!(first(&h).live_status(), LiveStatus::Idle);
    delete_from_picker(&mut h, "s1");
    assert_eq!(
        h.notices().last().map(String::as_str),
        Some("Clear the queued messages of \"First\" first")
    );
    h.settle().await;
    assert!(h.calls("session.delete").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_queue_snapshot_updates_the_mirror_of_a_background_session_only() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    let mut snapshot = crate::testing::snapshot_running(
        "s1",
        crate::testing::LOAD,
        first(&h).recovery.revision + 1,
        "l1",
        entry("i1", "steer", "work in one"),
    );
    snapshot["queue"] = queue(vec![entry("i1", "follow_up", "later")], false);
    h.snapshot_event(snapshot).await;
    h.settle().await;
    assert_eq!(first(&h).queue.items[0].text, "later");
    assert!(
        h.queue().is_empty(),
        "the foreground session is not touched"
    );
}

#[tokio::test(start_paused = true)]
async fn an_idle_background_session_is_deleted_and_a_late_result_for_it_goes_nowhere() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| !s.is_busy()))
        .await;
    h.settle().await;
    let epoch = first(&h).epoch;
    delete_from_picker(&mut h, "s1");
    h.ctrl(KeyCode::Char('d'));
    h.settle().await;
    assert_eq!(
        h.calls("session.close"),
        [json!({
            "session_id":"s1", "load":crate::testing::LOAD,
        })]
    );
    assert_eq!(h.calls("session.delete"), [json!({"session_id":"s1"})]);
    assert!(h.app.live("s1").is_none());
    assert!(h.app.resident.others().next().is_none());

    h.session_mut().transcript.push(Block::User {
        text: "kept".into(),
        kind: UserKind::Prompt,
        at: None,
    });
    let late = crate::app::read::read_history(
        h.fake.client.clone(),
        "s1".into(),
        crate::app::read::Window::Tail,
    )
    .await
    .unwrap();
    h.app
        .on_msg(Msg::Session(epoch, Box::new(SessionOp::Tail(Ok(late)))));
    assert_eq!(
        h.texts(),
        ["kept"],
        "the foreground transcript is untouched"
    );
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
async fn an_untitled_background_session_is_found_and_named_by_its_first_prompt() {
    let mut h = Harness::opened(page(1, 0, vec![user(0, "l0", "refactor the parser")])).await;
    h.fake
        .script("session.open@s2", opened_session(session_info("s2")));
    h.fake
        .script("session.read@s2", history(display_read(0, 0)));
    show(&mut h, "s2").await;
    h.ctrl(KeyCode::Char('r'));
    h.type_text("parser");
    let screen = h.screen(100, 24);
    assert!(screen.contains("1 of 2"), "{screen}");
    assert!(screen.contains("refactor the parser"), "{screen}");
    h.ctrl(KeyCode::Char('d'));
    assert_eq!(
        h.notices(),
        ["Press Ctrl+D again to delete \"refactor the parser\" permanently"]
    );
}

#[tokio::test(start_paused = true)]
async fn the_picker_finds_a_session_by_its_workspace_folder() {
    let mut h = Harness::new();
    let mut elsewhere = session_info("s_alpha");
    elsewhere["title"] = json!("alpha");
    elsewhere["workspace"] = json!(std::path::Path::new("elsewhere").join("billing"));
    elsewhere["loaded"] = json!(false);
    h.fake.script(
        "session.list",
        json!({"sessions": [session_info("s1"), elsewhere]}),
    );
    h.open().await;
    h.ctrl(KeyCode::Char('r'));
    h.type_text("billing");
    let screen = h.screen(100, 24);
    assert!(screen.contains("1 of 2"), "{screen}");
    assert!(screen.contains("alpha"), "{screen}");
}

/// A phone that has just connected to `h`, with the first state it was sent.
async fn connected_phone(
    h: &mut Harness,
) -> (
    mpsc::Receiver<Value>,
    watch::Receiver<Option<&'static str>>,
    Value,
) {
    enable_remote(h);
    let (mut output, closing) = connect_phone(h, 1, "phone").await;
    let state = synced_state(h, &mut output).expect("a snapshot");
    (output, closing, state)
}

/// The ids of the phone's session list, in order, for a phone that just connected.
async fn phone_catalog(h: &mut Harness) -> Vec<String> {
    let (_output, _closing, state) = connected_phone(h).await;
    state["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A catalog entry that was updated long before the turns of these tests.
fn idle_session(id: &str) -> crate::protocol::SessionInfo {
    let mut info = session_info(id);
    info["updated_at"] = json!("2025-06-01T00:00:00Z");
    serde_json::from_value(info).unwrap()
}

/// `s1` leads the phone's list and its picker row reads `now`.
async fn assert_first_in_the_phone_and_picker_lists(h: &mut Harness) {
    assert_eq!(phone_catalog(h).await[0], "s1");
    h.ctrl(KeyCode::Char('r'));
    let screen = h.screen(100, 24);
    assert!(
        screen
            .lines()
            .any(|row| row.contains("First") && row.contains("now")),
        "{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_background_turn_end_is_the_sessions_latest_activity_in_the_phone_and_picker_lists() {
    let mut h = running_first().await;
    h.app.sessions.push(idle_session("s3"));
    show(&mut h, "s2").await;
    let generation = h.app.catalog_generation;
    h.fake.release("turn.wait@s1");
    h.until(|app| app.live("s1").is_some_and(|s| s.unseen))
        .await;
    assert!(h.app.catalog_generation > generation);
    let stamp = |h: &Harness, id: &str| {
        let info = h.app.sessions.iter().find(|info| info.session_id == id);
        info.unwrap().updated_at.clone()
    };
    assert!(stamp(&h, "s1").as_str() > "2025-06-01T00:00:00Z");
    assert_eq!(stamp(&h, "s1"), first(&h).info.updated_at);
    assert_first_in_the_phone_and_picker_lists(&mut h).await;
}

#[tokio::test(start_paused = true)]
async fn a_foreground_turn_end_is_the_sessions_latest_activity_in_the_phone_and_picker_lists() {
    let mut h = running_first().await;
    h.app.sessions.push(idle_session("s3"));
    let before = h.app.sessions[0].updated_at.clone();
    h.fake.release("turn.wait@s1");
    h.until(|app| {
        !app.is_busy() && app.transcript_blocks().iter().any(|block| {
        matches!(block, Block::TurnEnd { loop_id, duration: Some(_), .. } if loop_id == "l1")
    })
    })
    .await;
    assert!(h.app.sessions[0].updated_at > before);
    assert_first_in_the_phone_and_picker_lists(&mut h).await;
}

#[tokio::test(start_paused = true)]
async fn the_phone_sees_live_sessions_and_switches_to_one_at_once() {
    let mut h = running_first().await;
    show(&mut h, "s2").await;
    let (mut output, _closing, state) = connected_phone(&mut h).await;
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

    ask_approval(&mut h, approval_for("s1", "i1")).await;
    h.settle().await;
    let state = synced_state(&mut h, &mut output).expect("a new state");
    assert_eq!(listed(&state, "s1")["status"], "approval");
    assert_eq!(state["background"], json!({"running": 0, "approval": 1}));

    let opens = h.calls("session.open").len();
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"})).await;
    while let Ok(message) = output.try_recv() {
        if message["t"] == "ack" {
            assert_eq!(message["ok"], true);
        }
    }
    assert_eq!(h.session().info.session_id, "s1");
    assert_eq!(h.calls("session.open").len(), opens);
}

#[tokio::test(start_paused = true)]
async fn only_the_foreground_is_laid_out_and_an_unchanged_phone_frame_sends_nothing() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    for n in 0..20 {
        let id = format!("b{n}");
        let info: crate::protocol::SessionInfo = serde_json::from_value(session_info(&id)).unwrap();
        let mut session = Session::new(info.clone(), 100 + n, crate::testing::LOAD.into());
        for i in 0..500 {
            session.transcript.push(Block::User {
                text: format!("message {i}"),
                kind: crate::protocol::read::UserKind::Prompt,
                at: None,
            });
        }
        h.app.resident.insert(session);
        h.app.sessions.push(info);
    }
    h.app.catalog_generation += 1;
    let (mut output, _closing, snapshot) = connected_phone(&mut h).await;
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
