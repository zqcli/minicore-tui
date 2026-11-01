use super::*;

#[tokio::test(start_paused = true)]
async fn answers_validate_prompt_kind_and_index_before_consuming_the_interaction() {
    for approval in [true, false] {
        for choice in [
            json!("allow"),
            json!("allow_session"),
            json!("deny"),
            json!(0),
            json!(1),
            json!(2),
            json!(99),
            json!("unknown"),
        ] {
            let mut h = Harness::opened(display_read(0, 0)).await;
            h.app.remote.url = Some("http://localhost:8765".into());
            let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
            h.session_mut().interaction = Some(crate::app::PendingInteraction {
                turn: crate::protocol::TurnRef::new("s1", "l1"),
                interaction_id: "interaction".into(),
                tool_call_id: "call".into(),
                tool_name: "bash".into(),
                prompt: "choose".into(),
                kind: if approval {
                    crate::app::InteractionKind::Approval
                } else {
                    crate::app::InteractionKind::Choice(vec!["one".into(), "two".into()])
                },
                selected: 0,
            });
            let valid = if approval {
                matches!(choice.as_str(), Some("allow" | "allow_session" | "deny"))
            } else {
                choice.as_u64().is_some_and(|index| index < 2)
            };
            h.fake.script(
                "interaction.answer",
                after_snapshot(json!({}), h.control(json!({}))),
            );
            request(
                &mut h,
                json!({"kind":"answer", "interaction_id":"interaction", "choice":choice}),
            )
            .await;
            let ack = ack_next(&mut h, &mut output).await;
            assert_eq!(ack["ok"], valid, "approval={approval} choice={choice}");
            assert_eq!(h.app.has_interaction(), !valid);
            h.settle().await;
            assert_eq!(h.calls("interaction.answer").len(), usize::from(valid));
            if !valid {
                assert_eq!(ack["error"], "bad_request");
                assert!(h.session().allowed_tools.is_empty());
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_deny_with_a_message_answers_first_then_pushes_the_reason_as_a_steer() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let queued = vec![entry("i1", "steer", "Use the staging bucket")];
    h.fake.script(
        "input.push",
        after_snapshot(
            pushed(queued.clone()),
            h.control(json!({"queue":queue(queued, false)})),
        ),
    );
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    pending(&mut h, true);
    request(
        &mut h,
        json!({"kind":"answer", "interaction_id":"interaction", "choice":"deny", "message":"Use the staging bucket  "}),
    ).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    let methods: Vec<String> = h
        .fake
        .methods()
        .into_iter()
        .filter(|method| method == "interaction.answer" || method == "input.push")
        .collect();
    assert_eq!(methods, ["interaction.answer", "input.push"]);
    assert_eq!(
        h.calls("interaction.answer")[0]["answer"],
        json!({"type":"approval", "decision":"deny"})
    );
    assert_eq!(
        h.pushes(),
        [("steer".to_owned(), "Use the staging bucket".to_owned())]
    );
    assert_eq!(h.queue(), ["Use the staging bucket"]);

    // A plain answer steers nothing; a message beside anything but a deny is refused untouched.
    pending(&mut h, true);
    request(
        &mut h,
        json!({"kind":"answer", "interaction_id":"interaction", "choice":"deny"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    for (approval, choice, message) in [
        (true, json!("allow"), "why"),
        (true, json!("allow_session"), "why"),
        (true, json!("deny"), "  "),
        (false, json!(1), "why"),
    ] {
        pending(&mut h, approval);
        request(
            &mut h,
            json!({"kind":"answer", "interaction_id":"interaction", "choice":choice, "message":message}),
        ).await;
        assert_eq!(ack_next(&mut h, &mut output).await["error"], "bad_request");
        assert!(h.app.has_interaction());
    }
    h.settle().await;
    assert_eq!(h.calls("interaction.answer").len(), 2);
    assert_eq!(h.calls("input.push").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn phone_text_expands_templates_refuses_commands_and_keeps_other_slashes_as_the_composer_does()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_sequence("input.push", vec![starting_reply("l1", 2)]);
    h.fake.script("input.push", pushed(vec![]));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.templates = vec![crate::app::templates::Template::parse(
        "review".into(),
        "---\ndescription: Review a file\n---\nReview $1 for ${2:-bugs}; all: $@\n",
    )];
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    request(&mut h, json!({"kind":"submit", "text":"/review lib.rs"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.sends(), ["Review lib.rs for bugs; all: lib.rs"]);
    let mut queued = Vec::new();
    for (text, sent) in [
        (
            "/review parser.rs 'edge cases'",
            Some("Review parser.rs for edge cases; all: parser.rs edge cases"),
        ),
        ("//review x", Some("/review x")),
        (
            "/usr/bin is where it lives",
            Some("/usr/bin is where it lives"),
        ),
        (
            "fix src/main.rs then /review",
            Some("fix src/main.rs then /review"),
        ),
        ("/reveiw the parser", Some("/reveiw the parser")),
        ("/9lives", Some("/9lives")),
        ("/ hello", Some("/ hello")),
        ("/compact", None),
        ("/model mock", None),
        ("/rename", None),
        ("/reveiw", None),
        ("  /foo-bar  ", None),
    ] {
        request(&mut h, json!({"kind":"queue", "text":text})).await;
        let ack = ack_next(&mut h, &mut output).await;
        match sent {
            Some(sent) => {
                assert_eq!(ack["ok"], true, "{text}: {ack}");
                queued.push(sent);
            }
            None => assert_eq!(ack["error"], "unknown_command", "{text}"),
        }
    }
    h.settle().await;
    let mut expected = vec![(
        "steer".to_owned(),
        "Review lib.rs for bugs; all: lib.rs".to_owned(),
    )];
    expected.extend(
        queued
            .iter()
            .map(|text| ("follow_up".to_owned(), (*text).to_owned())),
    );
    assert_eq!(h.pushes(), expected, "commands never reach the Agent");
    assert!(h.notices().is_empty(), "{:?}", h.notices());
}

#[tokio::test(start_paused = true)]
async fn compact_runs_for_the_viewed_idle_session_and_is_refused_while_busy_or_in_a_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "session.compact",
        json!({"operation_id":"tui-1", "status":"noop"}),
    );
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    let progress = || {
        serde_json::from_value(json!({"operation_id":"auto-1", "phase":"summarizing", "covered_item_count":0, "retained_item_count":0})).unwrap()
    };
    for (live, compacting) in [(true, false), (false, true)] {
        let session = h.session_mut();
        session.live =
            live.then(|| crate::app::LiveTurn::new(crate::protocol::TurnRef::new("s1", "l1"), 0));
        session.compaction = compacting.then(progress);
        request(&mut h, json!({"kind":"compact"})).await;
        assert_eq!(ack_next(&mut h, &mut output).await["error"], "busy");
    }
    h.session_mut().compaction = None;
    request(&mut h, json!({"kind":"compact"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    let calls = h.calls("session.compact");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["session_id"], "s1");

    let mut draft = Harness::new();
    draft.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut draft, 1, "phone").await;
    request(&mut draft, json!({"kind":"compact"})).await;
    assert_eq!(
        ack_next(&mut draft, &mut output).await["error"],
        "No session"
    );
    draft.settle().await;
    assert!(draft.calls("session.compact").is_empty());
}

#[tokio::test(start_paused = true)]
async fn unqueue_removes_the_mirrored_entry_by_id_and_returns_its_text_only_to_the_requester() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut first, _close) = connect_phone(&mut h, 1, "first").await;
    let (mut second, _close2) = connect_phone(&mut h, 2, "second").await;
    let now = entry("a", "steer", "now");
    let one = entry("b", "follow_up", "one");
    let two = entry("c", "follow_up", "two");
    h.mirror(vec![now.clone(), one, two.clone()], false);
    for input_id in ["missing", "zzz"] {
        let intent = phone_intent(
            &mut h,
            1,
            json!({"kind":"unqueue", "input_id":input_id, "edit":true}),
        );
        let id = request(&mut h, intent).await;
        assert_eq!(
            messages(&mut first),
            [
                json!({"t":"ack", "id":id, "ok":false, "error":"input_not_queued",
                    "acceptance":"rejected"})
            ],
            "the phone learns the row it named is gone"
        );
    }
    h.settle().await;
    assert!(
        h.calls("input.remove").is_empty(),
        "a stale row asks nothing"
    );

    h.fake.script(
        "input.remove",
        json!({"text": "one", "queue": queue(vec![now.clone(), two.clone()], false)}),
    );
    let revision = h.session().recovery.revision;
    let control = h.control(json!({"queue":queue(vec![now.clone(), two.clone()], false)}));
    h.fake
        .script("session.state@s1", json!({"snapshot":control}));
    let intent = phone_intent(
        &mut h,
        2,
        json!({"kind":"unqueue", "input_id":"b", "edit":true}),
    );
    let id = request_from(&mut h, 2, intent).await;
    h.settle().await;
    assert_eq!(
        messages(&mut second),
        [json!({"t":"ack", "id":id, "ok":true, "input_id":"b", "text":"one"})]
    );
    assert!(messages(&mut first).is_empty());
    assert_eq!(
        h.calls("input.remove"),
        [json!({"session_id":"s1", "load":"load_1", "id":"b"})]
    );
    assert!(h.session().recovery.revision > revision);
    assert_eq!(h.queue(), ["now", "two"]);

    h.fake.script(
        "input.remove",
        json!({"text": "two", "queue": queue(vec![now.clone()], false)}),
    );
    let revision = h.session().recovery.revision;
    let control = h.control(json!({"queue":queue(vec![now], false)}));
    h.fake
        .script("session.state@s1", json!({"snapshot":control}));
    let intent = phone_intent(
        &mut h,
        1,
        json!({"kind":"unqueue", "input_id":"c", "edit":false}),
    );
    let id = request(&mut h, intent).await;
    h.settle().await;
    assert_eq!(
        messages(&mut first),
        [json!({"t":"ack", "id":id, "ok":true})]
    );
    assert!(messages(&mut second).is_empty());
    assert!(h.session().recovery.revision > revision);
    assert_eq!(
        h.queue(),
        ["now"],
        "the newer authoritative snapshot preserves the survivor"
    );
    assert_eq!(h.session().queue.items[0].id, "a");
    assert!(h.app.composer.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stale_phone_row_cannot_remove_a_different_input_with_the_same_text() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut h = resident_session();
        enable_remote(&mut h);
        let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
        let first = entry("a", "steer", "same");
        let second = entry("b", "steer", "same");
        h.mirror(vec![first, second.clone()], false);
        h.app.sync_remote();
        let snapshot = output.try_recv().expect("the phone's original view");
        assert_eq!(snapshot["state"]["queue"][0]["text"], "same");
        // The row went with `a`; naming the same text must not remove `b`.
        let stale = phone_intent(
            &mut h,
            1,
            json!({"kind":"unqueue", "input_id":"a", "edit":true}),
        );

        // The Agent handed `a` over; its queue now holds `b` alone.
        h.mirror(vec![second], false);
        request(&mut h, stale).await;
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["ok"], false, "the stale row must be rejected");
        assert_eq!(
            ack["error"], "input_not_queued",
            "the phone learns the entry is not queued"
        );
        h.settle().await;
        assert!(
            h.calls("input.remove").is_empty(),
            "a stale row must not dispatch removal of b: {:?}",
            h.calls("input.remove")
        );
    })
    .await
    .expect("stale phone intent timed out");
}

#[tokio::test(start_paused = true)]
async fn a_phone_submit_is_not_acknowledged_before_the_agent_accepts_it() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut h = resident_session();
        h.app.remote.url = Some("http://localhost:8765".into());
        let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
        h.fake.script_held("input.push", starting_reply("l1", 2));
        h.fake.script_held("turn.wait", waited("l1", completed()));
        request(&mut h, json!({"kind":"submit", "text":"phone input"})).await;
        h.fake.wait_for_calls("input.push", 1).await;
        assert_eq!(
            h.calls("input.push").len(),
            1,
            "the Agent call is held in flight"
        );
        let premature = output.try_recv();
        assert!(
            premature.is_err(),
            "no ACK is valid before the Agent answers: {premature:?}"
        );

        h.fake.release("input.push");
        h.until(|app| app.is_busy()).await;
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["t"], "ack");
        assert_eq!(ack["ok"], true);
    })
    .await
    .expect("phone submit timed out");
}

