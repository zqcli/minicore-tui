use super::*;
use crate::remote::auth::Device;

/// Types `command` and presses Enter, as at the keyboard.
fn command(h: &mut Harness, command: &str) {
    h.app.composer.set_text(command);
    h.press(KeyCode::Enter);
}

/// Remote control listening with a password file in `dir`.
fn listening(h: &mut Harness, dir: &tempfile::TempDir) -> Arc<Mutex<Auth>> {
    let auth = Arc::new(Mutex::new(
        Auth::load(dir.path().join("remote.json")).unwrap(),
    ));
    h.app.remote.auth = Some(auth.clone());
    h.app.remote.url = Some("http://localhost:8765".into());
    auth
}

/// The connected device names, oldest first.
fn devices(h: &Harness) -> Vec<&str> {
    let phones = &h.app.remote.phones;
    phones
        .iter()
        .map(|p| p.connection.device.name.as_str())
        .collect()
}

/// The first frame of type `t` within a bounded number of frames and time.
async fn frame_of_type<S>(socket: &mut S, t: &str) -> Value
where
    S: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures_util::StreamExt;
    for _ in 0..100 {
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .unwrap_or_else(|_| panic!("no {t} frame within 5s"));
        let reply: Value = serde_json::from_str(next.unwrap().unwrap().to_text().unwrap()).unwrap();
        if reply["t"] == t {
            return reply;
        }
    }
    panic!("no {t} frame within 100 frames");
}

#[tokio::test(start_paused = true)]
async fn a_real_phone_signs_in_submits_through_the_agent_and_receives_the_user_block() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let mut h = Harness::opened(display_read(0, 0)).await;
    // Harness quiet periods use virtual time; real socket deadlines start after setup.
    tokio::time::resume();
    h.fake.script(
        "session.state@s1",
        state_response(snapshot("s1", crate::testing::LOAD, 1)),
    );
    h.fake.script_held("input.push", starting_reply("l1", 2));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path().canonicalize().unwrap());
    let bound = Bound::bind(&config, root.path().join("remote.json"))
        .await
        .unwrap();
    let address = bound.listening[0];
    let password = bound.auth.lock().unwrap().password().to_owned();
    h.app.remote.config = Some(config);
    h.app.enter_remote(bound);
    h.app.composer.set_text("untouched");
    let reconnect_password = password.clone();
    let client = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut socket, _) = tokio_tungstenite::client_async(format!("ws://{address}/ws"), stream)
            .await
            .unwrap();
        socket
            .send(Message::Text(
                json!({"t":"login", "v":crate::remote::wire::VERSION, "password":password, "device":"test phone"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let mut snapshot = Value::Null;
        for kind in ["authorized", "welcome", "snapshot"] {
            let reply = frame_of_type(&mut socket, kind).await;
            assert_eq!(reply["t"], kind);
            if kind == "snapshot" {
                snapshot = reply;
            }
        }
        socket
            .send(Message::Text(
                json!({"t":"intent", "id":"send", "kind":"submit", "text":"real phone text",
                    "view_epoch":snapshot["view_epoch"], "session_id":snapshot["state"]["session"]["id"],
                    "load":snapshot["state"]["session"]["load"]})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let ack = frame_of_type(&mut socket, "ack").await;
        assert_eq!(ack["id"], "send");
        assert_eq!(ack["accepted"], true);
        assert_eq!(ack["input_id"], "i1");
        let reply = frame_of_type(&mut socket, "blocks").await;
        assert_eq!(reply["upserts"][0]["block"]["text"], "real phone text");
        socket
    });
    h.until(|app| !app.remote.phones.is_empty()).await;
    h.app.sync_remote();
    h.until(|app| {
        app.remote
            .pending(app.remote.phones[0].connection.id, "send")
            .is_some()
    })
    .await;
    called(&h, "input.push", 1).await;
    h.fake.release("input.push");
    h.until(|app| {
        app.is_busy()
            && app
                .remote
                .pending(app.remote.phones[0].connection.id, "send")
                .is_none()
    })
    .await;
    h.app.sync_remote();
    let mut socket = tokio::time::timeout(std::time::Duration::from_secs(5), client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.sends(), ["real phone text"]);
    assert_eq!(h.app.composer.text(), "untouched");
    let original = h.app.remote.phones[0].connection.id;
    for _ in 0..=512 {
        h.app
            .remote
            .send(Some(original), json!({"t":"notice", "text":"slow link"}));
    }
    assert!(h.app.remote.phones.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..1024 {
            if matches!(socket.next().await, Some(Ok(Message::Close(_)))) {
                return;
            }
        }
        panic!("overflow did not close the WebSocket");
    })
    .await
    .expect("overflow close timed out");
    h.session_mut()
        .transcript
        .push(crate::transcript::Block::User {
            text: "while disconnected".into(),
            kind: crate::protocol::read::UserKind::Prompt,
            at: None,
        });
    let reconnect = tokio::spawn(async move {
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut socket, _) = tokio_tungstenite::client_async(format!("ws://{address}/ws"), stream)
            .await
            .unwrap();
        socket
            .send(Message::Text(
                json!({"t":"login", "v":3, "password":reconnect_password,
            "device":"reconnected phone"})
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let snapshot = frame_of_type(&mut socket, "snapshot").await;
        assert_eq!(
            snapshot["blocks"].as_array().unwrap().last().unwrap()["text"],
            "while disconnected"
        );
        assert_eq!(snapshot["state"]["session"]["load"], "load_1");
        socket
    });
    h.until(|app| !app.remote.phones.is_empty()).await;
    h.app.sync_remote();
    let mut socket = tokio::time::timeout(std::time::Duration::from_secs(5), reconnect)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(h.app.remote.phones[0].connection.id, original);
    assert_eq!(
        h.sends(),
        ["real phone text"],
        "reconnection never resends input"
    );
    h.action(Action::Remote("off".into()));
    let reply = frame_of_type(&mut socket, "bye").await;
    assert_eq!(reply["reason"], "remote_off");
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .unwrap(),
        Some(Ok(Message::Close(_)))
    ));
}

#[tokio::test(start_paused = true)]
async fn a_full_outbound_queue_drops_the_only_phone_and_returns_to_waiting() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_output, closing) = connect_phone(&mut h, 1, "phone").await;
    for _ in 0..=512 {
        h.app
            .remote
            .send(None, json!({"t":"notice", "text":"bounded"}));
    }
    assert_eq!(*closing.borrow(), Some("overflow"));
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
    assert!(h.app.remote.phones.is_empty());
}

