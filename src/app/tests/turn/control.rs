use super::*;

#[tokio::test(start_paused = true)]
async fn queued_follow_ups_run_one_per_turn_as_the_agent_starts_them() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let first = entry("i2", "follow_up", "first");
    let second = entry("i3", "follow_up", "second");
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
    for (id, prompt) in [("l1", "start"), ("l2", "first"), ("l3", "second")] {
        let result = completed_result(id, vec![user(0, id, prompt)]);
        h.fake.script(&format!("turn.result@s1/{id}"), result);
    }
    let mut after_first = finished_snapshot(5, vec![completion(1, "l1", "completed")]);
    after_first["execution"] = running_snapshot(5, "l2", "first")["execution"].clone();
    after_first["queue"] = queue(vec![second.clone()], false);
    h.fake.script_held(
        "turn.wait@s1/l1",
        after_snapshot(waited("l1", completed()), after_first),
    );
    let mut after_second = finished_snapshot(
        6,
        vec![
            completion(1, "l1", "completed"),
            completion(2, "l2", "completed"),
        ],
    );
    after_second["execution"] = running_snapshot(6, "l3", "second")["execution"].clone();
    h.fake.script(
        "turn.wait@s1/l2",
        after_snapshot(waited("l2", completed()), after_second),
    );
    h.fake.script(
        "turn.wait@s1/l3",
        after_snapshot(
            waited("l3", completed()),
            finished_snapshot(
                7,
                vec![
                    completion(1, "l1", "completed"),
                    completion(2, "l2", "completed"),
                    completion(3, "l3", "completed"),
                ],
            ),
        ),
    );
    h.submit("start");
    h.until(|app| app.is_busy()).await;
    for text in ["first", "second"] {
        h.app.composer.set_text(text);
        h.key(KeyCode::Enter, KeyModifiers::ALT);
    }
    h.until(|app| app.resident.current().unwrap().queue.items.len() == 2)
        .await;
    assert!(h.screen(60, 12).contains("↳ follow-up 1  first"));
    h.fake.release("turn.wait@s1/l1");
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
async fn a_push_into_a_paused_queue_shows_the_older_entry_the_agent_started_with() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.mirror(vec![entry("i1", "follow_up", "older")], true);
    h.fake.script(
        "input.push",
        after_snapshot(
            accepted("s1", "i2"),
            running_queue(
                h.next_revision(),
                "l2",
                "older",
                vec![entry("i2", "follow_up", "newer")],
            ),
        ),
    );
    h.fake.script_held("turn.wait", waited("l2", completed()));
    h.app.composer.set_text("newer");
    h.action(Action::QueueFollowUp);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.texts(), ["older"], "queue order wins over arrival");
    assert_eq!(h.queue(), ["newer"]);
    assert!(!h.session().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn a_wait_answer_refreshes_control_once_and_nothing_else() {
    let mut h = Harness::new();
    // The state names the loop that just ended, as a stale Agent could.
    h.fake.script(
        "session.state@s1",
        state_response(running_snapshot(2, "l9", "running")),
    );
    h.fake.script_held(
        "turn.wait",
        json!({"turn": turn("l9"), "outcome": completed()}),
    );
    h.open().await;
    assert!(h.is_busy());
    let states = h.calls("session.state").len();
    h.fake.script(
        "session.state@s1",
        state_response(finished_snapshot(3, vec![completion(1, "l9", "completed")])),
    );
    let result = completed_result("l9", vec![user(0, "l9", "running")]);
    h.fake.script("turn.result@s1/l9", result);
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
async fn a_snapshot_replaces_the_mirrored_queue_and_its_paused_flag() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let items = vec![
        entry("i1", "steer", "now"),
        entry("i2", "follow_up", "later"),
    ];
    h.control_event(json!({"queue": queue(items, true)})).await;
    h.settle().await;
    let session = h.session();
    assert!(session.queue.paused);
    assert_eq!(h.queue(), ["now", "later"]);
    h.control_event(json!({"queue": queue(vec![], false)}))
        .await;
    h.settle().await;
    assert!(h.queue().is_empty());
    assert!(!h.session().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn opening_a_session_takes_the_queued_entries_the_agent_reports() {
    let mut h = Harness::new();
    let mut state = crate::testing::snapshot("s1", crate::testing::LOAD, 2);
    state["queue"] = queue(vec![entry("i1", "follow_up", "waiting")], true);
    h.fake.script("session.state@s1", state_response(state));
    h.open().await;
    assert_eq!(h.queue(), ["waiting"]);
    assert!(h.session().queue.paused);
}

#[tokio::test(start_paused = true)]
async fn a_paused_queue_shows_how_to_go_on_and_enter_without_a_draft_resumes_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    let mut paused = finished_snapshot(3, vec![completion(1, "l1", "cancelled")]);
    paused["queue"] = queue(vec![entry("i2", "follow_up", "after the stop")], true);
    h.fake.script_sequence(
        "turn.wait",
        vec![after_snapshot(waited("l1", cancelled()), paused)],
    );
    h.fake.script(
        "turn.result",
        turn_result("l1", vec![user(0, "l1", "start")]),
    );
    h.submit("start");
    h.settle().await;
    assert!(!h.is_busy());
    assert!(h.session().queue.paused);
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
        after_snapshot(
            json!({"start_error": null}),
            running_snapshot(h.next_revision(), "l2", "after the stop"),
        ),
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
    assert!(!h.session().queue.paused);
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
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
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
        after_snapshot(
            json!({"texts": ["waiting steer", "follow1", "follow2"]}),
            running_snapshot(h.next_revision(), "l1", "start"),
        ),
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
            starting_reply("l1", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(3, "l1", "start", vec![waiting]),
            ),
            refusal(-32030, "input_queue_full").into(),
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

#[tokio::test(start_paused = true)]
async fn a_wait_answer_for_another_loop_leaves_the_turn_running() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script("turn.wait", waited("l0", completed()));
    h.submit("go");
    h.settle().await;
    assert!(h.is_busy());
    assert!(turn_ends(&h).is_empty());
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
    h.fake.script(
        "turn.wait",
        completed_reply("l9", 3, vec![user(0, "l9", "running")]),
    );
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
        "session.state@s1",
        state_response(running_snapshot(2, "l9", "running")),
    );
    h.fake.script_held(
        "turn.wait",
        completed_reply("l9", 3, vec![user(0, "l9", "running")]),
    );
    h.open().await;
    assert!(h.is_busy());
    assert_eq!(h.calls("turn.wait").len(), 1);

    h.fake.release("turn.wait");
    h.settle().await;
    assert!(!h.is_busy());
    assert_eq!(turn_ends(&h), ["l9"]);
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
               "accepted": true, "id": "inp_1", "start_error": null}),
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
        after_snapshot(
            json!({"session_id": "s1", "load": crate::testing::LOAD, "accepted": true,
               "id": "inp_1",
               "start_error": {"input_id": "inp_0", "kind": "provider_unavailable",
                               "message": "no provider is configured"}}),
            h.control(json!({"queue":queue(vec![entry("inp_1","steer","later")],false)})),
        ),
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
async fn receipts_wait_delivery_and_context_cannot_overwrite_snapshot_control() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("session.state@s1", json!({"session_id":"s1"}));
    h.fake.script_held("turn.wait", waited("A", completed()));
    let mut authoritative = h.control(json!({
        "execution":{"type":"running","loop_id":"A","input":entry("iA","steer","prompt A"),"request_index":0,"phase":"running_model"},
        "queue":queue(vec![entry("kept","steer","kept")],true),
        "compaction":{"operation_id":"current","phase":"summarizing","covered_item_count":1,"retained_item_count":0}
    }));
    h.app
        .apply_snapshot(serde_json::from_value(authoritative.clone()).unwrap());
    h.fake.script_held("input.push", accepted("s1", "i1"));
    h.app.submit_text("accepted".into(), false);
    let operation = h.app.local_inputs.back().unwrap().id;
    h.app.on_msg(crate::app::Msg::LocalPushed(
        operation,
        Ok(serde_json::from_value(accepted("s1", "new")).unwrap()),
    ));
    h.op(SessionOp::Waited(Ok(serde_json::from_value(waited(
        "A",
        completed(),
    ))
    .unwrap())));
    h.op(SessionOp::Operated(Ok(
        crate::app::operation::Outcome::Compacted(
            serde_json::from_value(json!({"operation_id":"old","status":"noop"})).unwrap(),
        ),
    )));
    h.context(json!({"session_id":"s1"}));
    let session = h.session_mut();
    session.apply_body_event(serde_json::from_value(json!({"type":"input_delivered","data":{
        "meta":{"session_id":"s1","load":crate::testing::LOAD,"dropped_before":0},"turn":turn("A"),"items":[{"id":"kept","text":"kept","request_index":1}]
    }})).unwrap(),256);
    assert_eq!(
        session
            .execution
            .as_ref()
            .and_then(|execution| execution.loop_id()),
        Some("A")
    );
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
