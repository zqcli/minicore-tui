use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::{Value, json};

use super::harness::*;
use crate::app::{Action, NoticeLevel, Scroll, SessionOp};
use crate::protocol::read::UserKind;
use crate::protocol::stamp::{now_millis, unix_millis};
use crate::transcript::{Block, ChangeSummary, ToolState};

#[tokio::test(start_paused = true)]
async fn queued_follow_ups_run_one_per_turn_as_the_agent_starts_them() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let first = entry("i2", "follow_up", "first");
    let second = entry("i3", "follow_up", "second");
    h.fake.script_sequence(
        "input.push",
        vec![
            started("l1"),
            pushed(vec![first.clone()]),
            pushed(vec![first, second.clone()]),
        ],
    );
    h.fake.script_sequence_later(
        "turn.wait",
        vec![
            (
                waited_then(
                    "l1",
                    completed(),
                    Some(next_turn("l2", "first")),
                    queue(vec![second], false),
                ),
                Duration::from_secs(1),
            ),
            (
                waited_then(
                    "l2",
                    completed(),
                    Some(next_turn("l3", "second")),
                    queue(vec![], false),
                ),
                Duration::ZERO,
            ),
            (
                waited_then("l3", completed(), None, queue(vec![], false)),
                Duration::ZERO,
            ),
        ],
    );
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    for text in ["first", "second"] {
        h.app.composer.set_text(text);
        h.key(KeyCode::Enter, KeyModifiers::ALT);
    }
    h.until(|app| app.session.as_ref().unwrap().queue.items.len() == 2)
        .await;
    assert!(h.screen(60, 12).contains("↳ follow-up 1  first"));

    h.settle().await;
    assert_eq!(
        h.sends(),
        ["start", "first", "second"],
        "the TUI pushes nothing more"
    );
    assert!(h.queue().is_empty());
    assert_eq!(turn_ends(&h), ["l1", "l2", "l3"]);
    let waits: Vec<Value> = h
        .calls("turn.wait")
        .iter()
        .map(|call| call["loop_id"].clone())
        .collect();
    assert_eq!(waits, ["l1", "l2", "l3"], "a wait for every successor");
    let shape: Vec<&str> = h
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User { .. } => "user",
            Block::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["user", "end", "user", "end", "user", "end"]);
    assert_eq!(h.texts(), ["start", "first", "second"]);
}

#[tokio::test(start_paused = true)]
async fn steers_a_running_turn_took_become_steering_blocks_in_the_agents_order() {
    let id = |text: &str| if text == "first" { "i2" } else { "i3" };
    for batches in [
        vec![vec!["first", "second"]],
        vec![vec!["first"], vec!["second"]],
    ] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        let first = entry("i2", "steer", "first");
        let second = entry("i3", "steer", "second");
        h.fake.script_sequence(
            "input.push",
            vec![
                started("l1"),
                pushed(vec![first.clone()]),
                pushed(vec![first, second]),
            ],
        );
        h.fake.script_held("turn.wait", waited("l1", completed()));
        h.submit("start");
        h.until(|app| app.is_busy()).await;
        h.emit(
            "output_delta",
            json!({"turn": turn("l1"), "request_index": 0, "channel": "text", "delta": "working"}),
        )
        .await;
        h.submit("first");
        h.submit("second");
        h.settle().await;
        assert_eq!(h.queue(), ["first", "second"]);
        // Another turn's inputs are not this turn's.
        h.emit(
            "input_delivered",
            json!({"turn": turn("old"), "items": [{"id": "x", "text": "stray", "request_index":1}]}),
        )
        .await;
        for batch in batches {
            let items: Vec<Value> = batch
                .iter()
                .map(|text| json!({"id": id(text), "text": text, "request_index":1}))
                .collect();
            h.emit(
                "input_delivered",
                json!({"turn": turn("l1"), "items": items}),
            )
            .await;
            h.settle().await;
        }
        assert_eq!(h.texts(), ["start", "working", "first", "second"]);
        assert!(
            h.queue().is_empty(),
            "delivered entries leave the queue by id"
        );
        let kinds: Vec<UserKind> = h
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::User { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [UserKind::Prompt, UserKind::Steering, UserKind::Steering]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn inputs_taken_by_a_successor_before_its_wait_starts_still_become_steering_blocks() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let next = entry("i2", "steer", "next up");
    h.fake
        .script_sequence("input.push", vec![started("A"), pushed(vec![next])]);
    h.fake.script_held(
        "turn.wait",
        waited_then(
            "A",
            completed(),
            Some(next_turn("B", "next up")),
            queue(vec![], false),
        ),
    );
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    h.submit("next up");
    h.settle().await;
    // B's first request takes a second steer before A's wait has answered.
    h.emit(
        "input_delivered",
        json!({"turn": turn("B"), "items": [{"id": "i3", "text": "right behind", "request_index":0}]}),
    )
    .await;
    h.settle().await;
    assert_eq!(h.texts(), ["start"]);
    h.fake.release("turn.wait");
    h.fake.script_held("turn.wait", waited("B", completed()));
    h.settle().await;
    assert_eq!(h.texts(), ["start", "next up", "right behind"]);
}

#[tokio::test(start_paused = true)]
async fn a_push_into_a_paused_queue_shows_the_older_entry_the_agent_started_with() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.mirror(vec![entry("i1", "follow_up", "older")], true);
    h.fake.script(
        "input.push",
        json!({"id": "i2", "started": next_turn("l2", "older"),
               "queue": queue(vec![entry("i2", "follow_up", "newer")], false)}),
    );
    h.fake.script_held("turn.wait", waited("l2", completed()));
    h.app.composer.set_text("newer");
    h.action(Action::QueueFollowUp);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.texts(), ["older"], "queue order wins over arrival");
    assert_eq!(h.queue(), ["newer"]);
    assert!(!h.app.session.as_ref().unwrap().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn a_wait_answer_without_a_queue_asks_for_the_control_once_and_nothing_else() {
    let mut h = Harness::new();
    // The state names the loop that just ended, as a stale Agent could.
    h.fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "running",
               "active_loop": {"loop_id": "l9"},
               "snapshot": crate::testing::snapshot_running(
                   "s1", crate::testing::LOAD, 1, "l9", entry("i1", "steer", "running"))}),
    );
    h.fake.script_held(
        "turn.wait",
        json!({"turn": turn("l9"), "outcome": completed()}),
    );
    h.open().await;
    assert!(h.is_busy());
    let states = h.calls("session.state").len();
    h.fake.release("turn.wait");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(h.calls("turn.wait").len(), 1, "no second wait for the loop");
    assert_eq!(
        h.calls("session.state").len(),
        states + 1,
        "a wait without a queue asks for one control snapshot"
    );
    assert!(
        h.calls("input.list").is_empty(),
        "input.list is not control"
    );
}

