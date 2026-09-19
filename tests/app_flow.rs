//! Focused App reducer tests for the Agent v0.3 / TUI r2 contract.

use std::collections::VecDeque;
use std::path::PathBuf;

use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use ratatui::text::Line;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

use minicore_tui::app::{App, CliPrefs, ConnectionState, RequestKind, StartupSession};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{
    CompactStatusWire, IncomingFrame, OutgoingRequest, RpcNotification, RpcResponse,
    SessionStateWire, SessionStatusWire, TurnRef,
};
use minicore_tui::state::selection::{Dock, SessionPanelMode};
use minicore_tui::state::session::HistoryTrigger;
use minicore_tui::state::tool::ToolStatus;
use minicore_tui::state::turn::{PendingSteerState, SteerQueueItem, SteerQueueState, UnsavedLoop};
use minicore_tui::state::{AssistantPart, TranscriptBlock};
use minicore_tui::ui::{layout, panel};

/// A deleted session is absent from both the known views and the catalog
/// list; the catalog generation, not a tombstone set, keeps it deleted.
fn session_absent(app: &minicore_tui::app::App, session_id: &str) -> bool {
    !app.sessions.known.contains_key(session_id)
        && !app
            .sessions
            .list
            .iter()
            .any(|session| session.session_id == session_id)
}

struct Driver {
    app: App,
    queue: VecDeque<OutgoingRequest>,
    copies: Vec<String>,
    /// One receiver per owned export writer the reducer started. The harness
    /// runs the identical production job on a real thread, so the file it
    /// writes is the file the product writes.
    exports: Vec<(
        minicore_tui::jobs::ExportCapture,
        std::sync::mpsc::Receiver<minicore_tui::jobs::ExportOutcome>,
    )>,
    editors: Vec<(
        minicore_tui::jobs::EditorCapture,
        std::sync::mpsc::Receiver<minicore_tui::jobs::EditorOutcome>,
    )>,
    exited: bool,
}

impl Driver {
    fn with_app(app: App) -> Self {
        Self {
            app,
            queue: VecDeque::new(),
            copies: Vec::new(),
            exports: Vec::new(),
            editors: Vec::new(),
            exited: false,
        }
    }

    fn new() -> Self {
        Self {
            app: App::new(PathBuf::from("/workspace")),
            queue: VecDeque::new(),
            copies: Vec::new(),
            exports: Vec::new(),
            editors: Vec::new(),
            exited: false,
        }
    }

    fn start_editor(&mut self, request: minicore_tui::command::StartEditorRequest) {
        let (tx, rx) = std::sync::mpsc::channel();
        let capture = request.capture.clone();
        std::thread::spawn(move || {
            let outcome = minicore_tui::jobs::run_editor_job(
                &request.editor,
                &request.draft,
                &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
            let _ = tx.send(outcome);
        });
        self.editors.push((capture, rx));
    }

    fn start_export(&mut self, request: minicore_tui::command::StartExportRequest) {
        let (tx, rx) = std::sync::mpsc::channel();
        let capture = request.capture.clone();
        let cancel = request.cancel.clone();
        std::thread::spawn(move || {
            let outcome = minicore_tui::jobs::run_export_job(
                &request.target,
                request.overwrite,
                request.rx,
                cancel,
            );
            let _ = tx.send(outcome);
        });
        self.exports.push((capture, rx));
    }

    fn drain_editors(&mut self) -> bool {
        let mut finished = Vec::new();
        for (index, (_, rx)) in self.editors.iter().enumerate() {
            if let Ok(outcome) = rx.try_recv() {
                finished.push((index, outcome));
            }
        }
        let progressed = !finished.is_empty();
        for (index, outcome) in finished.into_iter().rev() {
            let (capture, _) = self.editors.remove(index);
            let more = self.app.update(AppEvent::JobFinished(
                minicore_tui::event::JobOutcome::Editor { capture, outcome },
            ));
            self.commands(more);
        }
        progressed
    }

    /// Feeds every finished export job back to the reducer. The harness is
    /// synchronous, so completion is polled instead of awaited.
    fn drain_exports(&mut self) -> bool {
        let mut finished = Vec::new();
        for (index, (_, rx)) in self.exports.iter().enumerate() {
            match rx.try_recv() {
                Ok(outcome) => finished.push((index, outcome)),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        let progressed = !finished.is_empty();
        for (index, outcome) in finished.into_iter().rev() {
            let (capture, _) = self.exports.remove(index);
            let more = self.app.update(AppEvent::JobFinished(
                minicore_tui::event::JobOutcome::Export { capture, outcome },
            ));
            self.commands(more);
        }
        progressed
    }

    fn commands(&mut self, commands: Vec<AppCommand>) {
        for command in commands {
            match command {
                AppCommand::Rpc(request) if request.method == "workspace.status" => {
                    self.respond(request,json!({"repo_available":false,"head_oid":null,"branch":null,"detached":false,"staged":0,"unstaged":0,"untracked":0,"conflicted":0,"entries":[],"skipped_paths":0,"complete":true,"warnings":[],"consistency":"live","observed_at_unix_ms":1}));
                }
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
                AppCommand::LocalScan(request) => {
                    // The owned worker runs the identical scan body; the
                    // reducer harness runs it inline so assertions are
                    // deterministic.
                    let outcome = minicore_tui::state::search::run_local_scan(&request);
                    let more = self
                        .app
                        .update(AppEvent::LocalScanFinished(Box::new(outcome)));
                    self.commands(more);
                }
                AppCommand::KillChild => {}
                AppCommand::CopySelection(text) => self.copies.push(text.as_str().to_owned()),
                AppCommand::StartExport(request) => self.start_export(*request),
                AppCommand::StartEditor(request) => self.start_editor(*request),
                AppCommand::PersistConfig(request) => {
                    let request = *request;
                    let result = minicore_tui::config::persist(&request.path, &request.config)
                        .map_err(|error| error.to_string());
                    let more = self.app.update(AppEvent::JobFinished(
                        minicore_tui::event::JobOutcome::Config {
                            path: request.path,
                            config: request.config,
                            result,
                        },
                    ));
                    self.commands(more);
                }
                AppCommand::Exit => self.exited = true,
            }
        }
    }

    fn step(&mut self, event: AppEvent) {
        for command in self.app.update(event) {
            match command {
                AppCommand::Rpc(request) if request.method == "workspace.status" => {
                    self.respond(request,json!({"repo_available":false,"head_oid":null,"branch":null,"detached":false,"staged":0,"unstaged":0,"untracked":0,"conflicted":0,"entries":[],"skipped_paths":0,"complete":true,"warnings":[],"consistency":"live","observed_at_unix_ms":1}));
                }
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
                AppCommand::LocalScan(request) => {
                    let outcome = minicore_tui::state::search::run_local_scan(&request);
                    let more = self
                        .app
                        .update(AppEvent::LocalScanFinished(Box::new(outcome)));
                    self.commands(more);
                }
                AppCommand::KillChild => {}
                AppCommand::CopySelection(text) => self.copies.push(text.as_str().to_owned()),
                AppCommand::StartExport(request) => self.start_export(*request),
                AppCommand::StartEditor(request) => self.start_editor(*request),
                AppCommand::PersistConfig(request) => {
                    let request = *request;
                    let result = minicore_tui::config::persist(&request.path, &request.config)
                        .map_err(|error| error.to_string());
                    let more = self.app.update(AppEvent::JobFinished(
                        minicore_tui::event::JobOutcome::Config {
                            path: request.path,
                            config: request.config,
                            result,
                        },
                    ));
                    self.commands(more);
                }
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

/// The read request a fresh, unpinned first-page chain carries. `reconcile`
/// is true when the chain exists to reconcile a known event gap.
fn history_read_request(gap_revision: u64) -> minicore_tui::app::ReadRequest {
    history_read_request_with(gap_revision, false)
}

fn history_read_request_with(gap_revision: u64, reconcile: bool) -> minicore_tui::app::ReadRequest {
    minicore_tui::app::ReadRequest {
        cursor: minicore_tui::protocol::ReadCursor::start(),
        pin: None,
        window_start: 0,
        replacement: true,
        reconcile,
        // A fresh window issues the §6.3 one-item probe first.
        probe: true,
        gap_revision,
    }
}

/// Builds a `session.read` result from already-encoded Runtime item JSON.
/// Each item's canonical JSON is delivered as one `utf8_json` chunk, which is
/// what the real backend does for a short item. `next_cursor` is the
/// backend's own cursor; tests that need to prove local-offset inference is
/// not used pass an explicit one.
fn history(items: Vec<Value>, next_cursor: Option<Value>, total: usize) -> Value {
    read(&items, next_cursor, total)
}

/// Encodes `items` (already Runtime `{item,timestamp}` envelopes) as a
/// Protocol v1 `session.read` page. Each envelope carries a private `_index`
/// used only by the test encoder to place the item at its session-global
/// index, mirroring the backend's own ordering.
fn read(items: &[Value], next_cursor: Option<Value>, total: usize) -> Value {
    let chunks: Vec<Value> = items.iter().flat_map(encode_item).collect();
    let mut page = json!({
        "session": session("ses_1"),
        "items": chunks,
        "total": total,
        "records": [],
        "records_truncated": false,
        "history_revision": "0000000000000000000000000000000000000000000000000000000000000000",
        "captured_end": total as u64,
        "trailing_incomplete": false,
    });
    if let Some(cursor) = next_cursor {
        page["next_cursor"] = cursor;
    }
    page
}

/// Serializes one Runtime envelope into contiguous chunks no larger than
/// `max` bytes, splitting only on a char boundary. This mirrors the backend's
/// canonical-JSON chunking so the client assembler is genuinely exercised.
fn encode_item(envelope: &Value) -> Vec<Value> {
    let index = envelope.get("_index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let item = envelope.get("item").cloned().unwrap_or(Value::Null);
    let mut wire = json!({"item": item});
    if let Some(timestamp) = envelope.get("timestamp") {
        wire["timestamp"] = timestamp.clone();
    }
    let data = serde_json::to_string(&wire).expect("item envelope serializes");
    let total = data.len();
    const MAX: usize = 64;
    if total <= MAX {
        return vec![json!({
            "index": index, "offset": 0, "total_bytes": total,
            "encoding": "utf8_json", "data": data, "complete": true,
        })];
    }
    let mut chunks = Vec::new();
    let mut offset = 0usize;
    while offset < total {
        let mut end = (offset + MAX).min(total);
        while !data.is_char_boundary(end) {
            end -= 1;
        }
        let complete = end == total;
        chunks.push(json!({
            "index": index, "offset": offset, "total_bytes": total,
            "encoding": "utf8_json", "data": &data[offset..end],
            "complete": complete,
        }));
        offset = end;
    }
    chunks
}

/// The first argument is the intended session-global item index; the test
/// encoder places the item there, so pagination assertions stay meaningful.
fn user(index: usize, loop_id: &str, text: &str) -> Value {
    json!({"_index": index, "item": {"type": "user", "data": {"loop_id": loop_id, "kind": "prompt", "input": {"text": text}}}})
}

fn user_steering(index: usize, loop_id: &str, text: &str) -> Value {
    json!({"_index": index, "item": {"type": "user", "data": {"loop_id": loop_id, "kind": "steering", "input": {"text": text}}}})
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
    let mut content = Vec::new();
    if !reasoning.is_empty() {
        content.push(json!({"type": "reasoning", "data": {"text": reasoning}}));
    }
    if !text.is_empty() {
        content.push(json!({"type": "text", "data": text}));
    }
    json!({"_index": index, "item": {"type": "assistant", "data": {
        "loop_id": loop_id, "request_index": request_index, "model": model,
        "reasoning": "high", "content": content,
        "usage": {}, "finish_reason": "stop"
    }}})
}

/// A Runtime `ToolResultHistory` answering the preceding assistant tool call.
fn tool_result(
    index: usize,
    loop_id: &str,
    request_index: u32,
    call_id: &str,
    tool_name: &str,
    outcome: &str,
    content: &str,
) -> Value {
    json!({"_index": index, "item": {"type": "tool_result", "data": {
        "loop_id": loop_id, "request_index": request_index, "call_id": call_id,
        "tool_name": tool_name, "outcome": outcome, "output": {"content": content}
    }}})
}

/// A Runtime assistant item carrying one tool call in `content`.
fn assistant_with_tool(
    index: usize,
    loop_id: &str,
    request_index: u32,
    call_id: &str,
    name: &str,
    text: &str,
) -> Value {
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "data": text}));
    }
    content.push(json!({"type": "tool_call", "data": {
        "tool_call_id": call_id, "name": name, "arguments": {}, "call_index": 0
    }}));
    json!({"_index": index, "item": {"type": "assistant", "data": {
        "loop_id": loop_id, "request_index": request_index, "model": "deep",
        "reasoning": "high", "content": content,
        "usage": {}, "finish_reason": "tool_calls"
    }}})
}

fn wait_result(session_id: &str, loop_id: &str, persistence: &str) -> Value {
    json!({
        "turn": {"session_id": session_id, "loop_id": loop_id},
        "outcome": {"type": "completed"}, "usage": {}, "requests": 1,
        "tool_rounds": 0, "final_config_revision": 0, "persistence": persistence
    })
}

fn context_result(id: &str, current_operation: Value, last_result: Value) -> Value {
    json!({
        "session_id": id,
        "current_operation": current_operation,
        "coverage": {
            "covered_loop_count": 0,
            "covered_item_count": 0,
            "retained_item_count": 0
        },
        "last_result": last_result,
        "budget": {},
        "automatic": {"current": null, "last": null},
        "last_prepare_failure": null
    })
}

fn wait_result_with_usage(
    session_id: &str,
    loop_id: &str,
    persistence: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> Value {
    json!({
        "turn": {"session_id": session_id, "loop_id": loop_id},
        "outcome": {"type": "completed"},
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0
        },
        "requests": 1, "tool_rounds": 0, "final_config_revision": 0,
        "persistence": persistence
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

fn rendered_text(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| minicore_tui::ui::render(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
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
    driver.respond_method(
        "agent.ping",
        json!({
            "version": "0.5.0",
            "protocol_version": 1,
            "capabilities": minicore_tui::protocol::REQUIRED_CAPABILITIES,
        }),
    );
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
    open_idle_with_history(driver, id, Vec::new());
}

fn open_idle_with_history(driver: &mut Driver, id: &str, items: Vec<Value>) {
    driver.step(AppEvent::OpenSession {
        session_id: id.to_owned(),
    });
    driver.respond_method("session.open", json!({"session": session(id)}));
    driver.respond_method("session.state", state(id, "idle", Value::Null));
    let total = items.len();
    let history_request = driver.request("session.read");
    driver.respond(history_request, history(items, None, total));
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

fn assert_startup_header(app: &App, expected: bool) {
    assert_eq!(
        rendered_text(app, 80, 24).contains("MINICORE  v0.2.8"),
        expected
    );
}

fn reload_stage_pending(app: &App) -> bool {
    app.pending_requests.values().any(|kind| {
        matches!(
            kind,
            RequestKind::Reload { .. }
                | RequestKind::ReloadModels { .. }
                | RequestKind::ReloadProfiles { .. }
                | RequestKind::ReloadSessions { .. }
        )
    })
}

fn assert_reload_staging_finished(driver: &Driver) {
    assert!(!reload_stage_pending(&driver.app));
    assert!(driver.app.catalogs.loaded);
    assert_eq!(driver.app.catalogs.models.len(), 1);
    assert_eq!(driver.app.catalogs.models[0].id, "deep");
    assert_eq!(driver.app.catalogs.profiles.len(), 1);
    assert_eq!(driver.app.catalogs.profiles[0].id, "coding");
    assert!(
        driver
            .app
            .notices()
            .iter()
            .any(|notice| notice.text == "Agent configuration and session metadata reloaded")
    );
    // Catalog-only reload (spec §9): no session state, presentation, or
    // history request may be pending or queued.
    assert!(
        driver.app.pending_requests.values().all(|kind| !matches!(
            kind,
            RequestKind::SessionState { .. }
                | RequestKind::SessionPresentation { .. }
                | RequestKind::History { .. }
        )),
        "reload must not hold a session view read"
    );
    assert!(
        driver.queue.iter().all(|request| !matches!(
            request.method,
            "session.state" | "session.presentation" | "session.read"
        )),
        "reload must not issue session view reads"
    );
}

fn start_public_reload_with_live_turn(driver: &mut Driver, loop_id: &str) -> OutgoingRequest {
    bootstrap(driver);
    open_idle(driver, "ses_1");
    let view = driver.app.sessions.known.get_mut("ses_1").unwrap();
    view.state.as_mut().unwrap().status = SessionStatusWire::Running;
    let mut live = minicore_tui::state::turn::LiveLoop::new(
        minicore_tui::state::turn::LocalSubmissionId(1),
        "live prompt".to_owned(),
    );
    live.reference = Some(TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: loop_id.to_owned(),
    });
    view.live = Some(live);

    submit_command(driver, "/reload");
    let reload = driver.request("agent.reload");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.wait"),
        "a catalog reload never enqueues a wait"
    );
    reload
}

fn complete_public_reload(driver: &mut Driver, reload: OutgoingRequest) {
    driver.respond(reload, json!({"ok": true}));
    driver.respond_method(
        "model.list",
        json!({"models": [{
            "id":"deep","model_ref":"provider/deep","context_window":128000,
            "supports_tools":true,"supported_reasoning":["auto","high"]
        }]}),
    );
    driver.respond_method(
        "profile.list",
        json!({"profiles": [{"id":"coding","model":"deep","reasoning":"high","tools":[]}]}),
    );
    driver.respond_method("session.list", json!({"sessions": [session("ses_1")]}));
    assert_reload_staging_finished(driver);
}

#[test]
fn new_session_and_empty_created_session_keep_startup_header() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle_with_history(
        &mut driver,
        "ses_1",
        vec![
            user(0, "loop_1", "previous prompt"),
            assistant(1, "loop_1", 0, "deep", "previous answer"),
        ],
    );

    let old_screen = rendered_text(&driver.app, 80, 24);
    assert!(!old_screen.contains("MINICORE  v0.2.8"));
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_1"));

    // `/new` now creates quickly with the current workspace and the recent
    // explicit configuration; the custom form moved to `/new form`.
    submit_command(&mut driver, "/new");
    assert!(driver.app.new_session().is_none());
    let quick = driver.request("session.create");
    assert_eq!(quick.params["workspace"], "/workspace");
    assert_eq!(quick.params["model"], "deep");
    driver.respond_error(quick, 1234, "quick create unavailable");

    submit_command(&mut driver, "/new form");
    let form_screen = rendered_text(&driver.app, 120, 40);
    for expected in [
        "MINICORE  v0.2.8",
        "Coding agent TUI",
        "Open a session — /new, Ctrl+R, or F1 for help",
        "New session",
        "previous answer",
    ] {
        assert!(
            form_screen.contains(expected),
            "new-session screen is missing {expected:?}:\n{form_screen}"
        );
    }

    let prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 79);
    assert_eq!(
        driver.app.sessions.known["ses_1"].transcript.blocks.len(),
        2
    );
    assert_eq!(
        prepared
            .lines()
            .iter()
            .filter(|line| line_text(line).contains("MINICORE"))
            .count(),
        1
    );
    assert!(
        prepared
            .copy_ranges
            .iter()
            .all(|range| !range.text.contains("MINICORE")
                && !range.text.contains("Coding agent TUI")),
        "startup header must remain outside model/history copy"
    );

    let small_screen = rendered_text(&driver.app, 60, 16);
    assert!(small_screen.contains("New session"));
    assert!(small_screen.contains("workspace"));

    for _ in 0..5 {
        driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::empty(),
        ))));
    }
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    ))));
    let create = driver.request("session.create");
    driver.respond(create, json!({"session": session("ses_2")}));
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_2"));
    assert!(
        !rendered_text(&driver.app, 80, 24).contains("MINICORE  v0.2.8"),
        "an empty history still loading must not look confirmed empty"
    );

    driver.respond_method("session.read", history(Vec::new(), None, 0));
    assert!(
        !rendered_text(&driver.app, 80, 24).contains("MINICORE  v0.2.8"),
        "empty history cannot confirm the header while session state is unknown"
    );
    driver.respond_method("session.state", state("ses_2", "idle", Value::Null));
    let empty_screen = rendered_text(&driver.app, 80, 24);
    for expected in [
        "MINICORE  v0.2.8",
        "Coding agent TUI",
        "Open a session — /new, Ctrl+R, or F1 for help",
    ] {
        assert!(
            empty_screen.contains(expected),
            "empty created-session screen is missing {expected:?}:\n{empty_screen}"
        );
    }
    let empty_prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 79);
    assert!(
        empty_prepared
            .copy_ranges
            .iter()
            .all(|range| !range.text.contains("MINICORE")
                && !range.text.contains("Coding agent TUI")),
        "empty-session startup header must remain outside copy payload"
    );
    assert_eq!(
        driver.app.sessions.known["ses_1"].transcript.blocks.len(),
        2,
        "creating a session must not alter the prior history"
    );

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_2".to_owned(),
        text: "pending prompt".to_owned(),
    });
    assert!(
        !rendered_text(&driver.app, 80, 24).contains("MINICORE  v0.2.8"),
        "a live prompt without output must not make the header flicker back"
    );
}

