//! The app state machine. Every state change happens inside
//! `App::update(AppEvent)`; RPC tasks and the command executor only send
//! `AppEvent`s or run `AppCommand`s (development spec 9.1). Request ids are
//! allocated inside `update` and registered in `pending_requests` before any
//! command leaves it, so a response can never beat its registration.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::Event as CrosstermEvent;

use crate::command::{AppCommand, CommandIssue, LocalCommand, is_slash_command, parse_command};
use crate::event::{AppEvent, RpcEvent};
use crate::keymap::{self, Action, EditorCursor};
use crate::protocol::{
    AgentEventWire, AssistantDisplayPartWire, DEFAULT_HISTORY_LIMIT, EventMetaWire,
    HistoryItemWire, HistoryPageWire, IncomingFrame, IndexedHistoryItemWire, METHOD_LIST_MODELS,
    METHOD_LIST_PROFILES, METHOD_LIST_SESSIONS, OutgoingRequest, OutputChannelWire, Reasoning,
    RequestId, RpcNotification, RpcResponse, RpcResponseError, SessionInfo, SessionStateWire,
    SessionStatusWire, ToolDisplayWire, ToolOutcomeWire, ToolProgressWire, TurnPersistenceWire,
    TurnRef, UserMessageKindWire, is_supported_agent_version,
};
use crate::rpc::RpcError;
use crate::state::catalog::CatalogState;
use crate::state::composer::{Composer, MAX_COMPOSER_BYTES};
use crate::state::selection::{
    Dock, NewSessionField, NewSessionState, SelectorKind, SelectorState, SessionConfirmChoice,
    SessionPanelAction, SessionPanelMode, SessionSelectorState, filtered_models, filtered_profiles,
    filtered_sessions, supported_reasoning,
};
use crate::state::session::{SessionId, SessionView, SessionsState};
use crate::state::tool::{LiveTool, ToolKey, ToolPresentationState, ToolStatus};
use crate::state::transcript::{
    AssistantBlock, AssistantPart, SummaryBlock, ToolBlock, TranscriptBlock, UserBlock,
};
use crate::state::turn::{
    AppliedSteer, LiveLoop, LivePart, LocalSubmissionId, PendingSteer, PendingSteerState,
    SteerQueueState, UnsavedLoop,
};
use crate::state::view::{
    ConversationSelection, FoldOverride, PreparedConversation, SelectionPoint,
};
use crate::theme::ThemeKind;

pub mod ui_actions;
pub use self::ui_actions::SlashCompletionState;
use self::ui_actions::{EditorSelection, SelectionDrag};

/// The agent's stderr ring size, App side (spec 10.8).
pub const MAX_AGENT_LOG_LINES: usize = 200;
/// Bound for the per-session local steer FIFO. Small by design; a full queue
/// pauses and keeps the composer message rather than silently dropping.
pub const MAX_STEER_QUEUE_LEN: usize = 8;

const MAX_NOTICES: usize = 32;

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
    pending_offset: usize,
    pending_row: u16,
    visible_rows: usize,
    marker: bool,
    geometry: crate::ui::scrollbar::ScrollbarGeometry,
}

/// Why a request was issued; `pending_requests` routes each response to the
/// matching handler regardless of arrival order (spec 10.9, 10.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    Ping,
    ListModels,
    ListProfiles,
    ListSessions,
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
    History {
        session_id: SessionId,
        offset: usize,
        limit: usize,
        gap_revision: Option<u64>,
    },
    SendTurn {
        session_id: SessionId,
        local_submission: LocalSubmissionId,
    },
    WaitTurn(TurnRef),
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
}