#[tokio::test(start_paused = true)]
async fn unqueueing_an_entry_the_agent_already_handed_over_is_refused_without_losing_the_queue() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _close) = connect_phone(&mut h, 1, "phone").await;
    h.mirror(vec![entry("a", "steer", "now")], false);
    h.fake.script_error(
        "input.remove",
        -32031,
        "the input is no longer queued",
        "input_not_queued",
    );
    let intent = phone_intent(
        &mut h,
        1,
        json!({"kind":"unqueue", "input_id":"a", "edit":true}),
    );
    request(&mut h, intent).await;
    h.settle().await;
    let ack = messages(&mut output);
    assert_eq!(ack.len(), 1, "one answer for the one operation: {ack:?}");
    assert_eq!(ack[0]["ok"], false);
    assert_eq!(
        ack[0]["acceptance"], "rejected",
        "the text never became unsent work"
    );
    assert_eq!(h.queue(), ["now"], "the mirrored queue stays as it was");
    assert!(
        ack.iter().all(|message| message["t"] != "returned"),
        "a refused edit returns no text"
    );
}

#[tokio::test(start_paused = true)]
async fn a_phone_intent_from_another_view_epoch_or_load_is_refused_before_anything_runs() {
    let mut h = resident_session();
    enable_remote(&mut h);
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    h.mirror(vec![entry("a", "steer", "now")], false);
    h.app.sync_remote();
    output.try_recv().expect("the phone's original view");

    for (field, value) in [
        ("view_epoch", json!("view_1_999")),
        ("load", json!("load_other")),
    ] {
        let mut intent = phone_intent(
            &mut h,
            1,
            json!({"kind":"unqueue", "input_id":"a", "edit":true}),
        );
        intent[field] = value;
        request(&mut h, intent).await;
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["ok"], false, "{field}");
        assert!(
            ack["error"] == "stale" || ack["error"] == "stale_load",
            "{field}: {ack}"
        );
    }
    h.settle().await;
    assert!(
        h.calls("input.remove").is_empty(),
        "a stale target never reaches the Agent: {:?}",
        h.calls("input.remove")
    );
    assert_eq!(h.queue(), ["now"], "a stale target changed nothing");

    // The projection the phone actually holds still works.
    h.fake.script(
        "input.remove",
        after_snapshot(
            json!({"text":"now", "queue":queue(vec![], false)}),
            h.control(json!({"queue":queue(vec![], false)})),
        ),
    );
    let intent = phone_intent(
        &mut h,
        1,
        json!({"kind":"unqueue", "input_id":"a", "edit":false}),
    );
    request(&mut h, intent).await;
    h.settle().await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    assert_eq!(h.calls("input.remove").len(), 1, "the live row is removed");
    assert!(
        h.queue().is_empty(),
        "the refreshed control snapshot confirms removal"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancel_or_answer_for_another_loop_is_refused_rather_than_acting_on_the_current_one() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    h.session_mut().live = Some(crate::app::LiveTurn::new(
        crate::protocol::TurnRef::new("s1", "l_now"),
        0,
    ));
    h.fake.script("turn.cancel", json!({}));
    let stale = phone_intent(&mut h, 1, json!({"kind":"cancel", "loop_id":"l_old"}));
    request(&mut h, stale).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "stale");
    h.settle().await;
    assert!(
        h.calls("turn.cancel").is_empty(),
        "an old view never cancels the current turn"
    );

    let live = phone_intent(&mut h, 1, json!({"kind":"cancel", "loop_id":"l_now"}));
    request(&mut h, live).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    assert_eq!(h.calls("turn.cancel")[0]["loop_id"], "l_now");
}

