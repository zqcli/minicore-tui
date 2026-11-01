use super::*;

fn live_mut<'a>(h: &'a mut Harness, id: &str) -> &'a mut crate::app::Session {
    let app = &mut h.app;
    app.resident
        .iter_mut()
        .find(|session| session.info.session_id == id)
        .unwrap()
}

/// The `result` messages waiting in `output`, without their `t`.
fn results(output: &mut mpsc::Receiver<Value>) -> Vec<Value> {
    messages(output)
        .into_iter()
        .filter(|message| message["t"] == "result")
        .map(|mut message| {
            message.as_object_mut().unwrap().remove("t");
            message
        })
        .collect()
}

fn query_from(h: &mut Harness, connection: u64, id: &str, query: Value) {
    h.app.on_msg(Msg::Remote(RemoteEvent::Query {
        connection,
        id: id.into(),
        query: serde_json::from_value(query).unwrap(),
    }));
}

async fn separate_views() -> (Harness, mpsc::Receiver<Value>, mpsc::Receiver<Value>) {
    let mut h = Harness::opened(display_read(0, 0)).await;
    enable_remote(&mut h);
    h.app.default_profile = Some("coding".into());
    h.session_mut().info.title = Some("First".into());
    h.app.sessions[0].title = Some("First".into());
    for (id, title) in [("s1", "First"), ("s2", "Second"), ("s3", "Third")] {
        let mut info = session_info(id);
        info["workspace"] = json!(h.app.workspace);
        info["title"] = json!(title);
        if id != "s1" {
            h.app
                .sessions
                .push(serde_json::from_value(info.clone()).unwrap());
        }
        h.fake
            .script(&format!("session.open@{id}"), opened_session(info));
        h.fake.script(
            &format!("session.state@{id}"),
            state_response(snapshot(id, crate::testing::LOAD, 1)),
        );
        let turn = json!({"session_id":id, "loop_id":format!("loop-{id}")});
        h.fake.script(
            &format!("input.push@{id}"),
            after_snapshot(
                started_in(turn.clone(), vec![]),
                crate::testing::snapshot_running(
                    id,
                    crate::testing::LOAD,
                    2,
                    &format!("loop-{id}"),
                    entry("i1", "steer", "$text"),
                ),
            )
            .with_request_text(),
        );
        h.fake.script_held(
            &format!("turn.wait@{id}"),
            json!({"turn":turn, "outcome":completed()}),
        );
    }
    h.app.catalog_generation += 1;
    let (a, _close_a) = connect_phone(&mut h, 1, "first phone").await;
    let (mut b, _close_b) = connect_phone(&mut h, 2, "second phone").await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.until(|app| app.live("s2").is_some()).await;
    h.settle().await;
    (h, a, b)
}

fn listed<'a>(sessions: &'a [Value], id: &str) -> Option<&'a Value> {
    sessions.iter().find(|session| session["id"] == id)
}

/// The session list of the last state the next sync sends, if it sends one.
fn synced_sessions(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Option<Vec<Value>> {
    let state = synced_state(h, output)?;
    Some(state["sessions"].as_array().unwrap().clone())
}

/// `s1` resident and `s2` listed under the roots, `s3` outside them, with a
/// phone admitted and its first snapshot sent.
async fn catalog() -> (Harness, mpsc::Receiver<Value>) {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.config = Some(config(std::path::PathBuf::from("/tmp/ws")));
    h.app.remote.url = Some("http://localhost:8765".into());
    for (id, workspace) in [("s2", "/tmp/ws/app"), ("s3", "/elsewhere")] {
        let mut info = session_info(id);
        info["workspace"] = json!(workspace);
        h.app.sessions.push(serde_json::from_value(info).unwrap());
    }
    h.app.catalog_generation += 1;
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    assert!(synced_sessions(&mut h, &mut output).is_some());
    (h, output)
}

#[tokio::test(start_paused = true)]
async fn model_reasoning_and_absolute_workspace_are_validated_before_dispatch() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("session.update", json!({"session": session_info("s1")}));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut h, 1, "phone").await;
    for (intent, valid) in [
        (json!({"kind":"set_model", "model":"mock"}), true),
        (json!({"kind":"set_reasoning", "level":"high"}), true),
        (json!({"kind":"set_model", "model":"missing"}), false),
        (json!({"kind":"set_reasoning", "level":"missing"}), false),
        (json!({"kind":"new_session", "workspace":"relative"}), false),
    ] {
        request(&mut h, intent).await;
        assert_eq!(ack_next(&mut h, &mut output).await["ok"], valid);
        h.settle().await;
    }
    assert_eq!(h.calls("session.update").len(), 2);
    assert_eq!(h.calls("session.update")[0]["model"], "mock");
    assert_eq!(h.calls("session.update")[1]["reasoning"], "high");
}

