//! The phone and local keyboard share one reducer, never one composer.

use crossterm::event::KeyCode;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::harness::*;
use crate::app::{Action, Msg, Overlay, SessionOp};
use std::sync::{Arc, Mutex};

use crate::remote::{
    Bound, Connection, RemoteEvent, RemoteMode,
    auth::{Auth, Device},
    wire::Intent,
};

pub(super) fn phone(
    h: &mut Harness,
    id: u64,
    device: &str,
) -> (
    mpsc::Receiver<Value>,
    watch::Receiver<Option<&'static str>>,
    oneshot::Receiver<bool>,
) {
    let (outbound, output) = mpsc::channel(512);
    let (close, closing) = watch::channel(None);
    let (accepted, admission) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Connect {
        connection: Connection {
            id,
            generation: h.app.remote.generation,
            device: Device {
                name: device.into(),
                token_sha256: device.into(),
                paired_at: "2026-01-01T00:00:00Z".into(),
            },
            outbound,
            close,
        },
        accepted,
    }));
    (output, closing, admission)
}

pub(super) fn request(h: &mut Harness, intent: Value) {
    request_from(h, 1, intent);
}

fn request_from(h: &mut Harness, connection: u64, intent: Value) {
    let (done, _finished) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Request {
        connection,
        id: "request".into(),
        intent: serde_json::from_value::<Intent>(intent).unwrap(),
        done,
    }));
}

pub(super) fn config(root: std::path::PathBuf) -> crate::config::Remote {
    crate::config::Remote {
        bind: vec!["127.0.0.1:0".parse().unwrap()],
        url: None,
        roots: vec![root],
        allow_insecure: false,
        tls: None,
    }
}

#[tokio::test]
async fn a_real_phone_signs_in_submits_through_the_agent_and_receives_the_user_block() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
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
        for kind in ["authorized", "welcome", "snapshot"] {
            let reply: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(reply["t"], kind);
        }
        socket
            .send(Message::Text(
                json!({"t":"intent", "id":"send", "kind":"submit", "text":"real phone text"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let reply: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            if reply["t"] == "blocks" {
                assert_eq!(reply["upserts"][0]["block"]["text"], "real phone text");
                break;
            }
        }
        socket
    });
    h.until(|app| !app.remote.phones.is_empty()).await;
    h.app.sync_remote();
    h.until(|app| app.is_busy()).await;
    h.app.sync_remote();
    let mut socket = tokio::time::timeout(std::time::Duration::from_secs(5), client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.sends(), ["real phone text"]);
    assert_eq!(h.app.composer.text(), "untouched");
    h.action(Action::Remote("off".into()));
    loop {
        let reply: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        if reply["t"] == "bye" {
            assert_eq!(reply["reason"], "remote_off");
            break;
        }
    }
    assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
}

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
            let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
            assert!(admission.await.unwrap());
            h.app.session.as_mut().unwrap().interaction = Some(crate::app::PendingInteraction {
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
            request(
                &mut h,
                json!({"kind":"answer", "interaction_id":"interaction", "choice":choice}),
            );
            let ack = output.recv().await.unwrap();
            assert_eq!(ack["ok"], valid, "approval={approval} choice={choice}");
            assert_eq!(h.app.has_interaction(), !valid);
            h.settle().await;
            assert_eq!(h.calls("interaction.answer").len(), usize::from(valid));
            if !valid {
                assert_eq!(ack["error"], "bad_request");
                assert!(h.app.session.as_ref().unwrap().allowed_tools.is_empty());
            }
        }
    }
}