#[tokio::test(start_paused = true)]
async fn queue_cancel_and_history_intents_use_the_existing_reducer_operations() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    h.session_mut().live = Some(crate::app::LiveTurn::new(
        crate::protocol::TurnRef::new("s1", "l1"),
        0,
    ));
    h.fake.script(
        "input.push",
        after_snapshot(
            pushed(vec![entry("i1", "follow_up", "follow up")]),
            h.control(json!({"queue":queue(vec![entry("i1", "follow_up", "follow up")], false)})),
        ),
    );
    h.fake.script("session.compact.cancel", json!({}));
    request(&mut h, json!({"kind":"queue", "text":"follow up  \n"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    assert_eq!(h.queue(), ["follow up"]);
    assert_eq!(
        h.pushes(),
        [("follow_up".to_owned(), "follow up".to_owned())]
    );
    assert!(h.calls("turn.wait").is_empty());
    let session = h.session_mut();
    session.live = None;
    session.compaction = Some(serde_json::from_value(json!({"operation_id":"compact", "phase":"summarizing", "covered_item_count":0, "retained_item_count":0})).unwrap());
    request(&mut h, json!({"kind":"cancel"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    assert_eq!(
        h.calls("session.compact.cancel")[0]["operation_id"],
        "compact"
    );
    let before = h.calls("session.read").len();
    h.session_mut().loaded_from = 20;
    request(&mut h, json!({"kind":"load_older"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    assert!(h.calls("session.read").len() > before);
}

#[tokio::test(start_paused = true)]
async fn local_submit_is_refused_but_phone_submit_preserves_the_local_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", starting_reply("l1", 2));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.remote.url = Some("http://localhost:8765".into());
    h.app.composer.set_text("local draft");
    h.action(Action::Submit);
    assert_eq!(h.app.composer.text(), "local draft");
    assert_eq!(
        h.app.flash.as_ref().unwrap().0,
        "Remote control is on · /remote off to take back"
    );
    assert!(h.sends().is_empty());
    let (mut output, _close) = connect_phone(&mut h, 1, "phone").await;
    request(&mut h, json!({"kind":"submit", "text":"phone prompt"})).await;
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.sends(), ["phone prompt"]);
    assert_eq!(h.app.composer.text(), "local draft");
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.press(KeyCode::Esc);
    h.settle().await;
    assert_eq!(h.calls("turn.cancel").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_tool_allowed_for_the_session_is_answered_without_telling_the_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut waiting = running_snapshot(2, "l1", "");
    waiting["pending_interaction"] = json!({"loop_id":"l1", "request_index":0,
        "interaction_id":"i1", "tool_call_id":"c1", "tool_name":"bash",
        "kind":{"type":"approval", "data":{"prompt":"Allow bash?"}}});
    h.fake.script("session.state@s1", state_response(waiting));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.fake.script(
        "interaction.answer",
        after_snapshot(json!({"ok":true}), running_snapshot(3, "l1", "")),
    );
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close) = connect_phone(&mut h, 1, "first").await;
    received(&mut a);
    h.session_mut().allowed_tools.insert("bash".into());
    h.emit(
        "interaction_requested",
        json!({"turn": turn("l1"), "interaction": {
            "interaction_id": "i1", "tool_call_id": "c1", "tool_name": "bash",
            "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}}}),
    )
    .await;
    h.settle().await;
    assert_eq!(h.calls("interaction.answer").len(), 1);
    assert!(h.notices().is_empty());
    assert!(!received(&mut a).contains(&json!("notice")));
}

#[tokio::test(start_paused = true)]
async fn a_refused_input_answers_only_its_original_operation_and_never_other_viewers() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", starting_reply("l1", 2));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close) = connect_phone(&mut h, 1, "first").await;
    let (mut b, _close2) = connect_phone(&mut h, 2, "second").await;
    let (mut c, _close3) = connect_phone(&mut h, 3, "third").await;
    // A push the Agent rejects is answered only to the operation that made it.
    h.fake.script_error_with(
        "input.push",
        -32030,
        "refused",
        "input_rejected",
        Some("rejected"),
    );
    request_from(&mut h, 2, json!({"kind":"submit", "text":"from second"})).await;
    let ack = ack_next(&mut h, &mut b).await;
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["acceptance"], "rejected");
    // The original browser still owns its draft; no separately routed text or notice is needed.
    for output in [&mut a, &mut b, &mut c] {
        assert!(messages(output).is_empty());
    }
    assert!(
        !messages(&mut a)
            .iter()
            .any(|message| message["t"] == "returned")
            && !messages(&mut c)
                .iter()
                .any(|message| message["t"] == "returned")
    );

    // A connection that left keeps no claim on text its operation never sent.
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(2)));
    assert_eq!(h.app.composer.text(), "", "a gone phone returns no text");
    h.fake.script_error_with(
        "input.push",
        -32030,
        "refused",
        "input_rejected",
        Some("rejected"),
    );
    request_from(&mut h, 1, json!({"kind":"submit", "text":"still here"})).await;
    let ack = ack_next(&mut h, &mut a).await;
    assert_eq!(ack["ok"], false);
    assert!(received(&mut c).is_empty());
}

#[tokio::test(start_paused = true)]
async fn text_answers_use_the_snapshot_question_and_keep_literal_allowed_text_until_agent_ack() {
    let (mut h, mut output) = connected_session().await;
    h.app.composer.set_text("conversation draft");
    for (index, text) in [
        "allow".to_owned(),
        " deny ".into(),
        " ".into(),
        "é".repeat(4096),
    ]
    .into_iter()
    .enumerate()
    {
        let question = format!("text-{index}");
        text_control(&mut h, "text_loop", Some(&question)).await;
        h.app.sync_remote();
        let projection = projected(&mut output);
        assert_eq!(projection["interaction"]["kind"], "text");
        assert_eq!(projection["interaction"]["options"], json!([]));
        assert_eq!(projection["interaction"]["loop_id"], "text_loop");
        h.fake.script_held("interaction.answer", json!({"ok":true}));
        let operation = request(
            &mut h,
            json!({"kind":"answer", "loop_id":"text_loop", "interaction_id":question,
            "choice":{"text":text}}),
        )
        .await;
        called(&h, "interaction.answer", index + 1).await;
        assert!(
            messages(&mut output).is_empty(),
            "a text answer waits on the Agent outcome"
        );
        assert_eq!(
            h.calls("interaction.answer")[index],
            json!({"session_id":"s1", "load":"load_1", "loop_id":"text_loop",
            "interaction_id":question, "answer":{"type":"text", "text":text}})
        );
        h.fake.release("interaction.answer");
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["id"], operation);
        assert_eq!(ack["ok"], true);
        assert!(ack.get("input_id").is_none());
        assert!(h.session().allowed_tools.is_empty());
        assert!(
            h.calls("input.push").is_empty(),
            "text answers are not a deny-reason steer"
        );
        assert_eq!(h.app.composer.text(), "conversation draft");
    }
}

#[tokio::test(start_paused = true)]
async fn text_answers_validate_question_identity_and_kind_without_approval_or_steer_fallback() {
    let (mut h, mut output) = connected_session().await;
    text_control(&mut h, "text_loop", Some("text_question")).await;
    for (loop_id, question, choice, message, error) in [
        (
            "old_loop",
            "text_question",
            json!({"text":"answer"}),
            Value::Null,
            "stale",
        ),
        (
            "text_loop",
            "old_question",
            json!({"text":"answer"}),
            Value::Null,
            "stale",
        ),
        (
            "text_loop",
            "text_question",
            json!("allow"),
            Value::Null,
            "bad_request",
        ),
        (
            "text_loop",
            "text_question",
            json!(0),
            Value::Null,
            "bad_request",
        ),
        (
            "text_loop",
            "text_question",
            json!({"text":"answer"}),
            json!("not a deny reason"),
            "bad_request",
        ),
    ] {
        request(
            &mut h,
            json!({"kind":"answer", "loop_id":loop_id, "interaction_id":question,
            "choice":choice, "message":message}),
        )
        .await;
        assert_eq!(ack_next(&mut h, &mut output).await["error"], error);
    }
    assert!(h.calls("interaction.answer").is_empty());
    assert!(h.calls("input.push").is_empty());
    for approval in [true, false] {
        pending(&mut h, approval);
        messages(&mut output);
        request(&mut h, json!({"kind":"answer", "loop_id":"l1", "interaction_id":"interaction", "choice":{"text":"allow"}})).await;
        assert_eq!(ack_next(&mut h, &mut output).await["error"], "bad_request");
    }
    assert!(h.calls("interaction.answer").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_late_text_reply_does_not_clear_a_replacement_or_retarget_a_cancelled_question() {
    let (mut h, mut output) = connected_session().await;
    h.app.composer.set_text("untouched");
    text_control(&mut h, "text_loop", Some("old_question")).await;
    h.fake.script_held("interaction.answer", json!({"ok":true}));
    let original = request(
        &mut h,
        json!({"kind":"answer", "loop_id":"text_loop", "interaction_id":"old_question",
        "choice":{"text":"old answer"}}),
    )
    .await;
    called(&h, "interaction.answer", 1).await;
    text_control(&mut h, "text_loop", Some("replacement")).await;
    h.fake.release("interaction.answer");
    assert_eq!(ack_next(&mut h, &mut output).await["id"], original);
    assert_eq!(
        h.session().interaction.as_ref().unwrap().interaction_id,
        "replacement"
    );
    text_control(&mut h, "text_loop", None).await;
    request(&mut h, json!({"kind":"answer", "loop_id":"text_loop", "interaction_id":"replacement", "choice":{"text":"too late"}})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "stale");
    assert_eq!(h.calls("interaction.answer").len(), 1);
    assert!(h.session().interaction.is_none());
    assert_eq!(h.app.composer.text(), "untouched");
    assert!(h.calls("input.push").is_empty());
}

#[tokio::test(start_paused = true)]
async fn disconnected_text_answers_stay_budgeted_and_never_acknowledge_the_replacement_phone() {
    let (mut h, mut output) = connected_session().await;
    text_control(&mut h, "text_loop", Some("text_question")).await;
    h.fake.script_held("interaction.answer", json!({"ok":true}));
    let text = "é".repeat(4096);
    let operation = request(
        &mut h,
        json!({"kind":"answer", "loop_id":"text_loop", "interaction_id":"text_question",
        "choice":{"text":text}}),
    )
    .await;
    called(&h, "interaction.answer", 1).await;
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    let held = h.app.remote.pending(1, &operation).unwrap();
    assert_eq!(held.kind.text().unwrap(), text);
    let remaining = "x".repeat(crate::remote::Remote::MAX_PENDING_BYTES - text.len() + 1);
    assert!(!h.app.remote.admits_pending(2, Some(&remaining)));
    let (mut replacement, _closing) = connect_phone(&mut h, 2, "first").await;
    h.fake.release("interaction.answer");
    h.settle().await;
    assert!(messages(&mut output).is_empty());
    assert!(messages(&mut replacement).is_empty());
    assert!(h.app.remote.admits_pending(2, Some(&remaining)));
    assert!(h.calls("input.push").is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_accepted_start_failure_keeps_the_new_input_and_the_actual_failed_head() {
    let (mut h, mut output) = connected_session().await;
    let mut receipt = pushed(vec![
        entry("older", "follow_up", "old"),
        entry("new", "follow_up", "same"),
    ]);
    receipt["id"] = json!("new");
    receipt["accepted_at"] = json!("2026-10-30T10:00:00Z");
    receipt["start_error"] = json!({"input_id":"older", "kind":"runtime_start_failed", "message":"The queued turn could not start"});
    receipt["queue"]["paused"] = json!(true);
    h.fake.script_held("input.push", receipt);
    let id = request(&mut h, json!({"kind":"queue", "text":"same"})).await;
    called(&h, "input.push", 1).await;
    assert!(messages(&mut output).is_empty());
    h.fake.release("input.push");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], id);
    assert_eq!(ack["accepted"], true);
    assert_eq!(ack["input_id"], "new");
    assert_eq!(ack["session_id"], "s1");
    assert_eq!(ack["load"], "load_1");
    assert_eq!(ack["start_error"]["input_id"], "older");
    assert_eq!(ack["accepted_at"], "2026-10-30T10:00:00Z");
    assert!(h.app.composer.is_empty());
}

#[tokio::test(start_paused = true)]
async fn agent_backed_settings_and_compaction_share_the_pending_limit_and_wait_for_outcomes() {
    let (mut h, mut output) = connected_session().await;
    h.fake
        .script_held("session.update", json!({"session":session_info("s1")}));
    let mut ids = Vec::new();
    for _ in 0..crate::remote::Remote::MAX_PENDING_PER_CONNECTION {
        ids.push(request(&mut h, json!({"kind":"set_reasoning", "level":"high"})).await);
    }
    called(&h, "session.update", 4).await;
    assert!(messages(&mut output).is_empty());
    request(&mut h, json!({"kind":"compact"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "busy");
    assert!(h.calls("session.compact").is_empty());
    h.fake.release("session.update");
    let mut answers = Vec::new();
    for _ in 0..4 {
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["ok"], true);
        answers.push(ack["id"].as_str().unwrap().to_owned());
    }
    ids.sort();
    answers.sort();
    assert_eq!(ids, answers);
    let compacting = h.control(
        json!({"compaction":{"operation_id":"tui-1", "phase":"summarizing",
        "covered_item_count":0, "retained_item_count":0}}),
    );
    let mut compacted = compacting.clone();
    compacted["compaction"] = Value::Null;
    compacted["revision"] = json!(compacting["revision"].as_u64().unwrap() + 1);
    h.fake.script_held(
        "session.compact",
        after_snapshot(json!({"operation_id":"tui-1", "status":"noop"}), compacted),
    );
    let compact = "compact-operation".to_owned();
    let intent = phone_intent(&mut h, 1, json!({"kind":"compact"}));
    let (done, finished) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Request {
        connection: 1,
        id: compact.clone(),
        intent: serde_json::from_value(intent).unwrap(),
        done,
    }));
    assert!(
        h.session().compaction.is_none(),
        "dispatch cannot write compaction control"
    );
    finished.await.unwrap();
    called(&h, "session.compact", 1).await;
    h.snapshot_event(compacting).await;
    h.settle().await;
    assert!(messages(&mut output).is_empty());
    assert_eq!(
        h.session().compaction.as_ref().unwrap().operation_id,
        "tui-1"
    );
    h.fake.release("session.compact");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], compact);
    assert_eq!(ack["ok"], true);
    assert!(ack.get("input_id").is_none());
}

#[tokio::test(start_paused = true)]
async fn reusing_an_inflight_operation_id_revokes_delivery_instead_of_answering_the_wrong_operation()
 {
    let (mut h, mut output) = connected_session().await;
    let closing = h.app.remote.phones[0].connection.close.subscribe();
    h.fake.script_held("input.push", pushed(vec![]));
    let id = request(&mut h, json!({"kind":"submit", "text":"same"})).await;
    let mut intent = phone_intent(&mut h, 1, json!({"kind":"new_session"}));
    intent["view_epoch"] = json!("stale");
    let (done, finished) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Request {
        connection: 1,
        id: id.clone(),
        intent: serde_json::from_value(intent).unwrap(),
        done,
    }));
    finished.await.unwrap();
    assert_eq!(*closing.borrow(), Some("duplicate_id"));
    assert!(h.app.remote.phones.is_empty());
    assert!(h.app.remote.pending(1, &id).is_some());
    h.fake.release("input.push");
    h.settle().await;
    assert_eq!(h.calls("input.push").len(), 1);
    assert!(messages(&mut output).is_empty());
    assert!(h.app.remote.pending(1, &id).is_none());
}

#[tokio::test(start_paused = true)]
async fn a_receipt_for_another_load_is_unknown_not_a_success_for_the_current_operation() {
    let (mut h, mut output) = connected_session().await;
    let mut wrong = pushed(vec![]);
    wrong["session_id"] = json!("s1");
    wrong["load"] = json!("another_load");
    h.fake.script("input.push", wrong);
    request(&mut h, json!({"kind":"submit", "text":"same"})).await;
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["acceptance"], "unknown");
    assert_eq!(h.calls("input.push").len(), 1);
    assert!(h.app.composer.is_empty());
}