#[tokio::test(start_paused = true)]
async fn root_checks_and_stale_answers_refuse_before_dispatch_and_new_drafts_use_the_chosen_workspace()
 {
    let mut h = Harness::new();
    h.boot().await;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    h.app.remote.config = Some(config(workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _close) = connect_phone(&mut h, 1, "phone").await;
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(
        ack_next(&mut h, &mut output).await["error"],
        "outside_roots"
    );
    assert!(h.calls("session.open").is_empty());
    request(
        &mut h,
        json!({"kind":"answer", "loop_id":"old", "interaction_id":"stale", "choice":"allow"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "stale");
    request(
        &mut h,
        json!({"kind":"new_session", "workspace":workspace, "profile":"coding"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    assert!(h.calls("session.create").is_empty());
    let mut info = session_info("s1");
    info["workspace"] = json!(workspace);
    h.fake.script("session.create", opened_session(info));
    h.fake.script("input.push", starting_reply("l1", 2));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.composer.set_text("keep");
    request(&mut h, json!({"kind":"submit", "text":"new work"})).await;
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.calls("session.create")[0]["workspace"], json!(workspace));
    assert_eq!(h.sends(), ["new work"]);
    assert_eq!(h.app.composer.text(), "keep");
}

#[tokio::test(start_paused = true)]
async fn a_phone_renames_another_session_and_its_list_follows() {
    let (mut h, mut output) = catalog().await;
    let mut renamed = session_info("s2");
    renamed["workspace"] = json!("/tmp/ws/app");
    renamed["title"] = json!("Parser fix");
    h.fake.script("session.rename", json!({"session": renamed}));
    let before = h.calls("session.rename").len();
    request(
        &mut h,
        json!({"kind":"rename_session", "session_id":"s2", "title":"  Parser fix  "}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    let calls = h.calls("session.rename");
    assert_eq!(calls.len(), before + 1);
    assert_eq!(calls[before]["session_id"], "s2");
    assert_eq!(calls[before]["title"], "Parser fix");
    let sessions = synced_sessions(&mut h, &mut output).expect("a new state");
    assert_eq!(listed(&sessions, "s2").unwrap()["title"], "Parser fix");
}

#[tokio::test(start_paused = true)]
async fn a_long_or_control_character_title_is_refused_before_any_rename() {
    let (mut h, mut output) = catalog().await;
    let before = h.calls("session.rename").len();
    for title in ["x".repeat(201), "two\nlines".into()] {
        request(
            &mut h,
            json!({"kind":"rename_session", "session_id":"s2", "title":title}),
        )
        .await;
        let ack = ack_next(&mut h, &mut output).await;
        assert_eq!(
            (ack["ok"].clone(), ack["error"].clone()),
            (json!(false), json!("bad_request"))
        );
    }
    h.settle().await;
    assert_eq!(h.calls("session.rename").len(), before);
}

#[tokio::test(start_paused = true)]
async fn a_phone_deletes_another_session_and_it_leaves_the_list() {
    let (mut h, mut output) = catalog().await;
    h.fake.script("session.close", json!({}));
    h.fake.script("session.delete", json!({}));
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    assert!(
        h.calls("session.close").is_empty(),
        "a stored-only target has no load to close"
    );
    assert_eq!(h.calls("session.delete")[0]["session_id"], "s2");
    let sessions = synced_sessions(&mut h, &mut output).expect("a new state");
    assert!(listed(&sessions, "s2").is_none());
    assert!(listed(&sessions, "s1").is_some());
    assert!(h.notices().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_resident_unknown_and_outside_sessions_are_neither_renamed_nor_deleted() {
    let (mut h, mut output) = catalog().await;
    let renames = h.calls("session.rename").len();
    for (intent, error) in [
        (
            json!({"kind":"delete_session", "session_id":"s1"}),
            "Open another session before deleting this one",
        ),
        (
            json!({"kind":"delete_session", "session_id":"missing"}),
            "Unknown session",
        ),
        (
            json!({"kind":"delete_session", "session_id":"s3"}),
            "outside_roots",
        ),
        (
            json!({"kind":"rename_session", "session_id":"missing", "title":"x"}),
            "Unknown session",
        ),
        (
            json!({"kind":"rename_session", "session_id":"s3", "title":"x"}),
            "outside_roots",
        ),
    ] {
        request(&mut h, intent.clone()).await;
        assert_eq!(
            ack_next(&mut h, &mut output).await["error"],
            error,
            "{intent}"
        );
    }
    h.settle().await;
    assert!(h.calls("session.close").is_empty());
    assert!(h.calls("session.delete").is_empty());
    assert_eq!(h.calls("session.rename").len(), renames);
}

#[tokio::test(start_paused = true)]
async fn phones_open_their_own_sessions_without_replacing_each_others_projection() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(ack_next(&mut h, &mut a).await["ok"], true);
    h.app.sync_remote();
    for (output, id) in [(&mut a, "s1"), (&mut b, "s2")] {
        let snapshot = output.try_recv().unwrap();
        assert_eq!(snapshot["t"], "snapshot");
        assert_eq!(snapshot["state"]["session"]["id"], id);
        let sessions = snapshot["state"]["sessions"].as_array().unwrap();
        for listed in sessions {
            assert_eq!(listed["current"], listed["id"] == id);
        }
    }
    request(&mut h, json!({"kind":"open_session", "session_id":"s3"})).await;
    h.until(|app| app.live("s3").is_some()).await;
    h.settle().await;
    h.app.sync_remote();
    let changed = messages(&mut a);
    assert!(
        changed
            .iter()
            .any(|message| message["t"] == "snapshot" && message["state"]["session"]["id"] == "s3")
    );
    let unchanged = messages(&mut b);
    assert!(!unchanged.iter().any(|message| message["t"] == "snapshot"));
    assert_eq!(unchanged.last().unwrap()["state"]["session"]["id"], "s2");
    assert_eq!(h.session().info.session_id, "s3");
    h.action(Action::Remote("off".into()));
    assert_eq!(h.session().info.session_id, "s3");
}

#[tokio::test(start_paused = true)]
async fn phones_send_and_stream_concurrently_only_to_their_sessions_viewers() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.app.composer.set_text("local in two");
    let anchored = crate::app::Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    h.app.scroll = anchored;
    request(&mut h, json!({"kind":"submit", "text":"work in one"})).await;
    assert_eq!(h.app.live("s2").unwrap().scroll, anchored);
    let (mut c, _close_c) = connect_phone(&mut h, 3, "also first").await;
    h.app.composer.set_text("local in one");
    request_from(&mut h, 2, json!({"kind":"submit", "text":"work in two"})).await;
    h.until(|app| app.live("s1").unwrap().is_busy() && app.live("s2").unwrap().is_busy())
        .await;
    h.settle().await;
    assert_eq!(
        h.calls("input.push")
            .iter()
            .map(|call| (call["session_id"].clone(), call["text"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("s1"), json!("work in one")),
            (json!("s2"), json!("work in two"))
        ]
    );
    assert_eq!(h.app.composer.text(), "local in two");
    assert_eq!(
        h.app.live("s1").unwrap().draft.as_deref(),
        Some("local in one")
    );
    h.app.sync_remote();
    for output in [&mut a, &mut b, &mut c] {
        messages(output);
    }
    for (id, text) in [("s1", "one"), ("s1", " plus"), ("s2", "two")] {
        h.emit("output_delta", json!({"meta":{"session_id":id, "dropped_before":0}, "turn":{"session_id":id, "loop_id":format!("loop-{id}")}, "request_index":0, "channel":"text", "delta":text})).await;
        h.settle().await;
        h.app.sync_remote();
        for (output, viewing) in [(&mut a, "s1"), (&mut b, "s2"), (&mut c, "s1")] {
            let changes = messages(output);
            let blocks: Vec<_> = changes
                .iter()
                .filter(|message| message["t"] == "blocks")
                .collect();
            assert_eq!(
                blocks.len(),
                usize::from(viewing == id),
                "{viewing}: {changes:?}"
            );
            if text == " plus" && viewing == id {
                assert_eq!(blocks[0]["upserts"][0]["append"]["text"], " plus");
            }
        }
    }
    request(&mut h, json!({"kind":"cancel"})).await;
    assert_eq!(h.app.composer.text(), "local in one");
    assert_eq!(h.app.live("s2").unwrap().scroll, anchored);
}

#[tokio::test(start_paused = true)]
async fn an_approval_is_shown_and_answered_only_in_the_viewed_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"cancel"})).await;
    let mut waiting =
        crate::testing::snapshot_running("s2", crate::testing::LOAD, 2, "loop-s2", Value::Null);
    waiting["pending_interaction"] = json!({"loop_id":"loop-s2", "request_index":0,
        "interaction_id":"approve-two", "tool_call_id":"call-two", "tool_name":"bash",
        "kind":{"type":"approval", "data":{"prompt":"Allow?"}}});
    h.fake.script("session.state@s2", state_response(waiting));
    h.fake.script(
        "interaction.answer@s2",
        after_snapshot(
            json!({}),
            crate::testing::snapshot_running("s2", crate::testing::LOAD, 3, "loop-s2", Value::Null),
        ),
    );
    h.emit("interaction_requested", json!({"meta":{"session_id":"s2", "dropped_before":0}, "turn":{"session_id":"s2", "loop_id":"loop-s2"}, "interaction":{"interaction_id":"approve-two", "tool_call_id":"call-two", "tool_name":"bash", "kind":{"type":"approval", "data":{"prompt":"Allow?"}}}})).await;
    h.settle().await;
    h.app.sync_remote();
    let first = projected(&mut a);
    let second = projected(&mut b);
    assert!(
        first["interaction"].is_null(),
        "a phone viewing s1 never sees s2's approval: {first}"
    );
    assert_eq!(first["background"]["approval"], 1);
    assert_eq!(second["interaction"]["id"], "approve-two");
    assert_eq!(second["background"]["approval"], 0);
    request(
        &mut h,
        json!({"kind":"answer", "loop_id":"loop-s2", "interaction_id":"approve-two", "choice":"allow"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut a).await["error"], "stale");
    request_from(
        &mut h,
        2,
        json!({"kind":"answer", "interaction_id":"approve-two", "choice":"allow"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.settle().await;
    let answers = h.calls("interaction.answer");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["session_id"], "s2");
    assert_eq!(answers[0]["loop_id"], "loop-s2");
    assert_eq!(answers[0]["interaction_id"], "approve-two");
}

#[tokio::test(start_paused = true)]
async fn each_draft_keeps_its_workspace_model_and_reasoning_through_creation() {
    let (mut h, mut a, mut b) = separate_views().await;
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let one = root.join("one");
    let two = root.join("two");
    std::fs::create_dir(&one).unwrap();
    std::fs::create_dir(&two).unwrap();
    h.app
        .remote
        .config
        .as_mut()
        .unwrap()
        .roots
        .push(root.clone());
    h.app.models.push(
        serde_json::from_value(
            json!({"id":"chosen", "supported_reasoning":["low", "medium", "high"]}),
        )
        .unwrap(),
    );
    request(
        &mut h,
        json!({"kind":"new_session", "workspace":one, "profile":"coding"}),
    )
    .await;
    request(&mut h, json!({"kind":"set_model", "model":"chosen"})).await;
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"})).await;
    h.app.sync_remote();
    let draft = projected(&mut a);
    let session = projected(&mut b);
    assert_eq!(draft["draft"]["model"], "chosen");
    assert_eq!(draft["draft"]["reasoning"], "high");
    assert_eq!(session["session"]["id"], "s2");
    assert_eq!(session["session"]["model"], "mock");
    assert_eq!(
        h.app.model_id(),
        Some("chosen"),
        "the computer describes the last acting draft"
    );
    assert_eq!(h.app.launch, crate::app::LaunchSettings::default());
    let (mut c, _close_c) = connect_phone(&mut h, 3, "new arrival").await;
    h.app.sync_remote();
    assert_eq!(projected(&mut c)["draft"], draft["draft"]);
    request_from(
        &mut h,
        2,
        json!({"kind":"new_session", "workspace":two, "profile":"coding"}),
    )
    .await;
    h.app.sync_remote();
    let other = projected(&mut b);
    assert_eq!(
        other["draft"]["workspace"],
        crate::remote::fs::display(&two)
    );
    assert_eq!(other["draft"]["model"], "mock");
    assert_eq!(other["draft"]["reasoning"], "medium");
    let mut info = session_info("created");
    info["title"] = json!("Created");
    info["workspace"] = json!(one);
    info["model"] = json!("chosen");
    info["reasoning"] = json!("high");
    h.fake.script("session.create", opened_session(info));
    h.fake.script(
        "input.push@created",
        after_snapshot(
            started_in(json!({"session_id":"created", "loop_id":"first"}), vec![]),
            crate::testing::snapshot_running(
                "created",
                crate::testing::LOAD,
                2,
                "first",
                entry("i1", "steer", "$text"),
            ),
        )
        .with_request_text(),
    );
    h.fake.script_held(
        "turn.wait@created",
        json!({"turn":{"session_id":"created", "loop_id":"first"}, "outcome":completed()}),
    );
    request(
        &mut h,
        json!({"kind":"submit", "text":"first in my workspace"}),
    )
    .await;
    h.until(|app| app.live("created").is_some_and(|session| session.is_busy()))
        .await;
    h.settle().await;
    let create = &h.calls("session.create")[0];
    assert_eq!(create["workspace"], json!(one));
    assert_eq!(create["profile"], "coding");
    assert_eq!(create["model"], "chosen");
    assert_eq!(create["reasoning"], "high");
    assert_eq!(h.calls("input.push")[0]["session_id"], "created");
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "created");
    let other = projected(&mut b);
    assert!(other["session"].is_null());
    assert_eq!(
        other["draft"]["workspace"],
        crate::remote::fs::display(&two)
    );
    assert_eq!(other["draft"]["model"], "mock");
    let shared_draft = projected(&mut c);
    assert!(
        shared_draft["session"].is_null(),
        "even an identical draft stays independent"
    );
    assert_eq!(shared_draft["draft"]["model"], "chosen");
    request_from(&mut h, 2, json!({"kind":"set_reasoning", "level":"low"})).await;
    h.action(Action::Remote("off".into()));
    assert!(h.app.resident.current().is_none());
    assert_eq!(h.app.workspace, two);
    assert_eq!(h.app.reasoning(), Some(crate::protocol::Reasoning::Low));
}

#[tokio::test(start_paused = true)]
async fn opening_a_session_never_adopts_another_devices_draft_model() {
    let (mut h, mut a, mut b) = separate_views().await;
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    h.app
        .remote
        .config
        .as_mut()
        .unwrap()
        .roots
        .push(root.clone());
    h.app.models.push(
        serde_json::from_value(json!({"id":"draft-model", "supported_reasoning":["high"]}))
            .unwrap(),
    );
    request(&mut h, json!({"kind":"new_session", "workspace":root})).await;
    request(&mut h, json!({"kind":"set_model", "model":"draft-model"})).await;
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"})).await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s3"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.until(|app| app.live("s3").is_some()).await;
    h.settle().await;
    assert!(
        h.calls("session.update").is_empty(),
        "draft overrides must never reach session.update"
    );
    assert_eq!(h.app.launch, crate::app::LaunchSettings::default());
    assert_eq!(
        h.app.live("s3").unwrap().info.reasoning,
        crate::protocol::Reasoning::Medium
    );
    h.app.sync_remote();
    let draft = projected(&mut a);
    assert_eq!(draft["draft"]["model"], "draft-model");
    assert_eq!(draft["draft"]["reasoning"], "high");
    let opened = projected(&mut b);
    assert_eq!(opened["session"]["model"], "mock");
    assert_eq!(opened["session"]["reasoning"], "medium");
}

#[tokio::test(start_paused = true)]
async fn a_pending_create_keeps_its_original_input_while_other_devices_do_live_work() {
    let (mut h, mut a, mut b) = separate_views().await;
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    h.app
        .remote
        .config
        .as_mut()
        .unwrap()
        .roots
        .push(root.clone());
    request(
        &mut h,
        json!({"kind":"new_session", "workspace":root, "profile":"coding"}),
    )
    .await;
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"})).await;
    let (mut c, _close_c) = connect_phone(&mut h, 3, "another draft").await;
    let mut created = session_info("created");
    created["title"] = json!("Created");
    created["workspace"] = json!(root);
    created["reasoning"] = json!("high");
    h.fake
        .script_held("session.create", opened_session(created));
    h.fake.script(
        "input.push@created",
        after_snapshot(
            started_in(json!({"session_id":"created", "loop_id":"first"}), vec![]),
            crate::testing::snapshot_running(
                "created",
                crate::testing::LOAD,
                2,
                "first",
                entry("i1", "steer", "$text"),
            ),
        )
        .with_request_text(),
    );
    h.fake.script_held(
        "turn.wait@created",
        json!({"turn":{"session_id":"created", "loop_id":"first"}, "outcome":completed()}),
    );
    let creating = request(&mut h, json!({"kind":"submit", "text":"my first message"})).await;
    h.app.composer.set_text("typed while creating");
    h.settle().await;
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["status"]["loading"], true);
    assert_eq!(projected(&mut b)["status"]["loading"], false);
    assert_eq!(projected(&mut c)["status"]["loading"], false);
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s3"})).await;
    assert_eq!(
        ack_next(&mut h, &mut b).await["error"],
        "Another device is opening a session"
    );
    request_from(&mut h, 3, json!({"kind":"submit", "text":"another draft"})).await;
    assert_eq!(
        ack_next(&mut h, &mut c).await["error"],
        "Another device is opening a session"
    );
    request(&mut h, json!({"kind":"submit", "text":"again"})).await;
    assert_eq!(
        ack_next(&mut h, &mut a).await["error"],
        "A session is opening or being created"
    );
    assert_eq!(
        h.app.remote.pending(1, &creating).unwrap().kind.text(),
        Some("my first message")
    );
    assert!(h.sends().is_empty());
    request_from(&mut h, 2, json!({"kind":"submit", "text":"live work"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.fake.release("session.create");
    h.until(|app| app.live("created").is_some_and(|session| session.is_busy()))
        .await;
    h.settle().await;
    assert_eq!(h.sends(), ["live work", "my first message"]);
    assert_eq!(h.calls("input.push")[1]["session_id"], "created");
    assert_eq!(h.app.draft_text, "typed while creating");
    assert_eq!(h.session().info.session_id, "s2");
    assert_eq!(h.calls("session.create").len(), 1);
    assert_eq!(h.calls("session.open").len(), 2);
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "created");
    assert_eq!(projected(&mut b)["session"]["id"], "s2");
    assert_eq!(projected(&mut c)["draft"]["reasoning"], "high");
}

#[tokio::test(start_paused = true)]
async fn a_pending_open_allows_live_work_but_only_its_owner_changes_view_on_adoption() {
    let (mut h, mut a, mut b) = separate_views().await;
    let mut third = session_info("s3");
    third["title"] = json!("Third");
    h.fake.script_held("session.open@s3", opened_session(third));
    let opening = request(&mut h, json!({"kind":"open_session", "session_id":"s3"})).await;
    called(&h, "session.open", 3).await;
    assert!(messages(&mut a).is_empty(), "open ACK waits for the Agent");
    request_from(
        &mut h,
        2,
        json!({"kind":"submit", "text":"work while opening"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.until(|app| app.live("s2").unwrap().is_busy()).await;
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["status"]["loading"], true);
    assert_eq!(projected(&mut b)["status"]["loading"], false);
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    assert_eq!(h.session().info.session_id, "s1");
    h.fake.release("session.open@s3");
    h.until(|app| app.live("s3").is_some()).await;
    h.settle().await;
    assert_eq!(ack_next(&mut h, &mut a).await["id"], opening);
    assert_eq!(h.session().info.session_id, "s1");
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s3");
    assert_eq!(projected(&mut b)["session"]["id"], "s1");
    assert_eq!(h.calls("input.push")[0]["session_id"], "s2");
}

#[tokio::test(start_paused = true)]
async fn deletion_refuses_any_viewer_but_removes_an_unviewed_idle_live_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"})).await;
    assert_eq!(
        ack_next(&mut h, &mut a).await["error"],
        "Open on \"second phone\""
    );
    request_from(
        &mut h,
        2,
        json!({"kind":"delete_session", "session_id":"s2"}),
    )
    .await;
    assert_eq!(
        ack_next(&mut h, &mut b).await["error"],
        "Open another session before deleting this one"
    );
    h.settle().await;
    assert!(h.calls("session.delete").is_empty());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"})).await;
    assert_eq!(ack_next(&mut h, &mut a).await["ok"], true);
    h.settle().await;
    assert!(h.app.live("s2").is_none());
    assert_eq!(
        h.calls("session.close"),
        [json!({"session_id":"s2", "load":"load_1"})]
    );
    assert_eq!(h.calls("session.delete"), [json!({"session_id":"s2"})]);
    assert!(
        h.app
            .remote
            .phones
            .iter()
            .all(|phone| phone.view.session_id() == Some("s1"))
    );
}

#[tokio::test(start_paused = true)]
async fn opening_a_session_while_its_deletion_is_pending_falls_back_to_a_default_draft() {
    let (mut h, mut a, mut b) = separate_views().await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.fake.script_held("session.delete@s2", json!({}));
    let deleting = request(&mut h, json!({"kind":"delete_session", "session_id":"s2"})).await;
    called(&h, "session.delete", 1).await;
    assert!(messages(&mut a).is_empty());
    h.settle().await;
    assert!(h.app.live("s2").is_some());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.fake.release("session.delete@s2");
    h.until(|app| app.live("s2").is_none()).await;
    assert_eq!(ack_next(&mut h, &mut a).await["id"], deleting);
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    let fallback = projected(&mut b);
    assert!(fallback["session"].is_null());
    assert_eq!(fallback["draft"]["model"], "mock");
    assert_eq!(fallback["draft"]["reasoning"], "medium");
    request_from(&mut h, 2, json!({"kind":"set_reasoning", "level":"high"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    assert!(h.calls("session.update").is_empty());
}

#[tokio::test(start_paused = true)]
async fn notices_follow_the_intent_or_session_and_attention_excludes_its_viewers() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.app.models.push(
        serde_json::from_value(json!({"id":"strict", "supported_reasoning":["high"]})).unwrap(),
    );
    let mut second = session_info("s2");
    second["title"] = json!("Second");
    second["model"] = json!("strict");
    second["reasoning"] = json!("high");
    h.fake
        .script("session.update@s2", json!({"session":second}));
    request_from(&mut h, 2, json!({"kind":"set_model", "model":"strict"})).await;
    assert!(messages(&mut a).is_empty());
    let own = messages(&mut b);
    assert_eq!(own[0]["t"], "notice");
    assert_eq!(
        own[0]["text"],
        "strict does not support reasoning medium; using high"
    );
    assert_eq!(own[1]["t"], "ack");
    h.settle().await;
    let epoch = h.app.live("s1").unwrap().epoch;
    h.app.on_msg(Msg::Session(
        epoch,
        Box::new(SessionOp::Operated(Err(crate::protocol::RpcError::new(
            -1,
            "test",
            "async problem",
        )))),
    ));
    assert_eq!(a.try_recv().unwrap()["text"], "async problem");
    assert_eq!(b.try_recv().unwrap()["text"], "\"First\": async problem");
    assert_eq!(h.notices().last().unwrap(), "\"First\": async problem");
    request(&mut h, json!({"kind":"cancel"})).await;
    messages(&mut a);
    h.app.on_msg(Msg::Session(
        epoch,
        Box::new(SessionOp::Operated(Err(crate::protocol::RpcError::new(
            -1,
            "test",
            "foreground problem",
        )))),
    ));
    assert_eq!(a.try_recv().unwrap()["text"], "foreground problem");
    assert_eq!(
        b.try_recv().unwrap()["text"],
        "\"First\": foreground problem"
    );
    request_from(&mut h, 2, json!({"kind":"cancel"})).await;
    messages(&mut b);
    let mut waiting =
        crate::testing::snapshot_running("s1", crate::testing::LOAD, 2, "loop-s1", Value::Null);
    waiting["pending_interaction"] = json!({"loop_id":"loop-s1", "request_index":0,
        "interaction_id":"approval-one", "tool_call_id":"call-one", "tool_name":"bash",
        "kind":{"type":"approval", "data":{}}});
    h.fake.script("session.state@s1", state_response(waiting));
    h.emit("interaction_requested", json!({"turn":{"session_id":"s1", "loop_id":"loop-s1"}, "interaction":{"interaction_id":"approval-one", "tool_call_id":"call-one", "tool_name":"bash", "kind":{"type":"approval", "data":{}}}})).await;
    h.settle().await;
    assert!(
        messages(&mut a).is_empty(),
        "a viewer does not get attention to its own session"
    );
    assert_eq!(
        b.try_recv().unwrap()["text"],
        "\"First\" needs approval",
        "the phone's text carries no TUI key hint"
    );
    h.app.on_msg(Msg::Notice(
        crate::app::NoticeLevel::Info,
        "tui only".into(),
    ));
    assert!(messages(&mut a).is_empty() && messages(&mut b).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_refused_push_returns_its_text_only_to_the_operation_that_sent_it() {
    let (mut h, mut a, mut b) = separate_views().await;
    let (mut c, _close_c) = connect_phone(&mut h, 3, "another viewer").await;
    // Two phones submit to the same session; the Agent rejects one push only.
    h.fake.script_sequence(
        "input.push@s1",
        vec![
            json!({"error":{"code":-32030,"message":"no","data":{"kind":"input_rejected","acceptance":"rejected"}}}),
            pushed(vec![]),
        ],
    );
    request(&mut h, json!({"kind":"submit", "text":"first"})).await;
    request_from(&mut h, 3, json!({"kind":"submit", "text":"second"})).await;
    let refused = ack_next(&mut h, &mut a).await;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["acceptance"], "rejected");
    assert!(
        messages(&mut a).is_empty(),
        "the original browser still owns its text"
    );
    assert!(
        messages(&mut b).is_empty(),
        "another device receives no input feedback"
    );
    // The accepted one is answered for its own operation and keeps its text in the session.
    let accepted = ack_next(&mut h, &mut c).await;
    assert_eq!(accepted["ok"], true);
    assert!(
        messages(&mut c)
            .iter()
            .all(|message| message["t"] != "returned")
    );
    h.action(Action::Remote("off".into()));
    // A text no operation owns stays with the session, not this TUI's composer.
    assert_eq!(h.app.composer.text(), "");
}

#[tokio::test(start_paused = true)]
async fn viewed_turn_ends_stay_seen_and_opening_a_session_clears_unseen() {
    let (mut h, mut a, mut b) = separate_views().await;
    let mut completions = Vec::new();
    for viewed in [true, false] {
        let loop_id = format!("loop-s1-{viewed}");
        let turn = json!({"session_id":"s1", "loop_id":loop_id});
        let revision = h.app.live("s1").unwrap().recovery.revision + 1;
        let mut push = started_in(turn.clone(), vec![]);
        push["id"] = json!(format!("input-{viewed}"));
        push["started"]["input"]["id"] = push["id"].clone();
        let mut running = running_snapshot(revision, &loop_id, "work");
        running["execution"]["input"]["id"] = push["id"].clone();
        if !completions.is_empty() {
            running["completions"] =
                finished_snapshot(revision, completions.clone())["completions"].clone();
        }
        h.fake
            .script("input.push@s1", after_snapshot(push, running));
        completions.push(completion(
            completions.len() as u64 + 1,
            &loop_id,
            "completed",
        ));
        h.fake.script_held(
            "turn.wait@s1",
            after_snapshot(
                json!({"turn":turn, "outcome":completed()}),
                finished_snapshot(revision + 1, completions.clone()),
            ),
        );
        let mut result = turn_result(&loop_id, vec![user(0, &loop_id, "work")]);
        result["outcome"] = completed();
        h.fake.script(&format!("turn.result@s1/{loop_id}"), result);
        request(&mut h, json!({"kind":"open_session", "session_id":"s1"})).await;
        request(&mut h, json!({"kind":"submit", "text":"work"})).await;
        h.until(|app| app.live("s1").unwrap().is_busy()).await;
        request_from(&mut h, 2, json!({"kind":"cancel"})).await;
        if !viewed {
            request(&mut h, json!({"kind":"open_session", "session_id":"s2"})).await;
        }
        messages(&mut a);
        messages(&mut b);
        h.fake.release("turn.wait@s1");
        h.until(|app| !app.live("s1").unwrap().is_busy()).await;
        h.settle().await;
        assert_eq!(h.app.live("s1").unwrap().unseen, !viewed);
        if viewed {
            assert!(
                !messages(&mut a)
                    .iter()
                    .any(|message| message["t"] == "notice")
            );
            assert!(messages(&mut b).iter().any(|message| {
                message["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("\"First\" finished in"))
            }));
        }
    }
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert!(!h.app.live("s1").unwrap().unseen);
}

#[tokio::test(start_paused = true)]
async fn identical_views_share_one_state_build_and_all_unchanged_views_send_nothing() {
    let (mut h, mut a, mut b) = separate_views().await;
    assert_eq!(crate::remote::view::Shared::build(&mut h.app).len(), 2);
    h.app.sync_remote();
    messages(&mut a);
    messages(&mut b);
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    h.app.sync_remote();
    assert!(messages(&mut a).is_empty() && messages(&mut b).is_empty());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"})).await;
    let groups = crate::remote::view::Shared::build(&mut h.app);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].connections, [1, 2]);
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    assert_eq!(projected(&mut b)["session"]["id"], "s1");
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    h.app.sync_remote();
    assert!(messages(&mut a).is_empty() && messages(&mut b).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_create_returns_only_to_its_creator_and_preserves_the_draft_settings() {
    let (mut h, mut a, mut b) = separate_views().await;
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    h.app
        .remote
        .config
        .as_mut()
        .unwrap()
        .roots
        .push(root.clone());
    request(
        &mut h,
        json!({"kind":"new_session", "workspace":root, "profile":"coding"}),
    )
    .await;
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"})).await;
    messages(&mut a);
    h.fake
        .script_error("session.create", -1, "cannot create", "test");
    request(&mut h, json!({"kind":"submit", "text":"keep this text"})).await;
    h.until(|app| !app.loading_session).await;
    let ack = ack_next(&mut h, &mut a).await;
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["error"], "cannot create");
    assert_eq!(ack["acceptance"], "rejected");
    assert!(
        messages(&mut a).is_empty(),
        "the original operation owns its unsent text"
    );
    assert!(messages(&mut b).is_empty());
    assert!(h.app.composer.is_empty());
    h.app.sync_remote();
    let draft = projected(&mut a);
    assert!(draft["session"].is_null());
    assert_eq!(
        draft["draft"]["workspace"],
        crate::remote::fs::display(&root)
    );
    assert_eq!(draft["draft"]["reasoning"], "high");
    assert_eq!(draft["status"]["loading"], false);
    assert_eq!(projected(&mut b)["session"]["id"], "s2");
}

#[tokio::test(start_paused = true)]
async fn model_and_reasoning_updates_persist_only_in_the_acting_devices_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.app.models.push(
        serde_json::from_value(
            json!({"id":"chosen", "supported_reasoning":["low", "medium", "high"]}),
        )
        .unwrap(),
    );
    let mut changed = session_info("s1");
    changed["title"] = json!("First");
    changed["model"] = json!("chosen");
    changed["reasoning"] = json!("high");
    h.fake
        .script("session.update@s1", json!({"session":changed}));
    request(&mut h, json!({"kind":"set_model", "model":"chosen"})).await;
    assert_eq!(ack_next(&mut h, &mut a).await["ok"], true);
    h.settle().await;
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"})).await;
    assert_eq!(ack_next(&mut h, &mut a).await["ok"], true);
    request_from(&mut h, 2, json!({"kind":"cancel"})).await;
    h.settle().await;
    let updates = h.calls("session.update");
    assert_eq!(updates.len(), 2);
    assert!(updates.iter().all(|update| update["session_id"] == "s1"));
    assert_eq!(updates[0]["model"], "chosen");
    assert_eq!(updates[1]["reasoning"], "high");
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["model"], "chosen");
    let other = projected(&mut b);
    assert_eq!(other["session"]["model"], "mock");
    assert_eq!(other["session"]["reasoning"], "medium");
}

#[tokio::test(start_paused = true)]
async fn failed_opens_keep_device_views_and_answer_only_the_original_operation() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.fake
        .script_error("session.open@s3", -1, "cannot open", "test");
    request(&mut h, json!({"kind":"open_session", "session_id":"s3"})).await;
    let ack = ack_next(&mut h, &mut a).await;
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["error"], "cannot open");
    h.until(|app| !app.loading_session).await;
    assert!(a.try_recv().is_err());
    assert!(b.try_recv().is_err());
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    assert_eq!(projected(&mut b)["session"]["id"], "s2");
    assert!(h.app.remote.opening.is_none());
}

#[tokio::test(start_paused = true)]
async fn a_deletion_finishing_after_remote_off_keeps_the_local_draft_editable() {
    let (mut h, mut a, mut b) = separate_views().await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.fake.script_held("session.delete@s2", json!({}));
    let deleting = request(&mut h, json!({"kind":"delete_session", "session_id":"s2"})).await;
    called(&h, "session.delete", 1).await;
    assert!(h.app.remote.pending(1, &deleting).is_some());
    assert!(messages(&mut a).is_empty());
    h.settle().await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.action(Action::Remote("off".into()));
    h.fake.release("session.delete@s2");
    h.until(|app| app.live("s2").is_none()).await;
    h.app.set_reasoning(crate::protocol::Reasoning::High);
    assert!(h.app.resident.current().is_none());
    assert_eq!(h.app.reasoning(), Some(crate::protocol::Reasoning::High));
}

#[tokio::test(start_paused = true)]
async fn a_late_open_result_cannot_bring_back_a_session_deleted_by_another_device() {
    let (mut h, mut a, mut b) = separate_views().await;
    let mut third = session_info("s3");
    third["title"] = json!("Third");
    h.fake.script_held("session.open@s3", opened_session(third));
    let opening = request(&mut h, json!({"kind":"open_session", "session_id":"s3"})).await;
    called(&h, "session.open", 3).await;
    assert!(messages(&mut a).is_empty());
    request_from(
        &mut h,
        2,
        json!({"kind":"delete_session", "session_id":"s3"}),
    )
    .await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.until(|app| !app.sessions.iter().any(|info| info.session_id == "s3"))
        .await;
    h.fake.release("session.open@s3");
    h.until(|app| !app.loading_session).await;
    let ack = ack_next(&mut h, &mut a).await;
    assert_eq!(ack["id"], opening);
    assert_eq!(ack["ok"], false);
    assert!(h.app.live("s3").is_none());
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    assert_eq!(projected(&mut b)["session"]["id"], "s2");
}

#[tokio::test(start_paused = true)]
async fn context_and_files_queries_read_the_requesting_devices_session_and_map_the_reply() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.fake.script(
        "session.context@s2",
        json!({"session_id":"s2", "current_operation":null,
               "coverage":{"covered_loop_count":6, "covered_item_count":12, "retained_item_count":3},
               "last_result":{"operation_id":"tui-1", "status":"compacted",
                              "before_tokens":4200, "after_tokens":900},
               "budget":{"estimated_history_tokens":370, "input_budget_tokens":40000,
                         "trigger_tokens":32000, "target_tokens":16000},
               "automatic":{"current":null, "last":null}, "last_prepare_failure":null}),
    );
    h.fake.script(
        "workspace.files@s1",
        json!({"entries":[{"path":"src/main.rs", "kind":"file", "size":1}, {"path":"src", "kind":"directory"}],
               "truncated":true, "scan_complete":false}),
    );
    live_mut(&mut h, "s2").presentation = Some(
        serde_json::from_value(json!({"session_id":"s2",
        "context":{"tokens":18000, "window":64000, "percent":28.1, "kind":"reported"}}))
        .unwrap(),
    );
    messages(&mut a);
    messages(&mut b);
    query_from(&mut h, 2, "c", json!({"kind":"context"}));
    query_from(&mut h, 1, "f", json!({"kind":"files", "query":"main"}));
    h.settle().await;
    assert_eq!(
        h.calls("session.context").last().unwrap()["session_id"],
        "s2"
    );
    assert_eq!(
        results(&mut b),
        [json!({"id":"c", "ok":true, "data":{
            "tokens":18000, "window":64000, "percent":45.0, "budget":40000, "history_tokens":370,
            "trigger_tokens":32000, "target_tokens":16000, "compacting":false,
            "coverage":{"turns":6, "items":12, "kept":3},
            "last_compaction":{"status":"compacted", "before":4200, "after":900},
            "failure":null}})]
    );
    assert_eq!(
        h.calls("workspace.files").last().unwrap(),
        &json!({"session_id":"s1", "recursive":true, "query":"main", "limit":50, "max_bytes":262_144})
    );
    assert_eq!(
        results(&mut a),
        [json!({"id":"f", "ok":true, "data":{"paths":["src/main.rs"], "truncated":true}})]
    );

    let mut draft = Harness::new();
    draft.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing) = connect_phone(&mut draft, 1, "phone").await;
    query_from(&mut draft, 1, "draft", json!({"kind":"context"}));
    assert_eq!(
        results(&mut output),
        [json!({"id":"draft", "ok":false, "error":"No session"})]
    );
}

#[tokio::test(start_paused = true)]
async fn changes_and_diff_queries_list_the_workspace_like_the_diff_view_and_refuse_unlisted_paths()
{
    let (mut h, mut a, _b) = separate_views().await;
    h.fake.script(
        "changes.list@s1",
        json!({"records":[
            {"change_ref":"workspace:a", "path":"src/a.rs", "kind":"modified", "details_available":true},
            {"change_ref":"workspace:b", "path":"new.txt", "kind":"added", "details_available":true},
            {"change_ref":"workspace:c", "path":"src/a.rs", "kind":"modified", "details_available":true}],
            "next_cursor":{"offset":3}, "total":4, "warnings":[]}),
    );
    h.fake.script(
        "changes.diff@s1",
        json!({"hunks":[{"old_start":0, "old_count":1, "new_start":0, "new_count":2, "lines":[
                {"kind":"removed", "old_index":0, "text":"old\n", "line_complete":true},
                {"kind":"added", "new_index":0, "text":"new\n", "line_complete":true},
                {"kind":"added", "new_index":1, "text":"more\n", "line_complete":true}]}],
               "next_cursor":null, "complete":true, "binary":false, "availability":"available",
               "stale":false, "truncated":false}),
    );
    live_mut(&mut h, "s1").presentation = Some(
        serde_json::from_value(json!({"session_id":"s1", "git_branch":"feat/parser"})).unwrap(),
    );
    messages(&mut a);
    query_from(&mut h, 1, "changes", json!({"kind":"changes"}));
    h.settle().await;
    let lists = h.calls("changes.list");
    assert_eq!(
        lists.last().unwrap(),
        &json!({"session_id":"s1", "scope":"workspace", "limit":200, "max_bytes":262_144})
    );
    assert_eq!(
        results(&mut a),
        [
            json!({"id":"changes", "ok":true, "data":{"branch":"feat/parser", "files":[
            {"path":"src/a.rs", "status":"modified"}, {"path":"new.txt", "status":"added"}],
            "truncated":true}})
        ]
    );
    query_from(&mut h, 1, "diff", json!({"kind":"diff", "path":"new.txt"}));
    query_from(
        &mut h,
        1,
        "gone",
        json!({"kind":"diff", "path":"README.md"}),
    );
    h.settle().await;
    assert_eq!(h.calls("changes.list").len(), lists.len() + 2);
    let diffs = h.calls("changes.diff");
    assert_eq!(diffs.len(), 1);
    assert_eq!(
        (&diffs[0]["session_id"], &diffs[0]["change_ref"]),
        (&json!("s1"), &json!("workspace:b"))
    );
    let mut answers = results(&mut a);
    answers.sort_by_key(|answer| answer["id"].as_str().unwrap().to_owned());
    assert_eq!(
        answers,
        [
            json!({"id":"diff", "ok":true, "data":{"path":"new.txt", "added":2, "removed":1, "truncated":false,
                "lines":["@@ -1,1 +1,2 @@", "-old", "+new", "+more"]}}),
            json!({"id":"gone", "ok":false, "error":"stale"}),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn tool_output_pages_a_call_of_the_viewed_transcript_and_another_sessions_call_is_stale() {
    let (mut h, mut a, mut b) = separate_views().await;
    let call = crate::transcript::ToolCall::new(
        crate::protocol::ToolRef {
            session_id: "s1".into(),
            loop_id: "loop-s1".into(),
            request_index: 2,
            tool_call_id: "call-1".into(),
        },
        "bash",
    );
    live_mut(&mut h, "s1")
        .transcript
        .push(crate::transcript::Block::Tool(Box::new(call)));
    h.fake.script(
        "tool.output",
        json!({"tool_ref":{"session_id":"s1", "loop_id":"loop-s1", "request_index":2, "tool_call_id":"call-1"},
               "stream":"output", "encoding":"utf8", "base_offset":10, "next_offset":25, "observed_end":40,
               "eof":false, "truncated":false, "availability":"available", "data":"hello output\n"}),
    );
    messages(&mut a);
    messages(&mut b);
    query_from(
        &mut h,
        1,
        "out",
        json!({"kind":"tool_output", "call_id":"call-1", "offset":10}),
    );
    query_from(
        &mut h,
        2,
        "other",
        json!({"kind":"tool_output", "call_id":"call-1", "offset":0}),
    );
    h.settle().await;
    assert_eq!(
        h.calls("tool.output"),
        [
            json!({"session_id":"s1", "loop_id":"loop-s1", "request_index":2, "tool_call_id":"call-1",
                "stream":"output", "offset":10, "max_bytes":65_536})
        ]
    );
    assert_eq!(
        results(&mut a),
        [
            json!({"id":"out", "ok":true, "data":{"text":"hello output\n", "offset":10, "next_offset":25,
                "eof":false, "truncated":false}})
        ]
    );
    assert_eq!(
        results(&mut b),
        [json!({"id":"other", "ok":false, "error":"stale"})]
    );
}

#[tokio::test(start_paused = true)]
async fn a_fifth_query_in_flight_is_busy_and_a_reply_after_the_phone_left_is_dropped() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut first, _close) = connect_phone(&mut h, 1, "first").await;
    let (mut second, _close2) = connect_phone(&mut h, 2, "second").await;
    h.fake
        .script_held("session.context", json!({"session_id":"s1"}));
    for id in ["1", "2", "3", "4", "5"] {
        query_from(&mut h, 1, id, json!({"kind":"context"}));
    }
    query_from(&mut h, 2, "left", json!({"kind":"context"}));
    h.settle().await;
    assert_eq!(
        results(&mut first),
        [json!({"id":"5", "ok":false, "error":"busy"})]
    );
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(2)));
    h.fake.release("session.context");
    h.settle().await;
    let answered = results(&mut first);
    assert_eq!(answered.len(), 4);
    assert!(answered.iter().all(|answer| answer["ok"] == true));
    assert!(results(&mut second).is_empty());
    assert_eq!(h.app.remote.phones[0].queries, 0);
    query_from(&mut h, 1, "again", json!({"kind":"context"}));
    h.settle().await;
    assert_eq!(results(&mut first)[0]["ok"], true);
}

#[tokio::test(start_paused = true)]
async fn an_open_reply_for_another_known_session_never_adopts_or_refocuses_it() {
    let (mut h, mut output) = connected_session().await;
    for id in ["wanted", "other"] {
        let mut info = session_info(id);
        info["workspace"] = json!(h.app.workspace);
        h.app.sessions.push(serde_json::from_value(info).unwrap());
    }
    let mut other = session_info("other");
    other["workspace"] = json!(h.app.workspace);
    h.fake.script(
        "session.open@wanted",
        json!({"session":other, "load":"other_load",
        "snapshot":snapshot("other", "other_load", 1)}),
    );
    let id = request(
        &mut h,
        json!({"kind":"open_session", "session_id":"wanted"}),
    )
    .await;
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], id);
    assert_eq!(ack["ok"], false);
    assert!(h.app.live("other").is_none());
    assert_eq!(h.session().info.session_id, "s1");
    assert_eq!(h.app.remote.phones[0].view.session_id(), Some("s1"));
    assert_eq!(h.calls("session.open")[0]["session_id"], "wanted");
}