#[test]
fn confirmed_empty_header_requires_known_idle_and_clean_lifecycle() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    assert_startup_header(&driver.app, true);

    driver.app.sessions.known.get_mut("ses_1").unwrap().state = None;
    assert_startup_header(&driver.app, false);
    driver.app.sessions.known.get_mut("ses_1").unwrap().state = Some(
        serde_json::from_value::<SessionStateWire>(state("ses_1", "idle", Value::Null)).unwrap(),
    );

    for fence in [
        "event_gap",
        "history_reconcile",
        "history_post_wait",
        "unsaved_loop",
        "result_unknown",
    ] {
        let view = driver.app.sessions.known.get_mut("ses_1").unwrap();
        match fence {
            "event_gap" => view.event_gap = true,
            "history_reconcile" => view.history_read.begin(HistoryTrigger::Gap),
            "history_post_wait" => view.history_read.defer(HistoryTrigger::PostWait),
            "unsaved_loop" => {
                view.unsaved_loop = Some(UnsavedLoop {
                    turn: TurnRef {
                        session_id: "ses_1".to_owned(),
                        loop_id: "loop_unsaved".to_owned(),
                    },
                    user_text: "unfinished".to_owned(),
                    requests: Vec::new(),
                    result: None,
                    event_gap: false,
                });
            }
            "result_unknown" => {
                view.result_confirmation = minicore_tui::state::session::ResultConfirmation::Unknown
            }
            _ => unreachable!(),
        }
        assert_startup_header(&driver.app, false);
        let view = driver.app.sessions.known.get_mut("ses_1").unwrap();
        view.event_gap = false;
        view.history_read.reset();
        view.unsaved_loop = None;
        view.result_confirmation = minicore_tui::state::session::ResultConfirmation::Confirmed;
    }
    assert_startup_header(&driver.app, true);
}