/// A running turn `l1` in the foreground waiting on an approval or a choice.
fn pending(h: &mut Harness, approval: bool) {
    let session = h.app.session.as_mut().unwrap();
    session.live = Some(crate::app::LiveTurn::new(
        crate::protocol::TurnRef::new("s1", "l1"),
        0,
    ));
    session.interaction = Some(crate::app::PendingInteraction {
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
}

#[tokio::test(start_paused = true)]
async fn a_deny_with_a_message_answers_first_then_steers_the_reason_into_the_same_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    pending(&mut h, true);
    request(
        &mut h,
        json!({"kind":"answer", "interaction_id":"interaction", "choice":"deny", "message":"Use the staging bucket  "}),
    );
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    h.settle().await;
    let methods: Vec<String> = h
        .fake
        .methods()
        .into_iter()
        .filter(|method| method == "interaction.answer" || method == "turn.steer")
        .collect();
    assert_eq!(methods, ["interaction.answer", "turn.steer"]);
    assert_eq!(
        h.calls("interaction.answer")[0]["answer"],
        json!({"type":"approval", "decision":"deny"})
    );
    let steer = &h.calls("turn.steer")[0];
    assert_eq!(
        (&steer["loop_id"], &steer["text"]),
        (&json!("l1"), &json!("Use the staging bucket"))
    );

    // A plain answer steers nothing; a message beside anything but a deny is refused untouched.
    pending(&mut h, true);
    request(
        &mut h,
        json!({"kind":"answer", "interaction_id":"interaction", "choice":"deny"}),
    );
    assert_eq!(output.recv().await.unwrap()["ok"], true);
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
        );
        assert_eq!(output.recv().await.unwrap()["error"], "bad_request");
        assert!(h.app.has_interaction());
    }
    h.settle().await;
    assert_eq!(h.calls("interaction.answer").len(), 2);
    assert_eq!(h.calls("turn.steer").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn phone_text_expands_templates_refuses_commands_and_keeps_other_slashes_as_the_composer_does()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.templates = vec![crate::app::templates::Template::parse(
        "review".into(),
        "---\ndescription: Review a file\n---\nReview $1 for ${2:-bugs}; all: $@\n",
    )];
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    request(&mut h, json!({"kind":"submit", "text":"/review lib.rs"}));
    assert_eq!(output.recv().await.unwrap()["ok"], true);
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
        request(&mut h, json!({"kind":"queue", "text":text}));
        let ack = output.recv().await.unwrap();
        match sent {
            Some(sent) => {
                assert_eq!(ack["ok"], true, "{text}");
                queued.push(sent);
            }
            None => assert_eq!(ack["error"], "unknown_command", "{text}"),
        }
    }
    assert_eq!(h.queue(), queued);
    assert!(h.notices().is_empty(), "{:?}", h.notices());
    assert_eq!(h.sends().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn compact_runs_for_the_viewed_idle_session_and_is_refused_while_busy_or_in_a_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    let progress = || {
        serde_json::from_value(json!({"operation_id":"auto-1", "phase":"summarizing", "covered_item_count":0, "retained_item_count":0})).unwrap()
    };
    for (live, compacting) in [(true, false), (false, true)] {
        let session = h.app.session.as_mut().unwrap();
        session.live =
            live.then(|| crate::app::LiveTurn::new(crate::protocol::TurnRef::new("s1", "l1"), 0));
        session.compaction = compacting.then(progress);
        request(&mut h, json!({"kind":"compact"}));
        assert_eq!(output.recv().await.unwrap()["error"], "busy");
    }
    h.app.session.as_mut().unwrap().compaction = None;
    request(&mut h, json!({"kind":"compact"}));
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    h.settle().await;
    let calls = h.calls("session.compact");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["session_id"], "s1");

    let mut draft = Harness::new();
    draft.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut draft, 1, "phone");
    assert!(admission.await.unwrap());
    request(&mut draft, json!({"kind":"compact"}));
    assert_eq!(output.recv().await.unwrap()["error"], "No session");
    draft.settle().await;
    assert!(draft.calls("session.compact").is_empty());
}

#[tokio::test(start_paused = true)]
async fn unqueue_returns_a_matching_follow_up_only_to_its_requester() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut first, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut second, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
    let session = h.app.session.as_mut().unwrap();
    session.live = Some(crate::app::LiveTurn::new(
        crate::protocol::TurnRef::new("s1", "l1"),
        0,
    ));
    session.queue.push(crate::app::Queued {
        text: "steered".into(),
        kind: crate::app::QueuedKind::Steer,
        at: None,
    });
    session.push_follow_up("one".into());
    session.push_follow_up("two".into());
    for (index, text) in [(0, "steered"), (1, "two"), (3, "one")] {
        request(
            &mut h,
            json!({"kind":"unqueue", "index":index, "text":text}),
        );
        assert_eq!(
            messages(&mut first),
            [json!({"t":"ack", "id":"request", "ok":false, "error":"stale"})]
        );
    }
    assert_eq!(h.queue(), ["steered", "one", "two"]);
    request_from(
        &mut h,
        2,
        json!({"kind":"unqueue", "index":1, "text":"one"}),
    );
    assert_eq!(
        messages(&mut second),
        [
            json!({"t":"returned", "text":"one"}),
            json!({"t":"ack", "id":"request", "ok":true})
        ]
    );
    assert!(messages(&mut first).is_empty());
    assert_eq!(h.queue(), ["steered", "two"]);
    assert!(h.app.composer.is_empty());
}

#[tokio::test(start_paused = true)]
async fn queue_cancel_and_history_intents_use_the_existing_reducer_operations() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    h.app.session.as_mut().unwrap().live = Some(crate::app::LiveTurn::new(
        crate::protocol::TurnRef::new("s1", "l1"),
        0,
    ));
    request(&mut h, json!({"kind":"queue", "text":"follow up  \n"}));
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    assert_eq!(h.queue(), ["follow up"]);
    assert!(h.sends().is_empty());
    let session = h.app.session.as_mut().unwrap();
    session.live = None;
    session.compaction = Some(serde_json::from_value(json!({"operation_id":"compact", "phase":"summarizing", "covered_item_count":0, "retained_item_count":0})).unwrap());
    request(&mut h, json!({"kind":"cancel"}));
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    h.settle().await;
    assert_eq!(
        h.calls("session.compact.cancel")[0]["operation_id"],
        "compact"
    );
    let before = h.calls("session.read").len();
    h.app.session.as_mut().unwrap().loaded_from = 20;
    request(&mut h, json!({"kind":"load_older"}));
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    h.settle().await;
    assert!(h.calls("session.read").len() > before);
}

