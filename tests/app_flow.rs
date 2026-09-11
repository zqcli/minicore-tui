//! Focused App reducer tests for the Agent v0.3 / TUI r2 contract.

use std::collections::VecDeque;
use std::path::PathBuf;

use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::Line;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

use minicore_tui::app::{App, ConnectionState, RequestKind};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{
    IncomingFrame, OutgoingRequest, RpcNotification, RpcResponse, SessionStatusWire,
};
use minicore_tui::state::tool::ToolStatus;
use minicore_tui::state::turn::{PendingSteerState, SteerQueueState};
use minicore_tui::state::{AssistantPart, TranscriptBlock};

struct Driver {
    app: App,
    queue: VecDeque<OutgoingRequest>,
    copies: Vec<String>,
    exited: bool,
}

impl Driver {
    fn new() -> Self {
        Self {
            app: App::new(PathBuf::from("/workspace")),
            queue: VecDeque::new(),
            copies: Vec::new(),
            exited: false,
        }
    }

    fn step(&mut self, event: AppEvent) {
        for command in self.app.update(event) {
            match command {
                AppCommand::Rpc(request) if request.method == "session.presentation" => {
                    let session_id = request.params["session_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    self.respond(
                        request,
                        json!({
                            "session_id": session_id,
                            "context": {"kind": "unknown"}
                        }),
                    );
                }
                AppCommand::Rpc(request) => self.queue.push_back(request),
                AppCommand::KillChild => {}
                AppCommand::CopySelection(text) => self.copies.push(text.as_str().to_owned()),
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
    driver.step(AppEvent::ToggleReasoningSection {
        session_id: "ses_1".into(),
        loop_id: "loop_live_markdown".into(),
        request_index: 0,
        ordinal: 0,
    });
    driver.step(AppEvent::ToggleReasoningSection {
        session_id: "ses_1".into(),
        loop_id: "loop_live_markdown".into(),
        request_index: 1,
        ordinal: 0,
    });

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
    for request_index in 0..2 {
        driver.step(AppEvent::ToggleReasoningSection {
            session_id: "ses_1".into(),
            loop_id: "loop_history_markdown".into(),
            request_index,
            ordinal: 0,
        });
    }

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
    driver.step(AppEvent::ToggleReasoningSection {
        session_id: "ses_1".into(),
        loop_id: "loop_cjk_reasoning".into(),
        request_index: 0,
        ordinal: 0,
    });

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

    let prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 16);
    driver.step(AppEvent::ConversationPrepared(prepared));
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
    let light_prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 16);
    driver.step(AppEvent::ConversationPrepared(light_prepared));
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
fn ordered_live_parts_keep_late_reasoning_after_same_request_text() {
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
    driver.step(AppEvent::ToggleReasoningSection {
        session_id: "ses_1".into(),
        loop_id: "loop_late_reasoning".into(),
        request_index: 0,
        ordinal: 0,
    });

    let lines = transcript_lines(&driver.app);
    let reasoning = line_position(&lines, "late_r0_bold");
    let text = line_position(&lines, "late_r0_answer");
    assert!(
        text < reasoning,
        "known live part order must not move late reasoning before text"
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

    // 0.2.4 FIFO pacing: steer 1 issues immediately; the duplicate-text
    // steer 2 is admitted locally but NOT issued before steer 1's receipt.
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
    driver.respond(
        steer1,
        json!({"ok": true, "accepteAt": null, "steer_index": 1}),
    );

    // Duplicate text admitted, still unsent (only ONE steer in flight until
    // the receipt proves steer 1 entered a request history).
    driver.app.composer.set_text("retry");
    let commands = driver.app.submit_composer();
    let rpcs: Vec<_> = commands
        .into_iter()
        .filter(|c| matches!(c, AppCommand::Rpc(_)))
        .collect();
    assert!(
        rpcs.is_empty(),
        "no second steer RPC before receipt: {rpcs:?}"
    );
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(view.steer_queue.len(), 1, "duplicate retained unsent");
        assert_eq!(view.live.as_ref().unwrap().pending_steers.len(), 1);
        assert_eq!(
            view.live.as_ref().unwrap().pending_steers[0].state,
            minicore_tui::state::PendingSteerState::Queued
        );
    }

    // Receipt for steer 1 (request 0 history applied 1 steer): applied, and
    // the central advance now issues steer 2.
    driver.step(agent_event(json!({
        "type": "steer_progress",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "request_index": 0,
            "applied_count": 1,
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));
    let steer2 = driver.request("turn.steer");
    assert_eq!(
        steer2.params["text"], "retry",
        "FIFO: the duplicate is issued after the first receipt"
    );
    driver.respond(
        steer2,
        json!({"ok": true, "accepteAt": null, "steer_index": 2}),
    );
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert!(view.steer_queue.is_empty(), "both steers now in flight");
        assert_eq!(view.applied_steers.len(), 1);
        assert_eq!(view.live.as_ref().unwrap().pending_steers.len(), 1);
    }

    // Wait finishes
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));

    // History returns two steering items matching FIFO (exact-once replace).
    driver.respond_method("session.history", history(vec![
        user(0, "loop_1", "prompt"),
        json!({"index": 1, "item": {"type": "user", "data": {"loop_id": "loop_1", "kind": "steering", "text": "retry"}}}),
        json!({"index": 2, "item": {"type": "user", "data": {"loop_id": "loop_1", "kind": "steering", "text": "retry"}}}),
        assistant(3, "loop_1", 1, "deep", "finished"),
    ], None, 4));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    assert_eq!(view.transcript.items.len(), 4);
    assert!(
        view.applied_steers.is_empty(),
        "durable history replaced all applied steer cards exactly once"
    );
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

    // A blocked state remains unsafe even after close; direct deletion is
    // refused until the state is known to be idle.
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    assert!(driver.queue.iter().all(|r| r.method != "session.delete"));
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|notice| notice.text.contains("busy or its result is unconfirmed"))
    );

    // Once the retained state is idle, the closed session can be deleted.
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .state
        .as_mut()
        .unwrap()
        .status = minicore_tui::protocol::SessionStatusWire::Idle;
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
    // 0.2.4 D: the rejected steer was restored into the unsent queue (visibly
    // PAUSED) and, after the user's explicit retry, re-issued as the SAME
    // message. It is recorded exactly once - never duplicated and never lost.
    let queued_after_finish = view
        .steer_queue
        .iter()
        .any(|item| item.text == "steer text" && item.state == SteerQueueState::Unsent);
    assert!(
        queued_after_finish,
        "the retried message finished the loop; any later duplicate stays queued with no fake send"
    );
    let completed_matching = view
        .completed_steers
        .iter()
        .filter(|s| s.text == "steer text")
        .count();
    assert!(
        completed_matching <= 1,
        "the accepted message is recorded exactly once in completed_steers"
    );
    // The transcript must have exactly one steering block.
    let steering_blocks_count = view
        .transcript
        .blocks
        .iter()
        .filter(|b| matches!(b, TranscriptBlock::User(u) if u.kind == minicore_tui::protocol::UserMessageKindWire::Steering))
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
    assert!(
        view.state.is_none(),
        "unknown close verification must not retain idle as a destructive-action permit"
    );
    assert!(driver.app.sessions.active.as_deref() == Some("ses_1"));
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|notice| notice.text.contains("close verification is unknown"))
    );

    // An unknown close outcome may not reuse the retained idle snapshot as
    // permission to issue another close. The next explicit confirmation must
    // perform a fresh state read first.
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let recheck = driver.request("session.state");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.close"),
        "unknown close outcome must not send close before the recheck"
    );
    driver.respond(recheck, state("ses_1", "idle", Value::Null));

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
fn close_verification_running_state_is_written_without_cancelling_the_turn() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "hello".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let _wait = driver.request("turn.wait");

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond_error(
        close,
        minicore_tui::protocol::INTERNAL_ERROR,
        "busy closing",
    );
    let verify = driver.request("session.state");
    driver.respond(
        verify,
        state(
            "ses_1",
            "running",
            json!({
                "loop_id": "loop_1",
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            }),
        ),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.state.as_ref().unwrap().status,
        SessionStatusWire::Running
    );
    assert!(view.info.loaded);
    assert!(
        view.live.is_some(),
        "running state must not cancel the live turn"
    );
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_1"));
    assert!(!view.closing);

    // The state read is authoritative for the local projection. A running
    // session remains protected by the normal non-confirmed safety guard.
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: false,
    });
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.close"),
        "running state must block an unconfirmed close"
    );
    assert!(driver.app.sessions.known["ses_1"].live.is_some());
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_1"));
}