/// All app and UI state. Only `App::update` mutates it; render code reads
/// the public fields, tasks and executor never touch the app at all.
pub struct App {
    pub connection: ConnectionState,
    pub catalogs: CatalogState,
    pub sessions: SessionsState,
    pub notices: VecDeque<Notice>,
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
    pub frame_count: u64,
    pub composer: Composer,
    /// Local slash candidates derived from `command::SLASH_COMMAND_NAMES`.
    pub slash_completion: Option<SlashCompletionState>,
    /// Preferred visual column while moving vertically through wrapped editor
    /// rows, matching the native editor's temporary vertical-column state.
    composer_preferred_visual_col: Option<usize>,
    /// The dock panel below the transcript (spec 24.1).
    pub dock: Dock,
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
    selection_drag: Option<SelectionDrag>,
    editor_selection: Option<EditorSelection>,
    /// The single prepared conversation snapshot shared by measurement,
    /// rendering, hit testing, selection, and copying.
    prepared_conversation: Option<PreparedConversation>,
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
    next_request_id: RequestId,
    next_state_query: u64,
    next_submission: u64,
    next_draft_id: u64,
    next_steer_id: u64,
    bootstrap: BootstrapProgress,
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
    Page { offset: usize },
    Reconcile { offset: usize },
    LoopNotContained(String),
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionActionSafety {
    Safe,
    Unknown,
    Busy,
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
            },
            sessions: SessionsState::default(),
            notices: VecDeque::new(),
            dirty: false,
            agent_logs: VecDeque::new(),
            child_exit_status: None,
            theme: ThemeKind::Dark,
            reasoning_visible: true,
            frame_count: 0,
            composer: Composer::default(),
            slash_completion: None,
            composer_preferred_visual_col: None,
            dock: Dock::Composer,
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
            selection_drag: None,
            editor_selection: None,
            prepared_conversation: None,
            selection: None,
            selection_copied_until: None,
            spinner_next_due: None,
            notice_ttl: NOTICE_TTL,
            draft: None,
            shutdown_sent: false,
            shutdown_deadline: None,
            shutdown_child_exited: false,
            open_new_session_on_ready: false,
            now: SystemTime::now,
            pending_requests: HashMap::new(),
            next_request_id: RequestId(0),
            next_state_query: 0,
            next_submission: 0,
            next_draft_id: 0,
            next_steer_id: 0,
            bootstrap: BootstrapProgress::default(),
            blocked_notice: false,
            monotonic_now: Arc::new(Instant::now),
        }
    }

    /// Same as [`App::new`], with CLI preferences seeded into the catalog's
    /// next-session seats (spec 6.1).
    pub fn with_cli_prefs(default_workspace: PathBuf, prefs: CliPrefs) -> Self {
        let mut app = Self::new(default_workspace);
        app.catalogs.next_profile = prefs.profile;
        app.catalogs.next_model = prefs.model;
        app.catalogs.next_reasoning = prefs.reasoning;
        app.open_new_session_on_ready = prefs.open_new_session_on_ready;
        app
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
                    .is_some_and(|result| result.persistence == TurnPersistenceWire::Failed)
        });
        let unconfirmed = self.sessions.known.values().any(|view| {
            view.result_unconfirmed
                || view.live.as_ref().is_some_and(|live| {
                    !live
                        .last_result
                        .as_ref()
                        .is_some_and(|result| live.reference.as_ref() == Some(&result.turn))
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
        if matches!(&event, AppEvent::Rendered) {
            self.dirty = false;
            return Vec::new();
        }
        if matches!(&event, AppEvent::Terminal(CrosstermEvent::Mouse(mouse))
            if mouse.kind == crossterm::event::MouseEventKind::Moved)
            || matches!(&event, AppEvent::Viewport { total_lines, visible_rows }
                if (*total_lines, *visible_rows) == self.viewport)
        {
            return Vec::new();
        }
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
            self.prepared_conversation = None;
        }
        // Hit tests in one mouse event share the same immutable preparation.
        if matches!(&event, AppEvent::Terminal(CrosstermEvent::Mouse(_))) {
            let width = self.terminal_content_width();
            if self.prepared_conversation(width).is_none() {
                let prepared = crate::ui::transcript::prepare_conversation(self, width);
                self.install_conversation(prepared);
            }
        }
        self.dirty = true;
        let mut commands = match event {
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
            AppEvent::Rpc(event) => self.on_rpc_event(event),
            AppEvent::RpcChannelEnded => self.on_rpc_channel_ended(),
            AppEvent::RpcSendFailed { id, error } => self.on_send_failed(id, error),
            AppEvent::ShutdownRequested => self.request_shutdown(),
            AppEvent::Tick => {
                let now = self.instant_now();
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
                Vec::new()
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
                if self.scrollbar_drag.is_some() {
                    ui_actions::cancel_scrollbar_drag(self);
                }
                self.viewport = (total_lines, visible_rows);
                self.clamp_transcript_scroll();
                Vec::new()
            }
            AppEvent::TerminalSize { width, height } => {
                if self.terminal_size != (width, height) && self.scrollbar_drag.is_some() {
                    ui_actions::cancel_scrollbar_drag(self);
                }
                self.terminal_size = (width, height);
                Vec::new()
            }
            AppEvent::ClipboardResult { success, error } => {
                if success {
                    self.selection_copied_until = Some(
                        self.instant_now()
                            .checked_add(Duration::from_millis(1_800))
                            .expect("copy feedback deadline is representable"),
                    );
                } else {
                    self.notice(
                        NoticeLevel::Warning,
                        error.unwrap_or_else(|| "copy failed".to_owned()),
                    );
                }
                Vec::new()
            }
            AppEvent::ConversationPrepared(prepared) => {
                self.install_conversation(prepared);
                Vec::new()
            }
        };
        // Central FIFO queue advance: after any event, at most one steer RPC
        // per session (or a fresh-turn fallback once a finished loop settles).
        let advance = self.advance_steer_queues();
        commands.extend(advance);
        self.sync_spinner_deadline();
        commands
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
                std::borrow::Cow::Owned(crate::ui::transcript::prepare_conversation(self, width))
            })
    }

    pub fn scrollbar_preview_offset(&self, session_id: &str) -> Option<usize> {
        self.scrollbar_drag
            .as_ref()
            .filter(|drag| drag.session_id == session_id)
            .map(|drag| drag.pending_offset)
    }

    pub(crate) fn scrollbar_drag_preview(&self, session_id: &str) -> Option<(usize, usize, bool)> {
        self.scrollbar_drag
            .as_ref()
            .filter(|drag| drag.session_id == session_id)
            .map(|drag| (drag.pending_offset, drag.visible_rows, drag.marker))
    }

    pub(crate) fn install_conversation(&mut self, prepared: PreparedConversation) {
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
        self.rebase_selection(&prepared);
        self.prepared_conversation = Some(prepared);
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

    fn upsert_session_list(&mut self, session: SessionInfo) {
        let mut session = session;
        if self.sessions.deleted.contains(&session.session_id)
            || self.sessions.pending_deletes.contains(&session.session_id)
        {
            return;
        }
        if self.sessions.closed.contains(&session.session_id) {
            session.loaded = false;
        }
        if let Some(title) = self.sessions.title_overrides.get(&session.session_id) {
            session.title = title.clone();
        }
        if let Some(existing) = self
            .sessions
            .list
            .iter_mut()
            .find(|existing| existing.session_id == session.session_id)
        {
            *existing = session;
        } else {
            self.sessions.list.push(session);
        }
        self.sessions
            .list
            .sort_by(|left, right| left.session_id.cmp(&right.session_id));
        self.reconcile_session_selection(true);
    }

    fn session_is_visible(&self, session_id: &str) -> bool {
        !self.sessions.pending_deletes.contains(session_id)
            && !self.sessions.deleted.contains(session_id)
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

    fn filtered_session_items(&self, query: &str) -> Vec<&SessionInfo> {
        filtered_sessions(&self.sessions.list, query)
            .into_iter()
            .filter(|session| self.session_is_visible(&session.session_id))
            .collect()
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
        self.pending_requests.values().any(|request| {
            matches!(
                request,
                RequestKind::OpenSession { .. }
                    | RequestKind::RefreshSessions { .. }
                    | RequestKind::RenameSession { .. }
                    | RequestKind::CloseSession { .. }
                    | RequestKind::CloseVerifyState { .. }
                    | RequestKind::DeleteSession { .. }
            )
        })
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

    /// Opens `kind` and pre-selects the draft's current value. Opening
    /// model/reasoning/profile guarantees a new-session draft exists so
    /// the selection can never leak into the current session (spec 26.4).
    fn open_selector(&mut self, kind: SelectorKind) -> Vec<AppCommand> {
        if !self.guard_ready() {
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
            if self.sessions.active.is_none() || kind == SelectorKind::Profile {
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
            Dock::Help | Dock::Logs => Target::Composer,
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
        }
        let target = match &self.dock {
            Dock::Composer => Target::Composer,
            Dock::SessionSelector(_) => Target::SessionSelector,
            Dock::NewSession(_) => Target::NewSession,
            Dock::ModelSelector(_) | Dock::ReasoningSelector(_) | Dock::ProfileSelector(_) => {
                Target::Form
            }
            Dock::Help | Dock::Logs => Target::Panel,
        };
        match target {
            Target::Composer => {}
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

    fn session_action_safety(&self, session_id: &SessionId) -> SessionActionSafety {
        let listed = self
            .sessions
            .list
            .iter()
            .find(|session| &session.session_id == session_id);
        let Some(view) = self.sessions.known.get(session_id) else {
            return if self.sessions.closed.contains(session_id) {
                SessionActionSafety::Safe
            } else if listed.is_some_and(|session| session.loaded) {
                SessionActionSafety::Unknown
            } else {
                SessionActionSafety::Safe
            };
        };

        if view.event_gap {
            return SessionActionSafety::Busy;
        }
        if view.close_verification_unknown || view.latest_state_query.is_some() {
            return SessionActionSafety::Unknown;
        }

        if view.closing
            || view.live.is_some()
            || view.unsaved_loop.is_some()
            || view.result_unconfirmed
            || view.is_blocked()
            || view
                .state
                .as_ref()
                .is_some_and(|state| state.status != SessionStatusWire::Idle)
        {
            return SessionActionSafety::Busy;
        }

        let loaded = !self.sessions.closed.contains(session_id)
            && (view.info.loaded || listed.is_some_and(|session| session.loaded));
        if loaded && view.state.is_none() {
            return SessionActionSafety::Unknown;
        }
        if loaded && (view.loading || view.reconcile_inflight || self.pending_history(session_id)) {
            return SessionActionSafety::Busy;
        }
        SessionActionSafety::Safe
    }

    fn request_session_state_for_action(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        if !self.sessions.known.contains_key(session_id) {
            let Some(info) = self
                .sessions
                .list
                .iter()
                .find(|session| &session.session_id == session_id)
                .cloned()
            else {
                return Vec::new();
            };
            self.sessions
                .known
                .insert(session_id.clone(), SessionView::new(info));
        }
        if self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.latest_state_query.is_some())
            || self.pending_requests.values().any(|request| {
                matches!(
                    request,
                    RequestKind::SessionState { session_id: pending, .. }
                        if pending == session_id
                )
            })
        {
            return Vec::new();
        }
        vec![self.request_session_state(session_id)]
    }

    fn session_loaded(&self, session_id: &SessionId) -> Option<bool> {
        if self.sessions.closed.contains(session_id) {
            return Some(false);
        }
        let listed = self
            .sessions
            .list
            .iter()
            .find(|session| &session.session_id == session_id)
            .map(|session| session.loaded)
            .unwrap_or(false);
        self.sessions
            .known
            .get(session_id)
            .map(|view| view.info.loaded || listed)
            .or_else(|| {
                self.sessions
                    .list
                    .iter()
                    .find(|session| &session.session_id == session_id)
                    .map(|session| session.loaded)
            })
    }

    fn invalidate_session_state_requests(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = None;
        }
        self.pending_requests.retain(|_, request| {
            !matches!(
                request,
                RequestKind::SessionState { session_id: pending, .. }
                    if pending == session_id
            )
        });
    }

    fn mark_close_verification_unknown(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = false;
            view.close_verification_unknown = true;
            if view
                .state
                .as_ref()
                .is_some_and(|state| state.status == SessionStatusWire::Idle)
            {
                view.state = None;
            }
        }
    }

    fn report_unknown_session_state(
        &mut self,
        session_id: &SessionId,
        action: &str,
    ) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            state.error = Some(format!(
                "cannot {action} while session state is unknown; reread it before retrying"
            ));
        }
        self.notice(
            NoticeLevel::Warning,
            format!("Session {session_id} state is unknown; reread it before retrying."),
        );
        self.request_session_state_for_action(session_id)
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

    fn submit_session_rename(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
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
                if !self.composer.type_char(c) {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
                    );
                }
                ui_actions::refresh_slash_completion(self);
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
                    let visible = self.viewport.1.max(1) as i32;
                    self.scroll_focused(delta * visible)
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
            _ => self.transcript_scroll(delta),
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
        if self.scrollbar_drag.is_some() {
            ui_actions::cancel_scrollbar_drag(self);
        }
        let Some(active) = self.sessions.active.clone() else {
            return;
        };
        let Some(view) = self.sessions.known.get_mut(&active) else {
            return;
        };
        let (total, visible) = self.viewport;
        let visible = visible.max(1);
        let max_offset = total.saturating_sub(visible);
        if delta < 0 {
            let amount = delta.unsigned_abs();
            let anchor = if view.scroll.follow_tail {
                max_offset
            } else {
                view.scroll.offset
            };
            view.scroll.follow_tail = false;
            view.scroll.offset = anchor.saturating_sub(amount as usize);
        } else {
            let current = if view.scroll.follow_tail {
                max_offset
            } else {
                view.scroll.offset
            };
            let next = current.saturating_add(delta as usize);
            if next >= max_offset {
                view.scroll.follow_tail = true;
                view.scroll.offset = 0;
                view.scroll.new_content = false;
            } else {
                view.scroll.follow_tail = false;
                view.scroll.offset = next;
            }
        }
    }

    fn transcript_scroll_top(&mut self) -> Vec<AppCommand> {
        if self.scrollbar_drag.is_some() {
            ui_actions::cancel_scrollbar_drag(self);
        }
        if let Some(view) = self.active_session_mut() {
            view.scroll.follow_tail = false;
            view.scroll.offset = 0;
        }
        Vec::new()
    }

    fn transcript_scroll_bottom(&mut self) -> Vec<AppCommand> {
        if self.scrollbar_drag.is_some() {
            ui_actions::cancel_scrollbar_drag(self);
        }
        if let Some(view) = self.active_session_mut() {
            view.scroll.follow_tail = true;
            view.scroll.offset = 0;
            view.scroll.new_content = false;
        }
        Vec::new()
    }

    /// Clamps the stored offset after a geometry/content change and marks
    /// `new_content` when the transcript grew while the user scrolled up
    /// (marker cleared by End/bottom scrolling).
    fn clamp_transcript_scroll(&mut self) {
        let (total, visible) = self.viewport;
        let grew = total > self.last_total;
        self.last_total = total;
        if let Some(view) = self.active_session_mut() {
            if grew && !view.scroll.follow_tail {
                view.scroll.new_content = true;
            }
            let max_offset = total.saturating_sub(visible);
            if !view.scroll.follow_tail {
                view.scroll.offset = view.scroll.offset.min(max_offset);
            }
        }
    }

    fn active_session_mut(&mut self) -> Option<&mut SessionView> {
        let active = self.sessions.active.clone()?;
        self.sessions.known.get_mut(&active)
    }

    fn cancel_active_turn(&mut self) -> Vec<AppCommand> {
        let Some(active) = self.sessions.active.clone() else {
            self.notice(NoticeLevel::Warning, "no active turn to cancel");
            return Vec::new();
        };
        self.cancel_turn(&active)
    }

    fn open_dock(&mut self, dock: Dock) -> Vec<AppCommand> {
        if self.dock == dock {
            return self.cancel_dock();
        }
        self.panel_scroll = 0;
        self.dock = dock;
        Vec::new()
    }

    /// Submitting the composer: slash lines are parsed locally; plain text
    /// goes to the active session and clears the composer. Nothing is ever
    /// silently swallowed; a missing agent or session gets a notice.
    pub fn submit_composer(&mut self) -> Vec<AppCommand> {
        self.editor_selection = None;
        let text = self.composer.content().trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        if text.len() > MAX_COMPOSER_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
            );
            return Vec::new();
        }
        if is_slash_command(&text) {
            let commands = self.run_command(&text);
            self.composer.clear();
            return commands;
        }
        let Some(active) = self.sessions.active.clone() else {
            self.notice(
                NoticeLevel::Info,
                "Open or create a session first — /new or Ctrl+R.",
            );
            return Vec::new();
        };
        let is_running = self
            .sessions
            .known
            .get(&active)
            .map(|v| v.is_running())
            .unwrap_or(false);
        let status = self
            .sessions
            .known
            .get(&active)
            .and_then(|view| view.state.as_ref().map(|state| state.status));
        let is_blocked = status == Some(SessionStatusWire::Blocked);
        let is_waiting = status == Some(SessionStatusWire::WaitingForInput)
            || self
                .sessions
                .known
                .get(&active)
                .and_then(|view| view.live.as_ref())
                .is_some_and(|live| live.waiting);

        if is_blocked {
            self.notice(
                NoticeLevel::Error,
                "session is blocked; resolve or reset before submitting",
            );
            return Vec::new();
        }

        if is_running {
            let editor_revision = self.composer.editor_revision();
            let mut commands = self.steer_turn_with_revision(&active, text, Some(editor_revision));
            // Enter path issues the single in-flight steer via the same central
            // FIFO advance used by every other event (one RPC per session).
            commands.extend(self.advance_steer_queues());
            commands
        } else if is_waiting || status == Some(SessionStatusWire::Finishing) {
            self.notice(
                NoticeLevel::Warning,
                "session is not accepting input right now",
            );
            Vec::new()
        } else {
            let submitted_text = text.clone();
            let commands = self.submit_turn(active, text);
            if !commands.is_empty() {
                self.composer.submit_pushed(&submitted_text);
                self.composer.clear();
            }
            commands
        }
    }

    pub fn steer_turn(&mut self, session_id: &SessionId, text: String) -> Vec<AppCommand> {
        self.steer_turn_with_revision(session_id, text, None)
    }

    fn steer_turn_with_revision(
        &mut self,
        session_id: &SessionId,
        text: String,
        editor_revision: Option<u64>,
    ) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        if text.len() > MAX_COMPOSER_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
            );
            return Vec::new();
        }
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.closing {
            self.notice(NoticeLevel::Warning, "session is closing; cannot steer");
            return Vec::new();
        }
        if view.is_blocked() {
            self.notice(NoticeLevel::Warning, "session is blocked; cannot steer");
            return Vec::new();
        }
        if !view.is_running() {
            self.notice(NoticeLevel::Warning, "session is not running; cannot steer");
            return Vec::new();
        }
        // FIFO admission: a bounded per-session queue. No silent Sending-guard
        // drop; a full queue keeps the editor text and pauses instead.
        if view.steer_queue.len() >= MAX_STEER_QUEUE_LEN {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.steer_queue_paused = true;
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "steer queue is full ({MAX_STEER_QUEUE_LEN}); keep the message and retry after the turn"
                ),
            );
            return Vec::new();
        }
        self.next_steer_id = self
            .next_steer_id
            .checked_add(1)
            .expect("steering ids exhausted");
        let steer_id = self.next_steer_id;
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            // An explicit user admission is deliberate: it re-opens the gate
            // (the pause only blocks AUTOMATIC advance after cancel/failure).
            view.steer_queue_paused = false;
            view.steer_queue.push(crate::state::turn::SteerQueueItem {
                local_id: steer_id,
                text: text.clone(),
                state: crate::state::turn::SteerQueueState::Unsent,
                editor_revision,
                handoff: false,
            });
        }
        // A late ACK must never clear new editor content: clear only when the
        // submitting revision and text still match the composer exactly.
        let composer_text = self.composer.content().trim().to_owned();
        if self.sessions.active.as_ref() == Some(session_id)
            && editor_revision.is_some_and(|revision| revision == self.composer.editor_revision())
            && composer_text == text
        {
            self.composer.submit_pushed(&composer_text);
            self.composer.clear();
        }
        // The RPC is issued by the central FIFO advance (at most one in
        // flight per session), not inline.
        Vec::new()
    }

    /// Central FIFO queue advance for every session. At most ONE steer RPC (or
    /// accepted-but-unconfirmed item) per session until a receipt proves the
    /// previous one was issued into a request. A loop that sealed before the
    /// next unsent steer could be sent falls back to a fresh turn once the
    /// previous loop completed, persisted, and settled.
    fn advance_steer_queues(&mut self) -> Vec<AppCommand> {
        // Never issue a NEW steer (or fresh-turn) RPC once the connection is
        // failed or shutting down.
        if !self.can_send_requests() {
            return Vec::new();
        }
        let session_ids: Vec<SessionId> = self.sessions.known.keys().cloned().collect();
        let mut commands = Vec::new();
        for session_id in session_ids {
            commands.extend(self.advance_steer_queue_for(&session_id));
        }
        commands
    }

    fn advance_steer_queue_for(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.steer_queue_paused || view.steer_queue.is_empty() || view.closing {
            return Vec::new();
        }
        if view
            .steer_queue
            .iter()
            .any(|item| item.state == SteerQueueState::Unconfirmed)
        {
            // An uncertain outcome blocks the WHOLE queue advance: never
            // auto-resend it, and never let a newer unsent item skip past it.
            return Vec::new();
        }
        let running = view.is_running();
        let loop_id = view
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|r| r.loop_id.clone());
        if running {
            let Some(loop_id) = loop_id else {
                return Vec::new();
            };
            let has_inflight = view.live.as_ref().is_some_and(|live| {
                live.pending_steers.iter().any(|steer| {
                    matches!(
                        steer.state,
                        PendingSteerState::Sending
                            | PendingSteerState::Queued
                            | PendingSteerState::Unconfirmed
                    )
                })
            }) || view.steer_queue.iter().any(|item| item.handoff);
            let Some(head) = view.steer_queue.iter().find(|item| {
                item.state == crate::state::turn::SteerQueueState::Unsent && !item.handoff
            }) else {
                return Vec::new();
            };
            if has_inflight {
                return Vec::new();
            }
            let head = head.clone();
            // Pop from the local queue; the RPC lifecycle (Sending/Accepted,
            // then receipts) takes over from `pending_steers`.
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.steer_queue
                    .retain(|item| item.local_id != head.local_id);
                if let Some(live) = view.live.as_mut() {
                    live.pending_steers.push(PendingSteer {
                        local_id: head.local_id,
                        text: head.text.clone(),
                        state: PendingSteerState::Sending,
                        accepted_at: None,
                        steer_index: None,
                    });
                }
            }
            let req_loop_id = loop_id.clone();
            let req_text = head.text.clone();
            let req_revision = head.editor_revision;
            vec![self.request(
                RequestKind::SteerTurn {
                    session_id: session_id.clone(),
                    loop_id: loop_id.clone(),
                    steer_id: head.local_id,
                    text: head.text.clone(),
                    editor_revision: req_revision,
                },
                |id| {
                    OutgoingRequest::steer_turn(
                        id,
                        &TurnRef {
                            session_id: session_id.clone(),
                            loop_id: req_loop_id.clone(),
                        },
                        &req_text,
                    )
                },
            )]
        } else {
            // Race fallback: the previous loop sealed before this unsent
            // steer could be sent. Only after a normal completed+persisted,
            // history-settled idle session does the head become a fresh turn
            // (never a resend of an accepted message).
            // Explicit normal completion gate: only a genuinely completed,
            // persisted, history-settled idle session may start the next
            // queued message as a fresh turn (never after cancel/refusal/
            // error/blocked/unsaved, and never for an old loop).
            let settled = view.last_result.as_ref().is_some_and(|result| {
                result.outcome == crate::protocol::LoopOutcomeWire::Completed
            }) && view.transcript.complete
                && !view.is_blocked()
                && view.unsaved_loop.is_none()
                && !view.result_unconfirmed
                && view.state.as_ref().map(|s| s.status) == Some(SessionStatusWire::Idle);
            if !settled {
                return Vec::new();
            }
            let head = view
                .steer_queue
                .iter()
                .find(|item| {
                    item.state == crate::state::turn::SteerQueueState::Unsent && !item.handoff
                })
                .cloned();
            let Some(head) = head else {
                return Vec::new();
            };
            let text = head.text.clone();
            let commands = self.submit_turn(session_id.clone(), text);
            if !commands.is_empty() {
                // Keep the entry until the turn.send ACK (a send failure must
                // not drop the text); the handoff marker blocks FIFO jump.
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    if let Some(item) = view
                        .steer_queue
                        .iter_mut()
                        .find(|item| item.local_id == head.local_id)
                    {
                        item.handoff = true;
                    }
                }
            }
            commands
        }
    }

    /// Withdraws the NEXT UNSENT queue item into the empty editor (only when
    /// the composer is empty; accepted/in-flight messages cannot be withdrawn
    /// because they are already owned by the loop). Returns true when taken.
    pub fn retrieve_next_queued_steer(&mut self) -> bool {
        if !self.composer.is_empty() {
            return false;
        }
        let Some(session_id) = self.sessions.active.clone() else {
            return false;
        };
        let Some(head) = self.sessions.known.get(&session_id).and_then(|view| {
            view.steer_queue
                .iter()
                .find(|item| item.state == SteerQueueState::Unsent)
                .cloned()
        }) else {
            return false;
        };
        let text = head.text.clone();
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.steer_queue
                .retain(|item| item.local_id != head.local_id);
        }
        self.composer.set_text(&text);
        true
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
            LocalCommand::New => self.open_new_session(),
            LocalCommand::Resume | LocalCommand::Sessions => {
                self.open_selector(SelectorKind::Session)
            }
            LocalCommand::Model => self.open_selector(SelectorKind::Model),
            LocalCommand::Reasoning => self.open_selector(SelectorKind::Reasoning),
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
                if let Some(session_id) = self.sessions.active.clone() {
                    self.delete_session(&session_id, confirm)
                } else {
                    self.notice(NoticeLevel::Warning, "no active session to delete");
                    Vec::new()
                }
            }
            LocalCommand::Clear => self.clear_transcript(),
            LocalCommand::Help => self.open_dock(Dock::Help),
            LocalCommand::Logs => self.open_dock(Dock::Logs),
            LocalCommand::Cancel => self.cancel_active_turn(),
            LocalCommand::Refresh => {
                if let Some(session_id) = self.sessions.active.clone() {
                    self.refresh_turn(&session_id)
                } else {
                    self.notice(NoticeLevel::Warning, "no active session to refresh");
                    Vec::new()
                }
            }
            LocalCommand::Quit => self.request_shutdown(),
        }
    }

    /// `/clear` wipes only the local view of the active session and reloads
    /// its transcript from the beginning; the agent session is untouched
    /// and the command is refused while a turn is running (spec 23.3).
    fn clear_transcript(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
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
            view.reconcile_inflight = reconciling_gap;
            view.loading = true;
        }
        let offset = 0;
        vec![self.request_history(&active, offset, DEFAULT_HISTORY_LIMIT)]
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

    fn request_session_state(&mut self, session_id: &SessionId) -> AppCommand {
        let query = self.next_state_query;
        self.next_state_query = self
            .next_state_query
            .checked_add(1)
            .expect("session state query space exhausted");
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = Some(query);
        }
        self.request(
            RequestKind::SessionState {
                session_id: session_id.clone(),
                query,
            },
            |id| OutgoingRequest::session_state(id, session_id),
        )
    }

    fn request_session_presentation(&mut self, session_id: &SessionId) -> Option<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return None;
        }
        if !self.can_send_requests() {
            return None;
        }
        let view = self.sessions.known.get_mut(session_id)?;
        if view.presentation_pending {
            view.presentation_refresh_pending = true;
            return None;
        }
        view.presentation_pending = true;
        Some(self.request(
            RequestKind::SessionPresentation {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_presentation(id, session_id),
        ))
    }

    fn pending_history(&self, session_id: &SessionId) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(kind, RequestKind::History { session_id: pending, .. } if pending == session_id)
        })
    }

    /// Every history request captures the current local gap revision. Normal
    /// initial loads carry revision zero too, so an in-flight response cannot
    /// clear a gap that was observed after the request was issued.
    fn request_history(
        &mut self,
        session_id: &SessionId,
        offset: usize,
        limit: usize,
    ) -> AppCommand {
        let gap_revision = self
            .sessions
            .known
            .get(session_id)
            .expect("history request requires a known session")
            .gap_revision;
        self.request(
            RequestKind::History {
                session_id: session_id.clone(),
                offset,
                limit,
                gap_revision: Some(gap_revision),
            },
            |id| OutgoingRequest::session_history(id, session_id, Some(offset), Some(limit)),
        )
    }

    fn has_initialized_session_view(view: &SessionView) -> bool {
        view.info.loaded
            || view.state.is_some()
            || view.transcript.complete
            || view.live.is_some()
            || view.unsaved_loop.is_some()
    }

    fn activate_existing_session(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        if self.sessions.active.as_ref() != Some(session_id) {
            ui_actions::cancel_scrollbar_drag(self);
            ui_actions::clear_selection(self);
        }
        self.sessions.active = Some(session_id.clone());
        let state_pending = self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.latest_state_query.is_some());
        let mut commands = if state_pending {
            Vec::new()
        } else {
            vec![self.request_session_state(session_id)]
        };
        if let Some(view) = self.sessions.known.get(session_id) {
            if view.presentation.is_none() && !view.presentation_pending {
                if let Some(command) = self.request_session_presentation(session_id) {
                    commands.push(command);
                }
            }
        }
        let (fetch, reconciling_gap) = {
            let Some(view) = self.sessions.known.get(session_id) else {
                return commands;
            };
            if view.loading || self.pending_history(session_id) {
                (false, false)
            } else if view.event_gap {
                (true, true)
            } else if !view.transcript.complete {
                (true, false)
            } else {
                (false, false)
            }
        };
        if fetch {
            let offset = self
                .sessions
                .known
                .get(session_id)
                .map(|view| view.transcript.loaded_count)
                .unwrap_or(0);
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.loading = true;
                view.reconcile_inflight = reconciling_gap;
            }
            commands.push(self.request_history(session_id, offset, DEFAULT_HISTORY_LIMIT));
        }
        commands
    }

    fn can_activate_existing_session(&self, session_id: &SessionId) -> bool {
        self.sessions.known.get(session_id).is_some_and(|view| {
            view.info.loaded && !view.closing && Self::has_initialized_session_view(view)
        })
    }

    fn pending_open_or_history(&self, session_id: &SessionId) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::OpenSession { session_id: pending, .. } if pending == session_id
            ) || matches!(
                kind,
                RequestKind::History { session_id: pending, .. } if pending == session_id
            )
        })
    }

    fn request_session_id(kind: &RequestKind) -> Option<&str> {
        match kind {
            RequestKind::OpenSession { session_id, .. }
            | RequestKind::CloseSession { session_id }
            | RequestKind::CloseVerifyState { session_id }
            | RequestKind::DeleteSession { session_id }
            | RequestKind::SendTurn { session_id, .. }
            | RequestKind::SteerTurn { session_id, .. }
            | RequestKind::UpdateSession { session_id, .. }
            | RequestKind::RenameSession { session_id }
            | RequestKind::History { session_id, .. }
            | RequestKind::SessionState { session_id, .. } => Some(session_id),
            RequestKind::SessionPresentation { session_id } => Some(session_id),
            RequestKind::WaitTurn(turn) | RequestKind::CancelTurn(turn) => Some(&turn.session_id),
            RequestKind::Ping
            | RequestKind::ListModels
            | RequestKind::ListProfiles
            | RequestKind::ListSessions
            | RequestKind::RefreshSessions { .. }
            | RequestKind::CreateSession { .. }
            | RequestKind::Shutdown => None,
        }
    }

    /// Retires the current loop for event routing without discarding a wait
    /// that may still complete before the new session.open response.
    fn retire_reopened_session(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            let retired = view
                .live
                .as_ref()
                .and_then(|live| live.reference.clone())
                .or_else(|| view.unsaved_loop.as_ref().map(|loop_| loop_.turn.clone()))
                .or_else(|| view.last_result.as_ref().map(|result| result.turn.clone()));
            if retired.is_some() {
                view.retired_loop = retired;
            }
            view.latest_state_query = None;
        }
    }

    /// Invalidates only requests belonging to an explicitly reopened session
    /// after the new open response has been accepted. The single retired loop
    /// fence blocks already-buffered old events without retaining an unbounded
    /// registry.
    fn invalidate_reopened_session(&mut self, session_id: &SessionId) {
        self.pending_requests
            .retain(|_, kind| Self::request_session_id(kind) != Some(session_id.as_str()));
        self.retire_reopened_session(session_id);
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
                .items
                .iter()
                .any(|item| item.item.loop_id() == Some(loop_id))
    }

    fn history_proves_steer_not_recorded(
        view: &SessionView,
        loop_id: &str,
        steer_text: &str,
    ) -> bool {
        view.transcript.complete
            && (view.last_result.as_ref().is_some_and(|result| {
                result.turn.loop_id == loop_id
                    && result.persistence == TurnPersistenceWire::Persisted
            }) || view.live.as_ref().is_some_and(|live| {
                live.last_result.as_ref().is_some_and(|result| {
                    result.turn.loop_id == loop_id
                        && result.persistence == TurnPersistenceWire::Persisted
                })
            }))
            && view
                .transcript
                .items
                .iter()
                .any(|item| item.item.loop_id() == Some(loop_id))
            && !view.transcript.items.iter().any(|item| {
                matches!(
                    &item.item,
                    HistoryItemWire::User(user)
                        if user.loop_id == loop_id
                            && user.kind == UserMessageKindWire::Steering
                            && user.text == steer_text
                )
            })
    }

    fn mark_history_unconfirmed(view: &mut SessionView) {
        view.loading = false;
        view.reconcile_inflight = false;
        view.event_gap = true;
        view.transcript.complete = false;
        Self::mark_pending_steers_unconfirmed(view);
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
        self.notices.push_back(notice);
        while self.notices.len() > MAX_NOTICES {
            self.notices.pop_front();
        }
    }

    fn push_log(&mut self, line: String) {
        self.agent_logs.push_back(line);
        while self.agent_logs.len() > MAX_AGENT_LOG_LINES {
            self.agent_logs.pop_front();
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
        let sessions = self.request(RequestKind::ListSessions, OutgoingRequest::list_sessions);
        vec![ping, models, profiles, sessions]
    }

    fn bootstrap_progress(&mut self, part: BootstrapPart) {
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
            if self.open_new_session_on_ready && self.sessions.active.is_none() {
                self.open_new_session_on_ready = false;
                self.open_new_session();
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
        self.connection = ConnectionState::ShuttingDown;
        if self.shutdown_sent {
            return Vec::new();
        }
        self.shutdown_sent = true;
        vec![self.request(RequestKind::Shutdown, OutgoingRequest::shutdown)]
    }

    fn can_send_requests(&self) -> bool {
        match self.connection {
            ConnectionState::Starting | ConnectionState::Ready => true,
            ConnectionState::ShuttingDown | ConnectionState::Failed(_) => false,
        }
    }

    fn bootstrap_failure(&mut self, method: &str, error: RpcResponseError) -> Vec<AppCommand> {
        self.connection_terminated(&format!("bootstrap request {method} failed: {error}"))
    }

    fn create_session(
        &mut self,
        workspace: &str,
        profile: Option<&str>,
        model: Option<&str>,
        reasoning: Option<Reasoning>,
        title: Option<&str>,
    ) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        vec![
            self.request(RequestKind::CreateSession { draft: u64::MAX }, |id| {
                OutgoingRequest::session_create(id, workspace, profile, model, reasoning, title)
            }),
        ]
    }

    fn open_session(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        if self.pending_open_or_history(session_id)
            || self
                .sessions
                .known
                .get(session_id)
                .is_some_and(|view| view.closing)
        {
            return Vec::new();
        }
        if self.sessions.active.as_ref() != Some(session_id) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        if self.can_activate_existing_session(session_id) {
            return self.activate_existing_session(session_id);
        }
        // Establish the lifecycle fence before the new request leaves the
        // reducer. Old notifications can arrive before session.open responds.
        let retired_loop_on_failure = self
            .sessions
            .known
            .get(session_id)
            .and_then(|view| view.retired_loop.clone());
        self.retire_reopened_session(session_id);
        vec![self.request(
            RequestKind::OpenSession {
                session_id: session_id.clone(),
                previous_retired_loop: retired_loop_on_failure,
            },
            |id| OutgoingRequest::session_open(id, session_id),
        )]
    }

    fn refresh_sessions(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.session_panel_busy() {
            return Vec::new();
        }
        if !self
            .session_selector_state()
            .is_some_and(|state| matches!(&state.mode, SessionPanelMode::Browse))
        {
            return Vec::new();
        }
        if self
            .pending_requests
            .values()
            .any(|request| matches!(request, RequestKind::RefreshSessions { .. }))
        {
            return Vec::new();
        }
        let selected_session_id = self.selected_session_id();
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
        }
        vec![self.request(
            RequestKind::RefreshSessions {
                selected_session_id,
            },
            OutgoingRequest::list_sessions,
        )]
    }

    fn close_session(&mut self, session_id: &SessionId, confirm: bool) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.pending_requests.values().any(|request| {
            matches!(
                request,
                RequestKind::CloseSession { session_id: pending }
                    | RequestKind::CloseVerifyState { session_id: pending }
                    if pending == session_id
            )
        }) {
            return Vec::new();
        }
        match self.session_loaded(session_id) {
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
        if self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.event_gap)
        {
            if let Some(state) = self.session_selector_state_mut() {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    state.error =
                        Some("cannot close while history reconciliation is incomplete".to_owned());
                }
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Cannot close session {session_id} until history reconciliation completes."
                ),
            );
            return Vec::new();
        }
        let history_pending = self.pending_history(session_id);
        let history_incomplete = self.session_loaded(session_id) == Some(true)
            && (history_pending
                || self
                    .sessions
                    .known
                    .get(session_id)
                    .is_some_and(|view| view.loading || view.reconcile_inflight));
        if history_incomplete {
            if let Some(state) = self.session_selector_state_mut() {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    state.error = Some("cannot close while history is incomplete".to_owned());
                }
            }
            self.notice(
                NoticeLevel::Warning,
                format!("Cannot close session {session_id} until history completes."),
            );
            return Vec::new();
        }
        if matches!(
            self.session_action_safety(session_id),
            SessionActionSafety::Unknown
        ) {
            return self.report_unknown_session_state(session_id, "close");
        }
        let (is_blocked, has_unsaved, has_unconfirmed, is_active) =
            match self.sessions.known.get(session_id) {
                Some(view) => (
                    view.is_blocked(),
                    view.unsaved_loop.is_some(),
                    view.result_unconfirmed,
                    view.live.is_some()
                        || view
                            .state
                            .as_ref()
                            .is_some_and(|state| state.status != SessionStatusWire::Idle),
                ),
                None => (false, false, false, false),
            };
        if (is_blocked || has_unsaved || has_unconfirmed || is_active) && !confirm {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Session {session_id} has active or unsaved/blocked state. Type '/close confirm' to proceed."
                ),
            );
            return Vec::new();
        }
        if self.sessions.active.as_deref() == Some(session_id.as_str()) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        let mut commands = Vec::new();
        let reference = if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = true;
            view.live.as_ref().and_then(|l| l.reference.clone())
        } else {
            None
        };
        if let Some(reference) = reference {
            let has_wait = self
                .pending_requests
                .values()
                .any(|req| matches!(req, RequestKind::WaitTurn(t) if t == &reference));
            if !has_wait {
                commands.push(
                    self.request(RequestKind::WaitTurn(reference.clone()), |id| {
                        OutgoingRequest::wait_turn(id, &reference)
                    }),
                );
            }
        }
        commands.push(self.request(
            RequestKind::CloseSession {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_close(id, session_id),
        ));
        commands
    }

    fn mark_session_closed(&mut self, session_id: &SessionId) {
        self.sessions.closed.insert(session_id.clone());
        self.invalidate_session_state_requests(session_id);
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = false;
            view.close_verification_unknown = false;
            view.info.loaded = false;
        }
        if let Some(info) = self
            .sessions
            .known
            .get(session_id)
            .map(|view| view.info.clone())
        {
            self.upsert_session_list(info);
        }
        self.retire_reopened_session(session_id);
        if self.sessions.active.as_deref() == Some(session_id.as_str()) {
            ui_actions::cancel_scrollbar_drag(self);
            ui_actions::clear_selection(self);
            self.sessions.active = None;
        }
    }

    fn on_close_session_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id) {
            return Vec::new();
        }
        match response.parse_close() {
            Ok(_) => {
                self.mark_session_closed(session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        match &state.mode {
                            SessionPanelMode::ConfirmCloseForDelete => {
                                state.mode = SessionPanelMode::ConfirmDelete {
                                    choice: SessionConfirmChoice::Cancel,
                                    submitting: false,
                                };
                                state.error = None;
                            }
                            SessionPanelMode::ConfirmClose => {
                                state.mode = SessionPanelMode::Browse;
                                state.error = None;
                            }
                            _ => {}
                        }
                    }
                }
                self.reconcile_session_selection(true);
                self.notice(NoticeLevel::Info, format!("Session {session_id} closed."));
                Vec::new()
            }
            Err(RpcResponseError::Agent(_error)) => {
                // MIG-146: close returns error, perform a single read check of session state.
                // Do not retry indefinitely.
                self.invalidate_session_state_requests(session_id);
                vec![self.request(
                    RequestKind::CloseVerifyState {
                        session_id: session_id.clone(),
                    },
                    |id| OutgoingRequest::session_state(id, session_id),
                )]
            }
            Err(error) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Error,
                    format!("Failed to close session {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    fn on_close_verify_state_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id) {
            return Vec::new();
        }
        match response.parse_session_state() {
            Ok(state) if state.session_id == *session_id => {
                self.apply_session_state(&state, None, false);
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.closing = false;
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification: status is {:?}; unload not confirmed",
                        state.status
                    ),
                );
            }
            Ok(_) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification returned another session; state is unknown"
                    ),
                );
            }
            Err(RpcResponseError::Agent(error))
                if error.code == crate::protocol::SESSION_NOT_LOADED =>
            {
                self.mark_session_closed(session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        match &state.mode {
                            SessionPanelMode::ConfirmCloseForDelete => {
                                state.mode = SessionPanelMode::ConfirmDelete {
                                    choice: SessionConfirmChoice::Cancel,
                                    submitting: false,
                                };
                                state.error = None;
                            }
                            SessionPanelMode::ConfirmClose => {
                                state.mode = SessionPanelMode::Browse;
                                state.error = None;
                            }
                            _ => {}
                        }
                    }
                }
                self.notice(
                    NoticeLevel::Info,
                    format!("Session {session_id} was confirmed closed."),
                );
            }
            Err(error) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification is unknown; result/state retained: {error}"
                    ),
                );
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        state.mode = SessionPanelMode::Browse;
                        state.error = Some(error.to_string());
                    }
                }
            }
        }
        self.reconcile_session_selection(true);
        Vec::new()
    }

    fn delete_session(&mut self, session_id: &SessionId, confirm: bool) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
            || self.pending_requests.values().any(|request| {
                matches!(request, RequestKind::DeleteSession { session_id: pending } if pending == session_id)
            })
        {
            return Vec::new();
        }
        if !confirm {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Deleting session {session_id} is permanent. Type '/delete confirm' to proceed."
                ),
            );
            return Vec::new();
        }
        let Some(loaded) = self.session_loaded(session_id) else {
            self.notice(
                NoticeLevel::Warning,
                format!("Session {session_id} is not available for deletion."),
            );
            return Vec::new();
        };
        if !matches!(
            self.session_action_safety(session_id),
            SessionActionSafety::Safe
        ) {
            if matches!(
                self.session_action_safety(session_id),
                SessionActionSafety::Unknown
            ) {
                return self.report_unknown_session_state(session_id, "delete");
            }
            self.notice(
                NoticeLevel::Warning,
                format!("Session {session_id} is busy or its result is unconfirmed; deletion is blocked."),
            );
            return Vec::new();
        }
        if loaded {
            if let Dock::SessionSelector(state) = &mut self.dock {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    state.mode = SessionPanelMode::ConfirmCloseForDelete;
                }
            }
            self.notice(
                NoticeLevel::Warning,
                format!("Close session {session_id} before deleting it."),
            );
            return Vec::new();
        }
        if self.sessions.active.as_deref() == Some(session_id.as_str()) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        self.sessions.pending_deletes.insert(session_id.clone());
        vec![self.request(
            RequestKind::DeleteSession {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_delete(id, session_id),
        )]
    }

    fn on_delete_session_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id) {
            return Vec::new();
        }
        match response.parse_delete() {
            Ok(_) => {
                self.pending_requests.retain(|_, request| {
                    Self::request_session_id(request) != Some(session_id.as_str())
                });
                self.mouse_down = None;
                self.panel_click = None;
                self.sessions.pending_deletes.remove(session_id);
                self.sessions.deleted.insert(session_id.clone());
                self.sessions.closed.remove(session_id);
                self.sessions.title_overrides.remove(session_id);
                self.sessions.known.remove(session_id);
                self.sessions.list.retain(|s| &s.session_id != session_id);
                if self.sessions.active.as_deref() == Some(session_id.as_str()) {
                    ui_actions::cancel_scrollbar_drag(self);
                    ui_actions::clear_selection(self);
                    self.sessions.active = None;
                }
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        state.mode = SessionPanelMode::Browse;
                        state.error = None;
                    }
                }
                self.reconcile_session_selection(true);
                self.notice(NoticeLevel::Info, format!("Session {session_id} deleted."));
                Vec::new()
            }
            Err(error) => {
                self.sessions.pending_deletes.remove(session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if matches!(&state.mode, SessionPanelMode::ConfirmDelete { .. }) {
                            state.mode = SessionPanelMode::ConfirmDelete {
                                choice: SessionConfirmChoice::Cancel,
                                submitting: false,
                            };
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Error,
                    format!("Failed to delete session {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    fn on_session_response(
        &mut self,
        session_id: SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(&session_id)
            || self.sessions.pending_deletes.contains(&session_id)
        {
            return Vec::new();
        }
        let session = match response.parse_session() {
            Ok(result) => result.session,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("failed to parse session {session_id}: {error}"),
                );
                return Vec::new();
            }
        };
        if session.session_id != session_id {
            self.notice(
                NoticeLevel::Error,
                format!("session response does not match requested session {session_id}"),
            );
            return Vec::new();
        }
        self.sessions.closed.remove(&session_id);
        let mut commands = Vec::new();
        match self.sessions.known.get_mut(&session_id) {
            Some(view) => {
                view.info = session;
                view.completed_steers.clear();
            }
            None => {
                self.sessions
                    .known
                    .insert(session_id.clone(), SessionView::new(session));
            }
        }
        let listed_info = self
            .sessions
            .known
            .get(&session_id)
            .map(|view| view.info.clone());
        if let Some(info) = listed_info {
            self.upsert_session_list(info);
        }
        if self.sessions.active.as_ref() != Some(&session_id) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        ui_actions::clear_selection(self);
        self.sessions.active = Some(session_id.clone());

        commands.push(self.request_session_state(&session_id));
        if let Some(command) = self.request_session_presentation(&session_id) {
            commands.push(command);
        }

        let (fetch, reconciling_gap) = {
            let Some(view) = self.sessions.known.get(&session_id) else {
                return commands;
            };
            if view.loading {
                (false, false)
            } else if view.event_gap {
                (true, true)
            } else if !view.transcript.complete {
                (true, false)
            } else {
                (false, false)
            }
        };
        if fetch {
            let offset = self
                .sessions
                .known
                .get(&session_id)
                .map(|v| v.transcript.loaded_count)
                .unwrap_or(0);
            if let Some(view) = self.sessions.known.get_mut(&session_id) {
                view.loading = true;
                view.reconcile_inflight = reconciling_gap;
            }
            commands.push(self.request_history(&session_id, offset, 20));
        }
        commands
    }

    fn on_refresh_sessions_response(&mut self, response: &RpcResponse) -> Vec<AppCommand> {
        let result = match response.parse_sessions() {
            Ok(result) => result,
            Err(error) => {
                if let Some(state) = self.session_selector_state_mut() {
                    state.error = Some(format!("refresh failed: {error}"));
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session list refresh failed: {error}"),
                );
                return Vec::new();
            }
        };
        let rename_pending: std::collections::HashSet<SessionId> = self
            .pending_requests
            .values()
            .filter_map(|kind| match kind {
                RequestKind::RenameSession { session_id } => Some(session_id.clone()),
                _ => None,
            })
            .collect();
        let mut visible = Vec::with_capacity(result.sessions.len());
        for mut session in result.sessions {
            let session_id = session.session_id.clone();
            if self.sessions.pending_deletes.contains(&session_id)
                || self.sessions.deleted.contains(&session_id)
            {
                continue;
            }
            if self.sessions.closed.contains(&session_id) {
                session.loaded = false;
            }
            if let Some(title) = self.sessions.title_overrides.get(&session_id) {
                session.title = title.clone();
            }
            if !rename_pending.contains(&session_id) {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.info = session.clone();
                    if !session.loaded {
                        view.latest_state_query = None;
                    }
                } else {
                    self.sessions
                        .known
                        .insert(session_id.clone(), SessionView::new(session.clone()));
                }
            }
            visible.push(session);
        }
        self.sessions.list = visible;
        self.sessions.title_overrides.clear();
        self.reconcile_session_selection(true);
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
        }
        Vec::new()
    }

    fn on_rename_session_response(
        &mut self,
        session_id: SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(&session_id)
            || self.sessions.pending_deletes.contains(&session_id)
        {
            return Vec::new();
        }
        let parsed = response.parse_session_rename();
        let session = match parsed {
            Ok(result) => result.session,
            Err(error) => {
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                            *submitting = false;
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.rename failed for {session_id}: {error}"),
                );
                return Vec::new();
            }
        };
        if session.session_id != session_id {
            let error =
                format!("session.rename response does not match requested session {session_id}");
            if let Dock::SessionSelector(state) = &mut self.dock {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                        *submitting = false;
                    }
                    state.error = Some(error.clone());
                }
            }
            self.notice(NoticeLevel::Warning, error);
            return Vec::new();
        }
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.info = session.clone();
        } else {
            self.sessions
                .known
                .insert(session_id.clone(), SessionView::new(session.clone()));
        }
        self.sessions
            .title_overrides
            .insert(session_id.clone(), session.title.clone());
        self.upsert_session_list(session);
        let mut returned_to_browse = false;
        if let Dock::SessionSelector(state) = &mut self.dock {
            if state.selected_session_id.as_deref() == Some(session_id.as_str())
                && matches!(&state.mode, SessionPanelMode::Rename { .. })
            {
                state.mode = SessionPanelMode::Browse;
                state.error = None;
                returned_to_browse = true;
            }
        }
        if returned_to_browse {
            self.reconcile_session_selection(true);
        }
        self.notice(NoticeLevel::Info, format!("Session {session_id} renamed."));
        Vec::new()
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
        self.on_session_response(session_id, response)
    }

    fn on_open_response(
        &mut self,
        session_id: SessionId,
        previous_retired_loop: Option<TurnRef>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(&session_id)
            || self.sessions.pending_deletes.contains(&session_id)
        {
            return Vec::new();
        }
        let parsed = response.parse_session();
        if let Err(error) = &parsed {
            if let Some(retired_loop) = previous_retired_loop.clone() {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.retired_loop = Some(retired_loop);
                }
            }
            let message = match error {
                RpcResponseError::Agent(error) if error.code == crate::protocol::STORE_ERROR => {
                    "Unable to open this session. Its data may be unavailable, invalid, or from an unsupported format.".to_owned()
                }
                _ => format!("session.open failed: {error}"),
            };
            if let Dock::SessionSelector(state) = &mut self.dock {
                state.error = Some(message);
            } else {
                self.notice(NoticeLevel::Error, message);
            }
            return Vec::new();
        }
        if parsed
            .as_ref()
            .is_ok_and(|result| result.session.session_id != session_id)
        {
            if let Some(retired_loop) = previous_retired_loop {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.retired_loop = Some(retired_loop);
                }
            }
            let message =
                format!("session.open response does not match requested session {session_id}");
            if let Dock::SessionSelector(state) = &mut self.dock {
                state.error = Some(message);
            } else {
                self.notice(NoticeLevel::Error, message);
            }
            return Vec::new();
        }
        // Reopen is a lifecycle boundary. Retire old request ids before
        // rebuilding the view so late responses cannot mutate the new load.
        self.invalidate_reopened_session(&session_id);
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            // Rebuild from history offset 0; never compare the new total with
            // the old local projection.
            let preserving_gap = view.event_gap;
            view.transcript.clear_blocks();
            view.loading = false;
            view.reconcile_inflight = preserving_gap;
            view.needs_post_wait_history = false;
            view.closing = false;
            view.close_verification_unknown = false;
            view.live = None;
            view.unsaved_loop = None;
            view.last_result = None;
            view.usage_projection = crate::state::session::UsageProjection::default();
            view.last_request = None;
            view.config_update = None;
            view.presentation = None;
            view.presentation_pending = false;
            view.presentation_refresh_pending = false;
            view.user_timestamps.clear();
            view.live_user_timestamp = None;
            view.live_user_time_accepted = false;
            view.tool_presentations.clear();
            view.completed_steers.clear();
            view.result_unconfirmed = false;
        }
        let opened_id = session_id.clone();
        let commands = self.on_session_response(session_id, response);
        if matches!(&self.dock, Dock::SessionSelector(state) if state.selected_session_id.as_deref() == Some(opened_id.as_str()) && matches!(&state.mode, SessionPanelMode::Browse))
        {
            self.dock = Dock::Composer;
        }
        commands
    }

    fn on_session_state_response(
        &mut self,
        session_id: &SessionId,
        query: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.latest_state_query != Some(query) {
            return Vec::new();
        }
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = None;
        }
        match response.parse_session_state() {
            Ok(state) if state.session_id.as_str() == session_id.as_str() => {
                self.apply_session_state(&state, None, false)
            }
            Ok(_) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("state response does not match requested session {session_id}"),
                );
                Vec::new()
            }
            Err(error) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("failed to fetch state for {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    fn on_session_presentation_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let parsed = response.parse_session_presentation();
        // Captured before the refresh closure so the receipt can be reconciled
        // after the view borrow is released (dropped-event recovery).
        let receipt = parsed
            .as_ref()
            .ok()
            .and_then(|presentation| {
                (presentation.session_id == *session_id)
                    .then(|| presentation.steer_progress.clone())
            })
            .flatten();
        let refresh = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            view.presentation_pending = false;
            let refresh = view.presentation_refresh_pending;
            view.presentation_refresh_pending = false;
            match parsed {
                Ok(presentation) if presentation.session_id == *session_id => {
                    view.presentation = Some(presentation);
                }
                Ok(_) => self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "session.presentation response does not match requested session {session_id}"
                    ),
                ),
                Err(error) => self.notice(
                    NoticeLevel::Warning,
                    format!("failed to fetch presentation for {session_id}: {error}"),
                ),
            }
            refresh
        };
        if let Some(progress) = receipt {
            self.reconcile_steer_receipt(
                session_id,
                &progress.loop_id,
                progress.request_index,
                progress.applied_count,
            );
        }
        if refresh {
            self.request_session_presentation(session_id)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    }

    fn apply_session_state(
        &mut self,
        state: &SessionStateWire,
        event_loop_id: Option<&String>,
        from_event: bool,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(&state.session_id)
            || self.sessions.pending_deletes.contains(&state.session_id)
        {
            return Vec::new();
        }
        let show_unsupported = {
            let Some(view) = self.sessions.known.get_mut(&state.session_id) else {
                return Vec::new();
            };
            if event_loop_id.is_some_and(|event_loop_id| {
                view.retired_loop
                    .as_ref()
                    .is_some_and(|retired| retired.loop_id == event_loop_id.as_str())
            }) {
                return Vec::new();
            }
            if event_loop_id.is_some_and(|event_loop_id| {
                view.live
                    .as_ref()
                    .is_none_or(|live| live.reference.is_none())
                    && Self::is_prior_loop(view, event_loop_id)
            }) {
                return Vec::new();
            }
            if event_loop_id.is_some_and(|event_loop_id| {
                view.live
                    .as_ref()
                    .and_then(|live| live.reference.as_ref())
                    .is_some_and(|reference| reference.loop_id.as_str() != event_loop_id.as_str())
            }) {
                return Vec::new();
            }
            if from_event
                && event_loop_id.is_none()
                && state.status == SessionStatusWire::Idle
                && view
                    .live
                    .as_ref()
                    .is_some_and(|live| live.reference.is_some())
            {
                return Vec::new();
            }
            if let Some(reference) = view.live.as_ref().and_then(|live| live.reference.as_ref()) {
                if state.active_loop.as_ref().is_some_and(|loop_state| {
                    loop_state.loop_id.as_str() != reference.loop_id.as_str()
                }) || (state.status != SessionStatusWire::Idle
                    && state.status != SessionStatusWire::Blocked
                    && state.active_loop.is_none())
                {
                    return Vec::new();
                }
            }
            if view.live.is_none()
                && state.status != SessionStatusWire::Idle
                && event_loop_id.is_some_and(|event_loop_id| {
                    view.last_result.as_ref().is_some_and(|result| {
                        result.turn.loop_id.as_str() == event_loop_id.as_str()
                    })
                })
            {
                return Vec::new();
            }
            let was_waiting = view
                .state
                .as_ref()
                .is_some_and(|old| old.status == SessionStatusWire::WaitingForInput);
            let mut state = state.clone();
            if view.unsaved_loop.is_some() && state.status != SessionStatusWire::Blocked {
                state.status = SessionStatusWire::Blocked;
                state.block_reason = Some(crate::protocol::SessionBlockReasonWire::Persistence);
            }
            if view.live.is_none() && state.status != SessionStatusWire::Idle {
                if let Some(loop_state) = state.active_loop.as_ref() {
                    let mut live = LiveLoop::new(LocalSubmissionId(u64::MAX), String::new());
                    live.reference = Some(TurnRef {
                        session_id: state.session_id.clone(),
                        loop_id: loop_state.loop_id.clone(),
                    });
                    live.event_gap = true;
                    view.live = Some(live);
                    view.event_gap = true;
                }
            }
            if state.status == SessionStatusWire::Idle
                && view.unsaved_loop.is_none()
                && view
                    .live
                    .as_ref()
                    .is_some_and(|live| live.local_submission == LocalSubmissionId(u64::MAX))
            {
                view.live = None;
            }
            view.state = Some(state.clone());
            view.close_verification_unknown = false;
            !was_waiting && state.status == SessionStatusWire::WaitingForInput
        };
        if show_unsupported {
            self.sticky_notice(NoticeLevel::Warning, UNSUPPORTED_INTERACTION_NOTICE);
        }
        Vec::new()
    }

    fn on_history_response(
        &mut self,
        session_id: &SessionId,
        requested_offset: usize,
        gap_revision: Option<u64>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let page = match response.parse_history() {
            Ok(page) => page,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("malformed history for {session_id}: {error}"),
                );
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    Self::mark_history_unconfirmed(view);
                }
                return Vec::new();
            }
        };
        self.continue_history_chain(session_id, requested_offset, gap_revision, &page)
    }

    fn continue_history_chain(
        &mut self,
        session_id: &SessionId,
        requested_offset: usize,
        gap_revision: Option<u64>,
        page: &HistoryPageWire,
    ) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }

        // 1. Conflict detection & idempotent repetition filtering:
        // Any incoming item with index existing locally must exactly match known item; otherwise conflict!
        let has_conflict = self.sessions.known.get(session_id).is_some_and(|view| {
            page.items.iter().any(|incoming| {
                view.transcript
                    .items
                    .iter()
                    .any(|known| known.index == incoming.index && known.item != incoming.item)
            })
        });

        if has_conflict {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                Self::mark_history_unconfirmed(view);
            }
            self.notice(
                NoticeLevel::Error,
                format!("history for {session_id} changed at an existing item index"),
            );
            return Vec::new();
        }

        let loaded_count = self
            .sessions
            .known
            .get(session_id)
            .map(|view| view.transcript.loaded_count)
            .unwrap_or(0);

        // 2. Page contiguity: the first new item must begin at the requested
        // offset, and items within a page must advance without gaps.
        let page_contiguous = page
            .items
            .first()
            .is_none_or(|first| first.index == requested_offset)
            && page
                .items
                .windows(2)
                .all(|w| w[0].index.checked_add(1) == Some(w[1].index));

        if !page_contiguous {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                Self::mark_history_unconfirmed(view);
            }
            self.notice(
                NoticeLevel::Warning,
                format!("history for {session_id} is not contiguous at offset {requested_offset}"),
            );
            return Vec::new();
        }

        // Filter truly new items that advance our local loaded_count
        let new_items: Vec<_> = page
            .items
            .iter()
            .filter(|item| item.index >= loaded_count)
            .cloned()
            .collect();

        // Check if new_items contiguously advance from loaded_count
        let local_advances_contiguously = new_items
            .iter()
            .enumerate()
            .all(|(pos, item)| loaded_count.checked_add(pos) == Some(item.index));

        if !new_items.is_empty() && !local_advances_contiguously {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                Self::mark_history_unconfirmed(view);
            }
            self.notice(
                NoticeLevel::Warning,
                format!("history for {session_id} is not contiguous at offset {loaded_count}"),
            );
            return Vec::new();
        }

        let Some(new_loaded_count) = loaded_count.checked_add(new_items.len()) else {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                Self::mark_history_unconfirmed(view);
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "history for {session_id} did not advance its offset from {requested_offset}"
                ),
            );
            return Vec::new();
        };
        let next_valid = match page.next_offset {
            Some(next) => next > requested_offset && next == new_loaded_count && next <= page.total,
            None => new_loaded_count == page.total,
        };

        if !next_valid {
            let reason = format!(
                "history for {session_id} did not advance its offset from {requested_offset}"
            );
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                Self::mark_history_unconfirmed(view);
            }
            self.notice(NoticeLevel::Warning, reason);
            return Vec::new();
        }

        let next = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            merge_history_items(view, &new_items);
            view.transcript.total = page.total;
            view.transcript.loaded_count += new_items.len();
            view.transcript.next_offset = page.next_offset;

            let next = if let Some(next_offset) = page.next_offset {
                NextChain::Page {
                    offset: next_offset,
                }
            } else {
                view.transcript.complete = true;
                view.loading = false;

                let live_loop_id = view
                    .live
                    .as_ref()
                    .and_then(|live| live.reference.as_ref())
                    .map(|r| r.loop_id.clone());

                let raw_items_contain_loop = match &live_loop_id {
                    Some(id) => view
                        .transcript
                        .items
                        .iter()
                        .any(|item| item.item.loop_id() == Some(id.as_str())),
                    None => false,
                };

                let loop_contained_in_history = match &live_loop_id {
                    Some(id) => view.transcript.blocks.iter().any(|b| match b {
                        TranscriptBlock::User(u) => !u.pending && u.loop_id.as_deref() == Some(id),
                        TranscriptBlock::Assistant(a) => a.loop_id.as_str() == id.as_str(),
                        TranscriptBlock::Tool(t) => t.loop_id.as_str() == id.as_str(),
                        _ => false,
                    }),
                    None => false,
                };

                // Clear event_gap ONLY IF:
                // - all pages complete (we are in the else branch)
                // - no unsaved loop
                // - gap revision matches or fully loaded
                // - for live turn: valid same-turn persisted AND raw items contain loop
                // - for no live turn: view.live.is_none()
                let same_turn_persisted = match &live_loop_id {
                    Some(id) => {
                        view.last_result.as_ref().is_some_and(|r| {
                            r.persistence == TurnPersistenceWire::Persisted && r.turn.loop_id == *id
                        }) || view
                            .live
                            .as_ref()
                            .and_then(|l| l.last_result.as_ref())
                            .is_some_and(|r| {
                                r.persistence == TurnPersistenceWire::Persisted
                                    && r.turn.loop_id == *id
                            })
                    }
                    None => true,
                };

                let turn_satisfied = if live_loop_id.is_some() {
                    same_turn_persisted && raw_items_contain_loop
                } else {
                    view.live.is_none()
                };

                let gap_rev_matches =
                    gap_revision.is_some_and(|revision| revision == view.gap_revision);
                let needs_gap_reconcile = view.unsaved_loop.is_none()
                    && view.event_gap
                    && !gap_rev_matches
                    && view
                        .live
                        .as_ref()
                        .is_none_or(|live| live.last_result.is_some());

                if view.unsaved_loop.is_none()
                    && view.event_gap
                    && turn_satisfied
                    && gap_rev_matches
                {
                    view.event_gap = false;
                }

                view.reconcile_inflight = false;

                // Mark steer states based on history
                let loop_id = live_loop_id.as_deref();
                let mut persisted_steers: Vec<String> = view
                    .transcript
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        TranscriptBlock::User(user)
                            if user.kind == UserMessageKindWire::Steering
                                && loop_id.is_some_and(|loop_id| {
                                    user.loop_id.as_deref() == Some(loop_id)
                                }) =>
                        {
                            Some(user.text.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let blocked = view.is_blocked();
                let persistence_unconfirmed = view.unsaved_loop.is_some();
                // Terminal-only reconciliation: only a FINISHED loop (its
                // result observed) may downgrade an in-flight accepted steer;
                // a mid-loop empty/stale History page never marks NotRecorded
                // and a receipt race is never downgraded before the loop ends.
                let terminal = view
                    .live
                    .as_ref()
                    .is_some_and(|live| live.last_result.is_some());
                if let Some(live) = view.live.as_mut() {
                    for steer in &mut live.pending_steers {
                        if matches!(
                            steer.state,
                            PendingSteerState::Sending
                                | PendingSteerState::Queued
                                | PendingSteerState::Unconfirmed
                        ) {
                            if let Some(position) =
                                persisted_steers.iter().position(|text| text == &steer.text)
                            {
                                persisted_steers.remove(position);
                                steer.state = if persistence_unconfirmed {
                                    PendingSteerState::Unconfirmed
                                } else {
                                    PendingSteerState::Persisted
                                };
                            } else if steer.state == PendingSteerState::Queued && terminal {
                                steer.state = if blocked {
                                    PendingSteerState::Unconfirmed
                                } else {
                                    PendingSteerState::NotRecorded
                                };
                            } else if steer.state == PendingSteerState::Sending && terminal {
                                steer.state = PendingSteerState::Unconfirmed;
                            }
                        }
                    }
                }
                // Durable history replaces an applied steer card exactly once
                // (duplicates consumed one-to-one by text).
                view.applied_steers.retain(|applied| {
                    if let Some(position) = persisted_steers
                        .iter()
                        .position(|text| text == &applied.text)
                    {
                        persisted_steers.remove(position);
                        false
                    } else {
                        true
                    }
                });

                // If live turn has finished and is contained in history, take it
                if view.unsaved_loop.is_none()
                    && !view.is_blocked()
                    && loop_contained_in_history
                    && view
                        .live
                        .as_ref()
                        .is_some_and(|live| live.last_result.is_some())
                {
                    if let Some(live) = view.live.take() {
                        let current_loop = live
                            .reference
                            .as_ref()
                            .map(|r| r.loop_id.clone())
                            .unwrap_or_default();
                        for steer in live.pending_steers {
                            view.completed_steers.push(
                                crate::state::session::CompletedSteerNotice {
                                    session_id: session_id.clone(),
                                    loop_id: current_loop.clone(),
                                    local_id: steer.local_id,
                                    text: steer.text,
                                    state: steer.state,
                                    accepted_at: steer.accepted_at,
                                },
                            );
                        }
                    }
                }

                if needs_gap_reconcile {
                    // A dropped event advanced the fence while this page was
                    // in flight. Keep the merged page, but fetch the tail
                    // again under the newer revision before releasing it.
                    view.needs_post_wait_history = false;
                    view.loading = true;
                    view.reconcile_inflight = true;
                    NextChain::Reconcile {
                        offset: view.transcript.loaded_count,
                    }
                } else if view.needs_post_wait_history {
                    // Scenario B: We had an in-flight history when turn.wait completed.
                    // Now that history has completed, if the loop is not yet contained in history,
                    // we perform exactly ONE post-wait history fetch.
                    view.needs_post_wait_history = false;
                    if !loop_contained_in_history && view.live.is_some() {
                        view.loading = true;
                        view.reconcile_inflight = true;
                        NextChain::Reconcile {
                            offset: view.transcript.loaded_count,
                        }
                    } else {
                        NextChain::Done
                    }
                } else if !loop_contained_in_history
                    && view.live.as_ref().is_some_and(|l| l.last_result.is_some())
                {
                    // If a post-wait fetch already happened and the loop is still not in history,
                    // do NOT retry infinitely. Emit a warning and retain live/gap.
                    NextChain::LoopNotContained(live_loop_id.unwrap_or_default())
                } else {
                    NextChain::Done
                }
            };
            view.recompute_usage_projection();
            next
        };

        match next {
            NextChain::Page { offset } | NextChain::Reconcile { offset } => {
                vec![self.request_history(session_id, offset, 20)]
            }
            NextChain::LoopNotContained(loop_id) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "history sync warning: loop {loop_id} not contained in history response"
                    ),
                );
                Vec::new()
            }
            NextChain::Done => Vec::new(),
        }
    }

    fn submit_turn(&mut self, session_id: SessionId, text: String) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        if trimmed.len() > MAX_COMPOSER_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.closing)
        {
            self.notice(
                NoticeLevel::Warning,
                "session is closing; cannot submit a new turn",
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.is_blocked())
        {
            self.notice(
                NoticeLevel::Error,
                "session is blocked; resolve or reset before submitting",
            );
            return Vec::new();
        }
        if self.sessions.known.get(&session_id).is_some_and(|view| {
            view.state
                .as_ref()
                .is_some_and(|state| state.status != SessionStatusWire::Idle)
        }) {
            self.notice(
                NoticeLevel::Warning,
                "session is not idle; cannot submit a new turn",
            );
            return Vec::new();
        }
        let submission = LocalSubmissionId(self.next_submission);
        self.next_submission = self
            .next_submission
            .checked_add(1)
            .expect("submission ids exhausted");
        {
            let Some(view) = self.sessions.known.get_mut(&session_id) else {
                return Vec::new();
            };
            if view.live.is_some() {
                return Vec::new();
            }
            // Keep the previous last_result as a bounded fence until this
            // new submission receives its own loop reference. The UI hides a
            // result that does not belong to the live loop.
            view.last_request = None;
            view.completed_steers.clear();
            view.result_unconfirmed = false;
            if view.config_update.as_ref().is_some_and(|u| {
                u.loop_id.is_some() || u.state == crate::state::session::ConfigUpdateState::Applied
            }) {
                view.config_update = None;
            }
            view.live = Some(LiveLoop {
                reference: None,
                local_submission: submission,
                user_text: trimmed.to_owned(),
                requests: Vec::new(),
                pending_steers: Vec::new(),
                waiting: false,
                cancel_requested: false,
                event_gap: false,
                last_result: None,
            });
            // A fresh loop resumes the local steer queue: receipts reset, and
            // a paused queue may flow again (the user explicitly started it).
            view.steer_queue_paused = false;
            view.steer_receipt = None;
            view.applied_steers.clear();
            view.transcript
                .blocks
                .push(TranscriptBlock::User(UserBlock {
                    index: None,
                    loop_id: None,
                    kind: UserMessageKindWire::Prompt,
                    text: trimmed.to_owned(),
                    pending: true,
                }));
            view.transcript.invalidate();
        }
        vec![self.request(
            RequestKind::SendTurn {
                session_id: session_id.clone(),
                local_submission: submission,
            },
            |id| OutgoingRequest::send_turn(id, &session_id, trimmed),
        )]
    }

    fn cancel_turn(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let reference = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            if view.state.as_ref().is_some_and(|state| {
                matches!(
                    state.status,
                    SessionStatusWire::Finishing | SessionStatusWire::Blocked
                )
            }) {
                return Vec::new();
            }
            if view.live.as_ref().is_some_and(|live| live.waiting) {
                return Vec::new();
            }
            if let Some(live) = view.live.as_mut() {
                live.cancel_requested = true;
                live.reference.clone()
            } else {
                view.state.as_ref().and_then(|state| {
                    state.active_loop.as_ref().map(|loop_state| TurnRef {
                        session_id: session_id.clone(),
                        loop_id: loop_state.loop_id.clone(),
                    })
                })
            }
        };
        match reference {
            Some(turn) => {
                // A cancellation pauses the unsent queue: never auto-send an
                // ambiguous message after an explicit user cancel.
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.steer_queue_paused = true;
                }
                vec![self.request(RequestKind::CancelTurn(turn.clone()), |id| {
                    OutgoingRequest::cancel_turn(id, &turn)
                })]
            }
            None => Vec::new(),
        }
    }

    /// Explicitly reads a retained completion once. A repeated request for
    /// the same turn is ignored while one wait is already registered; the
    /// response reducer also ignores an identical completion, so this cannot
    /// duplicate history or live cards.
    fn refresh_turn(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        let turn = self.sessions.known.get(session_id).and_then(|view| {
            view.unsaved_loop
                .as_ref()
                .map(|unsaved| unsaved.turn.clone())
                .or_else(|| view.live.as_ref().and_then(|live| live.reference.clone()))
                .or_else(|| view.last_result.as_ref().map(|result| result.turn.clone()))
        });
        let Some(turn) = turn else {
            return Vec::new();
        };
        if self
            .pending_requests
            .values()
            .any(|kind| matches!(kind, RequestKind::WaitTurn(pending) if pending == &turn))
        {
            return Vec::new();
        }
        vec![self.request(RequestKind::WaitTurn(turn.clone()), |id| {
            OutgoingRequest::wait_turn(id, &turn)
        })]
    }

    fn on_send_response(
        &mut self,
        session_id: &SessionId,
        local_submission: LocalSubmissionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        enum Plan {
            Wait {
                turn: TurnRef,
                cancel: bool,
            },
            Failed {
                recovered: Option<String>,
                error: RpcResponseError,
            },
            Mismatch,
        }
        let plan = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            let pending_user_text = view.transcript.blocks.iter().find_map(|block| match block {
                TranscriptBlock::User(card) if card.pending => Some(card.text.clone()),
                _ => None,
            });
            let Some(live) = view.live.as_mut() else {
                return Vec::new();
            };
            let parsed = response.parse_turn_send();
            if live.local_submission == LocalSubmissionId(u64::MAX)
                && parsed.as_ref().is_ok_and(|result| {
                    result.turn.session_id.as_str() == session_id.as_str()
                        && live
                            .reference
                            .as_ref()
                            .is_some_and(|reference| reference == &result.turn)
                })
            {
                live.local_submission = local_submission;
                if live.user_text.is_empty() {
                    live.user_text = pending_user_text.unwrap_or_default();
                }
            }
            if live.local_submission != local_submission {
                return Vec::new();
            }
            match parsed {
                Ok(result) => {
                    if result.turn.session_id.as_str() != session_id.as_str()
                        || live
                            .reference
                            .as_ref()
                            .is_some_and(|reference| reference != &result.turn)
                    {
                        Plan::Mismatch
                    } else {
                        if view
                            .last_result
                            .as_ref()
                            .is_some_and(|previous| previous.turn != result.turn)
                        {
                            view.last_result = None;
                        }
                        view.result_unconfirmed = false;
                        live.reference = Some(result.turn.clone());
                        view.live_user_timestamp = result.accepted_at.clone();
                        view.live_user_time_accepted = true;
                        let pending_user =
                            view.transcript
                                .blocks
                                .iter_mut()
                                .find_map(|block| match block {
                                    TranscriptBlock::User(card) if card.pending => Some(card),
                                    _ => None,
                                });
                        if let Some(card) = pending_user {
                            card.loop_id = Some(result.turn.loop_id.clone());
                        }
                        view.transcript.invalidate();
                        Plan::Wait {
                            turn: result.turn,
                            cancel: live.cancel_requested,
                        }
                    }
                }
                Err(error) => {
                    let is_blocked_err = matches!(&error, crate::protocol::RpcResponseError::Agent(err) if err.code == -32004);
                    // A fresh-turn handoff whose loop the Agent ALREADY started
                    // (a TurnStarted event bound the reference) is a PROVEN
                    // accept: keep the running loop and wait; never abandon an
                    // accepted turn nor retry its message.
                    let is_handoff_send = view.steer_queue.iter().any(|item| item.handoff);
                    if is_handoff_send
                        && view
                            .live
                            .as_ref()
                            .is_some_and(|live| live.reference.is_some())
                    {
                        let turn = view
                            .live
                            .as_ref()
                            .and_then(|live| live.reference.as_ref())
                            .cloned()
                            .expect("checked above");
                        let cancel = view.live.as_ref().is_some_and(|live| live.cancel_requested);
                        view.transcript.blocks.retain(
                            |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                        );
                        view.transcript.invalidate();
                        // The started loop owns the text: drop the queue entry
                        // exactly like the accepted-Wait path.
                        view.steer_queue.retain(|item| !item.handoff);
                        Plan::Wait { turn, cancel }
                    } else {
                        let recovered = if view.is_blocked() || is_blocked_err {
                            view.live.as_ref().map(|live| live.user_text.clone())
                        } else {
                            view.live.take().map(|live| live.user_text)
                        };
                        view.transcript.blocks.retain(
                            |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                        );
                        view.transcript.invalidate();
                        Plan::Failed { recovered, error }
                    }
                }
            }
        };
        match plan {
            Plan::Wait { turn, cancel } => {
                if !self.can_send_requests()
                    && !matches!(self.connection, ConnectionState::ShuttingDown)
                {
                    return Vec::new();
                }
                // A fresh-turn handoff is now an owned, accepted turn: drop the
                // queue entry it represented (durable history owns it).
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.steer_queue.retain(|item| !item.handoff);
                }
                let mut commands = vec![self.request(RequestKind::WaitTurn(turn.clone()), |id| {
                    OutgoingRequest::wait_turn(id, &turn)
                })];
                if cancel {
                    commands.push(self.request(RequestKind::CancelTurn(turn.clone()), |id| {
                        OutgoingRequest::cancel_turn(id, &turn)
                    }));
                }
                commands
            }
            Plan::Failed { recovered, error } => {
                // A handoff item already owns its queued text: restoring a
                // second copy into the composer would duplicate it on the next
                // Enter. Plain (non-queue) submissions keep the editor restore.
                let is_handoff_send = self
                    .sessions
                    .known
                    .get(session_id)
                    .is_some_and(|view| view.steer_queue.iter().any(|item| item.handoff));
                // A response that reached the Agent but cannot be decoded
                // (Parse/Malformed) is UNCERTAIN: never auto-resend it.
                let uncertain = is_handoff_send
                    && matches!(
                        &error,
                        crate::protocol::RpcResponseError::Parse(_)
                            | crate::protocol::RpcResponseError::Malformed
                    );
                if self.sessions.active.as_ref() == Some(session_id) && !is_handoff_send {
                    if let Some(text) =
                        recovered.filter(|_| self.composer.content().trim().is_empty())
                    {
                        self.composer.set_text(&text);
                    }
                }
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    for item in &mut view.steer_queue {
                        if item.handoff {
                            item.handoff = false;
                            if uncertain {
                                item.state = SteerQueueState::Unconfirmed;
                            }
                        }
                    }
                    view.steer_queue_paused = true;
                }
                let message = if uncertain {
                    "turn send response could not be decoded; the queued steering is unconfirmed and will not be resubmitted automatically".to_owned()
                } else {
                    format!("turn send failed: {error}")
                };
                self.notice(NoticeLevel::Warning, message);
                Vec::new()
            }
            Plan::Mismatch => {
                self.connection_terminated("turn.send response does not match the live loop");
                Vec::new()
            }
        }
    }

    fn on_wait_response(&mut self, turn: TurnRef, response: &RpcResponse) -> Vec<AppCommand> {
        let parsed = response.parse_turn_wait();
        let mismatched_turn = parsed.as_ref().is_ok_and(|result| result.turn != turn);
        let (persistence_failed, result, duplicate) = {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return Vec::new();
            };
            if view
                .live
                .as_ref()
                .is_some_and(|live| live.reference.as_ref() != Some(&turn))
            {
                return Vec::new();
            }
            let old_result = view.last_result.clone();
            let live_matches = view
                .live
                .as_ref()
                .is_some_and(|live| live.reference.as_ref() == Some(&turn));
            match &parsed {
                Ok(_) if mismatched_turn => {
                    if live_matches {
                        let live = view.live.as_mut().expect("matching live turn exists");
                        live.waiting = true;
                    }
                    (false, None, false)
                }
                Ok(result) => {
                    let failed = result.persistence == TurnPersistenceWire::Failed;
                    let duplicate = old_result.as_ref() == Some(result);
                    if live_matches {
                        let live = view.live.as_mut().expect("matching live turn exists");
                        if !duplicate {
                            live.waiting = true;
                            live.last_result = Some(result.clone());
                        }
                    }
                    (failed, Some(result.clone()), duplicate)
                }
                Err(_) => {
                    if live_matches {
                        let live = view.live.as_mut().expect("matching live turn exists");
                        live.waiting = true;
                    }
                    (false, None, false)
                }
            }
        };
        if let Some(result) = result.as_ref() {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.last_result = Some(result.clone());
                view.recompute_usage_projection();
            }
        }

        if result.is_none() {
            let message = if mismatched_turn {
                "malformed turn.wait result (turn reference does not match request); result/save unconfirmed".to_owned()
            } else {
                match parsed {
                    Err(RpcResponseError::Agent(error)) => {
                        format!("turn wait failed ({error}); result/save unconfirmed")
                    }
                    Err(RpcResponseError::Parse(error)) => {
                        format!("malformed turn.wait result ({error}); result/save unconfirmed")
                    }
                    Err(RpcResponseError::Malformed) => {
                        "turn.wait response has no payload; result/save unconfirmed".to_owned()
                    }
                    Ok(_) => unreachable!(),
                }
            };
            self.notice(NoticeLevel::Warning, message);
            return Vec::new();
        }

        if persistence_failed {
            if !duplicate {
                self.notice(
                    NoticeLevel::Error,
                    "Turn completed but persistence failed; session is blocked.",
                );
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    if let Some(state) = view.state.as_mut() {
                        state.status = SessionStatusWire::Blocked;
                        state.active_loop = None;
                        state.block_reason =
                            Some(crate::protocol::SessionBlockReasonWire::Persistence);
                    } else {
                        view.state = Some(SessionStateWire {
                            session_id: turn.session_id.clone(),
                            status: SessionStatusWire::Blocked,
                            active_loop: None,
                            block_reason: Some(
                                crate::protocol::SessionBlockReasonWire::Persistence,
                            ),
                        });
                    }
                    if let Some(live) = view.live.as_ref() {
                        let user_text = live.user_text.clone();
                        let requests = live.requests.clone();
                        let event_gap = live.event_gap;
                        view.unsaved_loop = Some(UnsavedLoop {
                            turn: turn.clone(),
                            user_text,
                            requests,
                            result: result.clone(),
                            event_gap,
                        });
                    }
                    Self::mark_pending_steers_unconfirmed(view);
                    view.recompute_usage_projection();
                }
            }
            return Vec::new();
        }

        if duplicate {
            return Vec::new();
        }
        let mut commands = self.reconcile_after_wait(&turn);
        if let Some(command) = self.request_session_presentation(&turn.session_id) {
            commands.push(command);
        }
        commands
    }

    fn reconcile_after_wait(&mut self, turn: &TurnRef) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        // Closed or not-loaded session view must not issue session.state or session.history.
        // Wait itself has already recorded the result and kept live temporarily visible.
        if let Some(view) = self.sessions.known.get(&turn.session_id) {
            if !view.info.loaded || view.closing {
                return Vec::new();
            }
        } else {
            return Vec::new();
        }
        let mut commands = vec![self.request_session_state(&turn.session_id)];
        let pending_history = self.pending_history(&turn.session_id);
        let fetch = {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return commands;
            };
            if view.loading || pending_history {
                // If a history fetch is already in flight, flag that a post-wait
                // reconcile is required once the in-flight fetch completes (spec scenario B).
                view.needs_post_wait_history = true;
                false
            } else {
                view.loading = true;
                view.reconcile_inflight = true;
                true
            }
        };
        if fetch {
            let offset = self
                .sessions
                .known
                .get(&turn.session_id)
                .map(|view| view.transcript.loaded_count)
                .unwrap_or(0);
            commands.push(self.request_history(&turn.session_id, offset, 20));
        }
        commands
    }

    fn on_steer_response(
        &mut self,
        session_id: &SessionId,
        loop_id: &str,
        steer_id: u64,
        steer_text: &str,
        editor_revision: Option<u64>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let composer_text = self.composer.content().trim().to_owned();
        let composer_session = self.sessions.active.as_ref() == Some(session_id);
        let composer_revision_matches =
            editor_revision.is_some_and(|revision| revision == self.composer.editor_revision());

        let parsed = response.parse_steer();
        let is_ok = parsed.as_ref().is_ok_and(|res| res.ok);

        if is_ok {
            if composer_session && composer_revision_matches && composer_text == steer_text {
                self.composer.submit_pushed(&composer_text);
                self.composer.clear();
            }
            let mut acked_live = false;
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                let history_proves_not_recorded =
                    Self::history_proves_steer_not_recorded(view, loop_id, steer_text);
                let is_live_matching = view
                    .live
                    .as_ref()
                    .and_then(|l| l.reference.as_ref())
                    .is_some_and(|r| r.loop_id.as_str() == loop_id);

                if is_live_matching {
                    if let Some(live) = view.live.as_mut() {
                        if let Some(steer) = live
                            .pending_steers
                            .iter_mut()
                            .find(|s| s.local_id == steer_id)
                        {
                            if let PendingSteerState::Sending = steer.state {
                                steer.state = PendingSteerState::Queued;
                                steer.accepted_at = parsed
                                    .as_ref()
                                    .ok()
                                    .and_then(|result| result.accepted_at.clone());
                                steer.steer_index =
                                    parsed.as_ref().ok().and_then(|result| result.steer_index);
                                acked_live = true;
                            } else if steer.state == PendingSteerState::Unconfirmed
                                && history_proves_not_recorded
                            {
                                steer.state = PendingSteerState::NotRecorded;
                            }
                        }
                    }
                } else if let Some(steer) = view
                    .completed_steers
                    .iter_mut()
                    .find(|s| s.loop_id == loop_id && s.local_id == steer_id)
                {
                    if steer.state == PendingSteerState::Sending {
                        steer.state = PendingSteerState::Queued;
                        steer.accepted_at = parsed
                            .as_ref()
                            .ok()
                            .and_then(|result| result.accepted_at.clone());
                    } else if steer.state == PendingSteerState::Unconfirmed
                        && history_proves_not_recorded
                    {
                        // A complete persisted History with no matching item
                        // is authoritative: a late ok only confirms the
                        // request was accepted, not that it was recorded.
                        steer.state = PendingSteerState::NotRecorded;
                    }
                }
            }
            // The ACK may now pair against an already-observed receipt
            // (progress can race ahead of the ACK); reconcile by identity.
            if acked_live {
                self.try_apply_steer_receipts(session_id);
            }
            return Vec::new();
        }

        // Steer rejected or failed. Decide first whether the rejection is
        // DEFINITIVE (typed agent error / decoded ok:false) or AMBIGUOUS (the
        // frame could not be decoded and the agent may still have accepted it).
        let definitive_rejection = !matches!(
            parsed,
            Err(RpcResponseError::Parse(_) | RpcResponseError::Malformed)
        );
        let (queue_full, message) = match parsed {
            Ok(_) => (false, "agent rejected the steering request".to_owned()),
            Err(RpcResponseError::Agent(err)) => {
                let queue_full = err.code == crate::protocol::STEER_QUEUE_FULL;
                (queue_full, err.to_string())
            }
            Err(err) => (false, err.to_string()),
        };

        // Definite -> remove it from in-flight and restore the exact message
        // into the unsent queue (FIFO front, paused); ambiguous -> keep the
        // entry Unconfirmed, paused, and never auto-resend or copy it into the
        // unsent queue.
        if definitive_rejection {
            self.remove_pending_steer(session_id, loop_id, steer_id);
            self.pause_steer_queue(session_id);
            self.restore_steer_unsent(session_id, steer_id, steer_text, editor_revision);
            if self.sessions.active.as_ref() == Some(session_id) && self.composer.is_empty() {
                self.composer.set_text(steer_text);
            }
        } else {
            self.pause_steer_queue(session_id);
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                for steer in view
                    .live
                    .as_mut()
                    .into_iter()
                    .flat_map(|live| live.pending_steers.iter_mut())
                {
                    if steer.local_id == steer_id
                        && matches!(
                            steer.state,
                            PendingSteerState::Sending | PendingSteerState::Queued
                        )
                    {
                        steer.state = PendingSteerState::Unconfirmed;
                    }
                }
            }
        }
        if queue_full {
            self.notice(
                NoticeLevel::Warning,
                "Steering queue is full; cannot queue more steers.",
            );
        } else {
            self.notice(
                NoticeLevel::Warning,
                format!("turn.steer failed: {message}"),
            );
        }
        Vec::new()
    }

    /// Removes an in-flight steer from the live or completed registry.
    fn remove_pending_steer(&mut self, session_id: &SessionId, loop_id: &str, steer_id: u64) {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        let is_live_matching = view
            .live
            .as_ref()
            .and_then(|l| l.reference.as_ref())
            .is_some_and(|r| r.loop_id.as_str() == loop_id);
        if is_live_matching {
            if let Some(live) = view.live.as_mut() {
                live.pending_steers.retain(|s| s.local_id != steer_id);
            }
        } else {
            view.completed_steers
                .retain(|s| !(s.loop_id == loop_id && s.local_id == steer_id));
        }
    }

    /// Restores a definitely-not-accepted steer into the local unsent queue at
    /// the FIFO front, preserving its local id and text so nothing is dropped
    /// regardless of the current editor content or active session.
    fn restore_steer_unsent(
        &mut self,
        session_id: &SessionId,
        steer_id: u64,
        text: &str,
        editor_revision: Option<u64>,
    ) {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        if view
            .steer_queue
            .iter()
            .any(|item| item.local_id == steer_id)
        {
            return;
        }
        view.steer_queue.insert(
            0,
            crate::state::turn::SteerQueueItem {
                local_id: steer_id,
                text: text.to_owned(),
                state: crate::state::turn::SteerQueueState::Unsent,
                editor_revision,
                handoff: false,
            },
        );
    }

    fn pause_steer_queue(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.steer_queue_paused = true;
        }
    }

    fn clear_steer_handoffs(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            for item in &mut view.steer_queue {
                item.handoff = false;
            }
        }
    }

    fn on_cancel_response(&mut self, response: &RpcResponse) -> Vec<AppCommand> {
        if let Err(error) = response.parse_cancel() {
            self.notice(NoticeLevel::Warning, format!("turn cancel failed: {error}"));
        }
        Vec::new()
    }

    fn on_update_session_response(
        &mut self,
        session_id: SessionId,
        target_loop_id: Option<String>,
        model: Option<String>,
        reasoning: Option<Reasoning>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let mut refresh_presentation = false;
        match response.parse_session_update() {
            Ok(result) => {
                let active_revision = result.active_revision;
                let session = result.session;
                if session.session_id != session_id {
                    self.notice(
                        NoticeLevel::Warning,
                        format!(
                            "session.update response does not match requested session {session_id}"
                        ),
                    );
                    return Vec::new();
                }
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    // Session.update successful SessionInfo is always the durable authority for the session
                    // (at most one update in-flight per session) and must not be discarded because a loop finished.
                    view.info = session.clone();

                    let current_live_loop = view
                        .live
                        .as_ref()
                        .and_then(|l| l.reference.as_ref().map(|r| &r.loop_id));
                    let is_different_new_loop = match (&target_loop_id, current_live_loop) {
                        (Some(t_loop), Some(c_loop)) => t_loop != c_loop,
                        _ => false,
                    };

                    // Only actual-request applied evidence requires the same loop.
                    // When the loop has already finished, it becomes SavedNextTurn or reflects already observed evidence.
                    // Old responses across loops must not retag new requests.
                    if !is_different_new_loop {
                        let applied = active_revision.is_some_and(|revision| {
                            view.last_request.as_ref().is_some_and(|request| {
                                request.revision == revision
                                    && target_loop_id
                                        .as_ref()
                                        .is_none_or(|tl| request.loop_id.as_ref() == Some(tl))
                                    && request.model == session.model
                                    && request.reasoning == session.reasoning
                            })
                        });

                        view.config_update = Some(crate::state::session::PendingConfigUpdate {
                            loop_id: target_loop_id,
                            model,
                            reasoning,
                            revision: active_revision,
                            state: if applied {
                                crate::state::session::ConfigUpdateState::Applied
                            } else if view.live.is_none() {
                                crate::state::session::ConfigUpdateState::SavedNextTurn
                            } else if active_revision.is_some() {
                                crate::state::session::ConfigUpdateState::WaitingBoundary
                            } else {
                                crate::state::session::ConfigUpdateState::SavedNextTurn
                            },
                        });
                    }
                }
                self.upsert_session_list(session);
                if self.sessions.active.as_ref() == Some(&session_id)
                    && matches!(
                        &self.dock,
                        Dock::ModelSelector(_) | Dock::ReasoningSelector(_)
                    )
                {
                    self.dock = Dock::Composer;
                }
                if let Some(revision) = active_revision {
                    self.notice(
                        NoticeLevel::Info,
                        format!("Saved · applies at next model request (rev {revision})"),
                    );
                } else if self.sessions.known.get(&session_id).is_some_and(|view| {
                    view.state
                        .as_ref()
                        .is_some_and(|state| state.status != SessionStatusWire::Idle)
                }) {
                    self.notice(
                        NoticeLevel::Info,
                        "Saved for next turn; no active revision was returned.",
                    );
                } else {
                    self.notice(NoticeLevel::Info, "Updated for next turn");
                }
                refresh_presentation = true;
            }
            Err(error) => {
                let message = error.to_string();
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    let current_loop_id = view
                        .live
                        .as_ref()
                        .and_then(|l| l.reference.as_ref().map(|r| r.loop_id.clone()));
                    view.config_update = Some(crate::state::session::PendingConfigUpdate {
                        loop_id: current_loop_id,
                        model: model.clone(),
                        reasoning,
                        revision: None,
                        state: crate::state::session::ConfigUpdateState::Failed(message.clone()),
                    });
                }
                let matches_active = self.sessions.active.as_ref() == Some(&session_id);
                let selector_state = if matches_active {
                    match &mut self.dock {
                        Dock::ModelSelector(state) | Dock::ReasoningSelector(state) => Some(state),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(state) = selector_state {
                    state.submitting = false;
                    state.error = Some(message);
                } else {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("failed to update session {session_id}: {error}"),
                    );
                }
            }
        }
        if refresh_presentation {
            if let Some(command) = self.request_session_presentation(&session_id) {
                return vec![command];
            }
        }
        Vec::new()
    }

    fn on_send_failed(&mut self, id: RequestId, error: RpcError) -> Vec<AppCommand> {
        let kind = match self.pending_requests.remove(&id) {
            Some(kind) => kind,
            None => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("send failure for unknown request id {}", id.0),
                );
                return Vec::new();
            }
        };
        if Self::request_session_id(&kind)
            .is_some_and(|session_id| self.sessions.deleted.contains(session_id))
        {
            return Vec::new();
        }
        let mut commands = Vec::new();
        match kind {
            RequestKind::SendTurn {
                session_id,
                local_submission,
            } => {
                let recovered = {
                    let Some(view) = self.sessions.known.get_mut(&session_id) else {
                        return Vec::new();
                    };
                    let mut recovered = None;
                    let current_submission = view
                        .live
                        .as_ref()
                        .is_some_and(|live| live.local_submission == local_submission);
                    if current_submission {
                        if !view.is_blocked() {
                            if let Some(live) = view.live.take() {
                                recovered = Some(live.user_text);
                            }
                        } else if let Some(live) = view.live.as_ref() {
                            recovered = Some(live.user_text.clone());
                        }
                        view.transcript.blocks.retain(
                            |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                        );
                        view.transcript.invalidate();
                    }
                    recovered
                };
                // A handoff item owns its queued text: only plain (non-queue)
                // submissions restore the editor (a second copy would
                // duplicate it on the next Enter).
                let is_handoff = self
                    .sessions
                    .known
                    .get(&session_id)
                    .is_some_and(|view| view.steer_queue.iter().any(|item| item.handoff));
                if self.sessions.active.as_ref() == Some(&session_id) && !is_handoff {
                    if let Some(text) =
                        recovered.filter(|_| self.composer.content().trim().is_empty())
                    {
                        self.composer.set_text(&text);
                    }
                }
                self.notice(NoticeLevel::Warning, format!("turn send failed: {error}"));
                // The fresh-turn handoff could not be written: keep its queued
                // entry as Unsent, clear the handoff, and PAUSE (definitive
                // pre-write failure, so it may only be deliberately re-sent).
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    for item in &mut view.steer_queue {
                        item.handoff = false;
                    }
                    view.steer_queue_paused = true;
                }
            }
            RequestKind::WaitTurn(turn) => {
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    if view
                        .live
                        .as_ref()
                        .and_then(|live| live.reference.as_ref())
                        .is_some_and(|reference| reference == &turn)
                    {
                        if let Some(live) = view.live.as_mut() {
                            live.waiting = true;
                        }
                        Self::mark_pending_steers_unconfirmed(view);
                    }
                }
                self.clear_steer_handoffs(&turn.session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("turn wait send failed: {error}; result/save unconfirmed"),
                );
            }
            RequestKind::SteerTurn {
                session_id,
                loop_id,
                steer_id,
                text,
                editor_revision,
            } => {
                // A channel/serialization send failure means the steer was
                // NEVER written to the Agent (definitively not accepted):
                // restore it into the local unsent queue (exact FIFO front)
                // as PAUSED so the message is never dropped, whatever the
                // editor currently holds.
                self.remove_pending_steer(&session_id, &loop_id, steer_id);
                self.restore_steer_unsent(&session_id, steer_id, &text, editor_revision);
                self.pause_steer_queue(&session_id);
                self.notice(NoticeLevel::Warning, format!("turn.steer failed: {error}"));
            }
            RequestKind::CancelTurn(_) => {
                self.notice(NoticeLevel::Warning, format!("turn cancel failed: {error}"));
            }
            RequestKind::CloseSession { session_id } => {
                self.mark_close_verification_unknown(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.close failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::CloseVerifyState { session_id } => {
                self.mark_close_verification_unknown(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("close verification failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::DeleteSession { session_id } => {
                self.sessions.pending_deletes.remove(&session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if matches!(&state.mode, SessionPanelMode::ConfirmDelete { .. }) {
                            state.mode = SessionPanelMode::ConfirmDelete {
                                choice: SessionConfirmChoice::Cancel,
                                submitting: false,
                            };
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.delete failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::UpdateSession {
                session_id,
                loop_id,
                model,
                reasoning,
            } => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.update failed for {session_id}: {error}"),
                );
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.config_update = Some(crate::state::session::PendingConfigUpdate {
                        loop_id,
                        model,
                        reasoning,
                        revision: None,
                        state: crate::state::session::ConfigUpdateState::Failed(error.to_string()),
                    });
                }
                if let Dock::ModelSelector(state) | Dock::ReasoningSelector(state) = &mut self.dock
                {
                    state.submitting = false;
                    state.error = Some(error.to_string());
                }
            }
            RequestKind::RenameSession { session_id } => {
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                            *submitting = false;
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.rename failed for {session_id}: {error}"),
                );
            }
            RequestKind::History { session_id, .. } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    Self::mark_history_unconfirmed(view);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("history request failed: {error}"),
                );
            }
            RequestKind::Ping
            | RequestKind::ListModels
            | RequestKind::ListProfiles
            | RequestKind::ListSessions => {
                if self.connection == ConnectionState::ShuttingDown {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("bootstrap request failed during shutdown: {error}"),
                    );
                } else {
                    commands.extend(
                        self.connection_terminated(&format!("bootstrap request failed: {error}")),
                    );
                }
            }
            RequestKind::RefreshSessions { .. } => {
                if let Dock::SessionSelector(state) = &mut self.dock {
                    state.error = Some(format!("refresh failed: {error}"));
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session list refresh failed: {error}"),
                );
            }
            RequestKind::CreateSession { draft } => {
                if let Some(draft_state) = self.draft_matching(draft) {
                    draft_state.submitting = false;
                    draft_state.error = Some(format!("failed to send session.create: {error}"));
                } else {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("create session failed: {error}"),
                    );
                }
            }
            RequestKind::OpenSession {
                session_id,
                previous_retired_loop,
            } => {
                if let Some(retired_loop) = previous_retired_loop {
                    if let Some(view) = self.sessions.known.get_mut(&session_id) {
                        view.retired_loop = Some(retired_loop);
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("open session failed for {session_id}: {error}"),
                );
            }
            RequestKind::SessionState { session_id, query } => {
                let current = self
                    .sessions
                    .known
                    .get(&session_id)
                    .and_then(|view| view.latest_state_query)
                    == Some(query);
                if current {
                    if let Some(view) = self.sessions.known.get_mut(&session_id) {
                        view.latest_state_query = None;
                    }
                    self.notice(
                        NoticeLevel::Warning,
                        format!("state fetch failed for {session_id}: {error}"),
                    );
                }
            }
            RequestKind::SessionPresentation { session_id } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.presentation_pending = false;
                }
                if self.connection != ConnectionState::ShuttingDown {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("session presentation request failed for {session_id}: {error}"),
                    );
                }
            }
            RequestKind::Shutdown => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("shutdown request failed: {error}"),
                );
                commands.push(AppCommand::KillChild);
            }
        }
        commands
    }

    fn on_rpc_event(&mut self, event: RpcEvent) -> Vec<AppCommand> {
        match event {
            RpcEvent::Frame(frame) => self.on_frame(frame),
            RpcEvent::AgentLogLine(line) => {
                self.push_log(line);
                Vec::new()
            }
            RpcEvent::ConnectionClosed => {
                if self.connection == ConnectionState::ShuttingDown {
                    Vec::new()
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
        self.connection_terminated("agent RPC channel closed unexpectedly")
    }

    fn connection_terminated(&mut self, reason: &str) -> Vec<AppCommand> {
        if matches!(self.connection, ConnectionState::Failed(_)) {
            return Vec::new();
        }
        let mut unconfirmed = false;
        self.pending_requests.clear();
        for view in self.sessions.known.values_mut() {
            Self::mark_pending_steers_unconfirmed(view);
            // Transport loss pauses the unsent queue: never auto-send or
            // retry an ambiguous accepted message.
            view.steer_queue_paused = true;
            if view.unsaved_loop.is_none() {
                if let Some(live) = view.live.as_mut() {
                    if live.last_result.is_none() {
                        live.waiting = true;
                        view.result_unconfirmed = true;
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
        self.notice(NoticeLevel::Error, reason.to_owned());
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
        if Self::request_session_id(&kind)
            .is_some_and(|session_id| self.sessions.deleted.contains(session_id))
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
            RequestKind::Ping => {
                match response.parse_ping() {
                    Ok(pong) => {
                        if !is_supported_agent_version(&pong.version) {
                            let msg = format!(
                                "unsupported agent version '{}': minicore-tui requires agent 0.3.x",
                                pong.version
                            );
                            self.notice(NoticeLevel::Error, &msg);
                            self.connection = ConnectionState::Failed(msg);
                            return Vec::new();
                        }
                    }
                    Err(err) => {
                        let msg = format!("agent.ping failed: {err}");
                        self.notice(NoticeLevel::Error, &msg);
                        self.connection = ConnectionState::Failed(msg);
                        return Vec::new();
                    }
                }
                self.bootstrap_progress(BootstrapPart::Ping);
                Vec::new()
            }
            RequestKind::ListModels => match response.parse_models() {
                Ok(result) => {
                    self.catalogs.models = result.models;
                    self.bootstrap_progress(BootstrapPart::Models);
                    Vec::new()
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_MODELS, error),
            },
            RequestKind::ListProfiles => match response.parse_profiles() {
                Ok(result) => {
                    self.catalogs.profiles = result.profiles;
                    self.bootstrap_progress(BootstrapPart::Profiles);
                    Vec::new()
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_PROFILES, error),
            },
            RequestKind::ListSessions => match response.parse_sessions() {
                Ok(result) => {
                    let sessions: Vec<_> = result
                        .sessions
                        .into_iter()
                        .filter(|session| {
                            !self.sessions.deleted.contains(&session.session_id)
                                && !self.sessions.pending_deletes.contains(&session.session_id)
                        })
                        .collect();
                    self.sessions.list = sessions.clone();
                    for session in sessions {
                        let session_id = session.session_id.clone();
                        self.sessions
                            .known
                            .entry(session_id)
                            .or_insert_with(|| SessionView::new(session));
                    }
                    self.bootstrap_progress(BootstrapPart::Sessions);
                    Vec::new()
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_SESSIONS, error),
            },
            RequestKind::RefreshSessions { .. } => self.on_refresh_sessions_response(&response),
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
            RequestKind::History {
                session_id,
                offset,
                gap_revision,
                ..
            } => self.on_history_response(&session_id, offset, gap_revision, &response),
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
            AgentEventWire::ToolFinished { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::InteractionRequested { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::InteractionResolved { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::TurnFinished { data } => Some(data.meta.session_id.as_str()),
            AgentEventWire::Unknown => None,
        };
        if event_session_id.is_some_and(|session_id| {
            self.sessions.deleted.contains(session_id)
                || self.sessions.pending_deletes.contains(session_id)
        }) {
            return Vec::new();
        }
        if let AgentEventWire::SessionOpened { data } = &event {
            if self.sessions.deleted.contains(&data.session.session_id)
                || self
                    .sessions
                    .pending_deletes
                    .contains(&data.session.session_id)
            {
                return Vec::new();
            }
        }
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
                        self.sessions
                            .known
                            .insert(session_id.clone(), SessionView::new(data.session.clone()));
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
                if needs_state
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
                self.apply_session_state(&data.state, data.meta.loop_id.as_ref(), true);
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
        if let Some(session_id) = gap_session {
            commands.extend(self.start_gap_reconcile(&session_id));
        }
        commands
    }

    fn start_gap_reconcile(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.loading
            || view.reconcile_inflight
            || view.live.is_some()
            || view.unsaved_loop.is_some()
        {
            return Vec::new();
        }
        let offset = view.transcript.loaded_count;
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.loading = true;
            view.reconcile_inflight = true;
        }
        vec![self.request_history(session_id, offset, DEFAULT_HISTORY_LIMIT)]
    }

    fn mark_gap(&mut self, meta: &EventMetaWire) {
        if meta.dropped_before == 0 {
            return;
        }
        if self.sessions.deleted.contains(&meta.session_id)
            || self.sessions.pending_deletes.contains(&meta.session_id)
        {
            return;
        }
        if let Some(view) = self.sessions.known.get_mut(&meta.session_id) {
            view.event_gap = true;
            view.gap_revision = view.gap_revision.wrapping_add(1);
            if view.live.as_ref().is_some_and(|live| {
                meta.loop_id.as_ref().is_none_or(|loop_id| {
                    live.reference
                        .as_ref()
                        .is_none_or(|reference| reference.loop_id == *loop_id)
                })
            }) {
                if let Some(live) = view.live.as_mut() {
                    live.event_gap = true;
                }
            }
        }
    }

    fn mark_pending_steers_unconfirmed(view: &mut SessionView) {
        if let Some(live) = view.live.as_mut() {
            for steer in &mut live.pending_steers {
                if matches!(
                    steer.state,
                    PendingSteerState::Sending | PendingSteerState::Queued
                ) {
                    steer.state = PendingSteerState::Unconfirmed;
                }
            }
        }
    }

    fn adopt_turn_started(&mut self, turn: &TurnRef) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        Self::bind_live_turn(view, turn);
    }

    /// Binds the first loop-scoped event to the local pending start. Agent
    /// events may precede the `turn.send` response, so RequestStarted and
    /// OutputDelta must be able to establish the same TurnRef as well.
    fn bind_live_turn(view: &mut SessionView, turn: &TurnRef) -> bool {
        let event_gap = view.event_gap;
        // Check the lifecycle fence before an existing live reference. During
        // close→reopen, the old LiveLoop is intentionally retained until the
        // new open response so the old wait can still be handled, but late
        // old notifications must not mutate that retired view.
        if Self::is_prior_loop(view, &turn.loop_id) && view.retired_loop.is_some() {
            return false;
        }
        if let Some(reference) = view.live.as_ref().and_then(|live| live.reference.as_ref()) {
            return reference == turn;
        }
        if view.live.is_some() && Self::is_prior_loop(view, &turn.loop_id) {
            return false;
        }
        let Some(live) = view.live.as_mut() else {
            return false;
        };
        live.event_gap |= event_gap;
        live.reference = Some(turn.clone());
        let mut changed = false;
        for block in &mut view.transcript.blocks {
            if let TranscriptBlock::User(card) = block {
                if card.pending {
                    card.loop_id = Some(turn.loop_id.clone());
                    changed = true;
                }
            }
        }
        if changed {
            view.transcript.invalidate();
        }
        true
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

    /// Read-only steering receipt from `steer_progress` (real Model.start):
    /// records the highest observed applied count and pairs it against ACK
    /// `steer_index` values (identity, never queue position). Receipts for
    /// other loops (late/stale after a switch) are ignored.
    fn on_steer_progress(&mut self, turn: &TurnRef, request_index: u32, applied_count: u64) {
        {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return;
            };
            let loop_matches = view
                .live
                .as_ref()
                .and_then(|live| live.reference.as_ref())
                .is_some_and(|reference| reference.loop_id == turn.loop_id);
            if !loop_matches {
                return;
            }
            // Monotonic: only a higher count advances the cache, keeping the
            // request_index of the FIRST observation of the current max.
            let grew = view
                .steer_receipt
                .is_none_or(|cached| applied_count > cached.applied_count);
            if grew {
                view.steer_receipt = Some(crate::state::turn::SteerReceiptObserved {
                    request_index,
                    applied_count,
                });
            }
        }
        self.try_apply_steer_receipts(&turn.session_id);
    }

    /// Applies pending accepted entries whose ACK `steer_index` is covered by
    /// the observed receipt. Identity-paired: never inferred by queue position
    /// while an entry is Sending; older Agents (index None) hold conservatively
    /// until terminal History. Applied cards keep the ACK `accepted_at`.
    fn try_apply_steer_receipts(&mut self, session_id: &SessionId) {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        let Some(receipt) = view.steer_receipt else {
            return;
        };
        let (applied, applied_count) = {
            let Some(live) = view.live.as_mut() else {
                return;
            };
            let mut applied = Vec::new();
            let mut remaining = Vec::new();
            let mut applied_count = 0usize;
            for steer in live.pending_steers.drain(..) {
                let covered = matches!(steer.state, PendingSteerState::Queued)
                    && steer
                        .steer_index
                        .is_some_and(|index| index <= receipt.applied_count);
                if covered {
                    applied.push(AppliedSteer {
                        local_id: steer.local_id,
                        text: steer.text,
                        accepted_at: steer.accepted_at,
                        request_index: receipt.request_index,
                    });
                    applied_count += 1;
                } else {
                    remaining.push(steer);
                }
            }
            live.pending_steers = remaining;
            (applied, applied_count)
        };
        if applied_count == 0 {
            return;
        }
        let view = self.sessions.known.get_mut(session_id).expect("view");
        for applied in applied {
            view.applied_steers.push(applied);
        }
    }

    /// Legacy entry point used by presentation recovery: cache the receipt,
    /// then apply whatever the ACK indexes now cover.
    fn reconcile_steer_receipt(
        &mut self,
        session_id: &SessionId,
        loop_id: &str,
        request_index: u32,
        applied_count: u64,
    ) {
        let turn = TurnRef {
            session_id: session_id.clone(),
            loop_id: loop_id.to_owned(),
        };
        self.on_steer_progress(&turn, request_index, applied_count);
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
                request.text.push_str(delta);
                append_live_part(request, LivePart::Text(delta.to_owned()));
            }
            OutputChannelWire::Reasoning => {
                request.reasoning_text.push_str(delta);
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
        let presentation = view
            .tool_presentations
            .get(&ToolKey::new(
                &turn.session_id,
                &turn.loop_id,
                request_index,
                tool_call_id,
            ))
            .cloned();
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
                tool.display = Some(presentation.display.clone());
                if tool.result.is_none() {
                    tool.result = presentation.result.clone();
                    tool.result_truncated = presentation.result_truncated;
                }
            }
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: tool_name.to_owned(),
                status: ToolStatus::Pending,
                progress: None,
                display: presentation.as_ref().map(|state| state.display.clone()),
                result: presentation.as_ref().and_then(|state| state.result.clone()),
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
            tool.result = content.clone();
            tool.result_truncated = content_truncated;
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: "(unknown tool)".to_owned(),
                status: tool_outcome_status(outcome),
                progress: None,
                display: None,
                result: content.clone(),
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
        if let Some(presentation) = view.tool_presentations.get_mut(&key) {
            // A completed ToolPresentation event carries the authoritative
            // input+result hidden count. If it arrived before ToolFinished,
            // leave that count intact; the later result event only fills the
            // result side of the state.
            presentation.result = content;
            presentation.result_truncated = content_truncated;
            presentation.display.truncated |= content_truncated;
        } else {
            // Presentation is best effort. A result without its companion
            // event still gets a safe result-only card instead of losing the
            // tool from the live/history-shaped view.
            let hidden_line_count = content
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(|text| text.split('\n').count());
            view.tool_presentations.insert(
                key,
                ToolPresentationState {
                    display: ToolDisplayWire {
                        detail: fallback_name,
                        expanded_input: None,
                        input_line_count: None,
                        hidden_line_count,
                        truncated: content_truncated,
                    },
                    result: content,
                    result_truncated: content_truncated,
                },
            );
        }
        if view.transcript.blocks.iter().any(|block| {
            matches!(block,
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
        let state = view
            .tool_presentations
            .entry(key)
            .or_insert_with(|| ToolPresentationState {
                display: display.clone(),
                result: existing_result
                    .as_ref()
                    .and_then(|(result, _)| result.clone()),
                result_truncated: existing_result
                    .as_ref()
                    .is_some_and(|(_, truncated)| *truncated),
            });
        state.display = display.clone();
        let display_for_live = display.clone();
        let _ = state;
        if view.transcript.blocks.iter().any(|block| {
            matches!(block,
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

fn assistant_parts(assistant: &crate::protocol::AssistantHistoryViewWire) -> Vec<AssistantPart> {
    if let Some(parts) = &assistant.parts {
        return parts
            .iter()
            .filter_map(|part| match part {
                AssistantDisplayPartWire::Text { text } if !text.is_empty() => {
                    Some(AssistantPart::Text(text.clone()))
                }
                AssistantDisplayPartWire::Reasoning { text } if !text.is_empty() => {
                    Some(AssistantPart::Reasoning(text.clone()))
                }
                AssistantDisplayPartWire::ToolCall { tool_call_id, name } => {
                    Some(AssistantPart::ToolCall(
                        assistant
                            .tool_calls
                            .iter()
                            .find(|call| call.tool_call_id == *tool_call_id)
                            .cloned()
                            .unwrap_or_else(|| crate::protocol::ToolCallViewWire {
                                tool_call_id: tool_call_id.clone(),
                                name: name.clone(),
                                call_index: 0,
                                display: None,
                            }),
                    ))
                }
                _ => None,
            })
            .collect();
    }
    let mut parts = Vec::new();
    if !assistant.reasoning.is_empty() {
        parts.push(AssistantPart::Reasoning(assistant.reasoning.clone()));
    }
    if !assistant.text.is_empty() {
        parts.push(AssistantPart::Text(assistant.text.clone()));
    }
    parts.extend(
        assistant
            .tool_calls
            .iter()
            .cloned()
            .map(AssistantPart::ToolCall),
    );
    parts
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

fn has_item_index(blocks: &[TranscriptBlock], index: usize) -> bool {
    blocks.iter().any(|block| block.index() == Some(index))
}

fn merge_history_items(view: &mut SessionView, items: &[IndexedHistoryItemWire]) {
    for indexed in items {
        if !view
            .transcript
            .items
            .iter()
            .any(|item| item.index == indexed.index)
        {
            view.transcript.items.push(indexed.clone());
        }
        let index = indexed.index;
        match &indexed.item {
            HistoryItemWire::User(user) => {
                let replaced =
                    view.transcript
                        .blocks
                        .iter_mut()
                        .rev()
                        .find_map(|block| match block {
                            TranscriptBlock::User(card)
                                if card.pending
                                    && (card.loop_id.as_deref() == Some(&user.loop_id)
                                        || card.text == user.text) =>
                            {
                                Some(card)
                            }
                            _ => None,
                        });
                if let Some(card) = replaced {
                    card.index = Some(index);
                    card.loop_id = Some(user.loop_id.clone());
                    card.kind = user.kind;
                    card.text = user.text.clone();
                    card.pending = false;
                    if let Some(timestamp) = &user.timestamp {
                        view.user_timestamps.insert(index, timestamp.clone());
                    }
                    view.transcript.invalidate();
                } else if !has_item_index(&view.transcript.blocks, index) {
                    view.transcript
                        .blocks
                        .push(TranscriptBlock::User(UserBlock {
                            index: Some(index),
                            loop_id: Some(user.loop_id.clone()),
                            kind: user.kind,
                            text: user.text.clone(),
                            pending: false,
                        }));
                    if let Some(timestamp) = &user.timestamp {
                        view.user_timestamps.insert(index, timestamp.clone());
                    }
                    view.transcript.invalidate();
                }
            }
            HistoryItemWire::Assistant(assistant) => {
                if has_item_index(&view.transcript.blocks, index) {
                    continue;
                }
                let parts = assistant_parts(assistant);
                view.transcript
                    .blocks
                    .push(TranscriptBlock::Assistant(AssistantBlock {
                        index,
                        loop_id: assistant.loop_id.clone(),
                        request_index: assistant.request_index,
                        model: assistant.model.clone(),
                        reasoning_level: assistant.reasoning_level,
                        parts,
                        tool_calls: assistant.tool_calls.clone(),
                        usage: assistant.usage,
                        finish_reason: assistant.finish_reason.clone(),
                        terminal_error: None,
                    }));
                for call in &assistant.tool_calls {
                    if let Some(display) = &call.display {
                        view.tool_presentations.insert(
                            ToolKey::new(
                                &view.info.session_id,
                                &assistant.loop_id,
                                assistant.request_index,
                                &call.tool_call_id,
                            ),
                            ToolPresentationState {
                                display: display.clone(),
                                result: None,
                                result_truncated: false,
                            },
                        );
                    }
                    view.transcript
                        .blocks
                        .push(TranscriptBlock::Tool(ToolBlock {
                            index: None,
                            loop_id: assistant.loop_id.clone(),
                            request_index: assistant.request_index,
                            tool_call_id: call.tool_call_id.clone(),
                            name: call.name.clone(),
                            result: None,
                            outcome: None,
                            live_status: None,
                            progress: None,
                            expanded: view
                                .tool_folds
                                .get(&ToolKey::new(
                                    &view.info.session_id,
                                    &assistant.loop_id,
                                    assistant.request_index,
                                    &call.tool_call_id,
                                ))
                                .is_some_and(FoldOverride::expanded),
                        }));
                }
                view.transcript.invalidate();
            }
            HistoryItemWire::ToolResult(result) => {
                if has_item_index(&view.transcript.blocks, index) {
                    continue;
                }
                let patched =
                    view.transcript
                        .blocks
                        .iter_mut()
                        .rev()
                        .find_map(|block| match block {
                            TranscriptBlock::Tool(tool)
                                if tool.tool_call_id == result.tool_call_id
                                    && tool.loop_id == result.loop_id
                                    && tool.request_index == result.request_index =>
                            {
                                Some(tool)
                            }
                            _ => None,
                        });
                if let Some(tool) = patched {
                    tool.index = Some(index);
                    tool.result = Some(result.content.clone());
                    tool.outcome = Some(result.outcome);
                } else {
                    view.transcript
                        .blocks
                        .push(TranscriptBlock::Tool(ToolBlock {
                            index: Some(index),
                            loop_id: result.loop_id.clone(),
                            request_index: result.request_index,
                            tool_call_id: result.tool_call_id.clone(),
                            name: result.tool_name.clone(),
                            result: Some(result.content.clone()),
                            outcome: Some(result.outcome),
                            live_status: None,
                            progress: None,
                            expanded: view
                                .tool_folds
                                .get(&ToolKey::new(
                                    &view.info.session_id,
                                    &result.loop_id,
                                    result.request_index,
                                    &result.tool_call_id,
                                ))
                                .is_some_and(FoldOverride::expanded),
                        }));
                }
                let key = ToolKey::new(
                    &view.info.session_id,
                    &result.loop_id,
                    result.request_index,
                    &result.tool_call_id,
                );
                let presentation =
                    view.tool_presentations
                        .entry(key)
                        .or_insert_with(|| ToolPresentationState {
                            display: ToolDisplayWire {
                                detail: result.tool_name.clone(),
                                expanded_input: None,
                                input_line_count: None,
                                hidden_line_count: (!result.content.is_empty())
                                    .then(|| result.content.split('\n').count()),
                                truncated: result.content_truncated,
                            },
                            result: None,
                            result_truncated: false,
                        });
                presentation.result = Some(result.content.clone());
                presentation.result_truncated = result.content_truncated;
                presentation.display.truncated |= result.content_truncated;
                view.transcript.invalidate();
            }
            HistoryItemWire::Summary(summary) => {
                if has_item_index(&view.transcript.blocks, index) {
                    continue;
                }
                view.transcript
                    .blocks
                    .push(TranscriptBlock::Summary(SummaryBlock {
                        index,
                        content: summary.content.clone(),
                    }));
                view.transcript.invalidate();
            }
        }
    }
}

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

    fn session_info(session_id: &str) -> Value {
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

    fn history_page_json(items: Vec<Value>, next_offset: Option<usize>, total: usize) -> Value {
        json!({
            "items": items,
            "next_offset": next_offset,
            "total": total
        })
    }

    fn user_item(index: usize, loop_id: &str, text: &str) -> Value {
        json!({
            "index": index,
            "item": {
                "type": "user",
                "data": {
                    "loop_id": loop_id,
                    "kind": "prompt",
                    "text": text
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
                    "text": text
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
                    "reasoning_level": "high",
                    "text": text,
                    "reasoning": "",
                    "tool_calls": [],
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
                    "tool_call_id": call_id,
                    "tool_name": name,
                    "outcome": outcome,
                    "content": content
                }
            }
        })
    }

    fn ready(app: &mut App) {
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        assert_eq!(requests.len(), 4);
        for request in &requests {
            let result = match request.method {
                "agent.ping" => json!({"version": "0.3.0"}),
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
        assert_eq!(requests.len(), 3);
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
            .find(|r| r.method == "session.history")
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
        take_requests(respond(
            app,
            history_req,
            history_page_json(vec![], None, 0),
        ));
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
    fn bootstrap_rejects_version_0_2_0_and_latches_failed() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let ping_req = requests.iter().find(|r| r.method == "agent.ping").unwrap();
        respond(&mut app, ping_req, json!({"version": "0.2.0"}));
        assert!(matches!(app.connection, ConnectionState::Failed(_)));
        let notice = app.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Error);
        assert!(notice.text.contains("0.2.0"));
    }

    #[test]
    fn bootstrap_rejects_version_0_4_0_and_latches_failed() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let ping_req = requests.iter().find(|r| r.method == "agent.ping").unwrap();
        respond(&mut app, ping_req, json!({"version": "0.4.0"}));
        assert!(matches!(app.connection, ConnectionState::Failed(_)));
    }

    #[test]
    fn bootstrap_accepts_prerelease_0_3_1_rc_1() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        for req in &requests {
            let res = match req.method {
                "agent.ping" => json!({"version": "0.3.1-rc.1"}),
                "model.list" => json!({"models": []}),
                "profile.list" => json!({"profiles": []}),
                "session.list" => json!({"sessions": []}),
                _ => unreachable!(),
            };
            take_requests(respond(&mut app, req, res));
        }
        if cfg!(debug_assertions) {
            assert_eq!(app.connection, ConnectionState::Ready);
        } else {
            assert!(matches!(app.connection, ConnectionState::Failed(_)));
        }
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
        assert_eq!(requests.len(), 3);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let history_request = requests
            .iter()
            .find(|r| r.method == "session.history")
            .unwrap();
        let presentation_request = requests
            .iter()
            .find(|r| r.method == "session.presentation")
            .unwrap();
        assert_eq!(app.sessions.active.as_deref(), Some("ses_1"));
        assert!(app.sessions.known["ses_1"].loading);

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
        let page1 = history_page_json(
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
        assert_eq!(requests[0].method, "session.history");
        assert_eq!(
            app.pending_requests.get(&requests[0].id),
            Some(&RequestKind::History {
                session_id: "ses_1".into(),
                offset: 2,
                limit: 20,
                gap_revision: Some(0),
            })
        );
        assert!(app.sessions.known["ses_1"].loading);

        // Page 2 completes chain
        let commands = respond(
            &mut app,
            &requests[0],
            history_page_json(vec![user_item(2, "loop_1", "follow up")], None, 3),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading);
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
        assert!(!view.loading);
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
        assert_eq!(live.requests[0].text, "Thinking...");
        assert_eq!(live.requests[0].tools.len(), 1);
        assert_eq!(live.requests[1].text, "Done with second iteration.");
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
        respond(&mut app, &req[0], history_page_json(items, None, 4));

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
        app.update(AppEvent::ClipboardResult {
            success: false,
            error: Some("arm spinner".to_owned()),
        });

        assert_eq!(app.next_tick(), Some(Duration::from_millis(100)));
        for millis in [0, 10, 20, 40, 60, 80, 99] {
            elapsed.store(millis, Ordering::Relaxed);
            app.update(AppEvent::Tick);
            app.update(AppEvent::Rpc(RpcEvent::AgentLogLine(
                "rpc traffic".to_owned(),
            )));
            app.update(AppEvent::ClipboardResult {
                success: false,
                error: Some("notice traffic".to_owned()),
            });
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
        let mut app = crate::ui::testapp::new_output_marker(ThemeKind::Dark);
        app.monotonic_now =
            Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
        app.terminal_size = (80, 24);
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
        let mut app = crate::ui::testapp::new_output_marker(ThemeKind::Dark);
        app.monotonic_now =
            Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
        app.terminal_size = (80, 24);
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
    fn marker_row_is_not_a_conversation_link_or_fold_target() {
        let mut app = crate::ui::testapp::chat(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let (link_row, link_cell) = prepared
            .link_cells
            .iter()
            .enumerate()
            .find_map(|(row, cells)| cells.first().map(|range| (row, range.start)))
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
            !app.pressed_cell_is_link(column, marker_row),
            "the marker row is not a markdown link cell"
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
            "the marker row cannot start selection"
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
        let column = screen.content.x + tool.content_columns.start as u16;
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

        app.update(AppEvent::ClipboardResult {
            success: false,
            error: Some("copy failed: unavailable".to_owned()),
        });
        assert!(!app.selection_copied());
        assert!(
            app.notices()
                .iter()
                .any(|notice| notice.text.contains("copy failed"))
        );

        app.update(AppEvent::ClipboardResult {
            success: true,
            error: None,
        });
        assert!(app.selection_copied());
        elapsed.store(1_799, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(app.selection_copied());
        elapsed.store(1_800, Ordering::Relaxed);
        app.update(AppEvent::Tick);
        assert!(!app.selection_copied());
    }

    #[test]
    fn scrollbar_drag_requires_release_and_cancels_on_focus_or_resize() {
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
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, visible.max(1), current)
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
        assert_eq!(app.active_view().unwrap().scroll.offset, 0);
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
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, visible.max(1), current)
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
            (preview, false)
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
            (preview, false),
            "a late mouse-up after focus cancellation must not commit the pending drag"
        );

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag((geometry.track_top + geometry.max_thumb_start) as u16);
        app.update(AppEvent::TerminalSize {
            width: 81,
            height: 24,
        });
        assert!(app.scrollbar_preview_offset("ses_1").is_none());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            (preview, false)
        );
    }

    #[test]
    fn scrollbar_drag_cancels_before_tail_scroll_and_late_release() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry = crate::ui::scrollbar::geometry(
            screen.transcript,
            total,
            visible.max(1),
            total.saturating_sub(visible),
        )
        .expect("overflowing transcript has a scrollbar");

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update_scrollbar_drag((geometry.track_top + geometry.max_thumb_start / 2) as u16);
        app.update(AppEvent::Terminal(CrosstermEvent::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::End,
                crossterm::event::KeyModifiers::CONTROL,
            ),
        )));
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
                    .blocks
                    .push(TranscriptBlock::Assistant(AssistantBlock {
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
        let geometry = crate::ui::scrollbar::geometry(
            screen.transcript,
            total,
            visible.max(1),
            total.saturating_sub(visible),
        )
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
        let geometry = crate::ui::scrollbar::geometry(
            screen.transcript,
            total,
            visible.max(1),
            total.saturating_sub(visible),
        )
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
        let geometry = crate::ui::scrollbar::geometry(
            screen.transcript,
            total,
            visible.max(1),
            total.saturating_sub(visible),
        )
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
    fn scrollbar_drag_cancels_on_a_real_viewport_change() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let geometry = crate::ui::scrollbar::geometry(
            screen.transcript,
            total,
            visible.max(1),
            total.saturating_sub(visible),
        )
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
        assert!(app.scrollbar_preview_offset("ses_1").is_none());
        assert_eq!(
            (
                app.active_view().unwrap().scroll.offset,
                app.active_view().unwrap().scroll.follow_tail,
            ),
            committed,
            "viewport cancellation must discard pending state without committing it"
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
            "a late mouse-up after viewport cancellation must not commit"
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
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, visible.max(1), current)
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
        let geometry = crate::ui::scrollbar::geometry(
            screen.transcript,
            total,
            visible.max(1),
            total.saturating_sub(visible),
        )
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
    fn scrollbar_wheel_input_cancels_an_uncommitted_drag() {
        let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
        app.terminal_size = (80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
        let total = prepared.total_rows();
        let visible = crate::ui::transcript::visible_rows(&app, total, screen.transcript.height);
        let current = total.saturating_sub(visible);
        let geometry =
            crate::ui::scrollbar::geometry(screen.transcript, total, visible.max(1), current)
                .expect("overflowing transcript has a scrollbar");

        assert!(app.begin_scrollbar_drag(geometry.column as u16, geometry.thumb_top as u16));
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: geometry.column as u16,
                row: geometry.thumb_top as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )));
        assert!(
            app.scrollbar_preview_offset("ses_1").is_none(),
            "wheel input must not leave a stale drag to be committed by a later release"
        );
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
                    "reasoning_level": "high",
                    "text": "See [docs](https://example.com/doc) then read.",
                    "reasoning": "",
                    "tool_calls": [{"tool_call_id": "call-1", "name": "read", "call_index": 0}],
                    "usage": {},
                    "finish_reason": "tool_calls"
                }
            }
        });
        let holes = take_requests(app.clear_transcript());
        respond(
            &mut app,
            &holes[0],
            history_page_json(
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
        for (row, line) in prepared.lines.iter().enumerate() {
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
                    "reasoning_level": "high",
                    "text": "See [`inline`](https://e.com/code) and **bold [link](https://e.com/b)** and plain.",
                    "reasoning": "",
                    "tool_calls": [{"tool_call_id": "call-1", "name": "read", "call_index": 0}],
                    "usage": {},
                    "finish_reason": "tool_calls"
                }
            }
        });
        let holes = take_requests(app.clear_transcript());
        respond(
            &mut app,
            &holes[0],
            history_page_json(
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
            let cell = cell_of(&prepared.lines[text_row], needle);
            assert!(
                app.pressed_cell_is_link((content_x + cell) as u16, text_row as u16),
                "link cell for {needle:?} must be detected"
            );
        }

        // Plain prose is not a link cell, so a press there cannot fold and the
        // guard stays off (no same-colored false positive).
        let plain_cell = cell_of(&prepared.lines[text_row], "plain");
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
        let link_cell = cell_of(&prepared.lines[text_row], "e.com/b");
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
        let mid_loop = RpcResponse {
            id: RequestId(1),
            result: Some(serde_json::json!({
                "items": [
                    {"index": 0, "item": {"type": "user", "data": {"loop_id": "loop_live", "kind": "prompt", "text": "stream me"}}}
                ],
                "next_offset": null,
                "total": 1
            })),
            error: None,
        };
        app.on_history_response(&"ses_1".into(), 0, None, &mid_loop);
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
                usage: crate::protocol::UsageWire::default(),
                requests: 1,
                tool_rounds: 0,
                final_config_revision: 0,
                persistence: crate::protocol::TurnPersistenceWire::Persisted,
                accepted_at: None,
            });
        }
        let terminal = RpcResponse {
            id: RequestId(1),
            result: Some(serde_json::json!({
                "items": [
                    {"index": 0, "item": {"type": "user", "data": {"loop_id": "loop_live", "kind": "prompt", "text": "stream me"}}},
                    {"index": 1, "item": {"type": "assistant", "data": {"loop_id": "loop_live", "request_index": 0, "model": "deep", "text": "answer", "reasoning": ""}}}
                ],
                "next_offset": null,
                "total": 2
            })),
            error: None,
        };
        app.on_history_response(&"ses_1".into(), 0, None, &terminal);
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