#[test]
fn session_footer_new_invalidates_prepared_header_cache() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle_with_history(
        &mut driver,
        "ses_1",
        vec![user(0, "loop_1", "existing history")],
    );
    driver.step(AppEvent::OpenSessionSelector);
    driver.respond_method("session.list", json!({"sessions": [session("ses_1")] }));
    driver.step(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });

    let prepared = minicore_tui::ui::transcript::prepare_conversation(&driver.app, 79);
    driver.step(AppEvent::ConversationPrepared(prepared));
    assert!(driver.app.prepared_conversation(79).is_some());

    let (has_error, panel_area) = match &driver.app.dock {
        Dock::SessionSelector(state) => {
            let screen =
                layout::screen_layout(&driver.app, ratatui::layout::Rect::new(0, 0, 80, 24));
            (
                state.error.is_some(),
                panel::layout(
                    screen.panel,
                    panel::PanelSpec::new(u16::from(state.error.is_some()), true, 2),
                ),
            )
        }
        dock => panic!("unexpected dock: {dock:?}"),
    };
    // The footer's second row starts with `Ctrl+N New`; click its label.
    let column = panel_area.footer.x + 1;
    let row = panel_area.footer.y + 1;
    let mouse = |kind| {
        AppEvent::Terminal(CrosstermEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }))
    };
    assert!(!has_error);
    driver.step(mouse(crossterm::event::MouseEventKind::Down(
        crossterm::event::MouseButton::Left,
    )));
    driver.step(mouse(crossterm::event::MouseEventKind::Up(
        crossterm::event::MouseButton::Left,
    )));
    assert!(driver.app.new_session().is_some());
    assert!(driver.app.prepared_conversation(79).is_none());
    assert!(rendered_text(&driver.app, 120, 40).contains("MINICORE  v0.2.8"));
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
    let first = driver.request("session.read");
    driver.respond(
        first,
        history(
            vec![
                user(0, "loop_1", "hello"),
                assistant(1, "loop_1", 0, "deep", "answer"),
                tool_result(2, "loop_1", 0, "call", "read", "success", "ok"),
            ],
            Some(json!({"item": 3, "offset": 0})),
            4,
        ),
    );
    let second = driver.request("session.read");
    assert_eq!(second.params["cursor"]["item"], 3);
    driver.respond(
        second,
        history(vec![assistant(3, "loop_1", 1, "deep", "done")], None, 4),
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.transcript.loaded_count, 4);
    assert_eq!(view.transcript.window.len(), 4);
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
    let history_req = driver.request("session.read");
    driver.respond(
        history_req,
        history(vec![user(1, "loop_1", "out of order")], None, 2),
    );
    assert!(driver.app.notices().iter().any(|notice| {
        notice
            .text
            .contains("history for ses_1 is not contiguous at item 0")
    }));

    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_req = driver.request("session.read");
    driver.respond(
        history_req,
        history(Vec::new(), Some(json!({"item": 1, "offset": 0})), 1),
    );
    assert!(driver.app.notices().iter().any(|notice| {
        notice
            .text
            .contains("history for ses_1 did not advance from item 0")
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
        "session.read",
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
        "session.read",
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
    assert_eq!(live.requests[0].visible_text(), "still accepted");
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
    let _history = driver.request("session.read");
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
            .visible_text(),
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
    assert_eq!(
        view.live.as_ref().unwrap().requests[0].visible_text(),
        "event output"
    );
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
        "session.read",
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
    assert!(view.transcript.blocks.iter().any(|block| matches!(block.as_ref(), TranscriptBlock::Assistant(card) if card.parts == vec![AssistantPart::Text("durable answer".into())])));
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
    assert_eq!(live.requests[0].visible_text(), "already streaming");
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
    assert_eq!(live.requests[0].visible_text(), "first");
    assert_eq!(live.requests[1].visible_text(), "second");
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
    let history_request = driver.request("session.read");
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
    let reopened_history = driver.request("session.read");
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
    let history_request = driver.request("session.read");
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
        "session.read",
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
        Some(minicore_tui::protocol::TurnPersistenceWire::Persisted)
    );
}

#[test]
fn internal_refresh_turn_and_restricted_commands_remain_usable() {
    // The exact-turn wait path remains internal; there is no public slash
    // command for it.
    let mut no_session = Driver::new();
    no_session.step(AppEvent::RefreshTurn {
        session_id: "ses_1".into(),
    });
    assert!(no_session.queue.is_empty());
    assert!(no_session.app.composer.is_empty());

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

    // The internal event targets the retained blocked TurnRef exactly once.
    driver.step(AppEvent::RefreshTurn {
        session_id: "ses_1".into(),
    });
    let refresh = driver.request("turn.wait");
    assert_eq!(
        refresh.params,
        json!({"session_id": "ses_1", "loop_id": "loop_blocked"})
    );
    driver.step(AppEvent::RefreshTurn {
        session_id: "ses_1".into(),
    });
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
    finishing.step(AppEvent::RefreshTurn {
        session_id: "ses_1".into(),
    });
    let finishing_wait = finishing.request("turn.wait");
    assert_eq!(
        finishing_wait.params,
        json!({"session_id": "ses_1", "loop_id": "loop_finishing"})
    );
}

#[test]
fn slash_reload_starts_with_agent_reload_and_leaves_the_retained_turn_alone() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "retained failed turn".to_owned(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_reload_failed"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_reload_failed", "failed"));
    assert!(driver.app.sessions.known["ses_1"].unsaved_loop.is_some());

    // Public composer path, not AppEvent::RefreshTurn.
    submit_command(&mut driver, "/reload");
    let methods = driver
        .queue
        .iter()
        .map(|request| request.method)
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        vec!["agent.reload"],
        "a catalog reload issues agent.reload and nothing else"
    );
    let reload = driver.request("agent.reload");
    assert!(matches!(
        driver.app.pending_request_kind(reload.id),
        Some(RequestKind::Reload { .. })
    ));
    assert_eq!(
        driver.app.sessions.active.as_deref(),
        Some("ses_1"),
        "reload must not retarget the active session"
    );
    let after = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        after
            .last_result
            .as_ref()
            .map(|result| result.turn.loop_id.as_str()),
        Some("loop_reload_failed"),
        "the retained result is untouched"
    );
    assert!(after.unsaved_loop.is_some());
    assert!(
        driver.queue.is_empty(),
        "reload never enqueues a wait, send, steer, or lifecycle request"
    );
    assert!(driver.app.pending_requests.values().all(|kind| !matches!(
        kind,
        RequestKind::WaitTurn(_)
            | RequestKind::SendTurn { .. }
            | RequestKind::SteerTurn { .. }
            | RequestKind::CancelTurn(_)
    )));
}
#[test]
fn slash_reload_does_not_disturb_an_existing_turn_wait() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "wait already exists".to_owned(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_reload_wait"}}),
    );
    let existing_wait = driver.request("turn.wait");
    let turn = TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_reload_wait".to_owned(),
    };
    assert!(matches!(
        driver.app.pending_request_kind(existing_wait.id),
        Some(RequestKind::WaitTurn(pending)) if pending == &turn
    ));

    submit_command(&mut driver, "/reload");
    assert_eq!(
        driver
            .queue
            .iter()
            .map(|request| request.method)
            .collect::<Vec<_>>(),
        vec!["agent.reload"],
        "reload neither duplicates nor replaces the existing wait"
    );
    let reload = driver.request("agent.reload");

    // Re-entry while the first reload is outstanding adds nothing.
    submit_command(&mut driver, "/reload");
    assert!(
        driver.queue.is_empty(),
        "reload re-entry must not issue another reload or wait"
    );
    assert!(driver.app.request_is_pending(existing_wait.id));

    // The existing wait is deliberately answered after the reload request was
    // admitted. Its completion must not re-execute the wait.
    driver.respond(reload, json!({"ok": false}));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.wait"),
        "reload ACK/recovery must not duplicate the existing wait"
    );
    driver.respond(
        existing_wait,
        wait_result("ses_1", "loop_reload_wait", "persisted"),
    );
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.wait"),
        "the late wait ACK must not re-execute or duplicate turn.wait"
    );
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_1"));
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .last_result
            .as_ref()
            .map(|result| result.turn.loop_id.as_str()),
        Some("loop_reload_wait")
    );
}
#[test]
fn catalog_staging_does_not_touch_the_running_session_view() {
    let mut driver = Driver::new();
    let reload = start_public_reload_with_live_turn(&mut driver, "loop_reload_before");
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(
            view.state.as_ref().map(|state| state.status),
            Some(SessionStatusWire::Running)
        );
        assert_eq!(
            view.live
                .as_ref()
                .and_then(|live| live.reference.as_ref())
                .map(|turn| turn.loop_id.as_str()),
            Some("loop_reload_before")
        );
    }
    assert!(reload_stage_pending(&driver.app));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| !matches!(request.method, "turn.send" | "turn.steer" | "session.state"))
    );

    complete_public_reload(&mut driver, reload);
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.state.as_ref().map(|state| state.status),
        Some(SessionStatusWire::Running),
        "reload must not replace the live state projection"
    );
    assert_eq!(
        view.live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|turn| turn.loop_id.as_str()),
        Some("loop_reload_before")
    );
    assert_eq!(
        view.result_confirmation,
        minicore_tui::state::session::ResultConfirmation::Confirmed
    );
    assert!(driver.queue.iter().all(|request| !matches!(
        request.method,
        "turn.wait"
            | "turn.send"
            | "turn.steer"
            | "session.state"
            | "session.presentation"
            | "session.read"
    )));
}
#[test]
fn reload_staging_finishes_without_consuming_a_queued_steer() {
    let mut driver = Driver::new();
    let reload = start_public_reload_with_live_turn(&mut driver, "loop_reload_late");
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .steer_queue
        .push(SteerQueueItem {
            local_id: 1,
            text: "queued while reload waits".to_owned(),
            state: SteerQueueState::Unsent,
            editor_revision: None,
            handoff: false,
        });

    complete_public_reload(&mut driver, reload);
    assert_reload_staging_finished(&driver);
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(
        driver
            .queue
            .iter()
            .all(|request| !matches!(request.method, "turn.wait" | "turn.send" | "turn.steer"))
    );
}
#[test]
fn a_sealed_loop_steer_queue_is_not_advanced_by_a_catalog_reload() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "completed before reload".to_owned(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_reload_fifo"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond(wait, wait_result("ses_1", "loop_reload_fifo", "persisted"));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_request = driver.request("session.read");
    driver.respond(
        history_request,
        history(
            vec![
                user(0, "loop_reload_fifo", "completed before reload"),
                assistant(1, "loop_reload_fifo", 0, "deep", "done"),
            ],
            None,
            2,
        ),
    );
    assert!(driver.app.sessions.known["ses_1"].live.is_none());

    submit_command(&mut driver, "/reload");
    let reload = driver.request("agent.reload");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.wait"),
        "a catalog reload never refreshes a sealed turn"
    );
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .steer_queue
        .push(SteerQueueItem {
            local_id: 3,
            text: "queued fresh turn".to_owned(),
            state: SteerQueueState::Unsent,
            editor_revision: None,
            handoff: false,
        });

    complete_public_reload(&mut driver, reload);
    driver.step(AppEvent::Tick);
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(!view.steer_queue[0].handoff);
}
#[test]
fn a_reload_send_failure_does_not_retry_or_advance_the_fifo() {
    let mut driver = Driver::new();
    let reload = start_public_reload_with_live_turn(&mut driver, "loop_reload_send_failure");
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .steer_queue
        .push(SteerQueueItem {
            local_id: 2,
            text: "must remain queued".to_owned(),
            state: SteerQueueState::Unsent,
            editor_revision: None,
            handoff: false,
        });

    driver.step(AppEvent::RpcSendFailed {
        id: reload.id,
        error: minicore_tui::rpc::RpcError::Closed,
    });
    assert!(!driver.app.request_is_pending(reload.id));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "agent.reload"),
        "a reload send failure is never retried automatically"
    );
    assert_eq!(
        driver.app.sessions.known["ses_1"].steer_queue[0].state,
        SteerQueueState::Unsent
    );
    // The FIFO resumes only on the next ordinary event, and advances once.
    driver.step(AppEvent::Tick);
    assert_eq!(
        driver
            .queue
            .iter()
            .filter(|request| request.method == "turn.steer")
            .count(),
        1,
        "the queued steer advances exactly once after the reload failure"
    );
}
#[test]
fn slash_reload_without_retained_turn_enqueues_no_wait() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    submit_command(&mut driver, "/reload");
    assert_eq!(
        driver
            .queue
            .iter()
            .map(|request| request.method)
            .collect::<Vec<_>>(),
        vec!["agent.reload"]
    );
    let reload = driver.request("agent.reload");
    assert!(matches!(
        driver.app.pending_request_kind(reload.id),
        Some(RequestKind::Reload { .. })
    ));
    assert!(
        !driver
            .app
            .pending_requests
            .values()
            .any(|kind| matches!(kind, RequestKind::WaitTurn(_)))
    );
}
#[test]
fn a_wait_sent_before_a_reload_failure_stays_on_its_original_session() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "identity".to_owned(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_reload_identity"}}),
    );
    let wait = driver.request("turn.wait");

    submit_command(&mut driver, "/reload");
    let reload = driver.request("agent.reload");
    driver.respond(reload, json!({"ok": false}));
    assert!(!reload_stage_pending(&driver.app));
    assert!(
        driver.app.notices().iter().any(|notice| notice.text
            == "agent.reload returned {ok:false}; configuration was not applied")
    );

    driver.step(AppEvent::OpenSession {
        session_id: "ses_2".to_owned(),
    });
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_2")}));
    driver.respond_method("session.state", state("ses_2", "idle", Value::Null));
    let history_request = driver.request("session.read");
    driver.respond(history_request, history(Vec::new(), None, 0));
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_2"));

    driver.respond(
        wait,
        wait_result("ses_1", "loop_reload_identity", "persisted"),
    );
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_2"));
    assert!(driver.app.sessions.known["ses_2"].last_result.is_none());
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .last_result
            .as_ref()
            .map(|result| result.turn.loop_id.as_str()),
        Some("loop_reload_identity")
    );
    assert!(
        driver
            .queue
            .iter()
            .all(|request| !matches!(request.method, "turn.send" | "turn.steer"))
    );
}
#[test]
fn a_catalog_reload_leaves_no_stale_wait_for_a_settled_turn() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let t1_result = wait_result_with_usage("ses_1", "loop_t1", "persisted", 11, 5);
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "t1 prompt".to_owned(),
    });
    let t1_send = driver.request("turn.send");
    driver.respond(
        t1_send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_t1"}}),
    );
    let t1_wait = driver.request("turn.wait");
    driver.respond(t1_wait, t1_result.clone());
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let t1_history = driver.request("session.read");
    driver.respond(
        t1_history,
        history(
            vec![
                user(0, "loop_t1", "t1 prompt"),
                assistant(1, "loop_t1", 0, "deep", "t1 done"),
            ],
            None,
            2,
        ),
    );
    assert!(driver.app.sessions.known["ses_1"].live.is_none());

    submit_command(&mut driver, "/reload");
    let reload = driver.request("agent.reload");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.wait"),
        "a catalog reload never registers a wait"
    );
    complete_public_reload(&mut driver, reload);
    assert!(
        driver
            .app
            .pending_requests
            .values()
            .all(|kind| !matches!(kind, RequestKind::WaitTurn(_))),
        "a settled turn must not gain a reload-scoped wait"
    );

    submit_command(&mut driver, "t2 prompt");
    let t2_send = driver.request("turn.send");
    driver.respond(
        t2_send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_t2"}}),
    );
    let t2_wait = driver.request("turn.wait");
    let t2_result = wait_result_with_usage("ses_1", "loop_t2", "persisted", 22, 7);
    driver.respond(t2_wait, t2_result.clone());
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let t2_history = driver.request("session.read");
    driver.respond(
        t2_history,
        history(
            vec![
                user(0, "loop_t2", "t2 prompt"),
                assistant(1, "loop_t2", 0, "deep", "t2 done"),
            ],
            None,
            2,
        ),
    );

    let after = &driver.app.sessions.known["ses_1"];
    let t2_last_result = after.last_result.clone().expect("T2 result retained");
    let t2_usage = after.usage_projection.usage;
    let t2_loaded_count = after.transcript.loaded_count;
    assert_eq!(t2_last_result.turn.loop_id, "loop_t2");
    assert_eq!(
        t2_last_result.persistence,
        Some(minicore_tui::protocol::TurnPersistenceWire::Persisted)
    );
    assert_eq!(
        t2_last_result.usage.as_ref().unwrap().input_tokens,
        Some(22)
    );
    assert_eq!(
        t2_last_result.usage.as_ref().unwrap().output_tokens,
        Some(7)
    );
    assert_eq!(t2_loaded_count, 2);
    assert!(after.live.is_none());
    assert!(
        after
            .transcript
            .window
            .items()
            .any(|(_, item)| match item.as_ref() {
                TranscriptBlock::User(user) => user.loop_id.as_deref() == Some("loop_t2"),
                TranscriptBlock::Assistant(assistant) => assistant.loop_id == "loop_t2",
                TranscriptBlock::Tool(tool) => tool.loop_id == "loop_t2",
                _ => false,
            })
    );

    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_1"));
    assert_eq!(after.usage_projection.usage, t2_usage);
    assert!(
        driver.queue.is_empty(),
        "the settled T1 must not replay state/history RPCs"
    );
    assert!(driver.app.pending_requests.is_empty());
}
#[test]
fn a_stale_wait_send_failure_does_not_clear_a_sealed_loop_steer() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "t1 prompt".to_owned(),
    });
    let t1_send = driver.request("turn.send");
    driver.respond(
        t1_send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_t1_handoff"}}),
    );
    let t1_wait = driver.request("turn.wait");
    driver.respond(
        t1_wait,
        wait_result("ses_1", "loop_t1_handoff", "persisted"),
    );
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let history_request = driver.request("session.read");
    driver.respond(
        history_request,
        history(
            vec![
                user(0, "loop_t1_handoff", "t1 prompt"),
                assistant(1, "loop_t1_handoff", 0, "deep", "t1 done"),
            ],
            None,
            2,
        ),
    );
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .steer_queue
        .push(SteerQueueItem {
            local_id: 4,
            text: "T2 handoff".to_owned(),
            state: SteerQueueState::Unsent,
            editor_revision: None,
            handoff: false,
        });

    driver.step(AppEvent::Tick);
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );

    // A stale exact-turn wait (for a loop this view no longer targets) fails
    // at admission: it must not clear, hand off, or resend the queued steer.
    let stale_turn = TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: "loop_t_old".to_owned(),
    };
    let stale_request =
        OutgoingRequest::wait_turn(minicore_tui::protocol::RequestId(80_020), &stale_turn);
    driver.app.pending_requests.insert(
        stale_request.id,
        minicore_tui::app::RequestKind::WaitTurn(stale_turn.clone()),
    );
    driver.step(AppEvent::RpcSendFailed {
        id: stale_request.id,
        error: minicore_tui::rpc::RpcError::Closed,
    });
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].text, "T2 handoff");
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(!view.steer_queue[0].handoff);
    assert!(view.steer_queue_paused);
    driver.step(AppEvent::Tick);
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
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
    driver.respond_method(
        "session.read",
        history(
            vec![
                user(0, "loop_1", "start task"),
                assistant_with_tool(1, "loop_1", 0, "call_1", "read", ""),
                tool_result(2, "loop_1", 0, "call_1", "read", "success", "file contents"),
                assistant(3, "loop_1", 1, "fast", "done with fast model"),
            ],
            None,
            4,
        ),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    assert_eq!(view.transcript.window.len(), 4);
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
    driver.respond_method(
        "session.read",
        history(
            vec![
                user(0, "loop_1", "prompt"),
                user_steering(1, "loop_1", "retry"),
                user_steering(2, "loop_1", "retry"),
                assistant(3, "loop_1", 1, "deep", "finished"),
            ],
            None,
            4,
        ),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.live.is_none());
    assert_eq!(view.transcript.window.len(), 4);
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
        "session.read",
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

    // Open session: sends session.open, session.state, and initial session.read
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    // Leave the initial history request strictly in-flight!
    let inflight_history = driver.request("session.read");

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
        driver.queue.iter().all(|r| r.method != "session.read"),
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

    // Open session: requests session.open, session.state, and initial session.read
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    // Leave the initial history request in flight!
    let inflight_history = driver.request("session.read");

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

    let post_hist_req = driver.request("session.read");

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
        driver.queue.iter().all(|r| r.method != "session.read"),
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

    // failed wait MUST NOT automatically reconcile this loop via session.read
    assert!(
        driver.queue.iter().all(|r| r.method != "session.read"),
        "failed wait must not dispatch session.read to reconcile"
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
        driver.queue.iter().all(|r| r.method != "session.read"),
        "duplicate wait must not dispatch session.read"
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
    let history_req = driver.request("session.read");
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
    let history_req = recorded.request("session.read");
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
    let history_req = uncertain.request("session.read");
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
    let hist_req = driver.request("session.read");

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
        .filter(|b| matches!(b.as_ref(), TranscriptBlock::User(u) if u.kind == minicore_tui::protocol::UserMessageKindWire::Steering))
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
    let hist1 = driver.request("session.read");
    // Return page 1: offset 0, next_offset 1, total 2
    driver.respond(
        hist1,
        history(
            vec![user(0, "loop_1", "prompt 1")],
            Some(json!({"item": 1, "offset": 0})),
            2,
        ),
    );
    assert_eq!(
        driver.app.sessions.known["ses_1"].transcript.loaded_count,
        1
    );

    // Automated paging fetches next page starting at offset 1
    let hist_page2 = driver.request("session.read");
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
    let ses2_hist1 = driver.request("session.read");
    driver.respond(
        ses2_hist1,
        history(
            vec![user(0, "loop_x", "initial item")],
            Some(json!({"item": 1, "offset": 0})),
            2,
        ),
    );
    assert_eq!(
        driver.app.sessions.known["ses_2"].transcript.loaded_count,
        1
    );

    // Next page request arrives
    let ses2_hist2 = driver.request("session.read");
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
    if let Some(pos) = driver.queue.iter().position(|r| r.method == "session.read") {
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
    assert!(!session_absent(&driver.app, "ses_1"));
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
    let failed_history = driver.request("session.read");
    assert_eq!(
        driver.app.pending_requests.get(&failed_history.id),
        Some(&minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            read: history_read_request(0),
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
    assert!(!view.history_read.is_loading());

    // `/clear` starts a new history read but must not erase the safety fence.
    submit_command(&mut driver, "/clear");
    let clear_history = driver.request("session.read");
    assert_eq!(
        driver.app.pending_requests.get(&clear_history.id),
        Some(&minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            read: history_read_request_with(failed_revision, true),
        })
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.event_gap);
    assert!(!view.transcript.complete);
    assert!(view.history_read.is_loading());

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
    assert!(!view.history_read.is_loading());

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
    let history = reopen.request("session.read");
    assert_eq!(
        reopen.app.pending_requests.get(&history.id),
        Some(&minicore_tui::app::RequestKind::History {
            session_id: "ses_1".into(),
            read: history_read_request_with(reopen_revision, true),
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
    let initial_history = driver.request("session.read");
    assert_eq!(
        driver.app.pending_requests.get(&initial_history.id),
        Some(&RequestKind::History {
            session_id: "ses_1".into(),
            read: history_read_request(0),
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
    let retry = driver.request("session.read");
    assert_eq!(
        driver.app.pending_requests.get(&retry.id),
        Some(&RequestKind::History {
            session_id: "ses_1".into(),
            read: history_read_request_with(1, true),
        })
    );
    assert!(driver.app.sessions.known["ses_1"].event_gap);
    assert!(driver.app.sessions.known["ses_1"].history_read.is_loading());
    assert!(
        driver.app.sessions.known["ses_1"]
            .history_read
            .is_reconciling()
    );

    // Only the complete response for the new revision releases the fence.
    driver.respond(retry, history(Vec::new(), None, 0));
    let view = &driver.app.sessions.known["ses_1"];
    assert!(!view.event_gap);
    assert!(view.transcript.complete);
    assert!(!view.history_read.is_loading());
    assert!(!view.history_read.is_reconciling());

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
    assert!(session_absent(&driver.app, "ses_1"));
}

#[test]
fn stale_gap_revision_history_reply_cannot_release_a_new_event_gap() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let old_history = driver.request("session.read");

    // Model a request whose captured gap revision is already stale: its
    // response must remain unsafe after a newer gap.
    if let Some(RequestKind::History { read, .. }) =
        driver.app.pending_requests.get_mut(&old_history.id)
    {
        read.gap_revision = 0;
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
    let retry = driver.request("session.read");
    assert_eq!(
        driver.app.pending_requests.get(&retry.id),
        Some(&RequestKind::History {
            session_id: "ses_1".into(),
            read: history_read_request_with(1, true),
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
                read: history_read_request(0),
            },
        ),
    ] {
        driver.app.pending_requests.insert(request_id, kind);
    }
    driver.respond(delete, json!({"ok": true}));
    assert!(session_absent(&driver.app, "ses_1"));
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
            read: history_read_request(0),
        },
    );
    driver.respond(
        minicore_tui::protocol::OutgoingRequest::session_read(
            late_history_id,
            "ses_1",
            Some(minicore_tui::protocol::ReadCursor::start()),
            minicore_tui::protocol::READ_PAGE_LIMIT,
            minicore_tui::protocol::READ_PAGE_MAX_BYTES,
            None,
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

    assert!(session_absent(&driver.app, "ses_1"));
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
    let hist_req = driver.request("session.read");
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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

    // Crucial assertion: ZERO extra session.state or session.read requests emitted!
    assert!(
        driver.queue.is_empty(),
        "closed view must not emit extra session.state or session.read requests"
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
        "session.read",
        history(
            vec![
                user(0, "loop_failed_tool", "run the failing tool"),
                assistant_with_tool(1, "loop_failed_tool", 0, "call_failed", "bash", ""),
                tool_result(
                    2,
                    "loop_failed_tool",
                    0,
                    "call_failed",
                    "bash",
                    "failed",
                    "tool failed",
                ),
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
        .map(|range| range.text)
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
    let body_end = body_start + UnicodeWidthStr::width(body_copy.text).saturating_sub(1) as u16;
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
        Some(minicore_tui::protocol::TurnPersistenceWire::Failed)
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

    let view = driver.app.sessions.known.get("ses_1").unwrap();
    assert!(view.unsaved_loop.is_none());
    let last = view.last_result.as_ref().unwrap();
    assert!(matches!(
        last.outcome,
        minicore_tui::protocol::LoopOutcomeWire::Failed { .. }
    ));
    assert_eq!(
        last.persistence,
        Some(minicore_tui::protocol::TurnPersistenceWire::Persisted)
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
        "session.read",
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
        "session.read",
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
fn agent_exit_marks_the_outcome_unknown_without_overwriting_a_known_failure() {
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
    assert_eq!(
        view.result_confirmation,
        minicore_tui::state::session::ResultConfirmation::Unknown,
        "transport loss leaves no read-back source"
    );
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
    assert_eq!(
        view.result_confirmation,
        minicore_tui::state::session::ResultConfirmation::Confirmed,
        "a known persistence failure is not transport uncertainty"
    );
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
    unknown.step(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: "agent hung during shutdown".len(),
        dropped: 0,
    }));
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
    assert!(message.contains("agent stderr: 26 bytes"));

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
    known.step(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: "known failure stderr".len(),
        dropped: 0,
    }));
    known.app.update(AppEvent::ShutdownRequested);
    let message = known.app.shutdown_force_message();
    assert!(message.contains("known persistence failure retained"));
    assert!(message.contains("agent stderr: 20 bytes"));
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
    driver.respond_method("session.read", history(Vec::new(), None, 0));

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
/// FIFO-blocked unsent), then settles the session. B2 deliberately keeps the
/// second message bound to the sealed loop; it is not converted into a fresh
/// `turn.send` automatically.
fn handoff_setup(driver: &mut Driver) {
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
        "session.read",
        history(
            vec![
                user(0, "loop_1", "prompt"),
                user_steering(1, "loop_1", "first"),
                assistant(2, "loop_1", 1, "deep", "done"),
            ],
            None,
            3,
        ),
    );

    // Settled idle: the central advance must not submit "second" as a fresh
    // turn. The user must withdraw it into the editor deliberately.
    driver.app.update(AppEvent::Tick);
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(view.steer_queue_paused);
}

#[test]
fn sealed_loop_queue_is_not_resubmitted_after_tick_or_new_prompt() {
    let mut driver = Driver::new();
    handoff_setup(&mut driver);
    driver.app.update(AppEvent::Tick);
    assert!(driver.queue.iter().all(|r| r.method != "turn.send"));
    assert!(driver.app.composer.content().is_empty());
    assert_eq!(
        driver.app.sessions.known["ses_1"].steer_queue[0].text,
        "second"
    );
    assert!(driver.app.sessions.known["ses_1"].steer_queue_paused);
}

#[test]
fn sealed_loop_steer_can_only_be_withdrawn_deliberately() {
    let mut driver = Driver::new();
    handoff_setup(&mut driver);
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(view.steer_queue_paused);
    assert!(driver.app.composer.content().is_empty());

    assert!(driver.app.retrieve_next_queued_steer());
    assert_eq!(driver.app.composer.content(), "second");
    assert!(driver.app.sessions.known["ses_1"].steer_queue.is_empty());
}

#[test]
fn sealed_loop_steer_remains_unsent_without_a_send_failure_path() {
    let mut driver = Driver::new();
    handoff_setup(&mut driver);
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].state, SteerQueueState::Unsent);
    assert!(view.steer_queue_paused);
    assert!(driver.app.composer.content().is_empty());
    driver.app.update(AppEvent::Tick);
    assert!(driver.queue.iter().all(|r| r.method != "turn.send"));
}

#[test]
fn late_turn_started_does_not_consume_sealed_loop_steer() {
    let mut driver = Driver::new();
    handoff_setup(&mut driver);
    driver.step(agent_event(json!({
        "type": "turn_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_2"},
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.steer_queue.len(), 1);
    assert_eq!(view.steer_queue[0].text, "second");
    assert!(driver.queue.iter().all(|r| r.method != "turn.send"));
}

#[test]
fn preparation_cancel_waits_for_the_observed_operation_id() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "needs preparation".to_owned(),
    });
    let send = driver.request("turn.send");

    // Esc/cancel before context observation records intent but cannot guess an
    // operation id or cancel an unrelated compaction.
    driver.step(AppEvent::CancelTurn {
        session_id: "ses_1".to_owned(),
    });
    assert!(driver.queue.iter().all(|request| {
        request.method != "session.compact.cancel" && request.method != "turn.cancel"
    }));

    // An explicit context read uses the existing submission-owned poll and
    // therefore preserves its cancellation owner.
    submit_command(&mut driver, "/context");
    let context = driver.request("session.context");
    driver.respond(
        context,
        json!({
            "session_id": "ses_1",
            "current_operation": {
                "operation_id": "prep_exact",
                "phase": "preparing",
                "covered_item_count": 0,
                "retained_item_count": 0
            },
            "coverage": {"covered_loop_count": 0, "covered_item_count": 0, "retained_item_count": 0},
            "last_result": null,
            "budget": {},
            "automatic": {"current": null, "last": null},
            "last_prepare_failure": null
        }),
    );
    let cancel = driver.request("session.compact.cancel");
    assert_eq!(cancel.params["session_id"], "ses_1");
    assert_eq!(cancel.params["operation_id"], "prep_exact");
    assert!(matches!(
        driver.app.pending_request_kind(cancel.id),
        Some(RequestKind::CompactCancel { operation_id, .. }) if operation_id == "prep_exact"
    ));

    // The deferred send is still owned by the original submission and is not
    // resent while cancellation is being resolved.
    assert!(driver.app.request_is_pending(send.id));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );
}

/// REF-33 baseline fact: completed tool events update the existing card in
/// place by `tool_call_id` and never append a duplicate card or reorder the
/// request's cards. This must keep holding after the stage-C rework.
#[test]
fn manual_compact_noop_finishes_without_retrying_or_blocking() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    submit_command(&mut driver, "/compact");
    let compact = driver.request("session.compact");
    assert_eq!(compact.params["operation_id"], "tui-compact-0");
    let initial_context = driver.request("session.context");
    driver.respond(
        initial_context,
        context_result("ses_1", Value::Null, Value::Null),
    );
    driver.respond(
        compact,
        json!({"operation_id": "tui-compact-0", "status": "noop"}),
    );
    let refresh = driver.request("session.context");
    driver.respond(refresh, context_result("ses_1", Value::Null, Value::Null));

    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.manual_compact
            .as_ref()
            .and_then(|compact| compact.result.as_ref())
            .map(|result| result.status),
        Some(CompactStatusWire::Noop)
    );
    assert!(!view.is_preparing());
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.compact")
    );
}

#[test]
fn manual_compact_failed_preserves_history_without_retrying() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle_with_history(
        &mut driver,
        "ses_1",
        vec![user(0, "loop_a", "before compact")],
    );
    let blocks_before = driver.app.sessions.known["ses_1"].transcript.blocks.len();

    submit_command(&mut driver, "/compact");
    let compact = driver.request("session.compact");
    let initial_context = driver.request("session.context");
    driver.respond(
        initial_context,
        context_result("ses_1", Value::Null, Value::Null),
    );
    driver.respond(
        compact,
        json!({
            "operation_id": "tui-compact-0",
            "status": "failed",
            "failure_kind": "context_uncompressible"
        }),
    );
    let refresh = driver.request("session.context");
    driver.respond(refresh, context_result("ses_1", Value::Null, Value::Null));

    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.manual_compact
            .as_ref()
            .and_then(|compact| compact.result.as_ref())
            .map(|result| result.status),
        Some(CompactStatusWire::Failed)
    );
    assert_eq!(view.transcript.blocks.len(), blocks_before);
    assert!(!view.is_preparing());
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.compact")
    );
}

