//! Focused App reducer tests for the Agent v0.3 / TUI r2 contract.

use std::collections::VecDeque;
use std::path::PathBuf;

use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::Line;
use serde_json::{Value, json};

use minicore_tui::app::{App, ConnectionState, RequestKind};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{IncomingFrame, OutgoingRequest, RpcNotification, RpcResponse};
use minicore_tui::state::tool::ToolStatus;
use minicore_tui::state::turn::PendingSteerState;
use minicore_tui::state::{AssistantPart, TranscriptBlock};

struct Driver {
    app: App,
    queue: VecDeque<OutgoingRequest>,
    exited: bool,
}

impl Driver {
    fn new() -> Self {
        Self {
            app: App::new(PathBuf::from("/workspace")),
            queue: VecDeque::new(),
            exited: false,
        }
    }

    fn step(&mut self, event: AppEvent) {
        for command in self.app.update(event) {
            match command {
                AppCommand::Rpc(request) => self.queue.push_back(request),
                AppCommand::KillChild => {}
                AppCommand::Exit => self.exited = true,
            }
        }
    }

    fn request(&mut self, method: &str) -> OutgoingRequest {
        let position = self
            .queue
            .iter()
            .position(|request| request.method == method)
            .unwrap_or_else(|| {
                panic!(
                    "missing request {method}; queued methods: {:?}",
                    self.queue
                        .iter()
                        .map(|request| request.method)
                        .collect::<Vec<_>>()
                )
            });
        self.queue.remove(position).unwrap()
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

    fn respond_method(&mut self, method: &str, result: Value) {
        let request = self.request(method);
        self.respond(request, result);
    }

    fn respond_error(&mut self, request: OutgoingRequest, code: i64, message: &str) {
        self.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: None,
                error: Some(minicore_tui::protocol::RpcError {
                    code,
                    message: message.to_owned(),
                    data: None,
                }),
            },
        ))));
    }

    #[allow(dead_code)]
    fn respond_error_method(&mut self, method: &str, code: i64, message: &str) {
        let request = self.request(method);
        self.respond_error(request, code, message);
    }
}

fn session(id: &str) -> Value {
    json!({
        "session_id": id, "title": null, "profile": "coding", "workspace": "/workspace",
        "model": "deep", "reasoning": "high", "loaded": true,
        "created_at": "2026-01-02T03:04:05Z", "updated_at": "2026-01-02T03:04:05Z"
    })
}

fn state(id: &str, status: &str, active_loop: Value) -> Value {
    json!({"session_id": id, "status": status, "active_loop": active_loop, "block_reason": null})
}

fn history(items: Vec<Value>, next_offset: Option<usize>, total: usize) -> Value {
    json!({"items": items, "next_offset": next_offset, "total": total})
}

fn user(index: usize, loop_id: &str, text: &str) -> Value {
    json!({"index": index, "item": {"type": "user", "data": {"loop_id": loop_id, "kind": "prompt", "text": text}}})
}

fn user_steering(index: usize, loop_id: &str, text: &str) -> Value {
    json!({"index": index, "item": {"type": "user", "data": {"loop_id": loop_id, "kind": "steering", "text": text}}})
}

fn assistant(index: usize, loop_id: &str, request_index: u32, model: &str, text: &str) -> Value {
    assistant_with_reasoning(index, loop_id, request_index, model, text, "")
}

fn assistant_with_reasoning(
    index: usize,
    loop_id: &str,
    request_index: u32,
    model: &str,
    text: &str,
    reasoning: &str,
) -> Value {
    json!({"index": index, "item": {"type": "assistant", "data": {
        "loop_id": loop_id, "request_index": request_index, "model": model,
        "reasoning_level": "high", "text": text, "reasoning": reasoning, "tool_calls": [],
        "usage": {}, "finish_reason": "stop"
    }}})
}

fn wait_result(session_id: &str, loop_id: &str, persistence: &str) -> Value {
    json!({
        "turn": {"session_id": session_id, "loop_id": loop_id},
        "outcome": {"type": "completed"}, "usage": {}, "requests": 1,
        "tool_rounds": 0, "final_config_revision": 0, "persistence": persistence
    })
}

fn agent_event(value: Value) -> AppEvent {
    AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(serde_json::from_value(value).unwrap()),
    )))
}

fn request_started(driver: &mut Driver, loop_id: &str, request_index: u32) {
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": loop_id},
            "request_index": request_index,
            "config_revision": request_index,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": loop_id, "dropped_before": 0}
        }
    })));
}

fn output_delta(
    driver: &mut Driver,
    loop_id: &str,
    request_index: u32,
    channel: &str,
    delta: &str,
) {
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": loop_id},
            "request_index": request_index,
            "channel": channel,
            "delta": delta,
            "meta": {"session_id": "ses_1", "loop_id": loop_id, "dropped_before": 0}
        }
    })));
}

fn reasoning_markdown(prefix: &str) -> String {
    format!(
        "### {prefix}_heading\n\n**{prefix}_bold**\n\n- {prefix}_item\n\n`{prefix}_code`\n\n```text\n{prefix}_fence\n```"
    )
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn transcript_lines_at(app: &App, width: usize) -> Vec<Line<'static>> {
    minicore_tui::ui::transcript::all_lines(&app.theme.theme(), app, width)
}

fn transcript_lines(app: &App) -> Vec<Line<'static>> {
    transcript_lines_at(app, 100)
}

fn line_position(lines: &[Line<'_>], needle: &str) -> usize {
    lines
        .iter()
        .position(|line| line_text(line).contains(needle))
        .unwrap_or_else(|| panic!("transcript line containing {needle:?} was not rendered"))
}

fn has_span_modifier(lines: &[Line<'_>], needle: &str, modifier: Modifier) -> bool {
    lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.contains(needle) && span.style.add_modifier.contains(modifier))
    })
}

fn has_span_color(lines: &[Line<'_>], needle: &str, color: ratatui::style::Color) -> bool {
    lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.contains(needle) && span.style.fg == Some(color))
    })
}

fn assert_reasoning_markdown(lines: &[Line<'_>], prefix: &str, context: &str) {
    let raw_bold = format!("**{prefix}_bold**");
    let raw_code = format!("`{prefix}_code`");
    let heading = format!("{prefix}_heading");
    let bold = format!("{prefix}_bold");
    let item = format!("• {prefix}_item");
    let code = format!("{prefix}_code");
    let fence = format!("{prefix}_fence");
    assert!(
        !lines.iter().any(|line| line_text(line).contains(&raw_bold)),
        "{context} reasoning leaked Markdown bold markers"
    );
    assert!(
        !lines.iter().any(|line| line_text(line).contains(&raw_code)),
        "{context} reasoning leaked Markdown code markers"
    );
    assert!(
        !lines.iter().any(|line| line_text(line).contains("```")),
        "{context} reasoning leaked fenced-code markers"
    );
    assert!(
        has_span_color(
            lines,
            &heading,
            minicore_tui::theme::Theme::dark().md_heading
        ),
        "{context} reasoning heading is not styled"
    );
    assert!(
        has_span_modifier(lines, &bold, Modifier::BOLD),
        "{context} reasoning bold span is not styled"
    );
    assert!(
        lines.iter().any(|line| line_text(line).contains(&item)),
        "{context} reasoning list was not rendered with a bullet"
    );
    assert!(
        has_span_color(
            lines,
            "•",
            minicore_tui::theme::Theme::dark().md_list_bullet,
        ),
        "{context} reasoning list marker is not styled"
    );
    assert!(
        has_span_color(lines, &code, minicore_tui::theme::Theme::dark().md_code),
        "{context} reasoning code span is not styled"
    );
    assert!(
        lines.iter().any(|line| line_text(line).contains("╭")),
        "{context} reasoning fenced code has no frame"
    );
    assert!(
        has_span_color(
            lines,
            &fence,
            minicore_tui::theme::Theme::dark().md_code_block,
        ),
        "{context} reasoning fenced code span is not styled"
    );
}

fn assert_request_local_order(
    lines: &[Line<'_>],
    first_prefix: &str,
    second_prefix: &str,
    context: &str,
) {
    let first_reasoning = line_position(lines, &format!("{first_prefix}_bold"));
    let first_text = line_position(lines, &format!("{first_prefix}_answer"));
    let second_reasoning = line_position(lines, &format!("{second_prefix}_bold"));
    let second_text = line_position(lines, &format!("{second_prefix}_answer"));
    assert!(
        first_reasoning < first_text,
        "{context}: request 0 reasoning must precede its text"
    );
    assert!(
        first_text < second_reasoning,
        "{context}: request 0 text must remain before request 1 reasoning; no global reasoning hoist"
    );
    assert!(
        second_reasoning < second_text,
        "{context}: request 1 reasoning must precede its text"
    );
}

fn bootstrap(driver: &mut Driver) {
    driver.step(AppEvent::Bootstrap);
    driver.respond_method("agent.ping", json!({"version": "0.3.0"}));
    driver.respond_method(
        "model.list",
        json!({"models": [
            {"id":"deep","model_ref":"provider/deep","context_window":128000,"supports_tools":true,"supported_reasoning":["auto","high"]},
            {"id":"fast","model_ref":"provider/fast","context_window":64000,"supports_tools":true,"supported_reasoning":["auto","high","low"]}
        ]}),
    );
    driver.respond_method(
        "profile.list",
        json!({"profiles": [{"id":"coding","model":"deep","reasoning":"high","tools":["read"]}]}),
    );
    driver.respond_method("session.list", json!({"sessions": []}));
    assert_eq!(driver.app.connection, ConnectionState::Ready);
}

fn open_idle(driver: &mut Driver, id: &str) {
    driver.step(AppEvent::OpenSession {
        session_id: id.to_owned(),
    });
    driver.respond_method("session.open", json!({"session": session(id)}));
    driver.respond_method("session.state", state(id, "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));
}

fn submit_command(driver: &mut Driver, command: &str) {
    for character in command.chars() {
        driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::empty(),
        ))));
    }
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
}