#[tokio::test(start_paused = true)]
async fn model_reasoning_and_absolute_workspace_are_validated_before_dispatch() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("session.update", json!({"session": session_info("s1")}));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    for (intent, valid) in [
        (json!({"kind":"set_model", "model":"mock"}), true),
        (json!({"kind":"set_reasoning", "level":"high"}), true),
        (json!({"kind":"set_model", "model":"missing"}), false),
        (json!({"kind":"set_reasoning", "level":"missing"}), false),
        (json!({"kind":"new_session", "workspace":"relative"}), false),
    ] {
        request(&mut h, intent);
        assert_eq!(output.recv().await.unwrap()["ok"], valid);
        h.settle().await;
    }
    assert_eq!(h.calls("session.update").len(), 2);
    assert_eq!(h.calls("session.update")[0]["model"], "mock");
    assert_eq!(h.calls("session.update")[1]["reasoning"], "high");
}

#[tokio::test(start_paused = true)]
async fn a_full_outbound_queue_drops_the_only_phone_and_returns_to_waiting() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_output, closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
async fn local_submit_is_refused_but_phone_submit_preserves_the_local_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
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
    let (mut output, _close, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    request(&mut h, json!({"kind":"submit", "text":"phone prompt"}));
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.sends(), ["phone prompt"]);
    assert_eq!(h.app.composer.text(), "local draft");
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    h.press(KeyCode::Esc);
    h.settle().await;
    assert_eq!(h.calls("turn.cancel").len(), 1);
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
async fn entering_remote_recalls_followups_and_an_agent_refusal_returns_only_to_the_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app
        .session
        .as_mut()
        .unwrap()
        .push_follow_up("queued".into());
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().canonicalize().unwrap());
    let bound = Bound::bind(&config, dir.path().join("remote.json"))
        .await
        .unwrap();
    h.app.remote.config = Some(config);
    h.app.enter_remote(bound);
    assert_eq!(h.app.composer.text(), "queued");
    assert!(h.queue().is_empty());
    let (mut output, _close, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    let error = crate::protocol::RpcError::new(-1, "refused", "refused");
    h.op(SessionOp::Sent("remote rejected".into(), Err(error)));
    assert_eq!(
        output.recv().await.unwrap(),
        json!({"t":"returned", "text":"remote rejected"})
    );
    assert_eq!(output.recv().await.unwrap()["t"], "notice");
    assert_eq!(h.app.composer.text(), "queued");
    h.action(Action::Remote("off".into()));
}

#[tokio::test(start_paused = true)]
async fn a_device_replaces_only_its_own_connection_and_disconnecting_one_keeps_the_others() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_out, old, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (_out2, second, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
    let (_out3, first, admission) = phone(&mut h, 3, "first");
    assert!(admission.await.unwrap());
    assert_eq!(*old.borrow(), Some("replaced"));
    assert_eq!(*second.borrow(), None);
    assert_eq!(devices(&h), ["second", "first"]);
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(2)));
    assert_eq!(devices(&h), ["first"]);
    assert_eq!(h.app.remote.mode(), RemoteMode::Connected);
    let (_out4, other, admission) = phone(&mut h, 4, "other");
    assert!(admission.await.unwrap());
    h.action(Action::Remote("off".into()));
    assert_eq!(*first.borrow(), Some("remote_off"));
    assert_eq!(*other.borrow(), Some("remote_off"));
    assert_eq!(h.app.remote.mode(), RemoteMode::Off);
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_out5, _close5, admission) = phone(&mut h, 5, "first");
    assert!(admission.await.unwrap());
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(5)));
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
}

/// The connected device names, oldest first.
fn devices(h: &Harness) -> Vec<&str> {
    let phones = &h.app.remote.phones;
    phones
        .iter()
        .map(|p| p.connection.device.name.as_str())
        .collect()
}

/// The message types waiting in `output`.
fn received(output: &mut mpsc::Receiver<Value>) -> Vec<Value> {
    std::iter::from_fn(|| output.try_recv().ok())
        .map(|message| message["t"].clone())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn every_phone_gets_its_snapshot_then_the_changes_and_sees_who_is_connected() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close, admission) = phone(&mut h, 1, "iPhone Safari");
    assert!(admission.await.unwrap());
    let (mut b, _close2, admission) = phone(&mut h, 2, "Mac Edge");
    assert!(admission.await.unwrap());
    h.app.sync_remote();
    for output in [&mut a, &mut b] {
        let snapshot = output.try_recv().unwrap();
        assert_eq!(snapshot["t"], "snapshot");
        assert_eq!(
            snapshot["state"]["devices"],
            json!([{"name":"iPhone Safari", "session":"s1"}, {"name":"Mac Edge", "session":"s1"}])
        );
    }
    let session = h.app.session.as_mut().unwrap();
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
    let (mut c, _close3, admission) = phone(&mut h, 3, "iPad Safari");
    assert!(admission.await.unwrap());
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
async fn an_intent_is_acked_only_to_its_sender_and_notices_reach_every_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut b, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
    request_from(&mut h, 2, json!({"kind":"cancel"}));
    assert_eq!(received(&mut a), Vec::<Value>::new());
    assert_eq!(received(&mut b), ["ack"]);
    h.app.notice(crate::app::NoticeLevel::Info, "for everyone");
    assert_eq!(received(&mut a), ["notice"]);
    assert_eq!(received(&mut b), ["notice"]);
}