#[tokio::test(start_paused = true)]
async fn entering_remote_names_each_configured_address_it_cannot_listen_on() {
    let mut h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(dir.path().canonicalize().unwrap());
    config.bind.push("192.0.2.1:0".parse().unwrap());
    let bound = Bound::bind(&config, dir.path().join("remote.json"))
        .await
        .unwrap();
    let reason = bound.skipped[0].1.clone();
    h.app.remote.config = Some(config);
    h.app.enter_remote(bound);
    assert_eq!(
        h.notices(),
        [format!("Remote: not listening on 192.0.2.1:0 ({reason})")]
    );
    assert_eq!(h.app.remote.listening.len(), 1);
    h.action(Action::Remote("off".into()));
}

#[tokio::test(start_paused = true)]
async fn entering_remote_recalls_the_queue_and_an_agent_refusal_returns_only_to_the_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.mirror(vec![entry("i1", "follow_up", "queued")], false);
    h.fake.script(
        "input.recall",
        after_snapshot(
            json!({"texts": ["queued"], "queue": queue(vec![], false)}),
            h.control(json!({"queue":queue(vec![], false)})),
        ),
    );
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().canonicalize().unwrap());
    let bound = Bound::bind(&config, dir.path().join("remote.json"))
        .await
        .unwrap();
    h.app.remote.config = Some(config);
    h.app.enter_remote(bound);
    h.settle().await;
    assert_eq!(h.app.composer.text(), "queued");
    assert!(h.queue().is_empty());
    let (mut output, _close) = connect_phone(&mut h, 1, "phone").await;
    // A refused push is answered to the operation that sent it, in its own terms.
    h.fake.script_error_with(
        "input.push",
        -32031,
        "refused",
        "input_rejected",
        Some("rejected"),
    );
    let intent = phone_intent(
        &mut h,
        1,
        json!({"kind":"submit", "text":"remote rejected"}),
    );
    request(&mut h, intent).await;
    h.settle().await;
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["t"], "ack");
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["acceptance"], "rejected");
    assert_eq!(h.app.composer.text(), "queued");
    h.action(Action::Remote("off".into()));
}