#[tokio::test(start_paused = true)]
async fn an_input_queue_event_replaces_the_mirrored_queue_and_its_paused_flag() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let items = vec![
        entry("i1", "steer", "now"),
        entry("i2", "follow_up", "later"),
    ];
    h.control_event(json!({"queue": queue(items, true)})).await;
    h.settle().await;
    let session = h.app.session.as_ref().unwrap();
    assert!(session.queue.paused);
    assert_eq!(h.queue(), ["now", "later"]);
    h.control_event(json!({"queue": queue(vec![], false)}))
        .await;
    h.settle().await;
    assert!(h.queue().is_empty());
    assert!(!h.app.session.as_ref().unwrap().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn opening_a_session_takes_the_queued_entries_the_agent_reports() {
    let mut h = Harness::new();
    let mut state = crate::testing::snapshot("s1", crate::testing::LOAD, 1);
    state["queue"] = queue(vec![entry("i1", "follow_up", "waiting")], true);
    h.fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "idle", "snapshot": state}),
    );
    h.open().await;
    assert_eq!(h.queue(), ["waiting"]);
    assert!(h.app.session.as_ref().unwrap().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn a_paused_queue_shows_how_to_go_on_and_enter_without_a_draft_resumes_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_sequence(
        "turn.wait",
        vec![waited_then(
            "l1",
            cancelled(),
            None,
            queue(vec![entry("i2", "follow_up", "after the stop")], true),
        )],
    );
    h.fake.script(
        "turn.result",
        turn_result("l1", vec![user(0, "l1", "start")]),
    );
    h.submit("start");
    h.settle().await;
    assert!(!h.is_busy());
    assert!(h.app.session.as_ref().unwrap().queue.paused);
    let screen = h.screen(80, 16);
    assert!(screen.contains("↳ follow-up 1  after the stop"), "{screen}");
    assert!(
        screen.contains("paused · enter resumes · alt+↑ edit"),
        "{screen}"
    );
    h.type_text("x");
    assert!(
        h.screen(80, 16)
            .contains("paused · enter sends · alt+↑ edit"),
        "a draft sends instead"
    );
    h.app.composer.clear();

    h.fake.script(
        "input.resume",
        json!({"started": next_turn("l2", "after the stop"), "queue": queue(vec![], false)}),
    );
    h.fake.script_held("turn.wait", waited("l2", completed()));
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;
    assert_eq!(
        h.calls("input.resume"),
        [json!({"session_id": "s1", "load": crate::testing::LOAD})]
    );
    assert_eq!(h.sends(), ["start"], "resuming pushes no text");
    assert_eq!(h.texts(), ["start", "after the stop"]);
    assert!(h.queue().is_empty());
    assert!(!h.app.session.as_ref().unwrap().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn enter_on_an_empty_draft_asks_the_agent_for_nothing_unless_the_queue_is_paused() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.mirror(vec![entry("i1", "follow_up", "waiting")], false);
    h.press(KeyCode::Enter);
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.settle().await;
    assert!(h.calls("input.resume").is_empty());
    assert!(h.calls("input.push").is_empty());
}

#[tokio::test(start_paused = true)]
async fn alt_up_takes_every_queued_message_back_in_the_agents_order_into_an_empty_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    // The push's own receipt states the queue after it, so the entries the
    // Agent took meanwhile arrive once the receipt has been applied.
    h.settle().await;
    h.mirror(
        vec![
            entry("i2", "steer", "waiting steer"),
            entry("i3", "follow_up", "follow1"),
            entry("i4", "follow_up", "follow2"),
        ],
        false,
    );
    h.fake.script(
        "input.recall",
        json!({"texts": ["waiting steer", "follow1", "follow2"], "queue": queue(vec![], false)}),
    );
    h.app.composer.set_text("draft");
    h.key(KeyCode::Up, KeyModifiers::ALT);
    h.settle().await;
    assert_eq!(h.queue().len(), 3);
    assert_eq!(
        h.app.composer.text(),
        "draft",
        "a draft is never overwritten"
    );
    assert_eq!(
        h.notices(),
        ["Clear the draft before recalling a queued message"]
    );
    assert!(h.calls("input.recall").is_empty());

    h.app.composer.clear();
    h.key(KeyCode::Up, KeyModifiers::ALT);
    h.settle().await;
    assert_eq!(h.app.composer.text(), "waiting steer\n\nfollow1\n\nfollow2");
    assert!(h.queue().is_empty());
    assert_eq!(
        h.calls("input.recall"),
        [json!({"session_id": "s1", "load": crate::testing::LOAD})]
    );

    h.app.composer.clear();
    h.key(KeyCode::Up, KeyModifiers::ALT);
    h.settle().await;
    assert_eq!(
        h.calls("input.recall").len(),
        1,
        "an empty queue asks nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn a_push_the_agent_refuses_returns_its_text_and_leaves_the_queue_as_it_was() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let waiting = entry("i2", "follow_up", "waiting");
    h.fake.script_sequence(
        "input.push",
        vec![
            started("l1"),
            pushed(vec![waiting]),
            refusal(-32030, "input_queue_full"),
        ],
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    h.app.composer.set_text("waiting");
    h.action(Action::QueueFollowUp);
    h.settle().await;
    h.submit("one too many");
    h.settle().await;
    assert_eq!(h.app.composer.text(), "one too many");
    assert_eq!(h.notices(), ["input_queue_full"]);
    assert_eq!(h.queue(), ["waiting"]);
}

fn turn_ends(h: &Harness) -> Vec<String> {
    h.blocks()
        .iter()
        .filter_map(|block| match block {
            Block::TurnEnd { loop_id, .. } => Some(loop_id.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_turn_streams_tool_facts_and_ends_on_its_wait() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": completed(),
               "usage": {"input_tokens": 100, "output_tokens": 20},
               "requests": 2, "tool_rounds": 1}),
    );
    h.submit("fix the greeting");
    h.until(|app| app.is_busy()).await;

    h.emit("turn_started", json!({"turn": turn("l1")})).await;
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 0, "channel": "text", "delta": "Fixed."}),
    )
    .await;
    let call = json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1"});
    h.emit(
        "tool_started",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1", "tool_name": "bash"}),
    )
    .await;
    let mut presentation = call.clone();
    presentation["tool_name"] = json!("bash");
    presentation["display"] = json!({"detail": "$ cargo test", "truncated": false});
    h.emit("tool_presentation", presentation).await;
    h.emit(
        "tool_execution",
        json!({"turn": turn("l1"), "data": {
            "tool_ref": {"session_id": "s1", "loop_id": "l1", "request_index": 0,
                         "tool_call_id": "c1"},
            "name": "bash", "state": "succeeded", "result_truncated": false,
            "command": {"status": "exited", "exit_code": 1, "termination_confirmed": true}}}),
    )
    .await;
    // A second terminal fact for the same call must not count it twice.
    h.emit(
        "tool_finished",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1",
               "result": {"outcome": "success", "content_bytes": 0}}),
    )
    .await;
    h.settle().await;
    let screen = h.screen(80, 20);
    assert!(screen.contains("1 tool"), "{screen}");
    assert!(!screen.contains("2 tools"), "{screen}");
    assert!(screen.contains("▎ $ cargo test"), "{screen}");
    assert!(screen.contains("exit 1"), "{screen}");
    assert!(screen.contains("esc to interrupt"), "{screen}");

    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;
    h.settle().await;

    assert_eq!(h.texts(), ["fix the greeting", "Fixed."]);
    let card = h
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::Tool(call) => Some((call.state, call.exit_code)),
            _ => None,
        })
        .unwrap();
    assert_eq!(card, (ToolState::Succeeded, Some(1)));
    let end = h.blocks().iter().find_map(|block| match block {
        Block::TurnEnd {
            outcome,
            requests,
            tools,
            ..
        } => Some((outcome.is_completed(), *requests, *tools)),
        _ => None,
    });
    assert_eq!(end, Some((true, 2, 1)));
    assert_eq!(h.app.session.as_ref().unwrap().usage.output, 20);
    let waits = h.calls("turn.wait");
    assert_eq!(waits.len(), 1, "turn_started registers no second wait");
    assert_eq!(waits[0]["loop_id"], "l1");
}

#[tokio::test(start_paused = true)]
async fn a_provided_input_event_finishes_its_tool_without_ending_the_running_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.running_state("A", entry("input-A", "steer", "question"));
    h.emit("tool_started",json!({"turn":turn("A"),"request_index":0,"tool_call_id":"question-call","tool_name":"question"})).await;
    h.until(|app| {
        app.transcript_blocks().iter().any(
            |block| matches!(block,Block::Tool(call) if call.key.tool_call_id == "question-call"),
        )
    })
    .await;
    h.emit("tool_finished",json!({"turn":turn("A"),"request_index":0,"tool_call_id":"question-call","result":{"outcome":"input_provided","content_bytes":3}})).await;
    h.settle().await;
    let call = h
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::Tool(call) => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.state, ToolState::Succeeded);
    assert!(call.state.is_terminal());
    assert_eq!(h.app.session.as_ref().unwrap().observed_loop(), Some("A"));
    assert!(
        h.is_busy(),
        "a supplied tool input does not complete its model turn"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_turn_shows_the_providers_status_the_hint_and_its_message() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held(
        "turn.wait",
        waited(
            "l1",
            json!({"type": "failed", "kind": "model", "model_error": {
                "kind": "rate_limited", "delivery": "not_started", "retry_after_millis": 12000,
                "provider": {"status": 429, "code": "rate_limit_exceeded", "type": "requests",
                             "message": "Rate limit reached for gpt-6 in organization org-1"}}}),
        ),
    );
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;
    h.settle().await;
    let screen = h.screen(100, 20);
    for expected in [
        "failed · rate limited · 429 rate_limit_exceeded/requests",
        "then send \"continue\". Retry after 12 s.",
        "│ Rate limit reached for gpt-6 in organization org-1",
    ] {
        assert!(screen.contains(expected), "{expected}\n{screen}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_tick_keeps_tool_cards_cached_until_a_tool_event_changes_them() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    // The Agent previews a call before it starts, which makes a pending card.
    h.emit(
        "tool_presentation",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c2",
               "tool_name": "bash", "display": {"detail": "$ sleep 9", "truncated": false}}),
    )
    .await;
    for id in ["c1", "c2"] {
        h.emit(
            "tool_started",
            json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": id,
                   "tool_name": "bash"}),
        )
        .await;
    }
    h.emit(
        "tool_finished",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1",
               "result": {"outcome": "success", "content_bytes": 0}}),
    )
    .await;
    h.settle().await;
    let revisions = |h: &Harness| {
        h.app
            .session
            .as_ref()
            .unwrap()
            .transcript
            .revisions()
            .to_vec()
    };
    let before = revisions(&h);
    h.app.tick();
    let after = revisions(&h);
    let changed: Vec<usize> = (0..before.len())
        .filter(|&at| before[at] != after[at])
        .collect();
    let running: Vec<usize> = h
        .blocks()
        .iter()
        .enumerate()
        .filter(|(_, block)| matches!(block, Block::Tool(call) if call.is_running()))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(running.len(), 1, "c1 finished, c2 runs");
    assert!(changed.is_empty(), "no tool animations: {changed:?}");

    h.emit(
        "tool_finished",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c2",
               "result": {"outcome": "success", "content_bytes": 0}}),
    )
    .await;
    h.settle().await;
    let after_event = revisions(&h);
    let changed: Vec<usize> = (0..after.len())
        .filter(|&at| after[at] != after_event[at])
        .collect();
    assert_eq!(
        changed, running,
        "real tool updates still invalidate the card"
    );
}

#[tokio::test(start_paused = true)]
async fn a_wait_answer_for_another_loop_leaves_the_turn_running() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script("turn.wait", waited("l0", completed()));
    h.submit("go");
    h.settle().await;
    assert!(h.is_busy());
    assert!(turn_ends(&h).is_empty());
}

#[tokio::test(start_paused = true)]
async fn dropped_events_make_the_durable_items_replace_only_that_turn() {
    let mut h = Harness::opened(page(
        2,
        0,
        vec![
            user(0, "l0", "earlier question"),
            assistant(1, "l0", "earlier answer"),
        ],
    ))
    .await;
    h.fake
        .script("input.push", started_at("l1", "2026-10-20T21:38:00Z"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.fake.script(
        "turn.result",
        turn_result(
            "l1",
            vec![
                user(2, "l1", "the prompt"),
                assistant(3, "l1", "the durable answer"),
            ],
        ),
    );
    h.submit("the prompt");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 0, "channel": "text", "delta": "half an ans",
               "meta": {"session_id": "s1", "dropped_before": 2}}),
    )
    .await;
    h.settle().await;
    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;
    h.settle().await;

    assert_eq!(
        h.texts(),
        [
            "earlier question",
            "earlier answer",
            "the prompt",
            "the durable answer"
        ]
    );
    assert_eq!(turn_ends(&h), ["l1"]);
    assert_eq!(h.sent_at("the prompt"), unix_millis("2026-10-20T21:38:00Z"));
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn a_prompt_shows_the_time_the_agent_accepted_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", started_at("l1", "2026-10-20T21:38:00Z"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("with a time");
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    assert_eq!(
        h.sent_at("with a time"),
        unix_millis("2026-10-20T21:38:00Z")
    );
    assert!(h.screen(60, 12).contains("21:38 · 10/20/2026"));
}

