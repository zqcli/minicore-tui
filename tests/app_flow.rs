//! Cross-module app flow tests: bootstrap, create/open, multi-turn durable
//! reconciliation, background sessions, gap handling, and the shutdown
//! state machine. Fixtures are loaded through the production `parse_frame`
//! DTO path and every response is correlated by the id the app allocated,
//! exactly like the real event loop would.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use minicore_tui::app::{App, CliPrefs, ConnectionState};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{
    IncomingFrame, OutgoingRequest, Reasoning, RpcError as WireError, RpcErrorData, RpcResponse,
};
use minicore_tui::state::{Dock, TranscriptBlock};

fn fixture_result(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/protocol/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["result"].clone()
}

/// Fake-transport driver: applies `AppEvent`s through `App::update`, keeps
/// the resulting RPC requests in wire order, and feeds responses back
/// correlated by request id. Mirrors the production command executor.
struct Driver {
    app: App,
    queue: VecDeque<OutgoingRequest>,
    saw_exit: bool,
    saw_kill: bool,
}

impl Driver {
    fn new(app: App) -> Self {
        Self {
            app,
            queue: VecDeque::new(),
            saw_exit: false,
            saw_kill: false,
        }
    }

    fn step(&mut self, event: AppEvent) {
        for command in self.app.update(event) {
            match command {
                AppCommand::Rpc(request) => self.queue.push_back(request),
                AppCommand::Exit => self.saw_exit = true,
                AppCommand::KillChild => self.saw_kill = true,
            }
        }
    }

    fn step_raw(&mut self, bytes: &str) {
        let frame = minicore_tui::protocol::parse_frame(bytes.as_bytes()).unwrap();
        self.step(AppEvent::Rpc(RpcEvent::Frame(frame)));
    }

    fn request(&mut self, method: &str) -> OutgoingRequest {
        let index = self
            .queue
            .iter()
            .position(|request| request.method == method)
            .unwrap_or_else(|| panic!("expected a `{method}` request in flight"));
        self.queue.remove(index).unwrap()
    }

    fn respond(&mut self, request: OutgoingRequest, result: Value) {
        self.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
        ))));
    }

    fn respond_error(&mut self, request: OutgoingRequest, kind: &str, message: &str) {
        self.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: None,
                error: Some(WireError {
                    code: -32000,
                    message: message.to_owned(),
                    data: Some(RpcErrorData {
                        kind: kind.to_owned(),
                        retryable: false,
                    }),
                }),
            },
        ))));
    }

    fn respond_fixture(&mut self, request: OutgoingRequest, fixture: &str) {
        self.respond(request, fixture_result(fixture));
    }

    /// Pop the in-flight request for `method` and respond with the fixture;
    /// avoids a nested mutable borrow of `self`.
    fn respond_fixture_by_method(&mut self, method: &str, fixture: &str) {
        let request = self.request(method);
        self.respond_fixture(request, fixture);
    }

    /// Pop the in-flight request for `method` and respond with `result`.
    fn respond_by_method(&mut self, method: &str, result: Value) {
        let request = self.request(method);
        self.respond(request, result);
    }

    fn assert_no_requests(&self) {
        assert!(
            self.queue.is_empty(),
            "expected no outstanding requests, got {:?}",
            self.queue.iter().map(|r| r.method).collect::<Vec<_>>()
        );
    }
}

fn ready(bootstrap: &mut bool, driver: &mut Driver) {
    if *bootstrap {
        return;
    }
    *bootstrap = true;
    driver.step(AppEvent::Bootstrap);
    // Answer out of order with the fixture payloads; the app correlates by id.
    let ping = driver.request("agent.ping");
    let models = driver.request("model.list");
    let profiles = driver.request("profile.list");
    let sessions = driver.request("session.list");
    driver.respond(ping, json!({"version": "0.2.0"}));
    driver.respond_fixture(models, "model-list.json");
    driver.respond_fixture(profiles, "profile-list.json");
    driver.respond_fixture(sessions, "session-list.json");
    assert_eq!(
        driver.app.connection,
        ConnectionState::Ready,
        "bootstrap completes after all four responses"
    );
}

fn turn_result(session: &str, instance: &str, turn: &str) -> Value {
    json!({"turn": {"session_id": session, "instance_id": instance, "turn_id": turn}})
}

/// Shutdown-path tests only need the fact that the child left; the exit
/// status itself is exercised by the real-process tests (`tests/rpc_io.rs`).
fn exit() -> RpcEvent {
    RpcEvent::Exited(None)
}

fn session_info(session_id: &str, instance: &str) -> Value {
    json!({
        "session_id": session_id,
        "title": null,
        "profile": "coding",
        "workspace": "/ws/app",
        "model": "gpt-4o",
        "reasoning": "high",
        "loaded": true,
        "instance_id": instance,
        "created_at": "2026-09-01T08:00:00.000Z",
        "updated_at": "2026-09-01T08:00:00.000Z"
    })
}