#[tokio::test(start_paused = true)]
async fn a_refused_text_returns_to_its_session_sender_or_once_it_left_to_viewers() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut b, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
    let (mut c, _close3, admission) = phone(&mut h, 3, "third");
    assert!(admission.await.unwrap());
    request_from(&mut h, 2, json!({"kind":"submit", "text":"from second"}));
    h.until(|app| app.is_busy()).await;
    let refused = || crate::protocol::RpcError::new(-1, "refused", "refused");
    h.op(SessionOp::Sent("from second".into(), Err(refused())));
    assert_eq!(received(&mut a), ["notice"]);
    assert_eq!(received(&mut b), ["ack", "returned", "notice"]);
    assert_eq!(received(&mut c), ["notice"]);
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(2)));
    h.op(SessionOp::Sent("again".into(), Err(refused())));
    for output in [&mut a, &mut c] {
        let returned = output.try_recv().unwrap();
        assert_eq!(returned, json!({"t":"returned", "text":"again"}));
    }
}

#[tokio::test(start_paused = true)]
async fn a_phone_whose_queue_overflows_is_dropped_alone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_stalled, closing, admission) = phone(&mut h, 1, "stalled");
    assert!(admission.await.unwrap());
    let (mut other, open, admission) = phone(&mut h, 2, "other");
    assert!(admission.await.unwrap());
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
async fn root_checks_and_stale_answers_refuse_before_dispatch_and_new_drafts_use_the_chosen_workspace()
 {
    let mut h = Harness::new();
    h.boot().await;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    h.app.remote.config = Some(config(workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _close, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"}));
    assert_eq!(output.recv().await.unwrap()["error"], "outside_roots");
    assert!(h.calls("session.open").is_empty());
    request(
        &mut h,
        json!({"kind":"answer", "interaction_id":"stale", "choice":"allow"}),
    );
    assert_eq!(output.recv().await.unwrap()["error"], "stale");
    request(
        &mut h,
        json!({"kind":"new_session", "workspace":workspace, "profile":"coding"}),
    );
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    assert!(h.calls("session.create").is_empty());
    let mut info = session_info("s1");
    info["workspace"] = json!(workspace);
    h.fake.script("session.create", json!({"session":info}));
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.composer.set_text("keep");
    request(&mut h, json!({"kind":"submit", "text":"new work"}));
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.calls("session.create")[0]["workspace"], json!(workspace));
    assert_eq!(h.sends(), ["new work"]);
    assert_eq!(h.app.composer.text(), "keep");
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
    h.app.session.as_mut().unwrap().interaction = Some(crate::app::PendingInteraction {
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
        assert!(h.app.session.is_some());
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
    h.submit("local again");
    h.settle().await;
    assert_eq!(h.sends(), ["local again"]);
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

/// Types `command` and presses Enter, as at the keyboard.
fn command(h: &mut Harness, command: &str) {
    h.app.composer.set_text(command);
    h.press(KeyCode::Enter);
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
    let (_output, closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    let (_output2, other, admission) = phone(&mut h, 2, "other");
    assert!(admission.await.unwrap());
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
    let (_output, closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
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
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    assert!(synced_sessions(&mut h, &mut output).is_some());
    (h, output)
}

/// The last state the next sync sends, if it sends one.
pub(super) fn synced_state(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Option<Value> {
    h.app.sync_remote();
    let mut state = None;
    while let Ok(message) = output.try_recv() {
        if message.get("state").is_some() {
            state = Some(message["state"].clone());
        }
    }
    state
}

/// The session list of the last state the next sync sends, if it sends one.
fn synced_sessions(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Option<Vec<Value>> {
    let state = synced_state(h, output)?;
    Some(state["sessions"].as_array().unwrap().clone())
}

fn listed<'a>(sessions: &'a [Value], id: &str) -> Option<&'a Value> {
    sessions.iter().find(|session| session["id"] == id)
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
    );
    assert_eq!(output.recv().await.unwrap()["ok"], true);
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
        );
        let ack = output.recv().await.unwrap();
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
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"}));
    assert_eq!(output.recv().await.unwrap()["ok"], true);
    h.settle().await;
    assert_eq!(h.calls("session.close")[0]["session_id"], "s2");
    assert_eq!(h.calls("session.delete")[0]["session_id"], "s2");
    let sessions = synced_sessions(&mut h, &mut output).expect("a new state");
    assert!(listed(&sessions, "s2").is_none());
    assert!(listed(&sessions, "s1").is_some());
    assert!(h.notices().contains(&"Session deleted".to_owned()));
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
        request(&mut h, intent.clone());
        assert_eq!(output.recv().await.unwrap()["error"], error, "{intent}");
    }
    h.settle().await;
    assert!(h.calls("session.close").is_empty());
    assert!(h.calls("session.delete").is_empty());
    assert_eq!(h.calls("session.rename").len(), renames);
}

fn messages(output: &mut mpsc::Receiver<Value>) -> Vec<Value> {
    std::iter::from_fn(|| output.try_recv().ok()).collect()
}

fn ack(output: &mut mpsc::Receiver<Value>) -> Value {
    messages(output)
        .into_iter()
        .find(|message| message["t"] == "ack")
        .expect("an ack")
}

fn projected(output: &mut mpsc::Receiver<Value>) -> Value {
    messages(output)
        .into_iter()
        .rev()
        .find_map(|message| message.get("state").cloned())
        .expect("a projection")
}

async fn separate_views() -> (Harness, mpsc::Receiver<Value>, mpsc::Receiver<Value>) {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    h.app.default_profile = Some("coding".into());
    h.app.session.as_mut().unwrap().info.title = Some("First".into());
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
            .script(&format!("session.open@{id}"), json!({"session":info}));
        let turn = json!({"session_id":id, "loop_id":format!("loop-{id}")});
        h.fake
            .script(&format!("turn.send@{id}"), json!({"turn":turn}));
        h.fake.script_held(
            &format!("turn.wait@{id}"),
            json!({"turn":turn, "outcome":completed()}),
        );
    }
    h.app.catalog_generation += 1;
    let (a, _close_a, admission) = phone(&mut h, 1, "first phone");
    assert!(admission.await.unwrap());
    let (mut b, _close_b, admission) = phone(&mut h, 2, "second phone");
    assert!(admission.await.unwrap());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"}));
    assert_eq!(ack(&mut b)["ok"], true);
    h.until(|app| app.live("s2").is_some()).await;
    h.settle().await;
    (h, a, b)
}

#[tokio::test(start_paused = true)]
async fn phones_open_their_own_sessions_without_replacing_each_others_projection() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"}));
    assert_eq!(ack(&mut a)["ok"], true);
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
    request(&mut h, json!({"kind":"open_session", "session_id":"s3"}));
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
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s3");
    h.action(Action::Remote("off".into()));
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s3");
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
    request(&mut h, json!({"kind":"submit", "text":"work in one"}));
    assert_eq!(h.app.live("s2").unwrap().scroll, anchored);
    let (mut c, _close_c, admission) = phone(&mut h, 3, "also first");
    assert!(admission.await.unwrap());
    h.app.composer.set_text("local in one");
    request_from(&mut h, 2, json!({"kind":"submit", "text":"work in two"}));
    h.until(|app| app.live("s1").unwrap().is_busy() && app.live("s2").unwrap().is_busy())
        .await;
    h.settle().await;
    assert_eq!(
        h.calls("turn.send")
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
    request(&mut h, json!({"kind":"cancel"}));
    assert_eq!(h.app.composer.text(), "local in one");
    assert_eq!(h.app.live("s2").unwrap().scroll, crate::app::Scroll::Follow);
}

#[tokio::test(start_paused = true)]
async fn an_approval_is_shown_and_answered_only_in_the_viewed_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"cancel"}));
    h.emit("interaction_requested", json!({"meta":{"session_id":"s2", "dropped_before":0}, "turn":{"session_id":"s2", "loop_id":"loop-s2"}, "interaction":{"interaction_id":"approve-two", "tool_call_id":"call-two", "tool_name":"bash", "kind":{"type":"approval", "data":{"prompt":"Allow?"}}}})).await;
    h.settle().await;
    h.app.sync_remote();
    let first = projected(&mut a);
    let second = projected(&mut b);
    assert!(first["interaction"].is_null());
    assert_eq!(first["background"]["approval"], 1);
    assert_eq!(second["interaction"]["id"], "approve-two");
    assert_eq!(second["background"]["approval"], 0);
    request(
        &mut h,
        json!({"kind":"answer", "interaction_id":"approve-two", "choice":"allow"}),
    );
    assert_eq!(ack(&mut a)["error"], "stale");
    request_from(
        &mut h,
        2,
        json!({"kind":"answer", "interaction_id":"approve-two", "choice":"allow"}),
    );
    assert_eq!(ack(&mut b)["ok"], true);
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
    );
    request(&mut h, json!({"kind":"set_model", "model":"chosen"}));
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"}));
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
    let (mut c, _close_c, admission) = phone(&mut h, 3, "new arrival");
    assert!(admission.await.unwrap());
    h.app.sync_remote();
    assert_eq!(projected(&mut c)["draft"], draft["draft"]);
    request_from(
        &mut h,
        2,
        json!({"kind":"new_session", "workspace":two, "profile":"coding"}),
    );
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
    h.fake.script("session.create", json!({"session":info}));
    h.fake.script(
        "turn.send@created",
        json!({"turn":{"session_id":"created", "loop_id":"first"}}),
    );
    h.fake.script_held(
        "turn.wait@created",
        json!({"turn":{"session_id":"created", "loop_id":"first"}, "outcome":completed()}),
    );
    request(
        &mut h,
        json!({"kind":"submit", "text":"first in my workspace"}),
    );
    h.until(|app| app.live("created").is_some_and(|session| session.is_busy()))
        .await;
    h.settle().await;
    let create = &h.calls("session.create")[0];
    assert_eq!(create["workspace"], json!(one));
    assert_eq!(create["profile"], "coding");
    assert_eq!(create["model"], "chosen");
    assert_eq!(create["reasoning"], "high");
    assert_eq!(h.calls("turn.send")[0]["session_id"], "created");
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
    request_from(&mut h, 2, json!({"kind":"set_reasoning", "level":"low"}));
    h.action(Action::Remote("off".into()));
    assert!(h.app.session.is_none());
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
    request(&mut h, json!({"kind":"new_session", "workspace":root}));
    request(&mut h, json!({"kind":"set_model", "model":"draft-model"}));
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"}));
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s3"}));
    assert_eq!(ack(&mut b)["ok"], true);
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
async fn a_pending_create_blocks_other_intents_without_losing_its_first_message_or_draft() {
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
    );
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"}));
    let (mut c, _close_c, admission) = phone(&mut h, 3, "another draft");
    assert!(admission.await.unwrap());
    let mut created = session_info("created");
    created["title"] = json!("Created");
    created["workspace"] = json!(root);
    created["reasoning"] = json!("high");
    h.fake
        .script_held("session.create", json!({"session":created}));
    h.fake.script(
        "turn.send@created",
        json!({"turn":{"session_id":"created", "loop_id":"first"}}),
    );
    h.fake.script_held(
        "turn.wait@created",
        json!({"turn":{"session_id":"created", "loop_id":"first"}, "outcome":completed()}),
    );
    request(&mut h, json!({"kind":"submit", "text":"my first message"}));
    h.app.composer.set_text("typed while creating");
    h.settle().await;
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["status"]["loading"], true);
    assert_eq!(projected(&mut b)["status"]["loading"], false);
    assert_eq!(projected(&mut c)["status"]["loading"], false);
    for intent in [
        json!({"kind":"open_session", "session_id":"s3"}),
        json!({"kind":"new_session", "workspace":root}),
        json!({"kind":"submit", "text":"live work"}),
        json!({"kind":"cancel"}),
        json!({"kind":"open_session", "session_id":"s1"}),
    ] {
        request_from(&mut h, 2, intent);
        assert_eq!(ack(&mut b)["error"], "Another device is opening a session");
    }
    request_from(&mut h, 3, json!({"kind":"submit", "text":"another draft"}));
    assert_eq!(ack(&mut c)["error"], "Another device is opening a session");
    request(&mut h, json!({"kind":"submit", "text":"again"}));
    assert_eq!(
        ack(&mut a)["error"],
        "A session is opening or being created"
    );
    assert_eq!(h.app.first_message.as_deref(), Some("my first message"));
    assert!(h.sends().is_empty());
    h.fake.release("session.create");
    h.until(|app| app.live("created").is_some_and(|session| session.is_busy()))
        .await;
    h.settle().await;
    assert_eq!(h.sends(), ["my first message"]);
    assert_eq!(h.calls("turn.send")[0]["session_id"], "created");
    assert_eq!(h.app.composer.text(), "typed while creating");
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
    h.fake
        .script_held("session.open@s3", json!({"session":third}));
    request(&mut h, json!({"kind":"open_session", "session_id":"s3"}));
    assert_eq!(ack(&mut a)["ok"], true);
    request_from(
        &mut h,
        2,
        json!({"kind":"submit", "text":"work while opening"}),
    );
    assert_eq!(ack(&mut b)["ok"], true);
    h.until(|app| app.live("s2").unwrap().is_busy()).await;
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["status"]["loading"], true);
    assert_eq!(projected(&mut b)["status"]["loading"], false);
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"}));
    assert_eq!(ack(&mut b)["ok"], true);
    assert_eq!(h.app.session.as_ref().unwrap().info.session_id, "s1");
    h.fake.release("session.open@s3");
    h.until(|app| app.live("s3").is_some()).await;
    h.settle().await;
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s3");
    assert_eq!(projected(&mut b)["session"]["id"], "s1");
    assert_eq!(h.calls("turn.send")[0]["session_id"], "s2");
}