#[test]
fn close_verification_blocked_state_remains_unsafe_for_delete() {
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
    driver.respond(verify, state("ses_1", "blocked", Value::Null));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.is_blocked());
    assert!(view.info.loaded);
    assert!(!view.closing);
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.delete")
    );
    assert!(!driver.app.sessions.deleted.contains("ses_1"));
}

#[test]
fn close_verification_transport_failure_requires_a_fresh_state_read() {
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
    driver.step(AppEvent::RpcSendFailed {
        id: verify.id,
        error: minicore_tui::rpc::RpcError::Closed,
    });

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let recheck = driver.request("session.state");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.close"),
        "transport-unknown close must not reuse the old idle state"
    );
    driver.respond(recheck, state("ses_1", "idle", Value::Null));
}

#[test]
fn event_gap_blocks_close_and_delete_for_loaded_and_closed_sessions() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .event_gap = true;

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    assert!(driver.queue.iter().all(|request| {
        request.method != "session.close" && request.method != "session.delete"
    }));

    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .event_gap = false;
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond(close, json!({"ok": true}));
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .event_gap = true;

    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.delete")
    );
}

#[test]
fn history_failure_clear_and_reopen_keep_destructive_actions_guarded() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    // Start an incomplete history read, then fail it. The view must remain
    // incomplete and unsafe for lifecycle actions.
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .transcript
        .complete = false;
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let failed_history = driver.request("session.history");
    assert_eq!(
        driver.app.pending_requests.get(&failed_history.id),
        Some(&minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 20,
            gap_revision: Some(0),
        })
    );
    let failed_revision = driver.app.sessions.known["ses_1"].gap_revision;
    driver.respond_error(
        failed_history,
        minicore_tui::protocol::INTERNAL_ERROR,
        "history unavailable",
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.event_gap);
    assert!(!view.transcript.complete);
    assert!(!view.loading);

    // `/clear` starts a new history read but must not erase the safety fence.
    submit_command(&mut driver, "/clear");
    let clear_history = driver.request("session.history");
    assert_eq!(
        driver.app.pending_requests.get(&clear_history.id),
        Some(&minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 20,
            gap_revision: Some(failed_revision),
        })
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.event_gap);
    assert!(!view.transcript.complete);
    assert!(view.loading);

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    assert!(driver.queue.iter().all(|request| {
        request.method != "session.close" && request.method != "session.delete"
    }));

    // A complete response aligned with the current gap revision releases the
    // guard. Closing may then succeed, and the now-unloaded catalog entry may
    // be deleted without needing a fabricated local transcript state.
    driver.respond(clear_history, history(Vec::new(), None, 0));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.event_gap);
    assert!(view.transcript.complete);
    assert!(!view.loading);

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond(close, json!({"ok": true}));
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let delete = driver.request("session.delete");
    driver.respond(delete, json!({"ok": true}));

    // Reopening an already closed view must retain an existing gap and carry
    // its revision into the new history request.
    let mut reopen = Driver::new();
    bootstrap(&mut reopen);
    open_idle(&mut reopen, "ses_1");
    reopen.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = reopen.request("session.close");
    reopen.respond(close, json!({"ok": true}));
    let reopen_revision = 7;
    {
        let view = reopen.app.sessions.known.get_mut("ses_1").unwrap();
        view.event_gap = true;
        view.gap_revision = reopen_revision;
    }
    reopen.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    let open = reopen.request("session.open");
    reopen.respond(open, json!({"session": session("ses_1")}));
    let history = reopen.request("session.history");
    assert_eq!(
        reopen.app.pending_requests.get(&history.id),
        Some(&minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 20,
            gap_revision: Some(reopen_revision),
        })
    );
    assert!(reopen.app.sessions.known["ses_1"].event_gap);
    assert!(!reopen.app.sessions.known["ses_1"].transcript.complete);
}