#[tokio::test(start_paused = true)]
async fn a_prompt_without_an_acceptance_time_takes_the_clock_when_its_turn_starts() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    let before = now_millis();
    h.submit("no ack time");
    h.until(|app| app.is_busy()).await;
    let at = h.sent_at("no ack time").expect("stamped when it starts");
    assert!((before..=now_millis()).contains(&at));
}

#[tokio::test(start_paused = true)]
async fn a_pushed_steer_waits_in_the_dock_until_the_turn_takes_it_and_shows_its_accepted_time() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "input.push",
        vec![
            started("l1"),
            pushed(vec![entry("i2", "steer", "also the tests")]),
        ],
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    h.submit("also the tests");
    h.settle().await;
    assert_eq!(h.queue(), ["also the tests"]);
    assert!(h.screen(60, 12).contains("↗ steer  also the tests"));

    h.emit(
        "input_delivered",
        json!({"turn": turn("l1"), "items": [
            {"id": "i2", "text": "also the tests", "accepted_at": "2026-10-20T21:39:30Z", "request_index":1}]}),
    )
    .await;
    h.settle().await;
    assert!(h.queue().is_empty());
    assert_eq!(
        h.sent_at("also the tests"),
        unix_millis("2026-10-20T21:39:30Z")
    );
    let screen = h.screen(60, 14);
    assert!(
        screen.contains("21:39 · 10/20/2026 · ↗ steering"),
        "{screen}"
    );
    assert!(!screen.contains("↗ steer  also"), "{screen}");
}

/// A long history and a turn whose `bash` card is still running, drawn once.
async fn running_card() -> Harness {
    let history = (0..15)
        .flat_map(|n| {
            [
                user(2 * n, "l0", &format!("question {n}")),
                assistant(2 * n + 1, "l0", &format!("answer {n}")),
            ]
        })
        .collect();
    let mut h = Harness::opened(page(30, 0, history)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "tool_started",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1", "tool_name": "bash"}),
    )
    .await;
    h.emit(
        "tool_presentation",
        json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1",
               "tool_name": "bash", "display": {"detail": "$ sleep 40", "truncated": false}}),
    )
    .await;
    h.settle().await;
    h
}

#[tokio::test(start_paused = true)]
async fn rows_added_to_the_dock_never_cover_the_running_card() {
    for pinned in [false, true] {
        for shown in [
            "↗ steer  more",
            "↳ follow-up 1  later",
            "boom",
            "Allow bash?",
        ] {
            let mut h = running_card().await;
            assert!(h.screen(80, 20).contains("▎ $ sleep 40"));
            if pinned {
                let layout = &h.app.layout;
                let (block, offset) = layout.anchor(layout.viewport.top);
                h.app.scroll = Scroll::Anchor { block, offset };
            }
            match shown {
                "↗ steer  more" => {
                    h.fake
                        .script("input.push", pushed(vec![entry("i2", "steer", "more")]));
                    h.submit("more");
                    h.settle().await;
                }
                "↳ follow-up 1  later" => {
                    h.fake.script(
                        "input.push",
                        pushed(vec![entry("i2", "follow_up", "later")]),
                    );
                    h.app.composer.set_text("later");
                    h.key(KeyCode::Enter, KeyModifiers::ALT);
                    h.settle().await;
                }
                "boom" => h.app.notice(NoticeLevel::Error, "boom"),
                _ => {
                    h.emit(
                        "interaction_requested",
                        json!({"turn": turn("l1"), "interaction": {
                            "interaction_id": "i1", "tool_call_id": "c1", "tool_name": "bash",
                            "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}}}),
                    )
                    .await;
                    h.until(|app| app.has_interaction()).await;
                }
            }
            let screen = h.screen(80, 20);
            assert!(screen.contains(shown), "{shown}:\n{screen}");
            assert!(
                screen.contains("▎ $ sleep 40"),
                "pinned {pinned}, {shown}:\n{screen}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_send_returns_its_text_without_losing_a_newer_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_error("input.push", -32603, "provider unavailable", "internal");
    h.app.composer.insert("ship it");
    h.app.composer.type_char('!');
    h.action(Action::Submit);
    h.settle().await;
    assert_eq!(h.app.composer.text(), "ship it!");
    assert!(h.texts().is_empty());
    assert_eq!(h.notices(), ["provider unavailable"]);
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.text(), "ship it", "undo reaches the draft");
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.text(), "");
    h.ctrl(KeyCode::Char('y'));
    assert_eq!(h.app.composer.text(), "ship it");

    h.submit("refused prompt");
    h.app.composer.set_text("second thoughts");
    h.settle().await;
    assert_eq!(h.app.composer.text(), "second thoughts");
    assert_eq!(h.app.local_inputs[0].entry.text(), "refused prompt");
    assert_eq!(
        h.app.local_inputs[0].status,
        crate::app::unconfirmed::Status::Rejected
    );
}

#[tokio::test(start_paused = true)]
async fn a_running_loop_is_adopted_from_state_and_ends_through_its_wait() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.wait", waited("l9", completed()));
    h.running_state("l9", entry("i1", "steer", "running"));
    h.settle().await;
    assert_eq!(h.calls("turn.wait")[0]["loop_id"], "l9");
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l9"]);
}

#[tokio::test(start_paused = true)]
async fn opening_a_session_mid_turn_adopts_the_running_loop() {
    let mut h = Harness::new();
    h.fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "running",
               "active_loop": {"loop_id": "l9"},
               "snapshot": crate::testing::snapshot_running(
                   "s1", crate::testing::LOAD, 2, "l9", entry("i1", "steer", "running"))}),
    );
    h.fake.script_held("turn.wait", waited("l9", completed()));
    h.open().await;
    assert!(h.is_busy());
    assert_eq!(h.calls("turn.wait").len(), 1);

    h.fake.release("turn.wait");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l9"]);
}

#[tokio::test(start_paused = true)]
async fn a_capacity_refusal_on_turn_wait_is_retried() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_error_then(
        "turn.wait",
        -32019,
        "no deferred slot",
        "resource_exhausted",
        waited("l1", completed()),
    );
    h.submit("retry the wait");
    h.settle().await;
    assert_eq!(h.calls("turn.wait").len(), 2);
    assert_eq!(turn_ends(&h), ["l1"]);
    assert!(h.notices().is_empty());
}

fn diff_with(added: usize, removed: usize) -> Value {
    let lines: Vec<Value> = (0..removed)
        .map(|index| json!({"kind": "removed", "old_index": index, "text": "old", "line_complete": true}))
        .chain((0..added).map(
            |index| json!({"kind": "added", "new_index": index, "text": "new", "line_complete": true}),
        ))
        .collect();
    json!({"change_ref": "tool:a", "path": "app.py", "availability": "available",
           "complete": true, "binary": false, "stale": false, "truncated": false,
           "next_cursor": null,
           "hunks": [{"old_start": 0, "old_count": removed, "new_start": 0,
                      "new_count": added, "lines": lines}]})
}

#[tokio::test(start_paused = true)]
async fn the_turn_end_line_counts_changed_files_and_lines() {
    let record = |change_ref: &str, details: bool| {
        json!({"change_ref": change_ref, "path": "app.py", "kind": "modified",
               "details_available": details})
    };
    let cases = [
        (
            vec![record("tool:a", true)],
            diff_with(40, 7),
            "1 file changed (+40 −7)",
        ),
        (
            vec![record("tool:a", false)],
            diff_with(1, 1),
            "1 file changed",
        ),
        (
            vec![
                record("tool:w", true),
                record("tool:e", true),
                record("tool:p", true),
            ],
            diff_with(2, 1),
            "1 file changed (+6 −3)",
        ),
    ];
    for (records, diff, expected) in cases {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake.script("input.push", started("l1"));
        h.fake.script("turn.wait", waited("l1", completed()));
        h.fake.script(
            "changes.list",
            json!({"records": records, "total": records.len(), "complete": true, "warnings": []}),
        );
        h.fake.script("changes.diff", diff);
        h.submit("change it");
        h.settle().await;
        let screen = h.screen(100, 12);
        let ends = screen.lines().any(|row| row.contains(expected));
        assert!(ends, "{expected}:\n{screen}");
    }
}

#[tokio::test(start_paused = true)]
async fn late_changes_invalidate_only_the_turn_end_block() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script("turn.wait", waited("l1", completed()));
    h.submit("go");
    h.settle().await;
    let revisions = |h: &Harness| {
        h.app
            .session
            .as_ref()
            .unwrap()
            .transcript
            .revisions()
            .to_vec()
    };
    let before = revisions(&h);

    let summary = ChangeSummary {
        files: 3,
        lines: None,
    };
    h.op(SessionOp::Changes("l1".into(), Ok(summary)));
    let after = revisions(&h);
    let changed: Vec<usize> = (0..before.len())
        .filter(|&at| before[at] != after[at])
        .collect();
    assert_eq!(changed, [before.len() - 1]);
    assert!(h.screen(100, 12).contains("3 files changed"));
}

#[tokio::test(start_paused = true)]
async fn a_lost_wait_ends_the_turn_from_its_stored_result() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake
        .script_error("turn.wait", -32007, "turn not found", "turn_not_found");
    h.fake.script(
        "turn.result",
        json!({"turn": turn("l1"), "availability": "stored", "persistence": "persisted",
               "load": crate::testing::LOAD, "result_id": RESULT_ID,
               "total": 0, "items": [], "next_cursor": null, "outcome": completed(), "requests": 3}),
    );
    h.submit("go");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l1"]);
}

#[tokio::test(start_paused = true)]
async fn a_wait_read_failure_cannot_override_a_running_snapshot() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake
        .script_error("turn.wait", -32007, "turn not found", "turn_not_found");
    h.fake
        .script_error("turn.result", -32007, "turn not found", "turn_not_found");
    h.submit("go");
    h.settle().await;
    assert!(
        h.is_busy(),
        "only an authoritative snapshot can end execution"
    );
    assert!(
        turn_ends(&h).is_empty(),
        "read failure is not a terminal outcome"
    );
    h.control_event(json!({"execution":{"type":"none"}})).await;
    h.until(|app| !app.is_busy()).await;
}