#[test]
fn bootstrap_registers_ids_before_requests_leave_update() {
    let mut driver = Driver::new();
    let commands = driver.app.update(AppEvent::Bootstrap);
    let requests: Vec<_> = commands
        .into_iter()
        .filter_map(|command| match command {
            AppCommand::Rpc(request) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 4);
    for request in requests {
        assert!(driver.app.request_is_pending(request.id));
    }
}

#[test]
fn history_pages_by_contiguous_item_index_not_render_block_count() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::CreateSession {
        workspace: "/workspace".into(),
        profile: None,
        model: None,
        reasoning: None,
        title: None,
    });
    driver.respond_method("session.create", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let first = driver.request("session.history");
    driver.respond(first, history(vec![
        user(0, "loop_1", "hello"),
        assistant(1, "loop_1", 0, "deep", "answer"),
        json!({"index": 2, "item": {"type": "tool_result", "data": {"loop_id": "loop_1", "request_index": 0, "tool_call_id": "call", "tool_name": "read", "outcome": "success", "content": "ok"}}}),
    ], Some(3), 4));
    let second = driver.request("session.history");
    assert_eq!(second.params["offset"], 3);
    driver.respond(
        second,
        history(vec![assistant(3, "loop_1", 1, "deep", "done")], None, 4),
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.transcript.loaded_count, 4);
    assert_eq!(view.transcript.items.len(), 4);
    // The presentation may expand an Assistant item with tool-call
    // placeholders, but pagination remains driven by the raw item count.
    assert_eq!(view.transcript.blocks.len(), 4);
    assert!(view.transcript.complete);
}

#[test]
fn history_validation_reports_stable_gap_and_stalled_cursor_errors() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_req = driver.request("session.history");
    driver.respond(
        history_req,
        history(vec![user(1, "loop_1", "out of order")], None, 2),
    );
    assert!(driver.app.notices().iter().any(|notice| {
        notice
            .text
            .contains("history for ses_1 is not contiguous at offset 0")
    }));

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_req = driver.request("session.history");
    driver.respond(history_req, history(Vec::new(), Some(1), 1));
    assert!(driver.app.notices().iter().any(|notice| {
        notice
            .text
            .contains("history for ses_1 did not advance its offset from 0")
    }));
}

#[test]
fn late_completed_loop_events_cannot_bind_a_new_prompt() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    // Complete L1 and reconcile it into durable history.
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "first".into(),
    });
    let send1 = driver.request("turn.send");
    driver.respond(
        send1,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait1 = driver.request("turn.wait");
    driver.respond(wait1, wait_result("ses_1", "loop_1", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_1", "first"),
                assistant(1, "loop_1", 0, "deep", "done"),
            ],
            None,
            2,
        ),
    );
    assert!(driver.app.sessions.known["ses_1"].live.is_none());

    // Start L2. Its turn.send response is deliberately held while old L1
    // events arrive.
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "second".into(),
    });
    let send2 = driver.request("turn.send");

    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "channel": "text",
            "delta": "late old output",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state("ses_1", "running", json!({
                "loop_id": "loop_1",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            })),
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    driver.respond(
        send2,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_2"}}),
    );
    let wait2 = driver.request("turn.wait");
    assert_eq!(wait2.params["loop_id"], "loop_2");
    assert!(!matches!(driver.app.connection, ConnectionState::Failed(_)));
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|turn| turn.loop_id.as_str()),
        Some("loop_2")
    );
}

fn start_turn_and_close(driver: &mut Driver, session_id: &str, loop_id: &str) -> OutgoingRequest {
    driver.step(AppEvent::SubmitTurn {
        session_id: session_id.into(),
        text: "old prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": session_id, "loop_id": loop_id}}),
    );
    let wait = driver.request("turn.wait");
    driver.step(AppEvent::CloseSession {
        session_id: session_id.into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond(close, json!({"ok": true}));
    wait
}

fn reopen_with_history(driver: &mut Driver, session_id: &str, loop_id: &str) {
    driver.step(AppEvent::OpenSession {
        session_id: session_id.into(),
    });
    // The new open is still pending. These old-loop notifications must be
    // fenced before the open response rebuilds the view.
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": session_id, "loop_id": "loop_old"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": session_id, "loop_id": "loop_old", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": session_id, "loop_id": "loop_old"},
            "request_index": 0,
            "channel": "text",
            "delta": "late old output during reopen",
            "meta": {"session_id": session_id, "loop_id": "loop_old", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state(session_id, "running", json!({
                "loop_id": "loop_old",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            })),
            "meta": {"session_id": session_id, "loop_id": "loop_old", "dropped_before": 0}
        }
    })));
    let view = &driver.app.sessions.known[session_id];
    assert_eq!(
        view.live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|turn| turn.loop_id.as_str()),
        Some("loop_old")
    );
    assert!(
        view.live
            .as_ref()
            .is_some_and(|live| live.requests.is_empty()),
        "old notifications must be fenced before reopen completes"
    );
    driver.respond_method("session.open", json!({"session": session(session_id)}));
    driver.respond_method("session.state", state(session_id, "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, loop_id, "new prompt"),
                assistant(1, loop_id, 0, "deep", "new answer"),
            ],
            None,
            2,
        ),
    );
}

fn delayed_steer_driver() -> (Driver, OutgoingRequest, OutgoingRequest, u64) {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_steer"}}),
    );
    let wait = driver.request("turn.wait");
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state("ses_1", "running", json!({
                "loop_id": "loop_steer",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            })),
            "meta": {"session_id": "ses_1", "loop_id": "loop_steer", "dropped_before": 0}
        }
    })));
    driver.step(AppEvent::Terminal(CrosstermEvent::Paste(
        "late steer".into(),
    )));
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
    let steer = driver.request("turn.steer");
    let steer_id = match driver.app.pending_request_kind(steer.id).unwrap() {
        RequestKind::SteerTurn { steer_id, .. } => *steer_id,
        kind => panic!("unexpected request kind: {kind:?}"),
    };
    (driver, wait, steer, steer_id)
}

#[test]
fn reopen_invalidates_old_wait_persisted_response() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let old_wait = start_turn_and_close(&mut driver, "ses_1", "loop_old");
    reopen_with_history(&mut driver, "ses_1", "loop_new");

    driver.respond(old_wait, wait_result("ses_1", "loop_old", "persisted"));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(
        view.last_result.is_none(),
        "old wait must not create new result"
    );
    assert!(
        view.unsaved_loop.is_none(),
        "old wait must not block reopened session"
    );
    assert!(view.live.is_none(), "old wait must not create a live loop");
    assert!(
        driver.queue.is_empty(),
        "old wait must not trigger reconciliation"
    );
}

#[test]
fn reopen_invalidates_old_wait_failed_response() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let old_wait = start_turn_and_close(&mut driver, "ses_1", "loop_old");
    reopen_with_history(&mut driver, "ses_1", "loop_new");

    driver.respond_error(old_wait, -32603, "old wait failed");
    let view = &driver.app.sessions.known["ses_1"];
    assert!(
        view.last_result.is_none(),
        "old wait error must not alter result"
    );
    assert!(
        view.unsaved_loop.is_none(),
        "old wait error must not block reopened session"
    );
    assert!(
        view.live.is_none(),
        "old wait error must not create a live loop"
    );
    assert!(
        driver.queue.is_empty(),
        "old wait error must not trigger reconciliation"
    );
}

#[test]
fn loaded_running_session_reopen_reuses_view_after_state_failure() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_live"}}),
    );
    let wait = driver.request("turn.wait");
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state("ses_1", "running", json!({
                "loop_id": "loop_live",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            })),
            "meta": {"session_id": "ses_1", "loop_id": "loop_live", "dropped_before": 0}
        }
    })));

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.open")
    );
    let state_req = driver.request("session.state");
    driver.respond_error(state_req, -32603, "state temporarily unavailable");

    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_live", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "channel": "text",
            "delta": "still accepted",
            "meta": {"session_id": "ses_1", "loop_id": "loop_live", "dropped_before": 0}
        }
    })));

    let live = driver.app.sessions.known["ses_1"].live.as_ref().unwrap();
    assert_eq!(live.reference.as_ref().unwrap().loop_id, "loop_live");
    assert_eq!(live.requests[0].text, "still accepted");
    assert!(driver.app.request_is_pending(wait.id));
}

#[test]
fn failed_close_reopen_keeps_retired_loop_fenced() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let old_wait = start_turn_and_close(&mut driver, "ses_1", "loop_closed");

    assert!(!driver.app.sessions.known["ses_1"].info.loaded);
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .retired_loop
            .as_ref()
            .unwrap()
            .loop_id,
        "loop_closed"
    );

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    let open = driver.request("session.open");
    driver.respond_error(open, minicore_tui::protocol::STORE_ERROR, "open failed");

    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.info.loaded);
    assert_eq!(view.retired_loop.as_ref().unwrap().loop_id, "loop_closed");
    assert!(driver.app.request_is_pending(old_wait.id));

    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_closed"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_closed", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_closed"},
            "request_index": 0,
            "channel": "text",
            "delta": "must stay fenced",
            "meta": {"session_id": "ses_1", "loop_id": "loop_closed", "dropped_before": 0}
        }
    })));
    assert!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .is_some_and(|live| live.requests.is_empty())
    );
}

#[test]
fn steering_ack_only_clears_the_same_editor_revision() {
    let mut changed = Driver::new();
    bootstrap(&mut changed);
    open_idle(&mut changed, "ses_1");
    changed.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = changed.request("turn.send");
    changed.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_steer"}}),
    );
    let _wait = changed.request("turn.wait");
    changed.step(AppEvent::Terminal(CrosstermEvent::Paste("X".into())));
    changed.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
    let steer = changed.request("turn.steer");
    changed.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    changed.step(AppEvent::Terminal(CrosstermEvent::Paste("X".into())));
    let submitted_revision = match changed.app.pending_request_kind(steer.id).unwrap() {
        RequestKind::SteerTurn {
            editor_revision: Some(revision),
            ..
        } => *revision,
        kind => panic!("unexpected steer request kind: {kind:?}"),
    };
    assert_ne!(submitted_revision, changed.app.composer.editor_revision());
    changed.respond(steer, json!({"ok": true}));
    assert_eq!(changed.app.composer.content(), "X");

    let mut unchanged = Driver::new();
    bootstrap(&mut unchanged);
    open_idle(&mut unchanged, "ses_1");
    unchanged.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = unchanged.request("turn.send");
    unchanged.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_steer"}}),
    );
    let _wait = unchanged.request("turn.wait");
    unchanged.step(AppEvent::Terminal(CrosstermEvent::Paste("X".into())));
    unchanged.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
    let steer = unchanged.request("turn.steer");
    unchanged.respond(steer, json!({"ok": true}));
    assert!(unchanged.app.composer.content().is_empty());

    let mut direct = Driver::new();
    bootstrap(&mut direct);
    open_idle(&mut direct, "ses_1");
    direct.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = direct.request("turn.send");
    direct.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_steer"}}),
    );
    let _wait = direct.request("turn.wait");
    direct.step(AppEvent::Terminal(CrosstermEvent::Paste("X".into())));
    direct.step(AppEvent::SteerTurn {
        session_id: "ses_1".into(),
        text: "X".into(),
    });
    let steer = direct.request("turn.steer");
    direct.respond(steer, json!({"ok": true}));
    assert_eq!(direct.app.composer.content(), "X");
}

