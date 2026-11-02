use super::*;

fn diff_with(added: usize, removed: usize) -> Value {
    let lines: Vec<Value> = (0..removed)
        .map(|index| json!({"kind": "removed", "old_index": index, "text": "old", "line_complete": true}))
        .chain((0..added).map(
            |index| json!({"kind": "added", "new_index": index, "text": "new", "line_complete": true}),
        ))
        .collect();
    json!({"path": "app.py", "availability": "available",
           "complete": true, "binary": false, "stale": false, "truncated": false,
           "next_cursor": null,
           "hunks": [{"old_start": 0, "old_count": removed, "new_start": 0,
                      "new_count": added, "lines": lines}]})
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
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
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
async fn a_literal_truncated_line_in_live_output_is_visible_and_searchable() {
    let mut h = running_card().await;
    let output = "before\n[truncated]\nafter\n";
    h.emit(
        "tool_process",
        json!({"data":{
            "tool_ref":{"session_id":"s1","loop_id":"l1","request_index":0,"tool_call_id":"c1"},
            "chunk":{"stream":"stdout","encoding":"utf8","data":output,
                     "base_offset":0,"next_offset":output.len(),"expired":false}
        }}),
    )
    .await;
    h.settle().await;
    let call = h.session().transcript.tool_calls().next().unwrap();
    assert_eq!(call.output_content(), ["before", "[truncated]", "after"]);
    assert!(!call.truncated);
    let found = crate::app::find::find_matches(h.blocks(), "[truncated]", false);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].field, crate::app::find::Field::Output);
    h.ctrl(KeyCode::Char('o'));
    let screen = h.screen(80, 24);
    assert!(screen.contains("[truncated]"), "{screen}");

    use crate::app::tests::phone::{connect_phone, enable_remote, messages};
    enable_remote(&mut h);
    let (mut phone, _closing) = connect_phone(&mut h, 1, "phone").await;
    h.app.sync_remote();
    let frames = messages(&mut phone);
    let snapshot = frames
        .iter()
        .find(|frame| frame["t"] == "snapshot")
        .unwrap();
    let tool = snapshot["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "tool")
        .unwrap();
    assert_eq!(
        tool["output"],
        json!(["before", "[truncated]", "after", ""])
    );
    assert_eq!(tool["truncated"], false);
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
                starting_reply("l1", 2),
                after_snapshot(
                    accepted("s1", "i2"),
                    running_queue(3, "l1", "start", vec![first.clone()]),
                ),
                after_snapshot(
                    accepted("s1", "i3"),
                    running_queue(4, "l1", "start", vec![first.clone(), second.clone()]),
                ),
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
        let mut remaining = vec![first, second];
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
            remaining.retain(|queued| !batch.iter().any(|text| queued["id"] == id(text)));
            h.snapshot_event(running_queue(
                h.next_revision(),
                "l1",
                "start",
                remaining.clone(),
            ))
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
    h.fake.script_sequence(
        "input.push",
        vec![
            starting_reply("A", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(3, "A", "start", vec![next]),
            ),
        ],
    );
    let mut successor = finished_snapshot(4, vec![completion(1, "A", "completed")]);
    successor["execution"] = crate::testing::snapshot_running(
        "s1",
        crate::testing::LOAD,
        4,
        "B",
        entry("i2", "steer", "next up"),
    )["execution"]
        .clone();
    h.fake.script_held(
        "turn.wait",
        after_snapshot(waited("A", completed()), successor),
    );
    let result = completed_result("A", vec![user(0, "A", "start")]);
    h.fake.script("turn.result@s1/A", result);
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
async fn a_late_wait_finishes_only_its_turn_while_the_successor_runs_a_tool() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("session.state@s1", json!({"session_id":"s1"}));
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.running_state("B", entry("input-B", "follow_up", "prompt B"));
    h.emit(
        "tool_started",
        json!({"turn":turn("B"),"request_index":0,
        "tool_call_id":"tool-B","tool_name":"bash"}),
    )
    .await;
    let rows = crate::transcript::EXPANDED_ROWS + 20;
    let output = (0..rows)
        .map(|row| format!("line {row}\n"))
        .collect::<String>();
    h.emit(
        "tool_process",
        json!({"turn":turn("B"),"data":{
        "tool_ref":{"session_id":"s1","loop_id":"B","request_index":0,"tool_call_id":"tool-B"},
        "chunk":{"stream":"stdout","encoding":"utf8","data":output,
            "base_offset":0,"next_offset":output.len(),"expired":false}}}),
    )
    .await;
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .transcript
            .tool_calls()
            .any(|call| call.output_content().len() == rows)
    })
    .await;
    let before = h
        .session()
        .transcript
        .tool_calls()
        .next()
        .unwrap()
        .output
        .clone();

    h.op(SessionOp::Waited(Ok(serde_json::from_value(waited(
        "A",
        completed(),
    ))
    .unwrap())));

    let session = h.session();
    let call = session.transcript.tool_calls().next().unwrap();
    assert_eq!(
        call.output, before,
        "finishing A cannot trim B's running output"
    );
    assert_eq!(call.state, ToolState::Running);
    assert_eq!(session.observed_loop(), Some("B"));
    assert!(h.blocks().iter().any(|block| matches!(block,
        Block::TurnEnd {loop_id, tools:0, ..} if loop_id == "A")));
}