#[test]
fn manual_compact_unknown_write_requires_fresh_state_and_context() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    submit_command(&mut driver, "/compact");
    let compact = driver.request("session.compact");
    let initial_context = driver.request("session.context");
    driver.respond(
        initial_context,
        context_result("ses_1", Value::Null, Value::Null),
    );
    driver.respond(
        compact,
        json!({"operation_id": "tui-compact-0", "status": "unknown_write"}),
    );

    let state_refresh = driver.request("session.state");
    let context_refresh = driver.request("session.context");
    driver.respond(state_refresh, state("ses_1", "idle", Value::Null));
    assert!(driver.app.sessions.known["ses_1"].manual_compact.is_some());
    assert!(driver.app.sessions.known["ses_1"].is_preparing());

    driver.respond(
        context_refresh,
        context_result("ses_1", Value::Null, Value::Null),
    );
    assert!(driver.app.sessions.known["ses_1"].manual_compact.is_none());
    assert!(!driver.app.sessions.known["ses_1"].is_preparing());
}

#[test]
fn tool_completion_updates_in_place_without_duplicate_or_reorder() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "run two tools".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_tools"}}),
    );
    let turn = json!({"session_id": "ses_1", "loop_id": "loop_tools"});
    let started = |id: &str, name: &str| {
        agent_event(json!({
            "type": "tool_started",
            "data": {
                "turn": turn,
                "request_index": 0,
                "tool_call_id": id,
                "tool_name": name,
                "meta": {"session_id": "ses_1", "dropped_before": 0}
            }
        }))
    };
    driver.step(started("call_a", "read"));
    driver.step(started("call_b", "bash"));
    // Completion arrives for the *first* tool after both started. It must
    // update call_a in place; call_b stays running and order is unchanged.
    driver.step(agent_event(json!({
        "type": "tool_finished",
        "data": {
            "turn": turn,
            "request_index": 0,
            "tool_call_id": "call_a",
            "result": {"outcome": "success", "content_bytes": 12},
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));
    let tools = &driver.app.sessions.known["ses_1"]
        .live
        .as_ref()
        .unwrap()
        .requests[0]
        .tools;
    let ids: Vec<&str> = tools
        .iter()
        .map(|tool| tool.tool_call_id.as_str())
        .collect();
    assert_eq!(ids, vec!["call_a", "call_b"], "no duplicate and no reorder");
    assert_eq!(tools[0].status, ToolStatus::Succeeded);
    assert!(
        matches!(tools[1].status, ToolStatus::Pending | ToolStatus::Running),
        "the untouched second card must not be completed by the first tool's event"
    );
}

/// §6.3: a long session opens at `max(total-200, 0)`. The one-item probe
/// establishes the pin/total first; the tail item is then read from the
/// window start with that pin, never from a non-zero cursor without one.
#[test]
fn long_session_opens_at_the_tail_two_hundred_window() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));

    // Probe: total 500, so the window starts at item 300. Its own item 0 is
    // below the window and must be discarded, not shown.
    let probe = driver.request("session.read");
    assert_eq!(probe.params["limit"], 1, "first read is the §6.3 probe");
    assert!(probe.params.get("captured_end").is_none());
    driver.respond(
        probe,
        history(
            vec![user(0, "loop_0", "ancient")],
            Some(json!({"item": 1, "offset": 0})),
            500,
        ),
    );

    // The tail read must start at the window start and carry the new pin.
    let tail = driver.request("session.read");
    assert_eq!(tail.params["cursor"]["item"], 300);
    assert!(tail.params["captured_end"].is_number());
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(view.transcript.window.total(), 500);
        assert_eq!(
            view.transcript.window.len(),
            0,
            "probe item is out of window"
        );
    }
    driver.respond(
        tail,
        history(vec![user(300, "loop_300", "recent")], None, 500),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(view.transcript.window.total(), 500);
    assert!(view.transcript.window.item(300).is_some());
    assert!(
        view.transcript.window.item(0).is_none(),
        "an out-of-window item is never faked as loaded"
    );
}

/// §7.2: a lost/malformed `turn.wait` result triggers one authoritative
/// `turn.result` read-back. A `pending` report keeps the loop unconfirmed; a
/// `stored` persisted report settles it and reconciles history without
/// rerunning any tool.
#[test]
fn lost_wait_result_recovers_through_turn_result() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "say hello".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");

    // The wait response is lost: the client must not assume the turn never ran.
    driver.respond_error(wait, minicore_tui::protocol::INTERNAL_ERROR, "wait lost");
    let recover = driver.request("turn.result");
    assert_eq!(recover.params["turn"]["loop_id"], "loop_1");
    {
        let view = &driver.app.sessions.known["ses_1"];
        assert_eq!(
            view.result_confirmation,
            minicore_tui::state::session::ResultConfirmation::NeedsRead,
            "a lost wait needs a read-back for the exact turn"
        );
    }

    // `stored`: the turn really completed and was saved, so the authoritative
    // report settles the loop and reconciles history through the normal
    // post-wait path without rerunning any tool.
    driver.respond(
        recover,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "availability": "stored",
            "outcome": {"type": "completed"},
            "persistence": "persisted",
            "usage": {},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0,
            "completed_at": "2026-01-02T03:04:06Z",
            "items": [],
            "total": 0,
        }),
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.result_confirmation,
        minicore_tui::state::session::ResultConfirmation::Confirmed,
        "a stored persisted report settles the turn"
    );
    assert_eq!(
        view.last_result.as_ref().map(|r| r.turn.loop_id.as_str()),
        Some("loop_1")
    );
    assert!(driver.queue.iter().any(|r| r.method == "session.state"));
    assert!(driver.queue.iter().any(|r| r.method == "session.read"));
    assert!(
        driver.queue.iter().all(|r| r.method != "turn.send"),
        "recovery must never rerun the turn"
    );
}

/// A `pending` recovery report must NOT clear the unconfirmed fence: the turn
/// may still be running, so nothing is treated as saved.
#[test]
fn pending_turn_result_keeps_the_unconfirmed_fence() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "still running".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_1"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond_error(wait, minicore_tui::protocol::INTERNAL_ERROR, "wait lost");
    let recover = driver.request("turn.result");
    driver.respond(
        recover,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_1"},
            "availability": "pending",
            "items": [],
            "total": 0,
        }),
    );
    let view = &driver.app.sessions.known["ses_1"];
    assert_eq!(
        view.result_confirmation,
        minicore_tui::state::session::ResultConfirmation::NeedsRead,
        "a pending report keeps the read chain"
    );
    assert!(view.last_result.is_none(), "no result is fabricated");
    assert!(
        view.live
            .as_ref()
            .is_some_and(|live| live.waiting && live.last_result.is_none())
    );
    assert!(
        driver.queue.iter().all(|r| r.method != "turn.send"),
        "pending recovery must never rerun the turn"
    );
}

#[test]
fn failed_turn_result_projects_authoritative_body_when_live_deltas_are_missing() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "recover the saved body".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_saved_failed"}}),
    );
    let wait = driver.request("turn.wait");
    driver.respond_error(wait, minicore_tui::protocol::INTERNAL_ERROR, "wait lost");
    let recover = driver.request("turn.result");

    let items = [
        user(0, "loop_saved_failed", "recover the saved body"),
        assistant(
            1,
            "loop_saved_failed",
            0,
            "deep",
            "authoritative result body",
        ),
    ];
    let chunks: Vec<Value> = items.iter().flat_map(encode_item).collect();
    driver.respond(
        recover,
        json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_saved_failed"},
            "availability": "live",
            "outcome": {"type": "completed"},
            "persistence": "failed",
            "usage": {"input_tokens": 3, "output_tokens": 4},
            "requests": 1,
            "tool_rounds": 0,
            "final_config_revision": 0,
            "completed_at": "2026-01-02T03:04:06Z",
            "items": chunks,
            "next_cursor": null,
            "total": 2
        }),
    );

    let view = &driver.app.sessions.known["ses_1"];
    assert!(view.is_blocked());
    assert_eq!(
        view.result_confirmation,
        minicore_tui::state::session::ResultConfirmation::Confirmed,
        "the read reported the outcome; only the save is unconfirmed"
    );
    assert_eq!(
        view.last_result
            .as_ref()
            .and_then(|result| result.persistence),
        Some(minicore_tui::protocol::TurnPersistenceWire::Failed)
    );
    assert_eq!(
        view.live.as_ref().unwrap().requests[0].visible_text(),
        "authoritative result body"
    );
    assert_eq!(
        view.unsaved_loop.as_ref().unwrap().requests[0].visible_text(),
        "authoritative result body"
    );
    assert!(
        view.transcript
            .window
            .items()
            .all(|(_, item)| match item.as_ref() {
                TranscriptBlock::User(user) => user.loop_id.as_deref() != Some("loop_saved_failed"),
                TranscriptBlock::Assistant(assistant) => assistant.loop_id != "loop_saved_failed",
                TranscriptBlock::Tool(tool) => tool.loop_id != "loop_saved_failed",
                _ => true,
            }),
        "turn-local result items must not enter session history"
    );
}

/// D1 (spec §10.3): each session keeps its own whole composer. Switching
/// saves/restores text, cursor, undo and paste markers, and background
/// running loops are neither closed nor cancelled.
#[test]
fn switching_sessions_keeps_independent_composers_and_background_loops() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    // A loop is left running on ses_1 before switching away.
    driver.step(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "background work".into(),
    });
    let send = driver.request("turn.send");
    driver.respond(
        send,
        json!({"turn": {"session_id": "ses_1", "loop_id": "loop_bg"}}),
    );
    let _wait = driver.request("turn.wait");

    // A real draft: two paste projections and a non-default cursor.
    let pasted = (0..12)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    driver.app.composer.insert_paste(&pasted);
    driver.app.composer.type_text(" tail");
    driver.app.composer.move_to(0, 3);
    let first_revision = driver.app.composer.editor_revision();
    let first_bytes = driver.app.composer.byte_len();
    let first_pastes = driver.app.composer.display_paste_markers().len();
    assert!(first_pastes >= 1, "the paste projection is retained");

    // Draft independently in a second session.
    open_idle(&mut driver, "ses_2");
    assert!(
        driver.app.composer.content().is_empty(),
        "a new session starts with an empty draft"
    );
    driver.step(AppEvent::Terminal(CrosstermEvent::Paste("second".into())));
    driver.app.composer.move_to(0, 1);

    // Switching back restores the first session's whole composer.
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".into(),
    });
    assert_eq!(driver.app.composer.content(), format!("{pasted} tail"));
    assert_eq!(driver.app.composer.byte_len(), first_bytes);
    assert_eq!(driver.app.composer.cursor(), (0, 3));
    assert_eq!(
        driver.app.composer.display_paste_markers().len(),
        first_pastes,
        "paste markers follow the session"
    );
    assert!(driver.app.composer.editor_revision() >= first_revision);
    // Undo history follows the session too: typing then undoing restores the
    // exact pre-edit text.
    driver.app.composer.type_char('z');
    driver.app.composer.undo();
    assert_eq!(driver.app.composer.content(), format!("{pasted} tail"));

    // The background loop survived both switches untouched.
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|turn| turn.loop_id.as_str()),
        Some("loop_bg")
    );

    // The second session's draft is still its own.
    driver.step(AppEvent::OpenSession {
        session_id: "ses_2".into(),
    });
    assert_eq!(driver.app.composer.content(), "second");
}