#[test]
fn late_session_events_cannot_overwrite_info_or_clear_a_new_loop() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let mut old_open = session("ses_1");
    old_open["model"] = json!("old-model");
    old_open["reasoning"] = json!("low");
    driver.step(agent_event(json!({
        "type": "session_opened",
        "data": {
            "session": old_open,
            "meta": {"session_id": "ses_1", "loop_id": null, "dropped_before": 0}
        }
    })));
    assert_eq!(driver.app.sessions.known["ses_1"].info.model, "deep");
    assert!(driver.queue.is_empty());

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_new"}}),
    );
    let view = driver.app.sessions.known.get_mut("ses_1").unwrap();
    view.state.as_mut().unwrap().status = minicore_tui::protocol::SessionStatusWire::Running;
    view.last_result =
        Some(serde_json::from_value(wait_result("ses_1", "loop_old", "persisted")).unwrap());
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state("ses_1", "idle", Value::Null),
            "meta": {"session_id": "ses_1", "loop_id": null, "dropped_before": 0}
        }
    })));
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.state.as_ref().unwrap().status,
        minicore_tui::protocol::SessionStatusWire::Running
    );
    assert_eq!(
        view.live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id,
        "loop_new"
    );
    assert_eq!(view.last_result.as_ref().unwrap().turn.loop_id, "loop_old");
}

#[test]
fn first_open_running_placeholder_accepts_following_events() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    let state_req = driver.request("session.state");
    let _history = driver.request("session.history");
    driver.respond(
        state_req,
        state(
            "ses_1",
            "running",
            json!({
                "loop_id": "loop_first",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            }),
        ),
    );
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .unwrap()
            .local_submission,
        minicore_tui::state::turn::LocalSubmissionId(u64::MAX)
    );
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_first"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_first", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_first"},
            "request_index": 0,
            "channel": "text",
            "delta": "first output",
            "meta": {"session_id": "ses_1", "loop_id": "loop_first", "dropped_before": 0}
        }
    })));
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .unwrap()
            .requests[0]
            .text,
        "first output"
    );
}

#[test]
fn session_opened_event_initializes_unknown_view_and_reads_running_state() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(agent_event(json!({
        "type": "session_opened",
        "data": {
            "session": session("ses_event"),
            "meta": {"session_id": "ses_event", "loop_id": null, "dropped_before": 0}
        }
    })));
    assert_eq!(driver.app.sessions.known["ses_event"].info.model, "deep");
    let state_req = driver.request("session.state");
    driver.respond(
        state_req,
        state(
            "ses_event",
            "running",
            json!({
                "loop_id": "loop_event",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            }),
        ),
    );
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_event", "loop_id": "loop_event"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_event", "loop_id": "loop_event", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_event", "loop_id": "loop_event"},
            "request_index": 0,
            "channel": "text",
            "delta": "event output",
            "meta": {"session_id": "ses_event", "loop_id": "loop_event", "dropped_before": 0}
        }
    })));
    let view = &driver.app.sessions.known["ses_event"];
    assert_eq!(
        view.live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id,
        "loop_event"
    );
    assert_eq!(view.live.as_ref().unwrap().requests[0].text, "event output");
}

#[test]
fn send_response_registers_direct_wait_and_durable_history_replaces_live() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");
    assert!(
        driver
            .app
            .pending_request_kind(wait.id)
            .is_some_and(|kind| matches!(kind, RequestKind::WaitTurn(_)))
    );
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_1", "prompt"),
                assistant(1, "loop_1", 0, "deep", "durable answer"),
            ],
            None,
            2,
        ),
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    assert!(view.transcript.blocks.iter().any(|block| matches!(block, TranscriptBlock::Assistant(card) if card.parts == vec![AssistantPart::Text("durable answer".into())])));
}

#[test]
fn loop_events_can_bind_before_turn_send_response() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    let turn = json!({"session_id": "ses_1", "loop_id": "loop_early"});
    driver.step(agent_event(json!({
        "type": "turn_started",
        "data": {
            "turn": turn,
            "meta": {"session_id": "ses_1", "loop_id": "loop_early", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_early"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_early", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_early"},
            "request_index": 0,
            "channel": "text",
            "delta": "already streaming",
            "meta": {"session_id": "ses_1", "loop_id": "loop_early", "dropped_before": 0}
        }
    })));
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_early"}}),
    );
    let wait = driver.request("turn.wait");
    assert_eq!(wait.params["loop_id"], "loop_early");
    let live = driver.app.sessions.known["ses_1"].live.as_ref().unwrap();
    assert_eq!(live.requests[0].text, "already streaming");
    assert_eq!(live.reference.as_ref().unwrap().loop_id, "loop_early");
}

#[test]
fn stale_session_state_response_cannot_regress_a_newer_query() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    let old_state = driver.request("session.state");
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    let new_state = driver.request("session.state");

    driver.respond(old_state, state("ses_1", "idle", Value::Null));
    assert!(driver.app.sessions.known["ses_1"].state.is_none());
    driver.respond(new_state, state("ses_1", "idle", Value::Null));
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .state
            .as_ref()
            .unwrap()
            .status,
        minicore_tui::protocol::SessionStatusWire::Idle
    );
}

#[test]
fn request_index_keeps_multi_request_deltas_separate() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let turn = json!({"session_id": "ses_1", "loop_id": "loop_1"});
    for (index, text) in [(0, "first"), (1, "second")] {
        driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            minicore_tui::protocol::RpcNotification::AgentEvent(serde_json::from_value(json!({
                "type": "output_delta", "data": {"turn": turn, "request_index": index, "channel": "text", "delta": text, "meta": {"session_id": "ses_1", "dropped_before": 0}}
            })).unwrap()),
        ))));
    }
    let live = driver.app.sessions.known["ses_1"].live.as_ref().unwrap();
    assert_eq!(live.requests.len(), 2);
    assert_eq!(live.requests[0].text, "first");
    assert_eq!(live.requests[1].text, "second");
}

#[test]
fn tool_events_before_started_are_retained_and_mark_a_gap() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let turn = json!({"session_id": "ses_1", "loop_id": "loop_1"});
    driver.step(agent_event(json!({
        "type": "tool_progress",
        "data": {
            "turn": turn,
            "request_index": 0,
            "tool_call_id": "call_1",
            "progress": {"message": "half", "completed": 1, "total": 2},
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));

    let live = driver.app.sessions.known["ses_1"].live.as_ref().unwrap();
    assert!(live.event_gap);
    assert_eq!(live.requests[0].tools[0].name, "(unknown tool)");
    assert_eq!(live.requests[0].tools[0].status, ToolStatus::Running);
    assert_eq!(live.requests[0].tools[0].progress.as_deref(), Some("half"));

    driver.step(agent_event(json!({
        "type": "tool_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "tool_call_id": "call_1",
            "tool_name": "read",
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));
    let tool = &driver.app.sessions.known["ses_1"]
        .live
        .as_ref()
        .unwrap()
        .requests[0]
        .tools[0];
    assert_eq!(tool.name, "read");
    assert_eq!(tool.status, ToolStatus::Running);
}

#[test]
fn live_reasoning_renders_markdown_before_each_request_text() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "synthetic live prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_live_markdown"}}),
    );
    let _wait = driver.request("turn.wait");

    request_started(&mut driver, "loop_live_markdown", 0);
    output_delta(
        &mut driver,
        "loop_live_markdown",
        0,
        "reasoning",
        &reasoning_markdown("live_r0"),
    );
    output_delta(
        &mut driver,
        "loop_live_markdown",
        0,
        "text",
        "live_r0_answer",
    );
    request_started(&mut driver, "loop_live_markdown", 1);
    output_delta(
        &mut driver,
        "loop_live_markdown",
        1,
        "reasoning",
        &reasoning_markdown("live_r1"),
    );
    output_delta(
        &mut driver,
        "loop_live_markdown",
        1,
        "text",
        "live_r1_answer",
    );

    let lines = transcript_lines(&driver.app);
    assert_request_local_order(&lines, "live_r0", "live_r1", "live loop");
    assert_reasoning_markdown(&lines, "live_r0", "live request 0");
    assert_reasoning_markdown(&lines, "live_r1", "live request 1");
}

#[test]
fn persisted_reasoning_preserves_markdown_and_request_order_after_reopen() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "synthetic persisted prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_history_markdown"}}),
    );
    let wait = driver.request("turn.wait");

    request_started(&mut driver, "loop_history_markdown", 0);
    output_delta(
        &mut driver,
        "loop_history_markdown",
        0,
        "reasoning",
        &reasoning_markdown("history_r0"),
    );
    output_delta(
        &mut driver,
        "loop_history_markdown",
        0,
        "text",
        "history_r0_answer",
    );
    request_started(&mut driver, "loop_history_markdown", 1);
    output_delta(
        &mut driver,
        "loop_history_markdown",
        1,
        "reasoning",
        &reasoning_markdown("history_r1"),
    );
    output_delta(
        &mut driver,
        "loop_history_markdown",
        1,
        "text",
        "history_r1_answer",
    );

    driver.respond(
        wait,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_history_markdown"},
            "outcome": {"type": "completed"},
            "persistence": "persisted",
            "usage": {},
            "requests": 2,
            "tool_rounds": 0,
            "final_config_revision": 1
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_items = vec![
        user(0, "loop_history_markdown", "synthetic persisted prompt"),
        assistant_with_reasoning(
            1,
            "loop_history_markdown",
            0,
            "deep",
            "history_r0_answer",
            &reasoning_markdown("history_r0"),
        ),
        assistant_with_reasoning(
            2,
            "loop_history_markdown",
            1,
            "deep",
            "history_r1_answer",
            &reasoning_markdown("history_r1"),
        ),
    ];
    let history_request = driver.request("session.history");
    driver.respond(history_request, history(history_items.clone(), None, 3));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(
        view.live.is_none(),
        "persisted history should replace the live loop"
    );
    let lines = transcript_lines(&driver.app);
    assert_request_local_order(&lines, "history_r0", "history_r1", "persisted history");
    assert_reasoning_markdown(&lines, "history_r0", "persisted request 0");
    assert_reasoning_markdown(&lines, "history_r1", "persisted request 1");

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond(close, json!({"ok": true}));
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let reopened_history = driver.request("session.history");
    driver.respond(reopened_history, history(history_items, None, 3));

    let lines = transcript_lines(&driver.app);
    assert_request_local_order(&lines, "history_r0", "history_r1", "reopened history");
    assert_reasoning_markdown(&lines, "history_r0", "reopened request 0");
    assert_reasoning_markdown(&lines, "history_r1", "reopened request 1");
}