#[tokio::test(start_paused = true)]
async fn an_oversized_turn_read_keeps_the_live_region_and_reports_the_missing_item() {
    use crate::app::tasks::{ReadPartial, TurnRead};

    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", cancelled()));
    h.fake
        .script_held("turn.result", turn_result("l1", vec![user(0, "l1", "go")]));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("l1"), "request_index":0,
        "channel":"text", "delta":"already shown"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks().iter().any(|block| {
            matches!(block,
        Block::Assistant { text, .. } if text == "already shown")
        })
    })
    .await;
    h.fake.release("turn.wait");
    h.until(|app| {
        app.session.as_ref().is_some_and(|session| {
            session
                .recovery
                .entries
                .values()
                .any(|entry| entry.turn.loop_id == "l1")
        })
    })
    .await;
    let body = |h: &Harness| {
        format!(
            "{:?}",
            h.blocks()
                .iter()
                .filter(|block| !matches!(block, Block::TurnEnd { .. }))
                .collect::<Vec<_>>()
        )
    };
    let before = body(&h);
    let session = h.app.session.as_ref().unwrap();
    let seq = *session.recovery.entries.first_key_value().unwrap().0;
    let generation = session.recovery.generation;
    h.op(SessionOp::Recovered(
        crate::app::recovery::RecoveryOutcome {
            seq,
            generation,
            turn: crate::protocol::TurnRef::new("s1", "l1"),
            result: Ok(TurnRead::Partial(Box::new(ReadPartial::Oversized {
                index: 1,
                total_bytes: 8_388_609,
            }))),
        },
    ));
    assert_eq!(
        body(&h),
        before,
        "the received live body is preserved byte for byte"
    );
    assert_eq!(
        turn_ends(&h),
        ["l1"],
        "a partial result has one persistent terminal, not an empty canonical answer"
    );
    assert!(!h.app.session.as_ref().unwrap().is_closed("l1"));
    assert_eq!(
        h.app.session.as_ref().unwrap().body_recovery[0].status,
        crate::app::recovery::RecoveryStatus::Partial
    );
    assert_eq!(h.app.session.as_ref().unwrap().reconcile_from.len(), 1);
    assert!(
        h.notices()
            .iter()
            .any(|notice| notice.contains("Item 1 is too large"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_successor_started_before_the_finished_turn_reconciles_stays_after_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let next = entry("i2", "follow_up", "next");
    h.fake
        .script_sequence("input.push", vec![started("l1"), pushed(vec![next])]);
    h.fake.script_sequence_later(
        "turn.wait",
        vec![
            (
                waited_then(
                    "l1",
                    completed(),
                    Some(next_turn("l2", "next")),
                    queue(vec![], false),
                ),
                Duration::from_secs(1),
            ),
            (waited("l2", completed()), Duration::from_secs(60)),
        ],
    );
    h.fake.script_held(
        "turn.result",
        turn_result(
            "l1",
            vec![
                user(0, "l1", "start"),
                assistant_tool(1, "l1", "c1", "bash"),
                assistant(2, "l1", "partial answer"),
            ],
        ),
    );
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    // A gap in the events makes the durable items replace what streamed.
    h.emit(
        "request_started",
        json!({"turn": turn("l1"), "request_index": 0, "model": "mock",
               "meta": {"session_id": "s1", "dropped_before": 2}}),
    )
    .await;
    h.app.composer.set_text("next");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    // The turn ends and the successor starts while the stored items are still on their way.
    h.until(|app| app.transcript_blocks().len() >= 3).await;
    h.run_for(Duration::from_secs(3)).await;
    assert_eq!(h.sends(), ["start", "next"]);

    h.fake.release("turn.result");
    h.run_for(Duration::from_secs(2)).await;
    assert_eq!(h.texts(), ["start", "partial answer", "next"]);
    let shape: Vec<&str> = h
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User { .. } => "user",
            Block::Assistant { .. } => "assistant",
            Block::Tool(_) => "tool",
            Block::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["user", "tool", "assistant", "end", "user"]);
    let live = h.app.session.as_ref().unwrap().live.as_ref().unwrap();
    assert_eq!(live.start, 4, "the running turn still starts at its prompt");

    h.run_for(Duration::from_secs(120)).await;
    assert_eq!(turn_ends(&h), ["l1", "l2"]);
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn an_interrupted_turn_keeps_its_card_details_and_unfinished_thinking() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", cancelled()));
    h.fake.script(
        "turn.result",
        turn_result(
            "l1",
            vec![
                user(0, "l1", "go"),
                assistant_tool(1, "l1", "c1", "bash"),
                assistant_tool(2, "l1", "c2", "bash"),
            ],
        ),
    );
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    for (id, display) in [
        (
            "c1",
            json!({"detail": "$ sleep 90", "expanded_input": "sleep 90"}),
        ),
        (
            "c2",
            json!({"detail": "", "expanded_input": "echo hi\nsecond line"}),
        ),
    ] {
        h.emit(
            "tool_presentation",
            json!({"turn": turn("l1"), "request_index": 0, "tool_call_id": id,
                   "tool_name": "bash", "display": display}),
        )
        .await;
    }
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 1, "channel": "reasoning",
               "delta": "Implementing strict import validation"}),
    )
    .await;
    h.settle().await;
    h.fake.release("turn.wait");
    h.settle().await;

    let cards: Vec<(String, Option<String>)> = h
        .app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .tool_calls()
        .map(|call| (call.detail.clone(), call.expanded_input.clone()))
        .collect();
    assert_eq!(
        cards,
        [
            ("$ sleep 90".into(), Some("sleep 90".into())),
            ("$ echo hi".into(), Some("echo hi\nsecond line".into())),
        ]
    );
    let thinking = h.blocks().iter().any(|block| {
        matches!(block, Block::Assistant { reasoning, streaming: false, .. }
            if reasoning == "Implementing strict import validation")
    });
    assert!(thinking, "{:?}", h.blocks());
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn the_interrupted_reply_of_a_cancelled_turn_replaces_its_streamed_block_in_place() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", cancelled()));
    let mut result = turn_result(
        "l1",
        vec![user(0, "l1", "go"), assistant_tool(1, "l1", "c1", "bash")],
    );
    result["interrupted"] = json!({"stop_reason": "aborted", "request_index": 1, "model": "mock",
                                   "reasoning": "", "text": "I will start by readi"});
    h.fake.script("turn.result", result);
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn": turn("l1"), "request_index": 1, "channel": "text", "delta": "I will st"}),
    )
    .await;
    h.settle().await;
    h.fake.release("turn.wait");
    h.settle().await;

    let shape: Vec<&str> = h
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User { .. } => "user",
            Block::Assistant { .. } => "assistant",
            Block::Tool(_) => "tool",
            Block::TurnEnd { .. } => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(shape, ["user", "tool", "assistant", "end"]);
    assert_eq!(h.texts(), ["go", "I will start by readi"]);
}

#[tokio::test(start_paused = true)]
async fn the_turn_start_and_end_leave_an_anchored_view_where_it_is() {
    let anchor = Scroll::Anchor {
        block: 2,
        offset: 1,
    };
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "input.push",
        vec![started("l1"), pushed(vec![entry("i2", "follow_up", "two")])],
    );
    h.fake.script_sequence_later(
        "turn.wait",
        vec![
            (
                waited_then(
                    "l1",
                    completed(),
                    Some(next_turn("l2", "two")),
                    queue(vec![], false),
                ),
                Duration::from_secs(1),
            ),
            (waited("l2", completed()), Duration::from_secs(1)),
        ],
    );
    h.submit("one");
    assert_eq!(h.app.scroll, Scroll::Follow, "sending follows");
    h.until(|app| app.is_busy()).await;
    h.app.composer.set_text("two");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.app.scroll = anchor;
    // The turn ends and the Agent starts the queued follow-up by itself.
    h.until(|app| app.transcript_blocks().len() >= 3).await;
    h.run_for(Duration::from_secs(3)).await;
    assert_eq!(h.sends(), ["one", "two"]);
    h.settle().await;
    assert_eq!(h.app.scroll, anchor);
    assert!(!h.is_busy());

    h.fake.script("turn.wait", waited("l9", completed()));
    h.state(json!({"session_id": "s1", "status": "running",
                   "active_loop": {"loop_id": "l9"}}));
    assert_eq!(
        h.app.scroll, anchor,
        "adopting a running turn does not follow"
    );
}

#[tokio::test(start_paused = true)]
async fn the_footer_totals_move_with_each_request_and_the_wait_settles_them() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": completed(),
               "usage": {"input_tokens": 300, "output_tokens": 30}, "requests": 3}),
    );
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    let usage = |h: &Harness| h.app.session.as_ref().unwrap().usage.clone();

    for _ in 0..2 {
        h.emit(
            "request_usage",
            json!({"turn": turn("l1"), "usage": {"input_tokens": 100, "output_tokens": 10}}),
        )
        .await;
    }
    h.until(|app| app.session.as_ref().unwrap().usage.input == 200)
        .await;
    assert_eq!(usage(&h).output, 20);

    // The third request's event was dropped; the wait has the whole turn.
    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;
    h.until(|app| app.session.as_ref().unwrap().usage.input == 300)
        .await;
    assert_eq!((usage(&h).input, usage(&h).output), (300, 30));
}

#[tokio::test(start_paused = true)]
async fn a_refusal_with_unknown_acceptance_keeps_the_text_out_of_the_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "input.push",
        vec![
            json!({"error": {"code": -32000, "message": "the Agent closed mid-push",
                              "data": {"acceptance": "unknown"}}}),
        ],
    );
    h.submit("do not lose this twice");
    h.settle().await;
    assert_eq!(
        h.app.composer.text(),
        "",
        "an unknown acceptance must not invite a second send"
    );
}

#[tokio::test(start_paused = true)]
async fn a_refusal_classified_as_rejected_returns_the_text() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "input.push",
        vec![
            json!({"error": {"code": -32000, "message": "too many queued",
                              "data": {"acceptance": "rejected"}}}),
        ],
    );
    h.submit("send me back");
    h.settle().await;
    assert_eq!(h.app.composer.text(), "send me back");
}

