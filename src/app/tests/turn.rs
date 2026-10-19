use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::{Value, json};

use super::harness::*;
use crate::app::Action;
use crate::protocol::read::UserKind;
use crate::transcript::{Block, ToolState};

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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.settle().await;
    let screen = h.screen(80, 20);
    assert!(screen.contains("bash  $ cargo test"), "{screen}");
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
            tool_rounds,
            ..
        } => Some((outcome.is_completed(), *requests, *tool_rounds)),
        _ => None,
    });
    assert_eq!(end, Some((true, 2, 1)));
    assert_eq!(h.app.session.as_ref().unwrap().usage.output, 20);
    let waits = h.calls("turn.wait");
    assert_eq!(waits.len(), 1, "turn_started registers no second wait");
    assert_eq!(waits[0]["loop_id"], "l1");
}

#[tokio::test(start_paused = true)]
async fn a_wait_answer_for_another_loop_leaves_the_turn_running() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

#[tokio::test(start_paused = true)]
async fn a_steer_leaves_the_queue_only_when_the_agent_applies_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake
        .script("turn.steer", json!({"ok": true, "steer_index": 0}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;

    h.submit("also the tests");
    assert_eq!(h.queue(), ["also the tests"]);
    assert!(h.screen(60, 12).contains("↳ steer · also the tests"));

    h.emit(
        "steer_progress",
        json!({"turn": turn("l1"), "applied_count": 1}),
    )
    .await;
    h.settle().await;
    assert!(h.queue().is_empty());
    assert!(h.blocks().iter().any(|block| matches!(
        block,
        Block::User { text, kind: UserKind::Steering } if text == "also the tests"
    )));
}

#[tokio::test(start_paused = true)]
async fn queued_follow_ups_are_sent_one_per_turn_oldest_first() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "turn.send",
        ["l1", "l2", "l3"]
            .iter()
            .map(|id| json!({"turn": turn(id)}))
            .collect(),
    );
    h.fake.script_sequence_later(
        "turn.wait",
        vec![
            (waited("l1", completed()), Duration::from_secs(1)),
            (waited("l2", completed()), Duration::ZERO),
            (waited("l3", completed()), Duration::ZERO),
        ],
    );
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    for text in ["first", "second"] {
        h.app.composer.set_text(text);
        h.key(KeyCode::Enter, KeyModifiers::ALT);
    }
    assert_eq!(h.queue(), ["first", "second"]);
    assert!(h.screen(60, 12).contains("⏵ next · first"));

    h.settle().await;
    assert_eq!(h.sends(), ["start", "first", "second"]);
    assert!(h.queue().is_empty());
    assert_eq!(turn_ends(&h), ["l1", "l2", "l3"]);
}

#[tokio::test(start_paused = true)]
async fn a_busy_refusal_queues_the_text_and_sends_it_once_when_idle() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "turn.send",
        vec![refusal(-32003, "session_busy"), json!({"turn": turn("l2")})],
    );
    h.fake.script("turn.wait", waited("l2", completed()));
    h.fake.script_sequence(
        "session.state",
        vec![
            json!({"session_id": "s1", "status": "busy",
                   "compaction": {"operation_id": "auto-l1", "phase": "summarizing",
                                  "covered_item_count": 4, "retained_item_count": 1}}),
            json!({"session_id": "s1", "status": "idle"}),
        ],
    );
    h.submit("the important text");
    h.until(|app| app.status_label() == Some("Waiting for compaction"))
        .await;
    assert!(
        h.texts().is_empty(),
        "a refused prompt is not shown as sent"
    );
    assert_eq!(h.queue(), ["the important text"]);

    h.settle().await;
    assert_eq!(h.sends(), ["the important text", "the important text"]);
    assert_eq!(h.texts(), ["the important text"]);
    assert!(h.queue().is_empty());
    assert_eq!(h.app.status_label(), None);
}

#[tokio::test(start_paused = true)]
async fn a_refused_send_returns_its_text_without_losing_a_newer_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error(
        "turn.send",
        -32015,
        "history too large",
        "history_too_large",
    );
    h.app.composer.insert("ship it");
    h.app.composer.type_char('!');
    h.action(Action::Submit);
    h.settle().await;
    assert_eq!(h.app.composer.text(), "ship it!");
    assert!(h.texts().is_empty());
    assert_eq!(h.notices(), ["history too large"]);
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.text(), "ship it", "undo reaches the draft");
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.text(), "");
    h.ctrl(KeyCode::Char('y'));
    assert_eq!(h.app.composer.text(), "ship it");

    h.submit("refused prompt");
    h.app.composer.set_text("second thoughts");
    h.settle().await;
    assert_eq!(h.app.composer.text(), "second thoughtsrefused prompt");
    assert!(h.app.composer.undo());
    assert_eq!(h.app.composer.text(), "second thoughts");
}

#[tokio::test(start_paused = true)]
async fn a_running_loop_is_adopted_from_state_and_ends_through_its_wait() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.wait", waited("l9", completed()));
    h.state(json!({"session_id": "s1", "status": "running",
                   "active_loop": {"loop_id": "l9", "request_index": 1}}));
    h.settle().await;
    assert_eq!(h.calls("turn.wait")[0]["loop_id"], "l9");
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l9"]);
}

#[tokio::test(start_paused = true)]
async fn a_capacity_refusal_on_turn_wait_is_retried() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
            "1 file changed (+40 −7) ──",
        ),
        (
            vec![record("tool:a", false)],
            diff_with(1, 1),
            "1 file changed ──",
        ),
        (
            vec![
                record("tool:w", true),
                record("tool:e", true),
                record("tool:p", true),
            ],
            diff_with(2, 1),
            "1 file changed (+6 −3) ──",
        ),
    ];
    for (records, diff, expected) in cases {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake.script("turn.send", json!({"turn": turn("l1")}));
        h.fake.script("turn.wait", waited("l1", completed()));
        h.fake.script(
            "changes.list",
            json!({"records": records, "total": records.len(), "complete": true}),
        );
        h.fake.script("changes.diff", diff);
        h.submit("change it");
        h.settle().await;
        let screen = h.screen(100, 12);
        assert!(screen.contains(expected), "{expected}:\n{screen}");
    }
}