#[test]
fn reasoning_rendering_keeps_hidden_cache_fallback_themes_and_cjk_width() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "synthetic CJK reasoning prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_cjk_reasoning"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(
        wait,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_cjk_reasoning"},
            "outcome": {"type": "completed"},
            "persistence": "persisted",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let reasoning = "### 思考标题\n\n**思考粗体**\n\n- 中文项\n\n`代码`\n\n```text\n中文代码\n```";
    let history_request = driver.request("session.history");
    driver.respond(
        history_request,
        history(
            vec![
                user(0, "loop_cjk_reasoning", "synthetic CJK reasoning prompt"),
                assistant_with_reasoning(
                    1,
                    "loop_cjk_reasoning",
                    0,
                    "deep",
                    "synthetic answer",
                    reasoning,
                ),
            ],
            None,
            2,
        ),
    );

    let dark_fallback = transcript_lines_at(&driver.app, 16);
    assert!(
        has_span_color(
            &dark_fallback,
            "思考标题",
            minicore_tui::theme::Theme::dark().md_heading,
        ),
        "dark reasoning heading must be styled"
    );
    assert!(
        has_span_modifier(&dark_fallback, "思考粗体", Modifier::BOLD),
        "dark reasoning bold must be styled"
    );
    assert!(
        dark_fallback
            .iter()
            .any(|line| line_text(line).contains("• 中文项")),
        "dark reasoning list must use a bullet"
    );
    assert!(
        has_span_color(
            &dark_fallback,
            "代码",
            minicore_tui::theme::Theme::dark().md_code,
        ),
        "dark reasoning inline code must be styled"
    );
    assert!(
        dark_fallback
            .iter()
            .any(|line| line_text(line).contains("╭")),
        "dark reasoning fenced code must be framed"
    );
    let cjk_line = dark_fallback
        .iter()
        .find(|line| line_text(line).contains("中文项"))
        .expect("CJK reasoning list line");
    assert!(
        minicore_tui::markdown::line_width(cjk_line) <= 16,
        "CJK reasoning line exceeded its display width"
    );

    let prepared = minicore_tui::ui::transcript::prepare_cache(&driver.app, 16)
        .expect("durable reasoning cache preparation");
    driver.step(AppEvent::TranscriptCachePrepared(prepared));
    let dark_cached = transcript_lines_at(&driver.app, 16);
    assert_eq!(
        dark_cached, dark_fallback,
        "cached and fallback reasoning differ"
    );

    driver.step(AppEvent::ToggleReasoning);
    let hidden = transcript_lines_at(&driver.app, 16);
    assert_eq!(
        hidden
            .iter()
            .filter(|line| line_text(line).contains("Thinking..."))
            .count(),
        1,
        "one hidden reasoning run should render one Thinking label"
    );
    assert!(
        hidden
            .iter()
            .all(|line| !line_text(line).contains("思考标题")),
        "hidden reasoning content must not leak through"
    );
    driver.step(AppEvent::ToggleReasoning);

    driver.step(AppEvent::SetTheme(minicore_tui::theme::ThemeKind::Light));
    let light_fallback = transcript_lines_at(&driver.app, 16);
    assert!(
        has_span_color(
            &light_fallback,
            "思考标题",
            minicore_tui::theme::Theme::light().md_heading,
        ),
        "light reasoning heading must be styled"
    );
    assert!(
        light_fallback
            .iter()
            .any(|line| line_text(line).contains("• 中文项")),
        "light reasoning list must use a bullet"
    );
    let light_prepared = minicore_tui::ui::transcript::prepare_cache(&driver.app, 16)
        .expect("light durable reasoning cache preparation");
    driver.step(AppEvent::TranscriptCachePrepared(light_prepared));
    assert_eq!(
        transcript_lines_at(&driver.app, 16),
        light_fallback,
        "light cached and fallback reasoning differ"
    );
    assert!(
        minicore_tui::ui::reasoning::visible_lines(&minicore_tui::theme::Theme::light(), "", 16,)
            .is_empty(),
        "empty reasoning must not add a section"
    );
    assert!(
        minicore_tui::ui::reasoning::live_lines(
            &minicore_tui::theme::Theme::light(),
            "",
            16,
            true,
        )
        .is_empty(),
        "empty live reasoning must not add a section"
    );
}

#[test]
fn late_reasoning_delta_stays_before_same_request_text() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "synthetic late reasoning prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_late_reasoning"}}),
    );
    let _wait = driver.request("turn.wait");
    request_started(&mut driver, "loop_late_reasoning", 0);
    output_delta(
        &mut driver,
        "loop_late_reasoning",
        0,
        "text",
        "late_r0_answer",
    );
    output_delta(
        &mut driver,
        "loop_late_reasoning",
        0,
        "reasoning",
        &reasoning_markdown("late_r0"),
    );

    let lines = transcript_lines(&driver.app);
    let reasoning = line_position(&lines, "late_r0_bold");
    let text = line_position(&lines, "late_r0_answer");
    assert!(
        reasoning < text,
        "late reasoning delta must remain before its same-request text"
    );
    assert_reasoning_markdown(&lines, "late_r0", "late reasoning request");
}

#[test]
fn persistence_failure_blocks_without_losing_the_old_result_view() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_1", "failed"));
    assert!(driver.app.sessions.known["ses_1"].unsaved_loop.is_some());
    assert!(driver.app.sessions.known["ses_1"].is_blocked());
    assert!(driver.app.sessions.known["ses_1"].live.is_some());

    driver.step(AppEvent::RefreshTurn {
        session_id: "ses_1".into(),
    });
    let wait_again = driver.request("turn.wait");
    let pending_before = driver.app.pending_requests.len();
    driver.respond(wait_again, wait_result("ses_1", "loop_1", "failed"));
    assert_eq!(driver.app.pending_requests.len(), pending_before - 1);
}

#[test]
fn slash_cancel_sends_exact_turn_cancel_and_wait_reconciles() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "cancel me".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_cancel"}}),
    );
    let wait = driver.request("turn.wait");

    submit_command(&mut driver, "/cancel");
    let cancel = driver.request("turn.cancel");
    assert_eq!(
        cancel.params,
        json!({"session_id": "ses_1", "loop_id": "loop_cancel"})
    );
    assert!(driver.app.request_is_pending(wait.id));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "agent.shutdown")
    );

    driver.respond(cancel, json!({"cancelled": true}));
    assert!(driver.app.request_is_pending(wait.id));
    assert!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .is_some_and(|live| live.cancel_requested)
    );

    driver.respond(
        wait,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_cancel"},
            "outcome": {"type": "cancelled", "reason": "user"},
            "persistence": "persisted",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_cancel", "cancel me"),
                assistant(1, "loop_cancel", 0, "deep", "cancelled"),
            ],
            None,
            2,
        ),
    );

    let result = driver.app.sessions.known["ses_1"]
        .last_result
        .as_ref()
        .expect("cancelled wait result retained");
    assert_eq!(
        result.outcome,
        minicore_tui::protocol::LoopOutcomeWire::Cancelled {
            reason: minicore_tui::protocol::CancelReasonWire::User
        }
    );
    assert_eq!(
        result.persistence,
        minicore_tui::protocol::TurnPersistenceWire::Persisted
    );
}

#[test]
fn slash_refresh_and_restricted_commands_remain_usable() {
    // Slash commands are reachable with no active session even though a
    // normal prompt is not actionable.
    let mut no_session = Driver::new();
    submit_command(&mut no_session, "/refresh");
    assert!(no_session.queue.is_empty());
    assert!(no_session.app.composer.is_empty());
    assert!(
        no_session
            .app
            .notices()
            .iter()
            .any(|notice| notice.text.contains("no active session to refresh"))
    );

    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "blocked turn".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_blocked"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_blocked", "failed"));
    assert!(driver.app.sessions.known["ses_1"].is_blocked());

    // Blocked normal text remains in Composer and cannot become a prompt.
    driver.step(AppEvent::Terminal(CrosstermEvent::Paste("ordinary".into())));
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
    assert_eq!(driver.app.composer.content(), "ordinary");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    assert!(driver.app.composer.is_empty());

    // `/refresh` targets the retained blocked TurnRef exactly once.
    submit_command(&mut driver, "/refresh");
    let refresh = driver.request("turn.wait");
    assert_eq!(
        refresh.params,
        json!({"session_id": "ses_1", "loop_id": "loop_blocked"})
    );
    submit_command(&mut driver, "/refresh");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.wait")
    );

    // Blocked steer and active-session update remain forbidden.
    let commands = driver
        .app
        .steer_turn(&"ses_1".to_owned(), "try steer".to_owned());
    assert!(commands.is_empty());
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::ConfirmDock);
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.update")
    );
    driver.step(AppEvent::CancelDock);

    driver.respond(refresh, wait_result("ses_1", "loop_blocked", "failed"));

    // `/close confirm` is also reachable from the blocked Composer and
    // preserves the exact retained TurnRef for its close-time wait.
    submit_command(&mut driver, "/close confirm");
    let close_wait = driver.request("turn.wait");
    let close = driver.request("session.close");
    assert_eq!(
        close_wait.params,
        json!({"session_id": "ses_1", "loop_id": "loop_blocked"})
    );
    assert_eq!(close.params["session_id"], "ses_1");

    let mut finishing = Driver::new();
    bootstrap(&mut finishing);
    open_idle(&mut finishing, "ses_1");
    let view = finishing.app.sessions.known.get_mut("ses_1").unwrap();
    view.state.as_mut().unwrap().status = minicore_tui::protocol::SessionStatusWire::Finishing;
    let mut live = minicore_tui::state::turn::LiveLoop::new(
        minicore_tui::state::turn::LocalSubmissionId(1),
        "finishing turn".into(),
    );
    live.reference = Some(minicore_tui::protocol::TurnRef {
        session_id: "ses_1".into(),
        loop_id: "loop_finishing".into(),
    });
    view.live = Some(live);
    submit_command(&mut finishing, "/refresh");
    let finishing_wait = finishing.request("turn.wait");
    assert_eq!(
        finishing_wait.params,
        json!({"session_id": "ses_1", "loop_id": "loop_finishing"})
    );
}

#[test]
fn shutdown_drains_after_child_exit_until_rpc_channel_ends() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::ShutdownRequested);
    let shutdown = driver.request("agent.shutdown");
    driver.respond(shutdown, json!({"ok": true}));
    driver.step(AppEvent::Rpc(RpcEvent::Exited(None)));
    assert!(!driver.exited);
    driver.step(AppEvent::RpcChannelEnded);
    assert!(driver.exited);
    assert_eq!(driver.app.connection, ConnectionState::ShuttingDown);
}

