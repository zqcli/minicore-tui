//! Events produced by the RPC background tasks. Tasks only send events; app
//! state is mutated exclusively by the future `App::update` (development
//! spec 9.1).
//!
//! Ordering contract: frames and log lines arrive in the order their bytes
//! were read on their own pipe, but `Frame`, `AgentLogLine`,
//! `ConnectionClosed`, `Exited`, and `ProtocolError` are produced by four
//! independent tasks, so no total order is promised between them. The app
//! must latch the first connection-terminating event (`ProtocolError`,
//! `ConnectionClosed`, or `Exited`) into a terminal state and then ignore
//! later termination events idempotently (Phase 2 contract; see also
//! `RpcProcess::recv`).

use std::process::ExitStatus;

use crate::protocol::{FrameError, IncomingFrame, Reasoning, RequestId};
use crate::rpc::RpcError;
use crate::theme::ThemeKind;

/// A transport event from the agent process or its pipes. Events from
/// different tasks arrive without a promised total order; see the module
/// docs for the termination semantics.
#[derive(Debug)]
pub enum RpcEvent {
    /// One complete response or notification frame.
    Frame(IncomingFrame),
    /// One captured agent stderr line, UTF-8 and capped at 4096 bytes
    /// (spec 10.8). Stderr is never printed to the terminal.
    AgentLogLine(String),
    /// The agent's stdout pipe reached EOF.
    ConnectionClosed,
    /// Fatal protocol or pipe failure; the connection must be considered
    /// dead and frames must not be scanned ahead.
    ProtocolError(FrameError),
    /// The agent child ended. `None` means the exit status could not be
    /// obtained (the kill fallback path failed to reap).
    Exited(Option<ExitStatus>),
}

/// Everything the app loop hands to `App::update`. Tasks and the command
/// executor only produce these; they never hold the app or mutate it
/// directly (development spec 9.1).
#[derive(Debug)]
pub enum AppEvent {
    /// Start discovery: ping + model/profile/session list, issued together.
    /// The app is ready only after all four succeed.
    Bootstrap,
    /// Submit a non-empty message for a session (the composer arrives in
    /// Phase 5; this keeps the send path exercised).
    SubmitTurn {
        session_id: String,
        text: String,
    },
    /// Create and activate a session from catalog defaults (Phase 4 wires
    /// the new-session UI to this).
    CreateSession {
        workspace: String,
        profile: Option<String>,
        model: Option<String>,
        reasoning: Option<Reasoning>,
        title: Option<String>,
    },
    /// Open (and activate) an existing session; re-opening an already
    /// loaded session is idempotent.
    OpenSession {
        session_id: String,
    },
    /// Request cancellation of the active turn (Esc arrives in Phase 5).
    CancelTurn {
        session_id: String,
    },
    /// A transport event from the RPC background tasks.
    Rpc(RpcEvent),
    /// Executing an `AppCommand::Rpc` failed before any frame was written.
    /// The corresponding pending request is removed inside `update`.
    RpcSendFailed {
        id: RequestId,
        error: RpcError,
    },
    /// Advance the visual frame counter (spinner animation, spec 15.6).
    Tick,
    /// Select the color palette (spec 16.4).
    SetTheme(ThemeKind),
    /// Show or hide every reasoning run (spec 30.2).
    ToggleReasoning,
    /// Expand or collapse the result preview of every durable tool card in a
    /// session (spec 29.4).
    ToggleTools {
        session_id: String,
    },
    /// Expand or collapse one durable tool card.
    ToggleTool {
        session_id: String,
        turn_id: String,
        tool_call_id: String,
    },
    // ---- Phase 4: selectors (spec 24-28) -----------------------------
    //
    // Semantic actions only; the key mapping arrives in Phase 5.
    /// Open the new-session form with the catalog defaults (spec 25).
    OpenNewSession,
    /// Open a selector from the dock (session, model, reasoning, or
    /// profile). Opening model/reasoning/profile from the composer creates
    /// the new-session draft; the current session is never touched.
    OpenSessionSelector,
    OpenModelSelector,
    OpenReasoningSelector,
    OpenProfileSelector,
    /// Replace the open selector's query. Typing arrives in Phase 5; this
    /// keeps the filtering boundary testable now.
    SetSelectorQuery {
        query: String,
    },
    /// Move the open selector's cursor by `delta` rows (wrapping).
    MoveSelector {
        delta: i32,
    },
    /// Page the open selector's cursor by `delta` pages.
    PageSelector {
        delta: i32,
    },
    /// Confirm the dock: enter a selector from a field, open the selected
    /// session, or submit the new session (spec 25.3, 26.4, 28.6).
    ConfirmDock,
    /// Close the dock back to its parent: the composer for the form and
    /// session selector, the form for the model/reasoning/profile
    /// selectors.
    CancelDock,
    /// Move the highlighted new-session field by `delta` (Tab behaviour).
    DockFieldStep {
        delta: i32,
    },
    /// Set a text field of the new-session form (workspace/title).
    NewSessionSetField {
        field: super::state::NewSessionField,
        value: String,
    },
    /// Submit the new-session form via `session.create`.
    SubmitNewSession,
}
