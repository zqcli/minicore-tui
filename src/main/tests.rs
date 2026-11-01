use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;

use super::*;
use minicore_tui::app::{Action, Session, SessionOp};
use minicore_tui::protocol::AgentEvent;
use minicore_tui::transcript::Block;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

struct SplitDispatch {
    app: App,
    client: Arc<Client>,
    incoming: mpsc::Receiver<Incoming>,
    messages: mpsc::UnboundedReceiver<Msg>,
    posted: mpsc::UnboundedReceiver<()>,
    agent_open: bool,
    requests: BufReader<ReadHalf<DuplexStream>>,
    output: WriteHalf<DuplexStream>,
    forward: JoinHandle<()>,
    control: Value,
}

impl SplitDispatch {
    fn new() -> Self {
        let (client_pipe, agent_pipe) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_pipe);
        let (agent_read, output) = tokio::io::split(agent_pipe);
        let (incoming_tx, incoming) = mpsc::channel(256);
        let client = Client::start(client_write, client_read, tokio::io::empty(), incoming_tx);
        let (app_tx, mut app_rx) = mpsc::unbounded_channel();
        let (message_tx, messages) = mpsc::unbounded_channel();
        let (posted_tx, posted) = mpsc::unbounded_channel();
        let forward = tokio::spawn(async move {
            while let Some(message) = app_rx.recv().await {
                if message_tx.send(message).is_err() || posted_tx.send(()).is_err() {
                    break;
                }
            }
        });
        let workspace = std::env::temp_dir();
        let mut app = App::new(client.clone(), app_tx, workspace.clone());
        let info = serde_json::from_value(json!({
            "session_id": "s1", "title": "Test", "profile": "coding",
            "workspace": workspace, "model": "mock", "reasoning": "auto",
            "loaded": true, "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        app.session = Some(Session::new(info, 1));
        let session = app.session.as_mut().unwrap();
        session.load = Some("load_1".into());
        session.recovery.begin("load_1", 1);
        app.ready = true;
        Self {
            app,
            client,
            incoming,
            messages,
            posted,
            agent_open: true,
            requests: BufReader::new(agent_read),
            output,
            forward,
            control: json!({"session_id":"s1","load":"load_1","revision":0,"execution":{"type":"none"},"queue":queue(vec![]),"completions":{"through_seq":0,"retained_from_seq":1,"entries":[],"truncated":false}}),
        }
    }

    async fn next_request(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(self.requests.read_line(&mut line).await.unwrap(), 0);
        serde_json::from_str(&line).unwrap()
    }

    // Command receipts can refresh control before the request under test.
    async fn request(&mut self, method: &str) -> Value {
        loop {
            let request = self.next_request().await;
            if request["method"] == method {
                return request;
            }
            assert_eq!(
                request["method"], "session.state",
                "unexpected request before {method}"
            );
            let id = request["id"].clone();
            self.control["revision"] = json!(self.control["revision"].as_u64().unwrap() + 1);
            self.send(json!({"jsonrpc": "2.0", "id": id, "result": {
                "session_id": "s1", "status": "idle", "snapshot":self.control
            }}))
            .await;
        }
    }

    fn install(&mut self, execution: Value) {
        self.control["revision"] = json!(self.control["revision"].as_u64().unwrap() + 1);
        self.control["execution"] = execution;
        self.app
            .apply_snapshot(serde_json::from_value(self.control.clone()).unwrap());
    }

    async fn send(&mut self, frame: Value) {
        self.output
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        self.output.flush().await.unwrap();
    }

    async fn posted(&mut self) {
        self.posted
            .recv()
            .await
            .expect("the reducer message was posted");
    }

    async fn select(&mut self) -> Msg {
        next_backend_message(&mut self.incoming, &mut self.messages, &mut self.agent_open)
            .await
            .expect("a backend message")
    }

    async fn ordered_frames(&mut self, first: Value, second: Value) {
        let client = self.client.clone();
        let fence = tokio::spawn(async move { client.call_raw("agent.ping", &()).await });
        let request = self.request("agent.ping").await;
        self.send(first).await;
        self.send(second).await;
        self.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {}}))
            .await;
        fence.await.unwrap().expect("the ordered fence response");
        self.posted().await;
        // The fence proves both dispatch channels are ready before application.
        assert_eq!(
            self.incoming.len(),
            1,
            "the event is ready before application"
        );
        assert!(
            !self.messages.is_empty(),
            "the reply is ready before application"
        );
    }
}