#[tokio::test(start_paused = true)]
async fn accepted_inputs_coalesce_control_reads_without_waiting_for_them_to_ack() {
    let (mut h, mut output) = connected_session().await;
    h.mirror(vec![], true);
    let control = h.control(json!({"queue":queue(vec![], true)}));
    h.fake
        .script_held("session.state@s1", state_response(control.clone()));
    let mut queued = Vec::new();
    for index in 0..4 {
        let id = format!("input-{index}");
        queued.push(entry(&id, "follow_up", "same"));
        let mut receipt = pushed(queued.clone());
        receipt["id"] = json!(id);
        receipt["queue"]["paused"] = json!(true);
        h.fake.script("input.push", receipt);
        let operation = request(&mut h, json!({"kind":"queue", "text":"same"})).await;
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["id"], operation);
        assert_eq!(ack["input_id"], id);
    }
    called(&h, "session.state", 1).await;
    assert_eq!(h.calls("input.push").len(), 4);
    assert_eq!(
        h.calls("session.state").len(),
        1,
        "a slow read cannot accumulate one task per input"
    );
    h.fake.release("session.state@s1");
    let mut latest = control;
    latest["revision"] = json!(latest["revision"].as_u64().unwrap() + 1);
    latest["queue"] = queue(queued, true);
    h.fake.script("session.state@s1", state_response(latest));
    h.settle().await;
    assert!(
        h.calls("session.state").len() <= 2,
        "only one catch-up read is needed"
    );
}