#[tokio::test(start_paused = true)]
async fn a_device_replaces_only_its_own_connection_and_disconnecting_one_keeps_the_others() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_out, old) = connect_phone(&mut h, 1, "first").await;
    let (_out2, second) = connect_phone(&mut h, 2, "second").await;
    let (_out3, first) = connect_phone(&mut h, 3, "first").await;
    assert_eq!(*old.borrow(), Some("replaced"));
    assert_eq!(*second.borrow(), None);
    assert_eq!(devices(&h), ["second", "first"]);
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(2)));
    assert_eq!(devices(&h), ["first"]);
    assert_eq!(h.app.remote.mode(), RemoteMode::Connected);
    let (_out4, other) = connect_phone(&mut h, 4, "other").await;
    h.action(Action::Remote("off".into()));
    assert_eq!(*first.borrow(), Some("remote_off"));
    assert_eq!(*other.borrow(), Some("remote_off"));
    assert_eq!(h.app.remote.mode(), RemoteMode::Off);
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_out5, _close5) = connect_phone(&mut h, 5, "first").await;
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(5)));
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
}

#[tokio::test(start_paused = true)]
async fn every_phone_gets_its_snapshot_then_the_changes_and_sees_who_is_connected() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    enable_remote(&mut h);
    let (mut a, _close) = connect_phone(&mut h, 1, "iPhone Safari").await;
    let (mut b, _close2) = connect_phone(&mut h, 2, "Mac Edge").await;
    h.app.sync_remote();
    for output in [&mut a, &mut b] {
        let snapshot = output.try_recv().unwrap();
        assert_eq!(snapshot["t"], "snapshot");
        assert_eq!(
            snapshot["state"]["devices"],
            json!([{"name":"iPhone Safari", "session":"s1"}, {"name":"Mac Edge", "session":"s1"}])
        );
    }
    let session = h.session_mut();
    session.transcript.push(crate::transcript::Block::User {
        text: "hello".into(),
        kind: crate::protocol::read::UserKind::Prompt,
        at: None,
    });
    let len = session.transcript.len();
    h.app.sync_remote();
    for output in [&mut a, &mut b] {
        let blocks = output.try_recv().unwrap();
        assert_eq!(blocks["len"], len);
        assert_eq!(blocks["upserts"][0]["block"]["text"], "hello");
        received(output);
    }
    let (mut c, _close3) = connect_phone(&mut h, 3, "iPad Safari").await;
    h.app.sync_remote();
    assert_eq!(received(&mut c), ["snapshot"]);
    for output in [&mut a, &mut b] {
        let state = output.try_recv().unwrap();
        assert_eq!(state["t"], "state");
        assert_eq!(
            state["state"]["devices"],
            json!([{"name":"iPhone Safari", "session":"s1"}, {"name":"Mac Edge", "session":"s1"}, {"name":"iPad Safari", "session":"s1"}])
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_intent_is_acked_only_to_its_sender_and_only_a_sign_in_attempt_reaches_every_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close) = connect_phone(&mut h, 1, "first").await;
    let (mut b, _close2) = connect_phone(&mut h, 2, "second").await;
    request_from(&mut h, 2, json!({"kind":"cancel"})).await;
    assert_eq!(received(&mut a), Vec::<Value>::new());
    assert_eq!(received(&mut b), ["ack"]);
    h.app.notice(crate::app::NoticeLevel::Info, "TUI only");
    assert_eq!(received(&mut a), Vec::<Value>::new());
    h.app
        .on_msg(Msg::Remote(RemoteEvent::LoginRefused { locked: None }));
    assert_eq!(received(&mut a), ["notice"]);
    assert_eq!(received(&mut b), ["notice"]);
}

#[tokio::test(start_paused = true)]
async fn a_phone_whose_queue_overflows_is_dropped_alone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    enable_remote(&mut h);
    let (_stalled, closing) = connect_phone(&mut h, 1, "stalled").await;
    let (mut other, open) = connect_phone(&mut h, 2, "other").await;
    for _ in 0..=512 {
        h.app
            .remote
            .send(Some(1), json!({"t":"notice", "text":"bounded"}));
    }
    assert_eq!(*closing.borrow(), Some("overflow"));
    assert_eq!(*open.borrow(), None);
    assert_eq!(devices(&h), ["other"]);
    assert_eq!(h.app.remote.mode(), RemoteMode::Connected);
    h.app.sync_remote();
    let snapshot = other.try_recv().unwrap();
    assert_eq!(
        snapshot["state"]["devices"],
        json!([{"name":"other", "session":"s1"}])
    );
}