/// D1 (spec §12.1, §21): the all-drafts budget counts undo/paste retention,
/// trims the oldest undo records first, and never deletes un-sent text.
#[test]
fn draft_budget_trims_undo_before_touching_text() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let text = "x".repeat(200 * 1024);
    driver.app.composer.set_text(&text);
    let capacity = driver.app.composer.undo_capacity();
    assert!(capacity > 1);
    let retained = driver.app.draft_bytes();
    assert!(retained > 0);
    // Force a tiny budget through the app-level pass.
    driver.app.enforce_draft_budget_with(1);
    assert_eq!(driver.app.composer.content().len(), text.len());
    assert_eq!(driver.app.composer.undo_capacity(), 1);
}

/// Runs a slash command from the composer directly (works whatever the dock
/// is, which the Enter key does not).
fn slash(driver: &mut Driver, command: &str) {
    driver.app.composer.set_text(command);
    let commands = driver.app.submit_composer();
    driver.commands(commands);
}

fn drive_ctrl(driver: &mut Driver, c: char) {
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL,
    ))));
}

/// Brows a closed catalog row and answer its history read.
fn pending_browse(driver: &mut Driver, id: &str) {
    panel_with_closed_session(driver, id);
    drive_ctrl(driver, 'b');
    let read = driver.request("session.read");
    driver.respond(
        read,
        history(vec![user(0, "loop_old", "browsed prompt")], None, 1),
    );
}

fn enter() -> AppEvent {
    AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::empty(),
    )))
}

/// Opens the session panel with one closed catalog row selected.
fn panel_with_closed_session(driver: &mut Driver, id: &str) {
    // A closed row whose model is not available locally. The workspace is
    // the app's own string; the app never validates that the path exists,
    // and browse must not need the model either (spec §10.1).
    let mut row = session(id);
    row["model"] = json!("model-that-does-not-exist");
    driver.step(AppEvent::OpenSessionSelector);
    driver.respond_method("session.list", json!({"sessions": [row]}));
}

/// D1 (spec §10.1): Ctrl+B reads a closed session through `session.read`
/// alone. No `session.open` leaves, so the row's workspace and model need not
/// exist locally.
#[test]
fn browsing_a_closed_session_reads_history_without_opening_it() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    panel_with_closed_session(&mut driver, "ses_closed");
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
    ))));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.open"),
        "browse must not open the session; queued: {:?}",
        driver
            .queue
            .iter()
            .map(|request| request.method)
            .collect::<Vec<_>>()
    );
    let read = driver.request("session.read");
    driver.respond(
        read,
        history(vec![user(0, "loop_old", "browsed prompt")], None, 1),
    );
    let view = &driver.app.sessions.known["ses_closed"];
    assert!(view.browsing, "the view stays read-only");
    assert_eq!(view.transcript.blocks.len(), 1);
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_closed"));
}

/// D1 (spec §10.1): Enter in a read-only view never opens and never sends.
/// The draft is kept verbatim, and the notice names the explicit continue
/// action.
#[test]
fn browsing_refuses_send_and_keeps_the_draft_until_an_explicit_continue() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    pending_browse(&mut driver, "ses_closed");
    driver.app.composer.set_text("continue please");
    driver.step(enter());
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send" && request.method != "session.open"),
        "Enter alone must not open or send"
    );
    assert_eq!(
        driver.app.composer.content(),
        "continue please",
        "the draft is kept exactly as typed"
    );
    assert!(driver.app.sessions.known["ses_closed"].browsing);

    // The explicit continue opens the session and keeps the draft.
    drive_ctrl(&mut driver, 'g');
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_closed")}));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send"),
        "the open ACK never sends the draft"
    );
    assert_eq!(driver.app.composer.content(), "continue please");
    assert!(!driver.app.sessions.known["ses_closed"].browsing);

    // The next normal Enter sends it.
    driver.step(enter());
    let send = driver.request("turn.send");
    assert_eq!(send.params["text"], "continue please");
}

/// D1 (spec §10.1): the Ctrl+G action is the composer-level explicit
/// continue; it behaves exactly like `/resume`.
#[test]
fn ctrl_g_continues_a_browsed_session_without_sending() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    pending_browse(&mut driver, "ses_closed");
    drive_ctrl(&mut driver, 'g');
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_closed")}));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );
    assert!(!driver.app.sessions.known["ses_closed"].browsing);
}

/// D1 (spec §10.1): `/resume` is the command form of the explicit continue;
/// it opens with an empty composer and never auto-sends.
#[test]
fn resume_command_opens_a_browsed_session_without_sending() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    pending_browse(&mut driver, "ses_closed");
    slash(&mut driver, "/resume");
    let open = driver.request("session.open");
    driver.respond(open, json!({"session": session("ses_closed")}));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send"),
        "the open ACK never sends"
    );
    assert!(!driver.app.sessions.known["ses_closed"].browsing);
}

/// D1 (spec §10.1): a failed continue returns to read-only browse with the
/// loaded history and the untouched draft.
#[test]
fn continuing_a_browsed_session_keeps_history_and_draft_when_open_fails() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    pending_browse(&mut driver, "ses_closed");
    driver.app.composer.set_text("keep me");
    drive_ctrl(&mut driver, 'g');
    let open = driver.request("session.open");
    driver.respond_error(open, 1234, "session_not_found");
    let view = &driver.app.sessions.known["ses_closed"];
    assert!(view.browsing, "a failed open returns to read-only browse");
    assert_eq!(view.transcript.blocks.len(), 1, "browsed history is kept");
    assert_eq!(driver.app.composer.content(), "keep me");
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "turn.send")
    );
}

/// D1 (spec §10.2): the selector lists only the current workspace by default
/// and every workspace only after the explicit scope toggle. The title makes
/// the active scope visible.
#[test]
fn session_selector_scope_defaults_to_current_workspace_with_an_explicit_all() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSessionSelector);
    let mut other = session("ses_other");
    other["workspace"] = json!("/other-project");
    other["updated_at"] = json!("2026-03-01T00:00:00Z");
    let mut here = session("ses_here");
    here["updated_at"] = json!("2026-02-01T00:00:00Z");
    driver.respond_method("session.list", json!({"sessions": [other, here]}));
    let selected = match &driver.app.dock {
        Dock::SessionSelector(state) => {
            assert_eq!(
                state.scope,
                minicore_tui::state::selection::SessionScope::CurrentWorkspace
            );
            state.selected_session_id.clone()
        }
        other => panic!("expected the session selector, got {other:?}"),
    };
    assert_eq!(selected.as_deref(), Some("ses_here"));
    let screen = rendered_text(&driver.app, 80, 24);
    assert!(
        screen.contains("this workspace"),
        "the scope label is visible: {screen}"
    );

    drive_ctrl(&mut driver, 'a');
    match &driver.app.dock {
        Dock::SessionSelector(state) => assert_eq!(
            state.scope,
            minicore_tui::state::selection::SessionScope::All
        ),
        other => panic!("expected the session selector, got {other:?}"),
    }
    assert_eq!(
        driver
            .app
            .session_panel_items("", minicore_tui::state::selection::SessionScope::All)
            .len(),
        2,
        "All scope lists every workspace"
    );
    let screen = rendered_text(&driver.app, 80, 24);
    assert!(
        screen.contains("all workspaces"),
        "the scope label follows the toggle: {screen}"
    );
}

/// D1 (spec §21): the all-drafts budget is an admission gate. At the budget
/// further typing is refused with one warning, and no existing draft is
/// truncated or dropped.
#[test]
fn draft_budget_refuses_new_input_and_keeps_existing_drafts() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let text = "x".repeat(200 * 1024);
    driver.app.composer_mut().set_text(&text);
    // Fill four sessions with near-limit drafts; their retained totals
    // (undo estimates included) exceed the 8 MiB all-drafts budget.
    for id in ["ses_2", "ses_3", "ses_4"] {
        open_idle(&mut driver, id);
    }
    // Non-active sessions keep their drafts in the view; the active one owns
    // the scratch composer.
    for id in ["ses_1", "ses_2", "ses_3"] {
        driver
            .app
            .sessions
            .known
            .get_mut(id)
            .expect("session view")
            .composer
            .set_text(&"y".repeat(250 * 1024));
    }
    driver.app.composer_mut().set_text(&"z".repeat(250 * 1024));
    assert!(
        driver.app.draft_bytes() > minicore_tui::limits::COMPOSER_ALL_DRAFTS_BYTES,
        "the fixture exceeds the all-drafts budget: {} <= {}",
        driver.app.draft_bytes(),
        minicore_tui::limits::COMPOSER_ALL_DRAFTS_BYTES
    );
    let before = driver.app.sessions.known["ses_1"].composer.content();

    assert!(!driver.app.admit_draft_input(1));
    assert_eq!(
        driver.app.sessions.known["ses_1"].composer.content(),
        before,
        "refused input never truncates the existing draft"
    );
    assert!(!driver.app.admit_draft_input(1));
    let warnings = driver
        .app
        .notices
        .iter()
        .filter(|notice| notice.text.contains("draft budget"))
        .count();
    assert!(warnings <= 1, "one warning per over-budget episode");
}
/// D1 (spec §6.1): `--session <id>` opens the exact id after bootstrap and
/// never opens the selector.
#[test]
fn startup_session_flag_opens_the_exact_id_without_a_selector() {
    let prefs = CliPrefs {
        startup_session: Some(StartupSession::Exact("ses_pinned".into())),
        ..CliPrefs::default()
    };
    let mut driver = Driver::with_app(App::with_cli_prefs(PathBuf::from("/workspace"), prefs));
    bootstrap(&mut driver);
    assert!(
        !matches!(driver.app.dock, Dock::SessionSelector(_)),
        "an exact id never shows the selector"
    );
    let open = driver.request("session.open");
    assert_eq!(open.params["session_id"], "ses_pinned");
    driver.respond(open, json!({"session": session("ses_pinned")}));
    driver.respond_method("session.state", state("ses_pinned", "idle", Value::Null));
    let read = driver.request("session.read");
    driver.respond(read, history(Vec::new(), None, 0));
    assert_eq!(driver.app.sessions.active.as_deref(), Some("ses_pinned"));
}

/// D1 (spec §6.1): `--continue` picks the most recent session whose workspace
/// is exactly the current one, ignoring a newer session in another project.
#[test]
fn startup_continue_matches_only_the_current_workspace() {
    let prefs = CliPrefs {
        startup_session: Some(StartupSession::ContinueCurrentWorkspace),
        ..CliPrefs::default()
    };
    let mut driver = Driver::with_app(App::with_cli_prefs(PathBuf::from("/workspace"), prefs));
    driver.step(AppEvent::Bootstrap);
    driver.respond_method(
        "agent.ping",
        json!({
            "version": "0.5.0",
            "protocol_version": 1,
            "capabilities": minicore_tui::protocol::REQUIRED_CAPABILITIES,
        }),
    );
    driver.respond_method(
        "model.list",
        json!({"models": [
            {"id":"deep","model_ref":"provider/deep","context_window":128000,"supports_tools":true,"supported_reasoning":["auto","high"]}
        ]}),
    );
    driver.respond_method(
        "profile.list",
        json!({"profiles": [{"id":"coding","model":"deep","reasoning":"high","tools":["read"]}]}),
    );
    let mut other = session("ses_other");
    other["workspace"] = json!("/other-project");
    other["updated_at"] = json!("2026-03-01T00:00:00Z");
    other["loaded"] = json!(false);
    let mut here = session("ses_here");
    here["updated_at"] = json!("2026-02-01T00:00:00Z");
    here["loaded"] = json!(false);
    driver.respond_method("session.list", json!({"sessions": [other, here]}));
    let open = driver.request("session.open");
    assert_eq!(
        open.params["session_id"], "ses_here",
        "the current workspace wins over a newer other-project session"
    );
}

/// D1 (spec §6.1): when `--continue` finds nothing in the current workspace
/// the selector opens instead of guessing across projects.
#[test]
fn startup_continue_falls_back_to_the_selector_without_cross_project_guessing() {
    let prefs = CliPrefs {
        startup_session: Some(StartupSession::ContinueCurrentWorkspace),
        ..CliPrefs::default()
    };
    let mut driver = Driver::with_app(App::with_cli_prefs(PathBuf::from("/workspace"), prefs));
    driver.step(AppEvent::Bootstrap);
    driver.respond_method(
        "agent.ping",
        json!({
            "version": "0.5.0",
            "protocol_version": 1,
            "capabilities": minicore_tui::protocol::REQUIRED_CAPABILITIES,
        }),
    );
    driver.respond_method(
        "model.list",
        json!({"models": [
            {"id":"deep","model_ref":"provider/deep","context_window":128000,"supports_tools":true,"supported_reasoning":["auto","high"]}
        ]}),
    );
    driver.respond_method(
        "profile.list",
        json!({"profiles": [{"id":"coding","model":"deep","reasoning":"high","tools":["read"]}]}),
    );
    let mut other = session("ses_other");
    other["workspace"] = json!("/other-project");
    other["updated_at"] = json!("2026-03-01T00:00:00Z");
    other["loaded"] = json!(false);
    driver.respond_method("session.list", json!({"sessions": [other]}));
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.open"),
        "no cross-project guess is opened"
    );
    assert!(
        matches!(driver.app.dock, Dock::SessionSelector(_)),
        "the selector takes over"
    );
}

/// D1 (spec §10.2): the session selector defaults to the current workspace's
/// recent activity while other workspaces stay listed below it.
#[test]
fn session_selector_defaults_to_current_workspace_recent_activity() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSessionSelector);
    let mut other = session("ses_other");
    other["workspace"] = json!("/other-project");
    other["updated_at"] = json!("2026-03-01T00:00:00Z");
    let mut here = session("ses_here");
    here["updated_at"] = json!("2026-02-01T00:00:00Z");
    driver.respond_method("session.list", json!({"sessions": [other, here]}));
    let selected = match &driver.app.dock {
        Dock::SessionSelector(state) => state.selected_session_id.clone(),
        other => panic!("expected the session selector, got {other:?}"),
    };
    assert_eq!(selected.as_deref(), Some("ses_here"));
}

/// D1c (spec §10.4): `/new` creates directly in the current workspace with
/// the recent explicit configuration; the custom form stays on `/new form`.
#[test]
fn new_command_creates_quickly_and_new_form_opens_the_custom_form() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    slash(&mut driver, "/new");
    assert!(
        driver.app.new_session().is_none(),
        "the quick path never opens the catalog form"
    );
    let create = driver.request("session.create");
    assert_eq!(create.params["workspace"], "/workspace");
    assert_eq!(create.params["profile"], "coding");
    assert_eq!(create.params["model"], "deep");
    assert_eq!(create.params["reasoning"], "high");

    slash(&mut driver, "/new form");
    assert!(
        driver.app.new_session().is_some(),
        "/new form still reaches the custom form"
    );
}

/// D1c (spec §10.4): `/rename <title>` takes the safe mutation path; a failed
/// ACK neither rewrites local metadata nor duplicates the request.
#[test]
fn rename_command_failure_keeps_metadata_and_does_not_duplicate() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    let before = driver.app.sessions.known["ses_1"].info.title.clone();
    slash(&mut driver, "/rename fresh title");
    let rename = driver.request("session.rename");
    assert_eq!(rename.params["title"], "fresh title");
    assert_eq!(rename.params["session_id"], "ses_1");
    driver.respond_error(rename, 1234, "rename unavailable");
    assert_eq!(
        driver.app.sessions.known["ses_1"].info.title, before,
        "a failed ACK never writes local metadata"
    );
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.rename"),
        "no duplicate rename is issued"
    );
}

/// D1c (spec §10.4): a bare `/rename` opens the dialog for the active
/// session (the typed path is covered separately).
#[test]
fn rename_command_opens_the_dialog_without_a_title() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    slash(&mut driver, "/rename");
    if let Some(position) = driver
        .queue
        .iter()
        .position(|request| request.method == "session.list")
    {
        let list = driver.queue.remove(position).unwrap();
        driver.respond(list, json!({"sessions": [session("ses_1")]}));
    }
    match &driver.app.dock {
        Dock::SessionSelector(state) => assert!(
            matches!(state.mode, SessionPanelMode::Rename { .. }),
            "the dock rename form is reachable"
        ),
        dock => panic!("expected the rename dialog, got {dock:?}"),
    }
}

/// D1c (spec §10.4): the typed `/rename` applies the ACK's authoritative
/// metadata (never a local blind rewrite).
#[test]
fn rename_command_applies_the_ack_metadata() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let mut titled = session("ses_1");
    titled["title"] = json!("fresh title");
    slash(&mut driver, "/rename fresh title");
    let rename = driver.request("session.rename");
    driver.respond(rename, json!({"session": titled}));
    assert_eq!(
        driver.app.sessions.known["ses_1"].info.title.as_deref(),
        Some("fresh title")
    );
}