#[tokio::test(start_paused = true)]
async fn transport_errors_are_unknown_but_explicit_rejections_stay_rejected() {
    let (mut h, mut output) = connected_session().await;
    for acceptance in [None, Some("unknown"), Some("rejected")] {
        h.fake.script_error_with(
            "input.push",
            -32000,
            "not confirmed",
            "test_error",
            acceptance,
        );
        let id = request(&mut h, json!({"kind":"submit", "text":"same"})).await;
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(ack["id"], id);
        assert_eq!(ack["ok"], false);
        assert_eq!(ack["acceptance"], acceptance.unwrap_or("unknown"));
        assert!(h.session().returned.is_empty());
    }
    assert_eq!(
        h.calls("input.push").len(),
        3,
        "there is no automatic resend"
    );
}

#[tokio::test(start_paused = true)]
async fn late_acceptance_names_the_original_load_without_touching_the_reopened_instance() {
    let (mut h, mut output) = connected_session().await;
    h.fake.script_held("input.push", pushed(vec![]));
    let id = request(&mut h, json!({"kind":"submit", "text":"same"})).await;
    called(&h, "input.push", 1).await;
    let old = h.session();
    let replacement = crate::app::Session::new(old.info.clone(), old.epoch + 1, "reopened".into());
    h.app.resident.remove("s1");
    h.app.show_session(Some(replacement));
    h.fake.release("input.push");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], id);
    assert_eq!(ack["load"], "load_1");
    assert_eq!(h.session().load, "reopened");
    assert!(
        h.calls("session.state").is_empty(),
        "an old epoch cannot refresh the new instance"
    );
    assert!(!h.app.is_busy());
}