#[tokio::test(start_paused = true)]
async fn late_body_updates_its_region_without_changing_the_successors_working_line() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("session.state@s1", json!({"session_id":"s1"}));
    h.fake.script_held("turn.wait", waited("A", completed()));
    h.running_state("A", entry("input-A", "steer", "prompt A"));
    h.running_state("B", entry("input-B", "follow_up", "prompt B"));
    h.emit(
        "tool_started",
        json!({"turn":turn("B"),"request_index":0,
        "tool_call_id":"tool-B","tool_name":"bash"}),
    )
    .await;
    h.emit("tool_presentation", json!({"turn":turn("B"),"request_index":0,
        "tool_call_id":"tool-B","tool_name":"bash","display":{"detail":"B command","truncated":false}})).await;
    h.emit(
        "output_delta",
        json!({"turn":turn("A"),"request_index":0,
        "channel":"text","delta":"late A"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant {text,..} if text == "late A"))
    })
    .await;
    assert_eq!(
        h.session().live.as_ref().unwrap().phase,
        crate::app::Phase::Thinking
    );
    h.emit(
        "tool_started",
        json!({"turn":turn("A"),"request_index":0,
        "tool_call_id":"tool-A","tool_name":"write"}),
    )
    .await;
    h.emit("tool_presentation", json!({"turn":turn("A"),"request_index":0,
        "tool_call_id":"tool-A","tool_name":"write","display":{"detail":"A file","truncated":false}})).await;
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .transcript
            .tool_calls()
            .any(|call| call.key.loop_id == "A" && call.detail == "A file")
    })
    .await;
    assert_eq!(
        h.session().live.as_ref().unwrap().running_tool,
        Some(("bash".into(), "B command".into()))
    );
}

