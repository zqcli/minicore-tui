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

#[tokio::test(start_paused = true)]
async fn local_submit_is_refused_but_phone_submit_preserves_the_local_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn":turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.app.remote.mode = RemoteMode::Waiting;
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
    h.app.remote.mode = RemoteMode::Waiting;
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
    h.app.remote.mode = RemoteMode::Waiting;
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
    h.app.remote.mode = RemoteMode::Waiting;
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
async fn remote_approval_keeps_the_local_keyboard_and_denied_commands_keep_the_draft() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.mode = RemoteMode::Waiting;
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
    h.app.remote.mode = RemoteMode::Waiting;
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
