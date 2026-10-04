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
use minicore_tui::protocol::{IncomingFrame, OutgoingRequest, RpcResponse};
use serde_json::json;

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

/// Compatibility is capability-based, including both display-only reads;
/// package version strings alone do not authorize raw-history fallback.
#[test]
fn bootstrap_accepts_protocol_v1_with_all_required_capabilities() {
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
            assert!(
                message.contains("missing required capabilities"),
                "{message}"
            );
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

/// C1 migration (formerly the RED pin
/// `baseline_outbound_queue_is_64_and_send_awaits`): the UI admission path is
/// synchronous, bounded at 32 slots (28 ordinary, 4 reserved control), and a
/// full class refuses with a typed error instead of blocking `App::update`'s
/// caller. Behaviour is exercised against a real spawned child in
/// `tests/backpressure_baseline.rs`.
#[test]
fn ui_admission_is_synchronous_with_28_normal_and_4_control_slots() {
    let source = include_str!("../src/rpc.rs");
    assert!(source.contains("pub const OUTBOUND_QUEUE_CAPACITY: usize = 32;"));
    assert!(source.contains("pub const OUTBOUND_NORMAL_CAPACITY: usize = 28;"));
    assert!(source.contains("pub fn try_send("));
    assert!(source.contains("SendError::QueueFull("));
    assert!(
        !source.contains("REQUESTS_CHANNEL_CAPACITY"),
        "the single 64-slot awaiting channel is gone"
    );
    // `send` survives only as the transport test / non-UI helper; the main
    // loop never awaits the queue.
    assert!(source.contains("pub async fn send("));
    let main = include_str!("../src/main.rs");
    let production = main
        .split("#[cfg(test)]")
        .next()
        .expect("main.rs has a production half");
    assert!(!production.contains("process.send("));
}

/// C1 migration (formerly the RED pin `baseline_has_no_inbound_wire_byte_budget`):
/// the 32 MiB per-frame bound is joined by a 64 MiB aggregate budget whose
/// charge is released when the app takes ownership of a decoded frame, so a
/// fast producer cannot buffer decoded frames without bound (spec §5.4).
#[test]
fn inbound_wire_budget_is_released_by_frame_ownership() {
    let source = include_str!("../src/rpc.rs");
    assert!(source.contains("pub const MAX_RPC_FRAME_BYTES: usize = 32 * 1024 * 1024;"));
    assert!(source.contains("pub const MAX_WIRE_BUDGET_BYTES: usize = 64 * 1024 * 1024;"));
    assert!(
        source.contains("fn observe_consumed(") && source.contains("self.wire.release(bytes)"),
        "consuming a frame releases its share of the read budget"
    );
}

/// The five-state projection is a fixed Agent 0.5 wire fact; a running
/// compaction is carried alongside it, so `preparing` is representable instead
/// of being swallowed as idle (spec §7.3). The control methods the TUI needs
/// for that path now exist. Formerly the RED baseline pin
/// `baseline_has_no_preparing_state_or_control_methods`.
#[test]
fn session_state_represents_compaction_and_control_methods_exist() {
    let wire = serde_json::from_value::<minicore_tui::protocol::SessionStateWire>(json!({
        "session_id": "ses_1",
        "status": "running",
        "active_loop": null,
        "block_reason": null,
        "compaction": {
            "operation_id": "op_1",
            "phase": "preparing",
            "covered_item_count": 40,
            "retained_item_count": 2
        }
    }))
    .expect("a preparing compaction is decodable");
    let progress = wire.compaction.expect("compaction is retained");
    assert_eq!(
        progress.phase,
        minicore_tui::protocol::CompactionPhaseWire::Preparing
    );
    assert_eq!(progress.covered_item_count, 40);

    // A state without the field stays valid (it is absent, not defaulted to a
    // synthesised idle compaction).
    let plain = serde_json::from_value::<minicore_tui::protocol::SessionStateWire>(json!({
        "session_id": "ses_1",
        "status": "idle",
        "active_loop": null,
        "block_reason": null
    }))
    .expect("a state without compaction decodes");
    assert!(plain.compaction.is_none());

    let source = include_str!("../src/protocol.rs");
    for present in [
        "METHOD_SESSION_CONTEXT",
        "METHOD_TURN_RESULT",
        "METHOD_SESSION_COMPACT",
        "METHOD_SESSION_COMPACT_CANCEL",
        "METHOD_TOOL_READ",
        "METHOD_TOOL_OUTPUT",
        "METHOD_SESSION_READ",
        "METHOD_WORKSPACE_READ",
    ] {
        assert!(source.contains(present), "B1: {present} must exist");
    }
}

/// The read path is a real chunked Runtime-item decoder: the legacy indexed
/// display DTO is no longer the only history source, and the assembler
/// advances by delivered UTF-8 bytes. Formerly the RED baseline pin
/// `baseline_history_read_uses_the_legacy_display_dto`.
#[test]
fn history_read_uses_chunked_runtime_items_not_the_display_dto() {
    let source = include_str!("../src/protocol/read.rs");
    assert!(
        !source.contains("IndexedHistoryItemWire"),
        "B1: the read decoder must not reuse the legacy display DTO"
    );
    assert!(source.contains("struct ChunkAssembler"));
    assert!(source.contains("pub struct ReadChunk"));
    assert!(source.contains("pub enum Assembled"));
}

/// C2 migration (formerly the RED pin
/// `baseline_prepare_clones_full_history_and_rescans_tools`): the projection
/// builds one `ToolKey` index per pass and resolves each tool call in O(1)
/// (`tool_index.get`), never by scanning every block per call. The old defect
/// `all_lines` full-frame clone remains only in the test/diagnostic helper;
/// the production renderer composes through `prepare_conversation`.
#[test]
fn tool_projection_uses_one_index_and_never_rescans_blocks() {
    let source = include_str!("../src/ui/transcript.rs");
    assert!(
        source.contains("let mut tool_index") && source.contains("HashMap<(&str, u32, &str),"),
        "C2: the projection builds one tool index per pass"
    );
    assert!(
        source.contains("tool_index") && source.contains(".get(&("),
        "C2: each tool call resolves through the index"
    );
    assert!(
        !source.contains(".find_map(|block| match block.as_ref() {"),
        "C2: the projection must not scan every block per tool call"
    );
    assert!(
        source.contains("fn all_lines("),
        "the diagnostic helper remains available to tests"
    );
}

/// C1 migration (formerly the RED pin `baseline_run_commands_awaits_send_and_clipboard`):
/// `run_commands` admits synchronously with `try_send`, hands the clipboard to
/// an owned job, and never awaits either the writer or the clipboard in the
/// main loop. A full queue produces `AppEvent::RpcQueueFull` so the app can
/// revoke the pending registration and keep the typed input.
#[test]
fn run_commands_admits_synchronously_and_owns_the_clipboard() {
    let source = include_str!("../src/main.rs");
    // Test helpers may still await `send` when driving a fake child; the
    // production half must never await the outbound queue.
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("main.rs has a production half");
    assert!(production.contains("process.try_send("));
    assert!(
        !production.contains("process.send("),
        "the main loop must not await the RPC send"
    );
    assert!(production.contains("jobs.copy_to_clipboard("));
    assert!(source.contains("AppEvent::RpcQueueFull {"));
    assert!(production.contains("LocalJobs"), "owned jobs exist");
    assert!(include_str!("../src/lib.rs").contains("pub mod jobs;"));
    let jobs = include_str!("../src/jobs.rs");
    assert!(
        jobs.contains("tokio::spawn") && jobs.contains("shutdown"),
        "clipboard work runs on one owned async task with a joinable shutdown"
    );
    assert!(
        jobs.contains("spawn_blocking"),
        "C2b: durable layout work is isolated from the UI executor"
    );
    let clipboard = include_str!("../src/clipboard.rs");
    assert!(
        clipboard.contains("tokio::process::Command") && clipboard.contains("kill_on_drop(true)"),
        "the clipboard child is owned by an async process handle"
    );
    assert!(
        !clipboard.contains("thread::spawn"),
        "C2: no unrecyclable writer thread in the production clipboard"
    );
}

/// An unconfirmed wait/lost event now recovers through an authoritative
/// `turn.result` read-back. Formerly the RED baseline pin
/// `baseline_has_no_turn_result_recovery`; stage B1 adds `recover_turn` and the
/// behaviour is exercised end-to-end by
/// `tests/app_flow.rs::lost_wait_result_recovers_through_turn_result` and
/// `pending_turn_result_keeps_the_unconfirmed_fence`.
#[test]
fn turn_result_recovery_exists_and_is_settled_by_exact_turn() {
    let source = app_modules_source();
    assert!(source.contains("fn recover_turn"));
    assert!(
        source.contains("RequestKind::TurnResult"),
        "the read-back is routed by exact TurnRef, not by name or recency"
    );
    let request = minicore_tui::protocol::OutgoingRequest::turn_result(
        minicore_tui::protocol::RequestId(7),
        &minicore_tui::protocol::TurnRef {
            session_id: "ses_1".to_owned(),
            loop_id: "loop_1".to_owned(),
        },
        Some(minicore_tui::protocol::ReadCursor::start()),
        20,
        262_144,
    );
    assert_eq!(request.method, "turn.result");
    assert_eq!(request.params["turn"]["loop_id"], "loop_1");
}

/// B1 migration: `agent.reload` refreshes catalogs and metadata without a
/// second staged history authority. Existing execution/result/history state is
/// kept, and any needed history read uses the normal chain after the barrier.
#[test]
fn reload_does_not_stage_a_full_history_replacement() {
    let source = app_modules_source();
    assert!(!source.contains("struct ReloadHistoryStage"));
    assert!(!source.contains("fn install_reload_history"));
    assert!(source.contains("fn apply_reload"));
}

/// C1 migration (formerly the RED pin `baseline_control_sequences_reach_the_display_rows`):
/// (REF-48) the display boundary escapes control sequences in both the
/// markdown renderer and the plain wrapper, so an ESC/OSC-52+BEL payload can
/// never reach a terminal cell as a live sequence.
#[test]
fn control_sequences_are_escaped_at_the_display_boundary() {
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
        !rendered.contains('\u{1b}') && !rendered.contains('\u{7}'),
        "markdown rendering must not leak ESC/BEL control sequences"
    );
    assert!(
        !plain.contains('\u{1b}') && !plain.contains('\u{7}'),
        "plain wrapping must not leak ESC/BEL control sequences"
    );
    assert!(rendered.contains('␛') && plain.contains('␛'));
    assert!(rendered.contains("after") && plain.contains("after"));
}

/// C1 migration (formerly the RED pin `baseline_agent_stderr_is_logged_without_a_redaction_boundary`):
/// (REF-49) the Agent's stderr never becomes stored display text. The
/// transport emits only a byte count and a dropped count; the app keeps the
/// counters and the debug log stays method/id/byte-count/duration only.
#[test]
fn agent_stderr_is_never_stored_as_content() {
    let rpc = include_str!("../src/rpc.rs");
    assert!(
        !rpc.contains("AgentLogLine(agent_log_line"),
        "raw stderr text must not become an event payload"
    );
    assert!(
        rpc.contains("RpcEvent::AgentStderr {"),
        "stderr is reported as counts only"
    );
    let event = include_str!("../src/event.rs");
    assert!(event.contains("AgentStderr { bytes: usize, dropped: usize }"));
    assert!(
        !event.contains("AgentLogLine"),
        "no content-carrying stderr variant remains"
    );
    let app = app_modules_source();
    assert!(
        app.contains("fn push_stderr(&mut self, bytes: usize, dropped: usize)"),
        "the app records counts, not text"
    );
    assert!(
        app.contains("agent stderr: {bytes} bytes"),
        "visible feedback names the count, not the content"
    );
}