#[tokio::test(start_paused = true)]
async fn a_turn_streams_tool_facts_and_ends_on_its_wait() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    let mut result = completed_result(
        "l1",
        vec![
            user(0, "l1", "fix the greeting"),
            chunk(
                1,
                json!({"type":"assistant","data":{"loop_id":"l1","request_index":0,"content":[
            {"type":"text","data":"Fixed."},
            {"type":"tool_call","data":{"tool_call_id":"c1","name":"bash"}}]}}),
            ),
            chunk(
                2,
                json!({"type":"tool_result","data":{"loop_id":"l1","request_index":0,"call_id":"c1","outcome":"success","output":{"content":""}}}),
            ),
        ],
    );
    result["usage"] = json!({"input_tokens":100,"output_tokens":20});
    result["requests"] = json!(2);
    result["tool_rounds"] = json!(1);
    h.fake.script("turn.result@s1/l1", result);
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            json!({"turn": turn("l1"), "outcome": completed(),
               "usage": {"input_tokens": 100, "output_tokens": 20},
               "requests": 2, "tool_rounds": 1}),
            finished_snapshot(3, vec![completion(1, "l1", "completed")]),
        ),
    );
    h.submit("fix the greeting");
    h.until(|app| app.is_busy()).await;

    h.snapshot_event(running_snapshot(2, "l1", "fix the greeting"))
        .await;
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
    assert_eq!(h.session().usage.output, 20);
    let waits = h.calls("turn.wait");
    assert_eq!(
        waits.len(),
        1,
        "a repeated snapshot registers no second wait"
    );
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
    assert_eq!(h.session().observed_loop(), Some("A"));
    assert!(
        h.is_busy(),
        "a supplied tool input does not complete its model turn"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_turn_shows_the_providers_status_the_hint_and_its_message() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    let outcome = json!({"type": "failed", "kind": "model", "model_error": {
        "kind": "rate_limited", "delivery": "not_started", "retry_after_millis": 12000,
        "provider": {"status": 429, "code": "rate_limit_exceeded", "type": "requests",
                     "message": "Rate limit reached for gpt-6 in organization org-1"}}});
    let mut result = turn_result("l1", vec![user(0, "l1", "go")]);
    result["outcome"] = outcome.clone();
    h.fake.script("turn.result@s1/l1", result);
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            waited("l1", outcome),
            finished_snapshot(3, vec![completion(1, "l1", "failed")]),
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
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
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
    let revisions = |h: &Harness| h.session().transcript.revisions().collect::<Vec<_>>();
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
async fn a_prompt_shows_the_time_the_agent_accepted_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut starting = running_snapshot(h.next_revision(), "l1", "$text");
    starting["execution"]["input"]["accepted_at"] = json!("2026-10-20T21:38:00Z");
    let mut receipt = accepted("s1", "i1");
    receipt["accepted_at"] = json!("2026-10-20T21:38:00Z");
    h.fake.script(
        "input.push",
        after_snapshot(receipt, starting).with_request_text(),
    );
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
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
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
            starting_reply("l1", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(
                    3,
                    "l1",
                    "start",
                    vec![entry("i2", "steer", "also the tests")],
                ),
            ),
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
    h.snapshot_event(running_snapshot(h.next_revision(), "l1", "start"))
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
                    h.fake.script(
                        "input.push",
                        after_snapshot(
                            accepted("s1", "i2"),
                            running_queue(
                                h.next_revision(),
                                "l1",
                                "go",
                                vec![entry("i2", "steer", "more")],
                            ),
                        ),
                    );
                    h.submit("more");
                    h.settle().await;
                }
                "↳ follow-up 1  later" => {
                    h.fake.script(
                        "input.push",
                        after_snapshot(
                            accepted("s1", "i2"),
                            running_queue(
                                h.next_revision(),
                                "l1",
                                "go",
                                vec![entry("i2", "follow_up", "later")],
                            ),
                        ),
                    );
                    h.app.composer.set_text("later");
                    h.key(KeyCode::Enter, KeyModifiers::ALT);
                    h.settle().await;
                }
                "boom" => h.app.notice(NoticeLevel::Error, "boom"),
                _ => {
                    let mut waiting = running_snapshot(h.next_revision(), "l1", "go");
                    waiting["pending_interaction"] = json!({"loop_id":"l1","request_index":0,
                        "interaction_id":"i1","tool_call_id":"c1","tool_name":"bash",
                        "kind":{"type":"approval","data":{"prompt":"Allow bash?"}}});
                    h.fake
                        .script("session.state@s1", state_response(waiting.clone()));
                    h.snapshot_event(waiting).await;
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
async fn the_turn_end_line_counts_changed_files_and_lines() {
    let record = |call_id: &str, details: bool| {
        json!({"change_ref": tool_change_ref(call_id), "path": "app.py", "kind": "modified",
               "details_available": details})
    };
    let cases = [
        (
            vec![record("a", true)],
            diff_with(40, 7),
            "1 file changed (+40 −7)",
        ),
        (vec![record("a", false)], diff_with(1, 1), "1 file changed"),
        (
            vec![record("w", true), record("e", true), record("p", true)],
            diff_with(2, 1),
            "1 file changed (+6 −3)",
        ),
    ];
    for (records, diff, expected) in cases {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake
            .script("input.push", starting_reply("l1", h.next_revision()));
        h.fake.script(
            "turn.wait",
            completed_reply("l1", 3, vec![user(0, "l1", "change it")]),
        );
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
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script(
        "turn.wait",
        completed_reply("l1", 3, vec![user(0, "l1", "go")]),
    );
    h.submit("go");
    h.settle().await;
    let revisions = |h: &Harness| h.session().transcript.revisions().collect::<Vec<_>>();
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
async fn the_turn_start_and_end_leave_an_anchored_view_where_it_is() {
    let anchor = Scroll::Anchor {
        block: 2,
        offset: 1,
    };
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "input.push",
        vec![
            starting_reply("l1", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(3, "l1", "one", vec![entry("i2", "follow_up", "two")]),
            ),
        ],
    );
    let mut successor = finished_snapshot(4, vec![completion(1, "l1", "completed")]);
    successor["execution"] = running_snapshot(4, "l2", "two")["execution"].clone();
    h.fake.script_held(
        "turn.wait@s1/l1",
        after_snapshot(waited("l1", completed()), successor),
    );
    h.fake.script_held(
        "turn.wait@s1/l2",
        after_snapshot(
            waited("l2", completed()),
            finished_snapshot(
                5,
                vec![
                    completion(1, "l1", "completed"),
                    completion(2, "l2", "completed"),
                ],
            ),
        ),
    );
    for (id, text) in [("l1", "one"), ("l2", "two")] {
        h.fake.script(
            &format!("turn.result@s1/{id}"),
            completed_result(id, vec![user(0, id, text)]),
        );
    }
    h.submit("one");
    assert_eq!(h.app.scroll, Scroll::Follow, "sending follows");
    h.until(|app| app.is_busy()).await;
    h.app.composer.set_text("two");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.app.scroll = anchor;
    h.until(|app| app.resident.current().unwrap().queue.items.len() == 1)
        .await;
    h.fake.release("turn.wait@s1/l1");
    // The Agent starts the queued follow-up by itself.
    h.until(|app| app.transcript_blocks().len() >= 3).await;
    h.fake.release("turn.wait@s1/l2");
    assert_eq!(h.sends(), ["one", "two"]);
    h.settle().await;
    assert_eq!(h.app.scroll, anchor);
    assert!(!h.is_busy());

    h.fake
        .script_held("turn.wait@s1/l9", waited("l9", completed()));
    h.running_state("l9", entry("i9", "steer", "adopted"));
    assert_eq!(
        h.app.scroll, anchor,
        "adopting a running turn does not follow"
    );
}

#[tokio::test(start_paused = true)]
async fn the_footer_totals_move_with_each_request_and_the_wait_settles_them() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held(
        "turn.wait",
        after_snapshot(
            json!({"turn": turn("l1"), "outcome": completed(),
               "usage": {"input_tokens": 300, "output_tokens": 30}, "requests": 3}),
            finished_snapshot(3, vec![completion(1, "l1", "completed")]),
        ),
    );
    let mut result = completed_result("l1", vec![user(0, "l1", "go")]);
    result["usage"] = json!({"input_tokens":300,"output_tokens":30});
    result["requests"] = json!(3);
    h.fake.script("turn.result@s1/l1", result);
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    let usage = |h: &Harness| h.session().usage.clone();

    for _ in 0..2 {
        h.emit(
            "request_usage",
            json!({"turn": turn("l1"), "usage": {"input_tokens": 100, "output_tokens": 10}}),
        )
        .await;
    }
    h.until(|app| app.resident.current().unwrap().usage.input == 200)
        .await;
    assert_eq!(usage(&h).output, 20);

    // The third request's event was dropped; the wait has the whole turn.
    h.fake.release("turn.wait");
    h.until(|app| !app.is_busy()).await;
    h.until(|app| app.resident.current().unwrap().usage.input == 300)
        .await;
    assert_eq!((usage(&h).input, usage(&h).output), (300, 30));
}

#[tokio::test(start_paused = true)]
async fn delayed_predecessor_waits_preserve_successor_usage_in_both_reply_orders() {
    for order in [["A", "B"], ["B", "A"]] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake.script_held("turn.wait", waited("A", completed()));
        h.fake
            .script_held("session.state@s1", json!({"session_id":"s1"}));
        h.session_mut().usage = crate::protocol::UsageTotals {
            input: 1000,
            output: 100,
            cached: 50,
        };
        h.running_state("A", entry("iA", "steer", "A"));
        h.emit("request_usage",json!({"turn":turn("A"),"usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":5}})).await;
        h.until(|app| app.resident.current().unwrap().usage.input == 1100)
            .await;
        h.running_state("B", entry("iB", "steer", "B"));
        h.emit("request_usage",json!({"turn":turn("B"),"usage":{"input_tokens":200,"output_tokens":20,"cache_read_tokens":7}})).await;
        h.until(|app| app.resident.current().unwrap().usage.input == 1300)
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
            let usage = &h.session().usage;
            assert_eq!(
                (usage.input, usage.output, usage.cached),
                (1000 + a + b, 100 + (a + b) / 10, 50 + ac + bc),
                "reply order {order:?}, after {id}"
            );
            assert_eq!(
                h.session().observed_loop(),
                Some("B"),
                "wait metadata cannot change control"
            );
        }
        let before = h.session().usage.clone();
        h.op(SessionOp::Waited(Ok(serde_json::from_value(json!({"turn":turn("A"),"outcome":completed(),"usage":{"input_tokens":150,"output_tokens":15,"cache_read_tokens":8}})).unwrap())));
        assert_eq!(
            h.session().usage,
            before,
            "a duplicate terminal receipt is not counted twice"
        );
    }
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
        let session = h.session_mut();
        session.catalog = directory.clone();
        session.recovery.discover(&directory);

        h.op(SessionOp::Recovered(
            crate::app::recovery::RecoveryOutcome {
                seq: 1,
                turn: crate::protocol::TurnRef::new("s1", "A"),

                result: Ok(if partial {
                    crate::app::read::TurnRead::Partial(Box::new(
                        crate::app::read::ReadPartial::WorkspaceLimit,
                    ))
                } else {
                    crate::app::read::TurnRead::Unavailable(crate::app::read::UnavailableRead {
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
        assert_eq!(h.session().live.as_ref().unwrap().start, 9);
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
    h.until(|app| app.resident.current().unwrap().is_closed("A"))
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
    assert!(h.session().is_closed("A"));
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

#[tokio::test(start_paused = true)]
async fn wait_and_result_facts_replace_only_their_turns_usage_in_either_order() {
    use crate::app::read::TurnDetails;
    use crate::protocol::read::TurnPersistence;

    for result_first in [false, true] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake.script_held("turn.wait", waited("A", completed()));
        h.fake
            .script_held("session.state@s1", json!({"session_id":"s1"}));
        h.running_state("A", entry("iA", "steer", "prompt A"));
        h.emit(
            "request_usage",
            json!({"turn":turn("A"),"usage":{"input_tokens":100,"output_tokens":10}}),
        )
        .await;
        h.until(|app| app.resident.current().unwrap().usage.input == 100)
            .await;
        h.running_state("B", entry("iB", "steer", "prompt B"));
        h.emit(
            "request_usage",
            json!({"turn":turn("B"),"usage":{"input_tokens":200,"output_tokens":20}}),
        )
        .await;
        h.until(|app| app.resident.current().unwrap().usage.input == 300)
            .await;

        let wait = serde_json::from_value::<crate::protocol::TurnWaitResult>(json!({
            "turn":turn("A"), "outcome":completed(), "requests":3, "tool_rounds":2,
            "usage":{"input_tokens":150,"output_tokens":15}
        }))
        .unwrap();
        let descriptor = serde_json::from_value(json!({
            "seq":1, "loop_id":"A", "input_id":"iA", "completion_kind":"reported",
            "outcome_kind":"completed", "persistence":"persisted", "availability":"stored"
        }))
        .unwrap();
        for read_result in [result_first, !result_first] {
            if read_result {
                h.session_mut()
                    .reconcile_completion(
                        &descriptor,
                        &TurnDetails::from(&wait),
                        &[],
                        None,
                        TurnPersistence::Persisted,
                        false,
                    )
                    .expect("the predecessor has a recoverable region");
            } else {
                h.op(SessionOp::Waited(Ok(wait.clone())));
            }
            let session = h.session();
            assert_eq!((session.usage.input, session.usage.output), (350, 35));
            assert_eq!(session.live.as_ref().unwrap().turn.loop_id, "B");
            assert_eq!(session.observed_loop(), Some("B"));
        }
        h.op(SessionOp::Waited(Ok(wait)));
        let terminal = h
            .blocks()
            .iter()
            .find(|block| matches!(block, Block::TurnEnd { loop_id, .. } if loop_id == "A"))
            .unwrap();
        assert!(
            matches!(terminal, Block::TurnEnd {
            duration: Some(_), requests: 3, tools: 0, usage,
            saving: Some(crate::transcript::Saving::Saved), ..
        } if usage.input_tokens == Some(150) && usage.output_tokens == Some(15)),
            "wait and result describe one terminal, and tool rounds are not tool calls: {terminal:?}"
        );
        assert_eq!(h.session().usage.input, 350);
    }
}