#[tokio::test(start_paused = true)]
async fn a_receipt_for_another_instance_does_not_start_or_queue_anything() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "input.push",
        json!({"session_id": "s1", "load": "load_00000000000000000000000000000000",
               "accepted": true, "id": "inp_1", "started": null,
               "queue": {"items": [], "paused": false}}),
    );
    h.submit("stale instance");
    h.settle().await;
    assert!(
        h.calls("session.state").len() >= 2,
        "a stale receipt asks for the current control instead of applying itself"
    );
}

#[tokio::test(start_paused = true)]
async fn a_start_failure_keeps_the_entry_queued_and_reports_its_reason() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "input.push",
        json!({"session_id": "s1", "load": crate::testing::LOAD, "accepted": true,
               "id": "inp_1", "started": null,
               "start_error": {"input_id": "inp_0", "kind": "provider_unavailable",
                               "message": "no provider is configured"},
               "queue": {"items": [{"id": "inp_1", "mode": "steer", "text": "later"}],
                         "paused": false}}),
    );
    h.submit("later");
    h.settle().await;
    assert!(
        h.screen(100, 30).contains("no provider is configured"),
        "the start failure is reported, not swallowed"
    );
    assert_eq!(h.queue(), ["later"], "the entry the Agent kept is shown");
}

#[tokio::test(start_paused = true)]
async fn a_completion_whose_region_was_evicted_does_not_close_the_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", started("l1"));
    h.fake.script_held("turn.wait", waited("l1", cancelled()));
    h.fake
        .script_held("turn.result", turn_result("l1", vec![user(0, "l1", "go")]));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("l1"), "request_index":1, "channel":"text", "delta":"streamed"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block, Block::Assistant { text, .. } if text == "streamed"))
    })
    .await;
    h.fake.release("turn.wait");
    // The bounded region list drops old turns, so by the time this completion
    // arrives its streamed region is no longer recorded.
    h.until(|app| {
        app.session
            .as_ref()
            .is_some_and(|session| !session.reconcile_from.is_empty())
    })
    .await;
    h.app.session.as_mut().unwrap().reconcile_from.clear();
    h.fake.release("turn.result");
    h.settle().await;

    let session = h.app.session.as_ref().unwrap();
    assert!(
        !session.is_closed("l1"),
        "a completion that installed nothing must leave the turn repairable"
    );
    assert!(
        session
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::TurnEnd { saving: None, .. })),
        "and must not state a saving fact it did not establish: {:?}",
        h.blocks()
    );
}

#[tokio::test(start_paused = true)]
async fn receipts_wait_delivery_and_context_cannot_overwrite_snapshot_control() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("session.state", json!({"session_id":"s1"}));
    h.fake.script_held("turn.wait", waited("A", completed()));
    let mut authoritative = h.control(json!({
        "execution":{"type":"running","loop_id":"A","input":entry("iA","steer","prompt A"),"request_index":0,"phase":"running_model"},
        "queue":queue(vec![entry("kept","steer","kept")],true),
        "compaction":{"operation_id":"current","phase":"summarizing","covered_item_count":1,"retained_item_count":0}
    }));
    h.app
        .apply_snapshot(serde_json::from_value(authoritative.clone()).unwrap());
    h.fake.script_held("input.push", pushed(vec![]));
    h.app.submit_text("accepted".into(), false);
    let operation = h.app.local_inputs.back().unwrap().id;
    h.app.on_msg(crate::app::Msg::LocalPushed(
        operation,
        Ok(serde_json::from_value(json!({
            "id":"new","accepted":true,"load":crate::testing::LOAD,
            "started":next_turn("B","not authoritative"),"queue":queue(vec![],false)
        }))
        .unwrap()),
    ));
    h.op(SessionOp::Waited(Ok(serde_json::from_value(waited_then(
        "A",
        completed(),
        Some(next_turn("B", "not authoritative")),
        queue(vec![], false),
    ))
    .unwrap())));
    h.op(SessionOp::Compacted(Ok(serde_json::from_value(
        json!({"operation_id":"old","status":"noop","next":next_turn("B","not authoritative")}),
    )
    .unwrap())));
    h.context(json!({"session_id":"s1"}));
    let session = h.app.session.as_mut().unwrap();
    session.apply_body_event(serde_json::from_value(json!({"type":"input_delivered","data":{
        "meta":{"session_id":"s1","load":crate::testing::LOAD,"dropped_before":0},"turn":turn("A"),"items":[{"id":"kept","text":"kept","request_index":1}]
    }})).unwrap(),256);
    assert_eq!(session.observed_loop(), Some("A"));
    assert_eq!(session.live.as_ref().unwrap().turn.loop_id, "A");
    assert_eq!(session.queue.items[0].id, "kept");
    assert!(session.queue.paused);
    assert_eq!(session.compaction.as_ref().unwrap().operation_id, "current");
    authoritative["revision"] = json!(authoritative["revision"].as_u64().unwrap() + 1);
    authoritative["execution"] = json!({"type":"none"});
    authoritative["queue"] = queue(vec![], false);
    authoritative["compaction"] = serde_json::Value::Null;
    h.app
        .apply_snapshot(serde_json::from_value(authoritative).unwrap());
    assert!(!h.is_busy());
    assert!(h.queue().is_empty());
    assert!(!h.app.compacting());
}

#[tokio::test(start_paused = true)]
async fn an_unclassified_push_error_keeps_the_text_unconfirmed_not_unsent() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error_with(
        "input.push",
        -32603,
        "No acceptance fact",
        "internal_error",
        None,
    );
    h.submit("possibly accepted");
    h.app.composer.set_text("new draft");
    h.settle().await;
    assert_eq!(h.app.composer.text(), "new draft");
    assert_eq!(
        h.app.local_inputs.len(),
        1,
        "an unclassified error must retain an operation, not just a transient notice"
    );
    assert_eq!(
        h.app.local_inputs[0].status,
        crate::app::unconfirmed::Status::Unconfirmed
    );
    assert_eq!(h.app.local_inputs[0].entry.text(), "possibly accepted");
}

#[tokio::test(start_paused = true)]
async fn catalog_eviction_does_not_release_a_recovery_slot_before_its_read_finishes() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.result", turn_result("l1", vec![]));
    let directory = |start: u64| json!({"through_seq":start+255,"retained_from_seq":start,"truncated":start>1,"entries":(start..start+256).map(|seq|json!({"seq":seq,"loop_id":format!("l{seq}"),"completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"})).collect::<Vec<_>>()});
    h.control_event(json!({"completions":directory(1)})).await;
    h.until(|app| app.session.as_ref().unwrap().recovery_tasks.len() == 2)
        .await;
    h.fake.wait_for_calls("turn.result", 2).await;
    h.control_event(json!({"completions":directory(257)})).await;
    h.settle().await;
    let session = h.app.session.as_ref().unwrap();
    assert_eq!(session.recovery_tasks.len(), 2);
    assert_eq!(h.calls("turn.result").len(), 2);
    assert_eq!(session.recovery.observed_through, 512);
    assert_eq!(session.recovery.settled_through, 0);
    assert_eq!(session.recovery.entries.len(), 256);
    assert!(session.recovery.truncated_gap);
    h.app.session.as_mut().unwrap().recovery.retry_later(257);
    tokio::time::advance(Duration::from_secs(1)).await;
    h.app.notices.clear();
    h.app.flash = None;
    h.app.scrolled_at = None;
    assert!(
        h.app.next_expiry().is_none(),
        "an overdue retry cannot busy-poll while both reads still own their slots"
    );
}