#[tokio::test(start_paused = true)]
async fn disconnected_late_ack_and_edit_text_never_reach_a_replacement_device() {
    let (mut h, mut output) = connected_session().await;
    h.fake.script_held("input.push", pushed(vec![]));
    let first = request(&mut h, json!({"kind":"submit", "text":"same"})).await;
    h.mirror(vec![entry("old", "follow_up", "same")], false);
    h.fake.script_held(
        "input.remove",
        json!({"text":"same", "queue":queue(vec![], false)}),
    );
    let edit = request(
        &mut h,
        json!({"kind":"unqueue", "input_id":"old", "edit":true}),
    )
    .await;
    called(&h, "input.remove", 1).await;
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    assert!(
        h.app.remote.pending(1, &first).is_some(),
        "an in-flight operation remains budgeted"
    );
    let (mut replacement, _closing) = connect_phone(&mut h, 2, "first").await;
    h.fake.release("input.push");
    h.fake.release("input.remove");
    h.settle().await;
    assert!(messages(&mut output).is_empty());
    assert!(messages(&mut replacement).is_empty());
    assert!(h.app.remote.pending(1, &first).is_none());
    assert!(h.app.remote.pending(1, &edit).is_none());
    let second = request_from(&mut h, 2, json!({"kind":"submit", "text":"same"})).await;
    assert_ne!(first, second);
    assert_eq!(ack_next(&mut h, &mut replacement).await["id"], second);
    assert!(h.app.composer.is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancel_answer_and_recall_ack_only_after_the_exact_agent_operation() {
    let (mut h, mut output) = connected_session().await;
    pending(&mut h, true);
    messages(&mut output);
    h.fake.script_held("turn.cancel", json!({"ok":true}));
    let cancel = request(&mut h, json!({"kind":"cancel", "loop_id":"l1"})).await;
    called(&h, "turn.cancel", 1).await;
    assert!(messages(&mut output).is_empty());
    h.fake.release("turn.cancel");
    assert_eq!(ack_next(&mut h, &mut output).await["id"], cancel);
    pending(&mut h, true);
    messages(&mut output);
    let wrong = request(
        &mut h,
        json!({"kind":"answer", "loop_id":"old", "interaction_id":"interaction", "choice":"allow"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut output).await["id"], wrong);
    assert!(h.calls("interaction.answer").is_empty());
    h.fake.script_held("interaction.answer", json!({"ok":true}));
    let answer = request(&mut h, json!({"kind":"answer", "loop_id":"l1", "interaction_id":"interaction", "choice":"allow_session"})).await;
    called(&h, "interaction.answer", 1).await;
    assert!(messages(&mut output).is_empty());
    assert!(h.session().allowed_tools.is_empty());
    h.fake.release("interaction.answer");
    assert_eq!(ack_next(&mut h, &mut output).await["id"], answer);
    assert!(h.session().allowed_tools.contains("bash"));
    h.mirror(vec![entry("recall", "steer", "same")], false);
    h.fake.script_held(
        "input.remove",
        json!({"text":"same", "queue":queue(vec![], false)}),
    );
    let recall = request(
        &mut h,
        json!({"kind":"unqueue", "input_id":"recall", "edit":true}),
    )
    .await;
    called(&h, "input.remove", 1).await;
    assert!(messages(&mut output).is_empty());
    h.fake.release("input.remove");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], recall);
    assert_eq!(ack["input_id"], "recall");
    assert_eq!(ack["text"], "same");
    assert!(
        messages(&mut output).is_empty(),
        "edit text is part of the ACK, not a separately routed message"
    );
    for method in ["turn.cancel", "interaction.answer", "input.remove"] {
        assert_eq!(h.calls(method)[0]["load"], "load_1");
        assert_eq!(h.calls(method)[0]["session_id"], "s1");
    }
}

#[tokio::test(start_paused = true)]
async fn missing_load_and_stale_compaction_identity_are_refused_before_rpc() {
    let (mut h, mut output) = connected_session().await;
    request(&mut h, json!({"kind":"submit", "text":"same", "load":null})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "missing_load");
    assert!(h.calls("input.push").is_empty());
    h.session_mut().compaction = Some(serde_json::from_value(json!({
        "operation_id":"new", "phase":"summarizing", "covered_item_count":0, "retained_item_count":0
    })).unwrap());
    request(&mut h, json!({"kind":"cancel", "operation_id":"old"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "stale");
    assert!(h.calls("session.compact.cancel").is_empty());
    h.fake.script("session.compact.cancel", json!({}));
    request(&mut h, json!({"kind":"cancel", "operation_id":"new"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    assert_eq!(h.calls("session.compact.cancel")[0]["operation_id"], "new");
}

#[tokio::test(start_paused = true)]
async fn pending_count_and_expanded_text_byte_limits_are_busy_before_side_effects() {
    let (mut h, mut output) = connected_session().await;
    h.fake.script_held("input.push", pushed(vec![]));
    for _ in 0..crate::remote::Remote::MAX_PENDING_PER_CONNECTION {
        request(&mut h, json!({"kind":"submit", "text":"held"})).await;
    }
    called(&h, "input.push", 4).await;
    request(&mut h, json!({"kind":"queue", "text":"must not send"})).await;
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["error"], "busy");
    assert_eq!(ack["acceptance"], "rejected");
    assert_eq!(h.calls("input.push").len(), 4);
    h.fake.release("input.push");
    h.settle().await;
    messages(&mut output);
    let large = "x".repeat(crate::remote::Remote::MAX_PENDING_BYTES + 1);
    h.app.templates = vec![crate::app::templates::Template::parse(
        "large".into(),
        &large,
    )];
    request(&mut h, json!({"kind":"submit", "text":"/large"})).await;
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["error"], "busy");
    assert_eq!(ack["acceptance"], "rejected");
    assert_eq!(h.calls("input.push").len(), 4);
    assert!(!h.app.loading_session);
}

#[tokio::test(start_paused = true)]
async fn global_count_and_byte_budgets_remain_charged_after_disconnect_and_precede_create() {
    let (mut h, mut output) = connected_session().await;
    let held = |id: usize, text: String| crate::remote::PendingIntent {
        id: id.to_string(),
        view_epoch: "old".into(),
        target: crate::remote::PendingTarget::Resident(crate::app::resident::Target {
            session_id: "s1".into(),
            load: crate::testing::LOAD.into(),
            epoch: 1,
        }),
        focus_generation: 0,
        kind: crate::remote::PendingKind::Input { text },
    };
    for id in 0..crate::remote::Remote::MAX_PENDING {
        h.app
            .remote
            .hold_pending(2 + id as u64 / 4, held(id, String::new()));
    }
    request(&mut h, json!({"kind":"submit", "text":"must not run"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "busy");
    assert!(h.calls("input.push").is_empty());
    for id in 0..crate::remote::Remote::MAX_PENDING {
        h.app
            .remote
            .release_pending(2 + id as u64 / 4, &id.to_string())
            .unwrap();
    }
    h.app.remote.hold_pending(
        2,
        held(0, "x".repeat(crate::remote::Remote::MAX_PENDING_BYTES)),
    );
    h.app.remote.disconnected(2);
    assert!(h.app.remote.admits_pending(1, None));
    assert!(!h.app.remote.admits_pending(1, Some("x")));
    request(&mut h, json!({"kind":"new_session"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    request(&mut h, json!({"kind":"submit", "text":"must not create"})).await;
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["error"], "busy");
    assert_eq!(ack["acceptance"], "rejected");
    assert!(h.calls("session.create").is_empty());
    assert!(h.calls("input.push").is_empty());
    h.app.remote.release_pending(2, "0").unwrap();
    assert!(h.app.remote.admits_pending(1, Some("x")));
}