fn transcript_tail(blocks: &[TranscriptBlock]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            TranscriptBlock::User(card) => card.text.clone(),
            TranscriptBlock::Assistant(card) => card
                .parts
                .iter()
                .filter_map(|part| match part {
                    minicore_tui::state::AssistantPart::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            TranscriptBlock::Tool(_)
            | TranscriptBlock::Summary(_)
            | TranscriptBlock::Terminal(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn dirty_flag_tracks_updates_and_rendered_clears_it() {
    let mut app = App::new(PathBuf::from("/ws"));
    assert!(!app.dirty, "a fresh app never needs a draw");
    app.update(AppEvent::SetTheme(minicore_tui::theme::ThemeKind::Light));
    assert!(app.dirty);
    app.update(AppEvent::Rendered);
    assert!(!app.dirty);
    app.update(AppEvent::Tick);
    assert!(app.dirty);
}

#[test]
fn next_tick_is_none_when_idle_and_armed_for_spinner_notices_and_ctrl_c() {
    let mut app = App::new(PathBuf::from("/ws"));
    assert_eq!(app.next_tick(), None, "idle: no timer needed");
    // The double-Ctrl+C window arms a tick so the stale anchor expires.
    let _ = app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )));
    let tick = app
        .next_tick()
        .expect("the double-Ctrl+C window arms a tick");
    assert!(
        tick <= Duration::from_secs(1),
        "the window expiry is at most 1s, got {tick:?}"
    );
}

#[test]
fn bootstrap_with_explicit_workspace_opens_a_prefilled_new_session() {
    let app = App::with_cli_prefs(
        PathBuf::from("/ws/explicit"),
        CliPrefs {
            profile: Some("coding".to_owned()),
            model: Some("gpt-4o".to_owned()),
            reasoning: Some(Reasoning::High),
            open_new_session_on_ready: true,
        },
    );
    let mut driver = Driver::new(app);
    driver.step(AppEvent::Bootstrap);
    driver.respond_by_method("agent.ping", json!({"version": "0.2.0"}));
    driver.respond_fixture_by_method("profile.list", "profile-list.json");
    driver.respond_fixture_by_method("model.list", "model-list.json");
    driver.respond_fixture_by_method("session.list", "session-list.json");

    assert_eq!(driver.app.connection, ConnectionState::Ready);
    match &driver.app.dock {
        Dock::NewSession(draft) => {
            assert_eq!(draft.workspace, "/ws/explicit");
            assert_eq!(draft.profile, "coding");
            assert_eq!(draft.model, "gpt-4o");
            assert_eq!(draft.reasoning, Reasoning::High);
        }
        other => panic!("expected a pre-filled new-session form, got {other:?}"),
    }
}

#[test]
fn two_turns_in_one_session_reconcile_durable_text() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    let mut booted = false;
    ready(&mut booted, &mut driver);

    // Create the session from the fixture; the response also requests the
    // session state and the first durable transcript page.
    driver.step(AppEvent::CreateSession {
        workspace: "/ws/app".to_owned(),
        profile: None,
        model: None,
        reasoning: None,
        title: None,
    });
    let create = driver.request("session.create");
    driver.respond_fixture(create, "session-create.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_6f3c1a"));

    // Turn 1: send -> the same-update wait -> reconcile chain.
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_6f3c1a".to_owned(),
        text: "why is parse failing?".to_owned(),
    });
    let send = driver.request("turn.send");
    driver.respond(send, turn_result("ses_6f3c1a", "ins_9fe2", "trn_77aa"));
    let wait = driver.request("turn.wait");
    driver.respond_fixture(wait, "turn-wait.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");
    driver.assert_no_requests();

    let view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    assert!(
        view.live.is_none(),
        "the live turn ends after reconciliation"
    );
    let text = transcript_tail(&view.transcript.blocks);
    assert!(
        text.contains("why is parse failing?"),
        "durable user text: {text}"
    );
    assert!(
        text.contains("Fixed: the parser now handles the offset"),
        "durable assistant text: {text}"
    );

    // Turn 2: a fresh send/wait/reconcile cycle continues the same session.
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_6f3c1a".to_owned(),
        text: "second round".to_owned(),
    });
    driver.respond_by_method(
        "turn.send",
        turn_result("ses_6f3c1a", "ins_9fe2", "trn_78bb"),
    );
    driver.respond_by_method(
        "turn.wait",
        json!({
            "turn_id": "trn_78bb",
            "terminal": "completed",
            "usage": {}
        }),
    );
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_by_method(
        "session.transcript",
        json!({
            "entries": [
                {"user_message": {
                    "seq": 6, "turn_id": "trn_78bb", "text": "second round",
                    "execution": {"model": "gpt-4o", "reasoning": "high", "max_tool_rounds": 8},
                    "created_at": "2026-09-01T08:31:00.000Z"
                }},
                {"assistant_message": {
                    "seq": 7, "turn_id": "trn_78bb", "model": "gpt-4o",
                    "text": "round two answer", "reasoning": null, "tool_calls": [],
                    "usage": {}, "finish_reason": "stop",
                    "created_at": "2026-09-01T08:31:01.000Z"
                }}
            ],
            "next_after": null,
            "observed_head": 7,
            "complete": true
        }),
    );
    driver.assert_no_requests();

    let view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    let text = transcript_tail(&view.transcript.blocks);
    assert!(text.contains("second round"));
    assert!(text.contains("round two answer"));
}