#[test]
fn session_update_is_sent_for_an_active_session() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::ConfirmDock);
    let update = driver.request("session.update");
    assert_eq!(update.params["session_id"], "ses_1");
    assert_eq!(update.params["model"], "deep");
    driver.respond(
        update,
        json!({"session": session("ses_1"), "active_revision": null}),
    );
    assert!(
        driver
            .app
            .notices
            .back()
            .is_some_and(|notice| notice.text.contains("next turn"))
    );
}

#[test]
fn deterministic_same_loop_model_a_to_tool_to_model_b() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "start task".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");
    assert_eq!(wait.params["loop_id"], "loop_1");

    // Request 0 starts with Model A ("deep"), config_revision=0
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    // Request 0 emits tool call "read"
    driver.step(agent_event(json!({
        "type": "tool_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "tool_call_id": "call_1",
            "tool_name": "read",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    // Mid-loop: update model to Model B ("fast")
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::MoveSelector { delta: 1 });
    driver.step(AppEvent::ConfirmDock);
    let update = driver.request("session.update");
    assert_eq!(update.params["model"], "fast");
    driver.respond(update, json!({
        "session": {
            "session_id": "ses_1", "title": null, "profile": "coding", "workspace": "/workspace",
            "model": "fast", "reasoning": "high", "loaded": true,
            "created_at": "2026-01-02T03:04:05Z", "updated_at": "2026-01-02T03:04:05Z"
        },
        "active_revision": 1
    }));

    // Tool finishes
    driver.step(agent_event(json!({
        "type": "tool_finished",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "tool_call_id": "call_1",
            "result": {"outcome": "success", "content_bytes": 1024},
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    // Request 1 starts with Model B ("fast"), config_revision=1 in same loop_1
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 1,
            "config_revision": 1,
            "model": "fast",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "output_delta",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 1,
            "channel": "text",
            "delta": "done with fast model",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    let live = driver.app.sessions.known["ses_1"].live.as_ref().unwrap();
    assert_eq!(live.requests.len(), 2);
    assert_eq!(live.requests[0].model, "deep");
    assert_eq!(live.requests[0].config_revision, 0);
    assert_eq!(live.requests[1].model, "fast");
    assert_eq!(live.requests[1].config_revision, 1);

    // Loop finishes
    driver.respond(
        wait,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {"type": "completed"},
            "usage": {},
            "requests": 2,
            "tool_rounds": 1,
            "final_config_revision": 1,
            "persistence": "persisted"
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(vec![
        user(0, "loop_1", "start task"),
        json!({
            "index": 1,
            "item": {
                "type": "assistant",
                "data": {
                    "loop_id": "loop_1",
                    "request_index": 0,
                    "model": "deep",
                    "reasoning_level": "high",
                    "text": "",
                    "reasoning": "",
                    "tool_calls": [{"tool_call_id": "call_1", "name": "read", "call_index": 0}],
                    "usage": {},
                    "finish_reason": "tool_calls"
                }
            }
        }),
        json!({
            "index": 2,
            "item": {
                "type": "tool_result",
                "data": {
                    "loop_id": "loop_1",
                    "request_index": 0,
                    "tool_call_id": "call_1",
                    "tool_name": "read",
                    "outcome": "success",
                    "content": "file contents"
                }
            }
        }),
        json!({
            "index": 3,
            "item": {
                "type": "assistant",
                "data": {
                    "loop_id": "loop_1",
                    "request_index": 1,
                    "model": "fast",
                    "reasoning_level": "high",
                    "text": "done with fast model",
                    "reasoning": "",
                    "tool_calls": [],
                    "usage": {},
                    "finish_reason": "stop"
                }
            }
        }),
    ], None, 4));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    assert_eq!(view.transcript.items.len(), 4);
    assert_eq!(view.info.model, "fast");
}

#[test]
fn update_request_started_before_update_response_confirms_applied() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let _wait = driver.request("turn.wait");

    // Initiate update
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::MoveSelector { delta: 1 });
    driver.step(AppEvent::ConfirmDock);
    let update = driver.request("session.update");

    // RequestStarted with revision 1 arrives before session.update response!
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 1,
            "config_revision": 1,
            "model": "fast",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    // Now session.update response arrives
    driver.respond(update, json!({
        "session": {
            "session_id": "ses_1", "title": null, "profile": "coding", "workspace": "/workspace",
            "model": "fast", "reasoning": "high", "loaded": true,
            "created_at": "2026-01-02T03:04:05Z", "updated_at": "2026-01-02T03:04:05Z"
        },
        "active_revision": 1
    }));

    let view = &driver.app.sessions.known["ses_1"];
    let config_update = view.config_update.as_ref().unwrap();
    assert_eq!(config_update.revision, Some(1));
    assert_eq!(
        config_update.state,
        minicore_tui::state::session::ConfigUpdateState::Applied
    );
}

#[test]
fn update_and_steer_fifo_duplicate_text_history_reconciliation() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");

    // Steer 1: "retry"
    driver.app.composer.set_text("retry");
    let commands = driver.app.submit_composer();
    let steer1 = commands
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .next()
        .unwrap();
    assert_eq!(steer1.method, "turn.steer");
    driver.respond(steer1, json!({"ok": true}));

    // Steer 2: "retry" (duplicate text)
    driver.app.composer.set_text("retry");
    let commands = driver.app.submit_composer();
    let steer2 = commands
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .next()
        .unwrap();
    assert_eq!(steer2.method, "turn.steer");
    driver.respond(steer2, json!({"ok": true}));

    {
        let live = driver.app.sessions.known["ses_1"].live.as_ref().unwrap();
        assert_eq!(live.pending_steers.len(), 2);
        assert_eq!(
            live.pending_steers[0].state,
            minicore_tui::state::PendingSteerState::Queued
        );
        assert_eq!(
            live.pending_steers[1].state,
            minicore_tui::state::PendingSteerState::Queued
        );
    }

    // Wait finishes
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));

    // History returns two steering items matching FIFO
    driver.respond_method("session.history", history(vec![
        user(0, "loop_1", "prompt"),
        json!({"index": 1, "item": {"type": "user", "data": {"loop_id": "loop_1", "kind": "steering", "text": "retry"}}}),
        json!({"index": 2, "item": {"type": "user", "data": {"loop_id": "loop_1", "kind": "steering", "text": "retry"}}}),
        assistant(3, "loop_1", 1, "deep", "finished"),
    ], None, 4));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    assert_eq!(view.transcript.items.len(), 4);
}

#[test]
fn steer_queue_full_retains_composer_input_and_shows_warning() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let _wait = driver.request("turn.wait");

    driver.app.composer.set_text("important instruction");
    let commands = driver.app.submit_composer();
    let steer = commands
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .next()
        .unwrap();

    driver.respond_error(steer, -32016, "steer queue full");
    assert_eq!(driver.app.composer.content(), "important instruction");
    let notice = driver.app.notices.back().unwrap();
    assert_eq!(notice.level, minicore_tui::app::NoticeLevel::Warning);
    assert!(notice.text.contains("queue is full") || notice.text.contains("32016"));
}

#[test]
fn steering_history_not_recorded_vs_unconfirmed() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");

    driver.app.composer.set_text("steer instruction");
    let commands = driver.app.submit_composer();
    let steer = commands
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .next()
        .unwrap();
    driver.respond(steer, json!({"ok": true}));

    // Persistence succeeded, but history omits the steering item -> NotRecorded
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_1", "prompt"),
                assistant(1, "loop_1", 0, "deep", "answer"),
            ],
            None,
            2,
        ),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());

    // In a second turn, test persistence failure -> Unconfirmed
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt 2".into(),
    });
    let send2 = driver.request("turn.send");
    driver.respond(
        send2,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_2"}}),
    );
    let wait2 = driver.request("turn.wait");

    driver.app.composer.set_text("steer 2");
    let commands2 = driver.app.submit_composer();
    let steer2 = commands2
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .next()
        .unwrap();
    driver.respond(steer2, json!({"ok": true}));

    driver.respond(wait2, wait_result("ses_1", "loop_2", "failed"));
    let view2 = &driver.app.sessions.known["ses_1"];
    assert!(view2.is_blocked());
    let pending_steers = &view2.live.as_ref().unwrap().pending_steers;
    assert_eq!(
        pending_steers[0].state,
        minicore_tui::state::PendingSteerState::Unconfirmed
    );
}

#[test]
fn blocked_session_forbids_send_steer_update_and_retains_completion() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_1", "failed"));

    assert!(driver.app.sessions.known["ses_1"].is_blocked());
    assert!(driver.app.sessions.known["ses_1"].unsaved_loop.is_some());

    // Attempting send on blocked session is refused
    driver.app.composer.set_text("try send");
    let commands = driver.app.submit_composer();
    assert!(commands.is_empty());
    assert_eq!(driver.app.composer.content(), "try send");

    // Attempting steer is refused
    let ses = String::from("ses_1");
    let commands = driver.app.steer_turn(&ses, "try steer".into());
    assert!(commands.is_empty());

    // Attempting session.update via selector is refused
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::ConfirmDock);
    assert!(driver.queue.iter().all(|r| r.method != "session.update"));

    // Simulating an in-flight send receiving -32004 (session_blocked) does not destroy the old completion
    let dummy_request = OutgoingRequest::send_turn(
        minicore_tui::protocol::RequestId(999),
        "ses_1",
        "old inflight",
    );
    driver.app.pending_requests.insert(
        minicore_tui::protocol::RequestId(999),
        minicore_tui::app::RequestKind::SendTurn {
            session_id: "ses_1".into(),
            local_submission: minicore_tui::state::turn::LocalSubmissionId(999),
        },
    );
    driver.respond_error(dummy_request, -32004, "session_blocked");
    assert!(driver.app.sessions.known["ses_1"].is_blocked());
    assert!(driver.app.sessions.known["ses_1"].unsaved_loop.is_some());
    assert!(driver.app.sessions.known["ses_1"].live.is_some());
}

#[test]
fn session_close_and_delete_command_lifecycle() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    // Attempting /close on a blocked session without confirm produces a warning notice
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .state
        .as_mut()
        .unwrap()
        .status = minicore_tui::protocol::SessionStatusWire::Blocked;
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: false,
    });
    assert!(driver.queue.iter().all(|r| r.method != "session.close"));

    // /close confirm proceeds
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close_req = driver.request("session.close");
    driver.respond(close_req, json!({"ok": true}));
    assert_eq!(driver.app.sessions.active, None);

    // /delete confirm deletes the session from known and list
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let del_req = driver.request("session.delete");
    driver.respond(del_req, json!({"ok": true}));
    assert!(!driver.app.sessions.known.contains_key("ses_1"));
}

