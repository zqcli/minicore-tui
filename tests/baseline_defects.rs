//! Stage-A baseline tests that pin *current* v0.2.8 behavior so the stage B/C
//! migration can prove it removed the old defect instead of silently changing
//! user-visible behavior.
//!
//! These are deliberately written against the current (old) implementation
//! and must stay green on the frozen baseline. When stage B/C replaces a
//! path, migrate the assertion rather than deleting it (Spec §22.2). Names are
//! prefixed `baseline_` so a reviewer can diff them against the new tests.
//!
//! Each test states the defect it pins so the acceptance matrix can cite it.

use std::path::PathBuf;

use minicore_tui::app::{App, ConnectionState};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{IncomingFrame, OutgoingRequest, RpcResponse, SessionStatusWire};
use serde_json::json;

fn ready_app() -> App {
    // Leave `connection == Starting` so `Bootstrap` is admitted.
    App::new(PathBuf::from("/project"))
}

fn take_requests(commands: Vec<AppCommand>) -> Vec<OutgoingRequest> {
    commands
        .into_iter()
        .filter_map(|command| match command {
            AppCommand::Rpc(request) => Some(request),
            _ => None,
        })
        .collect()
}

fn respond(
    app: &mut App,
    request: &OutgoingRequest,
    result: serde_json::Value,
) -> Vec<OutgoingRequest> {
    take_requests(
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
        )))),
    )
}

/// The pinned Agent 0.5 handshake (protocol_version 1 + required
/// capabilities) must bootstrap successfully. Formerly a RED baseline pin of
/// the 0.3.x-only gate; stage B flipped it to the contract the new code must
/// satisfy (spec §4.1).
#[test]
fn bootstrap_accepts_the_pinned_agent_0_5_protocol_v1() {
    let mut app = ready_app();
    let requests = take_requests(app.update(AppEvent::Bootstrap));
    let ping = requests
        .iter()
        .find(|request| request.method == "agent.ping")
        .expect("bootstrap sends agent.ping");
    let capabilities: Vec<&str> = minicore_tui::protocol::REQUIRED_CAPABILITIES.to_vec();
    let _ = respond(
        &mut app,
        ping,
        json!({
            "version": "0.5.0",
            "protocol_version": 1,
            "capabilities": capabilities,
        }),
    );
    assert!(
        !matches!(app.connection, ConnectionState::Failed(_)),
        "protocol v1 with all required capabilities must pass the handshake"
    );
}