#[test]
fn background_session_keeps_receiving_deltas_while_another_runs() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    let mut booted = false;
    ready(&mut booted, &mut driver);

    // Session A active with a live turn.
    driver.step(AppEvent::CreateSession {
        workspace: "/ws/app".to_owned(),
        profile: None,
        model: None,
        reasoning: None,
        title: None,
    });
    driver.respond_fixture_by_method("session.create", "session-create.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_6f3c1a".to_owned(),
        text: "turn on A".to_owned(),
    });
    driver.respond_by_method(
        "turn.send",
        turn_result("ses_6f3c1a", "ins_9fe2", "trn_77aa"),
    );
    // turn.wait for A is now pending; switch to B before finishing.
    let wait_a = driver.request("turn.wait");

    driver.step(AppEvent::OpenSession {
        session_id: "ses_3c4d".to_owned(),
    });
    driver.respond_by_method(
        "session.open",
        json!({"session": session_info("ses_3c4d", "ins_4d")}),
    );
    driver.respond_by_method(
        "session.state",
        json!({
            "session_id": "ses_3c4d", "instance_id": "ins_4d", "status": "idle",
            "health": "healthy", "active_turn": null, "pending_interaction": null,
            "conversation_seq": 0, "last_terminal": null
        }),
    );
    driver.respond_by_method(
        "session.transcript",
        json!({"entries": [], "next_after": null, "observed_head": 0, "complete": true}),
    );
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_3c4d"));

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_3c4d".to_owned(),
        text: "turn on B".to_owned(),
    });
    driver.respond_by_method("turn.send", turn_result("ses_3c4d", "ins_4d", "trn_9b"));

    // A's output delta arrives while B is the active session.
    driver.step_raw(
        r#"{"jsonrpc":"2.0","method":"agent.event","params":{"type":"output_delta","data":{
            "turn":{"session_id":"ses_6f3c1a","instance_id":"ins_9fe2","turn_id":"trn_77aa"},
            "channel":"text","delta":"background delta for A",
            "meta":{"session_id":"ses_6f3c1a","instance_id":"ins_9fe2","dropped_before":0}}}}"#,
    );
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_3c4d"));
    let a_view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    let a_live_text = a_view
        .live
        .as_ref()
        .map(|live| live.text.clone())
        .unwrap_or_else(|| "<no live>".to_owned());
    assert!(
        a_live_text.contains("background delta for A"),
        "background deltas still land in the non-active session; live text: {a_live_text:?}"
    );

    // Finish B, then A's wait still resolves and both transcripts settle.
    driver.respond_by_method(
        "turn.wait",
        json!({"turn_id": "trn_9b", "terminal": "completed", "usage": {}}),
    );
    driver.respond_by_method(
        "session.state",
        json!({
            "session_id": "ses_3c4d", "instance_id": "ins_4d", "status": "idle",
            "health": "healthy", "active_turn": null, "pending_interaction": null,
            "conversation_seq": 0, "last_terminal": null
        }),
    );
    driver.respond_by_method(
        "session.transcript",
        json!({"entries": [], "next_after": null, "observed_head": 0, "complete": true}),
    );
    driver.respond(
        wait_a,
        json!({"turn_id": "trn_77aa", "terminal": "completed", "usage": {}}),
    );
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");
    driver.assert_no_requests();
}

