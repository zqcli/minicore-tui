use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::{Value, json};

use super::harness::*;
use crate::app::{Action, NoticeLevel, Scroll, SessionOp};
use crate::protocol::read::UserKind;
use crate::protocol::stamp::{now_millis, unix_millis};
use crate::transcript::{Block, ChangeSummary, ToolState};

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
async fn a_failed_turn_shows_the_providers_status_the_hint_and_its_message() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.fake.script(
        "turn.send",
        json!({"turn": turn("l1"), "accepted_at": "2026-10-20T21:38:00Z"}),
    );
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
async fn a_sent_prompt_takes_the_agents_acceptance_time_over_the_clock() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "turn.send",
        json!({"turn": turn("l1"), "accepted_at": "2026-10-20T21:38:00Z"}),
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("with a time");
    assert!(
        h.sent_at("with a time").is_some(),
        "the clock stamps it at once"
    );
    h.until(|app| app.is_busy()).await;
    h.settle().await;
    assert_eq!(
        h.sent_at("with a time"),
        unix_millis("2026-10-20T21:38:00Z")
    );
    assert!(h.screen(60, 12).contains("21:38 · 10/20/2026"));
}

#[tokio::test(start_paused = true)]
async fn a_prompt_without_an_acceptance_time_keeps_the_send_clock() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    let before = now_millis();
    h.submit("no ack time");
    h.until(|app| app.is_busy()).await;
    let at = h.sent_at("no ack time").expect("stamped at send");
    assert!((before..=now_millis()).contains(&at));
}

#[tokio::test(start_paused = true)]
async fn a_steering_message_shows_the_time_the_agent_accepted_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script(
        "turn.steer",
        json!({"ok": true, "accepted_at": "2026-10-20T21:39:30Z"}),
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    h.submit("also the tests");
    h.settle().await;
    h.emit(
        "steer_progress",
        json!({"turn": turn("l1"), "applied_count": 1}),
    )
    .await;
    h.settle().await;
    assert_eq!(
        h.sent_at("also the tests"),
        unix_millis("2026-10-20T21:39:30Z")
    );
    assert!(h.screen(60, 14).contains("21:39 · 10/20/2026 · ↗ steering"));
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
    assert!(h.screen(60, 12).contains("↗ steer  also the tests"));

    h.emit(
        "steer_progress",
        json!({"turn": turn("l1"), "applied_count": 1}),
    )
    .await;
    h.settle().await;
    assert!(h.queue().is_empty());
    assert!(h.blocks().iter().any(|block| matches!(
        block,
        Block::User { text, kind: UserKind::Steering, .. } if text == "also the tests"
    )));
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.fake
        .script("turn.steer", json!({"ok": true, "steer_index": 0}));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.emit("turn_started", json!({"turn": turn("l1")})).await;
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
                "↗ steer  more" => h.submit("more"),
                "↳ follow-up 1  later" => {
                    h.app.composer.set_text("later");
                    h.key(KeyCode::Enter, KeyModifiers::ALT);
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
    assert!(h.screen(60, 12).contains("↳ follow-up 1  first"));

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
    h.fake
        .script_error("turn.send", -32603, "provider unavailable", "internal");
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
async fn opening_a_session_mid_turn_adopts_the_running_loop() {
    let mut h = Harness::new();
    h.fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "running", "active_loop": {"loop_id": "l9"}}),
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
        h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake
        .script_error("turn.wait", -32007, "turn not found", "turn_not_found");
    h.fake.script(
        "turn.result",
        json!({"items": [], "next_cursor": null, "outcome": completed(), "requests": 3}),
    );
    h.submit("go");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l1"]);
}

#[tokio::test(start_paused = true)]
async fn a_turn_whose_end_cannot_be_read_still_ends() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake
        .script_error("turn.wait", -32007, "turn not found", "turn_not_found");
    h.fake
        .script_error("turn.result", -32007, "turn not found", "turn_not_found");
    h.submit("go");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l1"]);
}

#[tokio::test(start_paused = true)]
async fn a_follow_up_sent_before_the_cancelled_turn_reconciles_stays_after_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "turn.send",
        ["l1", "l2"]
            .iter()
            .map(|id| json!({"turn": turn(id)}))
            .collect(),
    );
    h.fake.script_sequence_later(
        "turn.wait",
        vec![
            (waited("l1", cancelled()), Duration::from_secs(1)),
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
    h.app.composer.set_text("next");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    // The turn ends and the follow-up is sent while the stored items are still on their way.
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
        "turn.send",
        ["l1", "l2"]
            .iter()
            .map(|id| json!({"turn": turn(id)}))
            .collect(),
    );
    h.fake.script_sequence_later(
        "turn.wait",
        vec![
            (waited("l1", completed()), Duration::from_secs(1)),
            (waited("l2", completed()), Duration::from_secs(1)),
        ],
    );
    h.submit("one");
    assert_eq!(h.app.scroll, Scroll::Follow, "sending follows");
    h.until(|app| app.is_busy()).await;
    h.app.composer.set_text("two");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.app.scroll = anchor;
    // The turn ends and the queued follow-up starts the next one by itself.
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
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
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
    assert_eq!((usage(&h).input, usage(&h).output), (300, 30));
}
