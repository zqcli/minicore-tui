//! Preparing-state migration checks (Spec §8.2, §8.4).
//!
//! B1 makes an active compaction/preparation state visible and blocks a new
//! turn. Operation controls and `session.context` remain B2 work, so the two
//! legacy checks below stay as explicit scope markers rather than being
//! deleted.

use std::collections::VecDeque;
use std::path::PathBuf;

use minicore_tui::app::{App, ConnectionState};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{IncomingFrame, OutgoingRequest, RpcResponse};
use serde_json::{Value, json};

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

/// Defect: before the `turn.send` ACK binds a TurnRef, an explicit cancel
/// produces no `turn.cancel`, because the App has no operation identity to
/// route. Stage B adds `request_cancel` routing via the preparation ID.
#[test]
fn baseline_cannot_cancel_a_preparing_submission_before_turn_ref() {
    let mut driver = Driver::new();
    driver.ready_session("ses_prep");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_prep".to_owned(),
        text: "explain the parser".to_owned(),
    });
    let sent = driver.request("turn.send");
    // Deliberately do NOT answer yet: the deferred admission is in flight.
    driver.step(AppEvent::CancelTurn {
        session_id: "ses_prep".to_owned(),
    });
    assert!(
        !driver
            .queue
            .iter()
            .any(|request| request.method == "turn.cancel"),
        "BASELINE: a deferred submission with no TurnRef cannot be cancelled"
    );
    drop(sent);
}

/// Defect: the App never issues `session.context`, so an in-flight automatic
/// preparation is invisible and cannot be observed.
#[test]
fn baseline_no_context_observation_for_preparation() {
    let source = include_str!("../src/app.rs");
    assert!(
        !source.contains("session_context(") && !source.contains("METHOD_SESSION_CONTEXT"),
        "BASELINE: no session.context observation exists"
    );
    assert!(
        !source.contains("operation_id") && !source.contains("OperationRef"),
        "BASELINE: no preparation operation identity exists"
    );
}

/// After `turn.send` the App still registers exactly one `turn.wait` for the
/// normal path, and now also has an authoritative `turn.result` read-back for
/// a lost/unconfirmed result. Formerly the RED baseline pin
/// `baseline_only_registers_turn_wait_after_send`.
#[test]
fn wait_is_registered_once_and_turn_result_recovery_exists() {
    let source = include_str!("../src/app.rs");
    assert!(
        source.contains("RequestKind::WaitTurn("),
        "the normal path registers a single wait"
    );
    assert!(
        source.contains("TurnAvailability") && source.contains("recover_turn"),
        "the retained report can be read back by availability"
    );
}

/// B1 exposes the preparation state in the busy status surface. The
/// operation-specific controls remain deliberately out of scope for B2.
#[test]
fn preparing_status_is_visible() {
    assert!(
        include_str!("../src/ui/status.rs").contains("Preparing"),
        "B1 must expose a Preparing status label"
    );
}