#[test]
fn event_gap_sets_the_gap_flag_and_wait_reconciliation_heals_it() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    let mut booted = false;
    ready(&mut booted, &mut driver);

    driver.step(AppEvent::CreateSession {
        workspace: "/ws/app".to_owned(),
        profile: None,
        model: None,
        reasoning: None,
        title: None,
    });
    driver.respond_fixture_by_method("session.create", "session-create.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_6f3c1a".to_owned(),
        text: "gap me".to_owned(),
    });
    driver.respond_by_method(
        "turn.send",
        turn_result("ses_6f3c1a", "ins_9fe2", "trn_77aa"),
    );
    let wait = driver.request("turn.wait");

    // A dropped event marks the gap on the live turn.
    driver.step_raw(
        r#"{"jsonrpc":"2.0","method":"agent.event","params":{"type":"output_delta","data":{
            "turn":{"session_id":"ses_6f3c1a","instance_id":"ins_9fe2","turn_id":"trn_77aa"},
            "channel":"text","delta":"lost then found",
            "meta":{"session_id":"ses_6f3c1a","instance_id":"ins_9fe2","dropped_before":4}}}}"#,
    );
    let a_view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    assert!(a_view.event_gap, "dropped events set the session gap flag");

    // Wait response starts a reconcile chain; its completion clears the gap.
    driver.respond_fixture(wait, "turn-wait.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");
    driver.assert_no_requests();
    let a_view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    assert!(!a_view.event_gap, "the reconcile chain heals the gap");
    assert!(a_view.live.is_none());
}

#[test]
fn shutdown_blocks_further_requests_and_exits_after_the_child_is_gone() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    let mut booted = false;
    ready(&mut booted, &mut driver);

    driver.step(AppEvent::CreateSession {
        workspace: "/ws/app".to_owned(),
        profile: None,
        model: None,
        reasoning: None,
        title: None,
    });
    driver.respond_fixture_by_method("session.create", "session-create.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");

    // A turn is in flight; the send was answered and a wait is pending.
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_6f3c1a".to_owned(),
        text: "shutdown race".to_owned(),
    });
    driver.respond_by_method(
        "turn.send",
        turn_result("ses_6f3c1a", "ins_9fe2", "trn_77aa"),
    );
    let wait = driver.request("turn.wait");

    // The user quits; agent.shutdown goes out and no new work follows.
    driver.step(AppEvent::ShutdownRequested);
    let shutdown = driver.request("agent.shutdown");
    assert_eq!(driver.app.connection, ConnectionState::ShuttingDown);

    // The in-flight wait response lands after shutdown: state may update,
    // but no wait/transcript/state follow-up is issued.
    driver.respond_fixture(wait, "turn-wait.json");
    driver.assert_no_requests();
    let a_view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    assert!(
        a_view.live.is_some(),
        "the arrived result still updates the live turn state"
    );

    // The shutdown response, then the child exit: clean end.
    driver.respond(shutdown, json!({"ok": true}));
    driver.step(AppEvent::Rpc(exit()));
    assert!(driver.saw_exit, "the child exit emits Exit");
    assert_eq!(driver.app.connection, ConnectionState::ShuttingDown);
}

#[test]
fn shutdown_error_falls_back_to_kill_then_exit() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    let mut booted = false;
    ready(&mut booted, &mut driver);
    driver.step(AppEvent::ShutdownRequested);
    let shutdown = driver.request("agent.shutdown");
    driver.respond_error(shutdown, "shutdown_error", "agent refused to stop");
    assert!(driver.saw_kill, "a shutdown error falls back to KillChild");
    driver.step(AppEvent::Rpc(exit()));
    assert!(driver.saw_exit);
}

#[test]
fn shutdown_of_a_failed_connection_exits_immediately() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    driver.step(AppEvent::Rpc(RpcEvent::ProtocolError(
        minicore_tui::protocol::FrameError::new(
            minicore_tui::protocol::FrameErrorKind::Io,
            "pipe broke",
        ),
    )));
    assert!(matches!(driver.app.connection, ConnectionState::Failed(_)));
    driver.step(AppEvent::ShutdownRequested);
    assert!(
        driver.saw_exit,
        "a failed connection quits without an agent"
    );
}

#[test]
fn send_failure_recovers_the_composer_and_clears_the_pending_turn() {
    let mut driver = Driver::new(App::new(PathBuf::from("/ws/app")));
    let mut booted = false;
    ready(&mut booted, &mut driver);
    driver.step(AppEvent::CreateSession {
        workspace: "/ws/app".to_owned(),
        profile: None,
        model: None,
        reasoning: None,
        title: None,
    });
    driver.respond_fixture_by_method("session.create", "session-create.json");
    driver.respond_fixture_by_method("session.state", "session-state.json");
    driver.respond_fixture_by_method("session.transcript", "transcript-page.json");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_6f3c1a".to_owned(),
        text: "survive".to_owned(),
    });
    let send = driver.request("turn.send");
    // The transport fails before any frame is written.
    driver.step(AppEvent::RpcSendFailed {
        id: send.id,
        error: minicore_tui::rpc::RpcError::Closed,
    });
    let view = driver.app.sessions.known.get("ses_6f3c1a").unwrap();
    assert!(view.live.is_none(), "the failed send clears the live turn");
    assert_eq!(
        driver.app.composer.content(),
        "survive",
        "the text comes back"
    );
}