#[test]
fn inflight_history_gap_reconciles_new_revision_before_lifecycle_actions() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let initial_history = driver.request("session.history");
    assert_eq!(
        driver.app.pending_requests.get(&initial_history.id),
        Some(&RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 20,
            gap_revision: Some(0),
        })
    );

    // A dropped lifecycle event arrives while the initial history request is
    // still in flight. The event marks a new revision but cannot start a
    // second request over the current one.
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state("ses_1", "idle", Value::Null),
            "meta": {
                "session_id": "ses_1",
                "loop_id": null,
                "dropped_before": 1
            }
        }
    })));
    assert!(driver.app.sessions.known["ses_1"].event_gap);
    assert_eq!(driver.app.sessions.known["ses_1"].gap_revision, 1);
    assert!(driver.queue.iter().all(|request| {
        request.method != "session.close" && request.method != "session.delete"
    }));

    // The old response may still merge its history, but it must not authorize
    // lifecycle actions or clear the newer gap. It must schedule a fresh read
    // carrying the current revision.
    driver.respond(initial_history, history(Vec::new(), None, 0));
    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    assert!(driver.queue.iter().all(|request| {
        request.method != "session.close" && request.method != "session.delete"
    }));
    let retry = driver.request("session.history");
    assert_eq!(
        driver.app.pending_requests.get(&retry.id),
        Some(&RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 20,
            gap_revision: Some(1),
        })
    );
    assert!(driver.app.sessions.known["ses_1"].event_gap);
    assert!(driver.app.sessions.known["ses_1"].loading);
    assert!(driver.app.sessions.known["ses_1"].reconcile_inflight);

    // Only the complete response for the new revision releases the fence.
    driver.respond(retry, history(Vec::new(), None, 0));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.event_gap);
    assert!(view.transcript.complete);
    assert!(!view.loading);
    assert!(!view.reconcile_inflight);

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond(close, json!({"ok": true}));
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let delete = driver.request("session.delete");
    driver.respond(delete, json!({"ok": true}));
    assert!(driver.app.sessions.deleted.contains("ses_1"));
}

#[test]
fn legacy_none_history_reply_cannot_release_a_new_event_gap() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let old_history = driver.request("session.history");

    // Model a request created by the pre-revision protocol: its response has
    // no captured revision. This must remain unsafe after a newer gap.
    if let Some(RequestKind::History { gap_revision, .. }) =
        driver.app.pending_requests.get_mut(&old_history.id)
    {
        *gap_revision = None;
    }
    driver.step(agent_event(json!({
        "type": "session_state",
        "data": {
            "state": state("ses_1", "idle", Value::Null),
            "meta": {
                "session_id": "ses_1",
                "loop_id": null,
                "dropped_before": 1
            }
        }
    })));
    driver.respond(old_history, history(Vec::new(), None, 0));

    assert!(driver.app.sessions.known["ses_1"].event_gap);
    let retry = driver.request("session.history");
    assert_eq!(
        driver.app.pending_requests.get(&retry.id),
        Some(&RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 20,
            gap_revision: Some(1),
        })
    );
}