#[tokio::test(start_paused = true)]
async fn pending_recovery_exhausts_without_settling_or_losing_body_and_unrelated_revisions_do_not_restart_it()
 {
    use crate::app::recovery::RecoveryStatus;
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.running_state("l1", entry("i1", "steer", "question"));
    h.emit(
        "output_delta",
        json!({"turn":turn("l1"),"request_index":0,"channel":"text","delta":"already seen"}),
    )
    .await;
    h.settle().await;
    h.fake.script("turn.result",json!({"turn":turn("l1"),"load":crate::testing::LOAD,"availability":"pending","total":0,"items":[],"next_cursor":null}));
    let completions = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"l1","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live"}]});
    h.control_event(json!({"execution":{"type":"none"},"completions":completions}))
        .await;
    h.settle().await;
    for delay in [
        Duration::from_millis(250),
        Duration::from_secs(1),
        Duration::from_secs(4),
    ] {
        tokio::time::advance(delay).await;
        h.app.tick();
        h.settle().await;
    }
    assert_eq!(h.calls("turn.result").len(), 4);
    let session = h.app.session.as_ref().unwrap();
    assert_eq!(
        session.recovery.entries[&1].status,
        RecoveryStatus::Exhausted
    );
    assert_eq!(session.recovery.observed_through, 1);
    assert_eq!(session.recovery.settled_through, 0);
    assert!(!session.is_closed("l1"));
    assert_eq!(h.texts(), ["question", "already seen"]);
    assert_eq!(session.body_recovery[0].status, RecoveryStatus::Exhausted);
    h.control_event(json!({"execution":{"type":"none"},"completions":completions}))
        .await;
    h.settle().await;
    h.app.tick();
    h.settle().await;
    assert_eq!(
        h.calls("turn.result").len(),
        4,
        "same availability at a fresh revision must not restart retries"
    );
    h.fake.script("turn.result",json!({"turn":turn("l1"),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":"expired","persistence":"failed","total":0,"items":[],"next_cursor":null}));
    h.action(Action::RetryRecovery);
    h.settle().await;
    let session = h.app.session.as_ref().unwrap();
    assert_eq!(session.recovery.settled_through, 1);
    assert_eq!(session.body_recovery[0].status, RecoveryStatus::Unavailable);
    assert!(session.body_recovery[0].message.contains("not saved"));
    assert_eq!(h.texts(), ["question", "already seen"]);
    assert!(
        !session.is_closed("l1"),
        "explicit unavailability preserves seen content but is not canonical installation"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stale_load_body_cannot_evict_the_installed_loads_early_cache() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("unknown"),"request_index":0,"channel":"text","delta":"kept"}),
    )
    .await;
    h.settle().await;
    let before = h.app.session.as_ref().unwrap().early_bytes;
    h.emit_no_meta("output_delta",json!({"meta":{"session_id":"s1","load":"closed-load","dropped_before":0},"turn":turn("stale"),"request_index":0,"channel":"text","delta":"x".repeat(9*1024*1024)})).await;
    h.settle().await;
    let session = h.app.session.as_ref().unwrap();
    assert_eq!(session.early_bytes, before);
    assert!(!session.early_gap);
    assert!(session.transcript.is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unseen_completion_inserted_before_live_content_rebases_the_actual_view_anchor() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("C", completed()));
    let input = entry("iC", "steer", "prompt C");
    h.running_state("C", input.clone());
    h.emit(
        "output_delta",
        json!({"turn":turn("C"),"request_index":0,"channel":"text","delta":"live C"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant { text,.. } if text == "live C"))
    })
    .await;
    h.app.scroll = Scroll::Anchor {
        block: 1,
        offset: 1,
    };
    h.app.composer.set_text("keep the draft");
    let mut result = turn_result(
        "A",
        vec![user(0, "A", "prompt A"), assistant(1, "A", "answer A")],
    );
    result["outcome"] = completed();
    h.fake.script("turn.result", result);
    h.control_event(json!({"execution":{"type":"running","loop_id":"C","input":input,"request_index":0,"phase":"running_model"},"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.session.as_ref().unwrap().is_closed("A"))
        .await;
    assert_eq!(h.texts(), ["prompt A", "answer A", "prompt C", "live C"]);
    assert_eq!(
        h.app.session.as_ref().unwrap().live.as_ref().unwrap().start,
        3
    );
    assert_eq!(
        h.app.scroll,
        Scroll::Anchor {
            block: 4,
            offset: 1
        },
        "a zero-width insertion still moves the anchored old content"
    );
    assert_eq!(h.app.composer.text(), "keep the draft");
}

#[tokio::test(start_paused = true)]
async fn a_saved_anchor_after_a_recovered_unsaved_turn_controls_unseen_completion_order() {
    let mut history = page(
        4,
        0,
        vec![
            user(0, "S", "saved S"),
            assistant(1, "S", "answer S"),
            user(2, "B", "saved B"),
            assistant(3, "B", "answer B"),
        ],
    );
    history["records"] =
        json!([{"loop_id":"S","outcome":completed()},{"loop_id":"B","outcome":completed()}]);
    let mut h = Harness::opened(history).await;
    let result = |id: &str, text: &str| {
        let mut page = turn_result(
            id,
            vec![user(0, id, &format!("prompt {id}")), assistant(1, id, text)],
        );
        page["outcome"] = completed();
        page["persistence"] = json!(if id == "B" { "persisted" } else { "failed" });
        page
    };
    h.fake
        .script_sequence("turn.result", vec![result("A", "answer A")]);
    h.fake.script_held("turn.result", result("B", "answer B"));
    h.control_event(json!({"completions":{"through_seq":3,"retained_from_seq":1,"truncated":false,"entries":[
        {"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live","after_saved_loop_id":"S"},
        {"seq":2,"loop_id":"B","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored","after_saved_loop_id":"S"},
        {"seq":3,"loop_id":"C","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live","after_saved_loop_id":"B"}
    ]}})).await;
    h.until(|app| app.session.as_ref().unwrap().recovery_tasks.len() == 2)
        .await;
    h.fake.wait_for_calls("turn.result", 2).await;
    h.fake
        .script_sequence("turn.result", vec![result("C", "answer C")]);
    h.until(|app| app.session.as_ref().unwrap().is_closed("C"))
        .await;
    assert!(
        !h.app.session.as_ref().unwrap().is_closed("B"),
        "B's reader is still held"
    );
    assert_eq!(
        turn_ends(&h),
        ["S", "A", "B", "C"],
        "the saved B anchor is stronger than the earlier reconciled A"
    );
    h.fake.release("turn.result");
    h.until(|app| app.session.as_ref().unwrap().is_closed("B"))
        .await;
    assert_eq!(turn_ends(&h), ["S", "A", "B", "C"]);
}

#[tokio::test(start_paused = true)]
async fn delayed_predecessor_waits_preserve_successor_usage_in_both_reply_orders() {
    for order in [["A", "B"], ["B", "A"]] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake.script_held("turn.wait", waited("A", completed()));
        h.fake
            .script_held("session.state", json!({"session_id":"s1"}));
        h.app.session.as_mut().unwrap().usage = crate::protocol::UsageTotals {
            input: 1000,
            output: 100,
            cached: 50,
        };
        h.running_state("A", entry("iA", "steer", "A"));
        h.emit("request_usage",json!({"turn":turn("A"),"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":5}})).await;
        h.until(|app| app.session.as_ref().unwrap().usage.input == 1100)
            .await;
        h.running_state("B", entry("iB", "steer", "B"));
        h.emit("request_usage",json!({"turn":turn("B"),"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":7}})).await;
        h.until(|app| app.session.as_ref().unwrap().usage.input == 1300)
            .await;
        let mut a = 100;
        let mut b = 200;
        let mut ac = 5;
        let mut bc = 7;
        for id in order {
            let (input, cached) = if id == "A" {
                a = 150;
                ac = 8;
                (a, ac)
            } else {
                b = 250;
                bc = 9;
                (b, bc)
            };
            h.op(SessionOp::Waited(Ok(serde_json::from_value(json!({"turn":turn(id),"outcome":completed(),"usage":{"input_tokens":input,"output_tokens":input/10,"cache_read_tokens":cached}})).unwrap())));
            let usage = &h.app.session.as_ref().unwrap().usage;
            assert_eq!(
                (usage.input, usage.output, usage.cached),
                (1000 + a + b, 100 + (a + b) / 10, 50 + ac + bc),
                "reply order {order:?}, after {id}"
            );
            assert_eq!(
                h.app.session.as_ref().unwrap().observed_loop(),
                Some("B"),
                "wait metadata cannot change control"
            );
        }
        let before = h.app.session.as_ref().unwrap().usage.clone();
        h.op(SessionOp::Waited(Ok(serde_json::from_value(json!({"turn":turn("A"),"outcome":completed(),"usage":{"input_tokens":150,"output_tokens":15,"cache_read_tokens":8}})).unwrap())));
        assert_eq!(
            h.app.session.as_ref().unwrap().usage,
            before,
            "a duplicate terminal receipt is not counted twice"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn unseen_unavailable_completions_keep_terminal_identity_reason_and_saving_visible() {
    for (id, kind, outcome, persistence, reason, expected) in [
        (
            "internal",
            "internal_error",
            serde_json::Value::Null,
            serde_json::Value::Null,
            "internal_error",
            "internal",
        ),
        (
            "unsaved",
            "reported",
            json!("completed"),
            json!("failed"),
            "expired",
            "not saved",
        ),
        (
            "large",
            "reported",
            json!("failed"),
            json!("failed"),
            "too_large",
            "too large",
        ),
    ] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        #[cfg(feature = "e2e-observe")]
        let trace = tempfile::NamedTempFile::new().unwrap();
        #[cfg(feature = "e2e-observe")]
        h.app.start_observer(trace.reopen().unwrap());
        h.fake.script("turn.result",json!({"turn":turn(id),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":reason,"persistence":persistence,"items":[],"total":0,"next_cursor":null}));
        h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":id,"completion_kind":kind,"outcome_kind":outcome,"persistence":persistence,"availability":"unavailable","unavailable_reason":reason}]}})).await;
        h.until(|app| app.session.as_ref().unwrap().recovery.settled_through == 1)
            .await;
        h.app.notices.clear();
        let screen = h.screen(120, 30).to_lowercase();
        assert!(
            screen.contains(id),
            "persistent terminal identity is missing: {screen}"
        );
        assert!(
            screen.contains(expected),
            "persistent reason/saving is missing: {screen}"
        );
        assert!(
            screen.contains("unavailable"),
            "body unavailability is not visible: {screen}"
        );
        if persistence.is_null() {
            assert!(
                screen.contains("save unknown"),
                "an unknown save must not look saved: {screen}"
            );
            assert!(
                screen.contains("unknown outcome"),
                "an absent report must not imply a completed model outcome: {screen}"
            );
        }
        assert!(
            h.blocks()
                .iter()
                .any(|block| matches!(block,Block::TurnEnd { loop_id,.. } if loop_id == id))
        );
        assert!(
            !h.app.session.as_ref().unwrap().is_closed(id),
            "a terminal placeholder is not a canonical empty result"
        );
        #[cfg(feature = "e2e-observe")]
        {
            let rows: Vec<Value> = std::fs::read_to_string(trace.path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let status = rows
                .iter()
                .find(|row| row["event"] == "recovery_status")
                .unwrap();
            assert_eq!(status["loop_id"], id);
            assert_eq!(status["status"], "unavailable");
            assert_eq!(status["canonical"], false);
            assert!(status["terminal"]["block_index"].is_u64());
            assert!(!rows.iter().any(|row| row["event"] == "canonical_install"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_unseen_partial_completion_has_a_persistent_noncanonical_terminal() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    #[cfg(feature = "e2e-observe")]
    let trace = tempfile::NamedTempFile::new().unwrap();
    #[cfg(feature = "e2e-observe")]
    h.app.start_observer(trace.reopen().unwrap());
    let directory:crate::protocol::CompletionDirectory = serde_json::from_value(json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"partial-loop","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"live"}]})).unwrap();
    let session = h.app.session.as_mut().unwrap();
    session.catalog = directory.clone();
    session.recovery.discover(&directory);
    let generation = session.recovery.generation;
    h.op(SessionOp::Recovered(
        crate::app::recovery::RecoveryOutcome {
            seq: 1,
            turn: crate::protocol::TurnRef::new("s1", "partial-loop"),
            generation,
            result: Ok(crate::app::tasks::TurnRead::Partial(Box::new(
                crate::app::tasks::ReadPartial::Oversized {
                    index: 0,
                    total_bytes: 8_388_609,
                },
            ))),
        },
    ));
    h.app.notices.clear();
    let screen = h.screen(120, 30).to_lowercase();
    for text in ["partial-loop", "partial", "not saved", "too large"] {
        assert!(
            screen.contains(text),
            "{text} is missing from persistent UI: {screen}"
        );
    }
    assert!(!h.app.session.as_ref().unwrap().is_closed("partial-loop"));
    #[cfg(feature = "e2e-observe")]
    {
        let rows: Vec<Value> = std::fs::read_to_string(trace.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let status = rows
            .iter()
            .find(|row| row["event"] == "recovery_status")
            .unwrap();
        assert_eq!(status["status"], "partial");
        assert_eq!(status["terminal"]["saving"], "not saved");
        assert_eq!(status["canonical"], false);
        assert!(!rows.iter().any(|row| row["event"] == "canonical_install"));
    }
}

#[tokio::test(start_paused = true)]
async fn unavailable_regions_keep_sequence_order_and_late_received_body_before_their_terminal() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let response = |id| json!({"turn":turn(id),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":"expired","persistence":"failed","items":[],"total":0,"next_cursor":null});
    h.fake
        .script_sequence("turn.result", vec![response("A"), response("B")]);
    h.control_event(json!({"completions":{"through_seq":2,"retained_from_seq":1,"truncated":false,"entries":[
        {"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"unavailable"},
        {"seq":2,"loop_id":"B","completion_kind":"reported","outcome_kind":"failed","persistence":"failed","availability":"unavailable"}
    ]}})).await;
    h.until(|app| app.session.as_ref().unwrap().recovery.settled_through == 2)
        .await;
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":"late received A"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant { text,.. } if text == "late received A"))
    })
    .await;
    let order: Vec<_> = h
        .blocks()
        .iter()
        .filter_map(|block| block.loop_id())
        .collect();
    assert_eq!(order, ["A", "A", "B"]);
    assert_eq!(turn_ends(&h), ["A", "B"]);
    assert!(!h.app.session.as_ref().unwrap().is_closed("A"));
}

#[tokio::test(start_paused = true)]
async fn a_partial_history_region_keeps_late_body_before_its_existing_terminal() {
    let mut history = page(
        2,
        0,
        vec![user(0, "A", "prompt A"), assistant(1, "A", "tail A")],
    );
    history["records"] = json!([{"loop_id":"A","outcome":completed()}]);
    let mut h = Harness::opened(history).await;
    let directory = serde_json::from_value(json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[
        {"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}
    ]})).unwrap();
    let session = h.app.session.as_mut().unwrap();
    session.catalog = directory;
    session.recovery.discover(&session.catalog);
    let generation = session.recovery.generation;
    h.op(SessionOp::Recovered(
        crate::app::recovery::RecoveryOutcome {
            seq: 1,
            generation,
            turn: crate::protocol::TurnRef::new("s1", "A"),
            result: Ok(crate::app::tasks::TurnRead::Partial(Box::new(
                crate::app::tasks::ReadPartial::WorkspaceLimit,
            ))),
        },
    ));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":1,"channel":"text","delta":"late retained A"}),
    )
    .await;
    h.until(|app|app.transcript_blocks().iter().any(|block|matches!(block,Block::Assistant {key,text,..} if key.request_index == 1 && text == "late retained A"))).await;
    assert!(
        matches!(h.blocks().last(),Some(Block::TurnEnd {loop_id,saving:Some(crate::transcript::Saving::Saved),..}) if loop_id == "A")
    );
    assert_eq!(h.texts(), ["prompt A", "tail A", "late retained A"]);
    assert!(!h.app.session.as_ref().unwrap().is_closed("A"));
}

#[tokio::test(start_paused = true)]
async fn a_normal_completed_turn_keeps_its_late_tail_until_canonical_repair_then_rejects_duplicates()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.fake
        .script_held("session.state", json!({"session_id":"s1"}));
    h.running_state("A", entry("iA", "steer", "prompt A"));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":"prefix"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant{text,..} if text=="prefix"))
    })
    .await;
    assert!(!h.app.session.as_ref().unwrap().live.as_ref().unwrap().gap);
    h.op(SessionOp::Waited(Ok(serde_json::from_value(waited(
        "A",
        completed(),
    ))
    .unwrap())));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":" tail"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant{text,..} if text=="prefix tail"))
    })
    .await;
    let mut canonical = turn_result(
        "A",
        vec![
            user(0, "A", "prompt A"),
            assistant(1, "A", "prefix tail repaired"),
        ],
    );
    canonical["outcome"] = completed();
    h.fake.script_held("turn.result", canonical);
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","input_id":"iA","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.session.as_ref().unwrap().recovery_fetching() == 1)
        .await;
    h.fake.wait_for_calls("turn.result", 1).await;
    assert!(!h.app.session.as_ref().unwrap().is_closed("A"));
    assert_eq!(h.texts(), ["prompt A", "prefix tail"]);
    h.fake.release("turn.result");
    h.until(|app| app.session.as_ref().unwrap().is_closed("A"))
        .await;
    assert_eq!(h.texts(), ["prompt A", "prefix tail repaired"]);
    let before = format!("{:?}", h.blocks());
    for (request, channel, text) in [
        (0, "text", " tail"),
        (0, "reasoning", "late thinking"),
        (99, "text", "late new request"),
    ] {
        h.emit(
            "output_delta",
            json!({"turn":turn("A"),"request_index":request,"channel":channel,"delta":text}),
        )
        .await;
    }
    h.settle().await;
    assert_eq!(
        format!("{:?}", h.blocks()),
        before,
        "canonical installation closes every body channel, not just the last streamed key"
    );
    assert_eq!(turn_ends(&h), ["A"]);
    assert_eq!(h.app.session.as_ref().unwrap().recovery.settled_through, 1);
    assert_eq!(
        h.calls("turn.result").len(),
        1,
        "normal no-gap completion still reconciles exactly once"
    );
}

#[tokio::test(start_paused = true)]
async fn a_tool_record_saved_after_canonical_install_updates_only_its_auxiliary_facts() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let result = turn_result(
        "A",
        vec![
            user(0, "A", "prompt A"),
            assistant_tool(1, "A", "call-A", "bash"),
            assistant(2, "A", "answer A"),
        ],
    );
    h.fake.script("turn.result", result);
    h.fake.script("tool.read",json!({"execution":{"tool_ref":{"session_id":"s1","loop_id":"A","request_index":0,"tool_call_id":"call-A"},"state":"succeeded","recording":"memory_only","result_truncated":false}}));
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.session.as_ref().unwrap().is_closed("A"))
        .await;
    let card = || crate::protocol::ToolRef {
        session_id: "s1".into(),
        loop_id: "A".into(),
        request_index: 0,
        tool_call_id: "call-A".into(),
    };
    assert_eq!(
        h.app
            .session
            .as_ref()
            .unwrap()
            .transcript
            .find_tool(&card())
            .unwrap()
            .recording,
        Some(crate::protocol::ToolRecordingState::MemoryOnly)
    );
    let before = h.texts();
    let reads = h.calls("tool.read").len();
    let result_reads = h.calls("turn.result").len();
    for (loop_id, call_id, load) in [
        ("other-loop", "call-A", crate::testing::LOAD),
        ("A", "other-call", crate::testing::LOAD),
        ("A", "call-A", "old-load"),
    ] {
        h.emit_no_meta("tool_execution",json!({"meta":{"session_id":"s1","load":load,"dropped_before":0},"turn":turn(loop_id),"data":{"tool_ref":{"session_id":"s1","loop_id":loop_id,"request_index":0,"tool_call_id":call_id},"state":"succeeded","recording":"saved","result_truncated":false}})).await;
    }
    h.settle().await;
    assert_eq!(
        h.app
            .session
            .as_ref()
            .unwrap()
            .transcript
            .find_tool(&card())
            .unwrap()
            .recording,
        Some(crate::protocol::ToolRecordingState::MemoryOnly)
    );
    // The Agent publishes its auxiliary commit as ToolExecution(recording=saved).
    h.emit("tool_execution",json!({"turn":turn("A"),"data":{"tool_ref":{"session_id":"s1","loop_id":"A","request_index":0,"tool_call_id":"call-A"},"state":"succeeded","recording":"saved","finished_at":"2026-10-31T00:00:01Z","started_at":"2026-10-31T00:00:00Z","output_line_count":7,"result_truncated":false}})).await;
    h.settle().await;
    let call = h
        .app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .find_tool(&card())
        .unwrap();
    assert_eq!(
        call.recording,
        Some(crate::protocol::ToolRecordingState::Saved),
        "the late auxiliary save event must update the already closed card"
    );
    assert_eq!(call.output_lines, Some(7));
    assert_eq!(call.duration, Some(Duration::from_secs(1)));
    assert_eq!(call.state, ToolState::Succeeded);
    assert_eq!(h.texts(), before);
    assert_eq!(
        h.blocks()
            .iter()
            .filter(|block| matches!(block, Block::Tool(_)))
            .count(),
        1
    );
    assert_eq!(turn_ends(&h), ["A"]);
    assert_eq!(
        h.calls("tool.read").len(),
        reads,
        "the actual late commit event, not query backfill, updates the card"
    );
    assert_eq!(h.calls("turn.result").len(), result_reads);
    assert!(h.app.session.as_ref().unwrap().is_closed("A"));
}

#[tokio::test(start_paused = true)]
async fn late_identical_steers_keep_their_request_boundaries_before_an_incomplete_turn_end_and_successor()
 {
    for partial in [false, true] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        #[cfg(feature = "e2e-observe")]
        let trace = tempfile::NamedTempFile::new().unwrap();
        #[cfg(feature = "e2e-observe")]
        h.app.start_observer(trace.reopen().unwrap());
        h.fake.script_held("turn.wait", waited("A", completed()));
        h.running_state("A", entry("input-A", "steer", "prompt A"));
        for request in [0, 2] {
            h.emit("output_delta",json!({"turn":turn("A"),"request_index":request,"channel":"text","delta":format!("body A{request}")})).await;
            h.emit("tool_started",json!({"turn":turn("A"),"request_index":request,"tool_call_id":format!("tool-A{request}"),"tool_name":"bash"})).await;
        }
        h.until(|app| {
            app.transcript_blocks()
                .iter()
                .filter(|block| matches!(block, Block::Tool(_)))
                .count()
                == 2
        })
        .await;
        let directory:crate::protocol::CompletionDirectory=serde_json::from_value(json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"failed","availability":"unavailable","unavailable_reason":"expired"}]})).unwrap();
        let session = h.app.session.as_mut().unwrap();
        session.catalog = directory.clone();
        session.recovery.discover(&directory);
        let generation = session.recovery.generation;
        h.op(SessionOp::Recovered(
            crate::app::recovery::RecoveryOutcome {
                seq: 1,
                turn: crate::protocol::TurnRef::new("s1", "A"),
                generation,
                result: Ok(if partial {
                    crate::app::tasks::TurnRead::Partial(Box::new(
                        crate::app::tasks::ReadPartial::WorkspaceLimit,
                    ))
                } else {
                    crate::app::tasks::TurnRead::Unavailable(crate::app::tasks::UnavailableRead {
                        reason: crate::protocol::UnavailableReason::Expired,
                        persistence: Some(crate::protocol::TurnPersistence::Failed),
                        persistence_uncertain: false,
                    })
                }),
            },
        ));
        h.fake.script_held("turn.wait", waited("B", completed()));
        h.running_state("B", entry("input-B", "steer", "prompt B"));
        h.emit(
            "output_delta",
            json!({"turn":turn("B"),"request_index":0,"channel":"text","delta":"body B"}),
        )
        .await;
        h.until(|app| {
            app.transcript_blocks()
                .iter()
                .any(|block| matches!(block,Block::Assistant{text,..} if text=="body B"))
        })
        .await;
        h.app.scroll = Scroll::Anchor {
            block: 7,
            offset: 1,
        };
        h.emit(
            "input_delivered",
            json!({"turn":turn("A"),"items":[
                {"id":"steer-1","request_index":1,"text":"same"},
                {"id":"steer-3","request_index":3,"text":"same"},
                {"id":"steer-2","request_index":1,"text":"same"},
                {"id":"steer-1","request_index":1,"text":"not a new input"}
            ]}),
        )
        .await;
        h.settle().await;
        let shape: Vec<String> = h
            .blocks()
            .iter()
            .map(|block| match block {
                Block::User {
                    kind: UserKind::Steering,
                    ..
                } => "steer".into(),
                Block::User { text, .. } => text.clone(),
                Block::Assistant { text, .. } => text.clone(),
                Block::Tool(call) => call.key.tool_call_id.clone(),
                Block::TurnEnd { loop_id, .. } => format!("end {loop_id}"),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "prompt A", "body A0", "tool-A0", "steer", "steer", "body A2", "tool-A2", "steer",
                "end A", "prompt B", "body B"
            ],
            "partial={partial}: the candidate boundary, not text or arrival position, owns each steer"
        );
        assert_eq!(
            h.app.session.as_ref().unwrap().live.as_ref().unwrap().start,
            9
        );
        assert_eq!(
            h.app.scroll,
            Scroll::Anchor {
                block: 10,
                offset: 1
            }
        );
        #[cfg(feature = "e2e-observe")]
        {
            h.app.observe_checkpoint(false);
            let rows: Vec<Value> = std::fs::read_to_string(trace.path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let view = rows
                .iter()
                .rev()
                .find(|row| row["event"] == "canonical_view")
                .unwrap();
            let steers: Vec<_> = view["inventory"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["user_kind"] == "steering")
                .map(|row| {
                    (
                        row["input_id"].as_str().unwrap(),
                        row["request_index"].as_u64(),
                    )
                })
                .collect();
            assert_eq!(
                steers,
                [
                    ("steer-1", Some(1)),
                    ("steer-2", Some(1)),
                    ("steer-3", Some(3))
                ]
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn delivered_input_for_a_canonical_closed_turn_cannot_resurrect_a_steer_before_its_successor()
{
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "turn.result",
        turn_result(
            "A",
            vec![user(0, "A", "prompt A"), assistant(1, "A", "answer A")],
        ),
    );
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.session.as_ref().unwrap().is_closed("A"))
        .await;
    h.fake.script_held("turn.wait", waited("B", completed()));
    h.running_state("B", entry("input-B", "steer", "prompt B"));
    let before = format!("{:?}", h.blocks());
    h.emit(
        "input_delivered",
        json!({"turn":turn("A"),"items":[{"id":"late-closed","request_index":1,"text":"same"}]}),
    )
    .await;
    h.settle().await;
    assert_eq!(format!("{:?}", h.blocks()), before);
    assert!(h.app.session.as_ref().unwrap().is_closed("A"));
}

#[tokio::test(start_paused = true)]
async fn an_early_candidate_steer_stays_before_its_later_assistant_and_tool_blocks() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.emit(
        "input_delivered",
        json!({"turn":turn("A"),"items":[{"id":"future-steer","request_index":2,"text":"same"}]}),
    )
    .await;
    h.settle().await;
    for request in [0, 2, 1] {
        h.emit("tool_started",json!({"turn":turn("A"),"request_index":request,"tool_call_id":format!("tool-{request}"),"tool_name":"bash"})).await;
        h.emit("output_delta",json!({"turn":turn("A"),"request_index":request,"channel":"text","delta":format!("body-{request}")})).await;
    }
    h.settle().await;
    let shape: Vec<String> = h
        .blocks()
        .iter()
        .map(|block| match block {
            Block::User {
                kind: UserKind::Steering,
                ..
            } => "steer".into(),
            Block::User { text, .. } | Block::Assistant { text, .. } => text.clone(),
            Block::Tool(call) => call.key.tool_call_id.clone(),
            _ => "other".into(),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "prompt A", "body-0", "tool-0", "body-1", "tool-1", "steer", "body-2", "tool-2"
        ]
    );
}

