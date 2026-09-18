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

/// Defect: the 0.3.x-only version gate rejects the fixed Agent 0.5.0 even
/// though it speaks Protocol v1. Stage B replaces this with
/// `validate_backend(protocol_version, capabilities)`.
#[test]
fn baseline_bootstrap_rejects_agent_0_5_protocol_v1() {
    let mut app = ready_app();
    let requests = take_requests(app.update(AppEvent::Bootstrap));
    let ping = requests
        .iter()
        .find(|request| request.method == "agent.ping")
        .expect("bootstrap sends agent.ping");
    let _ = respond(
        &mut app,
        ping,
        json!({"version": "0.5.0", "protocol_version": 1, "capabilities": []}),
    );
    assert!(
        matches!(app.connection, ConnectionState::Failed(_)),
        "BASELINE: the 0.3.x-only gate rejects the fixed Agent 0.5.0"
    );
}

/// Defect: `PingResult` only carries `version`; it discards
/// `protocol_version` and `capabilities`. Stage B consumes both.
#[test]
fn baseline_ping_result_ignores_protocol_version_and_capabilities() {
    let ping: minicore_tui::protocol::PingResult =
        serde_json::from_value(json!({"version": "0.5.0"}))
            .expect("legacy PingResult accepts a bare version");
    assert_eq!(ping.version, "0.5.0");
    // There is no protocol_version/capabilities field on the current DTO; the
    // struct only exposes `version`. Stage B adds both fields.
    let debug = format!("{ping:?}");
    assert!(
        !debug.contains("protocol_version"),
        "BASELINE: PingResult has no protocol_version field yet"
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