impl Drop for SplitDispatch {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

fn event(kind: &str, mut data: Value) -> Value {
    data["meta"]["load"] = json!("load_1");
    json!({"jsonrpc": "2.0", "method": "agent.event", "params": {"type": kind, "data": data}})
}

fn queue(items: Vec<Value>) -> Value {
    json!({"items": items, "paused": false})
}

fn entry(id: &str, text: &str) -> Value {
    json!({"id": id, "mode": "steer", "text": text})
}

#[tokio::test(start_paused = true)]
async fn dispatch_keeps_event_priority_and_disables_a_closed_agent() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (incoming_tx, mut incoming) = mpsc::channel(4);
        let (message_tx, mut messages) = mpsc::unbounded_channel();
        let event: AgentEvent = serde_json::from_value(json!({
            "type": "input_queue", "data": {
                "meta": {"session_id": "s1", "dropped_before": 0}, "queue": queue(vec![])
            }
        }))
        .unwrap();
        incoming_tx
            .send(Incoming::Event {
                event: Box::new(event),
                bytes: 0,
            })
            .await
            .unwrap();
        message_tx.send(Msg::Flash("message".into())).unwrap();
        let mut agent_open = true;
        let first = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
        assert!(matches!(first, Some(Msg::Agent { .. })));
        let second = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
        assert!(matches!(second, Some(Msg::Flash(_))));

        incoming_tx
            .send(Incoming::Closed { status: None })
            .await
            .unwrap();
        message_tx.send(Msg::Flash("after close".into())).unwrap();
        let closed = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
        assert!(matches!(closed, Some(Msg::AgentClosed(reason)) if reason == "agent closed"));
        assert!(!agent_open);
        let next = next_backend_message(&mut incoming, &mut messages, &mut agent_open).await;
        assert!(matches!(next, Some(Msg::Flash(_))));
    })
    .await
    .expect("dispatcher timed out");
}

#[tokio::test(start_paused = true)]
async fn receipts_cannot_restore_consumed_inputs_or_completed_execution_in_either_wire_order() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        for snapshot_first in [false, true] {
            let mut h = SplitDispatch::new();
            h.control["queue"] = queue(vec![entry("i1", "accepted then finished")]);
            h.install(json!({"type":"none"}));
            h.app.composer.set_text("accepted then finished");
            h.app.on_action(Action::Submit);
            let push = h.request("input.push").await;
            assert_eq!(push["params"]["text"], "accepted then finished");
            let receipt = json!({"jsonrpc":"2.0","id":push["id"],"result":{
                "id":"i1","accepted":true,"load":"load_1",
                "started":{"turn":{"session_id":"s1","loop_id":"A"},"input":entry("i1","accepted then finished")},
                "queue":queue(vec![entry("i1","accepted then finished")])
            }});
            h.control["revision"] = json!(2);
            h.control["queue"] = queue(vec![]);
            h.control["completions"] = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{
                "seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed",
                "persistence":"failed","availability":"unavailable","unavailable_reason":"expired"
            }]});
            let snapshot = event("session_snapshot", json!({
                "meta":{"session_id":"s1","dropped_before":0},"snapshot":h.control
            }));
            let (first, second) = if snapshot_first { (snapshot, receipt) } else { (receipt, snapshot) };
            h.ordered_frames(first, second).await;
            let snapshot = h.select().await;
            assert!(matches!(&snapshot, Msg::Agent { event, .. } if matches!(**event, AgentEvent::SessionSnapshot { .. })));
            h.app.on_msg(snapshot);
            let receipt = h.select().await;
            assert!(matches!(&receipt, Msg::LocalPushed(..)));
            h.app.on_msg(receipt);
            let session = h.app.session.as_ref().unwrap();
            assert!(session.queue.items.is_empty());
            assert!(session.live.is_none());
            assert_eq!(session.observed_loop(), None);
            assert_eq!(session.recovery.observed_through, 1);
        }
    }).await.expect("FIFO dispatch did not finish");
}

#[cfg(feature = "e2e-observe")]
#[tokio::test(start_paused = true)]
async fn application_pause_leaves_the_stdio_reader_running_and_applies_fifo_after_release() {
    tokio::time::timeout(TEST_TIMEOUT,async {
        let mut h = SplitDispatch::new();
        h.install(json!({"type":"none"}));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let mut gate = application_gate::ApplicationGate::new(path.clone()).unwrap();
        std::fs::write(path.join("arm"),b"").unwrap();
        h.control["revision"] = json!(2);
        h.control["queue"] = queue(vec![entry("first","first observation")]);
        let held:AgentEvent = serde_json::from_value(json!({"type":"session_snapshot","data":{"meta":{"session_id":"s1","load":"load_1","dropped_before":0},"snapshot":h.control}})).unwrap();
        let held = Msg::Agent { event:Box::new(held),bytes:0 };
        h.control["revision"] = json!(3);
        h.control["queue"] = queue(vec![]);
        let later = event("session_snapshot",json!({"meta":{"session_id":"s1","load":"load_1","dropped_before":0},"snapshot":h.control}));
        let read_while_paused = {
        let pausing = gate.before_apply(&held,&mut h.app);
        tokio::pin!(pausing);
        tokio::select! { result = &mut pausing => panic!("gate returned before release: {result:?}"), _ = async {
            while !path.join("ready").exists() { tokio::task::yield_now().await; }
        } => {} }
        h.output.write_all(format!("{later}\n").as_bytes()).await.unwrap();
        h.output.flush().await.unwrap();
        let read_while_paused = h.incoming.recv().await.unwrap();
        assert!(matches!(&read_while_paused,Incoming::Event { event,.. } if matches!(**event,AgentEvent::SessionSnapshot { .. })));
        std::fs::write(path.join("release"),b"").unwrap();
        pausing.await.unwrap();
        read_while_paused
        };
        h.app.on_msg(held);
        assert_eq!(h.app.session.as_ref().unwrap().queue.items[0].id,"first");
        let Incoming::Event { event,bytes } = read_while_paused else { unreachable!() };
        h.app.on_msg(Msg::Agent { event,bytes });
        assert_eq!(h.app.session.as_ref().unwrap().recovery.revision,3);
        assert!(h.app.session.as_ref().unwrap().queue.items.is_empty());
    }).await.expect("application gate did not release");
}

