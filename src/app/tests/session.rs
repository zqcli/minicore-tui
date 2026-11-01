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
            .script("session.create", opened_session(session_info("s1")));
        h.fake
            .script("input.push", starting_reply("l1", h.next_revision()));
        h.fake.script_held("turn.wait", waited("l1", completed()));
        h.app.launch = launch;
        h.boot().await;
        let workspace = h.app.workspace.clone();
        h.submit("hello");
        h.until(|app| app.resident.current().is_some()).await;
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
    assert_eq!(h.session().info.model, "deep");
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
            opened_session(session_info("s1")),
            opened_session(session_info("s2")),
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
        ]
        .into_iter()
        .map(|(page, delay)| (history(page), delay))
        .collect::<Vec<_>>(),
    );
    h.fake.script(
        "session.read",
        history(page(1, 0, vec![user(0, "l0", "first session")])),
    );
    h.boot().await;
    h.action(Action::OpenSession("s1".into()));
    h.until(|app| app.resident.current().is_some()).await;
    h.action(Action::OpenSession("s2".into()));
    h.until(|app| {
        app.resident
            .current()
            .is_some_and(|session| session.info.session_id == "s2")
    })
    .await;
    h.settle().await;
    assert_eq!(h.texts(), ["second session"]);
}

#[tokio::test(start_paused = true)]
async fn copy_never_takes_the_answer_of_the_previous_session() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held(
        "turn.wait",
        completed_reply(
            "l1",
            3,
            vec![user(0, "l1", "question"), assistant(1, "l1", "an answer")],
        ),
    );
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
        .script("session.create", opened_session(session_info("s2")));
    h.fake.script(
        "input.push@s2",
        after_snapshot(
            started_in(json!({"session_id":"s2","loop_id":"l2"}), vec![]),
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
    h.fake.script_held("turn.wait", waited("l2", completed()));
    h.action(Action::NewSession);
    h.submit("a new question");
    h.until(|app| {
        app.resident
            .current()
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
        history(page(
            1,
            0,
            vec![user(0, "l0", "fix the greeting\nand run the tests")],
        )),
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

#[tokio::test(start_paused = true)]
async fn an_event_from_a_previous_load_never_reaches_this_instance() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let installed = h.session().load.clone();
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("write a file");
    h.until(|app| app.is_busy()).await;
    let mut text = json!({"turn": turn("l1"), "request_index": 0,
                          "channel": "output", "delta": "stale",
                          "meta": {"session_id": "s1",
                                   "load": "load_00000000000000000000000000000000",
                                   "dropped_before": 0}});
    // A body for the instance that was closed must not reach the new one.
    let stale = text.clone();
    h.emit_no_meta("output_delta", stale).await;
    h.settle().await;
    assert!(
        !h.texts().iter().any(|text| text == "stale"),
        "a stale instance's delta was applied: {:?}",
        h.texts()
    );
    text["meta"]["load"] = json!(installed);
    h.emit_no_meta("output_delta", text).await;
    h.settle().await;
    assert!(
        h.texts().iter().any(|text| text == "stale"),
        "the installed instance's delta was dropped: {:?}",
        h.texts()
    );
}

#[tokio::test(start_paused = true)]
async fn installing_a_load_refreshes_control_after_the_initial_snapshot() {
    let mut h = Harness::new();
    h.fake
        .script("session.open", opened_session(session_info("s1")));
    h.open().await;
    assert!(
        !h.calls("session.state").is_empty(),
        "installing a load must ask for the current control snapshot"
    );
    assert_eq!(h.session().load, crate::testing::LOAD);
}

#[tokio::test(start_paused = true)]
async fn an_approval_published_only_by_a_snapshot_reaches_the_sheet() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("write a file");
    h.until(|app| app.is_busy()).await;
    // The chain's shape once W2 lands: no interaction event at all, only the
    // control snapshot naming the pending approval.
    h.snapshot_event(h.control(json!({
        "pending_interaction": {
            "loop_id": "l1", "request_index": 0,
            "interaction_id": "int_1", "tool_call_id": "cal_1", "tool_name": "bash",
            "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}
        }
    })))
    .await;
    h.until(|app| app.has_interaction()).await;
    assert!(
        h.screen(80, 20).contains("Allow bash?"),
        "the approval sheet must come from the snapshot alone"
    );
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_that_arrives_before_its_open_is_applied_by_that_open() {
    let mut h = Harness::new();
    h.fake
        .script_held("session.open", opened_session(session_info("s1")));
    h.boot().await;
    h.action(Action::OpenSession("s1".into()));
    h.fake.wait_for_calls("session.open", 1).await;
    h.emit(
        "session_snapshot",
        json!({
            "meta": {"session_id": "s1", "load": crate::testing::LOAD, "dropped_before": 0},
            "snapshot": {
                "session_id": "s1", "load": crate::testing::LOAD, "revision": 4,
                "execution": {"type": "none"},
                "pending_interaction": {
                    "loop_id": "l1", "request_index": 0,
                    "interaction_id": "int_early", "tool_call_id": "cal_1", "tool_name": "bash",
                    "kind": {"type": "approval", "data": {"prompt": "Allow early bash?"}}
                },
                "queue": {"items": [{"id": "inp_1", "mode": "follow_up", "text": "held"}],
                          "paused": false},
                "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                                "truncated": false}
            }
        }),
    )
    .await;
    h.settle().await;
    assert_eq!(
        h.calls("session.open").len(),
        1,
        "the open is still in flight"
    );
    assert!(!h.app.pre_install.is_empty(), "the snapshot is held");
    // The Agent's own answer to the refresh states the same live control, at a
    // higher revision, exactly as the real one would.
    h.fake.script(
        "session.state@s1",
        json!({"session_id": "s1", "status": "idle", "snapshot": {
            "session_id": "s1", "load": crate::testing::LOAD, "revision": 5,
            "execution": {"type": "none"},
            "pending_interaction": {
                "loop_id": "l1", "request_index": 0,
                "interaction_id": "int_early", "tool_call_id": "cal_1", "tool_name": "bash",
                "kind": {"type": "approval", "data": {"prompt": "Allow early bash?"}}
            },
            "queue": {"items": [{"id": "inp_1", "mode": "follow_up", "text": "held"}],
                      "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }}),
    );
    h.fake.release("session.open");
    h.until(|app| app.resident.current().is_some()).await;
    h.settle().await;
    assert_eq!(h.queue(), ["held"], "the queue the Agent reports is shown");
    assert!(
        h.app.has_interaction(),
        "the approval survives the open and its refresh"
    );
}

#[tokio::test(start_paused = true)]
async fn an_open_applies_the_snapshot_it_held_before_the_refresh_answers() {
    let mut h = Harness::new();
    h.fake
        .script_held("session.open", opened_session(session_info("s1")));
    h.boot().await;
    h.action(Action::OpenSession("s1".into()));
    h.fake.wait_for_calls("session.open", 1).await;
    h.emit(
        "session_snapshot",
        json!({
            "meta": {"session_id": "s1", "load": crate::testing::LOAD, "dropped_before": 0},
            "snapshot": {
                "session_id": "s1", "load": crate::testing::LOAD, "revision": 4,
                "execution": {"type": "none"},
                "queue": {"items": [{"id": "inp_1", "mode": "follow_up", "text": "held"}],
                          "paused": false},
                "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                                "truncated": false}
            }
        }),
    )
    .await;
    h.settle().await;
    assert!(!h.app.pre_install.is_empty());
    // The refresh is held, so what is on screen is what the open installed.
    h.fake.script_held(
        "session.state@s1",
        json!({"session_id": "s1", "status": "idle"}),
    );
    h.fake.release("session.open");
    h.until(|app| app.resident.current().is_some()).await;
    h.settle().await;
    assert_eq!(
        h.queue(),
        ["held"],
        "the held snapshot supplies control until a newer one arrives"
    );
    h.fake.release("session.state@s1");
}

#[tokio::test(start_paused = true)]
async fn a_superseded_same_session_open_cannot_replace_the_newer_load() {
    let mut h = Harness::new();
    h.fake.script_sequence_later("session.open",vec![
        (json!({"session":session_info("s1"),"load":"load_first","snapshot":snapshot("s1","load_first",1)}),Duration::from_secs(1)),
        (json!({"session":session_info("s1"),"load":"load_second","snapshot":snapshot("s1","load_second",1)}),Duration::ZERO),
    ]);
    h.boot().await;
    h.action(Action::OpenSession("s1".into()));
    h.fake.wait_for_calls("session.open", 1).await;
    h.action(Action::OpenSession("s1".into()));
    h.settle().await;
    assert_eq!(h.calls("session.open").len(), 2);
    assert_eq!(h.session().load, "load_second");
    assert!(!h.app.loading_session);
}

#[tokio::test(start_paused = true)]
async fn an_unrelated_snapshot_is_not_held_for_another_pending_open() {
    let mut h = Harness::new();
    h.fake
        .script_held("session.open", opened_session(session_info("s2")));
    h.boot().await;
    h.action(Action::OpenSession("s2".into()));
    h.fake.wait_for_calls("session.open", 1).await;
    h.snapshot_event(snapshot("s1", crate::testing::LOAD, 10))
        .await;
    h.settle().await;
    assert!(h.app.pre_install.is_empty());
    assert!(h.app.resident.current().is_none());
    h.fake.release("session.open");
    h.until(|app| {
        app.resident
            .current()
            .is_some_and(|session| session.info.session_id == "s2")
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn unknown_bodies_share_a_global_budget_without_creating_visible_turns() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    for i in 2..=9 {
        let session = crate::app::Session::new(
            serde_json::from_value(session_info(&format!("s{i}"))).unwrap(),
            i,
            crate::testing::LOAD.into(),
        );
        h.app.resident.insert(session);
    }
    for i in 1..=9 {
        h.emit_no_meta("output_delta",json!({"meta":{"session_id":format!("s{i}"),"load":crate::testing::LOAD,"dropped_before":0},"turn":{"session_id":format!("s{i}"),"loop_id":format!("unknown{i}")},"request_index":0,"channel":"text","delta":"x".repeat(1024*1024)})).await;
        h.settle().await;
    }
    let sessions: Vec<_> = h.app.resident.iter().collect();
    assert!(
        sessions
            .iter()
            .map(|session| session.early_bytes)
            .sum::<usize>()
            <= 8 * 1024 * 1024
    );
    assert!(sessions.iter().any(|session| session.early_gap));
    assert!(sessions.iter().all(|session| session.transcript.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn a_delayed_older_state_sample_cannot_regress_a_newer_snapshot_without_legacy_hints() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut older = crate::testing::snapshot_running(
        "s1",
        crate::testing::LOAD,
        100,
        "A",
        entry("iA", "steer", "old input"),
    );
    older["queue"] = queue(vec![entry("old-queued", "follow_up", "old queue")], false);
    h.fake.script_held(
        "session.state@s1",
        json!({"session_id":"s1","status":"running","snapshot":older}),
    );
    h.app.refresh_control();
    let calls = h.calls("session.state").len();
    h.fake.wait_for_calls("session.state", calls + 1).await;
    h.fake.script_held("turn.wait", waited("B", completed()));
    let mut newer = crate::testing::snapshot_running(
        "s1",
        crate::testing::LOAD,
        101,
        "B",
        entry("iB", "steer", "new input"),
    );
    newer["queue"] = queue(vec![entry("new-queued", "follow_up", "new queue")], true);
    newer["block_reason"] = json!("persistence");
    newer["pending_interaction"] = json!({"loop_id":"B","request_index":0,"interaction_id":"new-question","tool_call_id":"question-call","tool_name":"question","kind":{"type":"tool_input","data":{"prompt":"Which branch?","answer_kind":"text"}}});
    h.snapshot_event(newer).await;
    h.until(|app| app.resident.current().unwrap().recovery.revision == 101)
        .await;
    h.session_mut()
        .interaction_editor
        .set_text("keep this answer");
    h.fake.release("session.state@s1");
    h.until(|app| !app.resident.current().unwrap().control_in_flight)
        .await;
    let session = h.session();
    assert_eq!(session.recovery.revision, 101);
    assert_eq!(session.observed_loop(), Some("B"));
    assert_eq!(h.queue(), ["new queue"]);
    assert!(session.queue.paused);
    assert_eq!(
        session.block_reason,
        Some(crate::protocol::BlockReason::Persistence)
    );
    assert_eq!(
        session.interaction.as_ref().unwrap().interaction_id,
        "new-question"
    );
    assert_eq!(session.interaction_editor.text(), "keep this answer");
    assert_eq!(h.texts(), ["new input"]);
    assert_eq!(
        h.calls("session.state").len(),
        calls + 1,
        "no legacy event or refresh repairs this assertion"
    );
    assert!(
        h.notices().is_empty(),
        "the delayed older sample must decode and be rejected by revision, not by a protocol error: {:?}",
        h.notices()
    );
}

#[tokio::test(start_paused = true)]
async fn queued_old_load_results_cannot_mutate_a_reopened_instance_and_pending_work_is_cancelled() {
    use crate::app::{SessionOp, read};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "session.read",
        vec![page(
            2,
            0,
            vec![
                user(0, "old-loop", "old prompt"),
                assistant(1, "old-loop", "old history answer"),
            ],
        )]
        .into_iter()
        .map(history)
        .collect::<Vec<_>>(),
    );
    h.fake.script_held(
        "session.read",
        history(page(
            2,
            0,
            vec![
                user(0, "old-loop", "old prompt"),
                assistant(1, "old-loop", "old history answer"),
            ],
        )),
    );
    let reads = h.calls("session.read").len();
    let mut result = turn_result(
        "old-loop",
        vec![
            user(0, "old-loop", "old prompt"),
            assistant(1, "old-loop", "old result answer"),
        ],
    );
    result["outcome"] = completed();
    h.fake.script_held("turn.result", result);
    let old = h.session();
    let epoch = old.epoch;

    let old_load = old.load.clone();
    let client = h.fake.client.clone();
    h.app.spawn(async move {
        let read = read::read_history(client, "s1".into(), read::Window::Tail).await;
        assert!(matches!(&read,Ok(read::HistoryRead::Complete(page)) if page.items.len()==2));
        SessionOp::Tail(read)
    });
    let client = h.fake.client.clone();
    h.app.spawn(async move {
        let turn = crate::protocol::TurnRef::new("s1", "old-loop");
        let result = read::read_turn(client, turn.clone(), old_load).await;
        assert!(matches!(&result,Ok(read::TurnRead::Complete(body)) if body.items.len()==2));
        SessionOp::Recovered(crate::app::recovery::RecoveryOutcome {
            seq: 1,
            turn,

            result,
        })
    });
    h.fake.wait_for_calls("session.read", reads + 2).await;
    h.fake.wait_for_calls("turn.result", 1).await;
    h.fake.release("session.read");
    h.fake.release("turn.result");
    let stale = [h.next_message().await, h.next_message().await];
    for message in &stale {
        assert!(
            matches!(message,crate::app::Msg::Session(owner,op) if *owner==epoch && matches!(&**op,SessionOp::Tail(_) | SessionOp::Recovered(_)))
        );
    }
    let (pending, cancelled) = tokio::sync::oneshot::channel::<()>();
    h.app.spawn(async move {
        let op = std::future::pending::<SessionOp>().await;
        drop(pending);
        op
    });
    h.fake.script("session.close", json!({"closed":true}));
    let _: serde_json::Value = h
        .fake
        .client
        .call(
            "session.close",
            &json!({"session_id":"s1","load":crate::testing::LOAD}),
        )
        .await
        .unwrap();
    h.app.resident.remove("s1");
    assert!(
        crate::testing::real_bounded(cancelled).await.is_err(),
        "resident removal cancels its unfinished work"
    );
    let mut reopened = crate::testing::snapshot_running(
        "s1",
        "load_reopened",
        1,
        "new-loop",
        entry("new-input", "steer", "new prompt"),
    );
    reopened["completions"] = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"new-completion","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]});
    let mut new_result = turn_result(
        "new-completion",
        vec![
            user(0, "new-completion", "new completed prompt"),
            assistant(1, "new-completion", "new completed answer"),
        ],
    );
    new_result["load"] = json!("load_reopened");
    h.fake
        .script_held("turn.result@s1/new-completion", new_result);
    h.fake.script(
        "session.open",
        json!({"session":session_info("s1"),"load":"load_reopened","snapshot":reopened}),
    );
    let mut refreshed = reopened.clone();
    refreshed["revision"] = json!(2);
    h.fake.script(
        "session.state@s1",
        json!({"session_id":"s1","status":"running","snapshot":refreshed}),
    );
    h.fake.script_held(
        "turn.wait",
        json!({"turn":turn("new-loop"),"outcome":completed()}),
    );
    let old_history_calls = h.calls("session.read").len();
    h.fake.script_sequence(
        "session.read",
        vec![display_read(0, 0)]
            .into_iter()
            .map(history)
            .collect::<Vec<_>>(),
    );
    h.action(Action::OpenSession("s1".into()));
    h.until(|app| {
        app.resident.current().is_some_and(|session| {
            session.load == "load_reopened" && session.recovery.revision == 2
        })
    })
    .await;
    h.emit_no_meta("output_delta",json!({"meta":{"session_id":"s1","load":"load_reopened","dropped_before":0},"turn":turn("new-loop"),"request_index":0,"channel":"text","delta":"new answer"})).await;
    h.until(|app| app.resident.current().unwrap().transcript.last_answer() == Some("new answer"))
        .await;
    assert!(h.session().epoch > epoch);
    h.app.composer.set_text("new draft");
    let before = format!("{:?}", h.blocks());
    for message in stale {
        h.app.on_msg(message);
        assert_eq!(
            format!("{:?}", h.blocks()),
            before,
            "an already queued old result must not mutate the new view even transiently"
        );
        assert_eq!(h.session().recovery.settled_through, 0);
    }
    h.settle().await;
    assert!(
        h.calls("session.read").len() > old_history_calls,
        "the reopened view issues a separate history read"
    );
    assert_eq!(format!("{:?}", h.blocks()), before);
    assert_eq!(h.app.composer.text(), "new draft");
    let session = h.session();
    assert_eq!(session.load, "load_reopened");
    assert_eq!(session.recovery.revision, 2);
    assert_eq!(
        (
            session.recovery.observed_through,
            session.recovery.settled_through
        ),
        (1, 0)
    );
    assert_eq!(session.recovery.entries[&1].turn.loop_id, "new-completion");
    assert_eq!(
        session.recovery.entries[&1].status,
        crate::app::recovery::RecoveryStatus::Fetching
    );
    assert_eq!(
        h.calls("turn.result")
            .iter()
            .map(|call| call["load"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        [crate::testing::LOAD, "load_reopened"],
        "both loads issue distinct real result reads; the new one remains explicitly held"
    );
    assert!(session.body_recovery.is_empty());
    assert!(!session.is_closed("old-loop"));
    assert!(h.notices().is_empty());
}

mod snapshot_control {
    use crate::app::{LiveStatus, Phase, Session};
    use crate::protocol::SessionSnapshot;
    use crate::protocol::read::UserKind;
    use crate::transcript::Block;
    use serde_json::json;

    fn session() -> Session {
        Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
            crate::testing::LOAD.into(),
        )
    }

    fn snapshot(execution: serde_json::Value, queue: serde_json::Value) -> SessionSnapshot {
        serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 1,
            "execution": execution, "queue": queue,
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap()
    }

    fn item(id: &str, text: &str) -> serde_json::Value {
        json!({"id": id, "mode": "steer", "text": text})
    }

    #[test]
    fn execution_retains_its_region_through_finishing_until_a_successor_arrives() {
        let mut session = session();
        let observed = snapshot(
            json!({"type": "running", "loop_id": "l1", "input": item("i1", "first"),
                   "request_index": 0, "phase": "running_model"}),
            json!({"items": [], "paused": false}),
        );
        session.apply_control(&observed);
        let live = session.live.as_ref().unwrap();
        assert_eq!(live.turn.loop_id, "l1");
        assert_eq!(live.phase, Phase::Waiting);
        assert!(matches!(
            session.transcript.blocks().first(),
            Some(Block::User { text, kind: UserKind::Prompt, .. }) if text == "first"
        ));
        assert_eq!(session.control_status(), LiveStatus::Running);
        session.apply_control(&snapshot(
            json!({"type":"finishing","loop_id":"l1","input":item("i1","first"),"request_index":1}),
            json!({"items":[],"paused":false}),
        ));
        assert_eq!(session.live.as_ref().unwrap().turn.loop_id, "l1");
        assert_eq!(session.transcript.len(), 1);
        session.apply_control(&snapshot(
            json!({"type":"running","loop_id":"l2","input":item("i2","second"),"request_index":0,"phase":"running_tools"}),
            json!({"items":[],"paused":false}),
        ));
        assert_eq!(session.observed_loop(), Some("l2"));
        assert_eq!(session.live.as_ref().unwrap().start, 1);
        assert!(
            matches!(&session.transcript.blocks()[1], Block::User {text,..} if text == "second")
        );
    }

    #[test]
    fn a_claimed_input_is_busy_without_creating_a_speculative_loop_or_queue_entry() {
        let mut session = session();
        session.apply_control(&snapshot(
            json!({"type":"starting","input":item("claimed","older input")}),
            json!({"items":[],"paused":false}),
        ));
        assert!(session.is_busy());
        assert!(session.live.is_none());
        assert_eq!(session.status_label(), Some("Thinking"));
        assert!(session.queue.items.is_empty());
        assert!(session.transcript.is_empty());
    }

    #[test]
    fn a_snapshot_interaction_is_operable_without_a_tool_card() {
        let mut session = session();
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 1,
            "execution": {"type": "running", "loop_id": "l1",
                          "input": item("i1", "go"), "request_index": 2,
                          "phase": "waiting_for_input"},
            "pending_interaction": {
                "loop_id": "l1", "request_index": 2, "interaction_id": "int_1",
                "tool_call_id": "call_1", "tool_name": "shell",
                "kind": {"type": "tool_input", "data": {
                    "prompt": "Which branch?", "answer_kind": "single_choice",
                    "choices": [{"index": 0, "text": "main"}, {"index": 1, "text": "next"}]
                }}
            },
            "queue": {"items": [], "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap();
        session.apply_control(&snapshot);
        let interaction = session.interaction.as_ref().unwrap();
        assert_eq!(interaction.turn.loop_id, "l1");
        assert_eq!(interaction.prompt, "Which branch?");
        assert_eq!(interaction.options(), ["main", "next"]);
        assert_eq!(session.control_status(), LiveStatus::Approval);
        // A later snapshot without the interaction resolves it.
        session.apply_control(&snapshot_json_without_interaction());
        assert!(session.interaction.is_none());
    }

    fn snapshot_json_without_interaction() -> SessionSnapshot {
        serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 2,
            "execution": {"type": "running", "loop_id": "l1",
                          "input": item("i1", "go"), "request_index": 2,
                          "phase": "running_tools"},
            "queue": {"items": [], "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap()
    }
}