/// D1c (spec §10.4): `/refresh` re-reads only this session's view data;
/// `/reload` stays the configuration path.
#[test]
fn refresh_reads_only_view_data_and_reload_stays_configuration() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let _ = driver.queue.drain(..).count();
    slash(&mut driver, "/refresh");
    let methods = driver
        .queue
        .iter()
        .map(|request| request.method)
        .collect::<Vec<_>>();
    // The presentation read coalesces with one already in flight; the
    // history reread is the view-data guarantee measured here.
    assert!(methods.contains(&"session.read"), "{methods:?}");
    for unrelated in ["agent.reload", "model.list", "profile.list", "session.list"] {
        assert!(
            !methods.contains(&unrelated),
            "/refresh must not issue {unrelated}: {methods:?}"
        );
    }

    let _ = driver.queue.drain(..).count();
    slash(&mut driver, "/reload");
    let methods = driver
        .queue
        .iter()
        .map(|request| request.method)
        .collect::<Vec<_>>();
    assert_eq!(methods, vec!["agent.reload"]);
}

/// D1c (spec §10.4, §23.3): `/clear` only re-reads the local view, `/close`
/// keeps receiving results, and `/delete` is closed-only with confirmation.
#[test]
fn clear_close_and_delete_keep_their_local_and_closed_only_contracts() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");
    let _ = driver.queue.drain(..).count();
    slash(&mut driver, "/clear");
    let written = driver
        .queue
        .iter()
        .map(|request| request.method)
        .collect::<Vec<_>>();
    assert!(
        written.iter().all(|method| *method == "session.read"),
        "/clear never writes to the Store: {written:?}"
    );
    let read = driver.request("session.read");
    driver.respond(read, history(Vec::new(), None, 0));

    // Close the running session through the command; the closed view stays.
    let wait = start_turn_and_close(&mut driver, "ses_1", "loop_1");

    // The retained wait still delivers the retired loop's result after close.
    driver.respond(wait, wait_result("ses_1", "loop_1", "persisted"));
    assert_eq!(
        driver.app.sessions.known["ses_1"]
            .last_result
            .as_ref()
            .map(|result| result.turn.loop_id.as_str()),
        Some("loop_1"),
        "results still land after close"
    );

    // A turn-free session closes cleanly, so the closed-only delete path is
    // reachable: the panel selection is the target and delete asks first.
    open_idle(&mut driver, "ses_2");
    slash(&mut driver, "/close confirm");
    let close2 = driver.request("session.close");
    driver.respond(close2, json!({"ok": true}));
    driver.step(AppEvent::OpenSessionSelector);
    driver.respond_method("session.list", json!({"sessions": [session("ses_2")]}));
    let selected = match &driver.app.dock {
        Dock::SessionSelector(state) => state.selected_session_id.clone(),
        dock => panic!("expected the session selector, got {dock:?}"),
    };
    assert_eq!(
        selected,
        Some("ses_2".to_owned()),
        "the closed session is selectable"
    );
    slash(&mut driver, "/delete");
    assert!(
        !driver
            .queue
            .iter()
            .any(|request| request.method == "session.delete"),
        "delete waits for the explicit confirm"
    );
    slash(&mut driver, "/delete confirm");
    let delete = driver.request("session.delete");
    assert_eq!(delete.params["session_id"], "ses_2");
}

/// D1c regression: every request the rename command registers is also
/// returned as a command. A registered-but-unsent request would keep the app
/// panel-busy forever (found by the real-Agent E2E).
#[test]
fn rename_command_never_leaks_an_unsent_pending_request() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_idle(&mut driver, "ses_1");

    for command in ["/rename leaked check", "/rename"] {
        let before = driver
            .app
            .pending_requests
            .keys()
            .map(|id| id.0)
            .collect::<Vec<_>>();
        driver.app.composer.set_text(command);
        let commands = driver.app.submit_composer();
        let sent = commands
            .iter()
            .filter_map(|command| match command {
                AppCommand::Rpc(request) => Some(request.id.0),
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in driver.app.pending_requests.keys() {
            if before.contains(&id.0) {
                continue;
            }
            assert!(
                sent.contains(&id.0),
                "{command} registered request {} without returning it",
                id.0
            );
        }
        // Clear the queue for the next iteration without driving the app.
        driver.queue.clear();
    }
}

// ---- D2: conversation search, navigation and jumps (spec §17.1/§17.2) ----

use minicore_tui::state::search::{SearchPanelMode, SearchScope, SearchSource, SearchStatus};

fn search_panel(app: &App) -> &minicore_tui::state::search::SearchPanelState {
    match &app.dock {
        Dock::Search(state) => state,
        other => panic!("expected the search panel, got {other:?}"),
    }
}

fn press(driver: &mut Driver, code: KeyCode) {
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::empty(),
    ))));
}

fn type_search_text(driver: &mut Driver, text: &str) {
    for character in text.chars() {
        press(driver, KeyCode::Char(character));
    }
}

fn open_chat_with(driver: &mut Driver, items: Vec<Value>) {
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".to_owned(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let total = items.len();
    let request = driver.request("session.read");
    driver.respond(request, history(items, None, total));
}

#[test]
fn search_loaded_scope_matches_body_thinking_and_tool_with_coverage() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "deploy the needle widget"),
            assistant_with_reasoning(
                1,
                "loop_1",
                0,
                "deep",
                "the needle body answer",
                "needle reasoning",
            ),
            tool_result(
                2,
                "loop_1",
                0,
                "call_1",
                "needle-read",
                "ok",
                "needle tool output",
            ),
        ],
    );

    slash(&mut driver, "/search needle");
    let panel = search_panel(&driver.app);
    assert_eq!(panel.scope, SearchScope::Loaded);
    assert_eq!(panel.matches.len(), 5, "{:?}", panel.matches);
    let sources: Vec<SearchSource> = panel.matches.iter().map(|m| m.source).collect();
    assert!(sources.contains(&SearchSource::Prompt));
    assert!(sources.contains(&SearchSource::AssistantText));
    assert!(sources.contains(&SearchSource::Thinking));
    assert!(sources.contains(&SearchSource::ToolName));
    assert!(sources.contains(&SearchSource::ToolResult));
    assert_eq!(panel.coverage.loaded_items, 3);
    assert_eq!(panel.coverage.total_items, 3);
    assert!(panel.coverage.complete);
    assert_eq!(panel.status, SearchStatus::Ready);
    let label = panel.status_label();
    assert!(label.contains("loaded content"), "{label}");
    assert!(label.contains("complete"), "{label}");

    // Esc leaves the search instead of cancelling the turn.
    press(&mut driver, KeyCode::Esc);
    assert!(matches!(
        search_panel(&driver.app).mode,
        SearchPanelMode::Input
    ));
    press(&mut driver, KeyCode::Esc);
    assert!(matches!(driver.app.dock, Dock::Composer));
    assert!(
        driver
            .app
            .pending_requests
            .values()
            .all(|kind| !matches!(kind, RequestKind::CancelTurn(_))),
        "Esc in the search must never cancel the running turn"
    );
}

#[test]
fn search_input_restarts_the_generation_and_a_late_scan_cannot_install() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "needle one"),
            user(1, "loop_2", "other two"),
        ],
    );
    slash(&mut driver, "/search needle");
    let first = search_panel(&driver.app);
    assert_eq!(first.matches.len(), 1);
    let stale_generation = first.generation;
    let stale_session = first.session_id.clone();

    // Edit the query and rerun: the new generation replaces the matches.
    // Ctrl+U clears the line, then typing edits it (any character switches
    // the panel back to its input mode).
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    ))));
    type_search_text(&mut driver, "other two");
    press(&mut driver, KeyCode::Enter);
    let panel = search_panel(&driver.app);
    assert!(panel.generation > stale_generation);
    assert_eq!(panel.matches.len(), 1);
    assert!(panel.matches[0].preview.contains("other two"));

    // A late completion from the old generation is ignored.
    let stale = minicore_tui::jobs::LocalScanOutcome {
        identity: minicore_tui::jobs::LocalScanIdentity {
            session_id: stale_session,
            session_epoch: 1,
            generation: stale_generation,
        },
        matches: vec![minicore_tui::state::search::SearchMatch {
            index: Some(0),
            source: SearchSource::Prompt,
            loop_id: Some("loop_1".to_owned()),
            request_index: None,
            ordinal: 0,
            tool_call_id: None,
            preview: "stale".to_owned(),
            source_offset: 0,
            byte_range: 0..6,
        }],
        truncated: false,
    };
    driver
        .app
        .update(AppEvent::LocalScanFinished(Box::new(stale)));
    let panel = search_panel(&driver.app);
    assert_eq!(panel.matches.len(), 1);
    assert!(panel.matches[0].preview.contains("other two"));
}

#[test]
fn search_caps_matches_and_reports_truncation_instead_of_guessing() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    let repeated = "needle ".repeat(700);
    open_chat_with(&mut driver, vec![user(0, "loop_1", &repeated)]);
    slash(&mut driver, "/search needle");
    let panel = search_panel(&driver.app);
    assert_eq!(
        panel.matches.len(),
        minicore_tui::state::search::MAX_SEARCH_MATCHES
    );
    assert!(panel.coverage.truncated);
    assert!(!panel.coverage.complete);
    let label = panel.status_label();
    assert!(label.contains("first 500 matches shown"), "{label}");
}

#[test]
fn full_session_search_scans_a_pinned_chain_and_never_claims_complete_on_large_items() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "loaded prompt"),
            assistant(1, "loop_1", 0, "deep", "loaded answer"),
            user(2, "loop_2", "another prompt"),
        ],
    );
    let session_pin = "0".repeat(64);
    slash(&mut driver, "/search full needle");
    let panel = search_panel(&driver.app);
    assert_eq!(panel.scope, SearchScope::FullSession);
    assert_eq!(panel.status, SearchStatus::ScanningFull);

    // The scan reuses the session's captured prefix and starts at item 0.
    let first = driver.request("session.read");
    assert_eq!(first.params["cursor"]["item"], 0);
    assert_eq!(
        first.params.get("captured_end").and_then(Value::as_u64),
        Some(3)
    );
    driver.respond(
        first,
        json!({
            "session": session("ses_1"),
            "items": encode_item(&user(0, "loop_1", "needle in the durable body")),
            "total": 3,
            "records": [],
            "records_truncated": true,
            "history_revision": session_pin,
            "captured_end": 3,
            "trailing_incomplete": false,
            "next_cursor": {"item": 1, "offset": 0},
        }),
    );
    // The item is decoded and scanned by the owned worker; the harness runs
    // the exact worker body inline and feeds the result back.
    let decode = driver.app.pending_decode_request().expect("decode queued");
    let scan = decode
        .scan
        .as_ref()
        .expect("a scan spec is carried")
        .clone();
    let item = minicore_tui::protocol::read::decode_item(&decode.item.data).expect("item decodes");
    let mut plan = minicore_tui::state::search::ScanPlan::new(&scan.needle, scan.include_thinking);
    plan.scan_item(0, &item);
    driver.app.mark_decode_scheduled();
    let decoded = minicore_tui::jobs::DecodeOutcome {
        identity: decode.identity.clone(),
        fingerprint: decode.fingerprint,
        result: Ok(item),
        cancelled: false,
        scan: Some(Box::new(minicore_tui::jobs::ScanItemOutcome {
            index: 0,
            matches: plan.collector.matches,
        })),
        export: None,
    };
    let more = driver
        .app
        .update(AppEvent::HistoryItemDecoded(Box::new(decoded)));
    driver.commands(more);
    let panel = search_panel(&driver.app);
    assert_eq!(panel.matches.len(), 1, "{:?}", panel.matches);
    assert!(panel.coverage.records_truncated);

    // The next page continues under the captured pin; its large item stops
    // the scan with explicit, incomplete coverage.
    let second = driver.request("session.read");
    assert_eq!(
        second.params.get("captured_end").and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        second
            .params
            .get("history_revision")
            .and_then(Value::as_str),
        Some(session_pin.as_str())
    );
    driver.respond(
        second,
        json!({
            "session": session("ses_1"),
            "items": [{
                "index": 1, "offset": 0, "total_bytes": 9_000_000,
                "encoding": "utf8_json", "data": "x", "complete": false,
            }],
            "total": 3,
            "records": [],
            "records_truncated": true,
            "history_revision": session_pin,
            "captured_end": 3,
            "trailing_incomplete": false,
        }),
    );
    let panel = search_panel(&driver.app);
    assert_eq!(panel.coverage.large_items, 1);
    assert!(!panel.coverage.complete);
    assert_ne!(panel.status, SearchStatus::ScanningFull);
    let label = panel.status_label();
    assert!(label.contains("large item"), "{label}");
    assert!(!label.ends_with("complete"), "{label}");
}

#[test]
fn full_session_search_continues_across_more_than_one_page() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    let loaded: Vec<Value> = (0..21)
        .map(|index| user(index, "loop_page", &format!("page item {index}")))
        .collect();
    open_chat_with(&mut driver, loaded.clone());

    slash(&mut driver, "/search full page item");
    let first = driver.request("session.read");
    assert_eq!(first.params["cursor"]["item"], 0);
    driver.respond(
        first,
        history(
            loaded[..20].to_vec(),
            Some(json!({"item": 20, "offset": 0})),
            21,
        ),
    );
    while let Some(decode) = driver.app.pending_decode_request() {
        let scan = decode.scan.as_ref().expect("full search scan request");
        let item = minicore_tui::protocol::read::decode_item(&decode.item.data)
            .expect("search item decodes");
        let mut plan =
            minicore_tui::state::search::ScanPlan::new(&scan.needle, scan.include_thinking);
        plan.scan_item(decode.item.index, &item);
        driver.app.mark_decode_scheduled();
        let more = driver.app.update(AppEvent::HistoryItemDecoded(Box::new(
            minicore_tui::jobs::DecodeOutcome {
                identity: decode.identity.clone(),
                fingerprint: decode.fingerprint,
                result: Ok(item),
                cancelled: false,
                scan: Some(Box::new(minicore_tui::jobs::ScanItemOutcome {
                    index: decode.item.index,
                    matches: plan.collector.matches,
                })),
                export: None,
            },
        )));
        driver.commands(more);
    }

    let second = driver.request("session.read");
    assert_eq!(second.params["cursor"]["item"], 20);
    driver.respond(second, history(vec![loaded[20].clone()], None, 21));
    while let Some(decode) = driver.app.pending_decode_request() {
        let scan = decode.scan.as_ref().expect("full search scan request");
        let item = minicore_tui::protocol::read::decode_item(&decode.item.data)
            .expect("search item decodes");
        let mut plan =
            minicore_tui::state::search::ScanPlan::new(&scan.needle, scan.include_thinking);
        plan.scan_item(decode.item.index, &item);
        driver.app.mark_decode_scheduled();
        let more = driver.app.update(AppEvent::HistoryItemDecoded(Box::new(
            minicore_tui::jobs::DecodeOutcome {
                identity: decode.identity.clone(),
                fingerprint: decode.fingerprint,
                result: Ok(item),
                cancelled: false,
                scan: Some(Box::new(minicore_tui::jobs::ScanItemOutcome {
                    index: decode.item.index,
                    matches: plan.collector.matches,
                })),
                export: None,
            },
        )));
        driver.commands(more);
    }

    let panel = search_panel(&driver.app);
    assert_eq!(panel.coverage.scanned_items, 21);
    assert!(panel.coverage.complete, "{panel:?}");
    assert_eq!(panel.matches.len(), 21);
}

#[test]
fn full_session_search_can_be_stopped_without_losing_found_matches() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "loaded prompt")]);
    slash(&mut driver, "/search full loaded");
    let request = driver.request("session.read");
    let scan_pin = "1".repeat(64);
    driver.respond(
        request,
        json!({
            "session": session("ses_1"),
            "items": encode_item(&user(0, "loop_1", "loaded prompt")),
            "total": 40,
            "records": [],
            "records_truncated": false,
            "history_revision": scan_pin,
            "captured_end": 40,
            "trailing_incomplete": false,
            "next_cursor": {"item": 1, "offset": 0},
        }),
    );
    // Stop while the next page request is queued.
    driver.app.composer.set_text("");
    press(&mut driver, KeyCode::Esc);
    press(&mut driver, KeyCode::Esc);
    assert!(matches!(driver.app.dock, Dock::Composer));
}

#[test]
fn search_jump_expands_a_fold_temporarily_and_restores_it_on_close() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    let reasoning = "needle reasoning line\nsecond\nthird\nfourth";
    open_chat_with(
        &mut driver,
        vec![assistant_with_reasoning(
            0, "loop_1", 0, "deep", "answer", reasoning,
        )],
    );
    let key = minicore_tui::state::view::ReasoningKey::new("loop_1", 0, 0);
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.reasoning_folds.get(&key))
            .copied(),
        None,
        "the user has no manual override yet"
    );

    slash(&mut driver, "/search needle");
    press(&mut driver, KeyCode::Enter);
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.reasoning_folds.get(&key))
            .copied(),
        Some(minicore_tui::state::view::FoldOverride::Expanded),
        "a jump expands the folded run it targets"
    );

    press(&mut driver, KeyCode::Esc);
    press(&mut driver, KeyCode::Esc);
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.reasoning_folds.get(&key))
            .copied(),
        None,
        "closing the search restores the user's own fold state"
    );
}