#[test]
fn regression_scenario_a_wait_internal_error_does_not_loop_history_or_clear_gap() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    // Open session: sends session.open, session.state, and initial session.history
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    // Leave the initial history request strictly in-flight!
    let inflight_history = driver.request("session.history");

    // Submit turn while initial history is in-flight
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt A".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_A"}}),
    );

    // Dropped event causes event_gap = true
    let started = serde_json::from_value(json!({
        "type": "turn_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_A"},
            "meta": {"session_id": "ses_1", "dropped_before": 1}
        }
    }))
    .unwrap();
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(started),
    ))));
    assert!(driver.app.sessions.known["ses_1"].event_gap);

    let wait = driver.request("turn.wait");
    // wait returns -32603 internal error
    driver.respond_error(wait, -32603, "internal error");

    // State notification reports blocked with internal block_reason
    let state_ev = serde_json::from_value(json!({
        "type": "session_state",
        "data": {
            "state": {
                "session_id": "ses_1",
                "status": "blocked",
                "active_loop": null,
                "block_reason": "internal"
            },
            "meta": {"session_id": "ses_1", "dropped_before": 1}
        }
    }))
    .unwrap();
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(state_ev),
    ))));

    // Now respond to the in-flight initial history with an empty page
    driver.respond(inflight_history, history(Vec::new(), None, 0));

    // Assert: Absolutely NO further history request emitted (no infinite while/drain)!
    assert!(
        driver.queue.iter().all(|r| r.method != "session.history"),
        "must not loop or emit further history requests"
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_some(), "live must exist");
    assert!(
        view.live.as_ref().unwrap().last_result.is_none(),
        "last_result must be None after wait error"
    );
    assert!(view.unsaved_loop.is_none(), "unsaved_loop must be None");
    assert!(
        view.event_gap,
        "event_gap must remain true after wait error"
    );
}

#[test]
fn regression_scenario_b_wait_persisted_post_wait_history_and_no_infinite_retry() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    // Open session: requests session.open, session.state, and initial session.history
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    // Leave the initial history request in flight!
    let inflight_history = driver.request("session.history");

    // Submit turn while old history is in-flight
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt B".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_B"}}),
    );

    // Dropped event causes event_gap = true
    let started = serde_json::from_value(json!({
        "type": "turn_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_B"},
            "meta": {"session_id": "ses_1", "dropped_before": 1}
        }
    }))
    .unwrap();
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(started),
    ))));
    assert!(driver.app.sessions.known["ses_1"].event_gap);

    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_B", "persisted"));

    // Complete the old in-flight history request with an empty page (does not contain loop_B)
    driver.respond(inflight_history, history(Vec::new(), None, 0));

    // Assert: retains live and gap, and emits exactly ONE post-wait history request
    let view = &driver.app.sessions.known["ses_1"];
    assert!(
        view.live.is_some(),
        "live must be retained before loop appears in history"
    );
    assert!(view.event_gap, "event_gap must be retained");

    let post_hist_req = driver.request("session.history");

    // Now respond with a fresh post-wait history page containing real loop_B items
    let correct_history = history(
        vec![
            user(0, "loop_B", "prompt B"),
            assistant(1, "loop_B", 0, "deep", "answer B"),
        ],
        None,
        2,
    );
    driver.respond(post_hist_req, correct_history);

    // Live turn is taken, event gap cleared cleanly without resetting via OpenSession!
    let final_view = &driver.app.sessions.known["ses_1"];
    assert!(
        final_view.live.is_none(),
        "live must be taken once loop is contained in history"
    );
    assert!(
        !final_view.event_gap,
        "event_gap must be cleared after same-turn persisted history arrives"
    );
    assert!(
        driver.queue.iter().all(|r| r.method != "session.history"),
        "no infinite history polling"
    );
}

#[test]
fn regression_scenario_c_failed_wait_does_not_reconcile_and_idempotent() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt C".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_C"}}),
    );

    // Dropped event -> event_gap = true
    let started = serde_json::from_value(json!({
        "type": "turn_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_C"},
            "meta": {"session_id": "ses_1", "dropped_before": 1}
        }
    }))
    .unwrap();
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(started),
    ))));
    assert!(driver.app.sessions.known["ses_1"].event_gap);

    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_C", "failed"));

    // failed wait MUST NOT automatically reconcile this loop via session.history
    assert!(
        driver.queue.iter().all(|r| r.method != "session.history"),
        "failed wait must not dispatch session.history to reconcile"
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.is_blocked(), "session must be blocked");
    assert!(view.event_gap, "event_gap must be preserved");
    assert!(
        view.unsaved_loop.is_some(),
        "unsaved_loop must be preserved"
    );
    assert!(view.last_result.is_some(), "last_result must be preserved");
    assert_eq!(
        view.live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id,
        "loop_C",
        "original TurnRef must be preserved"
    );

    // Repeat wait using AppEvent::RefreshTurn which registers a NEW wait request
    driver.step(AppEvent::RefreshTurn {
        session_id: "ses_1".into(),
    });
    let repeat_wait = driver.request("turn.wait");
    driver.respond(repeat_wait, wait_result("ses_1", "loop_C", "failed"));

    assert!(
        driver.queue.iter().all(|r| r.method != "session.history"),
        "duplicate wait must not dispatch session.history"
    );
    let view_after = &driver.app.sessions.known["ses_1"];
    assert!(view_after.is_blocked());
    assert!(view_after.unsaved_loop.is_some());
}

#[test]
fn late_steer_ack_after_complete_history_marks_missing_steer_not_recorded() {
    let (mut driver, wait, steer, steer_id) = delayed_steer_driver();

    // The wait completes before the steer response. History is complete but
    // deliberately omits the steering item, so the local entry is unconfirmed.
    driver.respond(wait, wait_result("ses_1", "loop_steer", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_req = driver.request("session.history");
    driver.respond(
        history_req,
        history(
            vec![
                user(0, "loop_steer", "prompt"),
                assistant(1, "loop_steer", 0, "deep", "answer"),
            ],
            None,
            2,
        ),
    );

    let archived = &driver.app.sessions.known["ses_1"].completed_steers;
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].local_id, steer_id);
    assert_eq!(archived[0].state, PendingSteerState::Unconfirmed);
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    driver.step(AppEvent::Terminal(CrosstermEvent::Paste(
        "new draft".into(),
    )));

    // Late ok confirms acceptance only; complete History already proved it was
    // not recorded in this turn.
    driver.respond(steer, json!({"ok": true}));
    assert_eq!(
        driver.app.sessions.known["ses_1"].completed_steers[0].state,
        PendingSteerState::NotRecorded
    );
    assert_eq!(driver.app.composer.content(), "new draft");
}

#[test]
fn late_steer_ack_respects_recorded_and_uncertain_history() {
    let (mut recorded, wait, steer, steer_id) = delayed_steer_driver();
    recorded.respond(wait, wait_result("ses_1", "loop_steer", "persisted"));
    recorded.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_req = recorded.request("session.history");
    recorded.respond(
        history_req,
        history(
            vec![
                user(0, "loop_steer", "prompt"),
                user_steering(1, "loop_steer", "late steer"),
                assistant(2, "loop_steer", 0, "deep", "answer"),
            ],
            None,
            3,
        ),
    );
    assert_eq!(
        recorded.app.sessions.known["ses_1"].completed_steers[0].local_id,
        steer_id
    );
    assert_eq!(
        recorded.app.sessions.known["ses_1"].completed_steers[0].state,
        PendingSteerState::Persisted
    );
    recorded.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    recorded.step(AppEvent::Terminal(CrosstermEvent::Paste(
        "new draft".into(),
    )));
    recorded.respond(steer, json!({"ok": true}));
    assert_eq!(
        recorded.app.sessions.known["ses_1"].completed_steers[0].state,
        PendingSteerState::Persisted
    );
    assert_eq!(recorded.app.composer.content(), "new draft");

    let (mut uncertain, wait, steer, steer_id) = delayed_steer_driver();
    uncertain.respond(wait, wait_result("ses_1", "loop_steer", "persisted"));
    uncertain.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_req = uncertain.request("session.history");
    uncertain.respond_error(history_req, -32603, "history unavailable");
    let view = &uncertain.app.sessions.known["ses_1"];
    assert_eq!(
        view.live.as_ref().unwrap().pending_steers[0].local_id,
        steer_id
    );
    assert_eq!(
        view.live.as_ref().unwrap().pending_steers[0].state,
        PendingSteerState::Unconfirmed
    );
    uncertain.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    uncertain.step(AppEvent::Terminal(CrosstermEvent::Paste(
        "new draft".into(),
    )));
    uncertain.respond(steer, json!({"ok": true}));
    assert_eq!(
        uncertain.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .unwrap()
            .pending_steers[0]
            .state,
        PendingSteerState::Unconfirmed
    );
    assert_eq!(uncertain.app.composer.content(), "new draft");
}

#[test]
fn regression_scenario_d_steer_retention_single_render_and_late_response_correlation() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt D".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_D"}}),
    );
    let wait = driver.request("turn.wait");

    // Send session_state event marking session as running
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": {
                "session_id": "ses_1",
                "status": "running",
                "active_loop": {
                    "loop_id": "loop_D",
                    "status": "running_model",
                    "request_index": 0,
                    "config_revision": 0,
                    "model": "deep",
                    "pending_interaction": null
                },
                "block_reason": null
            },
            "meta": {"session_id": "ses_1", "loop_id": "loop_D", "dropped_before": 0}
        }
    })));

    // Steer 1: user types "steer text"
    driver.app.composer.set_text("steer text");
    let cmds1 = driver.app.submit_composer();
    let steer_req1 = cmds1
        .into_iter()
        .find_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(steer_req1.method, "turn.steer");

    // Agent rejects steer 1 with STEER_QUEUE_FULL (-32016)
    driver.respond_error(steer_req1, -32016, "steering queue is full");

    // Rejected steer preserves composer text and raises warning notice
    assert_eq!(driver.app.composer.content(), "steer text");
    let has_queue_full_notice = driver.app.notices().iter().any(|n| {
        n.text.contains("queue is full")
            || n.text.contains("Steering queue is full")
            || n.text.contains("-32016")
    });
    assert!(has_queue_full_notice, "queue full error must be visible");

    // User retries identical text "steer text"
    let cmds2 = driver.app.submit_composer();
    let steer_req2 = cmds2
        .into_iter()
        .find_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(steer_req2.method, "turn.steer");

    // Steer 2 is accepted by the agent
    driver.respond(steer_req2, json!({"ok": true}));
    assert!(driver.app.composer.content().is_empty());

    // Complete wait with persisted
    driver.respond(wait, wait_result("ses_1", "loop_D", "persisted"));
    let hist_req = driver.request("session.history");

    // History contains ONLY ONE "steer text" entry (the accepted one)
    driver.respond(
        hist_req,
        history(
            vec![
                user(0, "loop_D", "prompt D"),
                user_steering(1, "loop_D", "steer text"),
                assistant(2, "loop_D", 0, "deep", "answer D"),
            ],
            None,
            3,
        ),
    );

    let view = &driver.app.sessions.known["ses_1"];
    // Steer 2 is matched and recorded in transcript blocks (UserBlock with kind == Steering).
    // It must NOT be duplicated in completed_steers or rendered twice!
    let rendered_steer_1_in_completed = view.completed_steers.iter().any(|s| s.local_id == 1);
    assert!(
        !rendered_steer_1_in_completed,
        "rejected steer must not be in completed_steers"
    );

    // The transcript must have exactly one steering block
    let steering_blocks_count = view
        .transcript
        .blocks
        .iter()
        .filter(|b| match b {
            TranscriptBlock::User(u) => {
                u.kind == minicore_tui::protocol::UserMessageKindWire::Steering
            }
            _ => false,
        })
        .count();
    assert_eq!(
        steering_blocks_count, 1,
        "exactly one steering block in transcript"
    );
}