#[tokio::test(start_paused = true)]
async fn a_stale_draft_create_sends_to_its_created_session_without_refocusing_the_phone() {
    let (mut h, mut output) = connected_session().await;
    request(&mut h, json!({"kind":"new_session"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.app.sync_remote();
    messages(&mut output);
    let mut info = session_info("created");
    info["workspace"] = json!(h.app.workspace);
    h.fake.script_held(
        "session.create",
        json!({"session":info, "load":"created_load",
        "snapshot":snapshot("created", "created_load", 1)}),
    );
    let mut receipt = pushed(vec![]);
    receipt["session_id"] = json!("created");
    receipt["load"] = json!("created_load");
    h.fake.script(
        "input.push",
        after_snapshot(receipt, snapshot("created", "created_load", 2)),
    );
    let create = request(&mut h, json!({"kind":"submit", "text":"original"})).await;
    called(&h, "session.create", 1).await;
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.app.sync_remote();
    messages(&mut output);
    h.fake.release("session.create");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], create);
    assert_eq!(ack["session_id"], "created");
    assert_eq!(ack["load"], "created_load");
    assert_eq!(h.app.remote.phones[0].view.session_id(), Some("s1"));
    assert_eq!(h.session().info.session_id, "s1");
    assert_eq!(h.calls("input.push")[0]["session_id"], "created");
    assert_eq!(h.calls("input.push")[0]["load"], "created_load");
    assert_eq!(h.calls("input.push")[0]["text"], "original");
}