#[cfg(feature = "e2e-observe")]
#[tokio::test(start_paused = true)]
async fn an_unreleased_application_gate_times_out_without_applying_the_held_message() {
    let mut h = SplitDispatch::new();
    h.install(json!({"type":"none"}));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_path_buf();
    let mut gate = application_gate::ApplicationGate::new(path.clone()).unwrap();
    std::fs::write(path.join("arm"), b"").unwrap();
    let held = Msg::Session(1, Box::new(SessionOp::Done(Ok(json!({})))));
    let error = gate.before_apply(&held, &mut h.app).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(path.join("ready").exists());
    assert_eq!(h.app.session.as_ref().unwrap().recovery.revision, 1);
    std::fs::write(path.join("release"), b"").unwrap();
    assert!(
        application_gate::ApplicationGate::new(path).is_err(),
        "stale gate state cannot be reused as a fresh acceptance run"
    );
}

#[tokio::test(start_paused = true)]
async fn a_successors_first_delta_survives_dispatch_before_the_wait_reply() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut h = SplitDispatch::new();
        h.app.composer.set_text("first prompt");
        h.app.on_action(Action::Submit);
        let push = h.request("input.push").await;
        h.send(json!({"jsonrpc": "2.0", "id": push["id"], "result": {
            "id": "i1", "queue": queue(vec![]),
            "started": {"turn": {"session_id": "s1", "loop_id": "A"}, "input": entry("i1", "first prompt")}
        }})).await;
        h.posted().await;
        let started = h.select().await;
        h.app.on_msg(started);
        assert!(h.app.session.as_ref().unwrap().live.is_none(), "a receipt is not execution authority");
        h.install(json!({"type":"running","loop_id":"A","input":entry("i1","first prompt"),"request_index":0,"phase":"running_model"}));
        assert_eq!(h.app.session.as_ref().unwrap().live.as_ref().unwrap().turn.loop_id, "A");
        let wait = h.request("turn.wait").await;
        assert_eq!(wait["params"]["loop_id"], "A");
        let response = json!({"jsonrpc": "2.0", "id": wait["id"], "result": {
            "turn": {"session_id": "s1", "loop_id": "A"}, "outcome": {"type": "completed"},
            "queue": queue(vec![]), "next": {
                "turn": {"session_id": "s1", "loop_id": "B"}, "input": entry("i2", "second prompt")
            }
        }});
        let event = event("output_delta", json!({
            "meta": {"session_id": "s1", "dropped_before": 0},
            "turn": {"session_id": "s1", "loop_id": "B"}, "request_index": 0,
            "channel": "text", "delta": "first part of B"
        }));
        h.ordered_frames(response, event).await;

        let first = h.select().await;
        assert!(
            matches!(&first, Msg::Agent { event, .. } if matches!(**event, AgentEvent::OutputDelta { .. }))
        );
        h.app.on_msg(first);
        assert!(!h.app.transcript_blocks().iter().any(|block|block.loop_id() == Some("B")), "unknown B stays invisible until identity confirmation");
        assert!(h.app.session.as_ref().unwrap().early_bytes > 0);
        // A receipt's control refresh may arrive before the wait reply.
        let second = loop {
            let next = h.select().await;
            if matches!(&next, Msg::Session(1, op) if matches!(**op, SessionOp::Waited(..))) {
                break next;
            }
            h.app.on_msg(next);
        };
        h.app.on_msg(second);
        h.control["completions"] = json!({"through_seq":1,"retained_from_seq":1,"truncated":false,"entries":[{"seq":1,"loop_id":"A","completion_kind":"reported","outcome_kind":"completed","persistence":"persisted","availability":"stored","next_loop_id":"B"}]});
        h.install(json!({"type":"running","loop_id":"B","input":entry("i2","second prompt"),"request_index":0,"phase":"running_model"}));
        assert!(h.app.transcript_blocks().iter().any(|block| matches!(block,
            Block::Assistant { key, text, .. } if key.loop_id == "B" && text == "first part of B"
        )), "the successor's first output must survive until B is installed");
    }).await.expect("split dispatch timed out");
}
