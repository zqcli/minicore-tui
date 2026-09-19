//! Events produced by the RPC background tasks. Tasks only send events; app
//! state is mutated exclusively by the future `App::update` (development
//! spec 9.1).
//!
//! Ordering contract: frames and stderr notices arrive in the order their
//! bytes were read on their own pipe, but `Frame`, `AgentStderr`,
//! `ConnectionClosed`, `Exited`, and `ProtocolError` are produced by four
//! independent tasks, so no total order is promised between them. The app
//! must latch the first connection-terminating event during normal
//! operation. The explicit shutdown path instead drains buffered frames until
//! all producers close (see also `RpcProcess::recv`).

use std::process::ExitStatus;

use crossterm::event::Event as CrosstermEvent;

use crate::protocol::{FrameError, IncomingFrame, OutgoingRequest, Reasoning, RequestId};
use crate::rpc::{RpcError, SendClass};
use crate::state::view::PreparedConversation;
use crate::theme::ThemeKind;

/// A transport event from the agent process or its pipes. Events from
/// different tasks arrive without a promised total order; see the module
/// docs for the termination semantics.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum RpcEvent {
    /// One complete response or notification frame.
    Frame(IncomingFrame),
    /// One captured agent stderr notice: a UTF-8 line capped at 4096 bytes.
    /// Only the byte length is carried; stderr content is never retained or
    /// displayed (spec §19). `dropped` counts notices discarded because the
    /// bounded event channel was full (spec §5.4).
    AgentStderr { bytes: usize, dropped: usize },
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
    /// Steer the active running turn of a session.
    SteerTurn {
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
    /// Close an open session (spec 12, 52).
    CloseSession {
        session_id: String,
        confirm: bool,
    },
    /// Delete a session (spec 12).
    DeleteSession {
        session_id: String,
        confirm: bool,
    },
    /// Request cancellation of the active turn (Esc arrives in Phase 5).
    CancelTurn {
        session_id: String,
    },
    /// Re-read the retained completion for a blocked turn. This is an
    /// explicit one-shot operation; it never polls or retries automatically.
    RefreshTurn {
        session_id: String,
    },
    /// Ask the Agent to reload its configuration, then refresh the TUI's
    /// read-only catalogs and active-session projections.
    Reload,
    /// A transport event from the RPC background tasks.
    Rpc(RpcEvent),
    /// All RPC producer tasks have ended and no buffered transport event
    /// remains. The main loop disables its RPC select arm; the app remains
    /// renderable so a fatal state can be acknowledged with `q`.
    RpcChannelEnded,
    /// Executing an `AppCommand::Rpc` failed before any frame was written.
    /// The corresponding pending request is removed inside `update`.
    RpcSendFailed {
        id: RequestId,
        error: RpcError,
    },
    /// Synchronous admission found the outbound FIFO full (spec §5.2). The
    /// request was **not** written: the app revokes its pending registration
    /// and keeps one bounded retry intent keyed by the precise target.
    RpcQueueFull {
        request: OutgoingRequest,
        class: SendClass,
    },
    /// Advance the visual frame counter (spinner animation, spec 15.6).
    Tick,
    /// The user asked to leave (Ctrl+C twice, `/quit`, `q` in Help/Fatal) or
    /// an OS signal fired. When the connection is alive the app enters
    /// `ShuttingDown` and issues `agent.shutdown` once; a failed connection
    /// exits immediately.
    ShutdownRequested,
    /// The main loop finished drawing this frame; clears the dirty flag.
    Rendered,
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
        loop_id: String,
        request_index: u32,
        tool_call_id: String,
    },
    /// Toggle one stable thinking run without changing global reasoning
    /// visibility.
    ToggleReasoningSection {
        session_id: String,
        loop_id: String,
        request_index: u32,
        ordinal: u32,
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
    // ---- Phase 5: input (spec 22-23, 43) ------------------------------
    /// A raw terminal event (key, paste, mouse, resize) from the main
    /// loop. The fixed `keymap` turns keys into `Action`s; only
    /// `App::update` mutates state. Text moves in whole chunks (paste) or
    /// single chars — never one render per character.
    Terminal(CrosstermEvent),
    /// The main loop measured the transcript geometry for this frame
    /// (wrapped total rows and the rows visible in the transcript area).
    /// Scroll clamping lives here, in `update`, never in the renderer.
    Viewport {
        total_lines: usize,
        visible_rows: usize,
    },
    /// The main loop's latest terminal dimensions, used only for App-owned
    /// screen hit geometry. Rendering still remains read-only.
    TerminalSize {
        width: u16,
        height: u16,
    },
    /// An owned local job finished. Jobs never mutate the app; this event is
    /// the only hand-off (spec §5.5).
    JobFinished(JobOutcome),
    /// A complete render preparation result. `App::update` installs it only
    /// when the active session and content width still match. Rows, section
    /// ranges, copy ranges, and total height are one immutable snapshot.
    ConversationPrepared(PreparedConversation),
    /// A durable section layout produced by the single owned layout worker.
    DurableLayoutPrepared(crate::ui::transcript::DurableLayoutResult),
    /// One canonical Runtime history item finished in the single serialized
    /// decode worker. The identity is checked before any state mutation.
    HistoryItemDecoded(Box<crate::jobs::DecodeOutcome>),
    /// One loaded-content search scan finished in its owned worker. The
    /// identity and generation are checked before any state mutation.
    LocalScanFinished(Box<crate::jobs::LocalScanOutcome>),
}

/// The result of one owned local job (clipboard now; export and the draft
/// editor plug in here when they land).
#[derive(Debug)]
pub enum JobOutcome {
    /// The native clipboard adapter finished. The session/revision identify
    /// the capture that was copied so stale feedback cannot be shown for a
    /// newer selection.
    Clipboard {
        session_id: String,
        revision: u64,
        result: Result<(), String>,
    },
    /// The one owned export writer finished (spec §17.4). The outcome carries
    /// the target so a stale completion for another target is never shown.
    Export { outcome: crate::jobs::ExportOutcome },
}