#[test]
fn deleted_session_id_rejects_late_lifecycle_responses_and_events() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::CloseSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let close = driver.request("session.close");
    driver.respond(close, json!({"ok": true}));
    driver.step(AppEvent::DeleteSession {
        session_id: "ses_1".into(),
        confirm: true,
    });
    let delete = driver.request("session.delete");

    for (request_id, kind) in [
        (
            minicore_tui::protocol::RequestId(80_010),
            minicore_tui::app::RequestKind::OpenSession {
                session_id: "ses_1".into(),
                previous_retired_loop: None,
            },
        ),
        (
            minicore_tui::protocol::RequestId(80_011),
            minicore_tui::app::RequestKind::RenameSession {
                session_id: "ses_1".into(),
            },
        ),
        (
            minicore_tui::protocol::RequestId(80_012),
            minicore_tui::app::RequestKind::SessionState {
                session_id: "ses_1".into(),
                query: 80,
            },
        ),
        (
            minicore_tui::protocol::RequestId(80_013),
            minicore_tui::app::RequestKind::History {
                session_id: "ses_1".into(),
                offset: 0,
                limit: 100,
                gap_revision: None,
            },
        ),
    ] {
        driver.app.pending_requests.insert(request_id, kind);
    }
    driver.respond(delete, json!({"ok": true}));
    assert!(driver.app.sessions.deleted.contains("ses_1"));
    assert!(!driver.app.sessions.known.contains_key("ses_1"));
    assert_eq!(driver.app.sessions.active, None);
    for request_id in [80_010, 80_011, 80_012, 80_013] {
        assert!(
            !driver
                .app
                .pending_requests
                .contains_key(&minicore_tui::protocol::RequestId(request_id)),
            "delete must retire lifecycle request {request_id}"
        );
    }

    let late_open_id = minicore_tui::protocol::RequestId(80_001);
    driver.app.pending_requests.insert(
        late_open_id,
        minicore_tui::app::RequestKind::OpenSession {
            session_id: "ses_1".into(),
            previous_retired_loop: None,
        },
    );
    driver.respond(
        minicore_tui::protocol::OutgoingRequest::session_open(late_open_id, "ses_1"),
        json!({"session": session("ses_1")}),
    );

    let late_rename_id = minicore_tui::protocol::RequestId(80_002);
    driver.app.pending_requests.insert(
        late_rename_id,
        minicore_tui::app::RequestKind::RenameSession {
            session_id: "ses_1".into(),
        },
    );
    driver.respond(
        minicore_tui::protocol::OutgoingRequest::session_rename(late_rename_id, "ses_1", "late"),
        json!({"session": session("ses_1")}),
    );

    let late_state_id = minicore_tui::protocol::RequestId(80_003);
    driver.app.pending_requests.insert(
        late_state_id,
        minicore_tui::app::RequestKind::SessionState {
            session_id: "ses_1".into(),
            query: 99,
        },
    );
    driver.respond(
        minicore_tui::protocol::OutgoingRequest::session_state(late_state_id, "ses_1"),
        state("ses_1", "idle", Value::Null),
    );

    let late_history_id = minicore_tui::protocol::RequestId(80_004);
    driver.app.pending_requests.insert(
        late_history_id,
        minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            offset: 0,
            limit: 100,
            gap_revision: None,
        },
    );
    driver.respond(
        minicore_tui::protocol::OutgoingRequest::session_history(
            late_history_id,
            "ses_1",
            Some(0),
            Some(100),
        ),
        history(Vec::new(), None, 0),
    );

    driver.step(agent_event(json!({
        "type": "session_opened",
        "data": {
            "session": session("ses_1"),
            "meta": {"session_id": "ses_1", "loop_id": null, "dropped_before": 0}
        }
    })));

    assert!(driver.app.sessions.deleted.contains("ses_1"));
    assert!(!driver.app.sessions.known.contains_key("ses_1"));
    assert_eq!(driver.app.sessions.active, None);
    assert!(
        driver.queue.is_empty(),
        "late lifecycle inputs must not emit RPCs"
    );
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
fn failed_tool_survives_live_finished_wait_and_history_with_folded_geometry() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "run the failing tool".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_failed_tool"}}),
    );
    let wait = driver.request("turn.wait");
    driver.step(agent_event(json!({
        "type": "turn_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_failed_tool"},
            "meta": {"session_id": "ses_1", "loop_id": "loop_failed_tool", "dropped_before": 0}
        }
    })));
    request_started(&mut driver, "loop_failed_tool", 0);
    driver.step(agent_event(json!({
        "type": "tool_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_failed_tool"},
            "request_index": 0,
            "tool_call_id": "call_failed",
            "tool_name": "bash",
            "meta": {"session_id": "ses_1", "loop_id": "loop_failed_tool", "dropped_before": 0}
        }
    })));
    driver.step(agent_event(json!({
        "type": "tool_presentation",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_failed_tool"},
            "request_index": 0,
            "tool_call_id": "call_failed",
            "tool_name": "bash",
            "display": {
                "detail": "$ failing command",
                "hidden_line_count": 20
            },
            "meta": {"session_id": "ses_1", "loop_id": "loop_failed_tool", "dropped_before": 0}
        }
    })));
    let error_body = "tool execution failed".to_owned();
    driver.step(agent_event(json!({
        "type": "tool_finished",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_failed_tool"},
            "request_index": 0,
            "tool_call_id": "call_failed",
            "result": {
                "outcome": "failed",
                "content_bytes": error_body.len(),
                "content": error_body,
                "content_truncated": false
            },
            "meta": {"session_id": "ses_1", "loop_id": "loop_failed_tool", "dropped_before": 0}
        }
    })));

    let collapsed = transcript_lines(&driver.app);
    let collapsed_text = collapsed
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(collapsed_text.contains("failed: tool execution failed"));
    assert!(collapsed_text.contains("ctrl+o to expand"));

    let prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 100);
    let tool_section = prepared
        .sections
        .iter()
        .find(|section| {
            section.id.kind == minicore_tui::state::view::SectionKind::Tool
                && section.id.loop_id.as_deref() == Some("loop_failed_tool")
                && section.id.tool_call_id.as_deref() == Some("call_failed")
                && section.id.history_index.is_none()
        })
        .expect("failed live tool section");
    let screen = minicore_tui::ui::layout::screen_layout(
        &driver.app,
        ratatui::layout::Rect::new(0, 0, 100, 24),
    );
    let offset = prepared
        .total_rows()
        .saturating_sub(screen.transcript.height as usize);
    let row = screen.transcript.y + (tool_section.rows.start - offset) as u16;
    let column = screen.content.x + tool_section.content_columns.start as u16;
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    driver.step(mouse(crossterm::event::MouseEventKind::Down(
        crossterm::event::MouseButton::Left,
    )));
    driver.step(mouse(crossterm::event::MouseEventKind::Up(
        crossterm::event::MouseButton::Left,
    )));

    let expanded = transcript_lines(&driver.app);
    let expanded_text = expanded
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(expanded_text.contains("tool execution failed"));
    assert!(!expanded_text.contains("ctrl+o to expand"));
    assert_eq!(
        driver.app.sessions.known["ses_1"].tool_folds[&minicore_tui::state::tool::ToolKey::new(
            "ses_1",
            "loop_failed_tool",
            0,
            "call_failed",
        )],
        minicore_tui::state::view::FoldOverride::Expanded
    );

    driver.respond(
        wait,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_failed_tool"},
            "outcome": {"type": "completed"},
            "usage": {},
            "requests": 1,
            "tool_rounds": 1,
            "final_config_revision": 0,
            "persistence": "persisted"
        }),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_failed_tool", "run the failing tool"),
                json!({
                    "index": 1,
                    "item": {"type": "assistant", "data": {
                        "loop_id": "loop_failed_tool", "request_index": 0,
                        "model": "deep", "reasoning_level": "high", "text": "",
                        "reasoning": "", "tool_calls": [{
                            "tool_call_id": "call_failed", "name": "bash", "call_index": 0
                        }], "usage": {}, "finish_reason": "tool_calls"
                    }}
                }),
                json!({
                    "index": 2,
                    "item": {"type": "tool_result", "data": {
                        "loop_id": "loop_failed_tool", "request_index": 0,
                        "tool_call_id": "call_failed", "tool_name": "bash",
                        "outcome": "failed", "content": "tool failed",
                        "content_truncated": false
                    }}
                }),
            ],
            None,
            3,
        ),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    let durable = transcript_lines(&driver.app);
    let durable_text = durable.iter().map(line_text).collect::<Vec<_>>().join("\n");
    assert!(durable_text.contains("tool failed"));

    let final_prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 100);
    let durable_tool = final_prepared
        .sections
        .iter()
        .find(|section| {
            section.id.kind == minicore_tui::state::view::SectionKind::Tool
                && section.id.loop_id.as_deref() == Some("loop_failed_tool")
                && section.id.tool_call_id.as_deref() == Some("call_failed")
                && section.id.history_index == Some(1)
        })
        .expect("final durable failed tool section");
    assert!(!durable_tool.folded);
    let body_copy = final_prepared
        .copy_ranges
        .iter()
        .find(|range| {
            range.row >= durable_tool.rows.start
                && range.row < durable_tool.rows.end
                && range.text.contains("tool failed")
        })
        .expect("final durable failed result is copy-visible");
    let section_copy = final_prepared
        .copy_ranges
        .iter()
        .filter(|range| range.row >= durable_tool.rows.start && range.row < durable_tool.rows.end)
        .map(|range| range.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(section_copy.contains("tool failed"));
    assert!(!section_copy.contains("ctrl+o to expand"));

    let hit_section = final_prepared
        .section_at(durable_tool.rows.start, durable_tool.content_columns.start)
        .expect("final durable tool row must hit its prepared section");
    assert_eq!(hit_section.id, durable_tool.id);
    driver.step(AppEvent::ConversationPrepared(final_prepared.clone()));

    let final_screen = minicore_tui::ui::layout::screen_layout(
        &driver.app,
        ratatui::layout::Rect::new(0, 0, 100, 24),
    );
    let final_offset = final_prepared
        .total_rows()
        .saturating_sub(final_screen.transcript.height as usize);
    assert!(
        body_copy.row >= final_offset,
        "final result row must be visible"
    );
    let body_row = final_screen.transcript.y + (body_copy.row - final_offset) as u16;
    let body_start = final_screen.content.x + body_copy.columns.start as u16;
    let body_end =
        body_start + UnicodeWidthStr::width(body_copy.text.as_str()).saturating_sub(1) as u16;
    let final_mouse = |kind, column, row| {
        AppEvent::Terminal(CrosstermEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    driver.step(final_mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        body_start,
        body_row,
    ));
    driver.step(final_mouse(
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        body_end,
        body_row,
    ));
    assert!(
        driver
            .copies
            .last()
            .is_some_and(|copy| copy.contains("tool failed")),
        "final durable result must copy through the App path: {:?}",
        driver.copies
    );

    let hit_row = final_screen.transcript.y + (durable_tool.rows.start - final_offset) as u16;
    let hit_column = final_screen.content.x + durable_tool.content_columns.start as u16;
    driver.step(final_mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        hit_column,
        hit_row,
    ));
    driver.step(final_mouse(
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        hit_column,
        hit_row,
    ));
    let key =
        minicore_tui::state::tool::ToolKey::new("ses_1", "loop_failed_tool", 0, "call_failed");
    assert_eq!(
        driver.app.sessions.known["ses_1"].tool_folds.get(&key),
        Some(&minicore_tui::state::view::FoldOverride::Collapsed)
    );
    let final_collapsed = transcript_lines(&driver.app);
    let final_collapsed_text = final_collapsed
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(final_collapsed_text.contains("failed: tool failed"));
    assert!(final_collapsed_text.contains("ctrl+o to expand"));
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

/// 0.2.4 repro (TUI steer Sending guard): while one turn.steer is still in
/// flight (Sending), a second steer submission is silently dropped by
/// `steer_turn_with_revision` — no RPC, no pending entry — and the editor
/// retains the second text; a late ACK for the first steer must not clear it.
#[test]
fn second_steer_while_first_sending_is_dropped_and_editor_retains_it() {
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

    // Task A: submitted while nothing is in flight -> turn.steer A (Sending).
    driver.step(AppEvent::SteerTurn {
        session_id: "ses_1".into(),
        text: "taskA".into(),
    });
    let steer_a = driver.request("turn.steer");
    let pending = driver.app.sessions.known["ses_1"]
        .live
        .as_ref()
        .unwrap()
        .pending_steers
        .clone();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, PendingSteerState::Sending);

    // Task B submitted while A is still Sending -> guard drops it silently.
    driver.step(AppEvent::SteerTurn {
        session_id: "ses_1".into(),
        text: "taskB".into(),
    });
    assert!(
        driver.queue.is_empty(),
        "a second steer must not be sent while one is still Sending"
    );
    let pending = driver.app.sessions.known["ses_1"]
        .live
        .as_ref()
        .unwrap()
        .pending_steers
        .clone();
    assert_eq!(pending.len(), 1, "taskB is not queued, it is dropped");
    assert_eq!(pending[0].text, "taskA");

    // A late ACK for A must not clear or otherwise disturb taskB.
    driver.respond(
        steer_a,
        json!({"ok": true, "accepted_at": "2026-01-02T03:04:05.000Z"}),
    );
    let pending = driver.app.sessions.known["ses_1"]
        .live
        .as_ref()
        .unwrap()
        .pending_steers
        .clone();
    assert_eq!(pending[0].state, PendingSteerState::Queued);
    assert_eq!(pending.len(), 1);
}