#[tokio::test(start_paused = true)]
async fn a_handshake_from_a_stopped_listener_cannot_take_control_after_restart() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    h.app.remote.generation = 2;
    let (outbound, _output) = mpsc::channel(512);
    let (close, _closing) = watch::channel(None);
    let (accepted, admission) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Connect {
        connection: Connection {
            id: 9,
            generation: 1,
            device: Device {
                name: "old phone".into(),
                token_sha256: "old".into(),
                paired_at: String::new(),
            },
            outbound,
            close,
        },
        accepted,
    }));
    assert!(!admission.await.unwrap());
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
    assert!(h.app.remote.phones.is_empty());
}

#[tokio::test(start_paused = true)]
async fn remote_approval_keeps_the_local_keyboard_and_denied_commands_keep_the_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    h.session_mut().interaction = Some(crate::app::PendingInteraction {
        turn: crate::protocol::TurnRef::new("s1", "l1"),
        interaction_id: "approval".into(),
        tool_call_id: "call".into(),
        tool_name: "bash".into(),
        prompt: "Approve".into(),
        kind: crate::app::InteractionKind::Approval,
        selected: 0,
    });
    h.type_text("y");
    assert_eq!(h.app.composer.text(), "y");
    assert!(h.app.has_interaction());
    assert!(crate::ui::sheets::sheet(&h.app, h.app.styles).is_none());
    for command in ["/new", "/model", "/compact"] {
        h.app.composer.set_text(command);
        h.app.overlay = Overlay::None;
        h.press(KeyCode::Enter);
        assert_eq!(h.app.composer.text(), command);
        assert!(h.app.resident.current().is_some());
    }
}

#[tokio::test(start_paused = true)]
async fn remote_off_restores_local_submit_and_quit_requires_two_requests_within_three_seconds() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    h.app.overlay = Overlay::None;
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit);
    assert_eq!(
        h.app.flash.as_ref().unwrap().0,
        "Remote control is on · quit again to end it"
    );
    tokio::time::advance(std::time::Duration::from_secs(4)).await;
    h.action(Action::Quit);
    assert!(!h.app.quit);
    h.action(Action::Quit);
    assert!(h.app.quit);
    h.app.quit = false;
    h.action(Action::Remote("off".into()));
    h.fake.script("input.push", pushed(vec![]));
    h.submit("local again");
    h.settle().await;
    assert_eq!(h.sends(), ["local again"]);
}

#[tokio::test(start_paused = true)]
async fn a_new_password_signs_every_phone_out_and_clears_the_failures() {
    let mut h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let auth = listening(&mut h, &dir);
    let password = auth.lock().unwrap().password().to_owned();
    let (device, token) = auth
        .lock()
        .unwrap()
        .prepare_login(&password, "phone".into())
        .unwrap();
    auth.lock().unwrap().remember(device).unwrap();
    let (_output, closing) = connect_phone(&mut h, 1, "phone").await;
    let (_output2, other) = connect_phone(&mut h, 2, "other").await;
    h.app
        .on_msg(Msg::Remote(RemoteEvent::LoginRefused { locked: None }));
    command(&mut h, "/remote password correct horse");
    assert_eq!(*closing.borrow(), Some("signed_out"));
    assert_eq!(*other.borrow(), Some("signed_out"));
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
    assert_eq!(h.app.remote.failed_logins, 0);
    assert_eq!(h.app.remote.password().as_deref(), Some("correct horse"));
    assert!(auth.lock().unwrap().verify(&token).is_none());
    assert_eq!(
        h.notices(),
        ["New remote password · phones must sign in again"]
    );
    command(&mut h, "/remote password");
    assert_ne!(h.app.remote.password().as_deref(), Some("correct horse"));
}

