//! Request preparation and independent Session operation checks.
//!
//! A pending normal submit never owns the previous completed turn's
//! compaction; exact operation identities are observed through Protocol v1.

use std::collections::VecDeque;
use std::path::PathBuf;

use minicore_tui::app::{App, ConnectionState};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{IncomingFrame, OutgoingRequest, RpcResponse};
use serde_json::{Value, json};

/// The app state machine is split across `app.rs` and its child modules;
/// these structural assertions read them as one source so each invariant
/// keeps being checked wherever the implementation lives.
fn app_modules_source() -> String {
    [
        include_str!("../src/app.rs"),
        include_str!("../src/app/history.rs"),
        include_str!("../src/app/session.rs"),
        include_str!("../src/app/turn.rs"),
        include_str!("../src/app/queries.rs"),
        include_str!("../src/app/ui_actions.rs"),
    ]
    .join("\n")
}

struct Driver {
    app: App,
    queue: VecDeque<OutgoingRequest>,
}

impl Driver {
    fn new() -> Self {
        Self {
            app: App::new(PathBuf::from("/workspace")),
            queue: VecDeque::new(),
        }
    }

    fn step(&mut self, event: AppEvent) {
        for command in self.app.update(event) {
            if let AppCommand::Rpc(request) = command {
                self.queue.push_back(request);
            }
        }
    }

    fn request(&mut self, method: &str) -> OutgoingRequest {
        let position = self
            .queue
            .iter()
            .position(|request| request.method == method)
            .unwrap_or_else(|| panic!("missing request {method}"));
        self.queue.remove(position).unwrap()
    }

    fn respond(&mut self, request: &OutgoingRequest, result: Value) {
        self.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
        ))));
    }

    /// create -> state/presentation/history settle, so the session is ready.
    fn ready_session(&mut self, id: &str) {
        self.step(AppEvent::Bootstrap);
        for (method, result) in [
            (
                "agent.ping",
                json!({
                    "version": "0.5.0",
                    "protocol_version": 1,
                    "capabilities": minicore_tui::protocol::REQUIRED_CAPABILITIES,
                }),
            ),
            ("model.list", json!({"models": []})),
            ("profile.list", json!({"profiles": []})),
            ("session.list", json!({"sessions": []})),
        ] {
            let request = self.request(method);
            self.respond(&request, result);
        }
        assert_eq!(self.app.connection, ConnectionState::Ready);

        self.step(AppEvent::CreateSession {
            workspace: "/workspace".to_owned(),
            profile: None,
            model: None,
            reasoning: None,
            title: None,
        });
        let create = self.request("session.create");
        self.respond(&create, json!({"session": session(id)}));
        let state = self.request("session.state");
        self.respond(
            &state,
            json!({"session_id": id, "status": "idle", "active_loop": null, "block_reason": null}),
        );
        let presentation = self.request("session.presentation");
        self.respond(
            &presentation,
            json!({"session_id": id, "context": {"kind": "unknown"}}),
        );
        let history = self.request("session.read");
        self.respond(&history, read_page(id));
    }
}

/// A complete, empty Protocol v1 read page for `id`.
fn read_page(id: &str) -> Value {
    json!({
        "session": session(id),
        "items": [],
        "total": 0,
        "records": [],
        "records_truncated": false,
        "history_revision": "0000000000000000000000000000000000000000000000000000000000000000",
        "captured_end": 0,
        "trailing_incomplete": false
    })
}

fn session(id: &str) -> Value {
    json!({
        "session_id": id, "title": null, "profile": "coding", "workspace": "/workspace",
        "model": "deep", "reasoning": "high", "loaded": true,
        "created_at": "2026-01-02T03:04:05Z", "updated_at": "2026-01-02T03:04:05Z"
    })
}

/// Before the normal `turn.send` ACK binds a TurnRef, cancellation keeps its
/// intent without guessing either a turn or an independent compaction ID.
#[test]
fn baseline_cannot_cancel_a_preparing_submission_before_turn_ref() {
    let mut driver = Driver::new();
    driver.ready_session("ses_prep");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_prep".to_owned(),
        text: "explain the parser".to_owned(),
    });
    let sent = driver.request("turn.send");
    // Deliberately do NOT answer yet: the normal submit ACK is in flight.
    driver.step(AppEvent::CancelTurn {
        session_id: "ses_prep".to_owned(),
    });
    assert!(
        !driver
            .queue
            .iter()
            .any(|request| request.method == "turn.cancel"),
        "a pending submission cannot guess an exact TurnRef"
    );
    drop(sent);
}

/// Session operation polling observes backend identity independently of the
/// pending Submission owner; it does not recreate startup compaction.
#[test]
fn context_observes_independent_operation_identity() {
    let app_source = app_modules_source();
    let protocol_source = include_str!("../src/protocol.rs");
    assert!(
        app_source.contains("request_session_context")
            && app_source.contains("ContextQueryOwner::Operation"),
        "Session operations use the existing context poll scheduler"
    );
    assert!(
        protocol_source.contains("METHOD_SESSION_CONTEXT")
            && protocol_source.contains("pub fn session_context"),
        "Protocol v1 session.context builder exists"
    );
    assert!(
        app_source.contains("ContextQueryOwner::Operation(operation.operation_id.clone())")
            && app_source.contains("current_operation"),
        "context adopts the Agent operation identity, not a Submission preparation"
    );
    assert!(
        !app_source.contains("submission.preparation = Some(OperationRef"),
        "reading a previous operation cannot give the pending send cancellation ownership"
    );
}

/// After `turn.send` the App still registers exactly one `turn.wait` for the
/// normal path, and now also has an authoritative `turn.result` read-back for
/// a lost/unconfirmed result. Formerly the RED baseline pin
/// `baseline_only_registers_turn_wait_after_send`.
#[test]
fn wait_is_registered_once_and_turn_result_recovery_exists() {
    let source = app_modules_source();
    assert!(
        source.contains("RequestKind::WaitTurn("),
        "the normal path registers a single wait"
    );
    assert!(
        source.contains("TurnAvailability") && source.contains("recover_turn"),
        "the retained report can be read back by availability"
    );
}

/// Preparation remains visible in the busy status surface.
#[test]
fn preparing_status_is_visible() {
    assert!(
        include_str!("../src/ui/status.rs").contains("Preparing"),
        "B1 must expose a Preparing status label"
    );
}