/// 0.2.4 repro: the same Sending guard also drops a duplicate-text second
/// steer while the first is still Sending.
#[test]
fn duplicate_text_steer_while_first_sending_is_dropped() {
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
    driver.step(AppEvent::SteerTurn {
        session_id: "ses_1".into(),
        text: "taskA".into(),
    });
    let _steer_a = driver.request("turn.steer");
    driver.step(AppEvent::SteerTurn {
        session_id: "ses_1".into(),
        text: "taskA".into(),
    });
    assert!(
        driver.queue.is_empty(),
        "duplicate steer dropped while Sending"
    );
    let pending = driver.app.sessions.known["ses_1"]
        .live
        .as_ref()
        .unwrap()
        .pending_steers
        .clone();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "taskA");
}

/// 0.2.4 D: a definitive channel/serialization send failure for a steer is a
/// "definitely not accepted" outcome: the exact message is restored into the
/// local unsent queue (FIFO front) and PAUSED, whatever the composer holds.
#[test]
fn send_failed_steer_restores_unsent_paused_and_preserves_editor() {
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
    request_started(&mut driver, "loop_1", 0);

    driver.app.composer.set_text("important instruction");
    let commands = driver.app.submit_composer();
    let steer = commands
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .find(|r| r.method == "turn.steer")
        .expect("steer issued");

    // New editor content after admission (must be preserved).
    driver.app.composer.set_text("new editor draft");
    // Simulate a definite send failure (channel closed before write).
    driver.step(AppEvent::RpcSendFailed {
        id: steer.id,
        error: minicore_tui::rpc::RpcError::Closed,
    });
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.steer_queue.len(),
        1,
        "message restored into unsent queue"
    );
    assert_eq!(view.steer_queue[0].text, "important instruction");
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(view.steer_queue_paused, "paused after a definite failure");
    assert!(view.live.as_ref().unwrap().pending_steers.is_empty());
    assert_eq!(
        driver.app.composer.content(),
        "new editor draft",
        "editor preserved"
    );
}

