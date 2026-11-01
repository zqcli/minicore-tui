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

/// Numbers test operations; an answer names only its operation id.
static NEXT_OP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(super) async fn request(h: &mut Harness, intent: Value) -> String {
    request_from(h, 1, intent).await
}

/// Delivers an intent the way the socket does, naming the view and instance
/// this phone holds unless the test supplies its own.
async fn request_from(h: &mut Harness, connection: u64, mut intent: Value) -> String {
    let viewed = h
        .app
        .remote
        .phones
        .iter()
        .find(|p| p.connection.id == connection)
        .and_then(|p| p.view.session_id())
        .map(str::to_owned);
    let session = viewed.as_deref().and_then(|id| h.app.live(id));
    let session_id = session.map(|s| s.info.session_id.clone());
    let load = session.map(|s| s.load.clone());
    let epoch = h
        .app
        .remote
        .phones
        .iter()
        .find(|phone| phone.connection.id == connection)
        .map(|phone| phone.projection.view_epoch().to_owned())
        .unwrap_or_default();
    intent["view_epoch"] = intent
        .get("view_epoch")
        .cloned()
        .unwrap_or_else(|| json!(epoch));
    // Mirror what core.js sends: an operation on the viewed session names it
    // and its resident instance; one that addresses another session, or only
    // changes this view, names just that.
    let acts_on_view = !matches!(
        intent["kind"].as_str(),
        Some("open_session" | "new_session" | "rename_session" | "delete_session")
    );
    if acts_on_view && let Some(id) = &session_id {
        intent["session_id"] = intent
            .get("session_id")
            .cloned()
            .unwrap_or_else(|| json!(id));
        if intent["session_id"] == json!(id)
            && let Some(load) = &load
        {
            intent["load"] = intent.get("load").cloned().unwrap_or_else(|| json!(load));
        }
    }
    // Like the client: the loop the phone was shown, when it showed one.
    if let Some(kind) = intent["kind"].as_str()
        && matches!(kind, "cancel" | "answer")
        && intent.get("loop_id").is_none()
        && h.app
            .remote
            .phones
            .iter()
            .any(|phone| phone.connection.id == connection && phone.view.session_id().is_some())
    {
        let cancel = kind == "cancel";
        let turn = match kind {
            "cancel" => session
                .and_then(|session| session.live.as_ref())
                .map(|live| live.turn.loop_id.clone()),
            _ => session
                .and_then(|session| session.interaction.as_ref())
                .map(|interaction| interaction.turn.loop_id.clone()),
        };
        if let Some(turn) = turn {
            intent["loop_id"] = json!(turn);
        }
        if cancel
            && intent.get("operation_id").is_none()
            && let Some(operation) = session.and_then(|s| s.compaction.as_ref())
        {
            intent["operation_id"] = json!(operation.operation_id);
        }
    }
    // A fresh operation each time: an answer names only its operation, so a
    // live id cannot be reused.
    let id = format!(
        "request{}",
        NEXT_OP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let (done, _finished) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Request {
        connection,
        id: id.clone(),
        intent: serde_json::from_value::<Intent>(intent).unwrap(),
        done,
    }));
    // A submit, queue or unqueue is answered from the Agent's own result, so the
    // app must run before the answer exists. Nothing is drained here: the caller
    // reads the frames this produces in order.
    h.settle().await;
    id
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
            .holds_pending(app.remote.phones[0].connection.id, "send")
    })
    .await;
    called(&h, "input.push", 1).await;
    h.fake.release("input.push");
    h.until(|app| {
        app.is_busy()
            && !app
                .remote
                .holds_pending(app.remote.phones[0].connection.id, "send")
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
    h.app
        .resident
        .current_mut()
        .unwrap()
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
            h.app.resident.current_mut().unwrap().interaction =
                Some(crate::app::PendingInteraction {
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
                assert!(h.app.resident.current().unwrap().allowed_tools.is_empty());
            }
        }
    }
}