#[test]
fn regression_scenario_e_stale_wait_and_history_paging_idempotence() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    // Turn 1
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt 1".into(),
    });
    let send1 = driver.request("turn.send");
    driver.respond(
        send1,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait1 = driver.request("turn.wait");

    // Turn 1 completes normally with persisted outcome and history
    driver.respond(wait1.clone(), wait_result("ses_1", "loop_1", "persisted"));
    let hist1 = driver.request("session.history");
    // Return page 1: offset 0, next_offset 1, total 2
    driver.respond(
        hist1,
        history(vec![user(0, "loop_1", "prompt 1")], Some(1), 2),
    );
    assert_eq!(
        driver.app.sessions.known["ses_1"].transcript.loaded_count,
        1
    );

    // Automated paging fetches next page starting at offset 1
    let hist_page2 = driver.request("session.history");
    driver.respond(
        hist_page2,
        history(vec![assistant(1, "loop_1", 0, "deep", "answer 1")], None, 2),
    );
    assert_eq!(
        driver.app.sessions.known["ses_1"].transcript.loaded_count,
        2
    );

    // State notification reports idle
    let idle_state = serde_json::from_value(json!({
        "type": "session_state",
        "data": {
            "state": {
                "session_id": "ses_1",
                "status": "idle",
                "active_loop": null,
                "block_reason": null
            },
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    }))
    .unwrap();
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(idle_state),
    ))));

    assert!(driver.app.sessions.known["ses_1"].live.is_none());

    // Turn 2 is now legitimately submitted from idle state
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt 2".into(),
    });
    let send2 = driver.request("turn.send");
    driver.respond(
        send2,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_2"}}),
    );
    let _wait2 = driver.request("turn.wait");

    assert!(driver.app.sessions.known["ses_1"].live.is_some());
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id,
        "loop_2"
    );

    // Now a stale duplicate wait response for loop_1 arrives!
    driver.respond(wait1, wait_result("ses_1", "loop_1", "persisted"));

    // Stale wait1 MUST NOT overwrite Turn 2's live loop or reference
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_some());
    assert_eq!(
        view.live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id,
        "loop_2"
    );
    assert_eq!(view.live.as_ref().unwrap().user_text, "prompt 2");

    // Open a second session to verify paging conflict rejection
    driver.step(AppEvent::OpenSession {
        session_id: "ses_2".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_2")}));
    driver.respond_method("session.state", state("ses_2", "idle", Value::Null));
    let ses2_hist1 = driver.request("session.history");
    driver.respond(
        ses2_hist1,
        history(vec![user(0, "loop_x", "initial item")], Some(1), 2),
    );
    assert_eq!(
        driver.app.sessions.known["ses_2"].transcript.loaded_count,
        1
    );

    // Next page request arrives
    let ses2_hist2 = driver.request("session.history");
    // Incoming page contains conflict on already loaded item 0
    let conflict_items = vec![
        user(0, "loop_conflict", "changed item"),
        assistant(1, "loop_x", 0, "deep", "answer"),
    ];
    driver.respond(ses2_hist2, history(conflict_items, None, 2));

    // Conflict must emit error notice and retain existing items
    let has_conflict_notice = driver.app.notices().iter().any(|n| {
        n.text
            .contains("history for ses_2 changed at an existing item index")
            || n.text.contains("conflict")
    });
    assert!(has_conflict_notice, "conflict notice must be emitted");
    assert_eq!(
        driver.app.sessions.known["ses_2"].transcript.loaded_count,
        1
    );
}

#[test]
fn regression_test_close_wait_correlation_and_guards() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Submit turn -> send_turn
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "hello".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );

    // Notice that turn.wait is now queued
    let wait_req = driver.request("turn.wait");

    // Try closing session while running without confirm -> rejected
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: false,
    });
    assert!(
        driver.queue.is_empty(),
        "close without confirm must be rejected"
    );
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|n| n.text.contains("Type '/close confirm' to proceed."))
    );

    // Close session with confirm -> emits session.close (turn.wait is already inflight, so not duplicated)
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close_req = driver.request("session.close");
    assert_eq!(close_req.method, "session.close");

    // Session is closing -> send, steer, and update must be rejected
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "another".into(),
    });
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|n| n.text.contains("session is closing; cannot submit"))
    );

    driver.step(AppEvent::SteerTurn {
        session_id: "ses_1".into(),
        text: "steer while closing".into(),
    });
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|n| n.text.contains("session is closing; cannot steer"))
    );

    // Close response succeeds
    driver.respond(close_req, json!({"ok": true}));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.closing, "closing flag reset");
    assert!(!view.info.loaded, "session unloaded");
    assert!(
        view.live.is_some(),
        "live state must be retained across close until explicit reopen"
    );

    // Later turn.wait returns persistence ok -> live last_result recorded
    driver.respond(wait_req, wait_result("ses_1", "loop_1", "persisted"));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.last_result.is_some());
}

#[test]
fn regression_test_pending_config_update_loop_scoping_and_no_rollback() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Submit turn 1
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "turn 1".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    // Open model selector and select "fast"
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::MoveSelector { delta: 1 });
    driver.step(AppEvent::ConfirmDock);
    let update_req = driver.request("session.update");

    // Verify PendingConfigUpdate recorded loop_id == "loop_1"
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.config_update
            .as_ref()
            .and_then(|u| u.loop_id.as_deref()),
        Some("loop_1")
    );

    // Event request_started arrives with revision 1
    driver.step(agent_event(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 1,
            "config_revision": 1,
            "model": "fast",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));

    // Now session.update response returns with active_revision = 1
    driver.respond(
        update_req,
        json!({
            "session": {
                "session_id": "ses_1", "title": null, "profile": "coding", "workspace": "/workspace",
                "model": "fast", "reasoning": "high", "loaded": true,
                "created_at": "2026-01-02T03:04:05Z", "updated_at": "2026-01-02T03:04:05Z"
            },
            "active_revision": 1
        }),
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.config_update.as_ref().map(|u| &u.state),
        Some(&minicore_tui::state::session::ConfigUpdateState::Applied)
    );

    // Complete loop 1
    driver.respond(wait_req, wait_result("ses_1", "loop_1", "persisted"));
    // History sync replaces live
    let hist_items = vec![user(0, "loop_1", "turn 1")];
    driver.step(agent_event(json!({
        "type": "turn_finished",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {"type": "completed"},
            "persistence": "persisted",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));
    if let Some(pos) = driver
        .queue
        .iter()
        .position(|r| r.method == "session.history")
    {
        let req = driver.queue.remove(pos).unwrap();
        driver.respond(req, history(hist_items, None, 1));
    }

    // Submit turn 2 (new loop)
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "turn 2".into(),
    });
    // Verify config_update was reset so new loop doesn't inherit old Applied label
    let view = &driver.app.sessions.known["ses_1"];
    assert!(
        view.config_update.is_none(),
        "new loop must not inherit old Applied label"
    );
}

#[test]
fn regression_test_close_agent_error_single_state_check_and_store_error() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Submit turn
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "work".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let _wait_req = driver.request("turn.wait");

    // Close session with confirm
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close_req = driver.request("session.close");

    // Agent returns an internal close error; the one state verification must
    // not mistake that error for proof of unloading.
    driver.respond_error(
        close_req,
        minicore_tui::protocol::INTERNAL_ERROR,
        "busy closing",
    );

    // System must issue exactly one session.state verification request
    let verify_req = driver.request("session.state");
    assert_eq!(verify_req.method, "session.state");
    assert!(driver.queue.is_empty(), "no indefinite retry loops");

    // Only SESSION_NOT_LOADED proves that the close unloaded the session.
    driver.respond_error(
        verify_req,
        minicore_tui::protocol::SESSION_NOT_LOADED,
        "session is not loaded",
    );

    // Verify session unloaded, closing cleared, but live state retained
    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.closing);
    assert!(!view.info.loaded);
    assert!(view.live.is_some());

    // Test store error on open session
    driver.step(AppEvent::OpenSession {
        session_id: "ses_corrupt".into(),
    });
    let corrupt_req = driver.request("session.open");
    driver.respond_error(
        corrupt_req,
        minicore_tui::protocol::STORE_ERROR,
        "corrupted sqlite database",
    );
    let has_store_notice = driver.app.notices().iter().any(|n| {
        n.text.contains("Unable to open this session. Its data may be unavailable, invalid, or from an unsupported format.")
    });
    assert!(
        has_store_notice,
        "exact spec store error notice must be shown"
    );
}

#[test]
fn close_verification_internal_or_malformed_retains_loaded_state() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond_error(
        close,
        minicore_tui::protocol::INTERNAL_ERROR,
        "close failed",
    );
    let verify = driver.request("session.state");
    driver.respond_error(
        verify,
        minicore_tui::protocol::INTERNAL_ERROR,
        "state unavailable",
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.closing);
    assert!(view.info.loaded, "internal error does not prove unloaded");
    assert!(driver.app.sessions.active.as_deref() == Some("ses_1"));
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|notice| notice.text.contains("close verification is unknown"))
    );

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond_error(
        close,
        minicore_tui::protocol::INTERNAL_ERROR,
        "close failed",
    );
    let verify = driver.request("session.state");
    driver.respond(verify, json!({"not": "a session state"}));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.info.loaded, "malformed state does not prove unloaded");
    assert!(driver.app.sessions.active.as_deref() == Some("ses_1"));
}