/// 0.2.4 D: an Agent rejection with a typed error (queue full) is definitive:
/// restore the message Unsent + paused at the FIFO front.
#[test]
fn agent_rejected_steer_restores_unsent_paused_at_fifo_front() {
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

    let steer = {
        driver.app.composer.set_text("steer A");
        let commands = driver.app.submit_composer();
        commands
            .into_iter()
            .filter_map(|c| match c {
                AppCommand::Rpc(r) => Some(r),
                _ => None,
            })
            .find(|r| r.method == "turn.steer")
            .expect("steer A issued")
    };
    driver.respond_error(steer, -32016, "steering queue full");

    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1, "rejected steer restored unsent");
    assert_eq!(view.steer_queue[0].text, "steer A");
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(view.steer_queue_paused);
    assert!(view.live.as_ref().unwrap().pending_steers.is_empty());
}

/// 0.2.4 D: an undecodable steer response is AMBIGUOUS (the agent may have
/// accepted it): the entry stays Unconfirmed + paused, never copied into the
/// unsent queue that a new Enter would auto-unpause.
#[test]
fn ambiguous_steer_response_keeps_unconfirmed_and_never_resends() {
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

    let steer = {
        driver.app.composer.set_text("steer X");
        let commands = driver.app.submit_composer();
        commands
            .into_iter()
            .filter_map(|c| match c {
                AppCommand::Rpc(r) => Some(r),
                _ => None,
            })
            .find(|r| r.method == "turn.steer")
            .expect("steer issued")
    };
    // Malformed result payload: cannot decode -> ambiguous.
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: steer.id,
            result: Some(json!({"bogus": true})),
            error: None,
        },
    ))));

    let view = &driver.app.sessions.known["ses_1"];
    assert!(
        view.steer_queue.is_empty(),
        "ambiguous must not enter the unsent queue"
    );
    assert!(
        view.live.as_ref().unwrap().pending_steers[0].state
            == minicore_tui::state::PendingSteerState::Unconfirmed,
        "ambiguous stays Unconfirmed"
    );
    assert!(
        view.steer_queue_paused,
        "paused; never auto-resends an ambiguous acceptance"
    );
}