#[cfg(feature = "e2e-observe")]
#[tokio::test(start_paused = true)]
async fn canonical_replacement_keeps_identified_equal_steers_and_reports_the_actual_verified_read_count()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let trace = tempfile::NamedTempFile::new().unwrap();
    h.app.start_observer(trace.reopen().unwrap());
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,"channel":"text","delta":"body A0"}),
    )
    .await;
    h.emit(
        "tool_started",
        json!({"turn":turn("A"),"request_index":0,"tool_call_id":"call-A","tool_name":"bash"}),
    )
    .await;
    h.emit("input_delivered",json!({"turn":turn("A"),"items":[{"id":"same-1","request_index":1,"text":"same"},{"id":"same-2","request_index":1,"text":"same"}]})).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":1,"channel":"text","delta":"body A1"}),
    )
    .await;
    h.settle().await;
    let mut result = turn_result(
        "A",
        vec![
            user(0, "A", "prompt A"),
            chunk(
                1,
                json!({"type":"assistant","data":{"loop_id":"A","request_index":0,"content":[{"type":"text","data":"body A0"},{"type":"tool_call","data":{"tool_call_id":"call-A","name":"bash"}}]}}),
            ),
            chunk(
                2,
                json!({"type":"tool_result","data":{"loop_id":"A","request_index":0,"call_id":"call-A","outcome":"success","output":{"content":"result"}}}),
            ),
            chunk(
                3,
                json!({"type":"user","data":{"loop_id":"A","kind":"steering","input":{"text":"same"}}}),
            ),
            chunk(
                4,
                json!({"type":"user","data":{"loop_id":"A","kind":"steering","input":{"text":"same"}}}),
            ),
            chunk(
                5,
                json!({"type":"assistant","data":{"loop_id":"A","request_index":1,"content":[{"type":"text","data":"body A1"}]}}),
            ),
        ],
    );
    result["outcome"] = completed();
    h.fake.script("turn.result", result);
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","input_id":"input-A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored"}]}})).await;
    h.until(|app| app.session.as_ref().unwrap().is_closed("A"))
        .await;
    h.app.observe_checkpoint(false);
    let rows: Vec<Value> = std::fs::read_to_string(trace.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let installed = rows
        .iter()
        .find(|row| row["event"] == "canonical_install")
        .unwrap();
    assert_eq!(installed["read_item_count"], 6);
    assert_eq!(
        installed["parts"].as_array().unwrap().len(),
        3,
        "the read count is not inferred from visible parts"
    );
    let view = rows
        .iter()
        .rev()
        .find(|row| row["event"] == "canonical_view")
        .unwrap();
    let steers: Vec<_> = view["inventory"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["user_kind"] == "steering")
        .map(|row| (row["input_id"].as_str(), row["request_index"].as_u64()))
        .collect();
    assert_eq!(
        steers,
        [(Some("same-1"), Some(1)), (Some("same-2"), Some(1))],
        "canonical repair must retain the original identified source rows at their proven boundaries"
    );
    assert_eq!(view["delivered_inputs"].as_array().unwrap().len(), 2);
    let first_delivery = rows
        .iter()
        .find(|row| {
            row["event"] == "canonical_view"
                && row["delivered_inputs"]
                    .as_array()
                    .is_some_and(|inputs| inputs.len() == 2)
        })
        .unwrap();
    assert!(
        first_delivery["event_seq"].as_u64().unwrap() < installed["event_seq"].as_u64().unwrap(),
        "the source delivery was actually observed before canonical repair"
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_wait_fallback_reads_the_original_load_instead_of_unbound_history() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error(
        "turn.wait",
        -32013,
        "wait result is outside the retained window",
        "turn_not_found",
    );
    let mut result = turn_result(
        "A",
        vec![user(0, "A", "prompt A"), assistant(1, "A", "answer A")],
    );
    result["outcome"] = completed();
    h.fake.script("turn.result", result);
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::TurnEnd{loop_id,..} if loop_id=="A"))
    })
    .await;
    let calls = h.calls("turn.result");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["loop_id"], "A");
    assert_eq!(
        calls[0]["load"],
        crate::testing::LOAD,
        "the resident wait fallback must retain its original load identity"
    );
}