#[test]
fn session_update_response_after_loop_finished_retains_info_and_saved_next_turn() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Submit turn 1
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "hello".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    // Initiate update during loop 1
    driver.step(AppEvent::OpenModelSelector);
    driver.step(AppEvent::MoveSelector { delta: 1 });
    driver.step(AppEvent::ConfirmDock);
    let update_req = driver.request("session.update");

    // Loop 1 finishes before session.update response arrives
    driver.respond(wait_req, wait_result("ses_1", "loop_1", "persisted"));
    let hist_items = vec![user(0, "loop_1", "hello")];
    driver.step(agent_event(json!({
        "type": "turn_finished",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {"type": "completed"},
            "persistence": "persisted",
            "meta": {"session_id": "ses_1", "loop_id": "loop_1", "dropped_before": 0}
        }
    })));
    let hist_req = driver.request("session.history");
    driver.respond(hist_req, history(hist_items, None, 1));
    let state_req = driver.request("session.state");
    driver.respond(state_req, state("ses_1", "idle", Value::Null));

    // At this point, view.live is None
    assert!(driver.app.sessions.known["ses_1"].live.is_none());

    // Now session.update response arrives with active_revision
    driver.respond(
        update_req,
        json!({
            "session": {
                "session_id": "ses_1", "title": null, "profile": "coding", "workspace": "/workspace",
                "model": "fast", "reasoning": "high", "loaded": true,
                "created_at": "2026-01-02T03:04:05Z", "updated_at": "2026-01-02T03:04:05Z"
            },
            "active_revision": 2
        }),
    );

    // view.info must be updated (session.update is durable authority), and config_update marked SavedNextTurn
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.info.model, "fast");
    assert_eq!(
        view.config_update.as_ref().map(|u| &u.state),
        Some(&minicore_tui::state::session::ConfigUpdateState::SavedNextTurn)
    );
}

#[test]
fn close_success_before_wait_response_processes_result_without_extra_state_or_history() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Submit turn
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "hello".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    // Close session while running
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close_req = driver.request("session.close");
    driver.respond(close_req, json!({"ok": true}));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.info.loaded, "session must be marked not loaded");

    // Now turn.wait response arrives
    driver.respond(wait_req, wait_result("ses_1", "loop_1", "persisted"));

    // Verify result is processed and live is temporarily visible
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.last_result.is_some());
    assert!(view.live.is_some());

    // Crucial assertion: ZERO extra session.state or session.history requests emitted!
    assert!(
        driver.queue.is_empty(),
        "closed view must not emit extra session.state or session.history requests"
    );
}

#[test]
fn shutdown_send_turn_in_flight_response_registers_wait() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Submit turn
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "hello".into(),
    });
    let send_req = driver.request("turn.send");

    // Initiate quit / shutdown while turn.send is in flight
    driver.step(AppEvent::ShutdownRequested);
    let _shutdown_req = driver.request("agent.shutdown");
    assert_eq!(
        driver.app.connection,
        minicore_tui::app::ConnectionState::ShuttingDown
    );

    // In-flight turn.send response arrives during shutdown
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );

    // Turn.wait must be immediately registered and dispatched
    let wait_req = driver.request("turn.wait");
    assert_eq!(wait_req.method, "turn.wait");
}

#[test]
fn turn_result_completed_and_persistence_failed() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "do work".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    // Agent completes loop, but persistence fails
    driver.respond(
        wait_req,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {"type": "completed"},
            "persistence": "failed",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );

    let view = driver.app.sessions.known.get("ses_1").unwrap();
    assert!(view.unsaved_loop.is_some());
    let last = view.last_result.as_ref().unwrap();
    assert_eq!(
        last.outcome,
        minicore_tui::protocol::LoopOutcomeWire::Completed
    );
    assert_eq!(
        last.persistence,
        minicore_tui::protocol::TurnPersistenceWire::Failed
    );
    assert!(matches!(
        view.state.as_ref().unwrap().status,
        minicore_tui::protocol::SessionStatusWire::Blocked
    ));
}

#[test]
fn turn_result_failed_and_persisted_with_model_error() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "generate".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    // Agent model error (e.g. rate limit), but persistence succeeded
    driver.respond(
        wait_req,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {
                "type": "failed",
                "kind": "model_error",
                "model_error": {
                    "kind": "rate_limit",
                    "delivery": "upstream",
                    "retryable": true
                }
            },
            "persistence": "persisted",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    let view = driver.app.sessions.known.get("ses_1").unwrap();
    assert!(view.unsaved_loop.is_none());
    let last = view.last_result.as_ref().unwrap();
    assert!(matches!(
        last.outcome,
        minicore_tui::protocol::LoopOutcomeWire::Failed { .. }
    ));
    assert_eq!(
        last.persistence,
        minicore_tui::protocol::TurnPersistenceWire::Persisted
    );
}

#[test]
fn turn_result_cancelled_user_and_unknown() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    // Case 1: Cancelled (user)
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "cancel me".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    driver.respond(
        wait_req,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {"type": "cancelled", "reason": "user"},
            "persistence": "persisted",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_1", "cancel me"),
                assistant(1, "loop_1", 0, "deep", "cancelled"),
            ],
            None,
            2,
        ),
    );

    let view = driver.app.sessions.known.get("ses_1").unwrap();
    let last = view.last_result.as_ref().unwrap();
    assert_eq!(
        last.outcome,
        minicore_tui::protocol::LoopOutcomeWire::Cancelled {
            reason: minicore_tui::protocol::CancelReasonWire::User
        }
    );

    // Case 2: Cancelled (unknown future reason)
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "cancel unknown".into(),
    });
    let send_req2 = driver.request("turn.send");
    driver.respond(
        send_req2,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_2"}}),
    );
    let wait_req2 = driver.request("turn.wait");

    driver.respond(
        wait_req2,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_2"},
            "outcome": {"type": "cancelled", "reason": "sandbox_evicted"},
            "persistence": "persisted",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(2, "loop_2", "cancel unknown"),
                assistant(3, "loop_2", 0, "deep", "cancelled"),
            ],
            None,
            4,
        ),
    );

    let view = driver.app.sessions.known.get("ses_1").unwrap();
    let last2 = view.last_result.as_ref().unwrap();
    assert_eq!(
        last2.outcome,
        minicore_tui::protocol::LoopOutcomeWire::Cancelled {
            reason: minicore_tui::protocol::CancelReasonWire::Unknown("sandbox_evicted".into())
        }
    );
}

#[test]
fn agent_exit_marks_live_result_unconfirmed_without_overwriting_known_result() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "crash me".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_crash"}}),
    );
    let _wait = driver.request("turn.wait");
    driver.step(AppEvent::Rpc(RpcEvent::Exited(None)));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.result_unconfirmed);
    assert!(view.live.as_ref().is_some_and(|live| live.waiting));
    assert!(view.last_result.is_none());
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|notice| { notice.text.contains("result/save status unconfirmed") })
    );
    assert!(matches!(driver.app.connection, ConnectionState::Failed(_)));

    // A known persistence-failed result remains a known result and is not
    // relabeled as transport uncertainty.
    let mut known = Driver::new();
    bootstrap(&mut known);
    open_idle(&mut known, "ses_1");
    known.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "known failure".into(),
    });
    let send = known.request("turn.send");
    known.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_failed"}}),
    );
    let wait = known.request("turn.wait");
    known.respond(wait, wait_result("ses_1", "loop_failed", "failed"));
    known.step(AppEvent::Rpc(RpcEvent::Exited(None)));
    let view = &known.app.sessions.known["ses_1"];
    assert!(!view.result_unconfirmed);
    assert!(view.last_result.is_some());
    assert!(view.unsaved_loop.is_some());
}

#[test]
fn forced_shutdown_message_combines_unknown_known_failure_and_stderr() {
    let mut unknown = Driver::new();
    bootstrap(&mut unknown);
    open_idle(&mut unknown, "ses_1");
    unknown.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "unfinished".into(),
    });
    let send = unknown.request("turn.send");
    unknown.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_unknown"}}),
    );
    let _wait = unknown.request("turn.wait");
    unknown.step(AppEvent::Rpc(RpcEvent::AgentLogLine(
        "agent hung during shutdown".into(),
    )));
    let shutdown = unknown.app.update(AppEvent::ShutdownRequested);
    assert!(shutdown.iter().any(
        |command| matches!(command, AppCommand::Rpc(request) if request.method == "agent.shutdown")
    ));
    assert!(
        unknown
            .app
            .shutdown_remaining()
            .is_some_and(|remaining| remaining <= std::time::Duration::from_secs(5))
    );
    let message = unknown.app.shutdown_force_message();
    assert!(message.contains("force-terminated"));
    assert!(message.contains("result/save status unconfirmed"));
    assert!(message.contains("agent hung during shutdown"));

    let mut known = Driver::new();
    bootstrap(&mut known);
    open_idle(&mut known, "ses_1");
    known.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "known failure".into(),
    });
    let send = known.request("turn.send");
    known.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_failed"}}),
    );
    let wait = known.request("turn.wait");
    known.respond(wait, wait_result("ses_1", "loop_failed", "failed"));
    known.step(AppEvent::Rpc(RpcEvent::AgentLogLine(
        "known failure stderr".into(),
    )));
    known.app.update(AppEvent::ShutdownRequested);
    let message = known.app.shutdown_force_message();
    assert!(message.contains("known persistence failure retained"));
    assert!(message.contains("known failure stderr"));
    assert!(!message.contains("result/save status unconfirmed"));
}

#[test]
fn shutdown_ok_after_known_failed_preserves_unsaved_and_last_result() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method("session.history", history(Vec::new(), None, 0));

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "failing turn".into(),
    });
    let send_req = driver.request("turn.send");
    driver.respond(
        send_req,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait_req = driver.request("turn.wait");

    driver.respond(
        wait_req,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "outcome": {"type": "completed"},
            "persistence": "failed",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0
        }),
    );

    let view = driver.app.sessions.known.get("ses_1").unwrap();
    assert!(view.unsaved_loop.is_some());
    assert!(view.last_result.is_some());

    // Shutdown requested and acknowledged
    driver.step(AppEvent::ShutdownRequested);
    let shutdown_req = driver.request("agent.shutdown");
    driver.respond(shutdown_req, json!({"ok": true}));

    // Verified: unsaved_loop and last_result are preserved and not cleared by shutdown ok
    let view = driver.app.sessions.known.get("ses_1").unwrap();
    assert!(
        view.unsaved_loop.is_some(),
        "unsaved banner must be preserved after shutdown"
    );
    assert!(
        view.last_result.is_some(),
        "last_result must be preserved after shutdown"
    );
}