#[tokio::test(start_paused = true)]
async fn a_short_password_is_refused_and_changes_nothing() {
    let mut h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    listening(&mut h, &dir);
    let before = h.app.remote.password();
    let (_output, closing) = connect_phone(&mut h, 1, "phone").await;
    command(&mut h, "/remote password a-b c");
    assert_eq!(h.app.remote.password(), before);
    assert_eq!(*closing.borrow(), None);
    assert_eq!(h.app.remote.mode(), RemoteMode::Connected);
    assert_eq!(
        h.notices(),
        ["Remote: A remote password needs at least 8 characters"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_password_command_never_reaches_the_prompt_history() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", starting_reply("l1", 2));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("earlier prompt");
    h.settle().await;
    let dir = tempfile::tempdir().unwrap();
    listening(&mut h, &dir);
    command(&mut h, "/remote password correct horse");
    assert_eq!(h.app.remote.password().as_deref(), Some("correct horse"));
    h.press(KeyCode::Up);
    assert_eq!(h.app.composer.text(), "earlier prompt");
}

#[tokio::test(start_paused = true)]
async fn remote_status_while_off_says_how_to_start() {
    let mut h = Harness::new();
    command(&mut h, "/remote");
    assert_eq!(
        h.notices(),
        ["Remote control is off · /remote on starts it"]
    );
    assert_eq!(h.app.remote.mode(), RemoteMode::Off);
}

#[tokio::test(start_paused = true)]
async fn refused_sign_ins_are_counted_and_announced() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    h.app
        .on_msg(Msg::Remote(RemoteEvent::LoginRefused { locked: None }));
    assert_eq!(h.notices(), ["Remote: wrong password"]);
    h.app.on_msg(Msg::Remote(RemoteEvent::LoginRefused {
        locked: Some(std::time::Duration::from_secs(30)),
    }));
    assert_eq!(h.app.remote.failed_logins, 2);
    assert_eq!(
        h.app.remote.paused_until,
        Some(tokio::time::Instant::now() + std::time::Duration::from_secs(30))
    );
    assert_eq!(
        h.notices().last().unwrap(),
        "Remote: sign-in paused for 30 s"
    );
}

#[tokio::test(start_paused = true)]
async fn the_status_screen_neither_scrolls_nor_selects() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    for kind in [
        MouseEventKind::ScrollUp,
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        let mouse = MouseEvent {
            kind,
            column: 5,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        assert!(!h.app.on_mouse(mouse), "{kind:?} redraws nothing");
    }
    assert!(h.app.press.is_none() && h.app.selection.is_none());
    assert_eq!(h.app.scroll, crate::app::Scroll::Follow);
}

#[tokio::test(start_paused = true)]
async fn snapshot_blocking_persists_across_phone_reconnect_and_keeps_the_question_actionable() {
    let (mut h, mut output) = connected_session().await;
    let blocked = h.control(json!({"block_reason":"persistence"}));
    h.snapshot_event(blocked.clone()).await;
    h.settle().await;
    h.app.sync_remote();
    let state = projected(&mut output);
    assert_eq!(state["status"]["block_reason"], "persistence");
    assert_eq!(
        state["status"]["label"],
        crate::protocol::BlockReason::Persistence.label()
    );
    assert_eq!(state["status"]["busy"], false);
    assert!(state["interaction"].is_null());
    assert_eq!(
        h.session().block_reason,
        Some(crate::protocol::BlockReason::Persistence)
    );
    assert!(messages(&mut output).is_empty());
    h.app.sync_remote();
    assert!(
        messages(&mut output).is_empty(),
        "idle blocking needs no repeated projection"
    );

    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    let (mut output, _closing) = connect_phone(&mut h, 2, "first").await;
    h.app.sync_remote();
    assert_eq!(projected(&mut output)["status"], state["status"]);

    h.fake
        .script_held("turn.wait", waited("blocked_loop", completed()));
    let waiting = h.control(json!({
        "block_reason":"internal",
        "execution":{"type":"running", "loop_id":"blocked_loop", "input":null,
            "request_index":0, "phase":"waiting_for_input"},
        "pending_interaction":{"loop_id":"blocked_loop", "request_index":0,
            "interaction_id":"still_valid", "tool_call_id":"call", "tool_name":"ask",
            "kind":{"type":"tool_input", "data":{"answer_kind":"text", "prompt":"Your answer?", "choices":[]}}}
    }));
    h.fake
        .script("session.state@s1", json!({"snapshot":waiting.clone()}));
    h.snapshot_event(waiting).await;
    h.settle().await;
    h.app.sync_remote();
    let waiting = projected(&mut output);
    assert_eq!(waiting["status"]["block_reason"], "internal");
    assert_eq!(
        waiting["status"]["label"],
        "Blocked: the Agent hit an internal error"
    );
    assert_eq!(waiting["status"]["busy"], true);
    assert_eq!(waiting["interaction"]["id"], "still_valid");
    assert_eq!(waiting["interaction"]["kind"], "text");
    assert_eq!(waiting["interaction"]["prompt"], "Your answer?");
    assert_eq!(waiting["interaction"]["options"], json!([]));

    h.fake.script_held("interaction.answer", json!({"ok":true}));
    let operation = request_from(
        &mut h,
        2,
        json!({"kind":"answer", "loop_id":"blocked_loop",
        "interaction_id":"still_valid", "choice":{"text":"yes"}}),
    )
    .await;
    called(&h, "interaction.answer", 1).await;
    assert!(messages(&mut output).is_empty());
    h.fake.release("interaction.answer");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], operation);
    assert_eq!(ack["ok"], true);
    assert_eq!(
        h.calls("interaction.answer")[0]["answer"],
        json!({"type":"text", "text":"yes"})
    );
    assert_eq!(
        h.session().block_reason,
        Some(crate::protocol::BlockReason::Internal)
    );
    assert_eq!(
        h.session().interaction.as_ref().unwrap().interaction_id,
        "still_valid"
    );

    h.snapshot_event(blocked).await;
    h.settle().await;
    h.app.sync_remote();
    let messages = messages(&mut output);
    assert!(
        messages.is_empty(),
        "an older control observation cannot replace the warning: {messages:?}"
    );
    h.control_event(json!({})).await;
    h.settle().await;
    h.app.sync_remote();
    let cleared = projected(&mut output);
    assert!(cleared["status"]["block_reason"].is_null());
    assert!(cleared["status"]["label"].is_null());
    assert!(cleared["interaction"].is_null());
}

