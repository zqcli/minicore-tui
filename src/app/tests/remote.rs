//! The phone and local keyboard share one reducer, never one composer.

use crossterm::event::KeyCode;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::harness::*;
use crate::app::{Action, Msg, Overlay, SessionOp};
use crate::remote::{Bound, Connection, RemoteEvent, RemoteMode, auth::Device, wire::Intent};

fn phone(
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

fn request(h: &mut Harness, intent: Value) {
    let (done, _finished) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Request {
        connection: 1,
        id: "request".into(),
        intent: serde_json::from_value::<Intent>(intent).unwrap(),
        done,
    }));
}

fn config(root: std::path::PathBuf) -> crate::config::Remote {
    crate::config::Remote {
        bind: "127.0.0.1:0".parse().unwrap(),
        url: None,
        roots: vec![root],
        allow_insecure: false,
    }
}

#[tokio::test]
async fn a_real_phone_pairs_submits_through_the_agent_and_receives_the_user_block() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path().canonicalize().unwrap());
    let bound = Bound::bind(&config, root.path().join("remote-devices.json"))
        .await
        .unwrap();
    let address = bound.listener.local_addr().unwrap();
    let code = bound.auth.lock().unwrap().pairing.code.clone();
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
                json!({"t":"pair", "v":1, "code":code, "device":"test phone"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        for kind in ["paired", "welcome", "snapshot"] {
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
    h.until(|app| app.remote.connection.is_some()).await;
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
async fn pairing_redraws_only_the_open_sheet_and_reuses_cached_qr_modules() {
    let mut h = Harness::new();
    let dir = tempfile::tempdir().unwrap();
    let auth = std::sync::Arc::new(std::sync::Mutex::new(
        crate::remote::auth::Auth::load(dir.path().join("remote-devices.json")).unwrap(),
    ));
    h.app.remote.auth = Some(auth.clone());
    h.app.remote.url = Some("http://127.0.0.1:8765".into());
    h.app.remote.refresh_pairing_qr();
    let (matrix, width) = h.app.remote.pairing_qr().unwrap();
    assert_eq!(matrix.len(), width * width);
    let cached = matrix.as_ptr();
    let now = tokio::time::Instant::now();
    assert_eq!(h.app.next_expiry(), None);
    h.app.overlay = Overlay::Remote;
    assert_eq!(
        h.app.next_expiry(),
        Some(now + std::time::Duration::from_secs(60))
    );
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    h.app.tick();
    assert_eq!(h.app.remote.pairing_qr().unwrap().0.as_ptr(), cached);
    assert_eq!(
        h.app.next_expiry(),
        Some(now + std::time::Duration::from_secs(120))
    );
    h.app.overlay = Overlay::None;
    assert_eq!(h.app.next_expiry(), None);
    tokio::time::advance(std::time::Duration::from_secs(480)).await;
    h.app.overlay = Overlay::Remote;
    assert_eq!(
        h.app.next_expiry(),
        Some(now + std::time::Duration::from_secs(600))
    );
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert_eq!(h.app.next_expiry(), None);
    assert!(h.app.remote.pairing_qr().is_none());
    auth.lock().unwrap().pairing = crate::remote::auth::Pairing::new().unwrap();
    h.app.remote.refresh_pairing_qr();
    h.app.remote.stop("remote_off");
    assert!(!auth.lock().unwrap().pairing.active());
    assert!(h.app.remote.pairing_qr().is_none());
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
async fn a_full_outbound_queue_drops_control_and_returns_to_waiting() {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_output, closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    for _ in 0..=512 {
        h.app.remote.send(json!({"t":"notice", "text":"bounded"}));
    }
    assert_eq!(*closing.borrow(), Some("overflow"));
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
    assert!(h.app.remote.connection.is_none());
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
async fn entering_remote_recalls_followups_and_an_agent_refusal_returns_only_to_the_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app
        .session
        .as_mut()
        .unwrap()
        .push_follow_up("queued".into());
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path().canonicalize().unwrap());
    let bound = Bound::bind(&config, dir.path().join("remote-devices.json"))
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
async fn controller_admission_replaces_only_the_same_device_and_disconnect_keeps_local_input_frozen()
 {
    let mut h = Harness::new();
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_out, old, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (_out2, _close2, admission) = phone(&mut h, 2, "second");
    assert!(!admission.await.unwrap());
    let (_out3, closing, admission) = phone(&mut h, 3, "first");
    assert!(admission.await.unwrap());
    assert_eq!(*old.borrow(), Some("replaced"));
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    assert_eq!(h.app.remote.mode(), RemoteMode::Connected);
    h.action(Action::Remote("off".into()));
    assert_eq!(*closing.borrow(), Some("remote_off"));
    assert_eq!(h.app.remote.mode(), RemoteMode::Off);
    h.app.remote.url = Some("http://localhost:8765".into());
    let (_out4, _close4, admission) = phone(&mut h, 4, "first");
    assert!(admission.await.unwrap());
    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(4)));
    assert_eq!(h.app.remote.mode(), RemoteMode::Waiting);
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
    assert!(h.app.remote.connection.is_none());
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
    assert!(
        crate::ui::sheets::sheet(&h.app, h.app.styles, ratatui::layout::Size::new(80, 24))
            .is_none()
    );
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