/// A running turn `l1` in the foreground waiting on an approval or a choice.
fn pending(h: &mut Harness, approval: bool) {
    let session = h.app.resident.current_mut().unwrap();
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
    // The phone must hold the interaction in its own projection, as it would
    // after the snapshot that carried it.
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.sync_remote();
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
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    let progress = || {
        serde_json::from_value(json!({"operation_id":"auto-1", "phase":"summarizing", "covered_item_count":0, "retained_item_count":0})).unwrap()
    };
    for (live, compacting) in [(true, false), (false, true)] {
        let session = h.app.resident.current_mut().unwrap();
        session.live =
            live.then(|| crate::app::LiveTurn::new(crate::protocol::TurnRef::new("s1", "l1"), 0));
        session.compaction = compacting.then(progress);
        request(&mut h, json!({"kind":"compact"})).await;
        assert_eq!(ack_next(&mut h, &mut output).await["error"], "busy");
    }
    h.app.resident.current_mut().unwrap().compaction = None;
    request(&mut h, json!({"kind":"compact"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
    h.settle().await;
    let calls = h.calls("session.compact");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["session_id"], "s1");

    let mut draft = Harness::new();
    draft.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut draft, 1, "phone");
    assert!(admission.await.unwrap());
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
    let (mut first, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut second, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
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
    let revision = h.app.resident.current().unwrap().recovery.revision;
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
    assert!(h.app.resident.current().unwrap().recovery.revision > revision);
    assert_eq!(h.queue(), ["now", "two"]);

    h.fake.script(
        "input.remove",
        json!({"text": "two", "queue": queue(vec![now.clone()], false)}),
    );
    let revision = h.app.resident.current().unwrap().recovery.revision;
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
    assert!(h.app.resident.current().unwrap().recovery.revision > revision);
    assert_eq!(
        h.queue(),
        ["now"],
        "the newer authoritative snapshot preserves the survivor"
    );
    assert_eq!(h.app.resident.current().unwrap().queue.items[0].id, "a");
    assert!(h.app.composer.is_empty());
}

fn w0_phone_session() -> Harness {
    let mut h = Harness::new();
    let mut info: crate::protocol::SessionInfo =
        serde_json::from_value(session_info("s1")).unwrap();
    info.workspace = h.app.workspace.to_string_lossy().into_owned();
    h.app.sessions.push(info.clone());
    h.app.epoch = 1;
    let session = crate::app::Session::new(info, 1, crate::testing::LOAD.into());
    h.app.show_session(Some(session));
    h.app.ready = true;
    h
}

/// The projection identity the server sent this connection last.
fn view_epoch(h: &mut Harness, connection: u64) -> String {
    h.app
        .remote
        .phones
        .iter()
        .find(|phone| phone.connection.id == connection)
        .expect("an admitted phone")
        .projection
        .view_epoch()
        .to_owned()
}

/// A v3 intent naming the projection and resident instance the phone holds.
fn phone_intent(h: &mut Harness, connection: u64, mut intent: Value) -> Value {
    let epoch = view_epoch(h, connection);
    intent["view_epoch"] = json!(epoch);
    intent["session_id"] = json!("s1");
    intent["load"] = json!("load_1");
    intent
}

#[tokio::test(start_paused = true)]
async fn w0_a_stale_phone_row_cannot_remove_a_different_input_with_the_same_text() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut h = w0_phone_session();
        h.app.remote.config = Some(config(h.app.workspace.clone()));
        h.app.remote.url = Some("http://localhost:8765".into());
        let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
        assert!(admission.await.unwrap());
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
async fn w0_a_phone_submit_is_not_acknowledged_before_the_agent_accepts_it() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut h = w0_phone_session();
        h.app.remote.url = Some("http://localhost:8765".into());
        let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
        assert!(admission.await.unwrap());
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
    let (mut output, _close, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    let mut h = w0_phone_session();
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    h.app.resident.current_mut().unwrap().live = Some(crate::app::LiveTurn::new(
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
    let (mut output, _closing, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
    h.app.resident.current_mut().unwrap().live = Some(crate::app::LiveTurn::new(
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
    let session = h.app.resident.current_mut().unwrap();
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
    h.app.resident.current_mut().unwrap().loaded_from = 20;
    request(&mut h, json!({"kind":"load_older"})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["ok"], true);
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
        request(&mut h, intent).await;
        assert_eq!(ack_next(&mut h, &mut output).await["ok"], valid);
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
    let (mut output, _close, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    let (mut output, _close, admission) = phone(&mut h, 1, "phone");
    assert!(admission.await.unwrap());
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
    let session = h.app.resident.current_mut().unwrap();
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
async fn an_intent_is_acked_only_to_its_sender_and_only_a_sign_in_attempt_reaches_every_phone() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.remote.url = Some("http://localhost:8765".into());
    let (mut a, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut b, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
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
    let (mut a, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    received(&mut a);
    h.app
        .resident
        .current_mut()
        .unwrap()
        .allowed_tools
        .insert("bash".into());
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
    let (mut a, _close, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    let (mut b, _close2, admission) = phone(&mut h, 2, "second");
    assert!(admission.await.unwrap());
    let (mut c, _close3, admission) = phone(&mut h, 3, "third");
    assert!(admission.await.unwrap());
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
    h.app.resident.current_mut().unwrap().interaction = Some(crate::app::PendingInteraction {
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

fn messages(output: &mut mpsc::Receiver<Value>) -> Vec<Value> {
    std::iter::from_fn(|| output.try_recv().ok()).collect()
}

/// The next frame the phone receives, running the app until it is produced.
///
/// A frame that never comes fails the test instead of hanging the suite.
async fn next(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Value {
    for _ in 0..64 {
        if let Ok(message) = output.try_recv() {
            return message;
        }
        if output.is_closed() {
            break;
        }
        h.settle().await;
    }
    panic!("the phone never received the frame");
}

/// The phone's answer to an intent, skipping the projections that precede it.
async fn ack_next(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Value {
    crate::testing::real_bounded(async {
        loop {
            let message = next(h, output).await;
            if message["t"] == "ack" {
                return message;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
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
    h.app.resident.current_mut().unwrap().info.title = Some("First".into());
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
    let (a, _close_a, admission) = phone(&mut h, 1, "first phone");
    assert!(admission.await.unwrap());
    let (mut b, _close_b, admission) = phone(&mut h, 2, "second phone");
    assert!(admission.await.unwrap());
    request_from(&mut h, 2, json!({"kind":"open_session", "session_id":"s2"})).await;
    assert_eq!(ack_next(&mut h, &mut b).await["ok"], true);
    h.until(|app| app.live("s2").is_some()).await;
    h.settle().await;
    (h, a, b)
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
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s3");
    h.action(Action::Remote("off".into()));
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s3");
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
    let (mut c, _close_c, admission) = phone(&mut h, 3, "also first");
    assert!(admission.await.unwrap());
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
    let (mut c, _close_c, admission) = phone(&mut h, 3, "new arrival");
    assert!(admission.await.unwrap());
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
    let (mut c, _close_c, admission) = phone(&mut h, 3, "another draft");
    assert!(admission.await.unwrap());
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
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s2");
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
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s1");
    h.fake.release("session.open@s3");
    h.until(|app| app.live("s3").is_some()).await;
    h.settle().await;
    assert_eq!(ack_next(&mut h, &mut a).await["id"], opening);
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s1");
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
    let (mut c, _close_c, admission) = phone(&mut h, 3, "another viewer");
    assert!(admission.await.unwrap());
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
    assert!(h.app.remote.holds_pending(1, &deleting));
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
    app.resident
        .iter_mut()
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

async fn w4_phone() -> (Harness, mpsc::Receiver<Value>) {
    let mut h = w0_phone_session();
    h.app.remote.config = Some(config(h.app.workspace.canonicalize().unwrap()));
    h.app.remote.url = Some("http://localhost:8765".into());
    h.fake.script(
        "session.state@s1",
        state_response(snapshot("s1", crate::testing::LOAD, 1)),
    );
    let (mut output, _closing, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    h.app.sync_remote();
    messages(&mut output);
    (h, output)
}

#[tokio::test(start_paused = true)]
async fn w4_snapshot_blocking_persists_across_phone_reconnect_and_keeps_the_question_actionable() {
    let (mut h, mut output) = w4_phone().await;
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
        h.app.resident.current().unwrap().block_reason,
        Some(crate::protocol::BlockReason::Persistence)
    );
    assert!(messages(&mut output).is_empty());
    h.app.sync_remote();
    assert!(
        messages(&mut output).is_empty(),
        "idle blocking needs no repeated projection"
    );

    h.app.on_msg(Msg::Remote(RemoteEvent::Disconnected(1)));
    let (mut output, _closing, admission) = phone(&mut h, 2, "first");
    assert!(admission.await.unwrap());
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
        h.app.resident.current().unwrap().block_reason,
        Some(crate::protocol::BlockReason::Internal)
    );
    assert_eq!(
        h.app
            .resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .unwrap()
            .interaction_id,
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

async fn called(h: &Harness, method: &str, count: usize) {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.fake.wait_for_calls(method, count),
    )
    .await
    .expect("the Agent call did not reach its gate");
}

async fn text_control(h: &mut Harness, loop_id: &str, interaction_id: Option<&str>) {
    h.fake
        .script_held("turn.wait", waited(loop_id, completed()));
    let pending = interaction_id.map(|id| json!({"loop_id":loop_id, "request_index":0,
        "interaction_id":id, "tool_call_id":"text_call", "tool_name":"ask",
        "kind":{"type":"tool_input", "data":{"prompt":"Your answer?", "answer_kind":"text", "choices":[]}}}));
    let control = h.control(json!({
        "execution":{"type":"running", "loop_id":loop_id, "input":null, "request_index":0, "phase":"waiting_for_input"},
        "pending_interaction":pending,
    }));
    h.fake
        .script("session.state@s1", json!({"snapshot":control.clone()}));
    h.snapshot_event(control).await;
    h.settle().await;
}

#[tokio::test(start_paused = true)]
async fn w4_text_answers_use_the_snapshot_question_and_keep_literal_allowed_text_until_agent_ack() {
    let (mut h, mut output) = w4_phone().await;
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
        assert!(h.app.resident.current().unwrap().allowed_tools.is_empty());
        assert!(
            h.calls("input.push").is_empty(),
            "text answers are not a deny-reason steer"
        );
        assert_eq!(h.app.composer.text(), "conversation draft");
    }
}

#[tokio::test(start_paused = true)]
async fn w4_text_answers_validate_question_identity_and_kind_without_approval_or_steer_fallback() {
    let (mut h, mut output) = w4_phone().await;
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
async fn w4_a_late_text_reply_does_not_clear_a_replacement_or_retarget_a_cancelled_question() {
    let (mut h, mut output) = w4_phone().await;
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
        h.app
            .resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .unwrap()
            .interaction_id,
        "replacement"
    );
    text_control(&mut h, "text_loop", None).await;
    request(&mut h, json!({"kind":"answer", "loop_id":"text_loop", "interaction_id":"replacement", "choice":{"text":"too late"}})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "stale");
    assert_eq!(h.calls("interaction.answer").len(), 1);
    assert!(h.app.resident.current().unwrap().interaction.is_none());
    assert_eq!(h.app.composer.text(), "untouched");
    assert!(h.calls("input.push").is_empty());
}

#[tokio::test(start_paused = true)]
async fn w4_disconnected_text_answers_stay_budgeted_and_never_acknowledge_the_replacement_phone() {
    let (mut h, mut output) = w4_phone().await;
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
    let (mut replacement, _closing, admission) = phone(&mut h, 2, "first");
    assert!(admission.await.unwrap());
    h.fake.release("interaction.answer");
    h.settle().await;
    assert!(messages(&mut output).is_empty());
    assert!(messages(&mut replacement).is_empty());
    assert!(h.app.remote.admits_pending(2, Some(&remaining)));
    assert!(h.calls("input.push").is_empty());
}

#[tokio::test(start_paused = true)]
async fn w4_real_websocket_reconnect_keeps_partial_saved_and_unavailable_bodies_and_live_provenance()
 {
    use crate::app::{recovery::RecoveryStatus, session::BodyRecovery};
    use crate::transcript::{Block, PreservedKey, RequestKey, Saving};
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut h = w0_phone_session();
    text_control(&mut h, "question_loop", Some("still_actionable")).await;
    let session = h.app.resident.current_mut().unwrap();
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
        let session = h.app.resident.current_mut().unwrap();
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
async fn w4_a_real_text_phone_keeps_original_answer_identity_through_a_replacement() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let mut h = w0_phone_session();
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
        h.until(|app| app.remote.holds_pending(app.remote.phones[0].connection.id, "original_op")).await;
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
        h.until(|app| !app.remote.holds_pending(app.remote.phones[0].connection.id, "original_op")).await;
        h.app.sync_remote();
        let ack = frame_of_type(&mut socket, "ack").await;
        assert_eq!(ack["id"], "original_op");
        assert_eq!(ack["ok"], true);
        assert_eq!(h.app.resident.current().unwrap().interaction.as_ref().unwrap().interaction_id, "replacement");
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
async fn w4_an_accepted_start_failure_keeps_the_new_input_and_the_actual_failed_head() {
    let (mut h, mut output) = w4_phone().await;
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
async fn w4_agent_backed_settings_and_compaction_share_the_pending_limit_and_wait_for_outcomes() {
    let (mut h, mut output) = w4_phone().await;
    h.fake
        .script_held("session.update", json!({"session":session_info("s1")}));
    let mut ids = Vec::new();
    for _ in 0..crate::remote::PendingIntents::PER_CONNECTION {
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
        h.app.resident.current().unwrap().compaction.is_none(),
        "dispatch cannot write compaction control"
    );
    finished.await.unwrap();
    called(&h, "session.compact", 1).await;
    h.snapshot_event(compacting).await;
    h.settle().await;
    assert!(messages(&mut output).is_empty());
    assert_eq!(
        h.app
            .resident
            .current()
            .unwrap()
            .compaction
            .as_ref()
            .unwrap()
            .operation_id,
        "tui-1"
    );
    h.fake.release("session.compact");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], compact);
    assert_eq!(ack["ok"], true);
    assert!(ack.get("input_id").is_none());
}

#[tokio::test(start_paused = true)]
async fn w4_reusing_an_inflight_operation_id_revokes_delivery_instead_of_answering_the_wrong_operation()
 {
    let (mut h, mut output) = w4_phone().await;
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
    assert!(h.app.remote.holds_pending(1, &id));
    h.fake.release("input.push");
    h.settle().await;
    assert_eq!(h.calls("input.push").len(), 1);
    assert!(messages(&mut output).is_empty());
    assert!(!h.app.remote.holds_pending(1, &id));
}

#[tokio::test(start_paused = true)]
async fn w4_a_receipt_for_another_load_is_unknown_not_a_success_for_the_current_operation() {
    let (mut h, mut output) = w4_phone().await;
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
async fn w4_an_open_reply_for_another_known_session_never_adopts_or_refocuses_it() {
    let (mut h, mut output) = w4_phone().await;
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
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s1");
    assert_eq!(h.app.remote.phones[0].view.session_id(), Some("s1"));
    assert_eq!(h.calls("session.open")[0]["session_id"], "wanted");
}

#[tokio::test(start_paused = true)]
async fn w4_accepted_inputs_coalesce_control_reads_without_waiting_for_them_to_ack() {
    let (mut h, mut output) = w4_phone().await;
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
async fn w4_transport_errors_are_unknown_but_explicit_rejections_stay_rejected() {
    let (mut h, mut output) = w4_phone().await;
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
        assert!(h.app.resident.current().unwrap().returned.is_empty());
    }
    assert_eq!(
        h.calls("input.push").len(),
        3,
        "there is no automatic resend"
    );
}

#[tokio::test(start_paused = true)]
async fn w4_late_acceptance_names_the_original_load_without_touching_the_reopened_instance() {
    let (mut h, mut output) = w4_phone().await;
    h.fake.script_held("input.push", pushed(vec![]));
    let id = request(&mut h, json!({"kind":"submit", "text":"same"})).await;
    called(&h, "input.push", 1).await;
    let old = h.app.resident.current().unwrap();
    let replacement = crate::app::Session::new(old.info.clone(), old.epoch + 1, "reopened".into());
    h.app.resident.remove("s1");
    h.app.show_session(Some(replacement));
    h.fake.release("input.push");
    let ack = ack_next(&mut h, &mut output).await;
    assert_eq!(ack["id"], id);
    assert_eq!(ack["load"], "load_1");
    assert_eq!(h.app.resident.current().unwrap().load, "reopened");
    assert!(
        h.calls("session.state").is_empty(),
        "an old epoch cannot refresh the new instance"
    );
    assert!(!h.app.is_busy());
}

#[tokio::test(start_paused = true)]
async fn w4_disconnected_late_ack_and_edit_text_never_reach_a_replacement_device() {
    let (mut h, mut output) = w4_phone().await;
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
        h.app.remote.holds_pending(1, &first),
        "an in-flight operation remains budgeted"
    );
    let (mut replacement, _closing, admission) = phone(&mut h, 2, "first");
    assert!(admission.await.unwrap());
    h.fake.release("input.push");
    h.fake.release("input.remove");
    h.settle().await;
    assert!(messages(&mut output).is_empty());
    assert!(messages(&mut replacement).is_empty());
    assert!(!h.app.remote.holds_pending(1, &first));
    assert!(!h.app.remote.holds_pending(1, &edit));
    let second = request_from(&mut h, 2, json!({"kind":"submit", "text":"same"})).await;
    assert_ne!(first, second);
    assert_eq!(ack_next(&mut h, &mut replacement).await["id"], second);
    assert!(h.app.composer.is_empty());
}

#[tokio::test(start_paused = true)]
async fn w4_cancel_answer_and_recall_ack_only_after_the_exact_agent_operation() {
    let (mut h, mut output) = w4_phone().await;
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
    assert!(h.app.resident.current().unwrap().allowed_tools.is_empty());
    h.fake.release("interaction.answer");
    assert_eq!(ack_next(&mut h, &mut output).await["id"], answer);
    assert!(
        h.app
            .resident
            .current()
            .unwrap()
            .allowed_tools
            .contains("bash")
    );
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
async fn w4_missing_load_and_stale_compaction_identity_are_refused_before_rpc() {
    let (mut h, mut output) = w4_phone().await;
    request(&mut h, json!({"kind":"submit", "text":"same", "load":null})).await;
    assert_eq!(ack_next(&mut h, &mut output).await["error"], "missing_load");
    assert!(h.calls("input.push").is_empty());
    h.app.resident.current_mut().unwrap().compaction = Some(serde_json::from_value(json!({
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
async fn w4_pending_count_and_expanded_text_byte_limits_are_busy_before_side_effects() {
    let (mut h, mut output) = w4_phone().await;
    h.fake.script_held("input.push", pushed(vec![]));
    for _ in 0..crate::remote::PendingIntents::PER_CONNECTION {
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
async fn w4_global_count_and_byte_budgets_remain_charged_after_disconnect_and_precede_create() {
    let (mut h, mut output) = w4_phone().await;
    let held = |id: usize, text: String| crate::remote::PendingIntent {
        id: id.to_string(),
        view_epoch: "old".into(),
        session_id: Some("s1".into()),
        load: Some("load_1".into()),
        epoch: Some(1),
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

#[tokio::test(start_paused = true)]
async fn w4_overflow_reconnect_replaces_the_whole_projection_and_view_epochs_never_repeat() {
    let (mut h, mut output) = w4_phone().await;
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
    h.app
        .resident
        .current_mut()
        .unwrap()
        .transcript
        .push(crate::transcript::Block::User {
            text: "while disconnected".into(),
            kind: crate::protocol::read::UserKind::Prompt,
            at: None,
        });
    h.mirror(vec![entry("still_queued", "follow_up", "same")], false);
    let (mut replacement, _close, admission) = phone(&mut h, 2, "first");
    assert!(admission.await.unwrap());
    h.app.sync_remote();
    let snapshot = replacement.try_recv().unwrap();
    assert_eq!(snapshot["t"], "snapshot");
    assert_eq!(snapshot["blocks"][0]["text"], "while disconnected");
    assert_eq!(snapshot["state"]["session"]["load"], "load_1");
    assert_eq!(snapshot["state"]["queue"][0]["id"], "still_queued");
    assert!(replacement.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn w4_a_stale_draft_create_sends_to_its_created_session_without_refocusing_the_phone() {
    let (mut h, mut output) = w4_phone().await;
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
    assert_eq!(h.app.resident.current().unwrap().info.session_id, "s1");
    assert_eq!(h.calls("input.push")[0]["session_id"], "created");
    assert_eq!(h.calls("input.push")[0]["load"], "created_load");
    assert_eq!(h.calls("input.push")[0]["text"], "original");
}