#[tokio::test(start_paused = true)]
async fn real_websocket_reconnect_keeps_partial_saved_and_unavailable_bodies_and_live_provenance() {
    use crate::app::{recovery::RecoveryStatus, session::BodyRecovery};
    use crate::transcript::{Block, PreservedKey, RequestKey, Saving};
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut h = resident_session();
    text_control(&mut h, "question_loop", Some("still_actionable")).await;
    let session = h.session_mut();
    for (id, saving, status, message) in [
        (
            "partial",
            Saving::Saved,
            RecoveryStatus::Partial,
            "Item 1 is too large",
        ),
        (
            "missing",
            Saving::Unsaved,
            RecoveryStatus::Unavailable,
            "Result expired",
        ),
    ] {
        session.transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: id.into(),
                request_index: 0,
            },
            text: format!("received {id}"),
            reasoning: "temporary thinking".into(),
            streaming: true,
            expanded: None,
        });
        session.transcript.push(Block::TurnEnd {
            loop_id: id.into(),
            outcome: crate::protocol::LoopOutcome::Completed,
            duration: None,
            requests: 1,
            tools: 0,
            usage: Default::default(),
            changes: None,
            saving: Some(saving),
        });
        session.body_recovery.push_back(BodyRecovery {
            loop_id: id.into(),
            status,
            message: message.into(),
        });
    }
    session
        .transcript
        .mark_preserved([PreservedKey::AssistantReasoning(RequestKey {
            loop_id: "partial".into(),
            request_index: 0,
        })]);
    let dir = tempfile::tempdir().unwrap();
    let config = config(h.app.workspace.clone());
    let bound = Bound::bind(&config, dir.path().join("remote.json"))
        .await
        .unwrap();
    let address = bound.listening[0];
    let password = bound.auth.lock().unwrap().password().to_owned();
    h.app.remote.config = Some(config);
    h.app.enter_remote(bound);
    tokio::time::resume();
    crate::testing::real_bounded(async {
        let connect = |password: String| {
            tokio::spawn(async move {
                let stream = tokio::net::TcpStream::connect(address).await.unwrap();
                let (mut socket, _) =
                    tokio_tungstenite::client_async(format!("ws://{address}/ws"), stream)
                        .await
                        .unwrap();
                socket
                    .send(Message::Text(
                        json!({"t":"login", "v":3, "password":password, "device":"recovery-phone"})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                let snapshot = frame_of_type(&mut socket, "snapshot").await;
                (socket, snapshot)
            })
        };
        let pending = connect(password.clone());
        h.until(|app| !app.remote.phones.is_empty()).await;
        h.app.sync_remote();
        let (mut socket, snapshot) = pending.await.unwrap();
        assert_eq!(snapshot["state"]["interaction"]["id"], "still_actionable");
        assert_eq!(snapshot["blocks"][0]["text"], "received partial");
        assert_eq!(
            snapshot["blocks"][0]["preserved_channels"],
            json!(["reasoning"])
        );
        assert_eq!(snapshot["blocks"][1]["saving"], "saved");
        assert_eq!(
            snapshot["blocks"][1]["recovery"],
            json!({"status":"partial", "reason":"Item 1 is too large"})
        );
        assert_eq!(snapshot["blocks"][3]["saving"], "not saved");
        assert_eq!(
            snapshot["blocks"][3]["recovery"],
            json!({"status":"unavailable", "reason":"Result expired"})
        );
        let session = h.session_mut();
        let revisions = session.transcript.revisions().collect::<Vec<_>>();
        session.body_recovery[0].message = "Larger item still missing".into();
        session.body_recovery[1].message = "Only received fragments remain".into();
        session
            .transcript
            .mark_preserved([PreservedKey::Assistant(RequestKey {
                loop_id: "missing".into(),
                request_index: 0,
            })]);
        assert_eq!(
            session.transcript.revisions().collect::<Vec<_>>(),
            revisions,
            "metadata invalidation cannot rely on a body bump"
        );
        h.app.sync_remote();
        let update = frame_of_type(&mut socket, "blocks").await;
        assert_eq!(update["upserts"][0]["i"], 1);
        assert_eq!(
            update["upserts"][0]["block"]["recovery"]["reason"],
            "Larger item still missing"
        );
        assert_eq!(update["upserts"][1]["block"]["preserved"], true);
        assert_eq!(update["upserts"][1]["block"]["text"], "received missing");
        assert_eq!(
            update["upserts"][2]["block"]["recovery"]["status"],
            "unavailable"
        );
        socket.close(None).await.unwrap();
        h.until(|app| app.remote.phones.is_empty()).await;
        let pending = connect(password);
        h.until(|app| !app.remote.phones.is_empty()).await;
        h.app.sync_remote();
        let (mut socket, snapshot) = pending.await.unwrap();
        assert_eq!(
            snapshot["blocks"][1]["recovery"]["reason"],
            "Larger item still missing"
        );
        assert_eq!(snapshot["blocks"][1]["saving"], "saved");
        assert_eq!(snapshot["blocks"][2]["preserved"], true);
        assert_eq!(snapshot["blocks"][2]["text"], "received missing");
        assert_eq!(snapshot["blocks"][3]["recovery"]["status"], "unavailable");
        assert_eq!(
            snapshot["blocks"][3]["recovery"]["reason"],
            "Only received fragments remain"
        );
        assert_eq!(snapshot["state"]["interaction"]["id"], "still_actionable");
        h.app.on_action(Action::Remote("off".into()));
        assert_eq!(
            frame_of_type(&mut socket, "bye").await["reason"],
            "remote_off"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_real_text_phone_keeps_original_answer_identity_through_a_replacement() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut h = resident_session();
    text_control(&mut h, "text_loop", Some("original")).await;
    h.app.composer.set_text("conversation draft");
    h.fake.script_held("interaction.answer", json!({"ok":true}));
    let dir = tempfile::tempdir().unwrap();
    let config = config(h.app.workspace.clone());
    let bound = Bound::bind(&config, dir.path().join("remote.json"))
        .await
        .unwrap();
    let address = bound.listening[0];
    let password = bound.auth.lock().unwrap().password().to_owned();
    h.app.remote.config = Some(config);
    h.app.enter_remote(bound);
    tokio::time::resume();
    crate::testing::real_bounded(async {
        let connect = tokio::spawn(async move {
            let stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut socket, _) = tokio_tungstenite::client_async(format!("ws://{address}/ws"), stream).await.unwrap();
            socket.send(Message::Text(json!({"t":"login", "v":3, "password":password, "device":"text phone"}).to_string().into())).await.unwrap();
            let snapshot = frame_of_type(&mut socket, "snapshot").await;
            assert_eq!(snapshot["state"]["interaction"]["kind"], "text");
            assert_eq!(snapshot["state"]["interaction"]["options"], json!([]));
            let target = json!({"view_epoch":snapshot["view_epoch"], "session_id":"s1", "load":"load_1",
                "t":"intent", "kind":"answer", "loop_id":"text_loop", "interaction_id":"original",
                "id":"original_op", "choice":{"text":" allow 中文 "}});
            socket.send(Message::Text(target.to_string().into())).await.unwrap();
            (socket, target)
        });
        h.until(|app| !app.remote.phones.is_empty()).await;
        h.app.sync_remote();
        h.until(|app| app.remote.pending(app.remote.phones[0].connection.id, "original_op").is_some()).await;
        called(&h, "interaction.answer", 1).await;
        let (mut socket, mut target) = connect.await.unwrap();
        assert_eq!(h.calls("interaction.answer")[0]["answer"], json!({"type":"text", "text":" allow 中文 "}));
        let replacement = h.control(json!({"execution":{"type":"running", "loop_id":"text_loop", "input":null,
            "request_index":0, "phase":"waiting_for_input"}, "pending_interaction":{"loop_id":"text_loop", "request_index":0,
            "interaction_id":"replacement", "tool_call_id":"call2", "tool_name":"ask",
            "kind":{"type":"tool_input", "data":{"answer_kind":"text", "prompt":"Replacement?", "choices":[]}}}}));
        h.app.apply_snapshot(serde_json::from_value(replacement.clone()).unwrap());
        h.fake.script("session.state@s1", json!({"snapshot":replacement}));
        h.fake.release("interaction.answer");
        h.until(|app| app.remote.pending(app.remote.phones[0].connection.id, "original_op").is_none()).await;
        h.app.sync_remote();
        let ack = frame_of_type(&mut socket, "ack").await;
        assert_eq!(ack["id"], "original_op");
        assert_eq!(ack["ok"], true);
        assert_eq!(h.session().interaction.as_ref().unwrap().interaction_id, "replacement");
        target["id"] = json!("stale_op");
        socket.send(Message::Text(target.to_string().into())).await.unwrap();
        let answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = answered.clone();
        let reply = tokio::spawn(async move {
            let ack = frame_of_type(&mut socket, "ack").await;
            signal.store(true, std::sync::atomic::Ordering::SeqCst);
            (socket, ack)
        });
        h.until(|_| answered.load(std::sync::atomic::Ordering::SeqCst)).await;
        let (mut socket, ack) = reply.await.unwrap();
        assert_eq!(ack["id"], "stale_op");
        assert_eq!(ack["error"], "stale");
        assert_eq!(h.calls("interaction.answer").len(), 1);
        assert!(h.calls("input.push").is_empty());
        assert_eq!(h.app.composer.text(), "conversation draft");
        h.action(Action::Remote("off".into()));
        assert_eq!(frame_of_type(&mut socket, "bye").await["reason"], "remote_off");
    }).await;
}

#[tokio::test(start_paused = true)]
async fn overflow_reconnect_replaces_the_whole_projection_and_view_epochs_never_repeat() {
    let (mut h, mut output) = connected_session().await;
    let original = view_epoch(&mut h, 1);
    let draft = crate::remote::PhoneView::Draft(crate::remote::DraftSettings {
        workspace: h.app.workspace.clone(),
        launch: Default::default(),
    });
    h.app.set_phone_view(1, draft);
    h.app.sync_remote();
    h.app
        .set_phone_view(1, crate::remote::PhoneView::Session("s1".into()));
    h.app.sync_remote();
    assert_ne!(view_epoch(&mut h, 1), original);
    messages(&mut output);
    for _ in 0..=512 {
        h.app
            .remote
            .send(Some(1), json!({"t":"notice", "text":"full"}));
    }
    assert!(h.app.remote.phones.is_empty());
    h.session_mut()
        .transcript
        .push(crate::transcript::Block::User {
            text: "while disconnected".into(),
            kind: crate::protocol::read::UserKind::Prompt,
            at: None,
        });
    h.mirror(vec![entry("still_queued", "follow_up", "same")], false);
    let (mut replacement, _close) = connect_phone(&mut h, 2, "first").await;
    h.app.sync_remote();
    let snapshot = replacement.try_recv().unwrap();
    assert_eq!(snapshot["t"], "snapshot");
    assert_eq!(snapshot["blocks"][0]["text"], "while disconnected");
    assert_eq!(snapshot["state"]["session"]["load"], "load_1");
    assert_eq!(snapshot["state"]["queue"][0]["id"], "still_queued");
    assert!(replacement.try_recv().is_err());
}