#[test]
fn prompt_jumps_skip_steering_and_read_an_unloaded_window() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "first prompt"),
            user_steering(1, "loop_1", "steer please"),
            user(2, "loop_2", "second prompt"),
            assistant(3, "loop_2", 0, "deep", "answer"),
        ],
    );

    slash(&mut driver, "/next");
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.scroll.anchor.as_ref())
            .and_then(|anchor| anchor.section_id.history_index),
        Some(0)
    );
    slash(&mut driver, "/next");
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.scroll.anchor.as_ref())
            .and_then(|anchor| anchor.section_id.history_index),
        Some(2),
        "/next skips the steering row"
    );
    slash(&mut driver, "/latest");
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.scroll.anchor.as_ref())
            .and_then(|anchor| anchor.section_id.history_index),
        Some(2)
    );
    slash(&mut driver, "/prev");
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.scroll.anchor.as_ref())
            .and_then(|anchor| anchor.section_id.history_index),
        Some(0)
    );
    // No extra read was needed: every prompt was already resident.
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.read")
    );

    // An unloaded older window is read at the exact item index with the pin.
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".to_owned(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let probe = driver.request("session.read");
    let pin_revision = "2".repeat(64);
    driver.respond(
        probe,
        read(
            &[user(0, "loop_0", "very old prompt")],
            Some(json!({"item": 1, "offset": 0})),
            202,
        ),
    );
    // 202 items open at the tail window [2, 202); index 0 stays unloaded.
    let mut tail_items = vec![user(2, "loop_2", "recent prompt")];
    tail_items.extend(
        (3..202).map(|index| user(index, &format!("loop_{index}"), &format!("filler {index}"))),
    );
    let tail = driver.request("session.read");
    assert_eq!(tail.params["cursor"]["item"], 2);
    driver.respond(tail, read(&tail_items, None, 202));
    assert!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .is_some_and(|view| view.transcript.window.item(0).is_none()),
        "index 0 is outside the opened tail window"
    );

    slash(&mut driver, "/next");
    slash(&mut driver, "/prev");
    let windowed = driver.request("session.read");
    assert_eq!(windowed.params["cursor"]["item"], 1);
    assert_eq!(
        windowed.params.get("captured_end").and_then(Value::as_u64),
        Some(202),
        "a windowed read beyond the loaded range keeps the captured pin"
    );
    let _ = pin_revision;
    driver.respond(
        windowed,
        read(
            &[user(1, "loop_1", "older prompt")],
            Some(json!({"item": 2, "offset": 0})),
            202,
        ),
    );
    assert_eq!(
        driver
            .app
            .sessions
            .known
            .get("ses_1")
            .and_then(|view| view.scroll.anchor.as_ref())
            .and_then(|anchor| anchor.section_id.history_index),
        Some(1),
        "the jump lands on the prompt the windowed read loaded"
    );
}

// ---- D2: /copy (spec §17.3) ----

fn set_terminal(driver: &mut Driver) {
    driver.step(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
}

#[test]
fn copy_last_reply_excludes_thinking_and_keeps_real_newlines() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    let long_line = "x".repeat(200);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "prompt one"),
            assistant_with_reasoning(
                1,
                "loop_1",
                0,
                "deep",
                &format!("first line\n\n{long_line}\n\nlast line"),
                "secret reasoning that must not be copied",
            ),
            user(2, "loop_2", "prompt two"),
            assistant(3, "loop_2", 0, "deep", "second reply body"),
        ],
    );
    // The viewport sits at the tail, as it does after a completed reply.
    driver.app.viewport = (40, 8);

    slash(&mut driver, "/copy");
    assert_eq!(driver.copies.len(), 1, "{:?}", driver.copies);
    let copied = &driver.copies[0];
    assert_eq!(copied, "second reply body");
    assert!(!copied.contains("secret reasoning"));

    // The last reply with its own body: hard newlines survive, soft-wrapped
    // rows do not gain one.
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "prompt one"),
            assistant_with_reasoning(
                1,
                "loop_1",
                0,
                "deep",
                &format!("first line\n\n{long_line}\n\nlast line"),
                "secret reasoning",
            ),
        ],
    );
    driver.app.viewport = (40, 8);
    slash(&mut driver, "/copy last");
    assert_eq!(driver.copies.len(), 1, "{:?}", driver.copies);
    let copied = &driver.copies[0];
    assert!(copied.contains("first line"), "{copied:?}");
    assert!(copied.ends_with("last line"), "{copied:?}");
    assert!(!copied.contains("secret reasoning"));
    assert_eq!(
        copied,
        &format!("first line\n\n{long_line}\n\nlast line"),
        "paragraph breaks survive and the soft-wrapped row joins directly"
    );
}

#[test]
fn copy_message_and_code_reuse_the_hit_operations_without_remote_reads() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "prompt one"),
            assistant(
                1,
                "loop_1",
                0,
                "deep",
                "intro text\n```rust\nlet value = 1;\nlet other = 2;\n```\noutro text",
            ),
        ],
    );
    driver.app.viewport = (40, 8);

    slash(&mut driver, "/copy message");
    assert_eq!(driver.copies.len(), 1, "{:?}", driver.copies);
    let message = &driver.copies[0];
    assert!(message.contains("intro text"), "{message:?}");
    assert!(message.contains("let value = 1;"), "{message:?}");
    assert!(message.contains("outro text"), "{message:?}");
    assert!(
        !message.contains("```"),
        "rendered copy never includes fence rows: {message:?}"
    );

    slash(&mut driver, "/copy code");
    assert_eq!(driver.copies.len(), 2, "{:?}", driver.copies);
    assert_eq!(driver.copies[1], "let value = 1;\nlet other = 2;");

    // A copy is local: nothing was sent for it, and no remote read happened.
    assert!(
        driver
            .queue
            .iter()
            .all(|request| request.method != "session.read"),
        "copy must never issue a read to complete a message"
    );
}

#[test]
fn copy_reports_unloaded_content_instead_of_copying_placeholder_text() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "loaded prompt")]);
    set_terminal(&mut driver);
    // A large item stays a visible placeholder; its body is not loaded. The
    // window owns that fact, so the copy path can name it.
    let view = driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .expect("session is open");
    view.transcript.push_block(
        minicore_tui::state::transcript::TranscriptBlock::HistoryPlaceholder(
            minicore_tui::state::transcript::HistoryPlaceholderBlock {
                index: 1,
                total_bytes: 9_000_000,
            },
        ),
    );
    view.transcript
        .window
        .insert_large_placeholder(1, 9_000_000, true);
    view.transcript.invalidate();
    driver.app.viewport = (40, 8);
    driver
        .app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .scroll
        .follow_tail = true;

    let before = driver.copies.len();
    slash(&mut driver, "/copy message");
    // Either the placeholder is picked (limitation) or the loaded prompt is
    // picked (its text is copied); a placeholder body is never copied.
    for copy in &driver.copies[before..] {
        assert!(
            !copy.contains("[large history item"),
            "placeholder text must never be copied: {copy:?}"
        );
    }

    // With nothing loaded at all the limitation is explicit.
    let mut empty = Driver::new();
    bootstrap(&mut empty);
    slash(&mut empty, "/copy");
    assert!(empty.copies.is_empty());
    assert!(
        empty
            .app
            .notices
            .iter()
            .any(|notice| notice.text.contains("no completed reply is loaded")),
        "{:?}",
        empty
            .app
            .notices
            .iter()
            .map(|notice| notice.text.clone())
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// Local export (spec §17.4)
// ============================================================================

/// A private scratch directory for one export test. The harness runs the real
/// writer, so these assertions cover the real temp-file/rename/cancel file
/// semantics, not a stub.
struct ExportDir(PathBuf);

impl ExportDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mctui-export-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn target(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Every file left in the scratch directory (a cancelled export must not
    /// leave its temp file behind).
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .expect("readable scratch dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for ExportDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One `session.read` page with a single durable item and an optional
/// continuation cursor.
fn export_page(index: usize, item: &Value, next: Option<usize>, total: usize) -> Value {
    let mut page = history(vec![item.clone()], None, total);
    page["records_truncated"] = json!(false);
    if let Some(next) = next {
        page["next_cursor"] = json!({"item": next, "offset": 0});
    }
    page["_index"] = json!(index);
    page
}

/// Answers the pending export read with one page and runs the exact worker
/// body for every queued item, mirroring `main.rs`'s decode hand-off.
fn export_advance(driver: &mut Driver, page: Value) {
    let request = driver.request("session.read");
    driver.respond(request, page);
    while let Some(decode) = driver.app.pending_decode_request() {
        let spec = decode
            .export
            .as_ref()
            .map(|spec| **spec)
            .expect("an export decode carries its spec");
        let item = minicore_tui::protocol::read::decode_item(&decode.item.data).expect("item");
        let rendered = minicore_tui::state::export::item_markdown(&item, spec);
        driver.app.mark_decode_scheduled();
        let decoded = minicore_tui::jobs::DecodeOutcome {
            identity: decode.identity.clone(),
            fingerprint: decode.fingerprint,
            result: Ok(item),
            cancelled: false,
            scan: None,
            export: Some(Box::new(minicore_tui::jobs::ExportItemOutcome {
                index: decode.item.index,
                markdown: rendered.markdown,
                opaque_parts: rendered.opaque_parts,
            })),
        };
        let more = driver
            .app
            .update(AppEvent::HistoryItemDecoded(Box::new(decoded)));
        driver.commands(more);
    }
}

/// Drives an in-flight export until the owned job stops, answering read pages
/// while it runs.
fn finish_export(driver: &mut Driver, pages: &mut VecDeque<Value>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        driver.drain_exports();
        // Settled means the chain stopped and the owned job reported back, so
        // the file on disk is the committed result (or a definite failure).
        let phase = driver.app.export_form().map(|form| form.phase);
        let settled = !driver.app.export_running()
            && driver.exports.is_empty()
            && phase != Some(minicore_tui::state::export::ExportPhase::Running);
        if settled {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the export did not finish: {:?}",
            driver.app.export_form().map(|form| form.notice.clone())
        );
        let has_read = driver
            .queue
            .iter()
            .any(|request| request.method == "session.read");
        if has_read {
            let page = pages.pop_front().expect("a page for every read");
            export_advance(driver, page);
        } else {
            // A parked record is retried by the next reducer pass. This also
            // models a slow writer freeing one bounded channel slot.
            driver.step(AppEvent::Tick);
            std::thread::yield_now();
        }
    }
}

#[test]
fn export_form_toggles_validate_and_write_nothing() {
    let dir = ExportDir::new("form");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "prompt one")]);

    slash(&mut driver, "/export");
    let form = driver.app.export_form().expect("form open").clone();
    assert_eq!(
        form.phase,
        minicore_tui::state::export::ExportPhase::Editing
    );
    // The defaults are the saved conversation only.
    assert!(!form.spec.include_thinking);
    assert!(!form.spec.include_tool);
    assert!(!form.include_unsaved);
    assert!(!form.overwrite);

    // The toggles are Ctrl chords, so a typed path can never change them.
    type_search_text(&mut driver, &dir.target("chat.md").display().to_string());
    drive_ctrl(&mut driver, 't');
    drive_ctrl(&mut driver, 'p');
    drive_ctrl(&mut driver, 'n');
    drive_ctrl(&mut driver, 'y');
    let form = driver.app.export_form().expect("form open");
    assert!(form.spec.include_thinking);
    assert!(form.spec.include_tool);
    assert!(form.include_unsaved);
    assert!(form.overwrite);

    // Esc closes without writing anything.
    press(&mut driver, KeyCode::Esc);
    assert!(driver.app.export_form().is_none());
    assert!(dir.entries().is_empty(), "{:?}", dir.entries());
}

#[test]
fn export_refuses_an_existing_target_until_overwrite_is_confirmed() {
    let dir = ExportDir::new("overwrite");
    let target = dir.target("chat.md");
    std::fs::write(&target, "old contents").expect("pre-existing target");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(
        &mut driver,
        vec![
            user(0, "loop_1", "hello"),
            assistant(1, "loop_1", 0, "deep", "a durable answer"),
        ],
    );

    slash(&mut driver, &format!("/export {}", target.display()));
    press(&mut driver, KeyCode::Enter);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        driver.drain_exports();
        if !driver.app.export_running() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "overwrite refusal");
        std::thread::yield_now();
    }
    // Nothing was written and the temp file is gone.
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old contents");
    assert_eq!(dir.entries(), vec!["chat.md".to_owned()]);
    let notice = driver
        .app
        .export_form()
        .and_then(|form| form.notice.clone())
        .unwrap_or_default();
    assert!(notice.contains("already exists"), "{notice}");

    // The refused attempt had already sent its first read: answering it
    // releases the read slot without touching the file.
    let stale = driver.request("session.read");
    driver.respond(
        stale,
        json!({
            "session": session("ses_1"),
            "items": [],
            "total": 0,
            "records": [],
            "records_truncated": false,
            "history_revision": "0".repeat(64),
            "captured_end": 0,
            "trailing_incomplete": false,
        }),
    );

    // The explicit confirmation enables the write.
    drive_ctrl(&mut driver, 'y');
    press(&mut driver, KeyCode::Enter);
    let mut pages = VecDeque::from(vec![export_page(0, &user(0, "loop_1", "hello"), None, 1)]);
    finish_export(&mut driver, &mut pages);
    assert!(!driver.app.export_running());
    let written = std::fs::read_to_string(&target).expect("exported file");
    assert!(written.contains("Conversation export"), "{written}");
    assert!(written.contains("source: saved history"), "{written}");
    assert!(written.contains("hello"), "{written}");
    assert!(!written.contains("old contents"));
    assert_eq!(
        dir.entries(),
        vec!["chat.md".to_owned()],
        "no temp file survives a finished export"
    );
}

#[test]
fn export_records_an_oversized_item_as_a_placeholder_and_reports_partial() {
    let dir = ExportDir::new("oversized");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export {}", target.display()));
    drive_ctrl(&mut driver, 'y');
    press(&mut driver, KeyCode::Enter);
    // One 9 MiB item: the export never assembles it.
    let mut pages = VecDeque::from(vec![
        export_page(0, &user(0, "loop_1", "hello"), Some(1), 2),
        json!({
            "session": session("ses_1"),
            "items": [{
                "index": 1, "offset": 0, "total_bytes": 9_000_000,
                "encoding": "utf8_json", "data": "x", "complete": false,
            }],
            "total": 2,
            "records": [],
            "records_truncated": false,
            "history_revision": "0".repeat(64),
            "captured_end": 2,
            "trailing_incomplete": false,
        }),
    ]);
    finish_export(&mut driver, &mut pages);
    let written = std::fs::read_to_string(&target).unwrap_or_else(|error| {
        panic!(
            "exported file: {error}; notice: {:?}",
            driver
                .app
                .export_form()
                .and_then(|form| form.notice.clone())
        )
    });
    assert!(
        written.contains("oversized history item: not exported"),
        "{written}"
    );
    assert!(written.contains("## Export notes"), "{written}");
    assert!(written.contains("partial:"), "{written}");
    let form = driver.app.export_form().expect("form stays open");
    assert!(form.limitations.is_partial());
    assert_eq!(form.limitations.oversized_items, 1);
}

#[test]
fn cancelling_an_export_removes_the_uncommitted_temp_file() {
    let dir = ExportDir::new("cancel");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export {}", target.display()));
    drive_ctrl(&mut driver, 'y');
    press(&mut driver, KeyCode::Enter);
    // The first page keeps the chain open, so the export is still running when
    // the user cancels.
    export_advance(
        &mut driver,
        export_page(0, &user(0, "loop_1", "hello"), Some(1), 2),
    );
    assert!(driver.app.export_running());
    press(&mut driver, KeyCode::Esc);
    assert!(driver.app.export_form().is_none());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !driver.drain_exports() {
        assert!(std::time::Instant::now() < deadline, "cancel completion");
        std::thread::yield_now();
    }
    assert!(!target.exists(), "a cancelled export commits nothing");
    assert!(dir.entries().is_empty(), "{:?}", dir.entries());
}

// ---- D2 review fixes: no-clobber race, typed cancel, busy owner, raw export ----

/// The parent-review race: a target created while the export is running is
/// never overwritten by the default commit. The form reports the typed
/// `TargetExists` outcome, the existing file is intact, and no temp remains.
#[test]
fn a_target_created_during_an_export_is_not_overwritten() {
    let dir = ExportDir::new("no-clobber-race");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export {}", target.display()));
    press(&mut driver, KeyCode::Enter);
    // The export is running with an uncommitted temp file. The target appears
    // now, after the writer's pre-flight check.
    let request = driver.request("session.read");
    std::fs::write(&target, "created during export").expect("interloper target");
    driver.respond(
        request,
        export_page(0, &user(0, "loop_1", "hello"), None, 1),
    );
    // Run the owned job to its typed completion.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        driver.drain_exports();
        if !driver.app.export_running() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "race export finish");
        std::thread::yield_now();
    }
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "created during export",
        "the default commit is a no-clobber commit"
    );
    let form = driver.app.export_form().expect("form open");
    assert_eq!(
        form.completion,
        Some(
            minicore_tui::state::export::ExportCompletion::TargetExists {
                target: target.display().to_string(),
            }
        )
    );
    assert!(
        form.notice
            .as_deref()
            .is_some_and(|notice| notice.contains("already exists")),
        "{:?}",
        form.notice
    );
    assert_eq!(
        dir.entries(),
        vec!["chat.md".to_owned()],
        "no temp file survives the refused commit"
    );
}

