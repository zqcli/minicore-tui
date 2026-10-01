//! The app state machine. Every state change happens inside
//! `App::update(AppEvent)`; RPC tasks and the command executor only send
//! `AppEvent`s or run `AppCommand`s (development spec 9.1). Request ids are
//! allocated inside `update` and registered in `pending_requests` before any
//! command leaves it, so a response can never beat its registration.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::Event as CrosstermEvent;

use crate::command::{AppCommand, CommandIssue, LocalCommand, is_slash_command, parse_command};
use crate::event::{AppEvent, JobOutcome, RpcEvent};
use crate::jobs::{DecodeIdentity, DecodeRequest, LocalScanIdentity, LocalScanRequest};
use crate::keymap::{self, Action, EditorCursor};
use crate::protocol::{
    AgentEventWire, EventMetaWire, IncomingFrame, METHOD_LIST_MODELS, METHOD_LIST_PROFILES,
    METHOD_LIST_SESSIONS, METHOD_PING, ModelInfo, OutgoingRequest, OutputChannelWire, ProfileInfo,
    READ_PAGE_LIMIT, READ_PAGE_MAX_BYTES, Reasoning, RequestId, RpcNotification, RpcResponse,
    RpcResponseError, SessionInfo, SessionStateWire, SessionStatusWire, ToolDisplayWire,
    ToolOutcomeWire, ToolProgressWire, TurnAvailability, TurnPersistenceWire, TurnRef,
    UserMessageKindWire, validate_backend,
};
use crate::rpc::{RpcError, SendClass};
use crate::state::catalog::CatalogState;
use crate::state::composer::{Composer, MAX_COMPOSER_BYTES};
use crate::state::export::ExportSpec;
use crate::state::selection::{
    Dock, NewSessionField, NewSessionState, SelectorKind, SelectorState, SessionConfirmChoice,
    SessionPanelAction, SessionPanelMode, SessionSelectorState, filtered_models, filtered_profiles,
    filtered_sessions, supported_reasoning,
};
use crate::state::session::{
    HistoryTrigger, ManualCompactState, ResultConfirmation, SessionId, SessionView, SessionsState,
};
use crate::state::tool::{LiveTool, ToolKey, ToolPresentationState, ToolStatus};
use crate::state::transcript::{
    AssistantBlock, AssistantPart, HistoryPlaceholderBlock, SummaryBlock, ToolBlock,
    TranscriptBlock, UserBlock,
};
use crate::state::turn::{
    AppliedSteer, LiveLoop, LivePart, LocalSubmissionId, OperationRef, PendingSteer,
    PendingSteerState, SteerQueueState, Submission, UnsavedLoop,
};
use crate::state::view::{
    ConversationLayout, ConversationSelection, FoldOverride, PreparedConversation, PreparedDurable,
    ReasoningKey, ScrollAnchor, SectionId, SelectionPoint,
};
use crate::theme::ThemeKind;
use crate::ui::transcript::{
    DurableLayoutIdentity, DurableLayoutRequest, DurableLayoutResult, DurableLayoutSnapshot,
};

pub mod changes;
#[cfg(test)]
mod changes_tests;
pub mod context;
#[cfg(test)]
mod context_tests;
pub mod copy;
pub mod export;
pub mod history;
#[cfg(test)]
mod history_navigation_tests;
pub mod panels;
#[cfg(test)]
mod panels_tests;
pub mod queries;
pub mod search;
pub mod session;
pub mod turn;
pub mod ui_actions;
pub mod workspace;
#[cfg(test)]
mod workspace_tests;
pub use self::ui_actions::SlashCompletionState;
use self::ui_actions::{EditorSelection, SelectionDrag};

/// The agent's stderr ring size, App side (spec 10.8).
pub const MAX_AGENT_LOG_LINES: usize = 200;
/// Bound for the per-session local steer FIFO. Small by design; a full queue
/// pauses and keeps the composer message rather than silently dropping.
pub const MAX_STEER_QUEUE_LEN: usize = 8;
pub const MAX_STEER_QUEUE_BYTES: usize = 256 * 1024;

const MAX_NOTICES: usize = 32;
const MAX_NOTICE_BYTES: usize = 4096;
const MAX_RETAINED_TURN_RESULTS: usize = 32;
/// The TUI targets at most this many outstanding deferred requests
/// (`turn.send`/`turn.wait`/`turn.result`/`session.compact`), leaving half of
/// the Agent's 32-slot deferred pool for other clients (spec §5.3/§21).
const MAX_DEFERRED_REQUESTS: usize = 16;
/// Bound for coalesced admission-failure intents. One intent per precise
/// target keeps the map naturally small; the cap only prevents pathology.
const MAX_RPC_RETRIES: usize = 64;

/// How long a transient notice stays before `Tick` removes it (spec 33.2).
const NOTICE_TTL: Duration = Duration::from_secs(5);
/// Normal busy-spinner cadence: ten frames per second.
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

/// Maximum time allowed for the orderly `agent.shutdown` sequence.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// The second Ctrl+C must follow the first within this window to quit
/// (spec 22.1, 43.7).
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(1);

/// Fixed text for interactions this TUI cannot answer (spec 11.7, 37.4).
pub const UNSUPPORTED_INTERACTION_NOTICE: &str =
    "This session is waiting for an interaction that this TUI version does not support.";
pub const UNCONFIRMED_RESULT_NOTICE: &str = "Last turn result/save status unconfirmed; reopening uses the Store history. Tool side effects may already exist.";

/// The connection lifecycle (spec 12.2). There is no reconnecting: a failed
/// bootstrap or a connection termination latches `Failed` forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Starting,
    Ready,
    ShuttingDown,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// A transient message for the status area. `sticky` notices persist until
/// dismissed (Phase 5); the rest age out naturally.
#[derive(Debug, Clone)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
    pub created_at: Instant,
    pub sticky: bool,
}

impl Notice {
    fn at(level: NoticeLevel, text: String, sticky: bool, created_at: Instant) -> Self {
        Self {
            level,
            text,
            created_at,
            sticky,
        }
    }
}

#[derive(Debug)]
struct MousePress {
    target: MouseTarget,
    column: u16,
    row: u16,
}

#[derive(Debug)]
enum MouseTarget {
    Editor,
    Conversation(SelectionPoint),
    Scrollbar,
    SessionSelector {
        session_id: SessionId,
        click_count: u8,
    },
    Selector {
        kind: SelectorKind,
        key: String,
        click_count: u8,
    },
    SessionAction(SessionPanelAction),
    NewSessionField(NewSessionField),
}

#[derive(Debug)]
struct LastClick {
    row: u16,
    at: Instant,
    count: u8,
    word_start: usize,
    word_end: usize,
}

#[derive(Debug)]
struct PanelClick {
    session_id: SessionId,
    at: Instant,
    count: u8,
}

#[derive(Debug)]
struct SelectorClick {
    kind: SelectorKind,
    key: String,
    at: Instant,
    count: u8,
}

#[derive(Debug)]
struct ScrollbarDrag {
    session_id: String,
    grab_offset: usize,
}

/// Why a request was issued; `pending_requests` routes each response to the
/// matching handler regardless of arrival order (spec 10.9, 10.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    Ping,
    Changes {
        session_id: String,
        epoch: u64,
        generation: u64,
        diff: bool,
    },
    WorkspaceStatus {
        session_id: String,
        epoch: u64,
        generation: u64,
    },
    Workspace {
        session_id: String,
        epoch: u64,
        generation: u64,
        kind: workspace::WorkspaceQuery,
    },
    ToolDetail {
        key: ToolKey,
        epoch: u64,
        generation: u64,
        stream: Option<crate::protocol::ToolDataStreamWire>,
    },
    /// A read request retired by a completed reload. Its response is
    /// consumed and intentionally ignored.
    StaleRead,
    Reload {
        generation: u64,
    },
    ListModels,
    ListProfiles,
    ListSessions,
    ReloadModels {
        generation: u64,
    },
    ReloadProfiles {
        generation: u64,
    },
    ReloadSessions {
        generation: u64,
    },
    RefreshSessions {
        selected_session_id: Option<SessionId>,
    },
    CreateSession {
        draft: u64,
    },
    OpenSession {
        session_id: SessionId,
        previous_retired_loop: Option<TurnRef>,
    },
    SessionState {
        session_id: SessionId,
        query: u64,
    },
    SessionPresentation {
        session_id: SessionId,
    },
    SessionContext {
        session_id: SessionId,
        generation: u64,
        owner: ContextQueryOwner,
    },
    Compact {
        session_id: SessionId,
        operation_id: String,
    },
    CompactCancel {
        session_id: SessionId,
        operation_id: String,
    },
    History {
        session_id: SessionId,
        read: ReadRequest,
    },
    /// One page of the explicit full-session search scan (spec §17.1). It
    /// shares the two read-only slots and never touches the history window.
    SearchRead {
        session_id: SessionId,
        generation: u64,
    },
    /// One page of the explicit export read chain (spec §17.4). It uses the
    /// same two read-only slots and never touches the history window.
    ExportRead {
        session_id: SessionId,
        export_id: u64,
    },
    SendTurn {
        session_id: SessionId,
        local_submission: LocalSubmissionId,
    },
    WaitTurn(TurnRef),
    /// Authoritative result read-back for a turn whose wait result was lost or
    /// unconfirmed (spec §7.2). Settled by exact `TurnRef`.
    TurnResult(TurnRef),
    SteerTurn {
        session_id: SessionId,
        loop_id: String,
        steer_id: u64,
        text: String,
        editor_revision: Option<u64>,
    },
    CancelTurn(TurnRef),
    UpdateSession {
        session_id: SessionId,
        loop_id: Option<String>,
        model: Option<String>,
        reasoning: Option<Reasoning>,
    },
    RenameSession {
        session_id: SessionId,
    },
    CloseSession {
        session_id: SessionId,
    },
    CloseVerifyState {
        session_id: SessionId,
    },
    DeleteSession {
        session_id: SessionId,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextQueryOwner {
    Panel(u64),
    Submission(LocalSubmissionId),
    ManualCompact(String),
    Explicit,
}

#[derive(Debug, Clone)]
struct ContextPoll {
    owner: ContextQueryOwner,
    due: Instant,
}

/// One `session.read` chain step (spec §6.3). `window_start` drops a reused
/// first-page chunk that falls below the requested window, and `replacement`
/// marks a re-pin that replaces the view instead of appending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    pub cursor: crate::protocol::ReadCursor,
    pub pin: Option<crate::protocol::SnapshotPin>,
    pub window_start: usize,
    pub replacement: bool,
    pub reconcile: bool,
    /// A one-item probe that only establishes the pin/`total` for a fresh
    /// window (spec §6.3 step 1). Its single item is reused only when it falls
    /// inside the chosen window.
    pub probe: bool,
    /// The local gap revision when the request was issued; a response cannot
    /// clear a gap observed after the request left.
    pub gap_revision: u64,
}

/// One request whose synchronous admission found the outbound FIFO full. It
/// was never written, so the exact request may be retried later (spec §5.2).
#[derive(Debug)]
struct RetryEntry {
    kind: RequestKind,
    request: OutgoingRequest,
}

/// The precise target of a retry intent. Repeated user intent for the same
/// target coalesces into one entry instead of queueing a second cancel/wait.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RetryKey {
    Submission(LocalSubmissionId),
    Wait(TurnRef),
    TurnResult(TurnRef),
    Steer {
        session_id: SessionId,
        steer_id: u64,
    },
    CancelTurn(TurnRef),
    Compact {
        session_id: SessionId,
        operation_id: String,
    },
    CancelCompact {
        session_id: SessionId,
        operation_id: String,
    },
    History(SessionId),
    SessionRead {
        session_id: SessionId,
        label: &'static str,
    },
}

/// Startup session selection from the CLI (spec §6.1): `--session <id>` is
/// exact and never prompts, `--continue` matches only the current explicit
/// workspace and falls back to the selector when nothing matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupSession {
    Exact(String),
    ContinueCurrentWorkspace,
}

/// CLI preferences injected at construction (spec 6.1). They only seed the
/// catalog's next-session seats, so an existing session is never touched;
/// a `None` seat lets the catalog default apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliPrefs {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
    /// When set and no session is active, a Ready app opens a pre-filled
    /// new-session form (explicit `--workspace`; never auto-creates).
    pub open_new_session_on_ready: bool,
    /// Consumed once, when the first current catalog arrives (spec §6.1).
    pub startup_session: Option<StartupSession>,
}

struct TranscriptFrame {
    generation: u64,
    session_id: String,
    session_epoch: u64,
    theme: ThemeKind,
    terminal_size: (u16, u16),
    scroll: (usize, bool, Option<usize>),
    tool_hits: Vec<(ratatui::layout::Rect, ToolKey)>,
    cells: ratatui::buffer::Buffer,
}

/// All app and UI state. The reducer owns mutations; the main loop also
/// records successfully drawn transcript cells. Renderers and workers only read.
pub struct App {
    pub connection: ConnectionState,
    pub catalogs: CatalogState,
    pub sessions: SessionsState,
    pub notices: VecDeque<Notice>,
    /// CLI startup session intent, consumed once (spec §6.1).
    pub startup_session: Option<StartupSession>,
    /// One warning per over-budget episode: while admission is blocked the
    /// same notice is not repeated on every keystroke.
    pub draft_budget_warned: bool,
    /// Set by every `update` except `Rendered`, which clears it, so the main
    /// loop can throttle draws (max 30 FPS) without missing a change.
    pub dirty: bool,
    /// The agent's stderr ring, newest last (spec 10.8).
    pub agent_logs: VecDeque<String>,
    /// Safe process status captured when the child reports `Exited`; it never
    /// contains a raw frame or command-line content.
    pub child_exit_status: Option<String>,
    /// Visual state (spec 16, 30): palette, reasoning visibility, the frame
    /// counter for the spinner, and the Phase 5 composer.
    pub theme: ThemeKind,
    pub reasoning_visible: bool,
    /// Local TUI preferences and the file they came from. These never contain
    /// provider credentials or Agent catalog data.
    pub tui_config: crate::config::TuiConfig,
    pub config_path: PathBuf,
    pub agent_restart_required: bool,
    pub frame_count: u64,
    pub composer: Composer,
    /// Local slash candidates derived from `command::COMMANDS`.
    pub slash_completion: Option<SlashCompletionState>,
    /// Preferred visual column while moving vertically through wrapped editor
    /// rows, matching the native editor's temporary vertical-column state.
    composer_preferred_visual_col: Option<usize>,
    /// The dock panel below the transcript (spec 24.1).
    pub dock: Dock,
    pub main_view: crate::state::panels::MainView,
    pub focus: crate::state::panels::Focus,
    tool_generation: u64,
    workspace_generation: u64,
    /// Measured transcript geometry for scroll math (total wrapped rows,
    /// visible rows); written only via `AppEvent::Viewport` from the main
    /// loop, never by the renderer.
    pub viewport: (usize, usize),
    /// Last measured total, to detect content that grew while scrolled up.
    last_total: usize,
    /// Manual scroll offset inside the Help/Logs panels.
    pub panel_scroll: usize,
    /// Double Ctrl+C window anchor.
    ctrl_c_at: Option<Instant>,
    /// Latest terminal size for mapping a mouse release to prepared rows.
    terminal_size: (u16, u16),
    mouse_down: Option<MousePress>,
    /// A press that landed on a link cell must not toggle a section fold on
    /// release (RAIL-14 pressedUrl guard, matching the fixed source).
    mouse_pressed_on_link: bool,
    last_click: Option<LastClick>,
    panel_click: Option<PanelClick>,
    selector_click: Option<SelectorClick>,
    scrollbar_drag: Option<ScrollbarDrag>,
    scrollbar: crate::ui::scrollbar::ScrollbarState,
    selection_drag: Option<SelectionDrag>,
    editor_selection: Option<EditorSelection>,
    /// The single prepared conversation snapshot shared by measurement,
    /// rendering, hit testing, selection, and copying.
    prepared_conversation: Option<PreparedConversation>,
    /// Only terminal cells, never history/layout ownership. Used while the
    /// production worker replaces a same-session, same-size conversation.
    transcript_frame: Option<TranscriptFrame>,
    prepared_generation: u64,
    /// Production rendering never rebuilds a durable layout synchronously;
    /// the main loop requests it from the single owned worker instead.
    async_layout: bool,
    /// Production enables this together with the owned LocalJobs decode
    /// worker. The false default is an explicit deterministic compatibility /
    /// fixture path; it is never used by `main`.
    async_decode: bool,
    pending_decode: Option<DecodeRequest>,
    decode_in_flight: Option<DecodeIdentity>,
    layout_pending: Option<DurableLayoutIdentity>,
    layout_partial: Option<(DurableLayoutIdentity, Arc<ConversationLayout>)>,
    next_layout_generation: u64,
    /// Current transcript selection. It is presentation-only and is rebased
    /// by stable section identity when a prepared snapshot changes.
    pub selection: Option<ConversationSelection>,
    /// Monotonic deadline for the one-row `selection copied` footer state.
    selection_copied_until: Option<Instant>,
    /// Independent monotonic deadline for the next spinner frame. Other Tick
    /// sources (selection and notice expiry) must not advance the spinner.
    spinner_next_due: Option<Instant>,
    /// Notice lifetime; a field so tests can shorten/past-expire it.
    pub notice_ttl: Duration,
    /// The new-session draft while a model/reasoning/profile selector sits
    /// on top of the form; `Some` only then (spec 26.4).
    draft: Option<NewSessionState>,
    /// `agent.shutdown` was issued; the response routes to
    /// `RequestKind::Shutdown` and the child exit ends the run.
    shutdown_sent: bool,
    /// Monotonic shutdown deadline, latched on the first shutdown request and
    /// never extended by repeated quit or signal events.
    shutdown_deadline: Option<Instant>,
    /// The agent child ended while `ShuttingDown` (a `RpcEvent::Exited`).
    shutdown_child_exited: bool,
    /// When set, reaching `Ready` with no active session opens a pre-filled
    /// new-session form (explicit `--workspace`).
    open_new_session_on_ready: bool,
    /// Clock for session-relative ages; injectable so render output is
    /// deterministic in tests. Read-only, never mutated by `update`.
    pub now: fn() -> SystemTime,
    /// Monotonic clock used for shutdown and transient timing. Production
    /// uses `Instant::now`; tests may inject a virtual clock at construction.
    monotonic_now: Arc<dyn Fn() -> Instant + Send + Sync>,
    pub pending_requests: HashMap<RequestId, RequestKind>,
    /// Coalesced retries for requests refused by the bounded outbound FIFO.
    /// Bounded by [`MAX_RPC_RETRIES`]; drains on the next progress event.
    pending_retries: std::collections::BTreeMap<RetryKey, RetryEntry>,
    /// Catalog generation captured when each `session.list` request was
    /// issued; a response older than the current generation is discarded.
    session_list_requests: std::collections::BTreeMap<RequestId, u64>,
    /// Monotonic identity for the current selection, so a clipboard job that
    /// finishes after the selection changed does not show stale feedback.
    selection_revision: u64,
    /// The two read-only in-flight slots (spec §5.3). Execution waits are
    /// counted separately and never take a slot.
    pub queries: crate::app::queries::QuerySlots,
    /// Search generation: only the newest one may install results.
    search_generation: u64,
    /// Item-window read attempts for the current jump, bounded so an
    /// unloadable target cannot loop.
    search_jump_attempts: u32,
    /// Fold overrides a search jump installed temporarily; closing the search
    /// restores the exact previous user choice.
    search_fold_restores: Vec<search::FoldRestore>,
    /// A jump whose target item is not resident yet.
    pending_search_jump: Option<(SessionId, search::PendingSearchJump)>,
    /// The explicit full-session search scan chain, if one is running.
    search_scan: Option<search::SearchScan>,
    /// The one owned export chain, if an export is running (spec §17.4).
    export_scan: Option<export::ExportScan>,
    /// Monotonic identity for the current export, so a completion for an older
    /// target or session is never shown.
    export_id: u64,
    /// Content options captured when the export started: the form can no
    /// longer change them while the file is being written.
    export_spec: ExportSpec,
    export_include_unsaved: bool,
    /// The bounded hand-off to the owned export writer.
    export_tx: Option<tokio::sync::mpsc::Sender<crate::jobs::ExportInbound>>,
    /// The identity of the export the App last started. A completion is routed
    /// by this capture, so a stale result can never decorate a newer form.
    export_capture: Option<crate::jobs::ExportCapture>,
    /// The cancel token shared with the owned writer: setting it makes the
    /// writer abort without waiting for channel room.
    export_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// At most one record waiting for room in the bounded channel. Paging is
    /// paused while it is set (backpressure, never an unbounded buffer).
    export_outbox: VecDeque<crate::jobs::ExportInbound>,
    export_hold: bool,
    /// Previous preferences retained until an atomic settings write reports.
    settings_previous: Option<crate::config::TuiConfig>,
    settings_previous_restart_required: Option<bool>,
    editor_capture: Option<crate::jobs::EditorCapture>,
    next_editor_operation: u64,
    /// Paged authoritative result bodies keyed by their exact TurnRef. Their
    /// item indexes are turn-local and never enter the session history window.
    turn_results: HashMap<TurnRef, crate::app::history::TurnResultWindow>,
    /// Exact wait/result summaries retained across reload/reopen boundaries.
    retained_results: HashMap<TurnRef, crate::protocol::TurnResultViewWire>,
    retained_result_order: VecDeque<TurnRef>,
    /// Deferred turn submissions retain their exact draft identity until the
    /// Agent returns a real TurnRef or a preparation failure.
    submissions: HashMap<LocalSubmissionId, Submission>,
    context_polls: HashMap<SessionId, ContextPoll>,
    next_operation_id: u64,
    pub context_supported: bool,
    pub compact_supported: bool,
    pub compact_cancel_supported: bool,
    /// Read keys whose slot became available while a response was being
    /// reduced. They are converted to requests only after the page/result
    /// handler has installed its newest cursor.
    pending_query_followups: VecDeque<crate::app::queries::QueryKey>,
    next_request_id: RequestId,
    next_state_query: u64,
    next_submission: u64,
    next_draft_id: u64,
    next_steer_id: u64,
    next_reload_generation: u64,
    next_read_chain: u64,
    /// Lifecycle responses crossing a reload boundary must start from fresh
    /// session state authority; the existing history window is not staged or
    /// replaced by configuration reload.
    bootstrap: BootstrapProgress,
    reload: Option<ReloadProgress>,
    /// Guards the single "not ready" notice so a Failed/Starting connection
    /// cannot flood the user; reset when the app becomes Ready again.
    blocked_notice: bool,
}

#[derive(Default)]
struct BootstrapProgress {
    ping: bool,
    models: bool,
    profiles: bool,
    sessions: bool,
}

impl BootstrapProgress {
    fn done(&self) -> bool {
        self.ping && self.models && self.profiles && self.sessions
    }
}

#[derive(Clone, Copy)]
enum BootstrapPart {
    Ping,
    Models,
    Profiles,
    Sessions,
}

/// What a completed history chain should do next.
enum NextChain {
    /// The window is incomplete; request the next page (the cursor comes from
    /// the backend, not a local offset).
    Page,
    /// The gap fence advanced while a page was in flight; re-read the tail
    /// under the newer revision before releasing it.
    Reconcile,
    LoopNotContained(String),
    Done,
}

/// One in-flight catalog generation. A configuration reload refreshes
/// catalogs only: it never stages session state, presentation, or history and
/// never patches a live loop, draft, or selection (spec §3.5/§9).
struct ReloadProgress {
    generation: u64,
    acknowledged: bool,
    models: Option<Vec<ModelInfo>>,
    profiles: Option<Vec<ProfileInfo>>,
    sessions: Option<Vec<SessionInfo>>,
}

impl ReloadProgress {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            acknowledged: false,
            models: None,
            profiles: None,
            sessions: None,
        }
    }
}

fn startup_error_message(method: &str, error: &RpcResponseError) -> String {
    match error {
        RpcResponseError::Parse(error) => {
            format!("protocol error during startup request {method}: {error}")
        }
        RpcResponseError::Malformed => {
            format!("protocol error during startup request {method}: malformed response")
        }
        RpcResponseError::Agent(error) => {
            let category = match error.code {
                crate::protocol::STORE_ERROR => "storage error",
                crate::protocol::PROVIDER_ERROR => "provider error",
                -32_014 => "Agent configuration rejected",
                _ => match error.data.as_ref().map(|data| data.kind.as_str()) {
                    Some("storage" | "store") => "storage error",
                    Some("provider") => "provider error",
                    Some("config" | "configuration" | "agent_config") => {
                        "Agent configuration rejected"
                    }
                    _ => "Agent startup error",
                },
            };
            format!("{category} during startup request {method}: {error}")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionActionSafety {
    Safe,
    Unknown,
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionStateSource {
    Notification,
    FreshResponse,
    CloseVerifyResponse,
}

impl App {
    pub fn new(default_workspace: PathBuf) -> Self {
        Self {
            connection: ConnectionState::Starting,
            catalogs: CatalogState {
                models: Vec::new(),
                profiles: Vec::new(),
                loaded: false,
                next_profile: None,
                next_model: None,
                next_reasoning: None,
                default_workspace,
                session_list_generation: 0,
                pending_deletions: std::collections::BTreeMap::new(),
            },
            sessions: SessionsState::default(),
            notices: VecDeque::new(),
            dirty: false,
            agent_logs: VecDeque::new(),
            child_exit_status: None,
            theme: ThemeKind::Dark,
            reasoning_visible: true,
            tui_config: crate::config::TuiConfig::default(),
            config_path: crate::config::default_path(),
            agent_restart_required: false,
            frame_count: 0,
            composer: Composer::default(),
            slash_completion: None,
            composer_preferred_visual_col: None,
            dock: Dock::Composer,
            main_view: Default::default(),
            focus: Default::default(),
            tool_generation: 0,
            workspace_generation: 0,
            viewport: (0, 0),
            last_total: 0,
            panel_scroll: 0,
            ctrl_c_at: None,
            terminal_size: (80, 24),
            mouse_down: None,
            mouse_pressed_on_link: false,
            last_click: None,
            panel_click: None,
            selector_click: None,
            scrollbar_drag: None,
            scrollbar: crate::ui::scrollbar::ScrollbarState::default(),
            selection_drag: None,
            editor_selection: None,
            prepared_conversation: None,
            transcript_frame: None,
            prepared_generation: 0,
            async_layout: false,
            async_decode: false,
            pending_decode: None,
            decode_in_flight: None,
            layout_pending: None,
            layout_partial: None,
            next_layout_generation: 0,
            selection: None,
            selection_copied_until: None,
            spinner_next_due: None,
            notice_ttl: NOTICE_TTL,
            draft: None,
            shutdown_sent: false,
            shutdown_deadline: None,
            shutdown_child_exited: false,
            open_new_session_on_ready: false,
            startup_session: None,
            draft_budget_warned: false,
            now: SystemTime::now,
            pending_requests: HashMap::new(),
            pending_retries: std::collections::BTreeMap::new(),
            session_list_requests: std::collections::BTreeMap::new(),
            selection_revision: 0,
            queries: crate::app::queries::QuerySlots::new(),
            search_generation: 0,
            search_jump_attempts: 0,
            search_fold_restores: Vec::new(),
            pending_search_jump: None,
            search_scan: None,
            export_scan: None,
            export_id: 0,
            export_spec: ExportSpec::default(),
            export_include_unsaved: false,
            export_tx: None,
            export_capture: None,
            export_cancel: None,
            export_outbox: VecDeque::new(),
            export_hold: false,
            settings_previous: None,
            settings_previous_restart_required: None,
            editor_capture: None,
            next_editor_operation: 0,
            turn_results: HashMap::new(),
            retained_results: HashMap::new(),
            retained_result_order: VecDeque::new(),
            submissions: HashMap::new(),
            context_polls: HashMap::new(),
            next_operation_id: 0,
            context_supported: true,
            compact_supported: true,
            compact_cancel_supported: true,
            pending_query_followups: VecDeque::new(),
            next_request_id: RequestId(0),
            next_state_query: 0,
            next_submission: 0,
            next_draft_id: 0,
            next_steer_id: 0,
            next_reload_generation: 0,
            next_read_chain: 0,
            bootstrap: BootstrapProgress::default(),
            reload: None,
            blocked_notice: false,
            monotonic_now: Arc::new(Instant::now),
        }
    }

    /// Same as [`App::new`], with CLI preferences seeded into the catalog's
    /// next-session seats (spec 6.1).
    pub fn with_cli_prefs(default_workspace: PathBuf, prefs: CliPrefs) -> Self {
        let mut app = Self::new(default_workspace);
        app.set_cli_prefs(prefs);
        app
    }

    pub fn set_cli_prefs(&mut self, prefs: CliPrefs) {
        self.catalogs.next_profile = prefs.profile;
        self.catalogs.next_model = prefs.model;
        self.catalogs.next_reasoning = prefs.reasoning;
        self.open_new_session_on_ready = prefs.open_new_session_on_ready;
        self.startup_session = prefs.startup_session;
    }

    /// Constructs an app with a caller-supplied monotonic clock. This is
    /// useful for deterministic lifecycle tests; production uses [`App::new`].
    pub fn with_monotonic_clock<F>(default_workspace: PathBuf, clock: F) -> Self
    where
        F: Fn() -> Instant + Send + Sync + 'static,
    {
        let mut app = Self::new(default_workspace);
        app.monotonic_now = Arc::new(clock);
        app
    }

    fn instant_now(&self) -> Instant {
        (self.monotonic_now)()
    }

    fn spinner_active(&self) -> bool {
        self.sessions.known.values().any(|view| {
            view.live.is_some()
                || view.is_preparing()
                || view
                    .state
                    .as_ref()
                    .is_some_and(|state| state.status != SessionStatusWire::Idle)
        })
    }

    fn sync_spinner_deadline(&mut self) {
        if self.spinner_active() {
            if self.spinner_next_due.is_none() {
                self.spinner_next_due = Some(
                    self.instant_now()
                        .checked_add(SPINNER_INTERVAL)
                        .expect("spinner deadline is representable"),
                );
            }
        } else {
            self.spinner_next_due = None;
        }
    }

    /// Whether the app currently needs a visual tick. The main loop sleeps
    /// until the earliest of the spinner cadence, transient-notice expiry,
    /// and the double-Ctrl+C window; `None` means idle and no timer is armed.
    pub fn next_tick(&self) -> Option<Duration> {
        let now = self.instant_now();
        let mut earliest: Option<Duration> = None;
        if self.spinner_active() {
            let remaining = self
                .spinner_next_due
                .map(|deadline| deadline.saturating_duration_since(now))
                .unwrap_or(SPINNER_INTERVAL);
            earliest = Some(remaining);
        }
        if let Some(deadline) = self.scrollbar.hide_at {
            let remaining = deadline.saturating_duration_since(now);
            earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
        }
        for notice in &self.notices {
            if !notice.sticky {
                let expiry = notice
                    .created_at
                    .checked_add(self.notice_ttl)
                    .expect("notice expiry is representable");
                let remaining = expiry.saturating_duration_since(now);
                earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
            }
        }
        if let Some(at) = self.ctrl_c_at {
            let expiry = at
                .checked_add(DOUBLE_CTRL_C_WINDOW)
                .expect("Ctrl+C expiry is representable");
            let remaining = expiry.saturating_duration_since(now);
            earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
        }
        if let Some(deadline) = self.selection_copied_until {
            let remaining = deadline.saturating_duration_since(now);
            earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
        }
        if let Some(deadline) = self.tool_detail().and_then(|detail| detail.due) {
            let remaining = deadline.saturating_duration_since(now);
            // A queued/in-flight query wakes on its actual completion, not a
            // zero-duration timer spin while both shared slots are occupied.
            if !self.queries.contains(&crate::app::queries::QueryKey::Tool {
                key: self.tool_detail().unwrap().key.clone(),
            }) {
                let remaining = remaining.max(Duration::from_millis(50));
                earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
            }
        }
        if let Some(browser) = self.workspace_browser() {
            if let Some(deadline) = browser.due.filter(|_| {
                !(browser.kind == crate::state::workspace::BrowserKind::Grep
                    && browser.query.is_empty())
            }) {
                if self.can_send_requests()
                    && self.queries.in_flight_len() < queries::QuerySlots::CAPACITY
                    && self.deferred_pending() + self.queries.in_flight_len() < MAX_DEFERRED_REQUESTS
                    && !self.pending_requests.values().any(|k| matches!(k, RequestKind::Workspace { kind, .. } if *kind != workspace::WorkspaceQuery::File)) {
                    let remaining = deadline.saturating_duration_since(now).max(Duration::from_millis(50));
                    earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
                }
            }
        }
        for (session, poll) in &self.context_polls {
            if self.context_query_pending(session) {
                continue;
            }
            let remaining = poll.due.saturating_duration_since(now);
            earliest = Some(earliest.map_or(remaining, |e| e.min(remaining)));
        }
        if self
            .selection_drag
            .as_ref()
            .is_some_and(|drag| self.selection_drag_direction(drag.row) != 0)
        {
            let selection_deadline = self
                .selection_drag
                .as_ref()
                .expect("selection drag was present")
                .next_deadline
                .saturating_duration_since(now);
            earliest = Some(earliest.map_or(selection_deadline, |e| e.min(selection_deadline)));
        }
        earliest
    }

    /// The main loop arms its 5-second kill fallback while this is true.
    pub fn shutting_down(&self) -> bool {
        self.connection == ConnectionState::ShuttingDown && !self.shutdown_child_exited
    }

    /// The user-facing result/persistence facts that remain after a forced
    /// shutdown. This is deliberately conservative: a live turn without a
    /// direct wait result is unknown, not failed or absent.
    pub fn shutdown_force_message(&self) -> String {
        let known_failure = self.sessions.known.values().any(|view| {
            view.unsaved_loop.is_some()
                || view
                    .last_result
                    .as_ref()
                    .is_some_and(|result| result.persistence == Some(TurnPersistenceWire::Failed))
        });
        let unconfirmed = self.sessions.known.values().any(|view| {
            view.result_confirmation != ResultConfirmation::Confirmed
                || view.live.as_ref().is_some_and(|live| {
                    live.last_result
                        .as_ref()
                        .is_none_or(|result| live.reference.as_ref() != Some(&result.turn))
                })
        });
        let mut message = "shutdown timed out; Agent force-terminated".to_owned();
        if unconfirmed {
            message.push_str(
                "; last turn result/save status unconfirmed; reopening uses the Store history. Tool side effects may already exist",
            );
        }
        if known_failure {
            message.push_str("; known persistence failure retained");
        }
        if let Some(stderr) = self.agent_logs.back() {
            message.push_str("; last Agent stderr: ");
            message.push_str(stderr);
        }
        message
    }

    /// Remaining time in the latched shutdown window. An expired deadline is
    /// deliberately returned as `Some(Duration::ZERO)` so a timer cannot be
    /// accidentally disarmed at the exact cutoff.
    pub fn shutdown_remaining(&self) -> Option<Duration> {
        if !self.shutting_down() {
            return None;
        }
        self.shutdown_deadline
            .map(|deadline| deadline.saturating_duration_since(self.instant_now()))
    }

    /// Read-only: whether a request with this id is currently registered in
    /// the pending map (tests pin the register-before-send contract).
    pub fn request_is_pending(&self, id: RequestId) -> bool {
        self.pending_requests.contains_key(&id)
    }

    /// Read-only: the pending kind for a request id, if registered.
    pub fn pending_request_kind(&self, id: RequestId) -> Option<&RequestKind> {
        self.pending_requests.get(&id)
    }

    pub fn notices(&self) -> &VecDeque<Notice> {
        &self.notices
    }

    /// The single state-mutation entry point. Returns the side effects the
    /// main loop must execute; commands are never executed here.
    pub fn update(&mut self, event: AppEvent) -> Vec<AppCommand> {
        self.sync_spinner_deadline();
        let reload_active_before = self.reload.is_some();
        let reload_event = self.is_reload_event(&event);
        if matches!(&event, AppEvent::Rendered) {
            self.dirty = false;
            return Vec::new();
        }
        if matches!(&event, AppEvent::TerminalSize { width, height }
            if (*width, *height) == self.terminal_size)
        {
            return Vec::new();
        }
        // A displayed transition is not the current layout. Never resolve
        // its cells against new section indices (or a placeholder layout).
        let scroll_key = self.async_layout
            && matches!(&event, AppEvent::Terminal(CrosstermEvent::Key(key))
                if matches!(keymap::map(self, *key), Action::ScrollRows(_)
                    | Action::ScrollWindow(_) | Action::ScrollTop | Action::ScrollBottom));
        let transcript_press = self.mouse_down.as_ref().is_some_and(|press| {
            matches!(
                &press.target,
                MouseTarget::Conversation(_) | MouseTarget::Scrollbar
            )
        });
        if self.async_layout
            && !self.has_main_detail()
            && (scroll_key
                || transcript_press
                || matches!(&event, AppEvent::Terminal(CrosstermEvent::Mouse(_)))
                || self.selection_drag.is_some()
                || self.scrollbar_drag.is_some())
            && !self.transcript_input_ready()
        {
            let was_dragging = self.selection_drag.is_some() || self.scrollbar_drag.is_some();
            self.selection_drag = None;
            self.scrollbar_drag = None;
            if transcript_press {
                self.mouse_down = None;
                self.mouse_pressed_on_link = false;
            }
            if scroll_key && matches!(self.dock, Dock::Composer | Dock::Search(_)) {
                return Vec::new();
            }
            if let AppEvent::Terminal(CrosstermEvent::Mouse(mouse)) = &event {
                let area =
                    ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1);
                let screen = crate::ui::layout::screen_layout(self, area);
                if screen.transcript.contains((mouse.column, mouse.row).into()) || was_dragging {
                    self.mouse_down = None;
                    self.mouse_pressed_on_link = false;
                    // The retained frame can still identify its own explicit
                    // tool button. Never resolve other stale cells against a
                    // newer layout (selection, links, folds and scrolling).
                    if mouse.kind
                        != crossterm::event::MouseEventKind::Down(
                            crossterm::event::MouseButton::Left,
                        )
                        || self.displayed_tool_hit(mouse.column, mouse.row).is_none()
                    {
                        return Vec::new();
                    }
                }
            }
        }
        if let AppEvent::Terminal(CrosstermEvent::Mouse(mouse)) = &event {
            if mouse.kind == crossterm::event::MouseEventKind::Moved {
                if self.scrollbar_drag.is_some() && self.scrollbar_allowed() {
                    let before = self.scroll_visual_state();
                    self.update_scrollbar_drag(mouse.row);
                    self.dirty |= before != self.scroll_visual_state();
                } else {
                    self.dirty |= self.update_scrollbar_hover(mouse.column, mouse.row);
                }
                return Vec::new();
            }
        }
        if matches!(&event, AppEvent::Viewport { total_lines, visible_rows }
            if (*total_lines, *visible_rows) == self.viewport)
        {
            return Vec::new();
        }
        let queues_empty = self
            .sessions
            .known
            .values()
            .all(|view| view.steer_queue.is_empty());
        let idle_tick_before = if queues_empty
            && matches!(&event, AppEvent::Tick)
            && !self.spinner_active()
            && self.selection_drag.is_none()
            && self.notices.is_empty()
            && self.ctrl_c_at.is_none()
            && self.selection_copied_until.is_none()
        {
            Some((self.dirty, self.scrollbar.hide_at))
        } else {
            None
        };
        let scroll_visual_before = if (matches!(&event, AppEvent::Terminal(CrosstermEvent::Key(key))
            if matches!(keymap::map(self, *key), Action::ScrollRows(_) | Action::ScrollWindow(_) | Action::ScrollTop | Action::ScrollBottom))
            || matches!(&event,
            AppEvent::Terminal(CrosstermEvent::Mouse(mouse))
                if matches!(mouse.kind, crossterm::event::MouseEventKind::ScrollUp | crossterm::event::MouseEventKind::ScrollDown)
                    || (mouse.kind == crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) && self.scrollbar_drag.is_some())))
            && queues_empty
            && !self.has_main_detail()
            && matches!(self.dock, Dock::Composer)
            && self.selection.is_none()
            && self.editor_selection.is_none()
            && self.selection_drag.is_none()
            && !self.selection_copied()
        {
            Some((self.dirty, self.scroll_visual_state()))
        } else {
            None
        };
        let reuse_layout = match &event {
            AppEvent::ConversationPrepared(_) | AppEvent::Tick | AppEvent::Viewport { .. } => true,
            AppEvent::Terminal(
                CrosstermEvent::Mouse(_)
                | CrosstermEvent::FocusLost
                | CrosstermEvent::FocusGained
                | CrosstermEvent::Paste(_),
            ) => true,
            AppEvent::Terminal(CrosstermEvent::Key(key)) => matches!(
                keymap::map(self, *key),
                Action::None
                    | Action::TypeChar(_)
                    | Action::Newline
                    | Action::Backspace
                    | Action::Delete
                    | Action::CursorMove(_)
                    | Action::LineStart
                    | Action::LineEnd
                    | Action::WordDelete
                    | Action::Undo
                    | Action::Redo
                    | Action::HistoryPrev
                    | Action::HistoryNext
                    | Action::CompletionMove(_)
                    | Action::CompletionAccept
                    | Action::CompletionCancel
                    | Action::ScrollRows(_)
                    | Action::ScrollWindow(_)
                    | Action::ScrollTop
                    | Action::ScrollBottom
            ),
            _ => false,
        };
        if !reuse_layout {
            self.capture_scroll_anchor();
            self.prepared_conversation = None;
        }
        // Admission-failure retries are only re-emitted after a progress
        // signal, never in direct response to another `RpcQueueFull`. That
        // keeps `run_commands` from ping-ponging inside one reducer pass.
        let progress_signal = matches!(
            &event,
            AppEvent::Tick
                | AppEvent::Rpc(_)
                | AppEvent::RpcSendFailed { .. }
                | AppEvent::RpcChannelEnded
        );
        // Hit tests in one mouse event share the same immutable preparation.
        if matches!(&event, AppEvent::Terminal(CrosstermEvent::Mouse(mouse))
            if !matches!(mouse.kind, crossterm::event::MouseEventKind::ScrollUp | crossterm::event::MouseEventKind::ScrollDown))
        {
            let width = self.terminal_content_width();
            if !self.async_layout && self.prepared_conversation(width).is_none() {
                let prepared = crate::ui::transcript::prepare_conversation(self, width);
                self.install_conversation(prepared);
            }
        }
        let header_visible_before = crate::ui::header::visible(self);
        self.dirty = true;
        // A slow export writer only pauses its own chain: every update retries
        // the one parked record before reducing anything else.
        let mut commands = self.pump_export();
        commands.extend(match event {
            AppEvent::Bootstrap => self.bootstrap(),
            AppEvent::SubmitTurn { session_id, text } => self.submit_turn(session_id, text),
            AppEvent::SteerTurn { session_id, text } => self.steer_turn(&session_id, text),
            AppEvent::CreateSession {
                workspace,
                profile,
                model,
                reasoning,
                title,
            } => self.create_session(
                &workspace,
                profile.as_deref(),
                model.as_deref(),
                reasoning,
                title.as_deref(),
            ),
            AppEvent::OpenSession { session_id } => self.open_session(&session_id),
            AppEvent::CloseSession {
                session_id,
                confirm,
            } => self.close_session(&session_id, confirm),
            AppEvent::DeleteSession {
                session_id,
                confirm,
            } => self.delete_session(&session_id, confirm),
            AppEvent::CancelTurn { session_id } => self.cancel_turn(&session_id),
            AppEvent::RefreshTurn { session_id } => self.refresh_turn(&session_id),
            AppEvent::Reload => self.reload(),
            AppEvent::Rpc(event) => self.on_rpc_event(event),
            AppEvent::RpcChannelEnded => self.on_rpc_channel_ended(),
            AppEvent::RpcSendFailed { id, error } => self.on_send_failed(id, error),
            AppEvent::RpcQueueFull { request, class } => self.on_queue_full(request, class),
            AppEvent::ShutdownRequested => self.request_shutdown(),
            AppEvent::Tick => {
                let now = self.instant_now();
                if self
                    .scrollbar
                    .hide_at
                    .is_some_and(|deadline| deadline <= now)
                {
                    self.scrollbar.hide_at = None;
                }
                if self.spinner_active()
                    && self
                        .spinner_next_due
                        .is_some_and(|deadline| deadline <= now)
                {
                    self.frame_count = self.frame_count.wrapping_add(1);
                    self.spinner_next_due = Some(
                        now.checked_add(SPINNER_INTERVAL)
                            .expect("spinner deadline is representable"),
                    );
                }
                if self.ctrl_c_at.is_some_and(|pressed| {
                    self.instant_now().saturating_duration_since(pressed) >= DOUBLE_CTRL_C_WINDOW
                }) {
                    self.ctrl_c_at = None;
                }
                self.expire_notices();
                ui_actions::auto_scroll_selection(self);
                if self
                    .selection_copied_until
                    .is_some_and(|deadline| deadline <= self.instant_now())
                {
                    self.selection_copied_until = None;
                }
                self.poll_contexts()
            }
            AppEvent::Rendered => unreachable!("handled before the match"),
            AppEvent::SetTheme(kind) => {
                self.theme = kind;
                Vec::new()
            }
            AppEvent::ToggleReasoning => {
                self.reasoning_visible = !self.reasoning_visible;
                Vec::new()
            }
            AppEvent::ToggleTools { session_id } => {
                ui_actions::toggle_tools(self, &session_id);
                Vec::new()
            }
            AppEvent::ToggleTool {
                session_id,
                loop_id,
                request_index,
                tool_call_id,
            } => {
                ui_actions::toggle_tool(self, &session_id, &loop_id, request_index, &tool_call_id);
                Vec::new()
            }
            AppEvent::ToggleReasoningSection {
                session_id,
                loop_id,
                request_index,
                ordinal,
            } => {
                ui_actions::toggle_reasoning_section(
                    self,
                    &session_id,
                    &loop_id,
                    request_index,
                    ordinal,
                );
                Vec::new()
            }
            AppEvent::OpenNewSession => self.open_new_session(),
            AppEvent::OpenSessionSelector => self.open_selector(SelectorKind::Session),
            AppEvent::OpenModelSelector => self.open_selector(SelectorKind::Model),
            AppEvent::OpenReasoningSelector => self.open_selector(SelectorKind::Reasoning),
            AppEvent::OpenProfileSelector => self.open_selector(SelectorKind::Profile),
            AppEvent::SetSelectorQuery { query } => {
                if self.selector_state().is_some_and(|state| state.submitting) {
                    return Vec::new();
                }
                if self.session_selector_state().is_some() && self.session_panel_busy() {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query = query;
                    state.cursor = 0;
                } else if let Some(state) = self.session_selector_state_mut() {
                    if matches!(&state.mode, SessionPanelMode::Browse) {
                        state.query = query;
                        self.reconcile_session_selection(true);
                    }
                }
                Vec::new()
            }
            AppEvent::MoveSelector { delta } => self.move_selector(delta),
            AppEvent::PageSelector { delta } => self.page_selector(delta),
            AppEvent::ConfirmDock => self.confirm_dock(),
            AppEvent::CancelDock => self.cancel_dock(),
            AppEvent::DockFieldStep { delta } => self.dock_field_step(delta),
            AppEvent::NewSessionSetField { field, value } => {
                if let Some(draft) = self.draft_mut() {
                    // Frozen while a create is in flight.
                    if draft.submitting {
                        return Vec::new();
                    }
                    match field {
                        NewSessionField::Workspace => draft.workspace = value,
                        NewSessionField::Title => draft.title = value,
                        _ => {}
                    }
                }
                Vec::new()
            }
            AppEvent::SubmitNewSession => self.submit_new_session(),
            AppEvent::Terminal(event) => self.on_terminal(event),
            AppEvent::Viewport {
                total_lines,
                visible_rows,
            } => {
                self.viewport = (total_lines, visible_rows);
                if total_lines <= visible_rows {
                    ui_actions::cancel_scrollbar_drag(self);
                }
                self.clamp_transcript_scroll();
                Vec::new()
            }
            AppEvent::TerminalSize { width, height } => {
                self.terminal_size = (width, height);
                Vec::new()
            }
            AppEvent::JobFinished(outcome) => self.on_job_finished(outcome),
            AppEvent::ConversationPrepared(prepared) => {
                self.install_conversation(prepared);
                Vec::new()
            }
            AppEvent::FileLayoutPrepared(layout) => {
                self.install_file_layout(layout);
                Vec::new()
            }
            AppEvent::DiffLayoutPrepared(layout) => {
                self.install_diff_layout(layout);
                Vec::new()
            }
            AppEvent::ToolLayoutPrepared(layout) => {
                self.install_tool_layout(layout);
                Vec::new()
            }
            AppEvent::DurableLayoutPrepared(result) => {
                self.install_durable_layout(result);
                Vec::new()
            }
            AppEvent::HistoryItemDecoded(outcome) => self.on_history_item_decoded(*outcome),
            AppEvent::LocalScanFinished(outcome) => self.on_local_scan_finished(*outcome),
        });
        if header_visible_before != crate::ui::header::visible(self) {
            self.prepared_conversation = None;
        }
        if !self.scrollbar_allowed() {
            if self.scrollbar_drag.take().is_some() {
                self.mouse_down = None;
            }
            self.scrollbar = crate::ui::scrollbar::ScrollbarState::default();
        }
        // Central FIFO queue advance: after any event, at most one steer RPC
        // per session (or a fresh-turn fallback once a finished loop settles).
        // Do not advance on the event that starts or finishes a reload: the
        // reload itself is a barrier for new turn/steer work, and finishing
        // the staged candidate must not accidentally issue the queued item in
        // the same reducer pass.
        let reload_active_after = self.reload.is_some();
        let should_advance_steer_queue =
            !(reload_active_before || reload_active_after || reload_event);
        if should_advance_steer_queue {
            let advance = self.advance_steer_queues();
            commands.extend(advance);
        }
        // A coalesced/queued read waits until the response that freed the slot
        // has been fully handled, so it observes the newest cursor and never
        // overtakes that reducer pass.
        self.drain_query_followups(&mut commands);
        if self.context_panel().is_some_and(|c| {
            self.sessions.active.as_ref() != Some(&c.session)
                || self
                    .sessions
                    .known
                    .get(&c.session)
                    .is_none_or(|v| !v.info.loaded || v.session_epoch != c.epoch)
        }) {
            self.close_main_detail();
        }
        commands.extend(self.poll_tool_detail());
        commands.extend(self.poll_workspace());
        commands.extend(self.poll_changes());
        commands.extend(self.poll_workspace_status());
        // A newer confirmation can be parked behind a retired context read.
        // Resume due work when that read actually releases its slot, not only
        // on a later timer. The poll owner still enforces its real deadline.
        commands.extend(self.poll_contexts());
        // A queued scan page may have missed the slot that freed before its
        // follow-up drained; both chains retry idempotently while they need a
        // page and no read is in flight.
        let search_page = self.resume_idle_search_page();
        commands.extend(search_page);
        let export_page = self.resume_idle_export_page();
        commands.extend(export_page);
        if progress_signal {
            commands.extend(self.drain_rpc_retries());
        }
        self.sync_spinner_deadline();
        if let Some((was_dirty, before)) = scroll_visual_before {
            self.dirty = was_dirty || before != self.scroll_visual_state() || !commands.is_empty();
        }
        if let Some((was_dirty, deadline)) = idle_tick_before {
            self.dirty = was_dirty || deadline != self.scrollbar.hide_at || !commands.is_empty();
        }
        // Drop screen cells across identity/geometry barriers, even if the
        // user switches away and back before the next terminal draw.
        if !self.transcript_frame_matches() {
            self.transcript_frame = None;
        } else if !matches!(self.dock, Dock::Composer)
            || self.has_main_detail()
            || self.transcript_frame.as_ref().is_some_and(|saved| {
                self.active_view().is_none_or(|view| {
                    saved.scroll
                        != (
                            view.scroll.offset,
                            view.scroll.follow_tail,
                            view.scroll.prompt_cursor,
                        )
                })
            })
        {
            if let Some(saved) = self.transcript_frame.as_mut() {
                saved.tool_hits.clear();
            }
        }
        // Cache budgets are enforced once per event pass, off the draw path.
        self.enforce_history_budget();
        self.enforce_draft_budget();
        self.enforce_layout_budget();
        self.enforce_live_budget();
        self.enforce_tool_budget();
        commands
    }

    /// The composer that owns the next keystroke. While a session is active
    /// this is that session's draft; with no session it is the scratch draft.
    /// Switching sessions swaps the whole composer with the view (spec §10.3).
    pub fn composer(&self) -> &Composer {
        &self.composer
    }

    /// Mutable form of [`Self::composer`].
    pub fn composer_mut(&mut self) -> &mut Composer {
        &mut self.composer
    }

    /// Admission gate for new composer input (spec §21): while the retained
    /// all-drafts total is at budget, further typing/pasting is refused with
    /// one explicit warning instead of growing drafts without bound. Existing
    /// drafts are never truncated and never silently dropped.
    pub fn admit_draft_input(&mut self, additional: usize) -> bool {
        let budget = crate::limits::COMPOSER_ALL_DRAFTS_BYTES;
        if u64::try_from(self.draft_bytes().saturating_add(additional)).unwrap_or(u64::MAX)
            <= u64::try_from(budget).unwrap_or(u64::MAX)
        {
            return true;
        }
        if !self.draft_budget_warned {
            self.draft_budget_warned = true;
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "draft budget is full ({} MiB across sessions); input is refused until a draft is sent or discarded — existing drafts are kept",
                    budget / (1024 * 1024)
                ),
            );
        }
        false
    }

    /// Switches the displayed session, moving the entire composer (text,
    /// cursor, undo/redo, paste markers, revision) between the scratch owner
    /// and the session view, so every session keeps an independent draft and
    /// the view keeps its anchor/folds. Background sessions are neither
    /// closed nor cancelled.
    pub(crate) fn set_active_session(&mut self, next: Option<SessionId>) {
        if self.sessions.active == next {
            return;
        }
        // A foreground 500ms deadline must not follow its Session into the
        // background. Start the slower observation interval at this boundary.
        let background_due = self.instant_now() + Duration::from_secs(2);
        if let Some(poll) = self
            .sessions
            .active
            .as_ref()
            .and_then(|id| self.context_polls.get_mut(id))
        {
            poll.due = poll.due.max(background_due);
        }
        if let Some(current) = self.sessions.active.take() {
            match self.sessions.known.get_mut(&current) {
                Some(view) => std::mem::swap(&mut view.composer, &mut self.composer),
                // The view no longer exists (delete is an explicit discard).
                None => self.composer.clear(),
            }
        }
        self.sessions.active = next.clone();
        if let Some(id) = next {
            if let Some(view) = self.sessions.known.get_mut(&id) {
                std::mem::swap(&mut view.composer, &mut self.composer);
            }
        }
    }

    /// Retained draft bytes across every session composer plus the scratch
    /// draft. Undo/redo and paste markers count (spec §12.1, §21).
    pub fn draft_bytes(&self) -> usize {
        self.composer.retained_bytes()
            + self
                .sessions
                .known
                .values()
                .map(|view| view.composer.retained_bytes())
                .sum::<usize>()
    }

    /// The active session's view, for read-only render access.
    pub fn active_view(&self) -> Option<&SessionView> {
        self.sessions
            .active
            .as_deref()
            .and_then(|session_id| self.sessions.known.get(session_id))
    }

    /// The prepared conversation for the requested content width, when it
    /// still belongs to the active session and durable transcript revision.
    pub fn prepared_conversation(&self, width: u16) -> Option<&PreparedConversation> {
        let active = self.sessions.active.as_ref();
        let revision = active
            .and_then(|session_id| self.sessions.known.get(session_id))
            .map_or(0, |view| view.transcript.render_revision);
        self.prepared_conversation.as_ref().filter(|prepared| {
            prepared.width == width
                && prepared.session_id.as_ref() == active
                && prepared.transcript_revision == revision
                && prepared.durable.as_ref().is_none_or(|durable| {
                    self.active_view().is_some_and(|view| {
                        durable.key
                            == crate::state::view::DurableCacheKey::new(
                                view,
                                width,
                                self.theme,
                                self.reasoning_visible,
                            )
                    })
                })
        })
    }

    fn transcript_input_ready(&self) -> bool {
        let area = ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1);
        let screen = crate::ui::layout::screen_layout(self, area);
        self.prepared_conversation(screen.content.width).is_some()
            && self.transcript_frame_matches()
            && self.transcript_frame.as_ref().is_some_and(|saved| {
                saved.generation == self.prepared_generation
                    && saved.cells.area == screen.transcript
            })
    }

    fn transcript_frame_matches(&self) -> bool {
        self.transcript_frame.as_ref().is_some_and(|saved| {
            self.reload.is_none()
                && !matches!(
                    self.connection,
                    ConnectionState::Failed(_) | ConnectionState::ShuttingDown
                )
                && saved.terminal_size == self.terminal_size
                && saved.theme == self.theme
                && self.active_view().is_some_and(|view| {
                    saved.session_id == view.info.session_id
                        && saved.session_epoch == view.session_epoch
                })
        })
    }

    /// Only the bounded tool buttons of the frame actually on screen may
    /// remain interactive while replacement layout is pending. Their stable
    /// identities must still exist; no old row is interpreted as a new one.
    fn displayed_tool_hit(&self, column: u16, row: u16) -> Option<ToolKey> {
        if !matches!(self.dock, Dock::Composer)
            || self.has_main_detail()
            || !self.transcript_frame_matches()
        {
            return None;
        }
        let saved = self.transcript_frame.as_ref()?;
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        let view = self.active_view()?;
        if saved.cells.area != screen.transcript
            || saved.scroll
                != (
                    view.scroll.offset,
                    view.scroll.follow_tail,
                    view.scroll.prompt_cursor,
                )
        {
            return None;
        }
        let (_, key) = saved
            .tool_hits
            .iter()
            .find(|(hit, _)| hit.contains((column, row).into()))?;
        let present = view.tool_presentations.contains_key(key)
            || view.live.as_ref().is_some_and(|live| {
                live.reference.as_ref().is_some_and(|reference| {
                    reference.session_id == key.session_id && reference.loop_id == key.loop_id
                }) && live.requests.iter().any(|request| {
                    request.request_index == key.request_index
                        && request
                            .tools
                            .iter()
                            .any(|tool| tool.tool_call_id == key.tool_call_id)
                })
            })
            || view
                .transcript
                .blocks
                .iter()
                .any(|block| match block.as_ref() {
                    TranscriptBlock::Tool(tool) => {
                        tool.loop_id == key.loop_id
                            && tool.request_index == key.request_index
                            && tool.tool_call_id == key.tool_call_id
                    }
                    TranscriptBlock::Assistant(assistant) => {
                        assistant.loop_id == key.loop_id
                            && assistant.request_index == key.request_index
                            && assistant
                                .tool_calls
                                .iter()
                                .any(|tool| tool.tool_call_id == key.tool_call_id)
                    }
                    _ => false,
                });
        present.then(|| key.clone())
    }

    /// Capture only the actually displayed viewport after a successful draw.
    /// This bounded cell buffer cannot pin evicted transcript sections.
    pub fn remember_transcript_frame(&mut self, buffer: &ratatui::buffer::Buffer) {
        let screen = crate::ui::layout::screen_layout(self, buffer.area);
        if !self.async_layout
            || self.has_main_detail()
            || crate::ui::layout::is_too_small(buffer.area)
            || self.reload.is_some()
            || matches!(self.connection, ConnectionState::Failed(_))
        {
            self.transcript_frame = None;
            return;
        }
        let Some(prepared) = self.prepared_conversation(screen.content.width) else {
            return;
        };
        let Some(view) = self.active_view() else {
            self.transcript_frame = None;
            return;
        };
        let position = crate::ui::transcript::scroll_position(
            self,
            prepared.total_rows(),
            screen.transcript.height as usize,
        );
        let tool_hits = if matches!(self.dock, Dock::Composer) {
            crate::ui::tool_detail::detail_hits(
                prepared,
                screen.transcript,
                position.offset,
                position.visible_rows,
            )
        } else {
            Vec::new()
        };
        let mut cells = ratatui::buffer::Buffer::empty(screen.transcript);
        for y in screen.transcript.y..screen.transcript.bottom() {
            for x in screen.transcript.x..screen.transcript.right() {
                cells[(x, y)] = buffer[(x, y)].clone();
            }
        }
        self.transcript_frame = Some(TranscriptFrame {
            generation: self.prepared_generation,
            session_id: view.info.session_id.clone(),
            session_epoch: view.session_epoch,
            theme: self.theme,
            terminal_size: self.terminal_size,
            scroll: (
                view.scroll.offset,
                view.scroll.follow_tail,
                view.scroll.prompt_cursor,
            ),
            tool_hits,
            cells,
        });
    }

    /// Retained cells; only their separately saved exact tool buttons may
    /// accept input without a current layout.
    pub fn transition_transcript_frame(
        &self,
        area: ratatui::layout::Rect,
    ) -> Option<&ratatui::buffer::Buffer> {
        if !self.transcript_frame_matches() {
            return None;
        }
        self.transcript_frame
            .as_ref()
            .map(|saved| &saved.cells)
            .filter(|cells| {
                cells.area.x == area.x && cells.area.y == area.y && cells.area.width == area.width
            })
    }

    pub fn cached_durable(&self, width: u16) -> Option<Arc<PreparedDurable>> {
        let view = self.active_view()?;
        let key = crate::state::view::DurableCacheKey::new(
            view,
            width,
            self.theme,
            self.reasoning_visible,
        );
        view.transcript
            .render_cache
            .as_ref()
            .filter(|durable| durable.key == key)
            .cloned()
    }

    /// Constructs the app with already-resolved local preferences. Startup
    /// applies these before Bootstrap so the first frame uses the same theme
    /// and visibility choices as later settings changes.
    pub fn with_tui_config(
        default_workspace: PathBuf,
        config_path: PathBuf,
        config: crate::config::TuiConfig,
    ) -> Self {
        let mut app = Self::new(default_workspace);
        app.config_path = config_path;
        app.apply_tui_config(config);
        app
    }

    pub fn apply_tui_config(&mut self, config: crate::config::TuiConfig) {
        self.theme = config.theme;
        self.reasoning_visible = config.thinking_visible;
        for view in self.sessions.known.values_mut() {
            view.tools_expanded = config.tools_expanded;
        }
        self.tui_config = config;
        self.prepared_conversation = None;
        self.dirty = true;
    }

    fn new_session_view(&self, info: SessionInfo) -> SessionView {
        let mut view = SessionView::new(info);
        view.tools_expanded = self.tui_config.tools_expanded;
        view
    }

    pub fn enable_async_layout(&mut self) {
        self.async_layout = true;
        self.prepared_conversation = None;
        self.layout_pending = None;
        self.layout_partial = None;
    }

    pub fn async_layout_enabled(&self) -> bool {
        self.async_layout
    }

    /// Enables the production serialized JSON decode hand-off. The main loop
    /// calls this before it starts consuming Agent frames; tests may leave it
    /// disabled and use the deterministic synchronous compatibility drain.
    pub fn enable_async_decode(&mut self) {
        self.async_decode = true;
        self.pending_decode = None;
        self.decode_in_flight = None;
    }

    pub fn async_decode_enabled(&self) -> bool {
        self.async_decode
    }

    /// The main loop passes the one retained encoded item to `LocalJobs`.
    /// Cloning this handle only bumps the `Arc<str>` count; it never copies
    /// the JSON body.
    pub fn pending_decode_request(&self) -> Option<DecodeRequest> {
        self.pending_decode.clone()
    }

    /// Marks the pending request as owned by the decode worker after its
    /// bounded queue accepts it.
    pub fn mark_decode_scheduled(&mut self) {
        self.pending_decode = None;
    }

    pub fn layout_request(&mut self, width: u16) -> Option<DurableLayoutRequest> {
        let (session_id, transcript_revision, snapshot, previous, live_tool_keys) = {
            let view = self.active_view()?;
            let live_tool_keys = crate::state::view::live_tool_keys(view);
            if self.layout_pending.as_ref().is_some_and(|pending| {
                pending.session_id == view.info.session_id
                    && pending.session_epoch == view.session_epoch
                    && pending.transcript_revision == view.transcript.render_revision
                    && pending.width == width
                    && pending.theme == self.theme
                    && pending.reasoning_visible == self.reasoning_visible
                    && pending.live_tool_keys.as_ref() == live_tool_keys.as_ref()
            }) {
                return None;
            }
            if view
                .transcript
                .render_cache
                .as_ref()
                .is_some_and(|durable| {
                    durable.key
                        == crate::state::view::DurableCacheKey::new(
                            view,
                            width,
                            self.theme,
                            self.reasoning_visible,
                        )
                })
            {
                return None;
            }
            (
                view.info.session_id.clone(),
                view.transcript.render_revision,
                DurableLayoutSnapshot::from_view(view),
                view.transcript.render_cache.clone(),
                live_tool_keys,
            )
        };
        self.next_layout_generation = self.next_layout_generation.wrapping_add(1);
        let identity = DurableLayoutIdentity {
            generation: self.next_layout_generation,
            session_id,
            session_epoch: snapshot.session_epoch,
            transcript_revision,
            width,
            theme: self.theme,
            reasoning_visible: self.reasoning_visible,
            live_tool_keys,
        };
        Some(DurableLayoutRequest {
            identity,
            snapshot,
            previous,
            viewport: self.viewport.0..self.viewport.0.saturating_add(self.viewport.1),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    pub fn mark_layout_pending(&mut self, identity: DurableLayoutIdentity) {
        self.layout_pending = Some(identity);
        self.layout_partial = None;
    }

    pub fn clear_layout_pending(&mut self) {
        self.layout_pending = None;
        self.layout_partial = None;
    }

    fn install_durable_layout(&mut self, result: DurableLayoutResult) {
        if self.layout_pending.as_ref() != Some(&result.identity) {
            return;
        }
        let Some(view) = self.active_view() else {
            return;
        };
        if view.info.session_id != result.identity.session_id
            || view.session_epoch != result.identity.session_epoch
            || view.transcript.render_revision != result.identity.transcript_revision
            || self.theme != result.identity.theme
            || self.reasoning_visible != result.identity.reasoning_visible
            || crate::state::view::live_tool_keys(view).as_ref()
                != result.identity.live_tool_keys.as_ref()
        {
            return;
        }
        if !result.complete {
            let mut sections = self
                .layout_partial
                .as_ref()
                .filter(|(identity, _)| identity == &result.identity)
                .map(|(_, layout)| {
                    layout
                        .sections
                        .iter()
                        .map(|placement| Arc::clone(&placement.layout))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            sections.extend(
                result
                    .durable
                    .layout
                    .sections
                    .iter()
                    .map(|placement| Arc::clone(&placement.layout)),
            );
            self.layout_partial = Some((
                result.identity,
                Arc::new(ConversationLayout::from_sections(sections)),
            ));
            return;
        }
        self.layout_pending = None;
        let durable = self
            .layout_partial
            .take()
            .filter(|(identity, _)| identity == &result.identity)
            .map(|(_, layout)| {
                Arc::new(PreparedDurable {
                    key: result.durable.key.clone(),
                    layout,
                })
            })
            .unwrap_or(result.durable);
        crate::perf::add(
            crate::perf::Counter::LayoutCalls,
            result.changed_sections as u64,
        );
        crate::perf::add(
            crate::perf::Counter::ToolIndexLookups,
            result.tool_index_lookups as u64,
        );
        let prepared = crate::ui::transcript::prepare_conversation_from_cache(
            self,
            result.identity.width,
            durable,
        );
        self.install_conversation(prepared);
    }

    pub fn selection_copied(&self) -> bool {
        self.selection_copied_until
            .is_some_and(|deadline| self.instant_now() < deadline)
    }

    pub(crate) fn conversation_for_input(
        &self,
        width: u16,
    ) -> std::borrow::Cow<'_, PreparedConversation> {
        self.prepared_conversation(width)
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| {
                if self.async_layout {
                    if let Some(durable) = self.cached_durable(width) {
                        std::borrow::Cow::Owned(
                            crate::ui::transcript::prepare_conversation_from_cache(
                                self, width, durable,
                            ),
                        )
                    } else {
                        std::borrow::Cow::Owned(PreparedConversation::placeholder(
                            self.active_view(),
                            width,
                        ))
                    }
                } else {
                    std::borrow::Cow::Owned(crate::ui::transcript::prepare_conversation(
                        self, width,
                    ))
                }
            })
    }

    pub fn scrollbar_preview_offset(&self, session_id: &str) -> Option<usize> {
        self.scrollbar_drag
            .as_ref()
            .filter(|drag| drag.session_id == session_id)
            .and_then(|_| self.active_view())
            .map(|view| {
                if view.scroll.follow_tail {
                    self.viewport.0.saturating_sub(self.viewport.1)
                } else {
                    view.scroll.offset
                }
            })
    }

    pub(crate) fn scrollbar_visible(&self, total: usize, height: usize) -> bool {
        self.scrollbar_allowed()
            && self.active_view().is_some()
            && total > height
            && height > 0
            && self.scrollbar.visible(self.instant_now())
    }

    pub(crate) fn scrollbar_active(&self) -> bool {
        self.scrollbar.active
    }

    fn scrollbar_allowed(&self) -> bool {
        matches!(
            self.dock,
            Dock::Composer | Dock::Help | Dock::Logs | Dock::Search(_)
        )
    }

    fn scroll_visual_state(&self) -> Option<(usize, bool, bool, bool, bool)> {
        self.active_view().map(|view| {
            (
                view.scroll.offset,
                view.scroll.follow_tail,
                view.scroll.new_content,
                self.scrollbar_visible(self.viewport.0, self.viewport.1),
                self.scrollbar.active,
            )
        })
    }

    fn mark_scrollbar_activity(&mut self, total: usize, visible: usize) {
        if total > visible {
            self.scrollbar.activity(self.instant_now());
        }
    }

    pub(crate) fn install_conversation(&mut self, prepared: PreparedConversation) {
        self.capture_scroll_anchor();
        let active = self.sessions.active.as_ref();
        let revision = active
            .and_then(|session_id| self.sessions.known.get(session_id))
            .map_or(0, |view| view.transcript.render_revision);
        if prepared.session_id.as_ref() != active || prepared.transcript_revision != revision {
            return;
        }
        if let Some(durable) = &prepared.durable {
            if let Some(view) = self
                .sessions
                .active
                .as_ref()
                .and_then(|id| self.sessions.known.get_mut(id))
            {
                if durable.key
                    != crate::state::view::DurableCacheKey::new(
                        view,
                        prepared.width,
                        self.theme,
                        self.reasoning_visible,
                    )
                {
                    return;
                }
                view.transcript.render_cache = Some(Arc::clone(durable));
            }
        }
        self.restore_scroll_anchor(&prepared);
        self.rebase_selection(&prepared);
        self.prepared_conversation = Some(prepared);
        self.prepared_generation = self.prepared_generation.wrapping_add(1);
    }

    /// Captures the first retained content row currently visible. The anchor
    /// is intentionally content-based, not an absolute row: wrapping,
    /// folding, prepending an earlier page, and live-to-history replacement
    /// may all change rows before it.
    fn capture_scroll_anchor(&mut self) {
        let Some(prepared) = self.prepared_conversation.as_ref() else {
            return;
        };
        let Some(view) = self.active_view() else {
            return;
        };
        if prepared.session_id.as_deref() != Some(view.info.session_id.as_str()) {
            return;
        }
        if view.scroll.follow_tail {
            if let Some(view) = self.active_session_mut() {
                view.scroll.anchor = None;
            }
            return;
        }
        let height = self.viewport.1.max(1);
        let position = crate::ui::transcript::scroll_position(self, prepared.total_rows(), height);
        let start = position.offset;
        let end = start
            .saturating_add(position.visible_rows)
            .min(prepared.total_rows());
        let anchor = (start..end).find_map(|row| {
            let section = prepared
                .sections
                .iter()
                .find(|section| section.rows.contains(&row))?;
            let copy = prepared.copy_row(row);
            if copy.is_some_and(|copy| copy.decorative) {
                return None;
            }
            Some(ScrollAnchor {
                section_id: section.id,
                source_offset: copy.map_or(0, |copy| copy.source_offset),
                screen_row: row.saturating_sub(start),
            })
        });
        if let Some(anchor) = anchor {
            if let Some(view) = self.active_session_mut() {
                view.scroll.anchor = Some(anchor);
            }
        }
    }

    fn restore_scroll_anchor(&mut self, prepared: &PreparedConversation) {
        let Some(anchor) = self
            .active_view()
            .and_then(|view| view.scroll.anchor.clone())
        else {
            return;
        };
        let Some(row) = prepared.row_for_scroll_anchor(&anchor) else {
            return;
        };
        let retained = prepared.has_scroll_anchor_section(&anchor);
        let visible = self.viewport.1.max(1);
        let max_offset = prepared.total_rows().saturating_sub(visible);
        let offset = row.saturating_sub(anchor.screen_row).min(max_offset);
        let fallback_anchor = (!retained).then(|| {
            prepared
                .sections
                .iter()
                .find(|section| section.rows.contains(&row))
                .map(|section| ScrollAnchor {
                    section_id: section.id.clone(),
                    source_offset: prepared.copy_row(row).map_or(0, |copy| copy.source_offset),
                    screen_row: anchor.screen_row,
                })
        });
        if !retained {
            self.notice(
                NoticeLevel::Info,
                "original scroll range is not loaded; showing the nearest retained content",
            );
        }
        if let Some(view) = self.active_session_mut() {
            view.scroll.follow_tail = false;
            view.scroll.offset = offset;
            // Reflow restores the viewport, not the user's read position.
            // Keep unread output sticky until scrolling returns to the tail.
            if let Some(Some(fallback_anchor)) = fallback_anchor {
                view.scroll.anchor = Some(fallback_anchor);
            }
        }
    }

    fn rebase_selection(&mut self, prepared: &PreparedConversation) {
        if self.selection.as_ref().is_some_and(|selection| {
            prepared.session_id.as_deref() != Some(selection.session_id.as_str())
        }) {
            self.selection = None;
            return;
        }
        let Some(selection) = self.selection.as_mut() else {
            return;
        };
        let mut valid = true;
        for point in [&mut selection.anchor, &mut selection.focus] {
            let Some(section_id) = point.section_id.as_ref() else {
                continue;
            };
            if let Some(section) = prepared
                .sections
                .iter()
                .find(|section| section_ids_match(&section.id, section_id))
            {
                point.section_row = point.section_row.min(section.rows.len().saturating_sub(1));
                point.row = section.rows.start + point.section_row;
                if section.content_columns.is_empty() {
                    point.column = 0;
                } else {
                    point.column = point
                        .column
                        .max(section.content_columns.start)
                        .min(section.content_columns.end - 1);
                }
            } else {
                valid = false;
            }
        }
        if !valid {
            self.selection = None;
        }
    }

    /// The current new-session draft, whether the form or a selector is
    /// showing it (read-only).
    pub fn new_session(&self) -> Option<&NewSessionState> {
        self.draft.as_ref().or(match &self.dock {
            Dock::NewSession(draft) => Some(draft),
            _ => None,
        })
    }

    fn session_is_visible(&self, session_id: &str) -> bool {
        !self.sessions.pending_deletes.contains(session_id)
            && !self.session_absent(session_id)
            && self
                .sessions
                .list
                .iter()
                .any(|session| session.session_id == session_id)
    }

    fn session_is_filtered_visible(&self, session_id: &str) -> bool {
        let query = self
            .session_selector_state()
            .map(|state| state.query.clone())
            .unwrap_or_default();
        self.filtered_session_items(&query)
            .iter()
            .any(|session| session.session_id == session_id)
    }

    /// The selector's visible rows for one scope, newest first. `Current`
    /// lists only the app's workspace; `All` is an explicit toggle and never
    /// the default (spec §10.2).
    pub fn session_panel_items(
        &self,
        query: &str,
        scope: crate::state::selection::SessionScope,
    ) -> Vec<&SessionInfo> {
        let workspace = self
            .catalogs
            .default_workspace
            .to_string_lossy()
            .into_owned();
        filtered_sessions(&self.sessions.list, query)
            .into_iter()
            .filter(|session| self.session_is_visible(&session.session_id))
            .filter(|session| {
                matches!(scope, crate::state::selection::SessionScope::All)
                    || session.workspace == workspace
            })
            .collect()
    }

    fn filtered_session_items(&self, query: &str) -> Vec<&SessionInfo> {
        let scope = self
            .session_selector_state()
            .map_or(crate::state::selection::SessionScope::default(), |state| {
                state.scope
            });
        self.session_panel_items(query, scope)
    }

    fn session_selector_cursor(&self, state: &SessionSelectorState) -> usize {
        let items = self.filtered_session_items(&state.query);
        state
            .selected_session_id
            .as_deref()
            .and_then(|selected| {
                items
                    .iter()
                    .position(|session| session.session_id == selected)
            })
            .unwrap_or(0)
    }

    fn session_panel_busy(&self) -> bool {
        if self.reload.is_some() {
            return true;
        }
        self.pending_requests.values().any(|request| {
            matches!(
                request,
                RequestKind::CreateSession { .. }
                    | RequestKind::OpenSession { .. }
                    | RequestKind::RefreshSessions { .. }
                    | RequestKind::RenameSession { .. }
                    | RequestKind::CloseSession { .. }
                    | RequestKind::CloseVerifyState { .. }
                    | RequestKind::DeleteSession { .. }
            )
        })
    }

    fn is_lifecycle_request(kind: &RequestKind) -> bool {
        matches!(
            kind,
            RequestKind::CreateSession { .. }
                | RequestKind::OpenSession { .. }
                | RequestKind::RenameSession { .. }
                | RequestKind::CloseSession { .. }
                | RequestKind::DeleteSession { .. }
        )
    }

    /// A catalog generation is a barrier for FIFO admission until it settles;
    /// the next ordinary event may resume the queue.
    fn is_reload_request(kind: &RequestKind) -> bool {
        matches!(
            kind,
            RequestKind::StaleRead
                | RequestKind::Reload { .. }
                | RequestKind::ReloadModels { .. }
                | RequestKind::ReloadProfiles { .. }
                | RequestKind::ReloadSessions { .. }
        )
    }

    fn is_reload_event(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::Reload => true,
            AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(response))) => self
                .pending_requests
                .get(&response.id)
                .is_some_and(Self::is_reload_request),
            AppEvent::RpcSendFailed { id, .. } => self
                .pending_requests
                .get(id)
                .is_some_and(Self::is_reload_request),
            AppEvent::Rpc(_) => self.reload.is_some(),
            _ => false,
        }
    }

    fn has_pending_lifecycle_request(&self) -> bool {
        self.pending_requests
            .values()
            .any(Self::is_lifecycle_request)
    }

    fn pending_lifecycle_session_ids(&self) -> HashSet<SessionId> {
        self.pending_requests
            .values()
            .filter_map(|kind| match kind {
                RequestKind::OpenSession { session_id, .. }
                | RequestKind::RenameSession { session_id }
                | RequestKind::CloseSession { session_id }
                | RequestKind::DeleteSession { session_id } => Some(session_id.clone()),
                RequestKind::CreateSession { .. } => None,
                _ => None,
            })
            .collect()
    }

    /// Reconciles Browse selection against the current lifecycle-visible
    /// query result, choosing its first item when requested. Other panel
    /// modes retain their stable dialog target.
    fn reconcile_session_selection(&mut self, choose_first_when_empty: bool) {
        let Some(state) = self.session_selector_state() else {
            return;
        };
        if !matches!(&state.mode, SessionPanelMode::Browse) {
            return;
        }
        let query = state.query.clone();
        let selected = state.selected_session_id.clone();
        let filtered = self.filtered_session_items(&query);
        let next = match selected {
            Some(id) if filtered.iter().any(|session| session.session_id == id) => Some(id),
            _ if choose_first_when_empty => {
                filtered.first().map(|session| session.session_id.clone())
            }
            _ => None,
        };
        if let Some(state) = self.session_selector_state_mut() {
            state.selected_session_id = next;
        }
    }

    // ---- dock & selectors (spec 24-28) -------------------------------

    fn make_new_session_draft(&mut self) -> NewSessionState {
        let draft_id = self.next_draft_id;
        self.next_draft_id = self
            .next_draft_id
            .checked_add(1)
            .expect("draft ids exhausted");
        // Workspace is a plain string the agent validates (spec 25.4).
        let workspace = self
            .catalogs
            .default_workspace
            .to_string_lossy()
            .into_owned();
        let workspace_len = workspace.chars().count();
        NewSessionState {
            workspace,
            profile: self.catalogs.next_profile.clone().unwrap_or_default(),
            model: self
                .catalogs
                .next_model
                .clone()
                .or_else(|| self.catalogs.models.first().map(|model| model.id.clone()))
                .unwrap_or_default(),
            reasoning: self.catalogs.next_reasoning.unwrap_or(Reasoning::Auto),
            title: String::new(),
            field: NewSessionField::Workspace,
            submitting: false,
            error: None,
            field_cursor: workspace_len,
            draft_id,
        }
    }

    /// A fresh draft in the form; the catalog seats are snapshots only, so
    /// the active session is never touched (spec 25.2).
    fn open_new_session(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let draft = self.make_new_session_draft();
        self.draft = None;
        self.dock = Dock::NewSession(draft);
        Vec::new()
    }

    /// The draft while it exists: the form in the dock, or the detached
    /// copy under a model/reasoning/profile selector.
    fn draft_mut(&mut self) -> Option<&mut NewSessionState> {
        if let Some(draft) = &mut self.draft {
            return Some(draft);
        }
        if let Dock::NewSession(draft) = &mut self.dock {
            return Some(draft);
        }
        None
    }

    fn draft_matching(&mut self, draft_id: u64) -> Option<&mut NewSessionState> {
        match &mut self.draft {
            Some(draft) if draft.draft_id == draft_id => return Some(draft),
            _ => {}
        }
        if let Dock::NewSession(draft) = &mut self.dock {
            if draft.draft_id == draft_id {
                return Some(draft);
            }
        }
        None
    }

    fn selector_state(&self) -> Option<&SelectorState> {
        match &self.dock {
            Dock::ModelSelector(state)
            | Dock::ReasoningSelector(state)
            | Dock::ProfileSelector(state) => Some(state),
            _ => None,
        }
    }

    fn session_selector_state(&self) -> Option<&SessionSelectorState> {
        match &self.dock {
            Dock::SessionSelector(state) => Some(state),
            _ => None,
        }
    }

    fn set_selector_submitting(&mut self) {
        if let Some(state) = self.selector_state_mut() {
            state.submitting = true;
            state.error = None;
        }
    }

    fn selector_state_mut(&mut self) -> Option<&mut SelectorState> {
        match &mut self.dock {
            Dock::ModelSelector(state)
            | Dock::ReasoningSelector(state)
            | Dock::ProfileSelector(state) => Some(state),
            _ => None,
        }
    }

    fn session_selector_state_mut(&mut self) -> Option<&mut SessionSelectorState> {
        match &mut self.dock {
            Dock::SessionSelector(state) => Some(state),
            _ => None,
        }
    }

    /// Opens `kind` and pre-selects the form draft or active session value.
    /// A selector opened from a new-session form retains that exact draft;
    /// its selection must never leak into the active session (spec 26.4).
    fn open_selector(&mut self, kind: SelectorKind) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.selector_state().is_some_and(|state| state.submitting) {
            return Vec::new();
        }
        // While a create is in flight the draft is frozen: opening a
        // model/reasoning/profile selector, or confirming one from a field,
        // must not leave the form or mutate the drafting session (spec
        // 25.5). The session selector is unrelated and stays available.
        if kind != SelectorKind::Session && self.new_session().is_some_and(|draft| draft.submitting)
        {
            return Vec::new();
        }
        if matches!(kind, SelectorKind::Model | SelectorKind::Reasoning) {
            let active = self.sessions.active.as_ref();
            if active.is_some_and(|session_id| {
                self.pending_requests.values().any(|request| {
                    matches!(
                        request,
                        RequestKind::UpdateSession {
                            session_id: pending_session,
                            ..
                        } if pending_session == session_id
                    )
                })
            }) {
                return Vec::new();
            }
        }
        if kind == SelectorKind::Session {
            let selected = self
                .sessions
                .active
                .clone()
                .filter(|id| self.session_is_visible(id))
                .or_else(|| {
                    self.filtered_session_items("")
                        .first()
                        .map(|session| session.session_id.clone())
                });
            self.dock = Dock::SessionSelector(SessionSelectorState::new(selected));
            return self.refresh_sessions();
        }
        let mut state = SelectorState::new(kind);
        if kind != SelectorKind::Session {
            // A model/reasoning selector edits a draft when the form is open;
            // otherwise it updates the active Session through session.update.
            if matches!(self.dock, Dock::NewSession(_))
                || self.sessions.active.is_none()
                || kind == SelectorKind::Profile
            {
                self.ensure_new_session_draft();
            }
            let model = self
                .new_session()
                .map(|draft| draft.model.clone())
                .or_else(|| self.active_view().map(|view| view.info.model.clone()))
                .unwrap_or_default();
            let profile = self
                .new_session()
                .map(|draft| draft.profile.clone())
                .unwrap_or_default();
            let reasoning = self
                .new_session()
                .map(|draft| draft.reasoning)
                .or_else(|| self.active_view().map(|view| view.info.reasoning));
            state.cursor = match kind {
                SelectorKind::Model => filtered_models(&self.catalogs.models, "")
                    .iter()
                    .position(|candidate| candidate.id == model)
                    .unwrap_or(0),
                SelectorKind::Profile => filtered_profiles(&self.catalogs.profiles, "")
                    .iter()
                    .position(|candidate| candidate.id == profile)
                    .unwrap_or(0),
                SelectorKind::Reasoning => supported_reasoning(&self.catalogs.models, &model)
                    .iter()
                    .position(|level| Some(*level) == reasoning)
                    .unwrap_or(0),
                SelectorKind::Session => 0,
            };
        }
        self.dock = match kind {
            SelectorKind::Session => unreachable!("session selector handled above"),
            SelectorKind::Model => Dock::ModelSelector(state),
            SelectorKind::Reasoning => Dock::ReasoningSelector(state),
            SelectorKind::Profile => Dock::ProfileSelector(state),
        };
        Vec::new()
    }

    fn ensure_new_session_draft(&mut self) {
        if self.draft.is_some() {
            return;
        }
        let draft = match &self.dock {
            Dock::NewSession(draft) => draft.clone(),
            _ => self.make_new_session_draft(),
        };
        self.draft = Some(draft);
    }

    fn move_selector(&mut self, delta: i32) -> Vec<AppCommand> {
        if self.session_selector_state().is_some() && self.session_panel_busy() {
            return Vec::new();
        }
        if let Some((query, cursor, editable)) = self.session_selector_state().map(|state| {
            (
                state.query.clone(),
                self.session_selector_cursor(state),
                matches!(&state.mode, SessionPanelMode::Browse),
            )
        }) {
            if !editable {
                return Vec::new();
            }
            let items = self.filtered_session_items(&query);
            if items.is_empty() {
                return Vec::new();
            }
            let next = (cursor as i64 + delta as i64).rem_euclid(items.len() as i64) as usize;
            let next_id = items[next].session_id.clone();
            if let Some(state) = self.session_selector_state_mut() {
                state.selected_session_id = Some(next_id);
            }
            return Vec::new();
        }
        let (kind, query, cursor, model_context) = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.submitting {
                return Vec::new();
            }
            (
                state.kind,
                state.query.clone(),
                state.cursor,
                state.model_context.clone(),
            )
        };
        let count = self.selector_count(kind, &query, model_context.as_deref());
        if count == 0 {
            return Vec::new();
        }
        if let Some(state) = self.selector_state_mut() {
            state.cursor = (cursor as i64 + delta as i64).rem_euclid(count as i64) as usize;
        }
        Vec::new()
    }

    fn page_selector(&mut self, delta: i32) -> Vec<AppCommand> {
        if self.session_selector_state().is_some() && self.session_panel_busy() {
            return Vec::new();
        }
        let step = self.selector_page_step();
        if let Some((query, cursor, editable)) = self.session_selector_state().map(|state| {
            (
                state.query.clone(),
                self.session_selector_cursor(state),
                matches!(&state.mode, SessionPanelMode::Browse),
            )
        }) {
            if !editable {
                return Vec::new();
            }
            let count = self.filtered_session_items(&query).len();
            if count == 0 {
                return Vec::new();
            }
            let next = (cursor as i64 + delta as i64 * step as i64)
                .clamp(0, count.saturating_sub(1) as i64) as usize;
            let next_id = self
                .filtered_session_items(&query)
                .get(next)
                .map(|session| session.session_id.clone());
            if let Some(state) = self.session_selector_state_mut() {
                state.selected_session_id = next_id;
            }
            return Vec::new();
        }
        let (kind, query, cursor, model_context) = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.submitting {
                return Vec::new();
            }
            (
                state.kind,
                state.query.clone(),
                state.cursor,
                state.model_context.clone(),
            )
        };
        let count = self.selector_count(kind, &query, model_context.as_deref());
        if count == 0 {
            return Vec::new();
        }
        if let Some(state) = self.selector_state_mut() {
            state.cursor = (cursor as i64 + delta as i64 * step as i64)
                .clamp(0, count.saturating_sub(1) as i64) as usize;
        }
        Vec::new()
    }

    fn selector_page_step(&self) -> usize {
        let area = ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1);
        let screen = crate::ui::layout::screen_layout(self, area);
        match &self.dock {
            Dock::SessionSelector(state) => {
                let items = self.filtered_session_items(&state.query);
                let wide = screen.panel.width >= 70;
                let heights = vec![usize::from(wide) + 1; items.len()];
                crate::ui::panel::visible_window(
                    &heights,
                    self.session_selector_cursor(state),
                    crate::ui::selector::session_panel_layout(screen.panel, state)
                        .content
                        .height as usize,
                )
                .len()
                .max(1)
            }
            Dock::ModelSelector(state)
            | Dock::ReasoningSelector(state)
            | Dock::ProfileSelector(state) => {
                crate::ui::selector::catalog_visible_window(self, screen.panel, state)
                    .len()
                    .max(1)
            }
            _ => 1,
        }
    }

    fn selector_count(
        &self,
        kind: SelectorKind,
        query: &str,
        model_context: Option<&str>,
    ) -> usize {
        match kind {
            SelectorKind::Model => filtered_models(&self.catalogs.models, query).len(),
            SelectorKind::Profile => filtered_profiles(&self.catalogs.profiles, query).len(),
            SelectorKind::Reasoning => {
                let model = self
                    .new_session()
                    .map(|draft| draft.model.clone())
                    .or_else(|| model_context.map(str::to_owned))
                    .or_else(|| self.active_view().map(|view| view.info.model.clone()))
                    .unwrap_or_default();
                supported_reasoning(&self.catalogs.models, &model).len()
            }
            SelectorKind::Session => filtered_sessions(&self.sessions.list, query).len(),
        }
    }

    fn confirm_dock(&mut self) -> Vec<AppCommand> {
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        enum Target {
            Composer,
            NewSession(NewSessionField),
            SessionSelector,
            ModelSelector,
            ReasoningSelector,
            ProfileSelector,
        }
        let target = match &self.dock {
            Dock::Composer => Target::Composer,
            Dock::NewSession(draft) => Target::NewSession(draft.field),
            Dock::SessionSelector(_) => Target::SessionSelector,
            Dock::ModelSelector(_) => Target::ModelSelector,
            Dock::ReasoningSelector(_) => Target::ReasoningSelector,
            Dock::ProfileSelector(_) => Target::ProfileSelector,
            Dock::Help | Dock::Logs | Dock::Search(_) => Target::Composer,
            Dock::Export(_) | Dock::Settings(_) | Dock::Workspace(_) => Target::Composer,
        };
        match target {
            Target::Composer => Vec::new(),
            Target::SessionSelector => self.session_panel_confirm(),
            Target::ModelSelector => self.confirm_model_item(),
            Target::ReasoningSelector => self.confirm_reasoning_item(),
            Target::ProfileSelector => self.confirm_profile_item(),
            Target::NewSession(field) => match field {
                NewSessionField::Profile => self.open_selector(SelectorKind::Profile),
                NewSessionField::Model => self.open_selector(SelectorKind::Model),
                NewSessionField::Reasoning => self.open_selector(SelectorKind::Reasoning),
                NewSessionField::Create => self.submit_new_session(),
                NewSessionField::Workspace | NewSessionField::Title => Vec::new(),
            },
        }
    }

    fn cancel_dock(&mut self) -> Vec<AppCommand> {
        if self.workspace_browser().is_some() {
            self.close_workspace_browser();
            return Vec::new();
        }
        if self.selector_state().is_some_and(|state| state.submitting) {
            return Vec::new();
        }
        if self.session_selector_state().is_some() {
            return self.session_panel_cancel();
        }
        enum Target {
            Composer,
            SessionSelector,
            NewSession,
            Form,
            Panel,
            Search,
            Export,
            Settings,
        }
        let target = match &self.dock {
            Dock::Composer => Target::Composer,
            Dock::SessionSelector(_) => Target::SessionSelector,
            Dock::NewSession(_) => Target::NewSession,
            Dock::ModelSelector(_) | Dock::ReasoningSelector(_) | Dock::ProfileSelector(_) => {
                Target::Form
            }
            Dock::Help | Dock::Logs => Target::Panel,
            Dock::Search(_) => Target::Search,
            Dock::Export(_) => Target::Export,
            Dock::Settings(_) => Target::Settings,
            Dock::Workspace(_) => Target::Panel,
        };
        match target {
            Target::Composer => {}
            Target::Search => self.close_search(),
            Target::Export => {
                self.export_escape();
            }
            Target::Settings => {
                self.settings_escape();
            }
            Target::SessionSelector => self.dock = Dock::Composer,
            Target::NewSession => {
                self.draft = None;
                self.dock = Dock::Composer;
            }
            Target::Form => {
                if self.draft.is_some() {
                    self.close_selector_to_form();
                } else {
                    self.dock = Dock::Composer;
                }
            }
            Target::Panel => self.dock = Dock::Composer,
        }
        Vec::new()
    }

    fn close_selector_to_form(&mut self) {
        if let Some(draft) = self.draft.take() {
            self.dock = Dock::NewSession(draft);
        }
    }

    fn dock_field_step(&mut self, delta: i32) -> Vec<AppCommand> {
        const FIELDS: [NewSessionField; 6] = [
            NewSessionField::Workspace,
            NewSessionField::Profile,
            NewSessionField::Model,
            NewSessionField::Reasoning,
            NewSessionField::Title,
            NewSessionField::Create,
        ];
        if let Dock::NewSession(draft) = &mut self.dock {
            if draft.submitting {
                return Vec::new();
            }
            let current = FIELDS
                .iter()
                .position(|field| *field == draft.field)
                .unwrap_or(0) as i64;
            draft.field = FIELDS[(current + delta as i64).rem_euclid(FIELDS.len() as i64) as usize];
        }
        Vec::new()
    }

    /// Ctrl+B in the session panel: read-only browse of the selected closed
    /// session without opening it (spec §10.1).
    fn browse_selected_session(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() || self.session_panel_busy() {
            return Vec::new();
        }
        let selected = {
            let Some(state) = self.session_selector_state() else {
                return Vec::new();
            };
            if !matches!(&state.mode, SessionPanelMode::Browse) {
                return Vec::new();
            }
            state.selected_session_id.clone()
        };
        let Some(selected) = selected else {
            return Vec::new();
        };
        if !self.session_is_visible(&selected) || !self.session_is_filtered_visible(&selected) {
            self.reconcile_session_selection(true);
            return Vec::new();
        }
        self.browse_session(&selected)
    }

    /// Explicit Continue for the selected read-only session (spec §10.1).
    fn continue_selected_session(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() || self.session_panel_busy() {
            return Vec::new();
        }
        // Prefer the active read-only session (Ctrl+G works from the
        // composer); otherwise continue the panel selection.
        let active = self.sessions.active.clone().filter(|active| {
            self.sessions
                .known
                .get(active)
                .is_some_and(|view| view.browsing)
        });
        let selected = active.or_else(|| {
            self.session_selector_state()
                .and_then(|state| state.selected_session_id.clone())
                .filter(|selected| {
                    self.sessions
                        .known
                        .get(selected)
                        .is_some_and(|view| view.browsing)
                })
        });
        let Some(selected) = selected else {
            self.notice(
                NoticeLevel::Info,
                "no read-only session to continue; browse one first (Ctrl+B)",
            );
            return Vec::new();
        };
        self.continue_browsed_session(&selected)
    }

    /// Toggles the selector between the current workspace and all projects.
    fn toggle_session_scope(&mut self) -> Vec<AppCommand> {
        let Some(state) = self.session_selector_state_mut() else {
            return Vec::new();
        };
        state.scope = state.scope.toggled();
        let scope = state.scope;
        self.notice(NoticeLevel::Info, format!("Showing {}", scope.label()));
        self.reconcile_session_selection(true);
        Vec::new()
    }

    fn confirm_session_selector(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.session_panel_busy() {
            return Vec::new();
        }
        let selected = {
            let Some(state) = self.session_selector_state() else {
                return Vec::new();
            };
            if !matches!(&state.mode, SessionPanelMode::Browse) {
                return Vec::new();
            }
            state.selected_session_id.clone()
        };
        let Some(selected) = selected else {
            return Vec::new();
        };
        if !self.session_is_visible(&selected) || !self.session_is_filtered_visible(&selected) {
            self.reconcile_session_selection(true);
            return Vec::new();
        }
        // One open at a time; the pending response owns the panel.
        if self.pending_open_or_history(&selected)
            || self
                .sessions
                .known
                .get(&selected)
                .is_some_and(|view| view.closing)
            || self.pending_requests.values().any(|request| {
                matches!(
                    request,
                    RequestKind::RenameSession { session_id }
                        | RequestKind::CloseSession { session_id }
                        | RequestKind::DeleteSession { session_id }
                        if session_id == &selected
                )
            })
        {
            return Vec::new();
        }
        if self.can_activate_existing_session(&selected) {
            self.dock = Dock::Composer;
            return self.activate_existing_session(&selected);
        }
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
        }
        self.open_session(&selected)
    }

    fn selected_session_id(&self) -> Option<SessionId> {
        self.session_selector_state()
            .and_then(|state| state.selected_session_id.clone())
    }

    fn panel_click_count(&mut self, session_id: &SessionId) -> u8 {
        let now = self.instant_now();
        let count = self
            .panel_click
            .as_ref()
            .filter(|click| {
                click.session_id == *session_id
                    && now.saturating_duration_since(click.at) <= Duration::from_millis(500)
            })
            .map_or(1, |click| click.count.saturating_add(1).min(2));
        self.panel_click = Some(PanelClick {
            session_id: session_id.clone(),
            at: now,
            count,
        });
        count
    }

    fn selector_click_count(&mut self, kind: SelectorKind, key: &str) -> u8 {
        let now = self.instant_now();
        let count = self
            .selector_click
            .as_ref()
            .filter(|click| {
                click.kind == kind
                    && click.key == key
                    && now.saturating_duration_since(click.at) <= Duration::from_millis(500)
            })
            .map_or(1, |click| click.count.saturating_add(1).min(2));
        self.selector_click = Some(SelectorClick {
            kind,
            key: key.to_owned(),
            at: now,
            count,
        });
        count
    }

    fn panel_session_action_safe(
        &mut self,
        session_id: &SessionId,
        action: &str,
    ) -> Option<Vec<AppCommand>> {
        match self.session_action_safety(session_id) {
            SessionActionSafety::Safe => None,
            SessionActionSafety::Unknown => {
                Some(self.report_unknown_session_state(session_id, action))
            }
            SessionActionSafety::Busy => {
                if let Some(state) = self.session_selector_state_mut() {
                    state.error = Some(format!("cannot {action}: session is busy or unsafe"));
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("Cannot {action} session {session_id}: it is busy or unsafe."),
                );
                Some(Vec::new())
            }
        }
    }

    fn begin_session_rename(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        let Some(session_id) = self.selected_session_id() else {
            self.notice(NoticeLevel::Info, "Select a session before renaming it.");
            return Vec::new();
        };
        if !self.session_is_visible(&session_id) || !self.session_is_filtered_visible(&session_id) {
            self.reconcile_session_selection(true);
            return Vec::new();
        }
        let title = self
            .sessions
            .known
            .get(&session_id)
            .and_then(|view| view.info.title.clone())
            .or_else(|| {
                self.sessions
                    .list
                    .iter()
                    .find(|session| session.session_id == session_id)
                    .and_then(|session| session.title.clone())
            })
            .unwrap_or_default();
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
            state.mode = SessionPanelMode::Rename {
                cursor: title.chars().count(),
                draft: title,
                submitting: false,
            };
        }
        Vec::new()
    }

    fn begin_session_close(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.session_panel_busy() {
            return Vec::new();
        }
        let Some(session_id) = self.selected_session_id() else {
            self.notice(NoticeLevel::Info, "Select a session before closing it.");
            return Vec::new();
        };
        if !self.session_is_visible(&session_id) || !self.session_is_filtered_visible(&session_id) {
            self.reconcile_session_selection(true);
            return Vec::new();
        }
        match self.session_loaded(&session_id) {
            Some(true) => {}
            Some(false) => {
                self.notice(NoticeLevel::Info, "Session is already closed.");
                return Vec::new();
            }
            None => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("Session {session_id} is not available for closing."),
                );
                return Vec::new();
            }
        }
        if let Some(commands) = self.panel_session_action_safe(&session_id, "close") {
            return commands;
        }
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
            state.mode = SessionPanelMode::ConfirmClose;
        }
        Vec::new()
    }

    fn begin_session_delete(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.session_panel_busy() {
            return Vec::new();
        }
        let Some(session_id) = self.selected_session_id() else {
            self.notice(NoticeLevel::Info, "Select a session before deleting it.");
            return Vec::new();
        };
        if !self.session_is_visible(&session_id) || !self.session_is_filtered_visible(&session_id) {
            self.reconcile_session_selection(true);
            return Vec::new();
        }
        if self.session_loaded(&session_id).is_none() {
            self.notice(
                NoticeLevel::Warning,
                format!("Session {session_id} is not available for deletion."),
            );
            return Vec::new();
        }
        if let Some(commands) = self.panel_session_action_safe(&session_id, "delete") {
            return commands;
        }
        let loaded = self.session_loaded(&session_id).unwrap_or(true);
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
            state.mode = if loaded {
                SessionPanelMode::ConfirmCloseForDelete
            } else {
                SessionPanelMode::ConfirmDelete {
                    choice: SessionConfirmChoice::Cancel,
                    submitting: false,
                }
            };
        }
        Vec::new()
    }

    fn session_panel_confirm(&mut self) -> Vec<AppCommand> {
        let Some(session_id) = self.selected_session_id() else {
            return Vec::new();
        };
        let mode = self
            .session_selector_state()
            .map(|state| state.mode.clone())
            .unwrap_or(SessionPanelMode::Browse);
        match mode {
            SessionPanelMode::Browse => self.confirm_session_selector(),
            SessionPanelMode::Rename { .. } => self.submit_session_rename(&session_id),
            SessionPanelMode::ConfirmClose => {
                if let Some(commands) = self.panel_session_action_safe(&session_id, "close") {
                    commands
                } else {
                    self.close_session(&session_id, true)
                }
            }
            SessionPanelMode::ConfirmCloseForDelete => {
                if let Some(commands) = self.panel_session_action_safe(&session_id, "close") {
                    commands
                } else {
                    self.close_session(&session_id, true)
                }
            }
            SessionPanelMode::ConfirmDelete {
                choice: SessionConfirmChoice::Cancel,
                ..
            } => self.session_panel_cancel(),
            SessionPanelMode::ConfirmDelete {
                choice: SessionConfirmChoice::Confirm,
                ..
            } => self.confirm_session_delete(&session_id),
        }
    }

    fn session_panel_action(&mut self, action: SessionPanelAction) -> Vec<AppCommand> {
        match action {
            SessionPanelAction::Open => self.confirm_session_selector(),
            SessionPanelAction::Browse => self.browse_selected_session(),
            SessionPanelAction::Continue => self.continue_selected_session(),
            SessionPanelAction::Scope => self.toggle_session_scope(),
            SessionPanelAction::New => self.open_new_session(),
            SessionPanelAction::Refresh => self.refresh_sessions(),
            SessionPanelAction::Rename => self.begin_session_rename(),
            SessionPanelAction::Close => self.begin_session_close(),
            SessionPanelAction::Delete => self.begin_session_delete(),
            SessionPanelAction::Cancel => self.session_panel_cancel(),
            SessionPanelAction::ConfirmDelete => {
                if let Some(state) = self.session_selector_state_mut() {
                    if let SessionPanelMode::ConfirmDelete { choice, .. } = &mut state.mode {
                        *choice = SessionConfirmChoice::Confirm;
                    }
                }
                self.session_panel_confirm()
            }
            SessionPanelAction::SaveRename => self.session_panel_confirm(),
        }
    }

    fn confirm_session_delete(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if let Some(commands) = self.panel_session_action_safe(session_id, "delete") {
            return commands;
        }
        let commands = self.delete_session(session_id, true);
        if !commands.is_empty() {
            if let Some(state) = self.session_selector_state_mut() {
                state.mode = SessionPanelMode::ConfirmDelete {
                    choice: SessionConfirmChoice::Confirm,
                    submitting: true,
                };
            }
        }
        commands
    }

    fn toggle_session_delete_choice(&mut self) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::ConfirmDelete { choice, .. } = &mut state.mode {
                *choice = match choice {
                    SessionConfirmChoice::Cancel => SessionConfirmChoice::Confirm,
                    SessionConfirmChoice::Confirm => SessionConfirmChoice::Cancel,
                };
            }
        }
        Vec::new()
    }

    fn session_panel_cancel(&mut self) -> Vec<AppCommand> {
        let mode = self
            .session_selector_state()
            .map(|state| state.mode.clone())
            .unwrap_or(SessionPanelMode::Browse);
        let mut returned_to_browse = false;
        match mode {
            SessionPanelMode::Browse => self.dock = Dock::Composer,
            SessionPanelMode::Rename {
                submitting: false, ..
            }
            | SessionPanelMode::ConfirmClose
            | SessionPanelMode::ConfirmCloseForDelete => {
                if let Some(state) = self.session_selector_state_mut() {
                    state.mode = SessionPanelMode::Browse;
                    state.error = None;
                    returned_to_browse = true;
                }
            }
            SessionPanelMode::Rename {
                submitting: true, ..
            } => {}
            SessionPanelMode::ConfirmDelete { .. } => {
                if let Some(state) = self.session_selector_state_mut() {
                    state.mode = SessionPanelMode::Browse;
                    state.error = None;
                    returned_to_browse = true;
                }
            }
        }
        if returned_to_browse {
            self.reconcile_session_selection(true);
        }
        Vec::new()
    }

    /// `/rename <title>`: carries the title through the same submission path
    /// as the dialog, so an ACK-lost rename rereads metadata instead of
    /// blind-rewriting (spec §10.4).
    fn rename_session_title(&mut self, session_id: &SessionId, title: String) -> Vec<AppCommand> {
        // The selector refresh command must be returned: dropping it would
        // register a pending request that is never sent (and the app would
        // stay panel-busy forever).
        let mut commands = self.open_selector(SelectorKind::Session);
        if let Some(state) = self.session_selector_state_mut() {
            state.selected_session_id = Some(session_id.clone());
            state.scope = crate::state::selection::SessionScope::CurrentWorkspace;
            state.error = None;
            state.mode = SessionPanelMode::Rename {
                cursor: title.chars().count(),
                draft: title,
                submitting: false,
            };
        }
        commands.extend(self.submit_session_rename(session_id));
        commands
    }

    fn submit_session_rename(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        if self.session_absent(session_id) || self.sessions.pending_deletes.contains(session_id) {
            return Vec::new();
        }
        let Some((draft, submitting)) =
            self.session_selector_state()
                .and_then(|state| match &state.mode {
                    SessionPanelMode::Rename {
                        draft, submitting, ..
                    } => Some((draft.clone(), *submitting)),
                    _ => None,
                })
        else {
            return Vec::new();
        };
        if submitting
            || self.pending_requests.values().any(|request| {
                matches!(request, RequestKind::RenameSession { session_id: pending } if pending == session_id)
            })
        {
            return Vec::new();
        }
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                *submitting = true;
                state.error = None;
            }
        }
        vec![self.request(
            RequestKind::RenameSession {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_rename(id, session_id, &draft),
        )]
    }

    fn confirm_model_item(&mut self) -> Vec<AppCommand> {
        let selected = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.kind != SelectorKind::Model {
                return Vec::new();
            }
            if state.submitting {
                return Vec::new();
            }
            let Some(model) = filtered_models(&self.catalogs.models, &state.query)
                .get(state.cursor)
                .cloned()
                .cloned()
            else {
                return Vec::new();
            };
            model
        };
        if self.draft.is_some() {
            let incompatible = {
                let draft = self.draft.as_mut().expect("draft exists");
                let incompatible = !selected.supported_reasoning.contains(&draft.reasoning);
                draft.model = selected.id.clone();
                incompatible
            };
            if incompatible {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "{} does not support the current reasoning level; choose a supported one.",
                        selected.id
                    ),
                );
            }
            return self.open_selector(SelectorKind::Reasoning);
        }

        let Some(session_id) = self.sessions.active.clone() else {
            return Vec::new();
        };
        if self.active_view().is_some_and(SessionView::is_blocked) {
            self.notice(
                NoticeLevel::Error,
                "session is blocked; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(SessionView::is_preparing) {
            self.notice(
                NoticeLevel::Info,
                "session is preparing context; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| {
            view.event_gap || view.latest_state_query.is_some() || view.state.is_none()
        }) {
            self.notice(
                NoticeLevel::Warning,
                "session state is not calibrated; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| {
            view.live.as_ref().is_some_and(|live| live.waiting)
                || view.state.as_ref().is_some_and(|state| {
                    matches!(
                        state.status,
                        SessionStatusWire::WaitingForInput | SessionStatusWire::Finishing
                    )
                })
        }) {
            self.notice(
                NoticeLevel::Warning,
                "session is not accepting configuration right now",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| view.closing) {
            self.notice(
                NoticeLevel::Warning,
                "session is closing; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| view.is_blocked()) {
            self.notice(
                NoticeLevel::Warning,
                "session is blocked; cannot update configuration",
            );
            return Vec::new();
        }
        if self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::UpdateSession {
                    session_id: pending_session,
                    ..
                } if pending_session == &session_id
            )
        }) {
            return Vec::new();
        }
        if self
            .active_view()
            .is_some_and(|view| !selected.supported_reasoning.contains(&view.info.reasoning))
        {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "{} does not support the current reasoning level; choose reasoning first.",
                    selected.id
                ),
            );
            let current_reasoning = self.active_view().map(|view| view.info.reasoning);
            let commands = self.open_selector(SelectorKind::Reasoning);
            if let Dock::ReasoningSelector(state) = &mut self.dock {
                state.model_context = Some(selected.id.clone());
                state.cursor = supported_reasoning(&self.catalogs.models, &selected.id)
                    .iter()
                    .position(|level| Some(*level) == current_reasoning)
                    .unwrap_or(0);
            }
            return commands;
        }
        self.set_selector_submitting();
        let target_loop_id = self.active_view().and_then(|v| {
            v.live
                .as_ref()
                .and_then(|l| l.reference.as_ref().map(|r| r.loop_id.clone()))
        });
        let model = Some(selected.id.clone());
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.config_update = Some(crate::state::session::PendingConfigUpdate {
                loop_id: target_loop_id.clone(),
                model: model.clone(),
                reasoning: None,
                revision: None,
                state: crate::state::session::ConfigUpdateState::WaitingBoundary,
            });
        }
        vec![self.request(
            RequestKind::UpdateSession {
                session_id: session_id.clone(),
                loop_id: target_loop_id,
                model: model.clone(),
                reasoning: None,
            },
            |id| OutgoingRequest::session_update(id, &session_id, model, None),
        )]
    }

    fn confirm_reasoning_item(&mut self) -> Vec<AppCommand> {
        let (cursor, kind, submitting, model_context) = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            (
                state.cursor,
                state.kind,
                state.submitting,
                state.model_context.clone(),
            )
        };
        if kind != SelectorKind::Reasoning || submitting {
            return Vec::new();
        }
        let model = self
            .new_session()
            .map(|draft| draft.model.clone())
            .or_else(|| model_context.clone())
            .or_else(|| self.active_view().map(|view| view.info.model.clone()))
            .unwrap_or_default();
        let Some(selected) = supported_reasoning(&self.catalogs.models, &model)
            .get(cursor)
            .copied()
        else {
            // No supported values (unknown model): nothing to confirm.
            return Vec::new();
        };
        if self.draft.is_some() {
            self.draft.as_mut().expect("draft exists").reasoning = selected;
            self.close_selector_to_form();
            return Vec::new();
        }
        let Some(session_id) = self.sessions.active.clone() else {
            return Vec::new();
        };
        if self.active_view().is_some_and(SessionView::is_blocked) {
            self.notice(
                NoticeLevel::Error,
                "session is blocked; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(SessionView::is_preparing) {
            self.notice(
                NoticeLevel::Info,
                "session is preparing context; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| {
            view.event_gap || view.latest_state_query.is_some() || view.state.is_none()
        }) {
            self.notice(
                NoticeLevel::Warning,
                "session state is not calibrated; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| {
            view.live.as_ref().is_some_and(|live| live.waiting)
                || view.state.as_ref().is_some_and(|state| {
                    matches!(
                        state.status,
                        SessionStatusWire::WaitingForInput | SessionStatusWire::Finishing
                    )
                })
        }) {
            self.notice(
                NoticeLevel::Warning,
                "session is not accepting configuration right now",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| view.closing) {
            self.notice(
                NoticeLevel::Warning,
                "session is closing; cannot update configuration",
            );
            return Vec::new();
        }
        if self.active_view().is_some_and(|view| view.is_blocked()) {
            self.notice(
                NoticeLevel::Warning,
                "session is blocked; cannot update configuration",
            );
            return Vec::new();
        }
        if self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::UpdateSession {
                    session_id: pending_session,
                    ..
                } if pending_session == &session_id
            )
        }) {
            return Vec::new();
        }
        self.set_selector_submitting();
        let target_loop_id = self.active_view().and_then(|v| {
            v.live
                .as_ref()
                .and_then(|l| l.reference.as_ref().map(|r| r.loop_id.clone()))
        });
        let reasoning = Some(selected);
        let requested_model = model_context;
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.config_update = Some(crate::state::session::PendingConfigUpdate {
                loop_id: target_loop_id.clone(),
                model: requested_model.clone(),
                reasoning,
                revision: None,
                state: crate::state::session::ConfigUpdateState::WaitingBoundary,
            });
        }
        vec![self.request(
            RequestKind::UpdateSession {
                session_id: session_id.clone(),
                loop_id: target_loop_id,
                model: requested_model.clone(),
                reasoning,
            },
            |id| OutgoingRequest::session_update(id, &session_id, requested_model, reasoning),
        )]
    }

    fn confirm_profile_item(&mut self) -> Vec<AppCommand> {
        let selected = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.kind != SelectorKind::Profile {
                return Vec::new();
            }
            if state.submitting {
                return Vec::new();
            }
            let Some(profile) = filtered_profiles(&self.catalogs.profiles, &state.query)
                .get(state.cursor)
                .cloned()
                .cloned()
            else {
                return Vec::new();
            };
            profile
        };
        // Choosing a profile adopts its model/reasoning defaults; the user
        // can still override both afterwards. The active session is never
        // touched (spec 7-required, 25.2).
        if self.draft.is_some() {
            if let Some(draft) = self.draft.as_mut() {
                draft.profile = selected.id.clone();
                draft.model = selected.model.clone();
                draft.reasoning = selected.reasoning;
            }
            self.close_selector_to_form();
        }
        Vec::new()
    }

    fn submit_new_session(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        let Some(draft) = self.new_session().cloned() else {
            return Vec::new();
        };
        // Submitting is gated while a create is already in flight (spec
        // 25.5); the response re-enables it.
        if draft.submitting {
            return Vec::new();
        }
        if let Some(current) = self.draft_mut() {
            current.submitting = true;
            current.error = None;
        }
        let profile = (!draft.profile.is_empty()).then_some(draft.profile.as_str());
        let model = (!draft.model.is_empty()).then_some(draft.model.as_str());
        let title = (!draft.title.is_empty()).then_some(draft.title.as_str());
        vec![self.request(
            RequestKind::CreateSession {
                draft: draft.draft_id,
            },
            |id| {
                OutgoingRequest::session_create(
                    id,
                    &draft.workspace,
                    profile,
                    model,
                    Some(draft.reasoning),
                    title,
                )
            },
        )]
    }

    // ---- Phase 5: input handling (spec 22-23, 32, 43) -----------------

    /// Terminal events enter here; only the fixed keymap turns keys into
    /// actions, and only this method mutates state.
    fn on_terminal(&mut self, event: CrosstermEvent) -> Vec<AppCommand> {
        match event {
            CrosstermEvent::Key(key) => {
                let action = keymap::map(self, key);
                self.apply_action(action)
            }
            CrosstermEvent::Paste(_)
                if self.focused_region() == crate::state::panels::Focus::Main =>
            {
                Vec::new()
            }
            CrosstermEvent::Paste(text) if self.workspace_browser().is_some() => {
                self.workspace_edit(Some(&text), false, false, false)
            }
            CrosstermEvent::Paste(text) => ui_actions::handle_paste(self, text),
            CrosstermEvent::Mouse(mouse) => ui_actions::handle_mouse(self, mouse),
            CrosstermEvent::FocusLost | CrosstermEvent::FocusGained => {
                ui_actions::cancel_scrollbar_drag(self);
                Vec::new()
            }
            // Resize is consumed via AppEvent::Viewport (same frame).
            _ => Vec::new(),
        }
    }

    fn apply_action(&mut self, action: Action) -> Vec<AppCommand> {
        use Action::*;
        if !matches!(&action, None | CompletionMove(_)) {
            self.editor_selection = std::option::Option::None;
        }
        if !matches!(
            &action,
            CursorMove(EditorCursor::Up | EditorCursor::Down) | CompletionMove(_)
        ) {
            self.composer_preferred_visual_col = std::option::Option::None;
        }
        match action {
            None => Vec::new(),
            WorkspaceType(c) => self.workspace_edit(Some(&c.to_string()), false, false, false),
            WorkspaceClear => {
                if let Dock::Workspace(browser) = &mut self.dock {
                    if browser.scope_focused {
                        browser.scope.clear();
                    } else {
                        browser.query.clear();
                    }
                }
                self.workspace_edit(std::option::Option::None, false, false, false)
            }
            WorkspaceBackspace => {
                self.workspace_edit(std::option::Option::None, true, false, false)
            }
            WorkspaceField => self.workspace_edit(std::option::Option::None, false, true, false),
            WorkspaceCase => self.workspace_edit(std::option::Option::None, false, false, true),
            WorkspaceMove(delta) => self.workspace_move(delta),
            WorkspaceSelect(preview) => self.workspace_select(preview),
            WorkspaceMore(refresh) => self.workspace_more(refresh),
            DetailActivate if self.context_panel().is_some() => self.context_action(),
            DetailActivate => self.changes_select(),
            FileMore if self.changes().is_some() => self.changes_more(false),
            FileMore => self.file_more(false),
            PreviewReference => self.preview_reference(),
            DetailFocus => {
                self.focus = if self.focus == crate::state::panels::Focus::Main {
                    crate::state::panels::Focus::Editor
                } else {
                    crate::state::panels::Focus::Main
                };
                Vec::new()
            }
            DetailEscape => self.detail_escape(),
            DetailTab(step) if self.context_panel().is_some() => {
                self.context_tab(step);
                Vec::new()
            }
            DetailTab(step) if self.changes().is_some() => self.changes_tab(step),
            DetailTab(step) => self.detail_tab(step),
            DetailScroll(delta) => {
                if self.context_panel().is_some() {
                    self.scroll_context(delta, false);
                } else if self.changes().is_some() {
                    self.scroll_changes(delta, false);
                } else if self.file_preview().is_some() {
                    self.scroll_file(delta, false);
                } else {
                    self.scroll_tool(delta, false);
                }
                Vec::new()
            }
            DetailEnd => {
                if self.context_panel().is_some() {
                    self.scroll_context(0, true);
                } else if self.changes().is_some() {
                    self.scroll_changes(0, true);
                } else if self.file_preview().is_some() {
                    self.scroll_file(0, true);
                } else {
                    self.scroll_tool(0, true);
                }
                Vec::new()
            }
            DetailRefresh if self.file_preview().is_some() => self.file_more(true),
            DetailRefresh if self.context_panel().is_some() => self.refresh_context_panel(),
            DetailRefresh if self.changes().is_some() => self.changes_more(true),
            DetailRefresh => self.refresh_tool_detail(),
            DetailCopy if self.file_preview().is_some() => self.copy_file(),
            DetailCopy if self.changes().is_some() => self.copy_diff(),
            DetailCopy => self.copy_tool_detail(),
            ClearSelection => {
                self.clear_selection();
                Vec::new()
            }
            Quit => self.request_shutdown(),
            FirstCtrlC => self.ctrl_c(),
            CtrlD => {
                if self.composer.is_empty()
                    && self.active_view().is_none_or(|view| {
                        view.live.is_none()
                            && view
                                .state
                                .as_ref()
                                .is_none_or(|state| state.status == SessionStatusWire::Idle)
                    })
                {
                    self.request_shutdown()
                } else {
                    Vec::new()
                }
            }
            TypeChar(c) => {
                let (line, col) = self.composer.cursor();
                let open_files = c == '@'
                    && (col == 0
                        || self
                            .composer
                            .lines()
                            .get(line)
                            .and_then(|l| l.chars().nth(col - 1))
                            .is_some_and(char::is_whitespace));
                if !self.admit_draft_input(c.len_utf8()) {
                    return Vec::new();
                }
                if !self.composer_mut().type_char(c) {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
                    );
                }
                ui_actions::refresh_slash_completion(self);
                if open_files {
                    return self.open_workspace_browser(
                        crate::state::workspace::BrowserKind::Files,
                        String::new(),
                        true,
                    );
                }
                Vec::new()
            }
            CompletionMove(delta) => {
                ui_actions::move_slash_completion(self, delta);
                Vec::new()
            }
            CompletionAccept => {
                ui_actions::accept_slash_completion(self);
                Vec::new()
            }
            CompletionAcceptAndSubmit => {
                if ui_actions::accept_slash_completion(self) {
                    Vec::new()
                } else {
                    self.submit_composer()
                }
            }
            CompletionCancel => {
                self.slash_completion = std::option::Option::None;
                Vec::new()
            }
            Newline => {
                if !self.composer.newline() {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
                    );
                }
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            Backspace => {
                self.composer.backspace();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            Delete => {
                self.composer.delete();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            CursorMove(direction) => {
                let commands = ui_actions::composer_move(self, direction);
                ui_actions::refresh_slash_completion(self);
                commands
            }
            LineStart => {
                self.composer.line_start();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            LineEnd => {
                self.composer.line_end();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            WordDelete => {
                self.composer.word_delete();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            Undo => {
                self.composer.undo();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            Redo => {
                self.composer.redo();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            Submit => self.submit_composer(),
            HistoryPrev => {
                // Alt+Up on an empty editor withdraws the NEXT UNSENT queue
                // item so a paused queue stays recoverable without copying
                // from the display-only dock; otherwise normal history nav.
                if !self.retrieve_next_queued_steer() {
                    self.composer.history_prev();
                }
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            HistoryNext => {
                self.composer.history_next();
                ui_actions::refresh_slash_completion(self);
                Vec::new()
            }
            OpenHelp => self.open_dock(Dock::Help),
            OpenLogs => self.open_dock(Dock::Logs),
            OpenSessions => self.open_selector(SelectorKind::Session),
            OpenNewSession => self.open_new_session(),
            OpenModel => self.open_selector(SelectorKind::Model),
            OpenReasoning => self.open_selector(SelectorKind::Reasoning),
            RefreshSessions => self.refresh_sessions(),
            SessionRename => self.begin_session_rename(),
            SessionClose => self.begin_session_close(),
            SearchTypeChar(c) => {
                self.search_type_char(c);
                Vec::new()
            }
            SearchBackspace => {
                self.search_backspace();
                Vec::new()
            }
            SearchClear => {
                self.search_clear();
                Vec::new()
            }
            SearchMove(delta) => {
                self.search_move(delta);
                Vec::new()
            }
            SearchConfirm => self.search_confirm(),
            SearchStep(delta) => self.search_step(delta),
            SearchScopeToggle => self.search_toggle_scope(),
            SearchStop => {
                self.search_stop();
                Vec::new()
            }
            SearchEscape => self.search_escape(),
            ExportTypeChar(c) => {
                self.export_type_char(c);
                Vec::new()
            }
            ExportBackspace => {
                self.export_backspace();
                Vec::new()
            }
            ExportClear => {
                self.export_clear();
                Vec::new()
            }
            ExportSubmit => self.export_submit(),
            ExportToggleThinking => {
                self.export_toggle_thinking();
                Vec::new()
            }
            ExportToggleTool => {
                self.export_toggle_tool();
                Vec::new()
            }
            ExportToggleUnsaved => {
                self.export_toggle_unsaved();
                Vec::new()
            }
            ExportToggleOverwrite => {
                self.export_toggle_overwrite();
                Vec::new()
            }
            ExportToggleRaw => {
                self.export_toggle_raw();
                Vec::new()
            }
            ExportEscape => self.export_escape(),
            SettingsTypeChar(c) => {
                if let Some(state) = self.settings_state_mut() {
                    state.type_char(c);
                }
                Vec::new()
            }
            SettingsBackspace => {
                if let Some(state) = self.settings_state_mut() {
                    state.backspace();
                }
                Vec::new()
            }
            SettingsClear => {
                if let Some(state) = self.settings_state_mut() {
                    state.clear();
                }
                Vec::new()
            }
            SettingsFieldStep(delta) => {
                if let Some(state) = self.settings_state_mut() {
                    state.step(delta);
                }
                Vec::new()
            }
            SettingsToggle => self.settings_toggle_or_submit(),
            SettingsSubmit => self.settings_submit(),
            SettingsEscape => self.settings_escape(),
            SessionBrowse => self.browse_selected_session(),
            SessionContinue => self.continue_selected_session(),
            SessionScopeToggle => self.toggle_session_scope(),
            SessionDelete => self.begin_session_delete(),
            SessionDeleteToggle => self.toggle_session_delete_choice(),
            ToggleTools => {
                if let Some(session_id) = self.sessions.active.as_ref().cloned() {
                    ui_actions::toggle_tools(self, &session_id);
                }
                Vec::new()
            }
            ToggleReasoning => {
                self.reasoning_visible = !self.reasoning_visible;
                Vec::new()
            }
            CloseDock => self.cancel_dock(),
            CancelTurn => self.cancel_active_turn(),
            SelectorMove(delta) => self.move_selector(delta),
            SelectorPage(delta) => self.page_selector(delta),
            SelectorConfirm => self.confirm_dock(),
            SelectorChar(c) => {
                if self.selector_state().is_some_and(|state| state.submitting)
                    || (self.session_selector_state().is_some() && self.session_panel_busy())
                {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.push(c);
                    state.cursor = 0;
                } else if let Some(state) = self.session_selector_state_mut() {
                    if matches!(&state.mode, SessionPanelMode::Browse) {
                        state.query.push(c);
                        self.reconcile_session_selection(true);
                    }
                }
                Vec::new()
            }
            SelectorBackspace => {
                if self.selector_state().is_some_and(|state| state.submitting)
                    || (self.session_selector_state().is_some() && self.session_panel_busy())
                {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.pop();
                    state.cursor = 0;
                } else if let Some(state) = self.session_selector_state_mut() {
                    if matches!(&state.mode, SessionPanelMode::Browse) {
                        state.query.pop();
                        self.reconcile_session_selection(true);
                    }
                }
                Vec::new()
            }
            SelectorClear => {
                if self.selector_state().is_some_and(|state| state.submitting)
                    || (self.session_selector_state().is_some() && self.session_panel_busy())
                {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.clear();
                    state.cursor = 0;
                } else if let Some(state) = self.session_selector_state_mut() {
                    if matches!(&state.mode, SessionPanelMode::Browse) {
                        state.query.clear();
                        self.reconcile_session_selection(true);
                    }
                }
                Vec::new()
            }
            SessionRenameChar(c) => self.field_char(c),
            SessionRenameBackspace => self.field_backspace(),
            SessionRenameDelete => self.field_delete(),
            SessionRenameCursor(delta) => self.field_cursor_move(delta),
            SessionRenameClear => self.field_clear(),
            SessionRenameHome => self.field_cursor_home(),
            SessionRenameEnd => self.field_cursor_end(),
            FieldStep(delta) => self.dock_field_step(delta),
            FieldChar(c) => self.field_char(c),
            FieldBackspace => self.field_backspace(),
            FieldClear => self.field_clear(),
            FieldCursor(delta) => self.field_cursor_move(delta),
            FieldHome => self.field_cursor_home(),
            FieldEnd => self.field_cursor_end(),
            ScrollRows(delta) => self.scroll_focused(delta),
            ScrollWindow(delta) => {
                if matches!(self.dock, Dock::Help | Dock::Logs) {
                    self.panel_scroll_page(delta)
                } else {
                    let step = self.transcript_scroll_extent().1.saturating_sub(4).max(1) as i32;
                    self.scroll_focused(delta.saturating_mul(step))
                }
            }
            ScrollTop => self.scroll_top_focused(),
            ScrollBottom => self.scroll_bottom_focused(),
        }
    }

    /// One Ctrl+C press: clears a non-empty composer, otherwise first
    /// press warns and a second press within 1s quits (spec 22.1, 43.7).
    fn ctrl_c(&mut self) -> Vec<AppCommand> {
        if !self.composer.is_empty() {
            self.composer.clear();
            self.ctrl_c_at = None;
            Vec::new()
        } else if self.ctrl_c_at.is_some_and(|pressed| {
            self.instant_now().saturating_duration_since(pressed) < DOUBLE_CTRL_C_WINDOW
        }) {
            self.request_shutdown()
        } else {
            self.ctrl_c_at = Some(self.instant_now());
            self.notice(NoticeLevel::Info, "Press Ctrl+C again to quit");
            Vec::new()
        }
    }

    /// Routes a scroll delta to the focused panel: selectors move their
    /// selection, Help/Logs scroll their own view, everything else scrolls
    /// the transcript.
    fn scroll_focused(&mut self, delta: i32) -> Vec<AppCommand> {
        if self.selector_state().is_some() || self.session_selector_state().is_some() {
            return self.move_selector(delta);
        }
        match &self.dock {
            Dock::Help | Dock::Logs => {
                self.panel_scroll_by(delta);
            }
            _ => {
                self.transcript_scroll(delta);
                let (total, visible) = self.transcript_scroll_extent();
                if delta < 0
                    && self.active_view().is_some_and(|view| {
                        view.scroll.offset == 0 && (!view.scroll.follow_tail || total <= visible)
                    })
                {
                    return self.load_earlier_history();
                }
            }
        }
        Vec::new()
    }

    fn panel_scroll_layout(&self) -> Option<crate::ui::panel::PanelLayout> {
        let area = ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1);
        let screen = crate::ui::layout::screen_layout(self, area);
        match &self.dock {
            Dock::Help => Some(crate::ui::panel::layout(
                screen.panel,
                crate::ui::panel::PanelSpec::new(0, false, 1),
            )),
            Dock::Logs => Some(crate::ui::panel::layout(
                screen.panel,
                crate::ui::panel::PanelSpec::new(1, false, 1),
            )),
            _ => None,
        }
    }

    fn panel_scroll_line_count(&self) -> usize {
        match &self.dock {
            Dock::Help => crate::ui::help::content_line_count(),
            Dock::Logs => {
                if self.agent_logs.is_empty() {
                    2
                } else {
                    self.agent_logs.len() + 1
                }
            }
            _ => 0,
        }
    }

    fn panel_scroll_max(&self) -> usize {
        let height = self
            .panel_scroll_layout()
            .map_or(0, |panel| panel.content.height as usize);
        self.panel_scroll_line_count().saturating_sub(height)
    }

    fn panel_scroll_by(&mut self, delta: i32) {
        let max = self.panel_scroll_max();
        self.panel_scroll = (self.panel_scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
    }

    fn panel_scroll_page(&mut self, delta: i32) -> Vec<AppCommand> {
        let height = self
            .panel_scroll_layout()
            .map_or(1, |panel| usize::from(panel.content.height).max(1));
        self.panel_scroll_by(delta.saturating_mul(height as i32));
        Vec::new()
    }

    fn scroll_top_focused(&mut self) -> Vec<AppCommand> {
        if matches!(self.dock, Dock::Help | Dock::Logs) {
            self.panel_scroll = 0;
            Vec::new()
        } else {
            self.transcript_scroll_top()
        }
    }

    fn scroll_bottom_focused(&mut self) -> Vec<AppCommand> {
        if matches!(self.dock, Dock::Help | Dock::Logs) {
            self.panel_scroll = self.panel_scroll_max();
            Vec::new()
        } else {
            self.transcript_scroll_bottom()
        }
    }

    /// Transcript scroll: negative deltas leave the tail and store an
    /// explicit offset from the top; positive deltas return toward the
    /// tail and restore follow at the bottom (spec 32, 43.8).
    fn transcript_scroll(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let (total, visible) = self.transcript_scroll_extent();
        let max_offset = total.saturating_sub(visible);
        let Some(view) = self.active_view() else {
            return;
        };
        let current = if view.scroll.follow_tail {
            max_offset
        } else {
            view.scroll.offset.min(max_offset)
        };
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            current.saturating_add(delta as usize).min(max_offset)
        };
        self.set_transcript_offset(next, total, visible);
    }

    fn set_transcript_offset(&mut self, offset: usize, total: usize, visible: usize) {
        let maximum = total.saturating_sub(visible);
        let offset = offset.min(maximum);
        let Some(view) = self.active_session_mut() else {
            return;
        };
        view.scroll.prompt_cursor = None;
        let current = if view.scroll.follow_tail {
            maximum
        } else {
            view.scroll.offset
        };
        let moved = current != offset;
        view.scroll.follow_tail = offset == maximum;
        view.scroll.offset = if view.scroll.follow_tail { 0 } else { offset };
        if view.scroll.follow_tail {
            view.scroll.new_content = false;
        }
        if moved {
            self.mark_scrollbar_activity(total, visible);
        }
    }

    fn transcript_scroll_top(&mut self) -> Vec<AppCommand> {
        let (total, visible) = self.transcript_scroll_extent();
        self.set_transcript_offset(0, total, visible);
        self.load_earlier_history()
    }

    /// A user reaching the leading gap requests one preceding item page.
    /// Layout/timer updates never call this, so a long history cannot turn
    /// into an unsolicited full-session read.
    fn load_earlier_history(&mut self) -> Vec<AppCommand> {
        let Some(start) = crate::ui::header::earlier_history_start(self) else {
            return Vec::new();
        };
        let Some(session_id) = self.sessions.active.clone() else {
            return Vec::new();
        };
        if !self.can_send_requests()
            || self.pending_search_jump.is_some()
            || self.pending_history(&session_id)
            || self
                .active_view()
                .is_some_and(|view| view.history_read.is_loading())
        {
            return Vec::new();
        }
        // Keep the first visible content row anchored while the page is
        // inserted ahead of it, including when the gap row itself is at top.
        if let Some(view) = self.active_session_mut() {
            view.scroll.follow_tail = false;
        }
        self.capture_scroll_anchor();
        self.request_history_window_at(&session_id, start.saturating_sub(READ_PAGE_LIMIT))
    }

    fn transcript_scroll_bottom(&mut self) -> Vec<AppCommand> {
        let (total, visible) = self.transcript_scroll_extent();
        self.set_transcript_offset(total.saturating_sub(visible), total, visible);
        Vec::new()
    }

    fn transcript_scroll_extent(&self) -> (usize, usize) {
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        self.prepared_conversation(screen.content.width)
            .map(|prepared| (prepared.total_rows(), screen.transcript.height as usize))
            // Like Pi's currentLayout, the last measurement remains usable until
            // the next preparation; pointer motion does not rebuild history.
            .unwrap_or(self.viewport)
    }

    /// Clamps the stored offset after a geometry/content change and marks
    /// `new_content` when the transcript grew while the user scrolled up
    /// (marker cleared by End/bottom scrolling).
    fn clamp_transcript_scroll(&mut self) {
        let (total, visible) = self.viewport;
        let grew = total > self.last_total;
        self.last_total = total;
        let suppress_follow = self.selection.is_some();
        if let Some(view) = self.active_session_mut() {
            if grew && !view.scroll.follow_tail {
                view.scroll.new_content = true;
            }
            let max_offset = total.saturating_sub(visible);
            if !view.scroll.follow_tail {
                view.scroll.offset = view.scroll.offset.min(max_offset);
                if view.scroll.offset == max_offset && !suppress_follow {
                    view.scroll.follow_tail = true;
                    view.scroll.offset = 0;
                    view.scroll.new_content = false;
                }
            }
        }
    }

    fn active_session_mut(&mut self) -> Option<&mut SessionView> {
        let active = self.sessions.active.clone()?;
        self.sessions.known.get_mut(&active)
    }

    fn open_dock(&mut self, dock: Dock) -> Vec<AppCommand> {
        if self.dock == dock {
            return self.cancel_dock();
        }
        self.panel_scroll = 0;
        self.dock = dock;
        Vec::new()
    }

    fn run_command(&mut self, content: &str) -> Vec<AppCommand> {
        match parse_command(content) {
            Err(CommandIssue::NotACommand) => Vec::new(),
            Err(CommandIssue::Unknown(name)) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("unknown command `{name}` — try /help"),
                );
                Vec::new()
            }
            Err(CommandIssue::InvalidArgs(message)) => {
                self.notice(NoticeLevel::Error, message);
                Vec::new()
            }
            Ok(command) => self.apply_command(command),
        }
    }

    fn apply_command(&mut self, command: LocalCommand) -> Vec<AppCommand> {
        match command {
            LocalCommand::New => self.create_session_quick(),
            LocalCommand::NewForm => self.open_new_session(),
            LocalCommand::Resume => {
                let browsing = self.sessions.active.clone().filter(|active| {
                    self.sessions
                        .known
                        .get(active)
                        .is_some_and(|view| view.browsing)
                });
                match browsing {
                    Some(active) => self.continue_browsed_session(&active),
                    None => self.open_selector(SelectorKind::Session),
                }
            }
            LocalCommand::Sessions => self.open_selector(SelectorKind::Session),
            LocalCommand::Model => self.open_selector(SelectorKind::Model),
            LocalCommand::Reasoning => self.open_selector(SelectorKind::Reasoning),
            LocalCommand::Tool(key) => self.tool_command(key),
            LocalCommand::Settings => self.open_settings(),
            LocalCommand::Editor => self.open_external_editor(),
            LocalCommand::Theme(kind) => {
                self.theme = kind;
                self.notice(NoticeLevel::Info, format!("theme: {kind:?}"));
                Vec::new()
            }
            LocalCommand::Close { confirm } => {
                if let Some(session_id) = self.sessions.active.clone() {
                    self.close_session(&session_id, confirm)
                } else {
                    self.notice(NoticeLevel::Warning, "no active session to close");
                    Vec::new()
                }
            }
            LocalCommand::Delete { confirm } => {
                // A closed session has no active view: fall back to the
                // session panel selection, which is the only way to delete a
                // closed session by command (spec §10.4).
                match self
                    .sessions
                    .active
                    .clone()
                    .or_else(|| self.selected_session_id())
                {
                    Some(session_id) => self.delete_session(&session_id, confirm),
                    None => {
                        self.notice(NoticeLevel::Warning, "no active session to delete");
                        Vec::new()
                    }
                }
            }
            LocalCommand::Search { query, scope } => {
                self.close_main_detail();
                self.open_search(query, scope)
            }
            LocalCommand::Copy { target } => self.copy_command(target),
            LocalCommand::Export {
                target,
                raw_oversized,
            } => self.open_export_form(target, raw_oversized),
            LocalCommand::PromptJump(direction) => self.prompt_jump(direction),
            LocalCommand::Latest => self.jump_latest(),
            LocalCommand::Clear => self.clear_transcript(),
            LocalCommand::Files(query) => self.open_workspace_browser(
                crate::state::workspace::BrowserKind::Files,
                query,
                false,
            ),
            LocalCommand::Grep(query) => self.open_workspace_browser(
                crate::state::workspace::BrowserKind::Grep,
                query,
                false,
            ),
            LocalCommand::Refresh if self.file_preview().is_some() => self.file_more(true),
            LocalCommand::Refresh if self.tool_detail().is_some() => self.refresh_tool_detail(),
            LocalCommand::Refresh if self.changes().is_some() => self.changes_more(true),
            LocalCommand::Refresh if self.context_panel().is_some() => self.refresh_context_panel(),
            LocalCommand::Refresh => self.refresh_view_data(),
            LocalCommand::Rename { title } => self.rename_from_command(title),
            LocalCommand::Help => self.open_dock(Dock::Help),
            LocalCommand::Logs => self.open_dock(Dock::Logs),
            LocalCommand::Cancel => self.cancel_active_turn(),
            LocalCommand::Context => self.open_context(),
            LocalCommand::Diff(scope) => self.open_changes(scope),
            LocalCommand::Compact => self.start_manual_compact(),
            LocalCommand::Reload => self.reload(),
            LocalCommand::Quit => self.request_shutdown(),
        }
    }

    fn settings_state(&self) -> Option<&crate::state::settings::SettingsState> {
        match &self.dock {
            Dock::Settings(state) => Some(state),
            _ => None,
        }
    }

    fn settings_state_mut(&mut self) -> Option<&mut crate::state::settings::SettingsState> {
        match &mut self.dock {
            Dock::Settings(state) => Some(state),
            _ => None,
        }
    }

    pub fn open_settings(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.settings_previous.is_some() {
            self.notice(NoticeLevel::Info, "settings are already being saved");
            return Vec::new();
        }
        self.dock = Dock::Settings(crate::state::settings::SettingsState::from_config(
            &self.tui_config,
        ));
        self.panel_scroll = 0;
        Vec::new()
    }

    fn settings_toggle_or_submit(&mut self) -> Vec<AppCommand> {
        let apply = self
            .settings_state()
            .is_some_and(|state| state.field == crate::state::settings::SettingsField::Apply);
        if apply {
            self.settings_submit()
        } else {
            if let Some(state) = self.settings_state_mut() {
                state.toggle();
            }
            Vec::new()
        }
    }

    fn settings_submit(&mut self) -> Vec<AppCommand> {
        let Some(state) = self.settings_state() else {
            return Vec::new();
        };
        if state.submitting {
            return Vec::new();
        }
        let config = match state.build_config() {
            Ok(config) => config,
            Err(error) => {
                if let Some(state) = self.settings_state_mut() {
                    state.error = Some(error);
                }
                return Vec::new();
            }
        };
        let previous = self.tui_config.clone();
        let agent_changed = previous.agent_executable != config.agent_executable
            || previous.agent_config != config.agent_config;
        self.settings_previous_restart_required = Some(self.agent_restart_required);
        self.settings_previous = Some(previous);
        self.apply_tui_config(config.clone());
        self.agent_restart_required |= agent_changed;
        if let Some(state) = self.settings_state_mut() {
            state.submitting = true;
            state.error = None;
        }
        vec![AppCommand::PersistConfig(Box::new(
            crate::command::PersistConfigRequest {
                path: self.config_path.clone(),
                config,
            },
        ))]
    }

    fn settings_escape(&mut self) -> Vec<AppCommand> {
        if self.settings_state().is_some_and(|state| state.submitting) {
            return Vec::new();
        }
        self.settings_previous = None;
        self.settings_previous_restart_required = None;
        self.dock = Dock::Composer;
        Vec::new()
    }

    pub fn settings_form(&self) -> Option<&crate::state::settings::SettingsState> {
        self.settings_state()
    }

    pub fn editor_active(&self) -> bool {
        self.editor_capture.is_some()
    }

    fn open_external_editor(&mut self) -> Vec<AppCommand> {
        if self.editor_active() {
            self.notice(NoticeLevel::Info, "an external editor is already open");
            return Vec::new();
        }
        let Some(editor) = self.tui_config.editor.clone() else {
            self.notice(
                NoticeLevel::Warning,
                "no external editor is configured; use /settings or MINICORE_TUI_EDITOR",
            );
            return Vec::new();
        };
        if self.composer.byte_len() > crate::limits::EDITOR_READ_BYTES {
            self.notice(
                NoticeLevel::Warning,
                "the current draft is too large for external editor admission",
            );
            return Vec::new();
        }
        let session_id = self.sessions.active.clone().unwrap_or_default();
        let session_epoch = self
            .sessions
            .known
            .get(&session_id)
            .map(|view| view.session_epoch)
            .unwrap_or(0);
        self.next_editor_operation = self.next_editor_operation.wrapping_add(1);
        let capture = crate::jobs::EditorCapture {
            operation_id: self.next_editor_operation,
            session_id,
            session_epoch,
            editor_revision: self.composer.editor_revision(),
        };
        let draft = self.composer.content();
        self.editor_capture = Some(capture.clone());
        vec![AppCommand::StartEditor(Box::new(
            crate::command::StartEditorRequest {
                capture,
                editor,
                draft,
            },
        ))]
    }

    fn on_editor_finished(
        &mut self,
        capture: crate::jobs::EditorCapture,
        outcome: crate::jobs::EditorOutcome,
    ) -> Vec<AppCommand> {
        if self.editor_capture.as_ref() != Some(&capture) {
            return Vec::new();
        }
        self.editor_capture = None;
        match outcome {
            crate::jobs::EditorOutcome::Updated(text) => {
                let current_session = self.sessions.active.clone().unwrap_or_default();
                let current_epoch = self
                    .sessions
                    .known
                    .get(&current_session)
                    .map(|view| view.session_epoch)
                    .unwrap_or(0);
                if current_session != capture.session_id
                    || current_epoch != capture.session_epoch
                    || self.composer.editor_revision() != capture.editor_revision
                {
                    self.notice(
                        NoticeLevel::Warning,
                        "external editor returned an older draft; newer Composer text was kept",
                    );
                } else if text.len() > crate::limits::EDITOR_READ_BYTES {
                    self.notice(
                        NoticeLevel::Warning,
                        "external editor output exceeded the draft readback limit",
                    );
                } else {
                    self.composer.set_text(&text);
                    ui_actions::refresh_slash_completion(self);
                    self.notice(NoticeLevel::Info, "draft updated from external editor");
                }
            }
            crate::jobs::EditorOutcome::Cancelled => {
                self.notice(NoticeLevel::Info, "external editor cancelled; draft kept");
            }
            crate::jobs::EditorOutcome::Failed(error) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("external editor failed: {error}"),
                );
            }
        }
        Vec::new()
    }

    fn on_config_finished(
        &mut self,
        path: std::path::PathBuf,
        config: crate::config::TuiConfig,
        result: Result<(), String>,
    ) -> Vec<AppCommand> {
        if path != self.config_path || self.settings_previous.is_none() {
            return Vec::new();
        }
        match result {
            Ok(()) => {
                self.settings_previous = None;
                self.settings_previous_restart_required = None;
                if self.agent_restart_required {
                    self.notice(
                        NoticeLevel::Info,
                        "settings saved; Agent path changes apply on the next startup",
                    );
                } else {
                    self.notice(NoticeLevel::Info, "settings saved");
                }
                self.dock = Dock::Composer;
            }
            Err(error) => {
                let previous = self.settings_previous.take().unwrap_or_default();
                self.agent_restart_required = self
                    .settings_previous_restart_required
                    .take()
                    .unwrap_or(false);
                self.apply_tui_config(previous.clone());
                let mut state = crate::state::settings::SettingsState::from_config(&previous);
                state.error = Some(format!("settings were not saved: {error}"));
                self.dock = Dock::Settings(state);
            }
        }
        let _ = config;
        Vec::new()
    }

    /// `/clear` wipes only the local view of the active session and reloads
    /// its transcript from the beginning; the agent session is untouched
    /// and the command is refused while a turn is running (spec 23.3).
    fn clear_transcript(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        let Some(active) = self.sessions.active.clone() else {
            self.notice(NoticeLevel::Info, "No session is open to clear.");
            return Vec::new();
        };
        if self
            .sessions
            .known
            .get(&active)
            .is_some_and(|view| view.live.is_some())
        {
            self.notice(
                NoticeLevel::Warning,
                "Cannot clear while the agent is working; press Esc to cancel first.",
            );
            return Vec::new();
        }
        if self.pending_history(&active) {
            self.notice(
                NoticeLevel::Info,
                "Transcript is still loading; clear it after history finishes.",
            );
            return Vec::new();
        }
        let reconciling_gap = self
            .sessions
            .known
            .get(&active)
            .is_some_and(|view| view.event_gap);
        if let Some(view) = self.sessions.known.get_mut(&active) {
            view.transcript.clear_blocks();
            view.usage_projection = crate::state::session::UsageProjection::default();
            view.scroll = crate::state::session::ScrollState::default();
            view.history_read.begin(if reconciling_gap {
                HistoryTrigger::Gap
            } else {
                HistoryTrigger::Refresh
            });
        }
        self.request_history(&active).into_iter().collect()
    }

    fn field_char(&mut self, c: char) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename {
                draft,
                cursor,
                submitting,
            } = &mut state.mode
            {
                if !*submitting {
                    let offset = Self::char_to_byte(draft, *cursor);
                    draft.insert(offset, c);
                    *cursor += 1;
                }
            }
            return Vec::new();
        }
        if self.new_session().is_some_and(|draft| draft.submitting) {
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            let cursor = draft.field_cursor;
            match draft.field {
                NewSessionField::Workspace => {
                    let offset = Self::char_to_byte(&draft.workspace, cursor);
                    draft.workspace.insert(offset, c);
                    draft.field_cursor = cursor + 1;
                }
                NewSessionField::Title => {
                    let offset = Self::char_to_byte(&draft.title, cursor);
                    draft.title.insert(offset, c);
                    draft.field_cursor = cursor + 1;
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn field_backspace(&mut self) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename {
                draft,
                cursor,
                submitting,
            } = &mut state.mode
            {
                if !*submitting && *cursor > 0 {
                    let offset = Self::char_to_byte(draft, *cursor);
                    let previous = draft[..offset]
                        .chars()
                        .next_back()
                        .map_or(0, char::len_utf8);
                    draft.remove(offset - previous);
                    *cursor -= 1;
                }
            }
            return Vec::new();
        }
        if self.new_session().is_some_and(|draft| draft.submitting) {
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            if draft.field_cursor == 0 {
                return Vec::new();
            }
            let cursor = draft.field_cursor;
            match draft.field {
                NewSessionField::Workspace => {
                    let offset = Self::char_to_byte(&draft.workspace, cursor);
                    let previous = draft.workspace[..offset]
                        .chars()
                        .next_back()
                        .map_or(0, char::len_utf8);
                    draft.workspace.remove(offset - previous);
                    draft.field_cursor = cursor - 1;
                }
                NewSessionField::Title => {
                    let offset = Self::char_to_byte(&draft.title, cursor);
                    let previous = draft.title[..offset]
                        .chars()
                        .next_back()
                        .map_or(0, char::len_utf8);
                    draft.title.remove(offset - previous);
                    draft.field_cursor = cursor - 1;
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn field_delete(&mut self) -> Vec<AppCommand> {
        let Some(state) = self.session_selector_state_mut() else {
            return Vec::new();
        };
        if let SessionPanelMode::Rename {
            draft,
            cursor,
            submitting,
        } = &mut state.mode
        {
            if !*submitting {
                let len = draft.chars().count();
                if *cursor < len {
                    let offset = Self::char_to_byte(draft, *cursor);
                    let next = draft[offset..].chars().next().map_or(0, char::len_utf8);
                    draft.drain(offset..offset + next);
                }
            }
        }
        Vec::new()
    }

    fn field_insert(&mut self, text: &str) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename {
                draft,
                cursor,
                submitting,
            } = &mut state.mode
            {
                if !*submitting {
                    let offset = Self::char_to_byte(draft, *cursor);
                    draft.insert_str(offset, text);
                    *cursor += text.chars().count();
                }
            }
            return Vec::new();
        }
        if self.new_session().is_some_and(|draft| draft.submitting) {
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            let cursor = draft.field_cursor;
            match draft.field {
                NewSessionField::Workspace => {
                    let offset = Self::char_to_byte(&draft.workspace, cursor);
                    draft.workspace.insert_str(offset, text);
                    draft.field_cursor = cursor + text.chars().count();
                }
                NewSessionField::Title => {
                    let offset = Self::char_to_byte(&draft.title, cursor);
                    draft.title.insert_str(offset, text);
                    draft.field_cursor = cursor + text.chars().count();
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn field_clear(&mut self) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename {
                draft,
                cursor,
                submitting,
            } = &mut state.mode
            {
                if !*submitting {
                    draft.clear();
                    *cursor = 0;
                }
            }
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            if !draft.submitting {
                match draft.field {
                    NewSessionField::Workspace => draft.workspace.clear(),
                    NewSessionField::Title => draft.title.clear(),
                    _ => {}
                }
                draft.field_cursor = 0;
            }
        }
        Vec::new()
    }

    fn field_cursor_move(&mut self, delta: i32) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename { draft, cursor, .. } = &mut state.mode {
                let len = draft.chars().count();
                *cursor = (*cursor as i64 + delta as i64).clamp(0, len as i64) as usize;
            }
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            let len = match draft.field {
                NewSessionField::Workspace => draft.workspace.chars().count(),
                NewSessionField::Title => draft.title.chars().count(),
                _ => return Vec::new(),
            };
            draft.field_cursor =
                (draft.field_cursor as i64 + delta as i64).clamp(0, len as i64) as usize;
        }
        Vec::new()
    }

    fn field_cursor_home(&mut self) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename { cursor, .. } = &mut state.mode {
                *cursor = 0;
            }
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            draft.field_cursor = 0;
        }
        Vec::new()
    }

    fn field_cursor_end(&mut self) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            if let SessionPanelMode::Rename { draft, cursor, .. } = &mut state.mode {
                *cursor = draft.chars().count();
            }
            return Vec::new();
        }
        if let Some(draft) = self.draft_mut() {
            let len = match draft.field {
                NewSessionField::Workspace => draft.workspace.chars().count(),
                NewSessionField::Title => draft.title.chars().count(),
                _ => return Vec::new(),
            };
            draft.field_cursor = len;
        }
        Vec::new()
    }

    /// Byte offset of the `cursor`-th char (cursor is a char index).
    fn char_to_byte(text: &str, cursor: usize) -> usize {
        text.chars().take(cursor).map(char::len_utf8).sum::<usize>()
    }

    /// Removes every transient notice past its TTL in one pass, keeping
    /// sticky notices and the insertion order; a newer transient never
    /// shields an older one (spec 33.2).
    fn expire_notices(&mut self) {
        let now = self.instant_now();
        let ttl = self.notice_ttl;
        self.notices.retain(|notice| {
            notice.sticky || now.saturating_duration_since(notice.created_at) < ttl
        });
    }

    fn next_request_id(&mut self) -> RequestId {
        let next = self
            .next_request_id
            .0
            .checked_add(1)
            .expect("request id space exhausted");
        self.next_request_id = RequestId(next);
        self.next_request_id
    }

    /// Stores a coalesced retry for a request that was refused admission. The
    /// bounded map keeps at most one request per precise target.
    fn defer_request(
        &mut self,
        kind: RequestKind,
        build: impl FnOnce(RequestId) -> OutgoingRequest,
    ) -> bool {
        let Some(key) = Self::retry_key(&kind) else {
            return false;
        };
        if self.pending_retries.len() >= MAX_RPC_RETRIES && !self.pending_retries.contains_key(&key)
        {
            return false;
        }
        let id = self.next_request_id();
        let request = build(id);
        self.pending_retries
            .insert(key, RetryEntry { kind, request });
        true
    }

    /// Re-emits retained retry intents. Every emitted command carries a fresh
    /// pending registration; a retry that is refused again is simply stored
    /// again by [`App::on_queue_full`]. Only never-written control and
    /// settlement intents are retained here, so re-emission is always
    /// re-sending something the Agent never saw, never replaying a failure.
    fn drain_rpc_retries(&mut self) -> Vec<AppCommand> {
        if self.pending_retries.is_empty() {
            return Vec::new();
        }
        let entries = std::mem::take(&mut self.pending_retries);
        let mut commands = Vec::with_capacity(entries.len());
        for (retry_key, entry) in entries {
            if let Some(query_key) = Self::retry_query_key(&entry.kind) {
                let waiting_before = self.queries.waiting_len();
                match self.queries.request_query(query_key, entry.request.id) {
                    crate::app::queries::QueryAdmission::Admitted => {}
                    crate::app::queries::QueryAdmission::Coalesced => continue,
                    crate::app::queries::QueryAdmission::Busy => {
                        // A Busy admission normally queues the key inside
                        // QuerySlots. If its bounded waiting queue was full,
                        // preserve the retry entry for a later progress pass.
                        if self.queries.waiting_len() == waiting_before {
                            self.pending_retries.insert(retry_key, entry);
                        }
                        continue;
                    }
                }
            }
            self.pending_requests.insert(entry.request.id, entry.kind);
            commands.push(AppCommand::Rpc(entry.request));
        }
        commands
    }

    /// Outstanding deferred requests (`turn.send`, `turn.wait`,
    /// `turn.result`, `session.compact`) counted against the local 16-slot
    /// target (spec §5.3/§21). Read-only queries have their own two slots.
    fn deferred_pending(&self) -> usize {
        self.pending_requests
            .values()
            .filter(|kind| {
                matches!(
                    kind,
                    RequestKind::SendTurn { .. }
                        | RequestKind::WaitTurn(_)
                        | RequestKind::TurnResult(_)
                        | RequestKind::Compact { .. }
                )
            })
            .count()
            + self
                .pending_retries
                .values()
                .filter(|entry| {
                    matches!(
                        entry.kind,
                        RequestKind::SendTurn { .. }
                            | RequestKind::WaitTurn(_)
                            | RequestKind::TurnResult(_)
                            | RequestKind::Compact { .. }
                    )
                })
                .count()
    }

    /// Whether a new deferred request fits the local target. A refused request
    /// is retained as a retry intent by its caller, never silently dropped.
    fn deferred_admission_ok(&self) -> bool {
        self.deferred_pending() + self.queries.in_flight_len() < MAX_DEFERRED_REQUESTS
    }

    /// Issues the next `session.read` page for one session. The cursor is
    /// always the request's own, never recomputed from local item count
    /// (spec §6.3). Admission goes through the two read-only slots: a second
    /// chain for the same view coalesces, and a third concurrent subject is
    /// refused rather than queued (spec §5.3).
    fn request_read(&mut self, session_id: &SessionId, read: ReadRequest) -> Option<AppCommand> {
        let cursor = read.cursor;
        let pin = read.pin.clone();
        let probe = read.probe;
        let generation = self
            .sessions
            .known
            .get(session_id)
            .map_or(0, |view| view.history_query_generation);
        let key = crate::app::queries::QueryKey::History {
            session_id: session_id.clone(),
            generation,
        };
        let build = move |id: RequestId| {
            let (limit, max_bytes) = if probe {
                (
                    crate::protocol::READ_PROBE_LIMIT,
                    crate::protocol::READ_PROBE_MAX_BYTES,
                )
            } else {
                (READ_PAGE_LIMIT, READ_PAGE_MAX_BYTES)
            };
            OutgoingRequest::session_read(
                id,
                session_id,
                Some(cursor),
                limit,
                max_bytes,
                pin.as_ref(),
            )
        };
        if self.reload.is_some() {
            let id = self.next_request_id();
            let request = build(id);
            self.pending_requests.insert(id, RequestKind::StaleRead);
            return Some(AppCommand::Rpc(request));
        }
        // The slot is claimed before the request is built so a full budget
        // never leaks a registered request id.
        let id = self.next_request_id();
        match self.queries.request_query(key, id) {
            crate::app::queries::QueryAdmission::Admitted => {}
            crate::app::queries::QueryAdmission::Coalesced
            | crate::app::queries::QueryAdmission::Busy => return None,
        }
        let request = build(id);
        self.pending_requests.insert(
            id,
            RequestKind::History {
                session_id: session_id.clone(),
                read,
            },
        );
        Some(AppCommand::Rpc(request))
    }

    /// Starts a catalog-only configuration reload (spec §9). The `agent.reload`
    /// ACK is followed by the three catalog reads; no session state,
    /// presentation, or history read is issued here, and no live loop, draft,
    /// or selection is touched.
    fn reload(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(
                NoticeLevel::Info,
                "configuration reload is already in progress",
            );
            return Vec::new();
        }
        let generation = self.next_reload_generation;
        self.next_reload_generation = self
            .next_reload_generation
            .checked_add(1)
            .expect("reload generations exhausted");
        self.reload = Some(ReloadProgress::new(generation));
        vec![self.request(RequestKind::Reload { generation }, OutgoingRequest::reload)]
    }

    fn reload_failed(&mut self, generation: u64, message: impl Into<String>) -> Vec<AppCommand> {
        let Some(reload) = self.reload.as_ref() else {
            return Vec::new();
        };
        if reload.generation != generation {
            return Vec::new();
        }
        let acknowledged = reload.acknowledged;
        self.reload = None;
        let detail = message.into();
        let message = if acknowledged {
            format!("Agent configuration reloaded; catalog refresh incomplete or failed: {detail}")
        } else if detail == "agent.reload returned {ok:false}; configuration was not applied" {
            detail
        } else {
            format!("configuration reload outcome is unknown; no automatic retry: {detail}")
        };
        self.notice(NoticeLevel::Warning, message);
        self.resume_uncalibrated_sessions()
    }

    fn on_reload_response(&mut self, generation: u64, response: &RpcResponse) -> Vec<AppCommand> {
        if self
            .reload
            .as_ref()
            .is_none_or(|reload| reload.generation != generation)
        {
            return Vec::new();
        }
        match response.parse_reload() {
            Ok(result) if result.ok => {
                if let Some(reload) = self.reload.as_mut() {
                    reload.acknowledged = true;
                }
                vec![
                    self.request(
                        RequestKind::ReloadModels { generation },
                        OutgoingRequest::list_models,
                    ),
                    self.request(
                        RequestKind::ReloadProfiles { generation },
                        OutgoingRequest::list_profiles,
                    ),
                    self.request_session_catalog(
                        RequestKind::ReloadSessions { generation },
                        OutgoingRequest::list_sessions,
                    ),
                ]
            }
            Ok(_) => self.reload_failed(
                generation,
                "agent.reload returned {ok:false}; configuration was not applied",
            ),
            Err(error) => self.reload_failed(
                generation,
                format!("agent.reload response was not exactly {{ok:true}}: {error}"),
            ),
        }
    }

    fn on_reload_models_response(
        &mut self,
        generation: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self
            .reload
            .as_ref()
            .is_none_or(|reload| reload.generation != generation)
        {
            return Vec::new();
        }
        match response.parse_models() {
            Ok(result) => {
                let mut ids = HashSet::new();
                if result
                    .models
                    .iter()
                    .any(|model| !ids.insert(model.id.clone()))
                {
                    return self.reload_failed(
                        generation,
                        "configuration reload returned duplicate model IDs",
                    );
                }
                if let Some(reload) = self.reload.as_mut() {
                    reload.models = Some(result.models);
                }
                self.maybe_finish_reload()
            }
            Err(error) => self.reload_failed(
                generation,
                format!("configuration reload model catalog failed: {error}"),
            ),
        }
    }

    fn on_reload_profiles_response(
        &mut self,
        generation: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self
            .reload
            .as_ref()
            .is_none_or(|reload| reload.generation != generation)
        {
            return Vec::new();
        }
        match response.parse_profiles() {
            Ok(result) => {
                let mut ids = HashSet::new();
                if result
                    .profiles
                    .iter()
                    .any(|profile| !ids.insert(profile.id.clone()))
                {
                    return self.reload_failed(
                        generation,
                        "configuration reload returned duplicate profile IDs",
                    );
                }
                if let Some(reload) = self.reload.as_mut() {
                    reload.profiles = Some(result.profiles);
                }
                self.maybe_finish_reload()
            }
            Err(error) => self.reload_failed(
                generation,
                format!("configuration reload profile catalog failed: {error}"),
            ),
        }
    }

    fn maybe_finish_reload(&mut self) -> Vec<AppCommand> {
        let complete = self.reload.as_ref().is_some_and(|reload| {
            reload.models.is_some() && reload.profiles.is_some() && reload.sessions.is_some()
        });
        if !complete {
            return Vec::new();
        }
        if let Some(error) = self.reload_catalog_error() {
            let generation = self
                .reload
                .as_ref()
                .expect("reload is present while validating")
                .generation;
            return self.reload_failed(generation, error);
        }
        let reload = self.reload.take().expect("reload completion was checked");
        self.apply_reload(reload)
    }

    fn apply_reload(&mut self, reload: ReloadProgress) -> Vec<AppCommand> {
        // Catalog-only install (spec §9): the reload refreshes models, profiles,
        // and session metadata. It never installs a staged session state or
        // presentation, never fences an unrelated read, and never patches a
        // live loop, draft, or selection. Sessions whose lifecycle ACK crossed
        // this barrier stay uncalibrated and resume through the normal gap
        // chain below.
        self.catalogs.models = reload.models.expect("complete reload has models");
        self.catalogs.profiles = reload.profiles.expect("complete reload has profiles");
        self.catalogs.loaded = true;
        self.refresh_catalog_seats();

        let mut sessions: Vec<SessionInfo> = reload.sessions.expect("complete reload has sessions");
        for session in &mut sessions {
            let preserve_info = self
                .sessions
                .known
                .get(&session.session_id)
                .is_some_and(|view| {
                    view.config_update.is_some() || view.closing || view.close_verification_unknown
                });
            if preserve_info {
                if let Some(view) = self.sessions.known.get(&session.session_id) {
                    *session = view.info.clone();
                }
            } else if self.sessions.closed.contains(&session.session_id) {
                session.loaded = false;
            }
        }
        for session in &sessions {
            if let Some(view) = self.sessions.known.get_mut(&session.session_id) {
                view.info = session.clone();
            } else {
                self.sessions.known.insert(
                    session.session_id.clone(),
                    self.new_session_view(session.clone()),
                );
            }
        }
        // The active session stays selected even when the refreshed catalog
        // does not list it (e.g. an unloaded session).
        if let Some(active) = self.sessions.active.clone() {
            if !sessions.iter().any(|session| session.session_id == active)
                && !self.session_absent(&active)
                && !self.sessions.pending_deletes.contains(&active)
            {
                if let Some(view) = self.sessions.known.get(&active) {
                    sessions.push(view.info.clone());
                }
            }
        }
        self.sessions.list = sessions;
        self.catalogs.pending_deletions.clear();
        self.reconcile_session_selection(true);

        let commands = self.resume_uncalibrated_sessions();
        self.prepared_conversation = None;
        self.notice(
            NoticeLevel::Info,
            "Agent configuration and session metadata reloaded",
        );
        commands
    }

    fn is_prior_loop(view: &SessionView, loop_id: &str) -> bool {
        view.retired_loop
            .as_ref()
            .is_some_and(|turn| turn.loop_id == loop_id)
            || view
                .last_result
                .as_ref()
                .is_some_and(|result| result.turn.loop_id == loop_id)
            || view
                .unsaved_loop
                .as_ref()
                .is_some_and(|unsaved| unsaved.turn.loop_id == loop_id)
            || view
                .transcript
                .window
                .items()
                .any(|(_, item)| match item.as_ref() {
                    TranscriptBlock::User(user) => user.loop_id.as_deref() == Some(loop_id),
                    TranscriptBlock::Assistant(assistant) => assistant.loop_id == loop_id,
                    TranscriptBlock::Tool(tool) => tool.loop_id == loop_id,
                    _ => false,
                })
    }

    /// Allocates an id, registers the pending kind, and builds the request
    /// via `build`. The pending entry exists before the command can leave
    /// `update`; the builder runs inside so the id cannot escape before
    /// registration.
    fn request(
        &mut self,
        kind: RequestKind,
        build: impl FnOnce(RequestId) -> OutgoingRequest,
    ) -> AppCommand {
        let id = self.next_request_id();
        let request = build(id);
        self.pending_requests.insert(id, kind);
        AppCommand::Rpc(request)
    }

    pub(crate) fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.push_notice(Notice::at(level, text.into(), false, self.instant_now()));
    }

    fn sticky_notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.push_notice(Notice::at(level, text.into(), true, self.instant_now()));
    }

    fn push_notice(&mut self, notice: Notice) {
        let mut notice = notice;
        if notice.text.len() > MAX_NOTICE_BYTES {
            let mut end = MAX_NOTICE_BYTES.saturating_sub(1);
            while end > 0 && !notice.text.is_char_boundary(end) {
                end -= 1;
            }
            notice.text.truncate(end);
            notice.text.push('…');
        }
        self.notices.push_back(notice);
        while self.notices.len() > MAX_NOTICES {
            self.notices.pop_front();
        }
    }

    // ---- bootstrap -----------------------------------------------------

    fn bootstrap(&mut self) -> Vec<AppCommand> {
        if self.connection != ConnectionState::Starting || !self.pending_requests.is_empty() {
            return Vec::new();
        }
        let ping = self.request(RequestKind::Ping, OutgoingRequest::ping);
        let models = self.request(RequestKind::ListModels, OutgoingRequest::list_models);
        let profiles = self.request(RequestKind::ListProfiles, OutgoingRequest::list_profiles);
        let sessions =
            self.request_session_catalog(RequestKind::ListSessions, OutgoingRequest::list_sessions);
        vec![ping, models, profiles, sessions]
    }

    fn bootstrap_progress(&mut self, part: BootstrapPart) -> Vec<AppCommand> {
        match part {
            BootstrapPart::Ping => self.bootstrap.ping = true,
            BootstrapPart::Models => self.bootstrap.models = true,
            BootstrapPart::Profiles => self.bootstrap.profiles = true,
            BootstrapPart::Sessions => self.bootstrap.sessions = true,
        }
        if self.bootstrap.done() && self.connection == ConnectionState::Starting {
            self.catalogs.loaded = true;
            self.connection = ConnectionState::Ready;
            self.blocked_notice = false;
            self.catalogs.seed_seats(&self.sessions.known);
            // CLI startup intent wins over the pre-filled new-session form:
            // opening or continuing an existing session is explicit.
            match self.startup_session.take() {
                Some(StartupSession::Exact(session_id)) => {
                    self.open_new_session_on_ready = false;
                    return self.open_session(&session_id);
                }
                Some(StartupSession::ContinueCurrentWorkspace) => {
                    self.open_new_session_on_ready = false;
                    return self.continue_startup_session();
                }
                None => {}
            }
            if self.open_new_session_on_ready && self.sessions.active.is_none() {
                self.open_new_session_on_ready = false;
                self.open_new_session();
            }
        }
        Vec::new()
    }

    /// `--continue`: the most recently updated session whose workspace is
    /// exactly the current one. A miss opens the selector instead of
    /// guessing across projects (spec §6.1).
    fn continue_startup_session(&mut self) -> Vec<AppCommand> {
        let workspace = self
            .catalogs
            .default_workspace
            .to_string_lossy()
            .into_owned();
        let candidate = crate::state::selection::filtered_sessions(&self.sessions.list, "")
            .into_iter()
            .find(|session| session.workspace == workspace)
            .map(|session| session.session_id.clone());
        match candidate {
            Some(session_id) => self.open_session(&session_id),
            None => {
                self.notice(
                    NoticeLevel::Info,
                    format!("no session in {workspace} to continue; choose one"),
                );
                self.open_selector(SelectorKind::Session)
            }
        }
    }

    fn guard_ready(&mut self) -> bool {
        if self.connection == ConnectionState::Ready {
            true
        } else {
            if !self.blocked_notice {
                self.notice(
                    NoticeLevel::Info,
                    "That action is unavailable until the agent is connected.",
                );
                self.blocked_notice = true;
            }
            false
        }
    }

    pub fn request_shutdown(&mut self) -> Vec<AppCommand> {
        if matches!(self.connection, ConnectionState::Failed(_)) {
            return vec![AppCommand::Exit];
        }
        if self.connection == ConnectionState::ShuttingDown {
            return Vec::new();
        }
        let now = self.instant_now();
        self.shutdown_deadline = Some(now + SHUTDOWN_TIMEOUT);
        self.reload = None;
        self.connection = ConnectionState::ShuttingDown;
        if self.shutdown_sent {
            return Vec::new();
        }
        self.shutdown_sent = true;
        vec![self.request(RequestKind::Shutdown, OutgoingRequest::shutdown)]
    }

    fn bootstrap_failure(&mut self, method: &str, error: RpcResponseError) -> Vec<AppCommand> {
        self.connection_terminated(&startup_error_message(method, &error))
    }

    fn on_create_response(&mut self, draft_id: u64, response: &RpcResponse) -> Vec<AppCommand> {
        let session = match response.parse_session() {
            Ok(result) => result.session,
            Err(error) => {
                if let Some(draft) = self.draft_matching(draft_id) {
                    draft.submitting = false;
                    draft.error = Some(format!("{error}"));
                } else {
                    self.notice(
                        NoticeLevel::Error,
                        format!("failed to create session: {error}"),
                    );
                }
                return Vec::new();
            }
        };
        let session_id = session.session_id.clone();
        if self
            .draft
            .as_ref()
            .is_some_and(|draft| draft.draft_id == draft_id)
        {
            self.draft = None;
        }
        if matches!(&self.dock, Dock::NewSession(draft) if draft.draft_id == draft_id) {
            self.dock = Dock::Composer;
        }
        if !self.sessions.known.contains_key(&session_id) {
            self.sessions
                .known
                .insert(session_id.clone(), self.new_session_view(session.clone()));
        }
        self.on_session_response(session_id, response)
    }

    fn on_rpc_event(&mut self, event: RpcEvent) -> Vec<AppCommand> {
        match event {
            RpcEvent::Frame(frame) => self.on_frame(frame),
            RpcEvent::AgentStderr { bytes, dropped } => {
                self.push_stderr(bytes, dropped);
                Vec::new()
            }
            RpcEvent::ConnectionClosed => {
                if self.connection == ConnectionState::ShuttingDown {
                    Vec::new()
                } else if self.connection == ConnectionState::Starting {
                    self.connection_terminated(
                        "Agent configuration rejected or Agent exited before the protocol handshake",
                    )
                } else {
                    self.connection_terminated("agent stdout closed unexpectedly")
                }
            }
            RpcEvent::ProtocolError(error) => {
                if self.connection == ConnectionState::ShuttingDown {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("RPC protocol error during shutdown: {error}"),
                    );
                    vec![AppCommand::KillChild]
                } else if self.connection == ConnectionState::Starting {
                    self.connection_terminated(&format!("protocol error during startup: {error}"))
                } else {
                    self.connection_terminated(&format!("RPC protocol error: {error}"))
                }
            }
            RpcEvent::Exited(status) => {
                let text = match &status {
                    Some(status) => status.code().map_or_else(
                        || "terminated without an exit code".to_owned(),
                        |code| format!("exit code {code}"),
                    ),
                    None => "unavailable".to_owned(),
                };
                self.child_exit_status = Some(text.clone());
                if self.connection == ConnectionState::ShuttingDown {
                    // Child exit does not prove that stdout's already-buffered
                    // shutdown/wait responses have been delivered. Keep
                    // draining until the RPC producers close the channel.
                    self.shutdown_child_exited = true;
                    Vec::new()
                } else if matches!(self.connection, ConnectionState::Failed(_)) {
                    Vec::new()
                } else if self.connection == ConnectionState::Starting {
                    self.connection_terminated(&format!(
                        "Agent configuration rejected or Agent exited before the protocol handshake ({text})"
                    ))
                } else {
                    self.connection_terminated(&format!("agent exited: {text}"))
                }
            }
        }
    }

    fn on_rpc_channel_ended(&mut self) -> Vec<AppCommand> {
        if self.connection == ConnectionState::ShuttingDown {
            self.shutdown_child_exited = true;
            return vec![AppCommand::Exit];
        }
        if self.connection == ConnectionState::Starting {
            self.connection_terminated(
                "Agent configuration rejected or Agent exited before the protocol handshake",
            )
        } else {
            self.connection_terminated("agent RPC channel closed unexpectedly")
        }
    }

    fn connection_terminated(&mut self, reason: &str) -> Vec<AppCommand> {
        if matches!(self.connection, ConnectionState::Failed(_)) {
            return Vec::new();
        }
        let mut unconfirmed = false;
        let reload_in_progress = self.reload.is_some();
        let reload_acknowledged = self
            .reload
            .as_ref()
            .is_some_and(|reload| reload.acknowledged);
        let mut reload_sessions = HashSet::new();
        if reload_in_progress {
            reload_sessions = self.pending_lifecycle_session_ids();
            if let Some(session_id) = self.sessions.active.clone() {
                reload_sessions.insert(session_id);
            }
        }
        self.reload = None;
        self.pending_requests.clear();
        // Every in-flight read slot is released with the connection; a late
        // response for a retired request must not corrupt the counters.
        self.queries = crate::app::queries::QuerySlots::new();
        self.pending_query_followups.clear();
        for session_id in reload_sessions {
            self.mark_session_uncalibrated(&session_id);
        }
        for view in self.sessions.known.values_mut() {
            Self::mark_pending_steers_unconfirmed(view);
            // Transport loss pauses the unsent queue: never auto-send or
            // retry an ambiguous accepted message.
            view.steer_queue_paused = true;
            if view.unsaved_loop.is_none() {
                if let Some(live) = view.live.as_mut() {
                    if live.last_result.is_none() {
                        live.waiting = true;
                        view.result_confirmation = ResultConfirmation::Unknown;
                        unconfirmed = true;
                    }
                }
            }
        }
        if let Dock::SessionSelector(state) = &mut self.dock {
            if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                if *submitting {
                    *submitting = false;
                    state.error = Some(
                        "rename outcome is unknown; reread the session before retrying".to_owned(),
                    );
                }
            }
        }
        self.connection = ConnectionState::Failed(reason.to_owned());
        if reload_in_progress {
            let text = if reload_acknowledged {
                format!(
                    "Agent configuration reloaded; catalog refresh outcome is unknown; no automatic retry: {reason}"
                )
            } else {
                format!("configuration reload outcome is unknown; no automatic retry: {reason}")
            };
            self.notice(NoticeLevel::Error, text);
        } else {
            self.notice(NoticeLevel::Error, reason.to_owned());
        }
        if unconfirmed {
            self.sticky_notice(NoticeLevel::Warning, UNCONFIRMED_RESULT_NOTICE);
        }
        Vec::new()
    }

    fn on_frame(&mut self, frame: IncomingFrame) -> Vec<AppCommand> {
        match frame {
            IncomingFrame::Response(response) => self.on_response(response),
            IncomingFrame::Notification(notification) => self.on_notification(notification),
        }
    }

    fn on_response(&mut self, response: RpcResponse) -> Vec<AppCommand> {
        let kind = match self.pending_requests.remove(&response.id) {
            Some(kind) => kind,
            None => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("response for unknown request id {}", response.id.0),
                );
                return Vec::new();
            }
        };
        if let Some(key) = Self::retry_key(&kind) {
            self.pending_retries.remove(&key);
        }
        self.free_query_slot(response.id);
        // A delete purges every pending request for the session, so a late
        // response is normally dropped as an unknown request id. The bounded
        // deletion window additionally rejects a response whose id was
        // re-registered after the delete; the stale list protection itself is
        // the catalog generation, not a permanent tombstone set.
        if Self::request_session_id(&kind)
            .is_some_and(|session_id| self.session_pending_deletion(session_id))
        {
            return Vec::new();
        }
        if self.connection == ConnectionState::ShuttingDown
            && !matches!(
                kind,
                RequestKind::Shutdown | RequestKind::WaitTurn(_) | RequestKind::SendTurn { .. }
            )
        {
            return Vec::new();
        }
        match kind {
            RequestKind::Changes {
                session_id,
                epoch,
                generation,
                diff,
            } => self.on_changes_response(&session_id, epoch, generation, diff, &response),
            RequestKind::WorkspaceStatus {
                session_id,
                epoch,
                generation,
            } => self.on_workspace_status(&session_id, epoch, generation, &response),
            RequestKind::Workspace {
                session_id,
                epoch,
                generation,
                kind,
            } => self.on_workspace_response(session_id, epoch, generation, kind, &response),
            RequestKind::ToolDetail {
                key,
                epoch,
                generation,
                stream,
            } => self.on_tool_detail_response(key, epoch, generation, stream, &response),
            RequestKind::StaleRead => Vec::new(),
            RequestKind::TurnResult(turn) => self.on_turn_result_response(&turn, &response),
            RequestKind::Reload { generation } => self.on_reload_response(generation, &response),
            RequestKind::Ping => {
                match response.parse_ping() {
                    Ok(pong) => {
                        if let Err(error) = validate_backend(&pong) {
                            let msg = format!("protocol incompatible during startup: {error}");
                            self.notice(NoticeLevel::Error, &msg);
                            self.connection = ConnectionState::Failed(msg);
                            return Vec::new();
                        }
                    }
                    Err(err) => {
                        let msg = startup_error_message(METHOD_PING, &err);
                        self.notice(NoticeLevel::Error, &msg);
                        self.connection = ConnectionState::Failed(msg);
                        return Vec::new();
                    }
                }
                self.bootstrap_progress(BootstrapPart::Ping)
            }
            RequestKind::ListModels => match response.parse_models() {
                Ok(result) => {
                    self.catalogs.models = result.models;
                    self.bootstrap_progress(BootstrapPart::Models)
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_MODELS, error),
            },
            RequestKind::ListProfiles => match response.parse_profiles() {
                Ok(result) => {
                    self.catalogs.profiles = result.profiles;
                    self.bootstrap_progress(BootstrapPart::Profiles)
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_PROFILES, error),
            },
            RequestKind::ReloadModels { generation } => {
                self.on_reload_models_response(generation, &response)
            }
            RequestKind::ReloadProfiles { generation } => {
                self.on_reload_profiles_response(generation, &response)
            }
            RequestKind::ReloadSessions { generation } => {
                if !self.session_catalog_is_current(response.id) {
                    if self
                        .reload
                        .as_ref()
                        .is_some_and(|reload| reload.generation == generation)
                    {
                        return vec![self.request_session_catalog(
                            RequestKind::ReloadSessions { generation },
                            OutgoingRequest::list_sessions,
                        )];
                    }
                    return Vec::new();
                }
                self.on_reload_sessions_response(generation, &response)
            }
            RequestKind::ListSessions => {
                if !self.session_catalog_is_current(response.id) {
                    return self.refresh_session_catalog();
                }
                match response.parse_sessions() {
                    Ok(result) => {
                        let sessions = result.sessions;
                        // A current-generation list is the authoritative
                        // confirmation: the bounded deletion window is over.
                        self.catalogs.pending_deletions.clear();
                        self.sessions.list = sessions.clone();
                        for session in sessions {
                            let session_id = session.session_id.clone();
                            let view = self.new_session_view(session);
                            self.sessions.known.entry(session_id).or_insert(view);
                        }
                        self.bootstrap_progress(BootstrapPart::Sessions)
                    }
                    Err(error) => self.bootstrap_failure(METHOD_LIST_SESSIONS, error),
                }
            }
            RequestKind::RefreshSessions {
                selected_session_id,
            } => {
                if !self.session_catalog_is_current(response.id) {
                    // The response predates a local create/open/rename/close/
                    // delete: re-issue instead of resurrecting old metadata.
                    if !self.can_send_requests() {
                        return Vec::new();
                    }
                    return vec![self.request_session_catalog(
                        RequestKind::RefreshSessions {
                            selected_session_id,
                        },
                        OutgoingRequest::list_sessions,
                    )];
                }
                self.on_refresh_sessions_response(&response)
            }
            RequestKind::CreateSession { draft } => self.on_create_response(draft, &response),
            RequestKind::OpenSession {
                session_id,
                previous_retired_loop,
            } => self.on_open_response(session_id, previous_retired_loop, &response),
            RequestKind::SessionState { session_id, query } => {
                self.on_session_state_response(&session_id, query, &response)
            }
            RequestKind::SessionPresentation { session_id } => {
                self.on_session_presentation_response(&session_id, &response)
            }
            RequestKind::SessionContext {
                session_id,
                generation,
                owner,
            } => self.on_session_context_response(&session_id, generation, owner, &response),
            RequestKind::Compact {
                session_id,
                operation_id,
            } => self.on_compact_response(&session_id, &operation_id, &response),
            RequestKind::CompactCancel {
                session_id,
                operation_id,
            } => self.on_compact_cancel_response(&session_id, &operation_id, &response),
            RequestKind::History { session_id, read } => {
                self.on_history_response(&session_id, &read, &response)
            }
            RequestKind::SearchRead {
                session_id,
                generation,
            } => self.on_search_read_response(&session_id, generation, &response),
            RequestKind::ExportRead {
                session_id,
                export_id,
            } => self.on_export_read_response(&session_id, export_id, &response),
            RequestKind::SendTurn {
                session_id,
                local_submission,
            } => self.on_send_response(&session_id, local_submission, &response),
            RequestKind::WaitTurn(turn) => self.on_wait_response(turn, &response),
            RequestKind::SteerTurn {
                session_id,
                loop_id,
                steer_id,
                text,
                editor_revision,
            } => self.on_steer_response(
                &session_id,
                &loop_id,
                steer_id,
                &text,
                editor_revision,
                &response,
            ),
            RequestKind::CancelTurn(_) => self.on_cancel_response(&response),
            RequestKind::UpdateSession {
                session_id,
                loop_id,
                model,
                reasoning,
            } => self.on_update_session_response(session_id, loop_id, model, reasoning, &response),
            RequestKind::RenameSession { session_id } => {
                self.on_rename_session_response(session_id, &response)
            }
            RequestKind::CloseSession { session_id } => {
                self.on_close_session_response(&session_id, &response)
            }
            RequestKind::CloseVerifyState { session_id } => {
                self.on_close_verify_state_response(&session_id, &response)
            }
            RequestKind::DeleteSession { session_id } => {
                self.on_delete_session_response(&session_id, &response)
            }
            RequestKind::Shutdown => match response.parse_shutdown() {
                Ok(_) => Vec::new(),
                Err(error) => {
                    self.notice(
                        NoticeLevel::Error,
                        format!("agent.shutdown failed: {error}"),
                    );
                    vec![AppCommand::KillChild]
                }
            },
        }
    }

    fn on_notification(&mut self, notification: RpcNotification) -> Vec<AppCommand> {
        match notification {
            RpcNotification::AgentEvent(event) => self.on_agent_event(event),
            RpcNotification::Unknown { .. } => Vec::new(),
        }
    }

    fn on_agent_event(&mut self, event: AgentEventWire) -> Vec<AppCommand> {
        let event_session_id = match &event {
            AgentEventWire::SessionOpened { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::SessionClosed { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::SessionState { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::TurnStarted { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::RequestStarted { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::RequestUsage { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::SteerProgress { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::OutputDelta { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolStarted { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolPresentation { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolProgress { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolInvocation { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolExecution { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolProcess { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::ToolFinished { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::InteractionRequested { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::InteractionResolved { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::TurnFinished { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::Unknown => None,
        };
        if event_session_id.is_some_and(|session_id| {
            self.session_pending_deletion(session_id)
                || self.sessions.pending_deletes.contains(session_id)
        }) {
            return Vec::new();
        }
        if let AgentEventWire::SessionOpened { data } = &event {
            if self.session_pending_deletion(&data.session.session_id)
                || self
                    .sessions
                    .pending_deletes
                    .contains(&data.session.session_id)
            {
                return Vec::new();
            }
        }
        let first_binding_turn = Self::loop_event_turn(&event).filter(|turn| {
            self.sessions
                .known
                .get(&turn.session_id)
                .is_some_and(|view| {
                    view.live
                        .as_ref()
                        .is_some_and(|live| live.reference.is_none())
                })
        });
        let gap_session = match &event {
            AgentEventWire::SessionOpened { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::SessionClosed { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::SessionState { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::TurnStarted { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::RequestStarted { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::RequestUsage { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::SteerProgress { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::OutputDelta { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolStarted { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolPresentation { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolProgress { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolInvocation { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolExecution { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolProcess { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolFinished { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::InteractionRequested { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::InteractionResolved { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::TurnFinished { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::Unknown => None,
        };
        let mut commands = Vec::new();
        match event {
            AgentEventWire::SessionOpened { data } => {
                let session_id = data.session.session_id.clone();
                let open_pending = self.pending_requests.values().any(|kind| {
                    matches!(kind, RequestKind::OpenSession { session_id: pending, .. } if pending == &session_id)
                });
                let (needs_state, info_changed) = match self.sessions.known.get_mut(&session_id) {
                    Some(view) => {
                        // Existing SessionInfo came from list/open/update and
                        // is authoritative over this best-effort event. The
                        // event may only request missing state information.
                        (view.state.is_none() && !open_pending, false)
                    }
                    None => {
                        self.sessions.known.insert(
                            session_id.clone(),
                            self.new_session_view(data.session.clone()),
                        );
                        (!open_pending, true)
                    }
                };
                if info_changed {
                    let listed_info = self
                        .sessions
                        .known
                        .get(&session_id)
                        .map(|view| view.info.clone());
                    if let Some(info) = listed_info {
                        self.upsert_session_list(info);
                    }
                }
                if info_changed {
                    self.arm_workspace_status(&session_id, true);
                }
                if needs_state
                    && self.reload.is_none()
                    && self.can_send_requests()
                    && self
                        .sessions
                        .known
                        .get(&session_id)
                        .is_none_or(|view| view.latest_state_query.is_none())
                {
                    commands.push(self.request_session_state(&session_id));
                }
                self.mark_gap(&data.meta);
            }
            AgentEventWire::SessionClosed { data } => {
                // This best-effort event is not a completion or persistence
                // proof; retain live/result state until the lifecycle owner
                // explicitly reopens the session.
                self.mark_gap(&data.meta);
            }
            AgentEventWire::SessionState { data } => {
                self.mark_gap(&data.meta);
                self.apply_session_state(
                    &data.state,
                    data.meta.loop_id.as_ref(),
                    SessionStateSource::Notification,
                );
            }
            AgentEventWire::TurnStarted { data } => {
                self.mark_gap(&data.meta);
                self.adopt_turn_started(&data.turn);
                // A fresh turn supersedes the previous loop's live per-request
                // usage rows (they now belong to persisted history/report).
                if let Some(view) = self.sessions.known.get_mut(&data.turn.session_id) {
                    view.discard_live_request_usage();
                }
                if let Some(command) = self.request_session_presentation(&data.turn.session_id) {
                    commands.push(command);
                }
            }
            AgentEventWire::RequestStarted { data } => {
                self.mark_gap(&data.meta);
                self.on_request_started(
                    &data.turn,
                    data.request_index,
                    data.config_revision,
                    &data.model,
                    data.reasoning,
                );
                if let Some(command) = self.request_session_presentation(&data.turn.session_id) {
                    commands.push(command);
                }
            }
            AgentEventWire::RequestUsage { data } => {
                self.mark_gap(&data.meta);
                self.on_request_usage(&data.turn, data.request_index, data.usage);
            }
            AgentEventWire::SteerProgress { data } => {
                self.mark_gap(&data.meta);
                self.on_steer_progress(&data.turn, data.request_index, data.applied_count);
            }
            AgentEventWire::OutputDelta { data } => {
                self.mark_gap(&data.meta);
                self.append_delta(&data.turn, data.request_index, &data.channel, &data.delta);
            }
            AgentEventWire::ToolStarted { data } => {
                self.mark_gap(&data.meta);
                self.on_tool_started(
                    &data.turn,
                    data.request_index,
                    &data.tool_call_id,
                    &data.tool_name,
                );
            }
            AgentEventWire::ToolPresentation { data } => {
                self.mark_gap(&data.meta);
                self.on_tool_presentation(
                    &data.turn,
                    data.request_index,
                    &data.tool_call_id,
                    &data.tool_name,
                    data.display,
                );
            }
            AgentEventWire::ToolProgress { data } => {
                self.mark_gap(&data.meta);
                self.on_tool_progress(
                    &data.turn,
                    data.request_index,
                    &data.tool_call_id,
                    &data.progress,
                );
            }
            AgentEventWire::ToolInvocation { data } => {
                self.mark_gap(&data.meta);
                self.accept_tool_invocation(data.data);
            }
            AgentEventWire::ToolExecution { data } => {
                self.mark_gap(&data.meta);
                self.accept_tool_execution(data.data, false);
            }
            AgentEventWire::ToolProcess { data } => {
                self.mark_gap(&data.meta);
                self.accept_tool_process(data.data);
            }
            AgentEventWire::ToolFinished { data } => {
                self.mark_gap(&data.meta);
                self.on_tool_finished(
                    &data.turn,
                    data.request_index,
                    &data.tool_call_id,
                    data.result.outcome,
                    data.result.content,
                    data.result.content_truncated,
                );
            }
            AgentEventWire::InteractionRequested { data } => {
                self.mark_gap(&data.meta);
                self.sticky_notice(NoticeLevel::Warning, UNSUPPORTED_INTERACTION_NOTICE);
            }
            AgentEventWire::InteractionResolved { data } => {
                self.mark_gap(&data.meta);
            }
            AgentEventWire::TurnFinished { data } => {
                self.mark_gap(&data.meta);
            }
            AgentEventWire::Unknown => {}
        }
        if let Some(turn) = first_binding_turn {
            if let Some(command) = self.maybe_request_state_after_turn_binding(&turn, true) {
                commands.push(command);
            }
        }
        if let Some(session_id) = gap_session {
            commands.extend(self.start_gap_reconcile(&session_id));
        }
        commands
    }

    /// Records one real per-request usage row from the Agent's live stream.
    /// Read-only and best-effort: a dropped or stale event only delays the
    /// footer hint; the persisted loop total remains the completion source.
    fn on_request_usage(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        usage: crate::protocol::UsageWire,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        // Stale events for a retired loop must not mutate the live view.
        if Self::is_prior_loop(view, &turn.loop_id) {
            return;
        }
        view.set_live_request_usage(&turn.loop_id, request_index, usage);
    }

    fn on_request_started(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        config_revision: u64,
        model: &str,
        reasoning: Reasoning,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::bind_live_turn(view, turn) {
            return;
        }
        let live = view.live.as_mut().expect("live turn was bound");
        let current_loop_id = live.reference.as_ref().map(|r| r.loop_id.clone());
        let evidence = {
            let request = live.ensure_request_mut(
                request_index,
                config_revision,
                model.to_owned(),
                reasoning,
            );
            request.config_revision = config_revision;
            request.model = model.to_owned();
            request.reasoning = reasoning;
            crate::state::session::RequestConfigEvidence {
                loop_id: current_loop_id.clone(),
                request_index,
                revision: config_revision,
                model: request.model.clone(),
                reasoning,
            }
        };
        if view
            .last_request
            .as_ref()
            .is_none_or(|last| request_index >= last.request_index)
        {
            view.last_request = Some(evidence.clone());
        }
        if let Some(update) = view.config_update.as_mut() {
            let loop_matches = match (&update.loop_id, &current_loop_id) {
                (Some(u_loop), Some(c_loop)) => u_loop == c_loop,
                (None, _) => true,
                _ => false,
            };
            if loop_matches
                && update.revision == Some(config_revision)
                && update
                    .model
                    .as_deref()
                    .is_none_or(|model| model == evidence.model)
                && update
                    .reasoning
                    .is_none_or(|level| level == evidence.reasoning)
            {
                update.state = crate::state::session::ConfigUpdateState::Applied;
            }
        }
    }

    fn append_delta(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        channel: &OutputChannelWire,
        delta: &str,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::bind_live_turn(view, turn) {
            return;
        }
        let request_missing = view.live.as_ref().is_some_and(|live| {
            !live
                .requests
                .iter()
                .any(|request| request.request_index == request_index)
        });
        if request_missing {
            view.event_gap = true;
        }
        let live = view.live.as_mut().expect("live turn was bound");
        live.event_gap |= request_missing;
        let request = live.ensure_request_mut(request_index, 0, String::new(), Reasoning::Auto);
        match channel {
            OutputChannelWire::Text => {
                append_live_part(request, LivePart::Text(delta.to_owned()));
            }
            OutputChannelWire::Reasoning => {
                append_live_part(request, LivePart::Reasoning(delta.to_owned()));
            }
        }
    }

    fn on_tool_started(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        tool_call_id: &str,
        tool_name: &str,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::bind_live_turn(view, turn) {
            return;
        }
        let request_missing = view.live.as_ref().is_some_and(|live| {
            !live
                .requests
                .iter()
                .any(|request| request.request_index == request_index)
        });
        if request_missing {
            view.event_gap = true;
        }
        let key = ToolKey::new(&turn.session_id, &turn.loop_id, request_index, tool_call_id);
        {
            let presentations = std::sync::Arc::make_mut(&mut view.tool_presentations);
            if let Some(facts) = presentations.get_mut(&key) {
                std::sync::Arc::make_mut(facts).accept_started(tool_name);
            } else {
                presentations.insert(
                    key.clone(),
                    std::sync::Arc::new(ToolPresentationState {
                        display: Arc::new(ToolDisplayWire {
                            detail: tool_name.to_owned(),
                            expanded_input: None,
                            input_line_count: None,
                            hidden_line_count: None,
                            truncated: false,
                        }),
                        result: None,
                        result_truncated: false,
                        status: ToolStatus::Running,
                        outcome: None,
                        needs_read: false,
                        input_available: false,
                        conflict: None,
                        invocation: None,
                        execution: None,
                        command: None,
                    }),
                );
            }
        }
        let presentation = view.tool_presentations.get(&key).cloned();
        let live = view.live.as_mut().expect("live turn was bound");
        live.event_gap |= request_missing;
        let request = live.ensure_request_mut(request_index, 0, String::new(), Reasoning::Auto);
        if !request
            .parts
            .iter()
            .any(|part| matches!(part, LivePart::Tool { tool_call_id: id } if id == tool_call_id))
        {
            request.parts.push(LivePart::Tool {
                tool_call_id: tool_call_id.to_owned(),
            });
        }
        if let Some(tool) = request
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id)
        {
            tool.name = tool_name.to_owned();
            if let Some(presentation) = &presentation {
                tool.status = presentation.status;
                tool.display = Some(presentation.display.clone());
                if tool.result.is_none() {
                    tool.result = presentation.result.as_ref().cloned();
                }
                tool.result_truncated |= presentation.result_truncated;
            }
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: tool_name.to_owned(),
                status: presentation
                    .as_ref()
                    .map_or(ToolStatus::Running, |state| state.status),
                progress: None,
                display: presentation.as_ref().map(|state| state.display.clone()),
                result: presentation
                    .as_ref()
                    .and_then(|state| state.result.as_ref().cloned()),
                result_truncated: presentation
                    .as_ref()
                    .is_some_and(|state| state.result_truncated),
                expanded: false,
            });
        }
    }

    fn on_tool_progress(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        tool_call_id: &str,
        progress: &ToolProgressWire,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::bind_live_turn(view, turn) {
            return;
        }
        let request_missing = view.live.as_ref().is_some_and(|live| {
            !live
                .requests
                .iter()
                .any(|request| request.request_index == request_index)
        });
        if request_missing {
            view.event_gap = true;
        }
        let tool_missing = view.live.as_ref().is_some_and(|live| {
            live.requests
                .iter()
                .find(|request| request.request_index == request_index)
                .is_none_or(|request| {
                    !request
                        .tools
                        .iter()
                        .any(|tool| tool.tool_call_id == tool_call_id)
                })
        });
        if tool_missing {
            view.event_gap = true;
        }
        let live = view.live.as_mut().expect("live turn was bound");
        live.event_gap |= request_missing || tool_missing;
        let request = live.ensure_request_mut(request_index, 0, String::new(), Reasoning::Auto);
        if !request
            .parts
            .iter()
            .any(|part| matches!(part, LivePart::Tool { tool_call_id: id } if id == tool_call_id))
        {
            request.parts.push(LivePart::Tool {
                tool_call_id: tool_call_id.to_owned(),
            });
        }
        let tool = request
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id);
        if let Some(tool) = tool {
            if matches!(tool.status, ToolStatus::Pending | ToolStatus::Running) {
                tool.status = ToolStatus::Running;
                if let Some(message) = &progress.message {
                    tool.progress = Some(message.clone());
                }
            }
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: "(unknown tool)".to_owned(),
                status: ToolStatus::Running,
                progress: progress.message.clone(),
                display: None,
                result: None,
                result_truncated: false,
                expanded: false,
            });
        }
    }

    fn on_tool_finished(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        tool_call_id: &str,
        outcome: ToolOutcomeWire,
        content: Option<String>,
        content_truncated: bool,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::bind_live_turn(view, turn) {
            return;
        }
        let shared_result = content
            .as_ref()
            .map(|result| Arc::<str>::from(result.as_str()));
        let request_missing = view.live.as_ref().is_some_and(|live| {
            !live
                .requests
                .iter()
                .any(|request| request.request_index == request_index)
        });
        if request_missing {
            view.event_gap = true;
        }
        let tool_missing = view.live.as_ref().is_some_and(|live| {
            live.requests
                .iter()
                .find(|request| request.request_index == request_index)
                .is_none_or(|request| {
                    !request
                        .tools
                        .iter()
                        .any(|tool| tool.tool_call_id == tool_call_id)
                })
        });
        if tool_missing {
            view.event_gap = true;
        }
        let live = view.live.as_mut().expect("live turn was bound");
        live.event_gap |= request_missing || tool_missing;
        let request = live.ensure_request_mut(request_index, 0, String::new(), Reasoning::Auto);
        if !request
            .parts
            .iter()
            .any(|part| matches!(part, LivePart::Tool { tool_call_id: id } if id == tool_call_id))
        {
            request.parts.push(LivePart::Tool {
                tool_call_id: tool_call_id.to_owned(),
            });
        }
        if let Some(tool) = request
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id)
        {
            tool.status = tool_outcome_status(outcome);
            tool.result = shared_result.clone();
            tool.result_truncated = content_truncated;
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: "(unknown tool)".to_owned(),
                status: tool_outcome_status(outcome),
                progress: None,
                display: None,
                result: shared_result.clone(),
                result_truncated: content_truncated,
                expanded: false,
            });
        }
        let fallback_name = view
            .live
            .as_ref()
            .and_then(|live| {
                live.requests
                    .iter()
                    .find(|request| request.request_index == request_index)
            })
            .and_then(|request| {
                request
                    .tools
                    .iter()
                    .find(|tool| tool.tool_call_id == tool_call_id)
            })
            .map(|tool| tool.name.clone())
            .unwrap_or_else(|| "(unknown tool)".to_owned());
        let key = ToolKey::new(&turn.session_id, &turn.loop_id, request_index, tool_call_id);
        let presentations = std::sync::Arc::make_mut(&mut view.tool_presentations);
        if let Some(presentation) = presentations.get_mut(&key) {
            let presentation = std::sync::Arc::make_mut(presentation);
            // A completed ToolPresentation event carries the authoritative
            // input+result hidden count. If it arrived before ToolFinished,
            // leave that count intact; the later result event only fills the
            // result side of the state.
            presentation.accept_finished(outcome, shared_result.clone(), content_truncated);
        } else {
            // Presentation is best effort. A result without its companion
            // event still gets a safe result-only card instead of losing the
            // tool from the live/history-shaped view.
            let hidden_line_count = content
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(|text| text.split('\n').count());
            presentations.insert(
                key.clone(),
                std::sync::Arc::new(ToolPresentationState {
                    display: Arc::new(ToolDisplayWire {
                        detail: fallback_name,
                        expanded_input: None,
                        input_line_count: None,
                        hidden_line_count,
                        truncated: content_truncated,
                    }),
                    result: shared_result,
                    result_truncated: content_truncated,
                    status: tool_outcome_status(outcome),
                    outcome: Some(outcome),
                    needs_read: false,
                    input_available: false,
                    conflict: None,
                    invocation: None,
                    execution: None,
                    command: None,
                }),
            );
        }
        let accepted = view.tool_presentations.get(&key).map(|facts| {
            (
                facts.status,
                facts.result.clone(),
                facts.result_truncated,
                facts.display.clone(),
            )
        });
        if let Some(live) = view.live.as_mut() {
            if let Some(request) = live
                .requests
                .iter_mut()
                .find(|request| request.request_index == request_index)
            {
                if let Some(tool) = request
                    .tools
                    .iter_mut()
                    .find(|tool| tool.tool_call_id == tool_call_id)
                {
                    if let Some((status, result, truncated, display)) = &accepted {
                        tool.status = *status;
                        tool.result = result.clone();
                        tool.result_truncated = *truncated;
                        tool.display = Some(display.clone());
                    }
                }
            }
        }
        if view.transcript.blocks.iter().any(|block| {
            matches!(
            block.as_ref(),
            TranscriptBlock::Tool(tool) if tool.loop_id == turn.loop_id
                && tool.request_index == request_index && tool.tool_call_id == tool_call_id)
        }) {
            view.transcript.invalidate();
        }
    }

    fn on_tool_presentation(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        tool_call_id: &str,
        tool_name: &str,
        display: ToolDisplayWire,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::bind_live_turn(view, turn) {
            return;
        }
        let key = ToolKey::new(&turn.session_id, &turn.loop_id, request_index, tool_call_id);
        let existing_result = view
            .live
            .as_ref()
            .and_then(|live| {
                live.requests
                    .iter()
                    .find(|request| request.request_index == request_index)
            })
            .and_then(|request| {
                request
                    .tools
                    .iter()
                    .find(|tool| tool.tool_call_id == tool_call_id)
            })
            .map(|tool| (tool.result.clone(), tool.result_truncated));
        let display_owner = Arc::new(display);
        let presentations = std::sync::Arc::make_mut(&mut view.tool_presentations);
        let state = presentations.entry(key).or_insert_with(|| {
            std::sync::Arc::new(ToolPresentationState {
                display: Arc::clone(&display_owner),
                result: existing_result
                    .as_ref()
                    .and_then(|(result, _)| result.clone()),
                result_truncated: existing_result
                    .as_ref()
                    .is_some_and(|(_, truncated)| *truncated),
                status: ToolStatus::Pending,
                outcome: None,
                needs_read: false,
                input_available: false,
                conflict: None,
                invocation: None,
                execution: None,
                command: None,
            })
        });
        let state = std::sync::Arc::make_mut(state);
        state.display = Arc::clone(&display_owner);
        let display_for_live = Arc::clone(&state.display);
        let _ = state;
        if view.transcript.blocks.iter().any(|block| {
            matches!(
            block.as_ref(),
            TranscriptBlock::Tool(tool) if tool.loop_id == turn.loop_id
                && tool.request_index == request_index && tool.tool_call_id == tool_call_id)
        }) {
            view.transcript.invalidate();
        }
        if let Some(live) = view.live.as_mut() {
            if let Some(request) = live
                .requests
                .iter_mut()
                .find(|request| request.request_index == request_index)
            {
                if let Some(tool) = request
                    .tools
                    .iter_mut()
                    .find(|tool| tool.tool_call_id == tool_call_id)
                {
                    tool.name = tool_name.to_owned();
                    tool.display = Some(display_for_live);
                }
            }
        }
    }
}

fn section_ids_match(
    left: &crate::state::view::SectionId,
    right: &crate::state::view::SectionId,
) -> bool {
    left.session_id == right.session_id
        && left.loop_id == right.loop_id
        && left.request_index == right.request_index
        && left.kind == right.kind
        && left.ordinal == right.ordinal
        && left.tool_call_id == right.tool_call_id
        && (left.history_index == right.history_index
            || left.history_index.is_none()
            || right.history_index.is_none())
}

fn append_live_part(request: &mut crate::state::turn::LiveRequest, part: LivePart) {
    match (request.parts.last_mut(), part) {
        (Some(LivePart::Text(existing)), LivePart::Text(delta))
        | (Some(LivePart::Reasoning(existing)), LivePart::Reasoning(delta)) => {
            existing.push_str(&delta);
        }
        (_, part) => request.parts.push(part),
    }
}

fn tool_outcome_status(outcome: ToolOutcomeWire) -> ToolStatus {
    match outcome {
        ToolOutcomeWire::Success => ToolStatus::Succeeded,
        ToolOutcomeWire::Failed => ToolStatus::Failed,
        ToolOutcomeWire::InputProvided => ToolStatus::Succeeded,
        ToolOutcomeWire::Denied => ToolStatus::Denied,
        ToolOutcomeWire::Cancelled => ToolStatus::Cancelled,
        ToolOutcomeWire::Unknown => ToolStatus::Failed,
    }
}

fn has_item_index(blocks: &[std::sync::Arc<TranscriptBlock>], index: usize) -> bool {
    blocks.iter().any(|block| block.index() == Some(index))
}

fn live_loop_from_turn_result(
    turn: &TurnRef,
    window: &crate::app::history::TurnResultWindow,
    local_submission: LocalSubmissionId,
    fallback_text: String,
) -> LiveLoop {
    let mut live = LiveLoop::new(local_submission, fallback_text);
    live.reference = Some(turn.clone());
    for item in window.items.values() {
        match item.as_ref() {
            TranscriptBlock::User(user) => {
                if user.kind == UserMessageKindWire::Prompt {
                    live.user_text = user.text.clone();
                }
            }
            TranscriptBlock::Assistant(assistant) => {
                let request = live.ensure_request_mut(
                    assistant.request_index,
                    0,
                    assistant.model.clone(),
                    assistant.reasoning_level,
                );
                for part in &assistant.parts {
                    match part {
                        AssistantPart::Text(text) => {
                            append_live_part(request, LivePart::Text(text.clone()));
                        }
                        AssistantPart::Reasoning(body) => {
                            append_live_part(request, LivePart::Reasoning(body.clone()));
                        }
                        AssistantPart::ToolCall(call) => {
                            request.parts.push(LivePart::Tool {
                                tool_call_id: call.tool_call_id.clone(),
                            });
                            if !request
                                .tools
                                .iter()
                                .any(|tool| tool.tool_call_id == call.tool_call_id)
                            {
                                request.tools.push(crate::state::tool::LiveTool {
                                    tool_call_id: call.tool_call_id.clone(),
                                    name: call.name.clone(),
                                    status: ToolStatus::Pending,
                                    progress: None,
                                    display: None,
                                    result: None,
                                    result_truncated: false,
                                    expanded: false,
                                });
                            }
                        }
                    }
                }
            }
            TranscriptBlock::Tool(tool_result) => {
                let status =
                    tool_outcome_status(tool_result.outcome.unwrap_or(ToolOutcomeWire::Unknown));
                if let Some(request) = live
                    .requests
                    .iter_mut()
                    .find(|request| request.request_index == tool_result.request_index)
                {
                    if let Some(tool) = request
                        .tools
                        .iter_mut()
                        .find(|tool| tool.tool_call_id == tool_result.tool_call_id)
                    {
                        tool.status = status;
                        tool.result = tool_result.result.clone();
                    } else {
                        request.tools.push(crate::state::tool::LiveTool {
                            tool_call_id: tool_result.tool_call_id.clone(),
                            name: tool_result.name.clone(),
                            status,
                            progress: None,
                            display: None,
                            result: tool_result.result.clone(),
                            result_truncated: false,
                            expanded: false,
                        });
                    }
                }
            }
            TranscriptBlock::Summary(_) | TranscriptBlock::HistoryPlaceholder(_) => {}
        }
    }
    live
}

fn install_history_placeholder(view: &mut SessionView, index: usize, total_bytes: usize) {
    if has_item_index(&view.transcript.blocks, index) {
        return;
    }
    view.transcript
        .insert_history_owner(Arc::new(TranscriptBlock::HistoryPlaceholder(
            HistoryPlaceholderBlock { index, total_bytes },
        )));
    view.transcript.invalidate();
}

/// Projects one decoded Runtime item into the display bridge. This is the
/// stage-C-removable short-term adapter (spec §11.1): the durable authority is
/// `view.transcript.window`, and these blocks only drive the current
/// renderer. A pending local user card is promoted in place rather than
/// duplicated; a tool result patches the matching card or creates one.
fn install_history_item(
    view: &mut SessionView,
    index: usize,
    item: &crate::protocol::read::RawHistoryItem,
) -> Option<std::sync::Arc<TranscriptBlock>> {
    use crate::protocol::read::{RuntimeAssistantPart, RuntimeItem, RuntimeUserKind};

    match &item.item {
        RuntimeItem::User(user) => {
            let kind = match user.kind {
                RuntimeUserKind::Prompt => UserMessageKindWire::Prompt,
                RuntimeUserKind::Steering => UserMessageKindWire::Steering,
            };
            let replaced = view
                .transcript
                .blocks_mut()
                .iter_mut()
                .rev()
                .find(|block| {
                    matches!(block.as_ref(), TranscriptBlock::User(card)
                    if card.pending && (card.loop_id.as_deref() == Some(&user.loop_id)
                        || card.text == user.input.text))
                })
                .and_then(|block| {
                    let TranscriptBlock::User(card) = std::sync::Arc::make_mut(block) else {
                        return None;
                    };
                    card.index = Some(index);
                    card.loop_id = Some(user.loop_id.clone());
                    card.kind = kind;
                    card.text = user.input.text.clone();
                    card.pending = false;
                    Some(std::sync::Arc::clone(block))
                });
            let owner = if let Some(owner) = replaced {
                view.transcript.insert_history_owner(Arc::clone(&owner));
                owner
            } else if let Some(owner) = view
                .transcript
                .blocks
                .iter()
                .find(|block| block.index() == Some(index))
                .cloned()
            {
                owner
            } else {
                let owner = std::sync::Arc::new(TranscriptBlock::User(UserBlock {
                    index: Some(index),
                    loop_id: Some(user.loop_id.clone()),
                    kind,
                    text: user.input.text.clone(),
                    pending: false,
                }));
                view.transcript
                    .insert_history_owner(std::sync::Arc::clone(&owner));
                owner
            };
            if let Some(timestamp) = &item.timestamp {
                Arc::make_mut(&mut view.user_timestamps).insert(index, timestamp.clone());
            }
            view.transcript.invalidate();
            Some(owner)
        }
        RuntimeItem::Assistant(assistant) => {
            if has_item_index(&view.transcript.blocks, index) {
                return view
                    .transcript
                    .blocks
                    .iter()
                    .find(|block| block.index() == Some(index))
                    .cloned();
            }
            let mut parts = Vec::new();
            let mut tool_calls = Vec::new();
            for part in &assistant.content {
                match part {
                    RuntimeAssistantPart::Text(text) if !text.is_empty() => {
                        parts.push(AssistantPart::Text(text.clone()));
                    }
                    RuntimeAssistantPart::Reasoning { text, summary, .. } => {
                        let body = text.clone().or_else(|| summary.clone()).unwrap_or_default();
                        if !body.is_empty() {
                            parts.push(AssistantPart::Reasoning(body));
                        }
                    }
                    RuntimeAssistantPart::Text(_) => {}
                    RuntimeAssistantPart::ToolCall {
                        tool_call_id,
                        name,
                        call_index,
                        ..
                    } => {
                        let call = crate::protocol::ToolCallViewWire {
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            call_index: *call_index,
                            display: None,
                        };
                        parts.push(AssistantPart::ToolCall(call.clone()));
                        tool_calls.push(call);
                    }
                }
            }
            let reasoning_level = assistant
                .reasoning
                .as_deref()
                .and_then(|value| {
                    serde_json::from_value::<Reasoning>(serde_json::Value::String(value.to_owned()))
                        .ok()
                })
                .unwrap_or_default();
            let owner = std::sync::Arc::new(TranscriptBlock::Assistant(AssistantBlock {
                index,
                loop_id: assistant.loop_id.clone(),
                request_index: assistant.request_index,
                model: assistant.model.clone(),
                reasoning_level,
                parts,
                tool_calls: tool_calls.clone(),
                usage: assistant.usage,
                finish_reason: assistant.finish_reason.clone(),
                terminal_error: None,
            }));
            view.transcript
                .insert_history_owner(std::sync::Arc::clone(&owner));
            view.transcript.invalidate();
            Some(owner)
        }
        RuntimeItem::ToolResult(result) => {
            // A tool result answers a call in the most recent matching assistant
            // item; several results may follow one assistant item, so pairing is
            // by ToolKey and never by the immediately preceding item's shape.
            let outcome = serde_json::from_value::<ToolOutcomeWire>(serde_json::Value::String(
                result.outcome.clone(),
            ))
            .unwrap_or(ToolOutcomeWire::Unknown);
            let tool_key = ToolKey::new(
                &view.info.session_id,
                &result.loop_id,
                result.request_index,
                &result.call_id,
            );
            let durable_result = Arc::<str>::from(result.output.content.as_str());
            let (
                shared_result,
                accepted_outcome,
                accepted_status,
                accepted_truncated,
                accepted_display,
            ) = {
                let presentations = std::sync::Arc::make_mut(&mut view.tool_presentations);
                let state = presentations.entry(tool_key.clone()).or_insert_with(|| {
                    std::sync::Arc::new(ToolPresentationState {
                        display: Arc::new(ToolDisplayWire {
                            detail: result.tool_name.clone(),
                            expanded_input: None,
                            input_line_count: None,
                            hidden_line_count: None,
                            truncated: false,
                        }),
                        result: None,
                        result_truncated: false,
                        status: ToolStatus::Pending,
                        outcome: None,
                        needs_read: false,
                        input_available: false,
                        conflict: None,
                        invocation: None,
                        execution: None,
                        command: None,
                    })
                });
                let state = std::sync::Arc::make_mut(state);
                state.accept_finished(outcome, Some(durable_result), false);
                (
                    state.result.clone().expect("durable tool result owner"),
                    state.outcome,
                    state.status,
                    state.result_truncated,
                    state.display.clone(),
                )
            };
            if let Some(live) = view.live.as_mut() {
                if let Some(request) = live
                    .requests
                    .iter_mut()
                    .find(|request| request.request_index == result.request_index)
                {
                    if let Some(tool) = request
                        .tools
                        .iter_mut()
                        .find(|tool| tool.tool_call_id == result.call_id)
                    {
                        tool.status = accepted_status;
                        tool.result = Some(Arc::clone(&shared_result));
                        tool.result_truncated = accepted_truncated;
                        tool.display = Some(Arc::clone(&accepted_display));
                    }
                }
            }
            let patched =
                view.transcript
                    .blocks_mut()
                    .iter_mut()
                    .rev()
                    .find_map(|block| match block.as_ref() {
                        TranscriptBlock::Tool(tool)
                            if tool.tool_call_id == result.call_id
                                && tool.loop_id == result.loop_id
                                && tool.request_index == result.request_index =>
                        {
                            Some(std::sync::Arc::clone(block))
                        }
                        _ => None,
                    });
            let owner = if patched.is_some() {
                let owner = std::sync::Arc::new(TranscriptBlock::Tool(ToolBlock {
                    index: Some(index),
                    loop_id: result.loop_id.clone(),
                    request_index: result.request_index,
                    tool_call_id: result.call_id.clone(),
                    name: result.tool_name.clone(),
                    result: Some(Arc::clone(&shared_result)),
                    outcome: accepted_outcome,
                    live_status: None,
                    progress: None,
                    expanded: view
                        .tool_folds
                        .get(&ToolKey::new(
                            &view.info.session_id,
                            &result.loop_id,
                            result.request_index,
                            &result.call_id,
                        ))
                        .is_some_and(FoldOverride::expanded),
                }));
                let blocks = view.transcript.blocks_mut();
                if let Some(position) = blocks.iter().position(|block| {
                    matches!(
                        block.as_ref(),
                        TranscriptBlock::Tool(tool)
                            if tool.tool_call_id == result.call_id
                                && tool.loop_id == result.loop_id
                                && tool.request_index == result.request_index
                    )
                }) {
                    blocks[position] = std::sync::Arc::clone(&owner);
                }
                view.transcript.insert_history_owner(Arc::clone(&owner));
                owner
            } else if let Some(owner) = view
                .transcript
                .blocks
                .iter()
                .find(|block| block.index() == Some(index))
                .cloned()
            {
                owner
            } else {
                let owner = std::sync::Arc::new(TranscriptBlock::Tool(ToolBlock {
                    index: Some(index),
                    loop_id: result.loop_id.clone(),
                    request_index: result.request_index,
                    tool_call_id: result.call_id.clone(),
                    name: result.tool_name.clone(),
                    result: Some(Arc::clone(&shared_result)),
                    outcome: accepted_outcome,
                    live_status: None,
                    progress: None,
                    expanded: view
                        .tool_folds
                        .get(&ToolKey::new(
                            &view.info.session_id,
                            &result.loop_id,
                            result.request_index,
                            &result.call_id,
                        ))
                        .is_some_and(FoldOverride::expanded),
                }));
                view.transcript
                    .insert_history_owner(std::sync::Arc::clone(&owner));
                owner
            };
            view.transcript.invalidate();
            Some(owner)
        }
        RuntimeItem::Summary(summary) => {
            if let Some(existing) = view
                .transcript
                .blocks
                .iter()
                .find(|block| block.index() == Some(index))
            {
                if matches!(existing.as_ref(), TranscriptBlock::Summary(old) if old.content == summary.content)
                {
                    return Some(std::sync::Arc::clone(existing));
                }
                // A new authoritative summary may reuse an old history index.
                // Never return another item's owner or inherit its fold choice.
                std::sync::Arc::make_mut(&mut view.summary_folds).remove(&index);
                view.transcript
                    .blocks_mut()
                    .retain(|block| block.index() != Some(index));
            }
            let owner = std::sync::Arc::new(TranscriptBlock::Summary(SummaryBlock {
                index,
                content: summary.content.clone(),
            }));
            view.transcript
                .insert_history_owner(std::sync::Arc::clone(&owner));
            view.transcript.invalidate();
            Some(owner)
        }
    }
}

#[cfg(test)]
mod scrollbar_tests;

#[cfg(test)]
mod earlier_history_tests;

#[cfg(test)]
mod tests {
    use crossterm::event::MouseEventKind;
    use serde_json::{Value, json};

    use super::*;
    use crate::command::AppCommand;
    use crate::event::{AppEvent, RpcEvent};
    use crate::protocol::{AgentEventWire, IncomingFrame, RpcResponse, TurnRef};
    use crate::state::view::SelectionGranularity;

    fn test_app() -> App {
        App::new(PathBuf::from("/project"))
    }

    fn new_session_selector_fixture(active: bool, field: NewSessionField) -> App {
        let mut app = test_app();
        ready(&mut app);
        if active {
            open_session(&mut app, "ses_1");
        }
        app.catalogs.models = ["deep", "fast"]
            .into_iter()
            .map(|id| ModelInfo {
                id: id.to_owned(),
                model_ref: id.to_owned(),
                context_window: 32_000,
                supports_tools: true,
                supported_reasoning: vec![Reasoning::Low, Reasoning::High],
            })
            .collect();
        app.catalogs.next_model = Some("deep".to_owned());
        app.catalogs.next_reasoning = Some(Reasoning::High);
        app.composer.set_text("unsent existing-session draft");
        assert!(app.update(AppEvent::OpenNewSession).is_empty());
        let draft = app.draft_mut().unwrap();
        draft.workspace = "/new workspace".to_owned();
        draft.title = "unsaved title".to_owned();
        draft.field = field;
        app
    }

    #[test]
    fn new_session_selectors_cancel_back_to_the_exact_form_with_or_without_active_session() {
        for active in [false, true] {
            for field in [NewSessionField::Model, NewSessionField::Reasoning] {
                let mut app = new_session_selector_fixture(active, field);
                let original = app.new_session().unwrap().clone();
                // Repeating the open/cancel flow must retain the same draft identity.
                for _ in 0..2 {
                    assert!(app.update(AppEvent::ConfirmDock).is_empty());
                    assert!(app.selector_state().is_some());
                    assert_eq!(app.new_session(), Some(&original));
                    assert!(app.update(AppEvent::CancelDock).is_empty());
                    assert!(matches!(app.dock, Dock::NewSession(_)));
                    assert_eq!(app.new_session(), Some(&original));
                    assert_eq!(app.composer.content(), "unsent existing-session draft");
                }
                if active {
                    assert_eq!(app.active_view().unwrap().info.model, "deep");
                    assert_eq!(app.active_view().unwrap().info.reasoning, Reasoning::High);
                }
            }
        }
    }

    #[test]
    fn new_session_reasoning_confirmation_edits_only_the_form() {
        for active in [false, true] {
            let mut app = new_session_selector_fixture(active, NewSessionField::Reasoning);
            let mut expected = app.new_session().unwrap().clone();
            expected.reasoning = Reasoning::Low;
            assert!(app.update(AppEvent::ConfirmDock).is_empty());
            assert!(app.update(AppEvent::MoveSelector { delta: -1 }).is_empty());
            assert!(app.update(AppEvent::ConfirmDock).is_empty());
            assert!(matches!(app.dock, Dock::NewSession(_)));
            assert_eq!(app.new_session(), Some(&expected));
            assert_eq!(app.composer.content(), "unsent existing-session draft");
            if active {
                assert_eq!(app.active_view().unwrap().info.model, "deep");
                assert_eq!(app.active_view().unwrap().info.reasoning, Reasoning::High);
            }
        }
    }

    #[test]
    fn new_session_model_and_reasoning_confirmations_reach_create_without_updating_active_session()
    {
        for active in [false, true] {
            let mut app = new_session_selector_fixture(active, NewSessionField::Model);
            let mut expected = app.new_session().unwrap().clone();
            expected.model = "fast".to_owned();
            expected.reasoning = Reasoning::Low;
            assert!(app.update(AppEvent::ConfirmDock).is_empty());
            assert!(app.update(AppEvent::MoveSelector { delta: 1 }).is_empty());
            assert!(app.update(AppEvent::ConfirmDock).is_empty());
            assert!(matches!(app.dock, Dock::ReasoningSelector(_)));
            assert!(app.update(AppEvent::MoveSelector { delta: -1 }).is_empty());
            assert!(app.update(AppEvent::ConfirmDock).is_empty());
            assert!(matches!(app.dock, Dock::NewSession(_)));
            assert_eq!(app.new_session(), Some(&expected));
            assert_eq!(app.composer.content(), "unsent existing-session draft");
            if active {
                assert_eq!(app.active_view().unwrap().info.model, "deep");
                assert_eq!(app.active_view().unwrap().info.reasoning, Reasoning::High);
            }
            let requests = take_requests(app.update(AppEvent::SubmitNewSession));
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "session.create");
            assert_eq!(requests[0].params["workspace"], "/new workspace");
            assert_eq!(requests[0].params["title"], "unsaved title");
            assert_eq!(requests[0].params["model"], "fast");
            assert_eq!(requests[0].params["reasoning"], "low");
        }
    }

    #[test]
    fn startup_errors_keep_protocol_provider_storage_and_config_categories() {
        let cases = [
            (
                RpcResponseError::Parse(serde_json::from_str::<Value>("{").unwrap_err()),
                "protocol error",
            ),
            (
                RpcResponseError::Agent(crate::protocol::RpcError {
                    code: crate::protocol::PROVIDER_ERROR,
                    message: "provider unavailable".to_owned(),
                    data: None,
                }),
                "provider error",
            ),
            (
                RpcResponseError::Agent(crate::protocol::RpcError {
                    code: crate::protocol::STORE_ERROR,
                    message: "store unavailable".to_owned(),
                    data: None,
                }),
                "storage error",
            ),
            (
                RpcResponseError::Agent(crate::protocol::RpcError {
                    code: -32_014,
                    message: "invalid config".to_owned(),
                    data: None,
                }),
                "Agent configuration rejected",
            ),
        ];
        for (error, category) in cases {
            assert!(startup_error_message("agent.ping", &error).contains(category));
        }
    }

    /// A clipboard job result carrying the app's current capture identity, so
    /// the reducer treats it as the live selection.
    fn clipboard_job(app: &App, result: Result<(), String>) -> AppEvent {
        AppEvent::JobFinished(JobOutcome::Clipboard {
            session_id: app.sessions.active.clone().unwrap_or_default(),
            revision: app.selection_revision,
            result,
        })
    }

    fn wire_event(raw: Value) -> AgentEventWire {
        serde_json::from_value(raw).expect("wire event fixture parses")
    }

    fn event(inner: AgentEventWire) -> AppEvent {
        AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(inner),
        )))
    }

    fn make_turn(session: &str, loop_id: &str) -> TurnRef {
        TurnRef {
            session_id: session.to_owned(),
            loop_id: loop_id.to_owned(),
        }
    }

    fn turn_ref_json(session: &str, loop_id: &str) -> Value {
        json!({"session_id": session, "loop_id": loop_id})
    }

    fn meta_json(session: &str, dropped: u64) -> Value {
        json!({"session_id": session, "dropped_before": dropped})
    }

    pub(super) fn session_info(session_id: &str) -> Value {
        json!({
            "session_id": session_id,
            "title": null,
            "profile": "coding",
            "workspace": "/project",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "created_at": "2026-01-02T03:04:05.006Z",
            "updated_at": "2026-01-02T03:04:05.006Z"
        })
    }

    fn state_json(session_id: &str, status: &str) -> Value {
        json!({
            "session_id": session_id,
            "status": status,
            "active_loop": null,
            "block_reason": null
        })
    }

    fn running_state_json(session_id: &str, loop_id: &str) -> Value {
        json!({
            "session_id": session_id,
            "status": "running",
            "active_loop": {
                "loop_id": loop_id,
                "status": "running_model",
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "pending_interaction": null
            },
            "block_reason": null
        })
    }

    /// Encodes a Runtime item envelope as a one-chunk Protocol v1 read page.
    fn read_page_json(items: Vec<Value>, next_cursor: Option<usize>, total: usize) -> Value {
        let chunks: Vec<Value> = items
            .iter()
            .map(|envelope| {
                let index = envelope.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let item = envelope.get("item").cloned().unwrap_or(Value::Null);
                let data = serde_json::to_string(&json!({"item": item})).unwrap();
                let total_bytes = data.len();
                json!({
                    "index": index,
                    "offset": 0,
                    "total_bytes": total_bytes,
                    "encoding": "utf8_json",
                    "data": data,
                    "complete": true
                })
            })
            .collect();
        let mut page = json!({
            "session": session_info("ses_1"),
            "items": chunks,
            "total": total,
            "records": [],
            "records_truncated": false,
            "history_revision": "0000000000000000000000000000000000000000000000000000000000000000",
            "captured_end": total as u64,
            "trailing_incomplete": false
        });
        if let Some(item) = next_cursor {
            page["next_cursor"] = json!({"item": item, "offset": 0});
        }
        page
    }

    fn user_item(index: usize, loop_id: &str, text: &str) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "user",
                "data": {
                    "loop_id": loop_id,
                    "kind": "prompt",
                    "input": {"text": text}
                }
            }
        })
    }

    #[allow(dead_code)]
    fn steer_item(index: usize, loop_id: &str, text: &str) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "user",
                "data": {
                    "loop_id": loop_id,
                    "kind": "steering",
                    "input": {"text": text}
                }
            }
        })
    }

    fn assistant_item(index: usize, loop_id: &str, text: &str) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "assistant",
                "data": {
                    "loop_id": loop_id,
                    "request_index": 0,
                    "model": "deep",
                    "reasoning": "high",
                    "content": [{"type": "text", "data": text}],
                    "usage": {},
                    "finish_reason": "stop"
                }
            }
        })
    }

    fn tool_result_item(
        index: usize,
        loop_id: &str,
        call_id: &str,
        name: &str,
        outcome: &str,
        content: &str,
    ) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "tool_result",
                "data": {
                    "loop_id": loop_id,
                    "request_index": 0,
                    "call_id": call_id,
                    "tool_name": name,
                    "outcome": outcome,
                    "output": {"content": content}
                }
            }
        })
    }

    fn ready(app: &mut App) {
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        assert_eq!(requests.len(), 4);
        for request in &requests {
            let result = match request.method {
                "agent.ping" => json!({
                    "version": "0.5.0",
                    "protocol_version": 1,
                    "capabilities": crate::protocol::REQUIRED_CAPABILITIES,
                }),
                "model.list" => json!({"models": []}),
                "profile.list" => json!({"profiles": []}),
                "session.list" => json!({"sessions": []}),
                other => panic!("unexpected bootstrap request: {other}"),
            };
            take_requests(respond(app, request, result));
        }
        assert_eq!(app.connection, ConnectionState::Ready);
    }

    fn respond(app: &mut App, request: &OutgoingRequest, result: Value) -> Vec<AppCommand> {
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
        ))))
    }

    fn respond_error(
        app: &mut App,
        request: &OutgoingRequest,
        code: i64,
        message: &str,
    ) -> Vec<AppCommand> {
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: None,
                error: Some(crate::protocol::RpcError {
                    code,
                    message: message.to_owned(),
                    data: None,
                }),
            },
        ))))
    }

    fn take_requests(commands: Vec<AppCommand>) -> Vec<OutgoingRequest> {
        commands
            .into_iter()
            .filter_map(|cmd| match cmd {
                AppCommand::Rpc(req) => Some(req),
                _ => None,
            })
            .collect()
    }

    fn open_session(app: &mut App, session_id: &str) {
        let requests = take_requests(app.update(AppEvent::OpenSession {
            session_id: session_id.into(),
        }));
        assert_eq!(requests.len(), 1);
        let commands = respond(
            app,
            &requests[0],
            json!({"session": session_info(session_id)}),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 4);
        let status_req = requests
            .iter()
            .find(|r| r.method == "workspace.status")
            .unwrap();
        take_requests(respond(
            app,
            status_req,
            json!({"repo_available":false,"head_oid":null,"branch":null,"detached":false,"staged":0,"unstaged":0,"untracked":0,"conflicted":0,"entries":[],"skipped_paths":0,"complete":true,"warnings":[],"consistency":"live","observed_at_unix_ms":1}),
        ));
        let state_req = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let presentation_req = requests
            .iter()
            .find(|r| r.method == "session.presentation")
            .unwrap();
        let history_req = requests
            .iter()
            .find(|r| r.method == "session.read")
            .unwrap();
        take_requests(respond(app, state_req, state_json(session_id, "idle")));
        take_requests(respond(
            app,
            presentation_req,
            json!({
                "session_id": session_id,
                "model_label": null,
                "git_branch": null,
                "context": {"tokens": null, "window": null, "percent": null, "kind": "unknown"},
                "cost_usd": null,
                "using_subscription": null,
                "last_loop": null
            }),
        ));
        take_requests(respond(app, history_req, read_page_json(vec![], None, 0)));
    }

    fn anchor_fixture() -> App {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        for index in 0..10 {
            view.transcript.push_block(TranscriptBlock::User(UserBlock {
                index: Some(index),
                loop_id: Some(format!("loop_{index}")),
                kind: crate::protocol::UserMessageKindWire::Prompt,
                text: format!("message {index} {}", "wrapped content ".repeat(3)),
                pending: false,
            }));
        }
        let prepared = crate::ui::transcript::prepare_conversation(&app, 79);
        app.install_conversation(prepared);
        app.viewport = (app.prepared_conversation(79).unwrap().total_rows(), 6);
        app
    }

    fn anchor_target_row(app: &App, history_index: usize) -> usize {
        app.prepared_conversation(79)
            .unwrap()
            .sections
            .iter()
            .find(|section| section.id.history_index == Some(history_index))
            .expect("anchor section")
            .rows
            .start
    }

    #[test]
    fn scroll_anchor_survives_width_resize() {
        let mut app = anchor_fixture();
        let target = anchor_target_row(&app, 6);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = target;
        app.capture_scroll_anchor();
        let anchor = app.active_view().unwrap().scroll.anchor.clone().unwrap();
        assert_eq!(anchor.section_id.history_index, Some(6));

        let resized = crate::ui::transcript::prepare_conversation(&app, 24);
        app.install_conversation(resized.clone());
        let row = resized.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            row.saturating_sub(anchor.screen_row)
        );
    }

    #[test]
    fn unread_output_survives_layout_replacement_until_scrolling_to_tail() {
        let mut app = anchor_fixture();
        let target = anchor_target_row(&app, 3);
        let view = app.active_session_mut().unwrap();
        view.scroll.follow_tail = false;
        view.scroll.offset = target;
        view.scroll.new_content = true;

        // Layout installation occurs for same-height stream updates, resizing,
        // and live-to-durable Markdown reflow. None means the output was read.
        for width in [79, 24, 120] {
            let prepared = crate::ui::transcript::prepare_conversation(&app, width);
            let total = prepared.total_rows();
            app.install_conversation(prepared);
            app.update(AppEvent::Viewport {
                total_lines: total,
                visible_rows: 6,
            });
            let scroll = &app.active_view().unwrap().scroll;
            assert!(!scroll.follow_tail);
            assert!(
                scroll.new_content,
                "layout width {width} cleared unread output"
            );
        }

        app.transcript_scroll_bottom();
        let scroll = &app.active_view().unwrap().scroll;
        assert!(scroll.follow_tail);
        assert!(
            !scroll.new_content,
            "returning to the tail acknowledges output"
        );
    }

    #[test]
    fn scroll_anchor_survives_reasoning_fold() {
        let mut app = anchor_fixture();
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index: 20,
                loop_id: "loop_reasoning".to_owned(),
                request_index: 0,
                model: "model".to_owned(),
                reasoning_level: crate::protocol::Reasoning::High,
                parts: vec![AssistantPart::Reasoning("long reasoning ".repeat(20))],
                tool_calls: Vec::new(),
                usage: Default::default(),
                finish_reason: "stop".to_owned(),
                terminal_error: None,
            }));
        view.transcript.invalidate();
        let expanded = crate::ui::transcript::prepare_conversation(&app, 79);
        let target = expanded
            .sections
            .iter()
            .find(|section| section.id.kind == crate::state::view::SectionKind::Thinking)
            .unwrap()
            .rows
            .start;
        app.install_conversation(expanded);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = target;
        app.capture_scroll_anchor();
        let anchor = app.active_view().unwrap().scroll.anchor.clone().unwrap();
        Arc::make_mut(&mut app.active_session_mut().unwrap().reasoning_folds).insert(
            crate::state::view::ReasoningKey::new("loop_reasoning", 0, 0),
            FoldOverride::Collapsed,
        );
        app.active_session_mut().unwrap().transcript.invalidate();
        let folded = crate::ui::transcript::prepare_conversation(&app, 79);
        app.install_conversation(folded.clone());
        let row = folded.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            row.saturating_sub(anchor.screen_row)
        );
    }

    #[test]
    fn scroll_anchor_survives_prepend_and_keeps_tool_identity() {
        let mut app = anchor_fixture();
        let target = anchor_target_row(&app, 6);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = target;
        app.capture_scroll_anchor();
        let anchor = app.active_view().unwrap().scroll.anchor.clone().unwrap();

        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.transcript.insert_block(
            0,
            TranscriptBlock::User(UserBlock {
                index: Some(100),
                loop_id: Some("older".to_owned()),
                kind: crate::protocol::UserMessageKindWire::Prompt,
                text: "prepended earlier history".to_owned(),
                pending: false,
            }),
        );
        view.transcript.invalidate();
        let prepared = crate::ui::transcript::prepare_conversation(&app, 79);
        app.install_conversation(prepared.clone());
        let section = prepared
            .sections
            .iter()
            .find(|section| section.id.history_index == Some(6))
            .unwrap();
        assert_eq!(section.id, anchor.section_id);
        let row = prepared.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            row.saturating_sub(anchor.screen_row)
        );
    }

    #[test]
    fn scroll_anchor_rebases_live_tool_to_saved_tool_by_tool_id() {
        let mut app = anchor_fixture();
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.transcript.clear_blocks();
        let mut live = LiveLoop::new(LocalSubmissionId(1), "run".to_owned());
        live.reference = Some(make_turn("ses_1", "loop_tool"));
        let mut request = crate::state::turn::LiveRequest::new(
            0,
            0,
            "model".to_owned(),
            crate::protocol::Reasoning::High,
        );
        request.parts.push(LivePart::Tool {
            tool_call_id: "call_tool".to_owned(),
        });
        request.tools.push(LiveTool {
            tool_call_id: "call_tool".to_owned(),
            name: "read".to_owned(),
            status: ToolStatus::Succeeded,
            progress: None,
            display: None,
            result: Some(Arc::from("live result")),
            result_truncated: false,
            expanded: false,
        });
        live.requests.push(request);
        view.live = Some(live);
        view.transcript.invalidate();
        let live_prepared = crate::ui::transcript::prepare_conversation(&app, 79);
        let live_section = live_prepared
            .sections
            .iter()
            .find(|section| section.id.tool_call_id.as_deref() == Some("call_tool"))
            .unwrap();
        app.install_conversation(live_prepared);
        app.viewport = (app.prepared_conversation(79).unwrap().total_rows(), 6);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = live_section.rows.start;
        app.capture_scroll_anchor();
        let anchor = app.active_view().unwrap().scroll.anchor.clone().unwrap();

        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live = None;
        view.transcript.clear_blocks();
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index: 0,
                loop_id: "loop_tool".to_owned(),
                request_index: 0,
                model: "model".to_owned(),
                reasoning_level: crate::protocol::Reasoning::High,
                parts: vec![AssistantPart::ToolCall(crate::protocol::ToolCallViewWire {
                    tool_call_id: "call_tool".to_owned(),
                    name: "read".to_owned(),
                    call_index: 0,
                    display: None,
                })],
                tool_calls: Vec::new(),
                usage: Default::default(),
                finish_reason: "stop".to_owned(),
                terminal_error: None,
            }));
        view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
            index: Some(1),
            loop_id: "loop_tool".to_owned(),
            request_index: 0,
            tool_call_id: "call_tool".to_owned(),
            name: "read".to_owned(),
            result: Some(Arc::from("saved result")),
            outcome: Some(crate::protocol::ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: false,
        }));
        view.transcript.invalidate();
        let saved = crate::ui::transcript::prepare_conversation(&app, 79);
        app.install_conversation(saved.clone());
        let saved_section = saved
            .sections
            .iter()
            .find(|section| section.id.tool_call_id.as_deref() == Some("call_tool"))
            .unwrap();
        assert_eq!(
            saved_section.id.tool_call_id,
            anchor.section_id.tool_call_id
        );
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            saved_section.rows.start.saturating_sub(anchor.screen_row)
        );
    }

    #[test]
    fn unloaded_scroll_anchor_falls_back_with_a_notice() {
        let mut app = anchor_fixture();
        let prepared = app.prepared_conversation(79).unwrap().clone();
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.anchor = Some(ScrollAnchor {
            section_id: crate::state::view::SectionId {
                session_id: "ses_1".into(),
                loop_id: Some("loop_missing".into()),
                request_index: None,
                kind: crate::state::view::SectionKind::User,
                ordinal: 0,
                tool_call_id: None,
                history_index: Some(9999),
            },
            source_offset: 0,
            screen_row: 1,
        });

        app.restore_scroll_anchor(&prepared);

        assert!(
            app.notices()
                .iter()
                .any(|notice| { notice.text.contains("original scroll range is not loaded") })
        );
        assert_ne!(
            app.active_view()
                .unwrap()
                .scroll
                .anchor
                .as_ref()
                .unwrap()
                .section_id
                .loop_id
                .as_deref(),
            Some("loop_missing")
        );
    }

    #[test]
    fn bootstrap_registers_pending_requests_before_commands_leave_update() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        assert_eq!(requests.len(), 4);
        let expectations = [
            ("agent.ping", RequestKind::Ping),
            ("model.list", RequestKind::ListModels),
            ("profile.list", RequestKind::ListProfiles),
            ("session.list", RequestKind::ListSessions),
        ];
        for (method, kind) in &expectations {
            let request = requests
                .iter()
                .find(|r| r.method == *method)
                .expect("request exists");
            assert_eq!(
                app.pending_requests.get(&request.id),
                Some(kind),
                "pending registered for {method}"
            );
        }
        assert_eq!(app.connection, ConnectionState::Starting);
    }

    #[test]
    fn bootstrap_reaches_ready_for_supported_version_0_3_0() {
        let mut app = test_app();
        ready(&mut app);
        assert_eq!(app.connection, ConnectionState::Ready);
    }

    #[test]
    fn reload_stages_out_of_order_catalogs_before_installing_them() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let requests = take_requests(app.update(AppEvent::Reload));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "agent.reload");

        let requests = take_requests(respond(&mut app, &requests[0], json!({"ok": true})));
        assert_eq!(requests.len(), 3);
        let models = requests
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        let profiles = requests
            .iter()
            .find(|request| request.method == "profile.list")
            .unwrap();
        let sessions = requests
            .iter()
            .find(|request| request.method == "session.list")
            .unwrap();

        take_requests(respond(
            &mut app,
            profiles,
            json!({
                "profiles": [{
                    "id": "p2",
                    "model": "m2",
                    "reasoning": "auto",
                    "tools": []
                }]
            }),
        ));
        take_requests(respond(
            &mut app,
            models,
            json!({
                "models": [{
                    "id": "m2",
                    "model_ref": "provider/m2",
                    "context_window": 1000,
                    "supports_tools": true,
                    "supported_reasoning": ["auto"]
                }]
            }),
        ));
        let reads = take_requests(respond(
            &mut app,
            sessions,
            json!({"sessions": [session_info("ses_1")]}),
        ));
        assert!(
            reads.is_empty(),
            "a catalog reload installs catalogs without reading the session view"
        );

        assert!(app.reload.is_none());
        assert_eq!(app.catalogs.models[0].id, "m2");
        assert_eq!(app.catalogs.profiles[0].id, "p2");
        let view = &app.sessions.known["ses_1"];
        assert!(view.state.is_some());
        assert!(!view.event_gap);
        assert_eq!(view.result_confirmation, ResultConfirmation::Confirmed);
    }

    #[test]
    fn reload_does_not_touch_a_live_projection() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.state.as_mut().unwrap().status = SessionStatusWire::Running;
        view.state.as_mut().unwrap().active_loop = Some(crate::protocol::LoopStateWire {
            loop_id: "loop_live".to_owned(),
            status: crate::protocol::LoopStatusWire::RunningModel,
            request_index: 0,
            config_revision: 0,
            model: Some("deep".to_owned()),
            pending_interaction: None,
        });
        let mut live = LiveLoop::new(LocalSubmissionId(1), "live prompt".to_owned());
        live.reference = Some(make_turn("ses_1", "loop_live"));
        view.live = Some(live);

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        let reads = take_requests(respond(&mut app, &reload, json!({"ok": true})));
        let models = reads
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        let profiles = reads
            .iter()
            .find(|request| request.method == "profile.list")
            .unwrap();
        let sessions = reads
            .iter()
            .find(|request| request.method == "session.list")
            .unwrap();
        take_requests(respond(&mut app, models, json!({"models": []})));
        take_requests(respond(&mut app, profiles, json!({"profiles": []})));
        let after_catalogs = take_requests(respond(
            &mut app,
            sessions,
            json!({"sessions": [session_info("ses_1")]}),
        ));
        assert!(
            after_catalogs.is_empty(),
            "no session view read follows the catalog generation"
        );

        let view = &app.sessions.known["ses_1"];
        assert!(view.live.is_some());
        assert_eq!(
            view.state.as_ref().unwrap().status,
            SessionStatusWire::Running
        );
        assert_eq!(
            view.live
                .as_ref()
                .and_then(|live| live.reference.as_ref())
                .map(|turn| turn.loop_id.as_str()),
            Some("loop_live")
        );
        assert!(!view.event_gap);
        assert_eq!(view.result_confirmation, ResultConfirmation::Confirmed);
    }

    #[test]
    fn reload_reentry_and_failed_ack_leave_the_existing_projection_untouched() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        assert!(take_requests(app.update(AppEvent::Reload)).is_empty());
        assert!(
            app.notices()
                .iter()
                .any(|notice| { notice.text == "configuration reload is already in progress" })
        );

        let commands = respond(&mut app, &reload, json!({"ok": false}));
        assert!(take_requests(commands).is_empty());
        assert!(app.reload.is_none());
        assert!(app.notices().iter().any(|notice| {
            notice.text == "agent.reload returned {ok:false}; configuration was not applied"
        }));
        assert!(app.sessions.known["ses_1"].transcript.complete);
    }

    #[test]
    fn malformed_reload_ack_reports_unknown_without_retrying_reload() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        let commands = respond(&mut app, &reload, json!({"ok": true, "extra": true}));

        assert!(take_requests(commands).is_empty());
        assert!(app.reload.is_none());
        assert!(app.notices().iter().any(|notice| {
            notice
                .text
                .contains("configuration reload outcome is unknown; no automatic retry")
        }));
        assert!(app.notices().iter().all(|notice| {
            !notice
                .text
                .starts_with("Agent configuration reloaded; view refresh")
        }));
    }

    #[test]
    fn reload_after_ack_reports_catalog_failure_without_claiming_rollback() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        let reads = take_requests(respond(&mut app, &reload, json!({"ok": true})));
        let models = reads
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        take_requests(respond(&mut app, models, json!({"models": "malformed"})));

        assert!(app.notices().iter().any(|notice| {
            notice
                .text
                .starts_with("Agent configuration reloaded; catalog refresh incomplete or failed:")
        }));
        assert!(
            !app.notices()
                .iter()
                .any(|notice| { notice.text.contains("configuration was not applied") })
        );
        assert!(app.reload.is_none());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.event_gap);
        assert!(view.state.is_some());
        assert_eq!(view.result_confirmation, ResultConfirmation::Confirmed);
    }

    #[test]
    fn an_open_ack_during_a_reload_stays_uncalibrated_until_recovery() {
        let mut app = test_app();
        ready(&mut app);

        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".to_owned(),
        }))
        .remove(0);
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);

        take_requests(respond(
            &mut app,
            &open,
            json!({"session": session_info("ses_1")}),
        ));
        let view = &app.sessions.known["ses_1"];
        assert!(view.state.is_none());
        assert!(view.event_gap);
        assert!(!view.transcript.complete);

        let recovery = take_requests(respond(&mut app, &reload, json!({"ok": false})));
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.state")
        );
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.read")
        );
    }

    #[test]
    fn a_late_open_ack_after_a_reload_failure_starts_view_recovery() {
        let mut app = test_app();
        ready(&mut app);

        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".to_owned(),
        }))
        .remove(0);
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        take_requests(respond(&mut app, &reload, json!({"ok": false})));

        let reads = take_requests(respond(
            &mut app,
            &open,
            json!({"session": session_info("ses_1")}),
        ));
        let view = &app.sessions.known["ses_1"];
        assert!(view.state.is_none());
        assert!(!view.transcript.complete);
        assert!(
            !view.event_gap,
            "a fresh open builds its own projection instead of inheriting a gap"
        );
        assert!(
            reads
                .iter()
                .any(|request| request.method == "session.state")
        );
        assert!(reads.iter().any(|request| request.method == "session.read"));
    }

    #[test]
    fn reload_leaves_a_pending_normal_state_read_to_finish_normally() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        assert!(matches!(
            app.pending_requests.get(&state.id),
            Some(RequestKind::SessionState { .. })
        ));

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        assert!(
            matches!(
                app.pending_requests.get(&state.id),
                Some(RequestKind::SessionState { .. })
            ),
            "a catalog reload does not retire an unrelated read"
        );
        let failure = take_requests(respond(&mut app, &reload, json!({"ok": false})));
        assert!(
            failure.is_empty(),
            "a failed catalog reload issues no session recovery of its own"
        );

        // The read was issued before the reload, but it is an independent
        // authority query and still installs normally.
        take_requests(respond(
            &mut app,
            &state,
            running_state_json("ses_1", "loop_normal"),
        ));
        assert_eq!(
            app.sessions.known["ses_1"]
                .state
                .as_ref()
                .map(|state| state.status),
            Some(SessionStatusWire::Running)
        );
    }

    #[test]
    fn failed_state_read_drops_old_authority_and_blocks_submission() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        take_requests(respond(&mut app, &state, json!({"malformed": true})));

        let view = &app.sessions.known["ses_1"];
        assert!(view.state.is_none());
        assert!(view.event_gap);
        assert!(!view.transcript.complete);
        assert!(view.close_verification_unknown);
        assert!(
            take_requests(app.update(AppEvent::SubmitTurn {
                session_id: "ses_1".to_owned(),
                text: "must remain blocked".to_owned(),
            }))
            .is_empty()
        );
    }

    #[test]
    fn state_send_failure_drops_old_authority_and_blocks_submission() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        take_requests(app.update(AppEvent::RpcSendFailed {
            id: state.id,
            error: crate::rpc::RpcError::RequestTooLarge {
                actual_bytes: 2,
                max_bytes: 1,
            },
        }));

        let view = &app.sessions.known["ses_1"];
        assert!(view.state.is_none());
        assert!(view.event_gap);
        assert!(!view.transcript.complete);
        assert!(
            take_requests(app.update(AppEvent::SubmitTurn {
                session_id: "ses_1".to_owned(),
                text: "must remain blocked".to_owned(),
            }))
            .is_empty()
        );
    }

    #[test]
    fn reload_does_not_advance_steer_queue_or_consume_new_input() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        {
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            let state = view.state.as_mut().unwrap();
            state.status = SessionStatusWire::Running;
            state.active_loop = Some(crate::protocol::LoopStateWire {
                loop_id: "loop_live".to_owned(),
                status: crate::protocol::LoopStatusWire::RunningModel,
                request_index: 0,
                config_revision: 0,
                model: Some("deep".to_owned()),
                pending_interaction: None,
            });
            let mut live = LiveLoop::new(LocalSubmissionId(1), "prompt".to_owned());
            live.reference = Some(make_turn("ses_1", "loop_live"));
            view.live = Some(live);
            view.steer_queue.push(crate::state::turn::SteerQueueItem {
                local_id: 10,
                text: "already authorized".to_owned(),
                state: SteerQueueState::Unsent,
                editor_revision: None,
                handoff: false,
            });
        }
        app.composer.set_text("typed during reload");

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        assert!(take_requests(app.submit_composer()).is_empty());
        assert!(
            take_requests(app.update(AppEvent::SteerTurn {
                session_id: "ses_1".to_owned(),
                text: "new steer".to_owned(),
            }))
            .is_empty()
        );
        assert_eq!(app.composer.content(), "typed during reload");
        assert_eq!(app.sessions.known["ses_1"].steer_queue.len(), 1);

        let reads = take_requests(respond(&mut app, &reload, json!({"ok": true})));
        let models = reads
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        let profiles = reads
            .iter()
            .find(|request| request.method == "profile.list")
            .unwrap();
        let sessions = reads
            .iter()
            .find(|request| request.method == "session.list")
            .unwrap();
        take_requests(respond(&mut app, models, json!({"models": []})));
        take_requests(respond(&mut app, profiles, json!({"profiles": []})));
        let after_catalogs = take_requests(respond(
            &mut app,
            sessions,
            json!({"sessions": [session_info("ses_1")]}),
        ));
        assert!(
            after_catalogs.is_empty(),
            "reload completion neither reads the view nor advances the FIFO"
        );
        assert!(app.reload.is_none());
        assert_eq!(app.composer.content(), "typed during reload");
        assert_eq!(app.sessions.known["ses_1"].steer_queue.len(), 1);

        // The next ordinary event advances the queue exactly once.
        let advance = take_requests(app.update(AppEvent::Tick));
        assert_eq!(advance.len(), 1);
        assert_eq!(advance[0].method, "turn.steer");
        assert_eq!(advance[0].params["loop_id"], "loop_live");
    }

    #[test]
    fn a_catalog_reload_does_not_retire_an_inflight_history_read() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let old_history = match app.request_history(&"ses_1".to_owned()) {
            Some(AppCommand::Rpc(request)) => request,
            _ => unreachable!(),
        };
        if let Some(view) = app.sessions.known.get_mut("ses_1") {
            view.history_read.begin(HistoryTrigger::Gap);
            view.event_gap = true;
        }
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        assert!(
            matches!(
                app.pending_requests.get(&old_history.id),
                Some(RequestKind::History { .. })
            ),
            "a catalog reload does not fence an unrelated history read"
        );
        let failure = take_requests(respond(&mut app, &reload, json!({"ok": false})));
        assert!(
            failure.is_empty(),
            "a failed catalog reload issues no view recovery"
        );
        assert!(app.sessions.known["ses_1"].event_gap);

        app.composer.set_text("must remain available");
        assert!(take_requests(app.submit_composer()).is_empty());
        assert!(
            take_requests(app.update(AppEvent::CloseSession {
                session_id: "ses_1".to_owned(),
                confirm: true,
            }))
            .is_empty()
        );
        assert!(
            take_requests(app.update(AppEvent::DeleteSession {
                session_id: "ses_1".to_owned(),
                confirm: true,
            }))
            .is_empty()
        );
        assert_eq!(app.composer.content(), "must remain available");

        // The in-flight read settles the gap normally.
        take_requests(respond(
            &mut app,
            &old_history,
            read_page_json(vec![], None, 0),
        ));
        assert!(!app.sessions.known["ses_1"].event_gap);
        assert!(app.sessions.known["ses_1"].transcript.complete);
    }

    #[test]
    fn an_uncalibrated_session_ignores_idle_events_until_the_state_response() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        app.mark_session_uncalibrated(&"ses_1".to_owned());

        let state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        take_requests(app.update(event(wire_event(json!({
            "type": "session_state",
            "data": {
                "state": state_json("ses_1", "idle"),
                "meta": meta_json("ses_1", 0)
            }
        })))));
        assert!(
            matches!(
                app.pending_requests.get(&state.id),
                Some(RequestKind::SessionState { .. })
            ),
            "an idle notification does not retire the pending authority read"
        );
        assert!(
            take_requests(app.update(AppEvent::CloseSession {
                session_id: "ses_1".to_owned(),
                confirm: true,
            }))
            .iter()
            .all(|request| request.method != "session.close" && request.method != "session.delete")
        );
        assert!(
            take_requests(app.update(AppEvent::DeleteSession {
                session_id: "ses_1".to_owned(),
                confirm: true,
            }))
            .iter()
            .all(|request| request.method != "session.close" && request.method != "session.delete")
        );

        take_requests(respond(&mut app, &state, state_json("ses_1", "idle")));
        assert!(app.sessions.known["ses_1"].state.is_some());
    }

    #[test]
    fn a_history_gap_keeps_submit_blocked_until_the_history_read_settles() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        app.mark_session_uncalibrated(&"ses_1".to_owned());

        let recovery = take_requests(app.start_gap_reconcile(&"ses_1".to_owned()));
        let state = recovery
            .iter()
            .find(|request| request.method == "session.state")
            .cloned()
            .expect("gap recovery requests state");
        let history = recovery
            .iter()
            .find(|request| request.method == "session.read")
            .cloned()
            .expect("gap recovery requests history");

        take_requests(respond(&mut app, &state, state_json("ses_1", "idle")));
        assert!(app.sessions.known["ses_1"].event_gap);
        assert!(
            take_requests(app.update(AppEvent::SubmitTurn {
                session_id: "ses_1".to_owned(),
                text: "blocked while history gap remains".to_owned(),
            }))
            .iter()
            .all(|request| request.method != "turn.send")
        );

        take_requests(respond(
            &mut app,
            &history,
            read_page_json(Vec::new(), None, 0),
        ));
        assert!(!app.sessions.known["ses_1"].event_gap);
        assert!(app.sessions.known["ses_1"].transcript.complete);
        let send = take_requests(app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "allowed after history recovery".to_owned(),
        }));
        assert_eq!(send.len(), 1);
        assert_eq!(send[0].method, "turn.send");
    }

    #[test]
    fn a_running_loop_steers_after_a_fresh_state_even_with_a_history_gap() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        {
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            let state = view.state.as_mut().unwrap();
            state.status = SessionStatusWire::Running;
            state.active_loop = Some(crate::protocol::LoopStateWire {
                loop_id: "loop_live".to_owned(),
                status: crate::protocol::LoopStatusWire::RunningModel,
                request_index: 0,
                config_revision: 0,
                model: Some("deep".to_owned()),
                pending_interaction: None,
            });
            let mut live = LiveLoop::new(LocalSubmissionId(1), "prompt".to_owned());
            live.reference = Some(make_turn("ses_1", "loop_live"));
            live.event_gap = true;
            view.live = Some(live);
            view.transcript.complete = false;
            view.steer_queue.push(crate::state::turn::SteerQueueItem {
                local_id: 10,
                text: "steer through history gap".to_owned(),
                state: SteerQueueState::Unsent,
                editor_revision: None,
                handoff: false,
            });
            // The uncalibrated authority blocks steering until a fresh state.
            view.event_gap = true;
            view.steer_state_unconfirmed = true;
            view.state = None;
        }

        let state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        let steer = take_requests(respond(
            &mut app,
            &state,
            running_state_json("ses_1", "loop_live"),
        ));
        assert_eq!(steer.len(), 1);
        assert_eq!(steer[0].method, "turn.steer");
        assert_eq!(steer[0].params["loop_id"], "loop_live");
        assert!(app.sessions.known["ses_1"].event_gap);
        assert!(!app.sessions.known["ses_1"].steer_state_unconfirmed);
    }

    #[test]
    fn a_pending_send_survives_a_catalog_reload_and_binds_after_its_ack() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let send = take_requests(app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "original user input".to_owned(),
        }));
        assert_eq!(send.len(), 1);
        let send = send.into_iter().next().unwrap();
        assert_eq!(send.method, "turn.send");
        assert!(matches!(
            app.pending_request_kind(send.id),
            Some(RequestKind::SendTurn { .. })
        ));
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .is_some_and(|live| live.reference.is_none())
        );

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        let reads = take_requests(respond(&mut app, &reload, json!({"ok": true})));
        let models = reads
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        let profiles = reads
            .iter()
            .find(|request| request.method == "profile.list")
            .unwrap();
        let sessions = reads
            .iter()
            .find(|request| request.method == "session.list")
            .unwrap();
        take_requests(respond(&mut app, models, json!({"models": []})));
        take_requests(respond(&mut app, profiles, json!({"profiles": []})));
        let after_catalogs = take_requests(respond(
            &mut app,
            sessions,
            json!({"sessions": [session_info("ses_1")]}),
        ));
        assert!(
            after_catalogs.is_empty(),
            "the reload leaves the pending submission alone"
        );
        assert!(app.reload.is_none());
        assert!(app.pending_request_kind(send.id).is_some());
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .is_some_and(|live| live.reference.is_none())
        );

        let after_send_ack = take_requests(respond(
            &mut app,
            &send,
            json!({"turn": turn_ref_json("ses_1", "loop_after_reload")}),
        ));
        assert_eq!(
            after_send_ack
                .iter()
                .filter(|request| request.method == "turn.wait")
                .count(),
            1
        );
        assert!(
            after_send_ack
                .iter()
                .all(|request| request.method != "turn.send"),
            "the ACK binds the exact loop and never re-sends the prompt"
        );
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .is_some_and(|live| live
                    .reference
                    .as_ref()
                    .is_some_and(|turn| turn.loop_id == "loop_after_reload"))
        );

        let steer = take_requests(app.update(AppEvent::SteerTurn {
            session_id: "ses_1".to_owned(),
            text: "after reload authority".to_owned(),
        }));
        assert_eq!(steer.len(), 1);
        assert_eq!(steer[0].method, "turn.steer");
        assert_eq!(steer[0].params["loop_id"], "loop_after_reload");
        assert!(steer.iter().all(|request| request.method != "turn.send"));
    }

    #[test]
    fn a_queued_steer_remains_paused_after_its_loop_settles() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let send = take_requests(app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "original user input".to_owned(),
        }));
        assert_eq!(send.len(), 1);
        let send = send[0].clone();
        assert_eq!(send.method, "turn.send");
        {
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            view.steer_queue.push(crate::state::turn::SteerQueueItem {
                local_id: 20,
                text: "queued B".to_owned(),
                state: SteerQueueState::Unsent,
                editor_revision: None,
                handoff: false,
            });
            // A paused queue must stay paused and may never convert into a
            // fresh prompt while its loop settles.
            view.steer_queue_paused = true;
        }

        let after_ack = take_requests(respond(
            &mut app,
            &send,
            json!({"turn": turn_ref_json("ses_1", "loop_a")}),
        ));
        let wait = after_ack
            .iter()
            .find(|request| request.method == "turn.wait")
            .cloned()
            .expect("the send ACK registers one wait");
        assert!(
            after_ack
                .iter()
                .all(|request| request.method != "turn.send" && request.method != "turn.steer")
        );

        let after_wait = take_requests(respond(
            &mut app,
            &wait,
            json!({
                "turn": turn_ref_json("ses_1", "loop_a"),
                "outcome": {"type": "completed"},
                "usage": {},
                "requests": 1,
                "tool_rounds": 0,
                "final_config_revision": 0,
                "persistence": "persisted"
            }),
        ));
        let wait_state = after_wait
            .iter()
            .find(|request| request.method == "session.state")
            .cloned()
            .expect("completion requests one fresh idle state");
        let history = after_wait
            .iter()
            .find(|request| request.method == "session.read")
            .cloned()
            .expect("completion requests terminal history");
        assert!(
            after_wait
                .iter()
                .all(|request| request.method != "turn.send" && request.method != "turn.steer")
        );

        take_requests(respond(&mut app, &wait_state, state_json("ses_1", "idle")));
        let after_history = take_requests(respond(
            &mut app,
            &history,
            read_page_json(
                vec![
                    user_item(0, "loop_a", "original user input"),
                    assistant_item(1, "loop_a", "A completed"),
                ],
                None,
                2,
            ),
        ));
        assert!(
            after_history
                .iter()
                .all(|request| request.method != "turn.send" && request.method != "turn.steer")
        );
        assert_eq!(app.sessions.known["ses_1"].steer_queue.len(), 1);
        assert_eq!(app.sessions.known["ses_1"].steer_queue[0].text, "queued B");
        assert_eq!(
            app.sessions.known["ses_1"].steer_queue[0].state,
            SteerQueueState::Unsent
        );
        assert!(
            take_requests(app.update(AppEvent::Tick))
                .iter()
                .all(|request| request.method != "turn.send" && request.method != "turn.steer")
        );
    }

    #[test]
    fn idle_state_evidence_does_not_release_steer_authority_fence() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        {
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            let state = view.state.as_mut().unwrap();
            state.status = SessionStatusWire::Running;
            state.active_loop = Some(crate::protocol::LoopStateWire {
                loop_id: "loop_live".to_owned(),
                status: crate::protocol::LoopStatusWire::RunningModel,
                request_index: 0,
                config_revision: 0,
                model: Some("deep".to_owned()),
                pending_interaction: None,
            });
            let mut live = LiveLoop::new(LocalSubmissionId(1), "prompt".to_owned());
            live.reference = Some(make_turn("ses_1", "loop_live"));
            view.live = Some(live);
            view.event_gap = true;
            view.steer_state_unconfirmed = true;
        }

        take_requests(app.update(event(wire_event(json!({
            "type": "session_state",
            "data": {
                "state": state_json("ses_1", "idle"),
                "meta": meta_json("ses_1", 0)
            }
        })))));
        assert!(app.sessions.known["ses_1"].steer_state_unconfirmed);
        assert!(
            take_requests(app.update(AppEvent::SteerTurn {
                session_id: "ses_1".to_owned(),
                text: "must wait for Running authority".to_owned(),
            }))
            .is_empty()
        );

        let idle_state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        take_requests(respond(&mut app, &idle_state, state_json("ses_1", "idle")));
        assert!(app.sessions.known["ses_1"].steer_state_unconfirmed);
        assert!(
            take_requests(app.update(AppEvent::SteerTurn {
                session_id: "ses_1".to_owned(),
                text: "still must wait".to_owned(),
            }))
            .is_empty()
        );

        let running_state = match app.request_session_state(&"ses_1".to_owned()) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        take_requests(respond(
            &mut app,
            &running_state,
            running_state_json("ses_1", "loop_live"),
        ));
        assert!(!app.sessions.known["ses_1"].steer_state_unconfirmed);
        let steer = take_requests(app.update(AppEvent::SteerTurn {
            session_id: "ses_1".to_owned(),
            text: "now authorized".to_owned(),
        }));
        assert_eq!(steer.len(), 1);
        assert_eq!(steer[0].method, "turn.steer");
        assert_eq!(steer[0].params["loop_id"], "loop_live");
        assert!(app.sessions.known["ses_1"].event_gap);
    }

    #[test]
    fn close_verify_failure_idle_event_does_not_authorize_repeat_close() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let close = take_requests(app.update(AppEvent::CloseSession {
            session_id: "ses_1".to_owned(),
            confirm: true,
        }))
        .remove(0);
        let verify = take_requests(respond_error(
            &mut app,
            &close,
            crate::protocol::INTERNAL_ERROR,
            "close failed",
        ))
        .into_iter()
        .find(|request| request.method == "session.state")
        .expect("close failure starts one verification read");
        take_requests(respond_error(
            &mut app,
            &verify,
            crate::protocol::INTERNAL_ERROR,
            "verification unavailable",
        ));

        take_requests(app.update(event(wire_event(json!({
            "type": "session_state",
            "data": {
                "state": state_json("ses_1", "idle"),
                "meta": meta_json("ses_1", 0)
            }
        })))));

        let retry = take_requests(app.update(AppEvent::CloseSession {
            session_id: "ses_1".to_owned(),
            confirm: true,
        }));
        assert!(
            retry
                .iter()
                .all(|request| request.method != "session.close")
        );
        assert_eq!(
            retry
                .iter()
                .filter(|request| request.method == "session.state")
                .count(),
            1
        );
    }

    #[test]
    fn dropped_close_verification_enters_guarded_unknown_and_recovers() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let close = take_requests(app.update(AppEvent::CloseSession {
            session_id: "ses_1".to_owned(),
            confirm: true,
        }))
        .remove(0);
        let verify = take_requests(respond_error(
            &mut app,
            &close,
            crate::protocol::INTERNAL_ERROR,
            "close failed",
        ))
        .into_iter()
        .find(|request| request.method == "session.state")
        .expect("close failure starts one verification read");
        assert!(app.sessions.known["ses_1"].closing);

        let recovery = take_requests(app.update(event(wire_event(json!({
            "type": "session_state",
            "data": {
                "state": state_json("ses_1", "idle"),
                "meta": meta_json("ses_1", 1)
            }
        })))));
        assert!(!app.sessions.known["ses_1"].closing);
        assert!(app.sessions.known["ses_1"].close_verification_unknown);
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.state")
        );
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.read")
        );
        assert!(
            recovery
                .iter()
                .all(|request| request.method != "session.close")
        );

        let late = take_requests(respond(&mut app, &verify, state_json("ses_1", "idle")));
        assert!(late.iter().all(|request| request.method != "session.close"));
        assert!(!app.sessions.known["ses_1"].closing);
        assert!(app.sessions.known["ses_1"].close_verification_unknown);
        let blocked = take_requests(app.update(AppEvent::CloseSession {
            session_id: "ses_1".to_owned(),
            confirm: true,
        }));
        assert!(
            blocked
                .iter()
                .all(|request| request.method != "session.close")
        );

        let recovery_state = recovery
            .iter()
            .find(|request| request.method == "session.state")
            .unwrap()
            .clone();
        let recovery_history = recovery
            .iter()
            .find(|request| request.method == "session.read")
            .unwrap()
            .clone();
        take_requests(respond(
            &mut app,
            &recovery_state,
            state_json("ses_1", "idle"),
        ));
        take_requests(respond(
            &mut app,
            &recovery_history,
            read_page_json(Vec::new(), None, 0),
        ));
        assert!(!app.sessions.known["ses_1"].event_gap);
        assert!(app.sessions.known["ses_1"].transcript.complete);
        assert!(!app.sessions.known["ses_1"].close_verification_unknown);

        let retry = take_requests(app.update(AppEvent::CloseSession {
            session_id: "ses_1".to_owned(),
            confirm: true,
        }));
        assert_eq!(
            retry
                .iter()
                .filter(|request| request.method == "session.close")
                .count(),
            1
        );
    }

    #[test]
    fn a_lifecycle_ack_during_a_reload_stays_uncalibrated_until_recovery() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let rename = match app.request(
            RequestKind::RenameSession {
                session_id: "ses_1".to_owned(),
            },
            |id| OutgoingRequest::session_rename(id, "ses_1", "renamed"),
        ) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        assert!(
            !app.sessions.known["ses_1"].event_gap,
            "a catalog reload does not mark the view uncalibrated"
        );

        // A lifecycle ACK that crosses the barrier is what makes the view
        // uncalibrated: its own read chain resumes when catalogs install.
        take_requests(respond(
            &mut app,
            &rename,
            json!({"session": session_info("ses_1")}),
        ));
        assert!(app.sessions.known["ses_1"].event_gap);
        assert!(
            take_requests(app.update(AppEvent::CloseSession {
                session_id: "ses_1".to_owned(),
                confirm: true,
            }))
            .is_empty()
        );
        assert!(
            take_requests(app.update(AppEvent::DeleteSession {
                session_id: "ses_1".to_owned(),
                confirm: true,
            }))
            .is_empty()
        );

        let reads = take_requests(respond(&mut app, &reload, json!({"ok": true})));
        let models = reads
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        let profiles = reads
            .iter()
            .find(|request| request.method == "profile.list")
            .unwrap();
        let sessions = reads
            .iter()
            .find(|request| request.method == "session.list")
            .unwrap();
        take_requests(respond(&mut app, models, json!({"models": []})));
        take_requests(respond(&mut app, profiles, json!({"profiles": []})));
        // Installing the last catalog resumes the uncalibrated session's own
        // read chain; the reload itself issued no view read.
        let recovery = take_requests(respond(
            &mut app,
            sessions,
            json!({"sessions": [session_info("ses_1")]}),
        ));
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.read")
        );
        assert!(
            recovery
                .iter()
                .all(|request| request.method != "turn.send" && request.method != "turn.steer")
        );
        assert!(app.sessions.known["ses_1"].event_gap);
        assert!(!app.sessions.known["ses_1"].transcript.complete);

        if let Some(fresh_state) = recovery
            .iter()
            .find(|request| request.method == "session.state")
        {
            take_requests(respond(&mut app, fresh_state, state_json("ses_1", "idle")));
        }
        let history = recovery
            .iter()
            .find(|request| request.method == "session.read")
            .unwrap();
        take_requests(respond(&mut app, history, read_page_json(vec![], None, 0)));
        assert!(!app.sessions.known["ses_1"].event_gap);
        assert!(app.sessions.known["ses_1"].transcript.complete);
    }

    #[test]
    fn a_create_ack_after_a_reload_failure_starts_fresh_session_reads() {
        let mut app = test_app();
        ready(&mut app);

        let create = take_requests(app.update(AppEvent::CreateSession {
            workspace: "/project".to_owned(),
            profile: None,
            model: None,
            reasoning: None,
            title: None,
        }))
        .remove(0);
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        take_requests(respond(&mut app, &reload, json!({"ok": false})));

        let reads = take_requests(respond(
            &mut app,
            &create,
            json!({"session": session_info("ses_created_after_reload")}),
        ));
        assert!(
            reads
                .iter()
                .any(|request| request.method == "session.state")
        );
        let history = reads
            .iter()
            .find(|request| request.method == "session.read")
            .expect("the new session starts its own history read");
        assert!(matches!(
            app.pending_requests.get(&history.id),
            Some(RequestKind::History { .. })
        ));
    }

    #[test]
    fn a_create_ack_during_a_reload_stays_uncalibrated_until_recovery() {
        let mut app = test_app();
        ready(&mut app);

        let create = take_requests(app.update(AppEvent::CreateSession {
            workspace: "/project".to_owned(),
            profile: None,
            model: None,
            reasoning: None,
            title: None,
        }))
        .remove(0);
        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);

        take_requests(respond(
            &mut app,
            &create,
            json!({"session": session_info("ses_created_during_reload")}),
        ));
        let view = &app.sessions.known["ses_created_during_reload"];
        assert!(view.state.is_none());
        assert!(view.event_gap);
        assert!(!view.transcript.complete);

        let recovery = take_requests(respond(&mut app, &reload, json!({"ok": false})));
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.state")
        );
        assert!(
            recovery
                .iter()
                .any(|request| request.method == "session.read")
        );
    }

    /// Builds a `session.list` row with an explicit title.
    fn titled_session_info(session_id: &str, title: &str) -> Value {
        let mut value = session_info(session_id);
        value["title"] = json!(title);
        value
    }

    fn request_rename(app: &mut App, session_id: &str, title: &str) -> OutgoingRequest {
        match app.request(
            RequestKind::RenameSession {
                session_id: session_id.to_owned(),
            },
            |id| OutgoingRequest::session_rename(id, session_id, title),
        ) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_late_session_list_cannot_resurrect_a_renamed_title() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let refresh = take_requests(app.update(AppEvent::OpenSessionSelector))
            .into_iter()
            .find(|request| request.method == "session.list")
            .expect("opening the session selector issues a session.list request");
        assert_eq!(refresh.method, "session.list");

        // A rename ACK lands while the list response is in flight.
        let rename = request_rename(&mut app, "ses_1", "renamed");
        take_requests(respond(
            &mut app,
            &rename,
            json!({"session": titled_session_info("ses_1", "renamed")}),
        ));
        assert_eq!(
            app.sessions.known["ses_1"].info.title.as_deref(),
            Some("renamed")
        );

        // The stale list still carries the old title: it is discarded and a
        // fresh list is requested instead.
        let after_stale = take_requests(respond(
            &mut app,
            &refresh,
            json!({"sessions": [titled_session_info("ses_1", "old title")]}),
        ));
        assert_eq!(after_stale.len(), 1);
        assert_eq!(after_stale[0].method, "session.list");
        assert_eq!(
            app.sessions.known["ses_1"].info.title.as_deref(),
            Some("renamed"),
            "a pre-rename catalog response never overwrites the new title"
        );
        assert!(
            app.sessions
                .list
                .iter()
                .all(|session| session.title.as_deref() != Some("old title"))
        );

        // The re-issued list is authoritative.
        take_requests(respond(
            &mut app,
            &after_stale[0],
            json!({"sessions": [titled_session_info("ses_1", "renamed")]}),
        ));
        assert_eq!(
            app.sessions.known["ses_1"].info.title.as_deref(),
            Some("renamed")
        );
    }

    #[test]
    fn a_late_session_list_cannot_resurrect_a_deleted_session() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let refresh = take_requests(app.update(AppEvent::OpenSessionSelector))
            .into_iter()
            .find(|request| request.method == "session.list")
            .expect("opening the session selector issues a session.list request");

        let delete = match app.request(
            RequestKind::DeleteSession {
                session_id: "ses_1".to_owned(),
            },
            |id| OutgoingRequest::session_delete(id, "ses_1"),
        ) {
            AppCommand::Rpc(request) => request,
            _ => unreachable!(),
        };
        take_requests(respond(&mut app, &delete, json!({"ok": true})));
        assert!(!app.sessions.known.contains_key("ses_1"));
        assert!(app.session_absent("ses_1"));

        let after_stale = take_requests(respond(
            &mut app,
            &refresh,
            json!({"sessions": [titled_session_info("ses_1", "deleted row")]}),
        ));
        assert_eq!(after_stale.len(), 1);
        assert_eq!(after_stale[0].method, "session.list");
        assert!(
            !app.sessions.known.contains_key("ses_1"),
            "a pre-delete catalog response never resurrects the session"
        );
        assert!(
            app.sessions
                .list
                .iter()
                .all(|session| session.session_id != "ses_1")
        );
    }

    #[test]
    fn a_stale_reload_list_is_reissued_before_the_reload_completes() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        let catalogs = take_requests(respond(&mut app, &reload, json!({"ok": true})));
        let models = catalogs
            .iter()
            .find(|request| request.method == "model.list")
            .unwrap();
        let profiles = catalogs
            .iter()
            .find(|request| request.method == "profile.list")
            .unwrap();
        let sessions = catalogs
            .iter()
            .find(|request| request.method == "session.list")
            .unwrap();
        take_requests(respond(&mut app, profiles, json!({"profiles": []})));
        take_requests(respond(&mut app, models, json!({"models": []})));

        // A local rename lands before the reload's session list arrives.
        let rename = request_rename(&mut app, "ses_1", "renamed during reload");
        take_requests(respond(
            &mut app,
            &rename,
            json!({"session": titled_session_info("ses_1", "renamed during reload")}),
        ));

        let after_stale = take_requests(respond(
            &mut app,
            sessions,
            json!({"sessions": [titled_session_info("ses_1", "old title")]}),
        ));
        assert_eq!(after_stale.len(), 1);
        assert_eq!(after_stale[0].method, "session.list");
        assert!(
            app.reload.is_some(),
            "the reload waits for the fresh catalog instead of installing a stale one"
        );

        take_requests(respond(
            &mut app,
            &after_stale[0],
            json!({"sessions": [titled_session_info("ses_1", "renamed during reload")]}),
        ));
        assert!(app.reload.is_none());
        assert_eq!(
            app.sessions.known["ses_1"].info.title.as_deref(),
            Some("renamed during reload")
        );
    }

    #[test]
    fn incomplete_reload_keeps_active_session_gap_after_channel_end() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        take_requests(app.update(AppEvent::Reload));
        take_requests(app.update(AppEvent::RpcChannelEnded));

        assert!(app.reload.is_none());
        assert!(app.sessions.known["ses_1"].event_gap);
        assert!(!app.sessions.known["ses_1"].transcript.complete);
        assert!(app.notices().iter().any(|notice| {
            notice
                .text
                .contains("configuration reload outcome is unknown; no automatic retry")
        }));
    }

    #[test]
    fn acknowledged_reload_channel_end_does_not_claim_refresh_success() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let reload = take_requests(app.update(AppEvent::Reload)).remove(0);
        take_requests(respond(&mut app, &reload, json!({"ok": true})));
        take_requests(app.update(AppEvent::RpcChannelEnded));

        assert!(app.notices().iter().any(|notice| {
            notice.text.contains(
                "Agent configuration reloaded; catalog refresh outcome is unknown; no automatic retry",
            )
        }));
        assert!(app.sessions.known["ses_1"].event_gap);
        assert!(app.sessions.known["ses_1"].state.is_none());
    }

    #[test]
    fn bootstrap_rejects_a_non_v1_protocol_and_latches_failed() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let ping_req = requests.iter().find(|r| r.method == "agent.ping").unwrap();
        respond(
            &mut app,
            ping_req,
            json!({
                "version": "0.5.0",
                "protocol_version": 0,
                "capabilities": crate::protocol::REQUIRED_CAPABILITIES,
            }),
        );
        assert!(matches!(app.connection, ConnectionState::Failed(_)));
        let notice = app.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(
            notice.text.contains("protocol_version"),
            "notice names the protocol mismatch: {}",
            notice.text
        );
    }

    #[test]
    fn bootstrap_rejects_a_backend_missing_required_capabilities() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let ping_req = requests.iter().find(|r| r.method == "agent.ping").unwrap();
        respond(
            &mut app,
            ping_req,
            json!({
                "version": "0.5.0",
                "protocol_version": 1,
                "capabilities": ["session.read"],
            }),
        );
        assert!(matches!(app.connection, ConnectionState::Failed(_)));
        let notice = app.notices.back().unwrap();
        assert!(
            notice.text.contains("tool.read"),
            "notice names a missing capability: {}",
            notice.text
        );
    }

    #[test]
    fn bootstrap_accepts_a_v1_backend_with_the_full_capability_set() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        for req in &requests {
            let res = match req.method {
                "agent.ping" => json!({
                    "version": "0.5.0",
                    "protocol_version": 1,
                    "capabilities": crate::protocol::REQUIRED_CAPABILITIES,
                }),
                "model.list" => json!({"models": []}),
                "profile.list" => json!({"profiles": []}),
                "session.list" => json!({"sessions": []}),
                _ => unreachable!(),
            };
            take_requests(respond(&mut app, req, res));
        }
        assert_eq!(app.connection, ConnectionState::Ready);
    }

    #[test]
    fn create_session_activates_and_pages_history() {
        let mut app = test_app();
        ready(&mut app);
        let requests = take_requests(app.update(AppEvent::CreateSession {
            workspace: "/w".into(),
            profile: None,
            model: None,
            reasoning: None,
            title: None,
        }));
        assert_eq!(requests.len(), 1);
        let create = &requests[0];
        assert_eq!(create.method, "session.create");
        let commands = respond(&mut app, create, json!({"session": session_info("ses_1")}));
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 4);
        let status = requests
            .iter()
            .find(|r| r.method == "workspace.status")
            .unwrap();
        take_requests(respond_error(
            &mut app,
            status,
            -32000,
            "status unavailable",
        ));
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let history_request = requests
            .iter()
            .find(|r| r.method == "session.read")
            .unwrap();
        let presentation_request = requests
            .iter()
            .find(|r| r.method == "session.presentation")
            .unwrap();
        assert_eq!(app.sessions.active.as_deref(), Some("ses_1"));
        assert!(app.sessions.known["ses_1"].history_read.is_loading());

        take_requests(respond(
            &mut app,
            state_request,
            state_json("ses_1", "idle"),
        ));
        take_requests(respond(
            &mut app,
            presentation_request,
            json!({
                "session_id": "ses_1",
                "context": {"kind": "unknown"}
            }),
        ));

        // Page 1 is partial
        let page1 = read_page_json(
            vec![
                user_item(0, "loop_1", "hello"),
                assistant_item(1, "loop_1", "hi back"),
            ],
            Some(2),
            3,
        );
        let commands = respond(&mut app, history_request, page1);
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.read");
        assert_eq!(
            app.pending_requests.get(&requests[0].id),
            Some(&RequestKind::History {
                session_id: "ses_1".into(),
                read: ReadRequest {
                    cursor: crate::protocol::ReadCursor { item: 2, offset: 0 },
                    pin: Some(crate::protocol::SnapshotPin {
                        captured_end: 3,
                        history_revision:
                            "0000000000000000000000000000000000000000000000000000000000000000"
                                .to_owned(),
                        total: 3,
                    }),
                    window_start: 2,
                    replacement: false,
                    reconcile: false,
                    probe: false,
                    gap_revision: 0,
                },
            })
        );
        assert!(app.sessions.known["ses_1"].history_read.is_loading());

        // Page 2 completes chain
        let commands = respond(
            &mut app,
            &requests[0],
            read_page_json(vec![user_item(2, "loop_1", "follow up")], None, 3),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.history_read.is_loading());
        assert!(view.transcript.complete);
        assert_eq!(view.transcript.blocks.len(), 3);
    }

    #[test]
    fn reopening_a_loaded_session_is_idempotent() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let requests = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.state");
        take_requests(respond(&mut app, &requests[0], state_json("ses_1", "idle")));
        let view = &app.sessions.known["ses_1"];
        assert!(!view.history_read.is_loading());
        assert!(view.transcript.complete);
    }

    #[test]
    fn composer_routes_to_steer_turn_when_session_is_running() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        // Set session state to running with active loop
        if let Some(view) = app.sessions.known.get_mut("ses_1") {
            if let Some(state) = view.state.as_mut() {
                state.status = SessionStatusWire::Running;
                state.active_loop = Some(crate::protocol::LoopStateWire {
                    loop_id: "loop_99".to_owned(),
                    status: crate::protocol::LoopStatusWire::RunningModel,
                    request_index: 0,
                    config_revision: 0,
                    model: Some("deep".to_owned()),
                    pending_interaction: None,
                });
            }
            view.live = Some(LiveLoop {
                reference: Some(make_turn("ses_1", "loop_99")),
                local_submission: LocalSubmissionId(1),
                user_text: "initial prompt".into(),
                requests: vec![],
                pending_steers: vec![],
                waiting: false,
                cancel_requested: false,
                event_gap: false,
                last_result: None,
            });
            view.live_user_timestamp = None;
            view.live_user_time_accepted = false;
        }

        // 0.2.4 FIFO: admission is local; the central update advance issues
        // the single in-flight turn.steer RPC.
        let commands = app.update(AppEvent::SteerTurn {
            session_id: "ses_1".into(),
            text: "Please correct this direction".into(),
        });
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "turn.steer");
        assert_eq!(requests[0].params["session_id"], "ses_1");
        assert_eq!(requests[0].params["loop_id"], "loop_99");
        assert_eq!(requests[0].params["text"], "Please correct this direction");

        let view = &app.sessions.known["ses_1"];
        assert!(
            view.steer_queue.is_empty(),
            "head admitted then popped for the RPC"
        );
        let live = view.live.as_ref().unwrap();
        assert_eq!(live.pending_steers.len(), 1);
        assert_eq!(live.pending_steers[0].state, PendingSteerState::Sending);
    }

    #[test]
    fn composer_clears_only_on_matching_submit_and_late_ack_never_clears_new_content() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        if let Some(view) = app.sessions.known.get_mut("ses_1") {
            if let Some(state) = view.state.as_mut() {
                state.status = SessionStatusWire::Running;
                state.active_loop = Some(crate::protocol::LoopStateWire {
                    loop_id: "loop_99".to_owned(),
                    status: crate::protocol::LoopStatusWire::RunningModel,
                    request_index: 0,
                    config_revision: 0,
                    model: Some("deep".to_owned()),
                    pending_interaction: None,
                });
            }
            view.live = Some(LiveLoop {
                reference: Some(make_turn("ses_1", "loop_99")),
                local_submission: LocalSubmissionId(1),
                user_text: "prompt".into(),
                requests: vec![],
                pending_steers: vec![],
                waiting: false,
                cancel_requested: false,
                event_gap: false,
                last_result: None,
            });
        }

        // Enter path (carries the composer revision): the matching submit
        // clears the editor at admission, and the same call issues the single
        // in-flight steer via the central FIFO advance.
        app.composer.set_text("Steer text");
        let commands = app.submit_composer();
        let reqs = take_requests(commands);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "turn.steer");
        assert_eq!(app.composer.content(), "", "cleared at admission");

        // A late ACK must never clear NEW editor content typed meanwhile.
        app.composer.set_text("New content");
        respond(&mut app, &reqs[0], json!({"ok": true, "steer_index": 1}));
        assert_eq!(app.composer.content(), "New content");
        let view = &app.sessions.known["ses_1"];
        let steer = &view.live.as_ref().unwrap().pending_steers[0];
        assert_eq!(steer.state, PendingSteerState::Queued);
        assert_eq!(steer.steer_index, Some(1));
    }

    #[test]
    fn composer_retains_text_on_steer_failure_and_shows_warning() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        if let Some(view) = app.sessions.known.get_mut("ses_1") {
            if let Some(state) = view.state.as_mut() {
                state.status = SessionStatusWire::Running;
                state.active_loop = Some(crate::protocol::LoopStateWire {
                    loop_id: "loop_99".to_owned(),
                    status: crate::protocol::LoopStatusWire::RunningModel,
                    request_index: 0,
                    config_revision: 0,
                    model: Some("deep".to_owned()),
                    pending_interaction: None,
                });
            }
            view.live = Some(LiveLoop {
                reference: Some(make_turn("ses_1", "loop_99")),
                local_submission: LocalSubmissionId(1),
                user_text: "prompt".into(),
                requests: vec![],
                pending_steers: vec![],
                waiting: false,
                cancel_requested: false,
                event_gap: false,
                last_result: None,
            });
        }

        // Direct steer (no editor revision): text stays in the composer; a
        // rejection pauses the queue and shows a warning.
        app.composer.set_text("Steer text");
        let commands = app.update(AppEvent::SteerTurn {
            session_id: "ses_1".into(),
            text: "Steer text".into(),
        });
        let reqs = take_requests(commands);
        assert_eq!(reqs.len(), 1);
        assert_eq!(app.composer.content(), "Steer text");
        respond_error(&mut app, &reqs[0], -32016, "steer queue full");
        assert_eq!(app.composer.content(), "Steer text");

        let notice = app.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Warning);
        assert!(notice.text.contains("queue is full"));
        assert!(
            app.sessions.known["ses_1"].steer_queue_paused,
            "rejection pauses the unsent queue"
        );
    }

    #[test]
    fn composer_rejects_submit_when_session_is_blocked() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        if let Some(view) = app.sessions.known.get_mut("ses_1") {
            if let Some(state) = view.state.as_mut() {
                state.status = SessionStatusWire::Blocked;
                state.block_reason = Some(crate::protocol::SessionBlockReasonWire::Persistence);
            }
        }

        app.composer.set_text("Can I submit?");
        let commands = app.submit_composer();
        assert!(take_requests(commands).is_empty());
        assert_eq!(app.composer.content(), "Can I submit?");

        let notice = app.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(notice.text.contains("session is blocked"));
    }

    #[test]
    fn turn_wait_persistence_failure_latches_blocked_and_creates_unsaved_loop() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let commands = app.submit_turn("ses_1".into(), "Do work".into());
        let send_req = &take_requests(commands)[0];
        let wait_commands = respond(
            &mut app,
            send_req,
            json!({"turn": {"session_id": "ses_1", "loop_id": "loop_42"}}),
        );
        let wait_req = &take_requests(wait_commands)[0];
        assert_eq!(wait_req.method, "turn.wait");

        let reconcile_commands = respond(
            &mut app,
            wait_req,
            json!({
                "turn": {"session_id": "ses_1", "loop_id": "loop_42"},
                "outcome": {"type": "completed"},
                "usage": {},
                "requests": 1,
                "tool_rounds": 0,
                "final_config_revision": 0,
                "persistence": "failed"
            }),
        );
        let reqs = take_requests(reconcile_commands);
        assert!(
            reqs.is_empty(),
            "failed persistence must not dispatch reconcile requests"
        );

        let view = &app.sessions.known["ses_1"];
        assert!(view.is_blocked());
        assert!(view.unsaved_loop.is_some());
        assert_eq!(view.unsaved_loop.as_ref().unwrap().turn.loop_id, "loop_42");
    }

    #[test]
    fn a_queue_full_send_restores_the_prompt_and_never_auto_retries() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let request = take_requests(app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "never admitted".to_owned(),
        }))
        .remove(0);
        assert_eq!(request.method, "turn.send");
        let more = app.update(AppEvent::RpcQueueFull {
            request: request.clone(),
            class: SendClass::Normal,
        });
        assert!(
            take_requests(more).is_empty(),
            "a refusal never re-sends in the same reducer pass"
        );
        // Ordinary sends keep the user's input and report Busy; they are never
        // re-emitted automatically on the next progress signal.
        assert!(
            take_requests(app.update(AppEvent::Tick)).is_empty(),
            "turn.send is not re-emitted automatically"
        );
        assert!(app.pending_retries.is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(
            view.live.is_none(),
            "the never-sent live placeholder is removed"
        );
        assert_eq!(
            app.composer.content(),
            "never admitted",
            "the prompt is back in the editor and was never dropped"
        );
        assert!(
            app.notices()
                .iter()
                .any(|notice| notice.text.contains("busy")),
            "the user is told the send path is busy"
        );
    }

    #[test]
    fn a_refused_cancel_is_retained_until_it_is_admitted() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let turn = make_turn("ses_1", "loop_1");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.state = Some(
            serde_json::from_value(running_state_json("ses_1", "loop_1")).expect("running state"),
        );
        view.live = Some(LiveLoop {
            reference: Some(turn.clone()),
            local_submission: LocalSubmissionId(1),
            user_text: "prompt".into(),
            requests: vec![],
            pending_steers: vec![],
            waiting: false,
            cancel_requested: false,
            event_gap: false,
            last_result: None,
        });
        let mut request = take_requests(app.update(AppEvent::CancelTurn {
            session_id: "ses_1".to_owned(),
        }))
        .remove(0);
        assert_eq!(request.method, "turn.cancel");
        // The FIFO stays full for many progress signals; the cancel must never
        // be abandoned, and no more than one intent per target is retained.
        for _ in 0..8 {
            app.update(AppEvent::RpcQueueFull {
                request: request.clone(),
                class: SendClass::Control,
            });
            assert_eq!(
                app.pending_retries.len(),
                1,
                "the cancel is retained while the FIFO is full"
            );
            let retried = take_requests(app.update(AppEvent::Tick));
            assert_eq!(retried.len(), 1, "the retained cancel is re-emitted");
            assert_eq!(retried[0].method, "turn.cancel");
            request = retried[0].clone();
        }
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .is_some_and(|live| live.cancel_requested)
        );
    }

    #[test]
    fn a_deferred_wait_retry_is_retired_at_a_session_lifecycle_boundary() {
        let mut app = test_app();
        let turn = make_turn("ses_1", "loop_1");
        let kind = RequestKind::WaitTurn(turn.clone());
        let request_id = app.next_request_id();
        let request = OutgoingRequest::wait_turn(request_id, &turn);
        let retry_key = App::retry_key(&kind).expect("wait retry key");
        app.pending_retries
            .insert(retry_key, RetryEntry { kind, request });

        app.retire_session_operations(&"ses_1".to_owned());

        assert!(app.pending_retries.is_empty());
        assert!(
            app.drain_rpc_retries().is_empty(),
            "a never-written wait must not be re-emitted after close/reopen"
        );
    }

    #[test]
    fn a_queue_full_turn_result_retry_reclaims_its_query_slot() {
        let mut app = test_app();
        let turn = make_turn("ses_1", "loop_1");
        let request = match app
            .request_turn_result_page(turn, crate::protocol::ReadCursor::start())
            .expect("turn.result request")
        {
            AppCommand::Rpc(request) => request,
            other => panic!("expected RPC command, got {other:?}"),
        };
        assert_eq!(app.queries.in_flight_len(), 1);

        app.update(AppEvent::RpcQueueFull {
            request,
            class: SendClass::Normal,
        });
        assert_eq!(app.queries.in_flight_len(), 0);
        let retry = take_requests(app.update(AppEvent::Tick));
        assert_eq!(retry.len(), 1);
        assert!(app.queries.owns_request(retry[0].id));
        assert_eq!(app.queries.in_flight_len(), 1);
    }

    #[test]
    fn a_rejected_send_keeps_an_existing_draft_and_appends_the_unsent_prompt() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        app.composer.set_text("existing draft");
        let request = take_requests(app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "unsent follow up".to_owned(),
        }))
        .remove(0);
        app.update(AppEvent::RpcSendFailed {
            id: request.id,
            error: RpcError::RequestTooLarge {
                actual_bytes: 2 * 1024 * 1024,
                max_bytes: 1024 * 1024,
            },
        });
        let content = app.composer.content();
        assert!(
            content.contains("existing draft"),
            "the draft survives: {content}"
        );
        assert!(
            content.contains("unsent follow up"),
            "the never-sent prompt survives too: {content}"
        );
        assert!(app.sessions.known["ses_1"].live.is_none());
    }

    #[test]
    fn a_queue_full_steer_returns_to_the_paused_queue() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let turn = make_turn("ses_1", "loop_1");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.state = Some(
            serde_json::from_value(running_state_json("ses_1", "loop_1")).expect("running state"),
        );
        view.live = Some(LiveLoop {
            reference: Some(turn),
            local_submission: LocalSubmissionId(1),
            user_text: "prompt".into(),
            requests: vec![],
            pending_steers: vec![],
            waiting: false,
            cancel_requested: false,
            event_gap: false,
            last_result: None,
        });
        app.steer_turn(&"ses_1".to_owned(), "steer text".to_owned());
        let request = take_requests(app.update(AppEvent::Tick)).remove(0);
        assert_eq!(request.method, "turn.steer");
        app.update(AppEvent::RpcQueueFull {
            request: request.clone(),
            class: SendClass::Normal,
        });
        assert!(
            take_requests(app.update(AppEvent::Tick)).is_empty(),
            "a refused steer is never auto-retried"
        );
        let view = &app.sessions.known["ses_1"];
        assert!(
            view.steer_queue_paused,
            "a never-sent steer never auto-resends"
        );
        assert!(view.live.as_ref().unwrap().pending_steers.is_empty());
        assert_eq!(view.steer_queue.len(), 1);
        assert_eq!(view.steer_queue[0].text, "steer text");
        assert_eq!(
            view.steer_queue[0].state,
            crate::state::turn::SteerQueueState::Unsent
        );
        assert!(app.notices().iter().any(|notice| {
            notice
                .text
                .contains("the steer stays in the paused queue and was not sent")
        }));
    }

    #[test]
    fn live_loop_records_multi_request_deltas_and_tools() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let turn = make_turn("ses_1", "loop_7");
        if let Some(view) = app.sessions.known.get_mut("ses_1") {
            view.live = Some(LiveLoop {
                reference: Some(turn.clone()),
                local_submission: LocalSubmissionId(1),
                user_text: "multi request task".into(),
                requests: vec![],
                pending_steers: vec![],
                waiting: false,
                cancel_requested: false,
                event_gap: false,
                last_result: None,
            });
        }

        // Request 0 starts
        app.update(event(wire_event(json!({
            "type": "request_started",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_7"),
                "request_index": 0,
                "config_revision": 0,
                "model": "deep",
                "reasoning": "high",
                "meta": meta_json("ses_1", 0)
            }
        }))));

        // Request 0 delta
        app.update(event(wire_event(json!({
            "type": "output_delta",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_7"),
                "request_index": 0,
                "channel": "text",
                "delta": "Thinking...",
                "meta": meta_json("ses_1", 0)
            }
        }))));

        // Tool on request 0
        app.update(event(wire_event(json!({
            "type": "tool_started",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_7"),
                "request_index": 0,
                "tool_call_id": "call_1",
                "tool_name": "read",
                "meta": meta_json("ses_1", 0)
            }
        }))));

        // Request 1 starts
        app.update(event(wire_event(json!({
            "type": "request_started",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_7"),
                "request_index": 1,
                "config_revision": 0,
                "model": "deep",
                "reasoning": "high",
                "meta": meta_json("ses_1", 0)
            }
        }))));

        // Request 1 delta
        app.update(event(wire_event(json!({
            "type": "output_delta",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_7"),
                "request_index": 1,
                "channel": "text",
                "delta": "Done with second iteration.",
                "meta": meta_json("ses_1", 0)
            }
        }))));

        let view = &app.sessions.known["ses_1"];
        let live = view.live.as_ref().unwrap();
        assert_eq!(live.requests.len(), 2);
        assert_eq!(live.requests[0].visible_text(), "Thinking...");
        assert_eq!(live.requests[0].tools.len(), 1);
        assert_eq!(
            live.requests[1].visible_text(),
            "Done with second iteration."
        );
    }

    #[test]
    fn request_usage_event_populates_live_projection_by_request_index() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let turn = make_turn("ses_1", "loop_fallback");
        app.sessions.known.get_mut("ses_1").unwrap().live = Some(LiveLoop {
            reference: Some(turn.clone()),
            local_submission: LocalSubmissionId(1),
            user_text: "run it".to_owned(),
            requests: Vec::new(),
            pending_steers: Vec::new(),
            waiting: false,
            cancel_requested: false,
            event_gap: false,
            last_result: None,
        });

        app.update(event(wire_event(json!({
            "type": "request_usage",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_fallback"),
                "request_index": 0,
                "usage": {
                    "input_tokens": 42,
                    "output_tokens": 7,
                    "reasoning_tokens": 2,
                    "cache_read_tokens": 0,
                    "cache_write_tokens": 0
                },
                "meta": meta_json("ses_1", 0)
            }
        }))));

        let view = &app.sessions.known["ses_1"];
        assert_eq!(
            view.live_request_usage
                .get(&("loop_fallback".to_owned(), 0))
                .map(|usage| usage.input_tokens),
            Some(Some(42)),
            "the reported request usage must land in the live map"
        );
        assert_eq!(
            view.usage_projection.usage.input_tokens,
            Some(42),
            "the footer projection must show the reported usage, not a fake zero"
        );
        assert_eq!(
            view.usage_projection.completeness,
            crate::state::session::UsageCompleteness::Partial
        );
    }

    #[test]
    fn tool_result_without_presentation_gets_a_safe_fallback() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let turn = make_turn("ses_1", "loop_fallback");
        app.sessions.known.get_mut("ses_1").unwrap().live = Some(LiveLoop {
            reference: Some(turn.clone()),
            local_submission: LocalSubmissionId(1),
            user_text: "run it".to_owned(),
            requests: Vec::new(),
            pending_steers: Vec::new(),
            waiting: false,
            cancel_requested: false,
            event_gap: false,
            last_result: None,
        });

        app.update(event(wire_event(json!({
            "type": "tool_started",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_fallback"),
                "request_index": 0,
                "tool_call_id": "call_fallback",
                "tool_name": "read",
                "meta": meta_json("ses_1", 0)
            }
        }))));
        app.update(event(wire_event(json!({
            "type": "tool_finished",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_fallback"),
                "request_index": 0,
                "tool_call_id": "call_fallback",
                "result": {
                    "outcome": "success",
                    "content_bytes": 3,
                    "content": "a\nb",
                    "content_truncated": false
                },
                "meta": meta_json("ses_1", 0)
            }
        }))));

        let view = &app.sessions.known["ses_1"];
        let presentation =
            &view.tool_presentations[&ToolKey::new("ses_1", "loop_fallback", 0, "call_fallback")];
        assert_eq!(presentation.display.detail, "read");
        assert_eq!(presentation.display.hidden_line_count, Some(2));
        assert_eq!(presentation.result.as_deref(), Some("a\nb"));
    }

    #[test]
    fn tool_result_body_is_one_arc_across_live_history_and_presentation() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");
        let turn = make_turn("ses_1", "loop_shared");
        app.sessions.known.get_mut("ses_1").unwrap().live = Some(LiveLoop {
            reference: Some(turn.clone()),
            local_submission: LocalSubmissionId(1),
            user_text: "run".to_owned(),
            requests: Vec::new(),
            pending_steers: Vec::new(),
            waiting: false,
            cancel_requested: false,
            event_gap: false,
            last_result: None,
        });
        app.update(event(wire_event(json!({
            "type": "tool_started",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_shared"),
                "request_index": 0,
                "tool_call_id": "call_shared",
                "tool_name": "read",
                "meta": meta_json("ses_1", 0)
            }
        }))));
        app.update(event(wire_event(json!({
            "type": "tool_finished",
            "data": {
                "turn": turn_ref_json("ses_1", "loop_shared"),
                "request_index": 0,
                "tool_call_id": "call_shared",
                "result": {
                    "outcome": "success",
                    "content_bytes": 6,
                    "content": "shared",
                    "content_truncated": false
                },
                "meta": meta_json("ses_1", 0)
            }
        }))));

        let weak = {
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            let item = crate::protocol::read::RawHistoryItem {
                item: crate::protocol::read::RuntimeItem::ToolResult(
                    crate::protocol::read::RuntimeToolResultItem {
                        loop_id: "loop_shared".to_owned(),
                        request_index: 0,
                        call_id: "call_shared".to_owned(),
                        tool_name: "read".to_owned(),
                        outcome: "success".to_owned(),
                        output: crate::protocol::read::RuntimeToolOutput {
                            content: "shared".to_owned(),
                        },
                    },
                ),
                timestamp: None,
            };
            let owner = install_history_item(view, 1, &item).expect("durable owner");
            let durable_result = match owner.as_ref() {
                TranscriptBlock::Tool(tool) => tool.result.as_ref().unwrap().clone(),
                _ => panic!("expected durable tool owner"),
            };
            let presentation_result = view.tool_presentations
                [&ToolKey::new("ses_1", "loop_shared", 0, "call_shared")]
                .result
                .as_ref()
                .unwrap()
                .clone();
            let live_result = view.live.as_ref().unwrap().requests[0].tools[0]
                .result
                .as_ref()
                .unwrap()
                .clone();
            let presentation_display = view.tool_presentations
                [&ToolKey::new("ses_1", "loop_shared", 0, "call_shared")]
                .display
                .clone();
            let live_display = view.live.as_ref().unwrap().requests[0].tools[0]
                .display
                .as_ref()
                .unwrap()
                .clone();
            assert!(Arc::ptr_eq(&durable_result, &presentation_result));
            assert!(Arc::ptr_eq(&durable_result, &live_result));
            assert!(Arc::ptr_eq(&presentation_display, &live_display));
            let weak = Arc::downgrade(&durable_result);
            let weak_display = Arc::downgrade(&presentation_display);
            view.transcript.window.insert_owner(
                1,
                Arc::clone(&owner),
                1,
                crate::app::history::owner_bytes(&owner),
            );
            (weak, weak_display)
        };
        app.prepared_conversation = None;
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live = None;
        view.tool_presentations = Arc::new(std::collections::HashMap::new());
        view.transcript.clear_blocks();
        assert!(
            weak.0.upgrade().is_none(),
            "all body owners must be released"
        );
        assert!(
            weak.1.upgrade().is_none(),
            "all display owners must be released"
        );
    }

    #[test]
    fn history_merges_user_assistant_tool_summary_without_synthetic_terminals() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1");

        let items = vec![
            user_item(0, "loop_1", "start"),
            assistant_item(1, "loop_1", "answer"),
            tool_result_item(2, "loop_1", "call_1", "read", "success", "file content"),
            json!({
                "index": 3,
                "item": {
                    "type": "summary",
                    "data": {
                        "content": "compacted"
                    }
                }
            }),
        ];

        let req = take_requests(app.clear_transcript());
        respond(&mut app, &req[0], read_page_json(items, None, 4));

        let view = &app.sessions.known["ses_1"];
        assert_eq!(view.transcript.blocks.len(), 4); // user, assistant, orphan tool_result, summary
        assert!(view.transcript.blocks.iter().all(|b| b.index().is_some()));
    }

    #[test]
    fn wrapped_editor_vertical_motion_uses_visual_rows_and_preserves_history_edges() {
        let mut app = test_app();
        let text = "x".repeat(90);
        app.composer.set_text(&text);
        app.composer.submit_pushed("old message");

        app.apply_action(crate::keymap::Action::CursorMove(EditorCursor::Up));
        assert_eq!(app.composer.cursor(), (0, 12));
        app.apply_action(crate::keymap::Action::CursorMove(EditorCursor::Up));
        assert_eq!(app.composer.content(), text);
        assert_eq!(app.composer.cursor(), (0, 12));

        app.composer.move_to(0, 0);
        app.apply_action(crate::keymap::Action::CursorMove(EditorCursor::Up));
        assert_eq!(app.composer.content(), "old message");
    }

    #[test]
    fn slash_completion_matches_native_trigger_and_selection_rules() {
        let mut app = test_app();
        app.composer.set_text(" /re");
        ui_actions::refresh_slash_completion(&mut app);
        assert!(app.slash_completion.is_none());

        app.composer.set_text("/re");
        ui_actions::refresh_slash_completion(&mut app);
        let completion = app.slash_completion.as_ref().expect("slash popup");
        assert_eq!(completion.start, 0);
        assert_eq!(completion.end, 3);
        assert!(completion.items.iter().any(|item| item == "/resume"));
        assert_eq!(completion.items[completion.selected], "/resume");

        ui_actions::accept_slash_completion(&mut app);
        assert_eq!(app.composer.content(), "/resume ");
        assert!(app.slash_completion.is_none());

        app.composer.set_text("/zzz");
        ui_actions::refresh_slash_completion(&mut app);
        assert!(app.slash_completion.is_none());
    }

    #[test]
    fn enter_accepts_a_skill_completion_without_submitting() {
        let mut app = test_app();
        app.composer.set_text("/ski");
        app.slash_completion = Some(SlashCompletionState {
            start: 0,
            end: 4,
            items: vec!["/skill:web-access".to_owned()],
            selected: 0,
        });

        let commands = app.apply_action(crate::keymap::Action::CompletionAcceptAndSubmit);

        assert!(commands.is_empty());
        assert_eq!(app.composer.content(), "/skill:web-access ");
        assert!(app.slash_completion.is_none());
    }

    #[test]
    fn editor_click_and_drag_use_visual_cells_and_copy_the_selected_text() {
        let mut app = test_app();
        app.composer
            .set_text("abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ");

        let click = |kind| {
            CrosstermEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 14,
                row: 20,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };
        app.update(AppEvent::Terminal(click(MouseEventKind::Down(
            crossterm::event::MouseButton::Left,
        ))));
        app.update(AppEvent::Terminal(click(MouseEventKind::Up(
            crossterm::event::MouseButton::Left,
        ))));
        assert_eq!(app.composer.cursor(), (0, 12));

        app.composer.set_text("alpha select omega");
        let mouse = |kind, column| {
            CrosstermEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row: 20,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        };
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            8,
        )));
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            13,
        )));
        let commands = app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            13,
        )));
        assert!(matches!(
            commands.as_slice(),
            [AppCommand::CopySelection(text)] if text.as_str() == "select"
        ));
    }

    #[test]
    fn paste_normalization_matches_the_native_editor_without_changing_submit_authority() {
        let mut app = test_app();
        ui_actions::handle_paste(&mut app, "word\t/x\r\ny\u{1}".to_owned());
        assert_eq!(app.composer.content(), "word    /x\ny");

        app.composer.set_text("word");
        ui_actions::handle_paste(&mut app, "/x".to_owned());
        assert_eq!(app.composer.content(), "word /x");
    }

    #[test]
    fn selection_auto_scroll_stops_at_both_transcript_limits() {
        let mut app = crate::ui::testapp::new_output_marker(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        assert!(total > visible);
        app.viewport = (total, visible);

        let section = prepared.sections.first().expect("transcript section");
        let point = SelectionPoint {
            row: section.rows.start,
            column: section.content_columns.start,
            section_id: Some(section.id.clone()),
            section_row: 0,
        };
        app.selection = Some(ConversationSelection {
            session_id: "ses_1".to_owned(),
            anchor: point.clone(),
            focus: point,
            granularity: SelectionGranularity::Character,
            dragged: true,
        });
        app.selection_drag = Some(SelectionDrag {
            session_id: "ses_1".to_owned(),
            column: screen.content.x,
            row: screen.transcript.bottom().saturating_sub(1),
            initial: None,
            next_deadline: app.instant_now(),
        });

        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = total - visible;
        ui_actions::auto_scroll_selection(&mut app);
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            total - visible,
            "selection does not jump past the lower limit"
        );
        assert!(app.selection_drag.is_none());

        app.selection_drag = Some(SelectionDrag {
            session_id: "ses_1".to_owned(),
            column: screen.content.x,
            row: screen.transcript.y,
            initial: None,
            next_deadline: app.instant_now(),
        });
        app.active_session_mut().unwrap().scroll.offset = 0;
        ui_actions::auto_scroll_selection(&mut app);
        assert_eq!(app.active_view().unwrap().scroll.offset, 0);
        assert!(app.selection_drag.is_none());
    }

    #[test]
    fn spinner_cadence_uses_monotonic_elapsed_not_tick_count() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let elapsed = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&elapsed);
        let base = Instant::now();
        let mut app = App::with_monotonic_clock(PathBuf::from("/project"), move || {
            base + Duration::from_millis(clock.load(Ordering::Relaxed))
        });
        let mut view = SessionView::new(
            serde_json::from_value(session_info("ses_spinner")).expect("session fixture parses"),
        );
        view.live = Some(LiveLoop::new(
            LocalSubmissionId(1),
            "spinner test".to_owned(),
        ));
        app.sessions.known.insert("ses_spinner".to_owned(), view);
        app.sessions.active = Some("ses_spinner".to_owned());
        app.update(clipboard_job(&app, Err("arm spinner".to_owned())));

        assert_eq!(app.next_tick(), Some(Duration::from_millis(100)));
        for millis in [0, 10, 20, 40, 60, 80, 99] {
            elapsed.store(millis, Ordering::Relaxed);
            app.update(AppEvent::Tick);
            app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
                bytes: 12,
                dropped: 0,
            }));
            app.update(clipboard_job(&app, Err("notice traffic".to_owned())));
            assert_eq!(
                app.frame_count, 0,
                "repeated Tick/RPC/notice traffic must not accelerate the spinner"
            );
        }
        assert_eq!(
            app.next_tick(),
            Some(Duration::from_millis(1)),
            "next_tick must expose the remaining spinner cadence"
        );

        elapsed.store(100, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(app.frame_count, 1);
        for _ in 0..10 {
            app.update(AppEvent::Tick);
        }
        assert_eq!(
            app.frame_count, 1,
            "multiple Tick events at one monotonic instant must advance once"
        );

        elapsed.store(200, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(app.frame_count, 2);

        for millis in 300..=1_000 {
            elapsed.store(millis, Ordering::Relaxed);
            app.update(AppEvent::Tick);
        }
        assert_eq!(
            app.frame_count, 10,
            "the ten-frame spinner must advance at about 100ms per frame"
        );
    }

    #[test]
    fn busy_spinner_does_not_advance_selection_drag_before_its_50ms_deadline() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let elapsed = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&elapsed);
        let base = Instant::now();
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.monotonic_now =
            Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
        app.terminal_size = (80, 24);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = 0;
        app.active_session_mut()
            .unwrap()
            .state
            .as_mut()
            .unwrap()
            .status = SessionStatusWire::Running;
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let column = screen.content.x + 2;
        let start_row = screen.transcript.y + 1;
        let edge_row = screen.transcript.bottom().saturating_sub(1);
        let mouse = |kind, row| {
            AppEvent::Terminal(CrosstermEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            }))
        };
        app.update(mouse(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            start_row,
        ));
        app.update(mouse(
            crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            edge_row,
        ));
        let before = app.active_view().unwrap().scroll.offset;

        assert_eq!(app.next_tick(), Some(Duration::from_millis(50)));
        elapsed.store(33, Ordering::Relaxed);
        assert_eq!(
            app.next_tick(),
            Some(Duration::from_millis(17)),
            "selection must retain its independent 50ms deadline"
        );
        app.update(AppEvent::Tick);
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            before,
            "an early spinner/selection tick must not auto-scroll a selection"
        );
        assert_eq!(
            app.frame_count, 0,
            "an early timer tick must not advance the spinner"
        );

        elapsed.store(50, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        let after = app.active_view().unwrap().scroll.offset;
        assert!(after > before, "selection auto-scroll fires at 50ms");
        assert_eq!(app.frame_count, 0, "selection must not advance the spinner");
        elapsed.store(60, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(
            app.active_view().unwrap().scroll.offset,
            after,
            "selection auto-scroll must not run again before the next 50ms deadline"
        );
        assert_eq!(
            app.frame_count, 0,
            "selection cadence must not accelerate the spinner"
        );

        elapsed.store(100, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(
            app.frame_count, 1,
            "the spinner advances at its 100ms deadline"
        );
    }

    #[test]
    fn selection_edge_reentry_waits_for_a_fresh_50ms_deadline() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let elapsed = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&elapsed);
        let base = Instant::now();
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.monotonic_now =
            Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
        app.terminal_size = (80, 24);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = 0;
        app.active_session_mut()
            .unwrap()
            .state
            .as_mut()
            .unwrap()
            .status = SessionStatusWire::Running;
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let column = screen.content.x + 2;
        let start_row = screen.transcript.y + 1;
        let edge_row = screen.transcript.bottom().saturating_sub(1);
        let body_row = screen.transcript.y + 2;
        let mouse = |kind, row| {
            AppEvent::Terminal(CrosstermEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            }))
        };

        app.update(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            start_row,
        ));
        app.update(mouse(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            edge_row,
        ));
        elapsed.store(50, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        let after_first = app.active_view().unwrap().scroll.offset;

        for millis in [60, 70, 80, 90] {
            elapsed.store(millis, Ordering::Relaxed);
            app.update(mouse(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                edge_row,
            ));
        }
        elapsed.store(99, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(app.active_view().unwrap().scroll.offset, after_first);
        elapsed.store(100, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        let after_second = app.active_view().unwrap().scroll.offset;
        assert!(after_second > after_first);

        elapsed.store(110, Ordering::Relaxed);
        app.update(mouse(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            body_row,
        ));
        elapsed.store(150, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(app.active_view().unwrap().scroll.offset, after_second);

        elapsed.store(160, Ordering::Relaxed);
        app.update(mouse(
            MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            edge_row,
        ));
        for millis in [170, 180, 190, 200] {
            elapsed.store(millis, Ordering::Relaxed);
            app.update(mouse(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                edge_row,
            ));
        }
        elapsed.store(209, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert_eq!(app.active_view().unwrap().scroll.offset, after_second);
        elapsed.store(210, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(
            app.active_view().unwrap().scroll.offset > after_second,
            "re-entering an edge must wait 50ms without starving on repeated edge moves"
        );
    }

    #[test]
    fn marker_overlay_leaves_uncovered_links_and_blocks_its_own_cells() {
        let mut app = crate::ui::testapp::chat(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let (link_row, link_cell) = (0..prepared.total_rows())
            .find_map(|row| {
                prepared
                    .links_at(row)
                    .first()
                    .map(|range| (row, range.start))
            })
            .expect("markdown link fixture");
        let marker_budget = screen.transcript.height.saturating_sub(1) as usize;
        assert!(
            link_row >= marker_budget,
            "link must be placeable behind marker"
        );
        let offset = link_row - marker_budget;
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = offset;
        app.active_session_mut().unwrap().scroll.new_content = true;

        let marker_row = screen.transcript.bottom().saturating_sub(1);
        let column = screen.content.x + link_cell as u16;
        assert!(
            app.pressed_cell_is_link(column, marker_row),
            "uncovered link remains interactive"
        );
        let column = crate::ui::transcript::marker_area(screen.transcript, "↓ new output", false).x;
        assert!(
            !app.pressed_cell_is_link(column, marker_row),
            "the indicator itself is not a markdown link cell"
        );
        let tool_folds = app.active_view().unwrap().tool_folds.clone();
        let reasoning_folds = app.active_view().unwrap().reasoning_folds.clone();
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row: marker_row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert!(
            app.selection.is_none(),
            "the indicator cannot start selection"
        );
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column,
                row: marker_row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert_eq!(app.active_view().unwrap().tool_folds, tool_folds);
        assert_eq!(app.active_view().unwrap().reasoning_folds, reasoning_folds);
    }

    #[test]
    fn marker_row_cannot_toggle_a_tool_section() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let budget = screen.transcript.height.saturating_sub(1) as usize;
        let tool = prepared
            .sections
            .iter()
            .find(|section| section.collapsible && section.rows.start >= budget)
            .expect("tool section below the marker window");
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = tool.rows.start - budget;
        app.active_session_mut().unwrap().scroll.new_content = true;
        let marker_row = screen.transcript.bottom().saturating_sub(1);
        let column = crate::ui::transcript::marker_area(screen.transcript, "↓ new output", false).x;
        let before = app.active_view().unwrap().tool_folds.clone();

        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row: marker_row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column,
                row: marker_row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert!(app.selection.is_none());
        assert_eq!(app.active_view().unwrap().tool_folds, before);
    }

    #[test]
    fn clipboard_feedback_retries_after_failure_and_expires_after_1800ms() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let elapsed = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&elapsed);
        let base = Instant::now();
        let mut app = App::with_monotonic_clock(PathBuf::from("/project"), move || {
            base + Duration::from_millis(clock.load(Ordering::Relaxed))
        });

        app.update(clipboard_job(
            &app,
            Err("copy failed: unavailable".to_owned()),
        ));
        assert!(!app.selection_copied());
        assert!(
            app.notices()
                .iter()
                .any(|notice| notice.text.contains("copy failed"))
        );

        app.update(clipboard_job(&app, Ok(())));
        assert!(app.selection_copied());
        elapsed.store(1_799, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(app.selection_copied());
        elapsed.store(1_800, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(!app.selection_copied());
    }

    #[test]
    fn scrollbar_drag_is_live_and_cancellation_keeps_applied_position() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let elapsed = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&elapsed);
        let base = Instant::now();
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.monotonic_now =
            Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let current = app.active_view().map_or(0, |view| {
            if view.scroll.follow_tail {
                total.saturating_sub(visible)
            } else {
                view.scroll.offset
            }
        });
        let geometry = crate::ui::scrollbar::geometry(screen.transcript, total, current)
            .expect("overflowing transcript has a scrollbar");

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag((geometry.track_top + geometry.max_thumb_start / 2) as u16);
        let preview = app
            .scrollbar_preview_offset("ses_1")
            .expect("preview offset");
        let committed = (
            app.active_view().unwrap().scroll.offset,
            app.active_view().unwrap().scroll.follow_tail,
        );
        assert_eq!(app.active_view().unwrap().scroll.offset, preview);
        elapsed.store(999, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(app.scrollbar_preview_offset("ses_1").is_some());
        elapsed.store(1_000, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(app.scrollbar_preview_offset("ses_1").is_some());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            committed
        );

        app.finish_scrollbar_drag((geometry.track_top + geometry.max_thumb_start / 2) as u16);
        assert!(app.scrollbar_preview_offset("ses_1").is_none());
        assert_eq!(app.active_view().unwrap().scroll.offset, preview);

        let view = app.active_view().unwrap();
        let current = if view.scroll.follow_tail {
            total.saturating_sub(visible)
        } else {
            view.scroll.offset
        };
        let geometry = crate::ui::scrollbar::geometry(screen.transcript, total, current)
            .expect("scrollbar remains available");
        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag((geometry.track_top + geometry.max_thumb_start) as u16);
        app.update(AppEvent::Terminal(CrosstermEvent::FocusLost));
        assert!(app.scrollbar_preview_offset("ses_1").is_none());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            (0, true)
        );
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: geometry.column as u16,
                row: (geometry.track_top + geometry.max_thumb_start) as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            (0, true),
            "a late mouse-up after focus cancellation must not change the applied position"
        );

        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .unwrap();
        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag((geometry.track_top + geometry.max_thumb_start) as u16);
        app.update(AppEvent::TerminalSize {
            width: 81,
            height: 24,
        });
        assert!(app.scrollbar_preview_offset("ses_1").is_some());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            (0, true)
        );
    }

    #[test]
    fn scrollbar_tail_scroll_during_capture_is_not_undone_by_release() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .expect("overflowing transcript has a scrollbar");

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag((geometry.track_top + geometry.max_thumb_start / 2) as u16);
        app.update(AppEvent::Terminal(CrosstermEvent::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::End,
                crossterm::event::KeyModifiers::CONTROL,
            ),
        )));
        assert!(app.scrollbar_preview_offset("ses_1").is_some());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            (0, true)
        );

        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: geometry.column as u16,
                row: (geometry.track_top + geometry.max_thumb_start / 2) as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            (0, true),
            "late release after tail scroll must not restore the canceled drag"
        );
    }

    #[test]
    fn scrollbar_release_cancels_if_live_output_grew_before_viewport_measurement() {
        let mut app = crate::ui::testapp::live_turn(ThemeKind::Dark);
        {
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            for index in 0..24 {
                view.transcript
                    .push_block(TranscriptBlock::Assistant(AssistantBlock {
                        index,
                        loop_id: format!("history_{index}"),
                        request_index: 0,
                        model: "deep".to_owned(),
                        reasoning_level: Reasoning::High,
                        parts: vec![AssistantPart::Text(format!(
                            "history block {index} {}",
                            "content ".repeat(12)
                        ))],
                        tool_calls: vec![],
                        usage: Default::default(),
                        finish_reason: "stop".to_owned(),
                        terminal_error: None,
                    }));
            }
            view.transcript.invalidate();
        }
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .expect("overflowing transcript has a scrollbar");
        app.update(AppEvent::ConversationPrepared(prepared));

        let mouse = |kind, row| {
            AppEvent::Terminal(CrosstermEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column: geometry.column as u16,
                row: row as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            }))
        };
        app.update(mouse(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            geometry.thumb_top,
        ));
        app.update(mouse(
            crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            geometry.track_top,
        ));
        let committed = (
            app.active_view().unwrap().scroll.offset,
            app.active_view().unwrap().scroll.follow_tail,
        );

        let growth = "new live line\n".repeat(200);
        app.update(event(wire_event(json!({
            "type": "output_delta",
            "data": {
                "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                "request_index": 0,
                "channel": "text",
                "delta": growth,
                "meta": {"session_id": "ses_1", "dropped_before": 0}
            }
        }))));

        app.update(mouse(
            crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            geometry.track_top,
        ));
        assert_eq!(app.scrollbar_preview_offset("ses_1"), None);
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            committed,
            "a release against stale scrollbar geometry must cancel without committing"
        );
    }

    #[test]
    fn scrollbar_drag_cancels_when_switching_sessions() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .expect("overflowing transcript has a scrollbar");
        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag(geometry.track_top as u16);

        let info: crate::protocol::SessionInfo = serde_json::from_value(serde_json::json!({
            "session_id": "ses_2",
            "title": null,
            "profile": "coding",
            "workspace": "/project",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "created_at": "2026-01-02T03:04:05.006Z",
            "updated_at": "2026-01-02T03:04:05.006Z"
        }))
        .unwrap();
        let mut second = crate::state::session::SessionView::new(info);
        second.state = Some(
            serde_json::from_value(serde_json::json!({
                "session_id": "ses_2",
                "status": "idle",
                "active_loop": null,
                "block_reason": null
            }))
            .unwrap(),
        );
        app.sessions.known.insert("ses_2".to_owned(), second);

        app.update(AppEvent::OpenSession {
            session_id: "ses_2".to_owned(),
        });
        assert!(app.scrollbar_preview_offset("ses_1").is_none());
        assert_eq!(app.sessions.active.as_deref(), Some("ses_2"));

        app.update(AppEvent::OpenSession {
            session_id: "ses_1".to_owned(),
        });
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .expect("overflowing transcript has a scrollbar");
        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));

        let info: crate::protocol::SessionInfo = serde_json::from_value(serde_json::json!({
            "session_id": "ses_3",
            "title": null,
            "profile": "coding",
            "workspace": "/project",
            "model": "deep",
            "reasoning": "high",
            "loaded": false,
            "created_at": "2026-01-02T03:04:05.006Z",
            "updated_at": "2026-01-02T03:04:05.006Z"
        }))
        .unwrap();
        app.sessions.known.insert(
            "ses_3".to_owned(),
            crate::state::session::SessionView::new(info),
        );
        let commands = app.update(AppEvent::OpenSession {
            session_id: "ses_3".to_owned(),
        });
        assert!(app.scrollbar_preview_offset("ses_1").is_none());
        assert_eq!(app.sessions.active.as_deref(), Some("ses_1"));
        assert!(commands.iter().any(|command| {
            matches!(command, AppCommand::Rpc(request) if request.method == "session.open")
        }));
    }

    #[test]
    fn scrollbar_drag_survives_content_viewport_change_without_release_remap() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .expect("overflowing transcript has a scrollbar");
        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag(geometry.track_top as u16);
        let committed = (
            app.active_view().unwrap().scroll.offset,
            app.active_view().unwrap().scroll.follow_tail,
        );

        app.update(AppEvent::Viewport {
            total_lines: total + 1,
            visible_rows: visible,
        });
        assert!(app.scrollbar_preview_offset("ses_1").is_some());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            committed,
            "viewport growth must retain the last applied position"
        );
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: geometry.column as u16,
                row: geometry.track_top as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            committed,
            "mouse-up must not remap the pointer against changed geometry"
        );
    }

    #[test]
    fn scrollbar_thumb_uses_the_same_pending_offset_as_content() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let current = total.saturating_sub(visible);
        let geometry = crate::ui::scrollbar::geometry(screen.transcript, total, current)
            .expect("overflowing transcript has a scrollbar");

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag(geometry.track_top as u16);
        let pending = app
            .scrollbar_preview_offset("ses_1")
            .expect("pending offset");
        assert_eq!(
            crate::ui::scrollbar::thumb_top_for_scroll(geometry, pending),
            geometry.track_top
        );
        assert_eq!(app.active_view().unwrap().scroll.offset, 0);
    }

    #[test]
    fn idle_scrollbar_drag_does_not_arm_a_tick_but_still_renders_pending_content() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, total.saturating_sub(visible))
                .expect("overflowing transcript has a scrollbar");
        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag(geometry.track_top as u16);
        assert_eq!(
            app.next_tick(),
            None,
            "an idle held drag must not arm a timer"
        );

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(
            rows.iter().any(|row| row.contains("line 00")),
            "pending drag content still renders without a tick timer"
        );
    }

    #[test]
    fn scrollbar_wheel_input_preserves_capture_and_release_does_not_undo_it() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let current = total.saturating_sub(visible);
        let geometry = crate::ui::scrollbar::geometry(screen.transcript, total, current)
            .expect("overflowing transcript has a scrollbar");

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update(AppEvent::Viewport {
            total_lines: total,
            visible_rows: visible,
        });
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: geometry.column as u16,
                row: geometry.thumb_top as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert!(
            app.scrollbar_preview_offset("ses_1").is_some(),
            "Pi retains scrollbar capture during wheel input"
        );
        assert_eq!(app.active_view().unwrap().scroll.offset, current - 1);
        app.finish_scrollbar_drag(geometry.track_top as u16);
        assert_eq!(app.active_view().unwrap().scroll.offset, current - 1);
    }

    #[test]
    fn rail14_link_and_overlay_clicks_never_fold_while_plain_click_toggles() {
        use crossterm::event::MouseEvent;

        fn mouse(kind: MouseEventKind, column: u16, row: u16) -> CrosstermEvent {
            CrosstermEvent::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        }

        let mut app =
            crate::ui::testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
        app.terminal_size = (100, 30);
        let assistant = json!({
            "index": 1,
            "item": {
                "type": "assistant",
                "data": {
                    "loop_id": "loop_1",
                    "request_index": 0,
                    "model": "deep",
                    "reasoning": "high",
                    "content": [
                        {"type": "text", "data": "See [docs](https://example.com/doc) then read."},
                        {"type": "tool_call", "data": {"tool_call_id": "call-1", "name": "read", "arguments": {}, "call_index": 0}}
                    ],
                    "usage": {},
                    "finish_reason": "tool_calls"
                }
            }
        });
        let holes = take_requests(app.clear_transcript());
        respond(
            &mut app,
            &holes[0],
            read_page_json(
                vec![
                    user_item(0, "loop_1", "run the tools"),
                    assistant,
                    tool_result_item(2, "loop_1", "call-1", "read", "success", "one two three"),
                ],
                None,
                3,
            ),
        );

        let width = app.terminal_content_width();
        let prepared = crate::ui::transcript::prepare_conversation(&app, width);
        let theme = crate::theme::Theme::for_kind(ThemeKind::Dark);
        let screen = crate::ui::layout::screen_layout(
            &app,
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: app.terminal_size.0,
                height: app.terminal_size.1,
            },
        );
        let content_x = screen.content.x as usize;

        let mut link_cell = None;
        for (row, line) in prepared.lines().iter().enumerate() {
            let mut cursor = 0usize;
            for span in &line.spans {
                let start = cursor;
                cursor += unicode_width::UnicodeWidthStr::width(span.content.as_ref());
                if span.style.fg == Some(theme.md_link) && span.content.as_ref().contains("docs") {
                    link_cell = Some((row, start));
                }
            }
        }
        let (link_row, link_col) = link_cell.expect("markdown link cell rendered");
        let link_col_terminal = (content_x + link_col) as u16;
        assert!(
            app.pressed_cell_is_link(link_col_terminal, link_row as u16),
            "link cell must be detected as a press-on-link"
        );

        let tool_section = prepared
            .sections
            .iter()
            .find(|section| section.id.kind == crate::state::view::SectionKind::Tool)
            .expect("tool section");
        let tool_row = tool_section.rows.start + 1;
        let tool_col_terminal = (content_x + tool_section.content_columns.start) as u16;
        assert!(!app.pressed_cell_is_link(tool_col_terminal, tool_row as u16));

        // Plain single click on a collapsible tool card toggles its fold.
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            tool_col_terminal,
            tool_row as u16,
        )));
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            tool_col_terminal,
            tool_row as u16,
        )));
        let folds_after_plain = app.sessions.known["ses_1"].tool_folds.len();
        assert_eq!(folds_after_plain, 1, "plain click must record a tool fold");

        // Clicking the link cell records the pressedUrl guard and leaves the
        // fold registry untouched (RAIL-14 link click cannot fold).
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            link_col_terminal,
            link_row as u16,
        )));
        assert!(app.mouse_pressed_on_link, "link press must set the guard");
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            link_col_terminal,
            link_row as u16,
        )));
        assert_eq!(
            app.sessions.known["ses_1"].tool_folds.len(),
            folds_after_plain,
            "link click must not fold a section"
        );

        // Double-click selects a word and copies it instead of folding. The
        // click target is a non-collapsible assistant row so no fold toggle can
        // interleave (RAIL-14 selectionInitialRange guard).
        let assistant_row = prepared
            .copy_ranges
            .iter()
            .find(|range| range.text.contains("then read"))
            .map(|range| (range.row, range.columns.start + 1))
            .expect("assistant text row");
        let (assistant_row, assistant_col) = assistant_row;
        let assistant_col_terminal = (content_x + assistant_col) as u16;
        let before_word = app.sessions.known["ses_1"].tool_folds.len();
        // First click: plain selection, no copy.
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            assistant_col_terminal,
            assistant_row as u16,
        )));
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            assistant_col_terminal,
            assistant_row as u16,
        )));
        // Second click at the same cell upgrades to a word selection that
        // copies and never folds.
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            assistant_col_terminal,
            assistant_row as u16,
        )));
        let commands = app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            assistant_col_terminal,
            assistant_row as u16,
        )));
        assert!(matches!(
            commands.as_slice(),
            [AppCommand::CopySelection(_)]
        ));
        assert_eq!(
            app.sessions.known["ses_1"].tool_folds.len(),
            before_word,
            "word-selection clicks must never fold"
        );

        // While an overlay (model selector) is open, a click on the same tool
        // card row is routed to the selector and cannot fold either.
        app.update(AppEvent::OpenModelSelector);
        assert!(app.selector_state().is_some());
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            tool_col_terminal,
            tool_row as u16,
        )));
        app.update(AppEvent::Terminal(mouse(
            MouseEventKind::Up(crossterm::event::MouseButton::Left),
            tool_col_terminal,
            tool_row as u16,
        )));
        assert_eq!(
            app.sessions.known["ses_1"].tool_folds.len(),
            before_word,
            "overlay-open click must not fold"
        );
    }

    #[test]
    fn rail14_link_geometry_covers_code_and_bold_links_but_not_plain_text() {
        use crossterm::event::MouseEvent;

        fn mouse_down(column: u16, row: u16) -> CrosstermEvent {
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column,
                row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            })
        }

        let mut app =
            crate::ui::testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
        app.terminal_size = (120, 30);
        let assistant = json!({
            "index": 1,
            "item": {
                "type": "assistant",
                "data": {
                    "loop_id": "loop_1",
                    "request_index": 0,
                    "model": "deep",
                    "reasoning": "high",
                    "content": [
                        {"type": "text", "data": "See [`inline`](https://e.com/code) and **bold [link](https://e.com/b)** and plain."},
                        {"type": "tool_call", "data": {"tool_call_id": "call-1", "name": "read", "arguments": {}, "call_index": 0}}
                    ],
                    "usage": {},
                    "finish_reason": "tool_calls"
                }
            }
        });
        let holes = take_requests(app.clear_transcript());
        respond(
            &mut app,
            &holes[0],
            read_page_json(
                vec![
                    user_item(0, "loop_1", "run"),
                    assistant,
                    tool_result_item(2, "loop_1", "call-1", "read", "success", "one two three"),
                ],
                None,
                3,
            ),
        );

        let width = app.terminal_content_width();
        let prepared = crate::ui::transcript::prepare_conversation(&app, width);
        let screen = crate::ui::layout::screen_layout(
            &app,
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: app.terminal_size.0,
                height: app.terminal_size.1,
            },
        );
        let content_x = screen.content.x as usize;
        let text_row = prepared
            .copy_ranges
            .iter()
            .find(|range| range.text.contains("and plain"))
            .map(|range| range.row)
            .expect("assistant text row");

        fn cell_of(line: &ratatui::text::Line<'static>, needle: &str) -> usize {
            line.spans
                .iter()
                .scan(0usize, |cursor, span| {
                    let start = *cursor;
                    *cursor += unicode_width::UnicodeWidthStr::width(span.content.as_ref());
                    Some((start, span.content.as_ref()))
                })
                .find(|(_, text)| text.contains(needle))
                .map(|(start, _)| start)
                .expect("span containing the needle")
        }

        // Inline-code link cells and bold-link cells are all links even though
        // `inline` is painted in the inline-code color, not md_link.
        for needle in ["inline", "link", "e.com/code"] {
            let cell = cell_of(&prepared.lines()[text_row], needle);
            assert!(
                app.pressed_cell_is_link((content_x + cell) as u16, text_row as u16),
                "link cell for {needle:?} must be detected"
            );
        }

        // Plain prose is not a link cell, so a press there cannot fold and the
        // guard stays off (no same-colored false positive).
        let plain_cell = cell_of(&prepared.lines()[text_row], "plain");
        assert!(
            !app.pressed_cell_is_link((content_x + plain_cell) as u16, text_row as u16),
            "plain prose must not be treated as a link"
        );
        let folds_before = app.sessions.known["ses_1"].tool_folds.len();
        app.update(AppEvent::Terminal(mouse_down(
            (content_x + plain_cell) as u16,
            text_row as u16,
        )));
        assert!(
            !app.mouse_pressed_on_link,
            "plain text must not arm the link guard"
        );
        assert_eq!(
            app.sessions.known["ses_1"].tool_folds.len(),
            folds_before,
            "plain text press must not fold"
        );

        // A link press does arm the guard (RAIL-14 flow through real ranges).
        let link_cell = cell_of(&prepared.lines()[text_row], "e.com/b");
        app.update(AppEvent::Terminal(mouse_down(
            (content_x + link_cell) as u16,
            text_row as u16,
        )));
        assert!(app.mouse_pressed_on_link, "link press must arm the guard");
    }

    #[test]
    fn paragraph_selection_stops_at_rendered_blank_boundaries() {
        let app = crate::ui::testapp::chat(ThemeKind::Dark);
        let prepared = crate::ui::transcript::prepare_conversation(&app, 79);
        let row = prepared
            .copy_ranges
            .iter()
            .find(|range| range.text.contains("A paragraph"))
            .expect("paragraph row");
        let section = prepared
            .sections
            .iter()
            .find(|section| section.rows.contains(&row.row))
            .expect("paragraph section");
        let selection = app.paragraph_selection(SelectionPoint {
            row: row.row,
            column: row.columns.start,
            section_id: Some(section.id.clone()),
            section_row: row.row - section.rows.start,
        });
        let (start, end) = selection.ordered_points();
        assert_eq!(start.row, row.row);
        assert_eq!(end.row, row.row);
        let copied = crate::ui::transcript::selection_text(&prepared, &selection);
        assert!(copied.contains("A paragraph with bold, italic, and code."));
        assert!(!copied.contains("first item"));
    }
}

#[cfg(test)]
mod steer_receipt_tests {
    use super::*;
    use crate::ui::testapp;
    use serde_json::{Value, json};

    fn take_requests(commands: Vec<AppCommand>) -> Vec<OutgoingRequest> {
        commands
            .into_iter()
            .filter_map(|command| match command {
                AppCommand::Rpc(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    fn respond(app: &mut App, request: &OutgoingRequest, result: Value) -> Vec<AppCommand> {
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
        ))))
    }

    /// Encodes a Runtime item envelope as a one-chunk Protocol v1 read page.
    fn read_page_json(items: Vec<Value>, next_cursor: Option<usize>, total: usize) -> Value {
        let chunks: Vec<Value> = items
            .iter()
            .map(|envelope| {
                let index = envelope.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let item = envelope.get("item").cloned().unwrap_or(Value::Null);
                let data = serde_json::to_string(&json!({"item": item})).unwrap();
                let total_bytes = data.len();
                json!({
                    "index": index,
                    "offset": 0,
                    "total_bytes": total_bytes,
                    "encoding": "utf8_json",
                    "data": data,
                    "complete": true
                })
            })
            .collect();
        let mut page = json!({
            "session": super::tests::session_info("ses_1"),
            "items": chunks,
            "total": total,
            "records": [],
            "records_truncated": false,
            "history_revision": "0000000000000000000000000000000000000000000000000000000000000000",
            "captured_end": total as u64,
            "trailing_incomplete": false
        });
        if let Some(item) = next_cursor {
            page["next_cursor"] = json!({"item": item, "offset": 0});
        }
        page
    }

    fn user_item(index: usize, loop_id: &str, text: &str) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "user",
                "data": {
                    "loop_id": loop_id,
                    "kind": "prompt",
                    "input": {"text": text}
                }
            }
        })
    }

    fn assistant_item(index: usize, loop_id: &str, text: &str) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "assistant",
                "data": {
                    "loop_id": loop_id,
                    "request_index": 0,
                    "model": "deep",
                    "reasoning": "high",
                    "content": [{"type": "text", "data": text}],
                    "usage": {},
                    "finish_reason": "stop"
                }
            }
        })
    }

    /// Submits one steer; returns the issued turn.steer request when the
    /// FIFO advance sent it (first in-flight), or None when it was admitted
    /// locally and held unsent behind an in-flight steer.
    fn submit_steer(app: &mut App, text: &str) -> Option<OutgoingRequest> {
        let commands = app.update(AppEvent::SteerTurn {
            session_id: "ses_1".to_owned(),
            text: text.to_owned(),
        });
        take_requests(commands)
            .into_iter()
            .find(|request| request.method == "turn.steer")
    }

    fn steer_progress_event(
        applied_count: u64,
        loop_id: &str,
        session_id: &str,
        request_index: u32,
    ) -> AppEvent {
        AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(
                serde_json::from_value(serde_json::json!({
                    "type": "steer_progress",
                    "data": {
                        "turn": {"session_id": session_id, "loop_id": loop_id},
                        "request_index": request_index,
                        "applied_count": applied_count,
                        "meta": {"session_id": session_id, "dropped_before": 0}
                    }
                }))
                .unwrap(),
            ),
        )))
    }

    // Receipt pairing is by ACK identity, never queue position: a receipt
    // alone never applies a Sending entry, and an ACK index beyond the
    // observed count is held until a covering receipt arrives.
    #[test]
    fn receipt_without_matching_index_holds_until_a_correct_receipt() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        let steer = submit_steer(&mut app, "first").expect("first steer in flight");
        let _second = submit_steer(&mut app, "second queued");
        // Receipt 1 races ahead of the ACK: history counted 1 steer but the
        // entry is still Sending (no identity) - nothing is applied.
        app.update(steer_progress_event(1, "loop_live", "ses_1", 5));
        let (applied, pending) = {
            let view = &app.sessions.known["ses_1"];
            (
                view.applied_steers.len(),
                view.live.as_ref().unwrap().pending_steers.len(),
            )
        };
        assert_eq!(applied, 0, "receipt alone never applies a Sending entry");
        assert_eq!(pending, 1, "Sending entry retained, blocking the next RPC");

        // ACK index 2 while the observed count is 1: not covered -> held.
        respond(
            &mut app,
            &steer,
            json!({"ok": true, "accepted_at": "T0", "steer_index": 2}),
        );
        let (applied, pending, queue) = {
            let view = &app.sessions.known["ses_1"];
            (
                view.applied_steers.len(),
                view.live.as_ref().unwrap().pending_steers.len(),
                view.steer_queue.len(),
            )
        };
        assert_eq!(applied, 0, "index 2 is not covered by count 1");
        assert_eq!(
            pending, 1,
            "still in flight; the next unsent must not be sent"
        );
        assert_eq!(queue, 1, "second entry stays locally unsent");

        // A covering receipt (count 2, request 7) applies entry "first" with
        // the FIRST-observation request_index and the ACK accepted_at, then
        // the FIFO advance issues the next unsent steer.
        app.update(steer_progress_event(2, "loop_live", "ses_1", 7));
        let view = &app.sessions.known["ses_1"];
        assert_eq!(view.applied_steers.len(), 1);
        assert_eq!(view.applied_steers[0].text, "first");
        assert_eq!(view.applied_steers[0].request_index, 7);
        assert_eq!(view.applied_steers[0].accepted_at.as_deref(), Some("T0"));
        let live_steers = view.live.as_ref().unwrap().pending_steers.clone();
        assert_eq!(live_steers.len(), 1, "the next unsent flowed to the RPC");
        assert_eq!(live_steers[0].text, "second queued");
        assert_eq!(view.steer_queue.len(), 0);
    }

    #[test]
    fn valid_ack_index_applies_with_original_request_and_then_sends_next() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        let steer = submit_steer(&mut app, "first").expect("first steer in flight");
        let _second = submit_steer(&mut app, "second queued");

        respond(
            &mut app,
            &steer,
            json!({"ok": true, "accepted_at": "T1", "steer_index": 1}),
        );
        // No receipt yet: accepted but unconfirmed, the next steer is blocked.
        let view = &app.sessions.known["ses_1"];
        assert!(view.applied_steers.is_empty());
        assert_eq!(view.steer_queue.len(), 1);

        // The receipt covering index 1 (observed at request 3) applies the
        // entry with accepted_at from the ACK and the original request index.
        app.update(steer_progress_event(1, "loop_live", "ses_1", 3));
        let view = &app.sessions.known["ses_1"];
        assert_eq!(view.applied_steers.len(), 1);
        assert_eq!(view.applied_steers[0].text, "first");
        assert_eq!(view.applied_steers[0].request_index, 3);
        assert_eq!(view.applied_steers[0].accepted_at.as_deref(), Some("T1"));
        assert_eq!(view.steer_queue.len(), 0, "B sent after A's receipt");
        assert_eq!(
            view.live.as_ref().unwrap().pending_steers.len(),
            1,
            "B in flight"
        );
    }

    #[test]
    fn stale_steer_progress_after_loop_switch_is_ignored() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        let steer = submit_steer(&mut app, "stale").expect("stale steer in flight");
        // A receipt scoped to a DIFFERENT (old) loop must never apply or cache.
        app.update(steer_progress_event(1, "loop_other", "ses_1", 0));
        let view = &app.sessions.known["ses_1"];
        assert!(view.applied_steers.is_empty());
        assert_eq!(view.live.as_ref().unwrap().pending_steers.len(), 1);

        // Current-loop duplicate progress (same count) after a valid apply is
        // a no-op: nothing is re-applied and no extra resend happens.
        app.update(steer_progress_event(1, "loop_live", "ses_1", 0));
        respond(&mut app, &steer, json!({"ok": true, "steer_index": 1}));
        let view = &app.sessions.known["ses_1"];
        assert_eq!(view.applied_steers.len(), 1);
        app.update(steer_progress_event(1, "loop_live", "ses_1", 0));
        let view = &app.sessions.known["ses_1"];
        assert_eq!(view.applied_steers.len(), 1, "same count never re-applies");
    }

    #[test]
    fn mid_loop_history_without_steer_keeps_queued_and_terminal_marks_not_recorded() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        let steer = submit_steer(&mut app, "steer instruction").expect("steer in flight");
        respond(&mut app, &steer, json!({"ok": true, "steer_index": 1}));
        assert_eq!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .unwrap()
                .pending_steers[0]
                .state,
            PendingSteerState::Queued
        );

        // MID-LOOP history page (live still present, no last_result): the
        // accepted steer missing from the page must stay Queued (never a
        // mid-loop NotRecorded downgrade).
        let read = ReadRequest {
            cursor: crate::protocol::ReadCursor::start(),
            pin: None,
            window_start: 0,
            replacement: true,
            reconcile: true,
            probe: false,
            gap_revision: 0,
        };
        let mid_loop = RpcResponse {
            id: RequestId(1),
            result: Some(read_page_json(
                vec![user_item(0, "loop_live", "stream me")],
                None,
                1,
            )),
            error: None,
        };
        app.on_history_response(&"ses_1".into(), &read, &mid_loop);
        assert_eq!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .unwrap()
                .pending_steers[0]
                .state,
            PendingSteerState::Queued,
            "mid-loop empty history must NOT mark NotRecorded"
        );

        // TERMINAL history (loop finished, last_result present, steering item
        // absent) legitimately marks NotRecorded.
        if let Some(live) = app
            .sessions
            .known
            .get_mut("ses_1")
            .and_then(|v| v.live.as_mut())
        {
            live.last_result = Some(crate::protocol::TurnResultViewWire {
                turn: TurnRef {
                    session_id: "ses_1".into(),
                    loop_id: "loop_live".into(),
                },
                outcome: crate::protocol::LoopOutcomeWire::Completed,
                usage: Some(crate::protocol::UsageWire::default()),
                requests: Some(1),
                tool_rounds: Some(0),
                final_config_revision: Some(0),
                persistence: Some(crate::protocol::TurnPersistenceWire::Persisted),
                accepted_at: None,
                completed_at: None,
            });
        }
        let terminal = RpcResponse {
            id: RequestId(1),
            result: Some(read_page_json(
                vec![
                    user_item(0, "loop_live", "stream me"),
                    assistant_item(1, "loop_live", "answer"),
                ],
                None,
                2,
            )),
            error: None,
        };
        app.on_history_response(&"ses_1".into(), &read, &terminal);
        let view = &app.sessions.known["ses_1"];
        let terminal_state = view
            .live
            .as_ref()
            .and_then(|live| live.pending_steers.first())
            .map(|steer| steer.state.clone());
        assert!(
            terminal_state.is_none() || terminal_state != Some(PendingSteerState::Queued),
            "terminal history resolves the accepted steer ({terminal_state:?})"
        );
    }

    #[test]
    fn dropped_receipt_recovered_via_presentation_steer_progress() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        let steer = submit_steer(&mut app, "recovered").expect("steer in flight");
        respond(&mut app, &steer, json!({"ok": true, "steer_index": 1}));
        let view = &app.sessions.known["ses_1"];
        assert_eq!(
            view.live.as_ref().unwrap().pending_steers[0].state,
            PendingSteerState::Queued
        );

        // The steer_progress event was DROPPED in flight; the next
        // session.presentation snapshot carries the receipt and reconciles it.
        let response = RpcResponse {
            id: RequestId(1),
            result: Some(serde_json::json!({
                "session_id": "ses_1",
                "model_label": "deep",
                "git_branch": null,
                "context": {"tokens": null, "window": null, "percent": null, "kind": "unknown"},
                "cost_usd": null,
                "using_subscription": null,
                "last_loop": null,
                "steer_progress": {
                    "loop_id": "loop_live",
                    "request_index": 0,
                    "applied_count": 1
                }
            })),
            error: None,
        };
        app.on_session_presentation_response(&"ses_1".into(), &response);
        let view = &app.sessions.known["ses_1"];
        assert_eq!(
            view.applied_steers.len(),
            1,
            "presentation receipt recovered the dropped event"
        );
    }
}

#[cfg(test)]
mod steer_queue_ui_tests {
    use super::*;
    use crate::ui::testapp;

    fn take_requests(commands: Vec<AppCommand>) -> Vec<OutgoingRequest> {
        commands
            .into_iter()
            .filter_map(|command| match command {
                AppCommand::Rpc(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    fn submit_steer(app: &mut App, text: &str) -> Option<OutgoingRequest> {
        let commands = app.update(AppEvent::SteerTurn {
            session_id: "ses_1".to_owned(),
            text: text.to_owned(),
        });
        take_requests(commands)
            .into_iter()
            .find(|request| request.method == "turn.steer")
    }

    #[test]
    fn alt_up_withdraws_next_unsent_into_empty_editor_only() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        // First steer flows to the RPC (Sending); second stays locally unsent.
        let _first = submit_steer(&mut app, "first");
        let _second = submit_steer(&mut app, "second queued");
        assert_eq!(app.sessions.known["ses_1"].steer_queue.len(), 1);

        // With nonempty editor Alt+Up stays a history navigation (no change).
        app.composer.set_text("draft");
        app.composer.history_prev();
        assert_eq!(app.composer.content(), "draft");

        // Empty editor: withdraw the NEXT unsent item.
        app.composer.clear();
        assert!(app.retrieve_next_queued_steer());
        assert_eq!(app.composer.content(), "second queued");
        assert_eq!(app.sessions.known["ses_1"].steer_queue.len(), 0);
        // A second call with an empty (now filled) editor cannot withdraw.
        assert!(!app.retrieve_next_queued_steer());
        assert_eq!(app.composer.content(), "second queued");
    }

    #[test]
    fn queue_layout_sits_above_status_with_one_blank_gap_and_hides_in_modal() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        let _first = submit_steer(&mut app, "first");
        for index in 0..5 {
            submit_steer(&mut app, &format!("s{index}"));
        }
        // Composer dock: queue ABOVE status, one blank gap between.
        let layout =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let queue = layout.queue.expect("queue rect while composing");
        let status = layout.status.expect("status row while busy");
        assert!(
            queue.bottom() <= status.y,
            "queue.bottom {} < status.y {}",
            queue.bottom(),
            status.y
        );
        assert_eq!(status.y - queue.bottom(), 1, "exactly one blank gap row");

        // A modal owns the dock: the queue is hidden and never squeezes it.
        app.update(AppEvent::OpenSessionSelector);
        let layout =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 60, 16));
        assert_eq!(layout.queue, None, "queue hidden in selector modal");
        app.update(AppEvent::CancelDock);

        // 60x16 with queue + notice + composer: footer is one row, editor
        // visible, and the shared transcript keeps at least one row.
        app.update(AppEvent::TerminalSize {
            width: 60,
            height: 16,
        });
        let layout =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 60, 16));
        assert_eq!(
            layout.footer.height, 1,
            "footer collapses to one row at 60x16"
        );
        assert!(layout.panel.height >= 1, "editor surface visible");
        assert!(layout.transcript.height >= 1, "shared viewport visible");
        // 6 entries with a withdrawable unsent item: 3 + overflow + hint.
        assert_eq!(layout.queue.expect("queue present").height, 5);
        // The explicit gap row keeps queue.bottom < status.y even at 60x16.
        assert_eq!(
            layout.status.expect("status").y - layout.queue.unwrap().bottom(),
            1
        );
    }

    #[test]
    fn dock_queue_caps_display_and_footer_counts_unsent_and_inflight() {
        let mut app = testapp::live_turn(crate::theme::ThemeKind::Dark);
        // One flows to the RPC, five more admitted locally.
        let _first = submit_steer(&mut app, "s1");
        for index in 0..5 {
            submit_steer(&mut app, &format!("s{index}"));
        }
        let view = &app.sessions.known["ses_1"];
        assert_eq!(view.steer_queue.len(), 5);
        assert_eq!(view.live.as_ref().unwrap().pending_steers.len(), 1);

        // Footer: queued = unsent + in-flight = 6, never applied.
        let view = crate::ui::footer::footer_view(&app);
        assert!(
            view.left.contains("queued 6"),
            "footer queued count, got: {}",
            view.left
        );

        // 6 entries with a withdrawable unsent item and an empty composer:
        // 3 content + 1 overflow + 1 Alt+Up hint (the blank gap is a separate
        // explicit row between queue and status).
        assert_eq!(crate::ui::layout::steer_queue_rows(&app), 5);
        assert_eq!(crate::ui::steer_queue::queue_entries(&app).len(), 6);
    }
}