/// A backend that is missing a required capability is a definite
/// incompatibility: the app stops instead of falling back to Agent 0.3.
#[test]
fn bootstrap_rejects_a_backend_missing_required_capabilities() {
    let mut app = ready_app();
    let requests = take_requests(app.update(AppEvent::Bootstrap));
    let ping = requests
        .iter()
        .find(|request| request.method == "agent.ping")
        .expect("bootstrap sends agent.ping");
    let _ = respond(
        &mut app,
        ping,
        json!({"version": "0.5.0", "protocol_version": 1, "capabilities": ["session.read"]}),
    );
    match &app.connection {
        ConnectionState::Failed(message) => {
            assert!(message.contains("missing required capabilities"), "{message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// The handshake now consumes `protocol_version` and `capabilities`; the
/// legacy bare-version DTO no longer exists.
#[test]
fn ping_result_carries_protocol_version_and_capabilities() {
    let ping: minicore_tui::protocol::PingResult = serde_json::from_value(json!({
        "version": "0.5.0",
        "protocol_version": 1,
        "capabilities": ["session.read", "turn.result"],
    }))
    .expect("pinned ping result parses");
    assert_eq!(ping.version, "0.5.0");
    assert_eq!(ping.protocol_version, 1);
    assert_eq!(ping.capabilities.len(), 2);
    assert!(
        serde_json::from_value::<minicore_tui::protocol::PingResult>(json!({"version": "0.3.0"}))
            .is_err(),
        "a ping without protocol_version is a protocol error"
    );
}

/// Defect: the UI admission path awaits a fixed 64-slot channel
/// (`RpcProcess::send`), so a full queue blocks `App::update`'s caller. Stage
/// B adds a synchronous `try_send` with a 28+4 split.
#[test]
fn baseline_outbound_queue_is_64_and_send_awaits() {
    let source = include_str!("../src/rpc.rs");
    assert!(
        source.contains("const REQUESTS_CHANNEL_CAPACITY: usize = 64;"),
        "BASELINE: current outbound queue is 64 with no control reserve"
    );
    assert!(
        source.contains("pub async fn send("),
        "BASELINE: the UI path still uses the awaiting `send`"
    );
    assert!(
        !source.contains("pub fn try_send("),
        "BASELINE: no synchronous admission exists yet"
    );
}

/// Defect: only a per-frame 32 MiB bound exists; there is no aggregate
/// pending-wire budget. Stage B adds a 64 MiB token budget.
#[test]
fn baseline_has_no_inbound_wire_byte_budget() {
    let source = include_str!("../src/rpc.rs");
    assert!(
        source.contains("pub const MAX_RPC_FRAME_BYTES: usize = 32 * 1024 * 1024;"),
        "BASELINE: per-frame bound exists"
    );
    assert!(
        !source.contains("WIRE_BUDGET") && !source.contains("wire_bytes"),
        "BASELINE: no aggregate wire-byte budget token exists yet"
    );
}

/// Defect: the five-state projection has no `preparing`, the TUI never calls
/// `session.context`/`turn.result`/`session.compact`, and a deferred
/// `turn.send` is treated as immediate success. Stage B adds the visible
/// preparation/compaction path.
#[test]
fn baseline_has_no_preparing_state_or_control_methods() {
    let states = [
        SessionStatusWire::Idle,
        SessionStatusWire::Running,
        SessionStatusWire::WaitingForInput,
        SessionStatusWire::Finishing,
        SessionStatusWire::Blocked,
    ];
    assert_eq!(states.len(), 5, "BASELINE: no distinct preparing status");
    let source = include_str!("../src/protocol.rs");
    for absent in [
        "METHOD_SESSION_CONTEXT",
        "METHOD_TURN_RESULT",
        "METHOD_SESSION_COMPACT",
        "METHOD_SESSION_COMPACT_CANCEL",
        "METHOD_TOOL_READ",
        "METHOD_TOOL_OUTPUT",
        "METHOD_SESSION_READ",
        "METHOD_WORKSPACE_READ",
    ] {
        assert!(
            !source.contains(absent),
            "BASELINE: {absent} must not exist yet"
        );
    }
}

/// Defect: the history read path is the legacy `HistoryPageWire` display DTO;
/// there is no chunked raw Runtime-item decoder. Stage B adds a distinct
/// `ReadItemChunk`/assembler and never reuses the display DTO.
#[test]
fn baseline_history_read_uses_the_legacy_display_dto() {
    let source = include_str!("../src/protocol.rs");
    assert!(
        source.contains("pub struct IndexedHistoryItemWire"),
        "BASELINE: legacy indexed history DTO is the only read path"
    );
    assert!(
        !source.contains("ReadItemChunk") && !source.contains("ChunkAssembler"),
        "BASELINE: no chunked raw-item decoder exists yet"
    );
}

/// Defect: `all_lines` clones the full prepared conversation and
/// `build_durable_prepared` rescans every block to pair a tool call with its
/// result. Stage C replaces both with shared sections and a single index.
#[test]
fn baseline_prepare_clones_full_history_and_rescans_tools() {
    let source = include_str!("../src/ui/transcript.rs");
    assert!(
        source.contains("|prepared| prepared.lines.clone()"),
        "BASELINE: all_lines clones the full prepared conversation"
    );
    assert!(
        source.contains(".find_map(|block| match block {"),
        "BASELINE: durable preparation rescans every block per tool call"
    );
}

/// Defect: `run_commands` awaits `process.send().await` and the clipboard
/// synchronously in the main loop. Stage C moves both to owned jobs.
#[test]
fn baseline_run_commands_awaits_send_and_clipboard() {
    let source = include_str!("../src/main.rs");
    assert!(
        source.contains("let result = process.send(request.clone()).await;"),
        "BASELINE: the main loop awaits the RPC send"
    );
    assert!(
        source.contains("let result = clipboard.set_text(text.as_str());"),
        "BASELINE: the main loop runs the clipboard inline"
    );
    assert!(
        !source.contains("mod jobs") && !source.contains("use crate::jobs"),
        "BASELINE: no owned job module exists yet"
    );
}

/// Defect: an unconfirmed `turn.send` response has no authoritative
/// `turn.result` recovery; the App only latches `result_unconfirmed`. Stage B
/// adds `recover_turn`.
#[test]
fn baseline_has_no_turn_result_recovery() {
    let source = include_str!("../src/app.rs");
    assert!(
        source.contains("result_unconfirmed"),
        "BASELINE: current recovery is a boolean latch"
    );
    assert!(
        !source.contains("fn recover_turn"),
        "BASELINE: no turn.result recovery function exists yet"
    );
}

/// Defect: `agent.reload` stages a full-history replacement. Stage B/C
/// replaces it with a catalog/metadata refresh only.
#[test]
fn baseline_reload_stages_a_full_history_replacement() {
    let source = include_str!("../src/app.rs");
    assert!(
        source.contains("struct ReloadHistoryStage"),
        "BASELINE: reload carries a staged history replacement"
    );
    assert!(
        source.contains("fn apply_reload"),
        "BASELINE: apply_reload installs the staged history"
    );
}

/// Defect (REF-48): the markdown display boundary passes raw control
/// sequences straight through. Measured: an ESC/OSC-52+BEL payload survives
/// both `MarkdownRenderer::render` and `wrap_plain` into the final rows.
/// Stage E adds the unified safe-display boundary.
#[test]
fn baseline_control_sequences_reach_the_display_rows() {
    use minicore_tui::markdown::{MarkdownRenderer, wrap_plain};
    use minicore_tui::theme::Theme;
    let theme = Theme::dark();
    let renderer = MarkdownRenderer::new(&theme);
    let payload = "before \u{1b}]52;c;evil\u{7} after";
    let style = ratatui::style::Style::new();
    let joined = |lines: Vec<ratatui::text::Line<'static>>| -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    };
    let rendered = joined(renderer.render(payload, 80, style));
    let plain = joined(wrap_plain(payload, 80, style));
    assert!(
        rendered.contains('\u{1b}') && rendered.contains('\u{7}'),
        "BASELINE: markdown rendering leaks ESC/BEL control sequences"
    );
    assert!(
        plain.contains('\u{1b}') && plain.contains('\u{7}'),
        "BASELINE: plain wrapping leaks ESC/BEL control sequences"
    );
}

/// Defect (REF-49): the Agent's stderr is stored into `agent_logs` and shown
/// verbatim; there is no redaction boundary and no test asserting log content
/// is content-free. Only `--debug` logs method/id/byte-count/duration. Stage C
/// adds the redaction boundary and its tests.
#[test]
fn baseline_agent_stderr_is_logged_without_a_redaction_boundary() {
    let rpc = include_str!("../src/rpc.rs");
    assert!(
        rpc.contains("RpcEvent::AgentLogLine(agent_log_line(&line))"),
        "BASELINE: raw stderr lines become AgentLogLine events"
    );
    let main = include_str!("../src/main.rs");
    assert!(
        !main.contains("redact") && !main.contains("sanitize_log"),
        "BASELINE: no log redaction function exists yet"
    );
    let tests = [
        include_str!("../tests/agent_e2e.rs"),
        include_str!("../tests/app_flow.rs"),
        include_str!("../tests/protocol.rs"),
    ]
    .concat();
    assert!(
        !tests.contains("agent_logs_contain_no"),
        "BASELINE: no test asserts the debug log is content-free"
    );
}