/// A cancel immediately says `Cancelling`, never "no file was written"; the
/// typed job outcome then reports the real state. The temp file is removed and
/// no partial or complete target appears. Both the open-form path and the
/// Esc-closed path are covered.
#[test]
fn cancelling_waits_for_the_typed_job_outcome() {
    let dir = ExportDir::new("cancel-typed");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export {}", target.display()));
    press(&mut driver, KeyCode::Enter);
    // One page is loaded, then the user cancels before the writer commits.
    export_advance(
        &mut driver,
        export_page(0, &user(0, "loop_1", "hello"), Some(1), 2),
    );
    assert!(driver.app.export_running());
    // A direct cancel keeps the form open and shows the honest intermediate
    // state instead of a premature "no file was written".
    driver.app.cancel_export();
    let form = driver.app.export_form().expect("form stays open");
    assert_eq!(
        form.phase,
        minicore_tui::state::export::ExportPhase::Cancelling,
        "a cancel is not reported as a local rollback"
    );
    assert!(
        form.notice
            .as_deref()
            .is_some_and(|notice| notice.contains("cancelling")),
        "{:?}",
        form.notice
    );
    assert!(
        !form
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("no file was written")),
        "the cancel must not claim a result before the job reports"
    );

    // Esc closes the form while the cancel is pending; the typed outcome is
    // then delivered as a notice.
    press(&mut driver, KeyCode::Esc);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !driver.drain_exports() {
        assert!(std::time::Instant::now() < deadline, "cancel outcome");
        std::thread::yield_now();
    }
    assert!(!target.exists(), "a cancelled export commits nothing");
    assert!(dir.entries().is_empty(), "{:?}", dir.entries());
    let notice = driver
        .app
        .notices()
        .iter()
        .map(|notice| notice.text.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(notice.contains("cancelled"), "{notice}");
    assert!(notice.contains("nothing was written"), "{notice}");
    assert!(
        !driver.app.export_running(),
        "the typed completion released the writer slot"
    );
}

/// The one writer slot is never overwritten by a second export: while the
/// previous completion has not been drained, a new submit is refused and the
/// old owner is kept, so no orphaned job can write a stale file.
#[test]
fn a_second_export_waits_for_the_owned_writer_slot() {
    let dir = ExportDir::new("busy-owner");
    let first_target = dir.target("first.md");
    let second_target = dir.target("second.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export {}", first_target.display()));
    press(&mut driver, KeyCode::Enter);
    assert!(driver.app.export_running());
    assert_eq!(
        driver.exports.len(),
        1,
        "exactly one owned writer is running"
    );

    // The form is closed while its job is still running; a new export is
    // refused until that job reports.
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))));
    slash(&mut driver, &format!("/export {}", second_target.display()));
    press(&mut driver, KeyCode::Enter);
    assert_eq!(
        driver.exports.len(),
        1,
        "the second export never overwrote the owner handle"
    );
    assert!(!second_target.exists());

    // Drain the first job: it commits only the first target.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while driver.app.export_owner_busy() {
        assert!(std::time::Instant::now() < deadline, "first export settles");
        driver.drain_exports();
        driver.step(AppEvent::Tick);
        std::thread::yield_now();
    }
    assert!(!driver.app.export_running(), "the first owner settled");
    assert!(
        !first_target.exists(),
        "cancelled first export commits nothing"
    );
}

#[test]
fn cancelling_export_keeps_input_responsive_while_writer_finishes() {
    let dir = ExportDir::new("input-during-cancel");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export {}", target.display()));
    press(&mut driver, KeyCode::Enter);
    assert!(driver.app.export_owner_busy());

    // Closing the form requests cancellation but keeps the owned writer alive
    // until its typed outcome. Input must still reach the Composer meanwhile.
    press(&mut driver, KeyCode::Esc);
    type_search_text(&mut driver, "draft while export cancels");
    assert_eq!(
        driver.app.composer().content(),
        "draft while export cancels"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while driver.app.export_owner_busy() {
        assert!(
            std::time::Instant::now() < deadline,
            "cancelled writer did not report"
        );
        driver.drain_exports();
        driver.step(AppEvent::Tick);
        std::thread::yield_now();
    }
    assert!(!target.exists());
    assert_eq!(
        driver.app.composer().content(),
        "draft while export cancels"
    );
}

#[cfg(unix)]
#[test]
fn external_editor_updates_only_the_current_draft_and_rejects_stale_return() {
    let dir = ExportDir::new("editor-reducer");
    let config_path = dir.target("config.toml");
    let editor = minicore_tui::config::EditorConfig {
        executable: "/bin/sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "sleep 0.05; printf 'edited 你好' > \"$1\"".to_owned(),
            "editor".to_owned(),
        ],
    };
    let config = minicore_tui::config::TuiConfig {
        editor: Some(editor),
        ..minicore_tui::config::TuiConfig::default()
    };
    let mut driver = Driver::with_app(App::with_tui_config(
        PathBuf::from("/workspace"),
        config_path,
        config,
    ));
    bootstrap(&mut driver);
    driver.app.composer_mut().set_text("old draft");
    slash(&mut driver, "/editor");
    assert!(driver.app.editor_active());
    driver.app.composer_mut().set_text("newer draft wins");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !driver.drain_editors() {
        assert!(
            std::time::Instant::now() < deadline,
            "editor did not return"
        );
        std::thread::yield_now();
    }
    assert_eq!(driver.app.composer().content(), "newer draft wins");
    assert!(!driver.app.editor_active());
}

#[test]
fn settings_apply_persists_atomically_without_reloading_agent() {
    let dir = ExportDir::new("settings");
    let config_path = dir.target("config.toml");
    let mut driver = Driver::with_app(App::with_tui_config(
        PathBuf::from("/workspace"),
        config_path.clone(),
        minicore_tui::config::TuiConfig::default(),
    ));
    bootstrap(&mut driver);
    slash(&mut driver, "/settings");
    press(&mut driver, KeyCode::Enter);
    driver.step(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL,
    ))));
    assert!(matches!(driver.app.dock, Dock::Composer));
    let saved = minicore_tui::config::load(&config_path, true).expect("saved settings");
    assert_eq!(saved.theme, minicore_tui::theme::ThemeKind::Light);
    assert!(!driver.app.agent_restart_required);
}

/// The explicit raw-export entry streams an oversized item's sanitized JSON
/// chunks verbatim, verifies byte count/offset/complete, and never typed
/// decodes it or raises the 8 MiB automatic ceiling. The written file carries
/// the raw item and a `raw:` note.
#[test]
fn raw_export_streams_oversized_item_bytes_without_decoding() {
    let dir = ExportDir::new("raw");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export raw {}", target.display()));
    let form = driver.app.export_form().expect("form open");
    assert!(form.spec.raw_oversized, "the raw entry is registered");
    press(&mut driver, KeyCode::Enter);

    // A canonical sanitized Runtime item strictly above the 8 MiB automatic
    // decode ceiling. Its bytes are streamed verbatim in three chunks; the
    // export never assembles or JSON-parses the whole body.
    let filler = "raw-body-line\n".repeat(900_000);
    let raw_body = serde_json::to_string(&json!({
        "item": {"type": "summary", "data": {"content": filler}},
        "timestamp": "2026-01-01T00:00:00Z",
    }))
    .unwrap();
    assert!(
        raw_body.len() > minicore_tui::protocol::read::MAX_AUTO_ITEM_BYTES,
        "the fixture must exceed the automatic ceiling: {}",
        raw_body.len()
    );
    let total = raw_body.len();
    let cut = total / 3;
    let cut = (cut..)
        .find(|index| raw_body.is_char_boundary(*index))
        .expect("a char boundary exists");
    let cut2 = (cut..total)
        .find(|index| raw_body.is_char_boundary(*index) && *index > cut)
        .unwrap_or(total);
    let raw_page = |offset: usize, end: usize| -> Value {
        json!({
            "session": session("ses_1"),
            "items": [{
                "index": 1, "offset": offset, "total_bytes": total,
                "encoding": "utf8_json", "data": &raw_body[offset..end],
                "complete": end == total,
            }],
            "total": 3,
            "records": [],
            "records_truncated": false,
            "history_revision": "0".repeat(64),
            "captured_end": 3,
            "trailing_incomplete": false,
            "next_cursor": if end == total { Some(json!({"item": 2, "offset": 0})) } else { Some(json!({"item": 1, "offset": end})) },
        })
    };
    // The discovery page surfaces the oversized item before its bytes are
    // exhausted; the assembler discards the prefix and the export restarts at
    // the item's own start for the explicit raw read.
    let discovery = json!({
        "session": session("ses_1"),
        "items": [{
            "index": 1, "offset": 0, "total_bytes": total,
            "encoding": "utf8_json", "data": &raw_body[..8], "complete": false,
        }],
        "total": 3,
        "records": [],
        "records_truncated": false,
        "history_revision": "0".repeat(64),
        "captured_end": 3,
        "trailing_incomplete": false,
    });
    let mut pages = VecDeque::from(vec![
        export_page(0, &user(0, "loop_1", "hello"), Some(1), 3),
        discovery,
        raw_page(0, cut),
        raw_page(cut, cut2),
        raw_page(cut2, total),
        export_page(2, &user(2, "loop_2", "after raw"), None, 3),
    ]);
    finish_export(&mut driver, &mut pages);
    let written = std::fs::read_to_string(&target).expect("raw export file");
    assert!(
        written.contains("Raw item 1"),
        "{}",
        &written[..400.min(written.len())]
    );
    assert!(
        written.contains("raw-body-line"),
        "the raw bytes are present"
    );
    assert!(
        written.contains("after raw"),
        "the chain continued past the raw item: tail={}",
        &written[written.len().saturating_sub(400)..]
    );
    assert!(
        written.contains("raw: 1 item(s)"),
        "the raw note is recorded"
    );
    let form = driver.app.export_form().expect("form stays open");
    assert_eq!(form.limitations.raw_items, 1);
    assert_eq!(form.limitations.oversized_items, 0);
    assert!(
        !form.limitations.is_partial(),
        "verified raw bytes are complete: {:?}",
        form.limitations
    );
}

/// The full-session scan reuses the strict pinned-chain rules (spec §17.1):
/// a page whose `total` disagrees with the captured prefix, or whose items are
/// non-contiguous, stops the scan honestly instead of mixing generations.
#[test]
fn full_search_stops_on_a_total_change_instead_of_splicing_generations() {
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    // The loaded window's total is the session total (40), as the backend
    // reports it; only the one newest item is resident.
    let items: Vec<Value> = (0..40)
        .map(|index| user(index, "loop_1", &format!("prompt {index}")))
        .collect();
    driver.step(AppEvent::OpenSession {
        session_id: "ses_1".to_owned(),
    });
    driver.respond_method("session.open", json!({"session": session("ses_1")}));
    driver.respond_method("session.state", state("ses_1", "idle", Value::Null));
    let request = driver.request("session.read");
    // A 40-item prefix delivered as one page with the backend's own cursor.
    let pin = "1".repeat(64);
    driver.respond(
        request,
        json!({
            "session": session("ses_1"),
            "items": encode_item(&items[0]),
            "total": 40,
            "records": [],
            "records_truncated": false,
            "history_revision": pin,
            "captured_end": 40,
            "trailing_incomplete": false,
            "next_cursor": {"item": 1, "offset": 0},
        }),
    );
    slash(&mut driver, "/search full needle");
    let first = driver.request("session.read");
    driver.respond(
        first,
        json!({
            "session": session("ses_1"),
            "items": encode_item(&user(0, "loop_1", "needle here")),
            "total": 40,
            "records": [],
            "records_truncated": false,
            "history_revision": pin,
            "captured_end": 40,
            "trailing_incomplete": false,
            "next_cursor": {"item": 1, "offset": 0},
        }),
    );
    while let Some(decode) = driver.app.pending_decode_request() {
        let item = minicore_tui::protocol::read::decode_item(&decode.item.data).expect("item");
        driver.app.mark_decode_scheduled();
        let decoded = minicore_tui::jobs::DecodeOutcome {
            identity: decode.identity.clone(),
            fingerprint: decode.fingerprint,
            result: Ok(item),
            cancelled: false,
            scan: Some(Box::new(minicore_tui::jobs::ScanItemOutcome {
                index: 0,
                matches: Vec::new(),
            })),
            export: None,
        };
        let more = driver
            .app
            .update(AppEvent::HistoryItemDecoded(Box::new(decoded)));
        driver.commands(more);
    }
    // The next page claims a different total for the same captured prefix: the
    // scan stops and never fabricates a complete scan across the two totals.
    let second = driver.request("session.read");
    driver.respond(
        second,
        json!({
            "session": session("ses_1"),
            "items": encode_item(&user(1, "loop_1", "needle two")),
            "total": 99,
            "records": [],
            "records_truncated": false,
            "history_revision": pin,
            "captured_end": 40,
            "trailing_incomplete": false,
        }),
    );
    let panel = search_panel(&driver.app);
    assert!(!panel.coverage.complete, "a changed total is not complete");
    assert!(panel.coverage.stopped);
    let label = panel.status_label();
    assert!(label.contains("stopped early"), "{label}");
    assert!(!label.contains("complete"), "{label}");
}

/// A raw item whose chunk byte/offset/complete data disagrees is never claimed
/// complete: the reducer records the mismatch and stops the chain instead of
/// resuming on bytes it could not verify. Byte verification itself is unit
/// tested in `state::export::tests`.
#[test]
fn raw_export_records_a_chunk_mismatch_instead_of_claiming_complete() {
    let dir = ExportDir::new("raw-mismatch");
    let target = dir.target("chat.md");
    let mut driver = Driver::new();
    bootstrap(&mut driver);
    set_terminal(&mut driver);
    open_chat_with(&mut driver, vec![user(0, "loop_1", "hello")]);

    slash(&mut driver, &format!("/export raw {}", target.display()));
    press(&mut driver, KeyCode::Enter);
    assert!(driver.app.export_running());

    // Page 0: a normal item. Page 1: the discovery page surfaces the oversized
    // item. Page 2: the raw continuation starts at the wrong offset.
    let mut pages = VecDeque::from(vec![
        export_page(0, &user(0, "loop_1", "hello"), Some(1), 2),
        json!({
            "session": session("ses_1"),
            "items": [{
                "index": 1, "offset": 0, "total_bytes": 9_000_000,
                "encoding": "utf8_json", "data": "x", "complete": false,
            }],
            "total": 2,
            "records": [],
            "records_truncated": false,
            "history_revision": "0".repeat(64),
            "captured_end": 2,
            "trailing_incomplete": false,
        }),
        json!({
            "session": session("ses_1"),
            "items": [{
                "index": 1, "offset": 5, "total_bytes": 9_000_000,
                "encoding": "utf8_json", "data": "yyyy", "complete": false,
            }],
            "total": 2,
            "records": [],
            "records_truncated": false,
            "history_revision": "0".repeat(64),
            "captured_end": 2,
            "trailing_incomplete": false,
        }),
    ]);
    // Drive the reducer with the same retry discipline `finish_export` uses,
    // but stop once the mismatch has been recorded.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while driver
        .app
        .export_form()
        .is_some_and(|form| form.limitations.raw_mismatched == 0)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "raw mismatch recorded"
        );
        if !driver
            .queue
            .iter()
            .any(|request| request.method == "session.read")
        {
            // The bounded writer may need one reducer pass to drain its
            // outbox before the next continuation is admitted.
            driver.step(AppEvent::Tick);
            std::thread::yield_now();
            continue;
        }
        let page = pages.pop_front().expect("a page for every request");
        export_advance(&mut driver, page);
        // The reducer's own update pass retries an admission that was
        // coalesced while a slot was busy, exactly as `main.rs` does.
        driver.step(AppEvent::Tick);
    }
    assert!(
        pages.is_empty(),
        "the mismatch settled before the last page"
    );
    let form = driver.app.export_form().expect("form stays open");
    assert_eq!(form.limitations.raw_mismatched, 1);
    assert!(form.limitations.is_partial());

    // The typed job outcome reports the committed (partial) file.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !driver.drain_exports() {
        assert!(std::time::Instant::now() < deadline, "raw mismatch settles");
        driver.step(AppEvent::Tick);
        std::thread::yield_now();
    }
    let written = std::fs::read_to_string(&target).expect("written file");
    assert!(
        written.contains("NOT complete"),
        "the mismatch is recorded: {}",
        &written[..600.min(written.len())]
    );
}