// ---- 0.2.4 LAST-FIX: fresh-turn handoff failure handling ----

/// Completes a running loop that admitted two steers (first in flight, second
/// FIFO-blocked unsent), settles the session to a completed+persisted+history
/// idle state, lets the central advance re-submit the second steer as a
/// fresh-turn handoff (`turn.send`, queue entry kept with `handoff=true`) and
/// returns that request.
fn handoff_setup(driver: &mut Driver) -> OutgoingRequest {
    bootstrap(driver);
    open_idle(driver, "ses_1");
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

    driver.app.composer.set_text("first");
    let steer1 = driver
        .app
        .submit_composer()
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .find(|r| r.method == "turn.steer")
        .expect("first steer issued");
    driver.respond(steer1, json!({"ok": true, "steer_index": 1}));

    // Second steer admitted locally but FIFO-blocked (first still in flight).
    driver.app.composer.set_text("second");
    let rpcs: Vec<_> = driver
        .app
        .submit_composer()
        .into_iter()
        .filter(|c| matches!(c, AppCommand::Rpc(_)))
        .collect();
    assert!(
        rpcs.is_empty(),
        "second steer must stay unsent while first is in flight"
    );
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(view.steer_queue.len(), 1);
        assert_eq!(view.steer_queue[0].text, "second");
        assert_eq!(
            view.steer_queue[0].state,
            minicore_tui::state::turn::SteerQueueState::Unsent
        );
    }

    // Loop completes + persists; history settles with only the FIRST steer.
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    driver.respond_method(
        "session.history",
        history(
            vec![
                user(0, "loop_1", "prompt"),
                json!({"index": 1, "item": {"type": "user", "data": {"loop_id": "loop_1", "kind": "steering", "text": "first"}}}),
                assistant(2, "loop_1", 1, "deep", "done"),
            ],
            None,
            3,
        ),
    );

    // Settled idle: the central advance re-submits "second" as a fresh turn.
    driver.app.update(AppEvent::Tick);
    let handoff = driver.request("turn.send");
    assert_eq!(
        handoff.params["text"], "second",
        "handoff sends the queued text"
    );
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(
            view.steer_queue.len(),
            1,
            "entry kept while handoff in flight"
        );
        assert!(view.steer_queue[0].handoff, "handoff marker blocks FIFO");
    }
    handoff
}