#[tokio::test(start_paused = true)]
async fn deletion_refuses_any_viewer_but_removes_an_unviewed_idle_live_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"}));
    assert_eq!(ack(&mut a)["error"], "Open on \"second phone\"");
    request_from(
        &mut h,
        2,
        json!({"kind":"delete_session", "session_id":"s2"}),
    );
    assert_eq!(
        ack(&mut b)["error"],
        "Open another session before deleting this one"
    );
    h.settle().await;
    assert!(h.calls("session.delete").is_empty());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"}));
    assert_eq!(ack(&mut b)["ok"], true);
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"}));
    assert_eq!(ack(&mut a)["ok"], true);
    h.settle().await;
    assert!(h.app.live("s2").is_none());
    assert_eq!(h.calls("session.close")[0]["session_id"], "s2");
    assert_eq!(h.calls("session.delete")[0]["session_id"], "s2");
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
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"}));
    h.fake.script_held("session.delete@s2", json!({}));
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"}));
    assert_eq!(ack(&mut a)["ok"], true);
    h.settle().await;
    assert!(h.app.live("s2").is_some());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"}));
    assert_eq!(ack(&mut b)["ok"], true);
    h.fake.release("session.delete@s2");
    h.until(|app| app.live("s2").is_none()).await;
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    let fallback = projected(&mut b);
    assert!(fallback["session"].is_null());
    assert_eq!(fallback["draft"]["model"], "mock");
    assert_eq!(fallback["draft"]["reasoning"], "medium");
    request_from(&mut h, 2, json!({"kind":"set_reasoning", "level":"high"}));
    assert_eq!(ack(&mut b)["ok"], true);
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
    request_from(&mut h, 2, json!({"kind":"set_model", "model":"strict"}));
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
        Box::new(SessionOp::Done(Err(crate::protocol::RpcError::new(
            -1,
            "test",
            "async problem",
        )))),
    ));
    assert_eq!(a.try_recv().unwrap()["text"], "async problem");
    assert_eq!(b.try_recv().unwrap()["text"], "\"First\": async problem");
    assert_eq!(h.notices().last().unwrap(), "\"First\": async problem");
    request(&mut h, json!({"kind":"cancel"}));
    messages(&mut a);
    h.app.on_msg(Msg::Session(
        epoch,
        Box::new(SessionOp::Done(Err(crate::protocol::RpcError::new(
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
    request_from(&mut h, 2, json!({"kind":"cancel"}));
    messages(&mut b);
    h.emit("interaction_requested", json!({"turn":{"session_id":"s1", "loop_id":"loop-s1"}, "interaction":{"interaction_id":"approval-one", "tool_call_id":"call-one", "tool_name":"bash", "kind":{"type":"approval", "data":{}}}})).await;
    h.settle().await;
    assert!(
        messages(&mut a).is_empty(),
        "a viewer does not get attention to its own session"
    );
    assert_eq!(
        b.try_recv().unwrap()["text"],
        "\"First\" needs approval · ctrl+r"
    );
    h.app.on_msg(Msg::Notice(
        crate::app::NoticeLevel::Info,
        "app wide".into(),
    ));
    assert_eq!(a.try_recv().unwrap()["text"], "app wide");
    assert_eq!(b.try_recv().unwrap()["text"], "app wide");
}

#[tokio::test(start_paused = true)]
async fn refused_text_uses_its_session_sender_then_viewers_then_stays_with_the_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    request(&mut h, json!({"kind":"submit", "text":"one"}));
    let (mut c, _close_c, admission) = phone(&mut h, 3, "another viewer");
    assert!(admission.await.unwrap());
    request_from(&mut h, 2, json!({"kind":"submit", "text":"two"}));
    h.until(|app| app.live("s1").unwrap().is_busy() && app.live("s2").unwrap().is_busy())
        .await;
    h.settle().await;
    messages(&mut a);
    messages(&mut b);
    let epoch = h.app.live("s1").unwrap().epoch;
    let refused = |text: &str| {
        Msg::Session(
            epoch,
            Box::new(SessionOp::Sent(
                text.into(),
                Err(crate::protocol::RpcError::new(-1, "refused", "test")),
            )),
        )
    };
    h.app.on_msg(refused("one"));
    assert_eq!(a.try_recv().unwrap(), json!({"t":"returned", "text":"one"}));
    for output in [&mut b, &mut c] {
        assert!(
            messages(output)
                .iter()
                .all(|message| message["t"] != "returned")
        );
    }
    messages(&mut a);
    request(&mut h, json!({"kind":"open_session", "session_id":"s2"}));
    messages(&mut a);
    h.app.on_msg(refused("still to its sender"));
    assert_eq!(a.try_recv().unwrap()["t"], "returned");
    messages(&mut c);
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    h.app.on_msg(refused("to its viewer"));
    assert_eq!(
        c.try_recv().unwrap(),
        json!({"t":"returned", "text":"to its viewer"})
    );
    assert!(
        messages(&mut b)
            .iter()
            .all(|message| message["t"] != "returned")
    );
    request_from(&mut h, 3, json!({"kind":"open_session", "session_id":"s2"}));
    messages(&mut c);
    h.app.on_msg(refused("kept for local control"));
    assert_eq!(
        h.app.live("s1").unwrap().returned,
        ["kept for local control"]
    );
    assert!(h.app.composer.is_empty());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"}));
    assert_eq!(
        h.app.live("s1").unwrap().returned,
        ["kept for local control"]
    );
    assert!(h.app.composer.is_empty());
    h.action(Action::Remote("off".into()));
    assert_eq!(h.app.composer.text(), "kept for local control");
}

#[tokio::test(start_paused = true)]
async fn viewed_turn_ends_stay_seen_and_opening_a_session_clears_unseen() {
    let (mut h, mut a, mut b) = separate_views().await;
    for viewed in [true, false] {
        h.fake.script_held(
            "turn.wait@s1",
            json!({"turn":{"session_id":"s1", "loop_id":"loop-s1"}, "outcome":completed()}),
        );
        request(&mut h, json!({"kind":"open_session", "session_id":"s1"}));
        request(&mut h, json!({"kind":"submit", "text":"work"}));
        h.until(|app| app.live("s1").unwrap().is_busy()).await;
        request_from(&mut h, 2, json!({"kind":"cancel"}));
        if !viewed {
            request(&mut h, json!({"kind":"open_session", "session_id":"s2"}));
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
    request(&mut h, json!({"kind":"open_session", "session_id":"s1"}));
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
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"}));
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
    );
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"}));
    messages(&mut a);
    h.fake
        .script_error("session.create", -1, "cannot create", "test");
    request(&mut h, json!({"kind":"submit", "text":"keep this text"}));
    h.until(|app| !app.loading_session).await;
    let returned = messages(&mut a);
    assert!(
        returned
            .iter()
            .any(|message| message == &json!({"t":"returned", "text":"keep this text"}))
    );
    assert!(
        returned
            .iter()
            .any(|message| message["t"] == "notice" && message["text"] == "cannot create")
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
    request(&mut h, json!({"kind":"set_model", "model":"chosen"}));
    assert_eq!(ack(&mut a)["ok"], true);
    h.settle().await;
    request(&mut h, json!({"kind":"set_reasoning", "level":"high"}));
    assert_eq!(ack(&mut a)["ok"], true);
    request_from(&mut h, 2, json!({"kind":"cancel"}));
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
async fn failed_opens_keep_device_views_and_route_the_error_with_the_requested_session() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.fake
        .script_error("session.open@s3", -1, "cannot open", "test");
    request(&mut h, json!({"kind":"open_session", "session_id":"s3"}));
    assert_eq!(ack(&mut a)["ok"], true);
    h.until(|app| !app.loading_session).await;
    assert_eq!(a.try_recv().unwrap()["text"], "\"Third\": cannot open");
    assert_eq!(b.try_recv().unwrap()["text"], "\"Third\": cannot open");
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    assert_eq!(projected(&mut b)["session"]["id"], "s2");
    assert!(h.app.remote.opening.is_none());
}

#[tokio::test(start_paused = true)]
async fn a_deletion_finishing_after_remote_off_keeps_the_local_draft_editable() {
    let (mut h, mut a, mut b) = separate_views().await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s1"}));
    h.fake.script_held("session.delete@s2", json!({}));
    request(&mut h, json!({"kind":"delete_session", "session_id":"s2"}));
    assert_eq!(ack(&mut a)["ok"], true);
    h.settle().await;
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"}));
    assert_eq!(ack(&mut b)["ok"], true);
    h.action(Action::Remote("off".into()));
    h.fake.release("session.delete@s2");
    h.until(|app| app.live("s2").is_none()).await;
    h.app.set_reasoning(crate::protocol::Reasoning::High);
    assert!(h.app.session.is_none());
    assert_eq!(h.app.reasoning(), Some(crate::protocol::Reasoning::High));
}

#[tokio::test(start_paused = true)]
async fn a_late_open_result_cannot_bring_back_a_session_deleted_by_another_device() {
    let (mut h, mut a, mut b) = separate_views().await;
    let mut third = session_info("s3");
    third["title"] = json!("Third");
    h.fake
        .script_held("session.open@s3", json!({"session":third}));
    request(&mut h, json!({"kind":"open_session", "session_id":"s3"}));
    assert_eq!(ack(&mut a)["ok"], true);
    request_from(
        &mut h,
        2,
        json!({"kind":"delete_session", "session_id":"s3"}),
    );
    assert_eq!(ack(&mut b)["ok"], true);
    h.until(|app| !app.sessions.iter().any(|info| info.session_id == "s3"))
        .await;
    h.fake.release("session.open@s3");
    h.until(|app| !app.loading_session).await;
    assert!(h.app.live("s3").is_none());
    h.app.sync_remote();
    assert_eq!(projected(&mut a)["session"]["id"], "s1");
    assert_eq!(projected(&mut b)["session"]["id"], "s2");
}

fn query_from(h: &mut Harness, connection: u64, id: &str, query: Value) {
    h.app.on_msg(Msg::Remote(RemoteEvent::Query {
        connection,
        id: id.into(),
        query: serde_json::from_value(query).unwrap(),
    }));
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

fn live_mut<'a>(h: &'a mut Harness, id: &str) -> &'a mut crate::app::Session {
    let app = &mut h.app;
    app.session
        .iter_mut()
        .chain(app.background.iter_mut())
        .find(|session| session.info.session_id == id)
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn context_and_files_queries_read_the_requesting_devices_session_and_map_the_reply() {
    let (mut h, mut a, mut b) = separate_views().await;
    h.fake.script(
        "session.context@s2",
        json!({"session_id":"s2", "current_operation":null,
               "coverage":{"covered_loop_count":6, "covered_item_count":12, "retained_item_count":3},
               "last_result":{"operation_id":"auto-1", "origin":"automatic", "status":"compacted",
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
            "last_compaction":{"origin":"automatic", "status":"compacted", "before":4200, "after":900},
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
    let (mut output, _closing, admission) = phone(&mut draft, 1, "phone");
    assert!(admission.await.unwrap());
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
    let (mut first, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut second, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
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