#[test]
fn handoff_malformed_response_keeps_unconfirmed_blocks_advance_and_never_auto_resends() {
    let mut driver = Driver::new();
    let handoff = handoff_setup(&mut driver);

    // Malformed result payload: the request reached the Agent but its response
    // cannot be decoded -> outcome is UNCERTAIN. It must never be treated as a
    // definitive failure (auto-resend) nor as plain Unsent.
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: handoff.id,
            result: Some(json!({"bogus": true})),
            error: None,
        },
    ))));
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(
            view.steer_queue[0].state,
            minicore_tui::state::turn::SteerQueueState::Unconfirmed,
            "uncertain handoff must be visibly unconfirmed, never Unsent"
        );
        assert!(
            !view.steer_queue[0].handoff,
            "handoff marker released once the outcome is final"
        );
        assert!(
            view.steer_queue_paused,
            "uncertain outcome pauses the queue"
        );
    }
    assert!(
        driver.app.composer.content().is_empty(),
        "a handoff owns its queued text: NO composer copy (would duplicate on Enter)"
    );

    // Tick and a brand-new message must never re-send the original text.
    driver.app.update(AppEvent::Tick);
    assert!(
        driver.queue.iter().all(|r| r.method != "turn.send"),
        "no auto-resend of the unconfirmed handoff on Tick"
    );
    driver.app.composer.set_text("new message");
    let new_send = driver
        .app
        .submit_composer()
        .into_iter()
        .filter_map(|c| match c {
            AppCommand::Rpc(r) => Some(r),
            _ => None,
        })
        .find(|r| r.method == "turn.send")
        .expect("deliberate new message still sends");
    assert_eq!(
        new_send.params["text"], "new message",
        "the deliberate new message is sent; the unconfirmed original is NOT"
    );
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(view.steer_queue.len(), 1, "unconfirmed original retained");
        assert_eq!(view.steer_queue[0].text, "second");
        assert_eq!(
            view.steer_queue[0].state,
            minicore_tui::state::turn::SteerQueueState::Unconfirmed
        );
    }
    // The unconfirmed item blocks the central queue advance even though the
    // deliberate admission released the pause gate.
    driver.app.update(AppEvent::Tick);
    assert!(
        driver.queue.iter().all(|r| r.method != "turn.send"),
        "unconfirmed item blocks the queue advance"
    );
}

#[test]
fn handoff_definite_reject_restores_unsent_paused_retained_once_no_composer_duplicate() {
    let mut driver = Driver::new();
    let handoff = handoff_setup(&mut driver);

    // A decoded Agent rejection is a DEFINITIVE failure: the message is
    // restored to the unsent queue, paused, retained exactly once, and the
    // editor is never duplicated.
    driver.respond_error(handoff, -32000, "rejected by agent");
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(view.steer_queue.len(), 1, "rejected handoff retained once");
        assert_eq!(
            view.steer_queue[0].state,
            minicore_tui::state::turn::SteerQueueState::Unsent
        );
        assert!(!view.steer_queue[0].handoff);
        assert!(view.steer_queue_paused);
    }
    assert!(
        driver.app.composer.content().is_empty(),
        "handoff reject must not copy its queued text into the editor"
    );

    // Deliberate withdraw (Alt+Up) is the ONLY way to retry.
    assert!(driver.app.retrieve_next_queued_steer());
    assert_eq!(driver.app.composer.content(), "second");
    assert!(
        driver.app.sessions.known["ses_1"].steer_queue.is_empty(),
        "withdrawals req đúngly empty the queue"
    );
}

#[test]
fn handoff_send_failure_before_write_restores_unsent_paused_and_never_auto_resends() {
    let mut driver = Driver::new();
    let handoff = handoff_setup(&mut driver);

    // The channel failed BEFORE anything reached the Agent: definitively not
    // sent, so the text is safely retained as Unsent+paused and the editor is
    // not duplicated.
    driver.step(AppEvent::RpcSendFailed {
        id: handoff.id,
        error: minicore_tui::rpc::RpcError::Closed,
    });
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(view.steer_queue.len(), 1, "unsent text retained");
        assert_eq!(
            view.steer_queue[0].state,
            minicore_tui::state::turn::SteerQueueState::Unsent
        );
        assert!(view.steer_queue_paused, "send failure pauses the queue");
    }
    assert!(
        driver.app.composer.content().is_empty(),
        "no composer duplicate for a handoff send failure"
    );
    driver.app.update(AppEvent::Tick);
    assert!(
        driver.queue.iter().all(|r| r.method != "turn.send"),
        "never auto-resend after a pre-write send failure"
    );
}

#[test]
fn handoff_turn_started_already_proves_accept_keeps_loop_and_drops_entry() {
    let mut driver = Driver::new();
    let handoff = handoff_setup(&mut driver);

    // The Agent already started the fresh loop (TurnStarted bound it): the
    // accept is PROVEN, so a later malformed send response must not abandon
    // the running loop nor retry the message.
    driver.step(agent_event(json!({
        "type": "turn_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_2"},
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(
            view.live
                .as_ref()
                .and_then(|l| l.reference.as_ref())
                .map(|r| r.loop_id.as_str()),
            Some("loop_2"),
            "the started loop is bound"
        );
    }
    driver.step(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: handoff.id,
            result: Some(json!({"bogus": true})),
            error: None,
        },
    ))));
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert!(
            view.steer_queue.is_empty(),
            "proven-accept handoff drops the queue entry (loop owns it)"
        );
        assert!(
            view.live.is_some(),
            "the running loop is preserved, never abandoned"
        );
    }
    let wait = driver.request("turn.wait");
    assert_eq!(wait.params["loop_id"], "loop_2");
    assert!(
        driver.app.composer.content().is_empty(),
        "no composer duplicate for a proven-accept handoff"
    );
}
