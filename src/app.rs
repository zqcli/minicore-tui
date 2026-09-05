//! The app state machine. Every state change happens inside
//! `App::update(AppEvent)`; RPC tasks and the command executor only send
//! `AppEvent`s or run `AppCommand`s (development spec 9.1). Request ids are
//! allocated inside `update` and registered in `pending_requests` before any
//! command leaves it, so a response can never beat its registration.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event as CrosstermEvent, MouseEvent, MouseEventKind};

use crate::command::{AppCommand, CommandIssue, LocalCommand, is_slash_command, parse_command};
use crate::event::{AppEvent, RpcEvent};
use crate::keymap::{self, Action, EditorCursor};
use crate::protocol::{
    AgentEventWire, DEFAULT_HISTORY_LIMIT, EventMetaWire, HistoryItemWire, HistoryPageWire,
    IncomingFrame, IndexedHistoryItemWire, METHOD_LIST_MODELS, METHOD_LIST_PROFILES,
    METHOD_LIST_SESSIONS, OutgoingRequest, OutputChannelWire, Reasoning, RequestId,
    RpcNotification, RpcResponse, RpcResponseError, SessionInfo, SessionStateWire,
    SessionStatusWire, ToolOutcomeWire, ToolProgressWire, TurnPersistenceWire, TurnRef,
    UserMessageKindWire, is_supported_agent_version,
};
use crate::rpc::RpcError;
use crate::state::catalog::CatalogState;
use crate::state::composer::{Composer, MAX_COMPOSER_BYTES};
use crate::state::selection::{
    Dock, NewSessionField, NewSessionState, SELECTOR_PAGE, SelectorKind, SelectorState,
    filtered_models, filtered_profiles, filtered_sessions, supported_reasoning,
};
use crate::state::session::{SessionId, SessionView, SessionsState};
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::{
    AssistantBlock, AssistantPart, PreparedTranscriptCache, SummaryBlock, ToolBlock,
    TranscriptBlock, TranscriptCacheKey, UserBlock,
};
use crate::state::turn::{
    LiveLoop, LocalSubmissionId, PendingSteer, PendingSteerState, UnsavedLoop,
};
use crate::theme::ThemeKind;

/// The agent's stderr ring size, App side (spec 10.8).
pub const MAX_AGENT_LOG_LINES: usize = 200;

const MAX_NOTICES: usize = 32;

/// How long a transient notice stays before `Tick` removes it (spec 33.2).
const NOTICE_TTL: Duration = Duration::from_secs(5);

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

/// Why a request was issued; `pending_requests` routes each response to the
/// matching handler regardless of arrival order (spec 10.9, 10.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    Ping,
    ListModels,
    ListProfiles,
    ListSessions,
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
    Page {
        offset: usize,
    },
    Reconcile {
        offset: usize,
        gap_revision: Option<u64>,
    },
    LoopNotContained(String),
    Done,
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
            dock: Dock::Composer,
            viewport: (0, 0),
            last_total: 0,
            panel_scroll: 0,
            ctrl_c_at: None,
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

    /// Whether the app currently needs a visual tick. The main loop sleeps
    /// until the earliest of the spinner cadence, transient-notice expiry,
    /// and the double-Ctrl+C window; `None` means idle and no timer is armed.
    pub fn next_tick(&self) -> Option<Duration> {
        let now = self.instant_now();
        let mut earliest: Option<Duration> = None;
        if self.sessions.known.values().any(|view| {
            view.live.is_some()
                || view
                    .state
                    .as_ref()
                    .is_some_and(|state| state.status != SessionStatusWire::Idle)
        }) {
            earliest = Some(Duration::from_millis(100));
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
        if matches!(&event, AppEvent::Rendered) {
            self.dirty = false;
            return Vec::new();
        }
        self.dirty = true;
        match event {
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
                self.frame_count = self.frame_count.wrapping_add(1);
                if self.ctrl_c_at.is_some_and(|pressed| {
                    self.instant_now().saturating_duration_since(pressed) >= DOUBLE_CTRL_C_WINDOW
                }) {
                    self.ctrl_c_at = None;
                }
                self.expire_notices();
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
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.tools_expanded = !view.tools_expanded;
                    view.transcript.render_cache.clear();
                }
                Vec::new()
            }
            AppEvent::ToggleTool {
                session_id,
                loop_id,
                tool_call_id,
            } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    let mut changed = false;
                    for block in &mut view.transcript.blocks {
                        if let TranscriptBlock::Tool(tool) = block {
                            if tool.loop_id == loop_id && tool.tool_call_id == tool_call_id {
                                tool.expanded = !tool.expanded;
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        view.transcript.invalidate();
                    }
                }
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
                if let Some(state) = self.selector_state_mut() {
                    state.query = query;
                    state.cursor = 0;
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
                self.clamp_transcript_scroll();
                Vec::new()
            }
            AppEvent::TranscriptCachePrepared(prepared) => {
                self.install_transcript_cache(prepared);
                Vec::new()
            }
        }
    }

    /// The active session's view, for read-only render access.
    pub fn active_view(&self) -> Option<&SessionView> {
        self.sessions
            .active
            .as_deref()
            .and_then(|session_id| self.sessions.known.get(session_id))
    }

    /// The current durable transcript cache key for the active session.
    /// Render preparation uses the same read-only key builder before sending
    /// a `TranscriptCachePrepared` event back through `update`.
    pub fn transcript_cache_key(&self, width: u16) -> Option<(String, TranscriptCacheKey)> {
        let session_id = self.sessions.active.as_ref()?.clone();
        let view = self.sessions.known.get(&session_id)?;
        Some((
            session_id,
            view.transcript.cache_key(
                width,
                self.theme,
                self.reasoning_visible,
                view.tools_expanded,
            ),
        ))
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
            Dock::SessionSelector(state)
            | Dock::ModelSelector(state)
            | Dock::ReasoningSelector(state)
            | Dock::ProfileSelector(state) => Some(state),
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
            Dock::SessionSelector(state)
            | Dock::ModelSelector(state)
            | Dock::ReasoningSelector(state)
            | Dock::ProfileSelector(state) => Some(state),
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
            SelectorKind::Session => Dock::SessionSelector(state),
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
        self.move_selector(delta * SELECTOR_PAGE as i32)
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
            Target::SessionSelector => self.confirm_session_selector(),
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
        let (cursor, query, submitting) = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.kind != SelectorKind::Session {
                return Vec::new();
            }
            (state.cursor, state.query.clone(), state.submitting)
        };
        // One open at a time; the pending response owns the panel.
        if submitting {
            return Vec::new();
        }
        let Some(selected) = filtered_sessions(&self.sessions.list, &query)
            .get(cursor)
            .map(|session| session.session_id.clone())
        else {
            return Vec::new();
        };
        if self.pending_open_or_history(&selected)
            || self
                .sessions
                .known
                .get(&selected)
                .is_some_and(|view| view.closing)
        {
            return Vec::new();
        }
        if self.can_activate_existing_session(&selected) {
            self.dock = Dock::Composer;
            return self.activate_existing_session(&selected);
        }
        if let Some(state) = self.selector_state_mut() {
            state.submitting = true;
            state.error = None;
        }
        self.open_session(&selected)
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
            CrosstermEvent::Paste(text) => self.handle_paste(text),
            CrosstermEvent::Mouse(mouse) => {
                let action = self.mouse_action(mouse);
                self.apply_action(action)
            }
            // Resize is consumed via AppEvent::Viewport (same frame).
            _ => Vec::new(),
        }
    }

    fn apply_action(&mut self, action: Action) -> Vec<AppCommand> {
        use Action::*;
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
                Vec::new()
            }
            Newline => {
                if !self.composer.newline() {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
                    );
                }
                Vec::new()
            }
            Backspace => {
                self.composer.backspace();
                Vec::new()
            }
            Delete => {
                self.composer.delete();
                Vec::new()
            }
            CursorMove(direction) => self.composer_move(direction),
            LineStart => {
                self.composer.line_start();
                Vec::new()
            }
            LineEnd => {
                self.composer.line_end();
                Vec::new()
            }
            WordDelete => {
                self.composer.word_delete();
                Vec::new()
            }
            Undo => {
                self.composer.undo();
                Vec::new()
            }
            Redo => {
                self.composer.redo();
                Vec::new()
            }
            Submit => self.submit_composer(),
            HistoryPrev => {
                self.composer.history_prev();
                Vec::new()
            }
            HistoryNext => {
                self.composer.history_next();
                Vec::new()
            }
            OpenHelp => self.open_dock(Dock::Help),
            OpenLogs => self.open_dock(Dock::Logs),
            OpenSessions => self.open_selector(SelectorKind::Session),
            OpenModel => self.open_selector(SelectorKind::Model),
            OpenReasoning => self.open_selector(SelectorKind::Reasoning),
            ToggleTools => {
                if let Some(session_id) = self.sessions.active.as_ref().cloned() {
                    if let Some(view) = self.sessions.known.get_mut(&session_id) {
                        view.tools_expanded = !view.tools_expanded;
                        view.transcript.render_cache.clear();
                    }
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
                if self.selector_state().is_some_and(|state| state.submitting) {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.push(c);
                    state.cursor = 0;
                }
                Vec::new()
            }
            SelectorBackspace => {
                if self.selector_state().is_some_and(|state| state.submitting) {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.pop();
                    state.cursor = 0;
                }
                Vec::new()
            }
            SelectorClear => {
                if self.selector_state().is_some_and(|state| state.submitting) {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.clear();
                    state.cursor = 0;
                }
                Vec::new()
            }
            FieldStep(delta) => self.dock_field_step(delta),
            FieldChar(c) => self.field_char(c),
            FieldBackspace => self.field_backspace(),
            FieldClear => self.field_clear(),
            FieldCursor(delta) => self.field_cursor_move(delta),
            FieldHome => self.field_cursor_home(),
            FieldEnd => self.field_cursor_end(),
            ScrollRows(delta) => self.scroll_focused(delta),
            ScrollWindow(delta) => {
                let visible = self.viewport.1.max(1) as i32;
                self.scroll_focused(delta * visible)
            }
            ScrollTop => self.transcript_scroll_top(),
            ScrollBottom => self.transcript_scroll_bottom(),
        }
    }

    fn composer_move(&mut self, direction: EditorCursor) -> Vec<AppCommand> {
        match direction {
            EditorCursor::Left => self.composer.move_left(),
            EditorCursor::Right => self.composer.move_right(),
            // History recall at the buffer edges (spec 22.2): up on the
            // first row, down on the last row.
            EditorCursor::Up => {
                if self.composer.at_first_line() {
                    self.composer.history_prev();
                } else {
                    self.composer.move_up();
                }
            }
            EditorCursor::Down => {
                if self.composer.at_last_line() {
                    self.composer.history_next();
                } else {
                    self.composer.move_down();
                }
            }
        }
        Vec::new()
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

    /// Pasted text: CRLF/CR normalize to LF, inserted in one edit (no
    /// per-character events). Composer keeps the newlines; selector queries
    /// and new-session text fields flatten them (spec 43.7).
    fn handle_paste(&mut self, text: String) -> Vec<AppCommand> {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        match &self.dock {
            Dock::Composer => {
                if !self.composer.type_text(&normalized) {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
                    );
                }
            }
            Dock::NewSession(_) => {
                if !self.new_session().is_some_and(|draft| draft.submitting) {
                    self.field_insert(&normalized.replace('\n', ""));
                }
            }
            Dock::SessionSelector(_)
            | Dock::ModelSelector(_)
            | Dock::ReasoningSelector(_)
            | Dock::ProfileSelector(_) => {
                if self.selector_state().is_some_and(|state| state.submitting) {
                    return Vec::new();
                }
                if let Some(state) = self.selector_state_mut() {
                    state.query.push_str(&normalized.replace('\n', ""));
                    state.cursor = 0;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn mouse_action(&self, mouse: MouseEvent) -> Action {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.selector_state().is_some() {
                    Action::SelectorMove(-1)
                } else {
                    Action::ScrollRows(-3)
                }
            }
            MouseEventKind::ScrollDown => {
                if self.selector_state().is_some() {
                    Action::SelectorMove(1)
                } else {
                    Action::ScrollRows(3)
                }
            }
            _ => Action::None,
        }
    }

    /// Routes a scroll delta to the focused panel: selectors move their
    /// selection, Help/Logs scroll their own view, everything else scrolls
    /// the transcript.
    fn scroll_focused(&mut self, delta: i32) -> Vec<AppCommand> {
        if self.selector_state().is_some() {
            return self.move_selector(delta);
        }
        match &self.dock {
            Dock::Help | Dock::Logs => {
                self.panel_scroll = (self.panel_scroll as i64 + delta as i64).max(0) as usize;
            }
            _ => self.transcript_scroll(delta),
        }
        Vec::new()
    }

    /// Transcript scroll: negative deltas leave the tail and store an
    /// explicit offset from the top; positive deltas return toward the
    /// tail and restore follow at the bottom (spec 32, 43.8).
    fn transcript_scroll(&mut self, delta: i32) {
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
        if let Some(view) = self.active_session_mut() {
            view.scroll.follow_tail = false;
            view.scroll.offset = 0;
        }
        Vec::new()
    }

    fn transcript_scroll_bottom(&mut self) -> Vec<AppCommand> {
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

    fn install_transcript_cache(&mut self, prepared: PreparedTranscriptCache) {
        let Some(active) = self.sessions.active.as_ref() else {
            return;
        };
        if active != &prepared.session_id {
            return;
        }
        let Some(key) = prepared.key else {
            return;
        };
        let Some(view) = self.sessions.known.get(active) else {
            return;
        };
        let expected = view.transcript.cache_key(
            key.width,
            self.theme,
            self.reasoning_visible,
            view.tools_expanded,
        );
        if expected != key {
            return;
        }
        if let Some(view) = self.sessions.known.get_mut(active) {
            view.transcript.render_cache.install(prepared);
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
            self.steer_turn_with_revision(&active, text, Some(editor_revision))
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
        if view.live.as_ref().is_some_and(|live| {
            live.pending_steers
                .iter()
                .any(|steer| steer.state == PendingSteerState::Sending)
        }) {
            return Vec::new();
        }
        if !view.is_running() {
            self.notice(NoticeLevel::Warning, "session is not running; cannot steer");
            return Vec::new();
        }
        let loop_id = if let Some(live) = &view.live {
            live.reference.as_ref().map(|r| r.loop_id.clone())
        } else if let Some(state) = &view.state {
            state
                .active_loop
                .as_ref()
                .map(|loop_state| loop_state.loop_id.clone())
        } else {
            None
        };
        let Some(loop_id) = loop_id else {
            self.notice(NoticeLevel::Warning, "cannot steer: active loop id unknown");
            return Vec::new();
        };

        self.next_steer_id = self
            .next_steer_id
            .checked_add(1)
            .expect("steering ids exhausted");
        let steer_id = self.next_steer_id;

        if let Some(live) = self
            .sessions
            .known
            .get_mut(session_id)
            .and_then(|view| view.live.as_mut())
        {
            live.pending_steers.push(PendingSteer {
                local_id: steer_id,
                text: text.clone(),
                state: PendingSteerState::Sending,
            });
        }

        let req_loop_id = loop_id.clone();
        let req_text = text.clone();
        vec![self.request(
            RequestKind::SteerTurn {
                session_id: session_id.clone(),
                loop_id,
                steer_id,
                text,
                editor_revision,
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
        if let Some(view) = self.sessions.known.get_mut(&active) {
            view.transcript.clear_blocks();
            view.scroll = crate::state::session::ScrollState::default();
            view.event_gap = false;
            view.reconcile_inflight = false;
            view.loading = true;
        }
        let offset = 0;
        vec![self.request(
            RequestKind::History {
                session_id: active.clone(),
                offset,
                limit: DEFAULT_HISTORY_LIMIT,
                gap_revision: None,
            },
            |id| OutgoingRequest::get_history(id, &active, offset, DEFAULT_HISTORY_LIMIT),
        )]
    }

    fn field_char(&mut self, c: char) -> Vec<AppCommand> {
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

    fn field_insert(&mut self, text: &str) -> Vec<AppCommand> {
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
        if let Some(draft) = self.draft_mut() {
            draft.field_cursor = 0;
        }
        Vec::new()
    }

    fn field_cursor_end(&mut self) -> Vec<AppCommand> {
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

    fn pending_history(&self, session_id: &SessionId) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(kind, RequestKind::History { session_id: pending, .. } if pending == session_id)
        })
    }

    fn has_initialized_session_view(view: &SessionView) -> bool {
        view.info.loaded
            || view.state.is_some()
            || view.transcript.complete
            || view.live.is_some()
            || view.unsaved_loop.is_some()
    }

    fn activate_existing_session(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
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
        let (fetch, gap_revision) = {
            let Some(view) = self.sessions.known.get(session_id) else {
                return commands;
            };
            if view.loading || self.pending_history(session_id) {
                (false, None)
            } else if view.event_gap {
                (true, Some(view.gap_revision))
            } else if !view.transcript.complete {
                (true, None)
            } else {
                (false, None)
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
            }
            commands.push(self.request(
                RequestKind::History {
                    session_id: session_id.clone(),
                    offset,
                    limit: DEFAULT_HISTORY_LIMIT,
                    gap_revision,
                },
                |id| {
                    OutgoingRequest::session_history(
                        id,
                        session_id,
                        Some(offset),
                        Some(DEFAULT_HISTORY_LIMIT),
                    )
                },
            ));
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
            | RequestKind::History { session_id, .. }
            | RequestKind::SessionState { session_id, .. } => Some(session_id),
            RequestKind::WaitTurn(turn) | RequestKind::CancelTurn(turn) => Some(&turn.session_id),
            RequestKind::Ping
            | RequestKind::ListModels
            | RequestKind::ListProfiles
            | RequestKind::ListSessions
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
        if self.pending_open_or_history(session_id)
            || self
                .sessions
                .known
                .get(session_id)
                .is_some_and(|view| view.closing)
        {
            return Vec::new();
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

    fn close_session(&mut self, session_id: &SessionId, confirm: bool) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let (is_blocked, has_unsaved, is_running) = match self.sessions.known.get(session_id) {
            Some(view) => (
                view.is_blocked(),
                view.unsaved_loop.is_some(),
                view.is_running(),
            ),
            None => (false, false, false),
        };
        if (is_blocked || has_unsaved || is_running) && !confirm {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Session {session_id} has active or unsaved/blocked state. Type '/close confirm' to proceed."
                ),
            );
            return Vec::new();
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

    fn on_close_session_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        match response.parse_close() {
            Ok(_) => {
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.closing = false;
                    view.info.loaded = false;
                }
                self.retire_reopened_session(session_id);
                if self.sessions.active.as_deref() == Some(session_id.as_str()) {
                    self.sessions.active = None;
                }
                self.notice(NoticeLevel::Info, format!("Session {session_id} closed."));
                Vec::new()
            }
            Err(RpcResponseError::Agent(_error)) => {
                // MIG-146: close returns error, perform a single read check of session state.
                // Do not retry indefinitely.
                vec![self.request(
                    RequestKind::CloseVerifyState {
                        session_id: session_id.clone(),
                    },
                    |id| OutgoingRequest::session_state(id, session_id),
                )]
            }
            Err(error) => {
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.closing = false;
                }
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
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = false;
        }
        match response.parse_session_state() {
            Ok(state) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification: status is {:?}; unload not confirmed",
                        state.status
                    ),
                );
            }
            Err(RpcResponseError::Agent(error))
                if error.code == crate::protocol::SESSION_NOT_LOADED =>
            {
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.info.loaded = false;
                    view.closing = false;
                }
                if self.sessions.active.as_deref() == Some(session_id.as_str()) {
                    self.sessions.active = None;
                }
                self.notice(
                    NoticeLevel::Info,
                    format!("Session {session_id} verified unmounted/closed."),
                );
            }
            Err(error) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification is unknown; result/state retained: {error}"
                    ),
                );
            }
        }
        Vec::new()
    }

    fn delete_session(&mut self, session_id: &SessionId, confirm: bool) -> Vec<AppCommand> {
        if !self.guard_ready() {
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
        match response.parse_delete() {
            Ok(_) => {
                self.sessions.known.remove(session_id);
                self.sessions.list.retain(|s| &s.session_id != session_id);
                if self.sessions.active.as_deref() == Some(session_id.as_str()) {
                    self.sessions.active = None;
                }
                self.notice(NoticeLevel::Info, format!("Session {session_id} deleted."));
                Vec::new()
            }
            Err(error) => {
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
        self.sessions.active = Some(session_id.clone());

        commands.push(self.request_session_state(&session_id));

        let (fetch, gap_revision) = {
            let Some(view) = self.sessions.known.get(&session_id) else {
                return commands;
            };
            if view.loading {
                (false, None)
            } else if view.event_gap {
                (true, Some(view.gap_revision))
            } else if !view.transcript.complete {
                (true, None)
            } else {
                (false, None)
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
            }
            commands.push(self.request(
                RequestKind::History {
                    session_id: session_id.clone(),
                    offset,
                    limit: 20,
                    gap_revision,
                },
                |id| OutgoingRequest::session_history(id, &session_id, Some(offset), Some(20)),
            ));
        }
        commands
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
                state.submitting = false;
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
                state.submitting = false;
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
            view.transcript.clear_blocks();
            view.loading = false;
            view.event_gap = false;
            view.reconcile_inflight = false;
            view.needs_post_wait_history = false;
            view.closing = false;
            view.live = None;
            view.unsaved_loop = None;
            view.last_result = None;
            view.last_request = None;
            view.config_update = None;
            view.completed_steers.clear();
            view.result_unconfirmed = false;
        }
        let commands = self.on_session_response(session_id, response);
        if matches!(&self.dock, Dock::SessionSelector(state) if state.submitting) {
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
                self.notice(
                    NoticeLevel::Warning,
                    format!("state response does not match requested session {session_id}"),
                );
                Vec::new()
            }
            Err(error) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("failed to fetch state for {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    fn apply_session_state(
        &mut self,
        state: &SessionStateWire,
        event_loop_id: Option<&String>,
        from_event: bool,
    ) -> Vec<AppCommand> {
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

            if let Some(next_offset) = page.next_offset {
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

                let gap_rev_matches = gap_revision
                    .is_some_and(|revision| revision == view.gap_revision)
                    || (gap_revision.is_none()
                        && view.transcript.loaded_count == view.transcript.total);

                if view.unsaved_loop.is_none()
                    && view.event_gap
                    && turn_satisfied
                    && gap_rev_matches
                {
                    view.event_gap = false;
                }

                let _reconciling = view.reconcile_inflight;
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
                            } else if steer.state == PendingSteerState::Queued {
                                steer.state = if blocked {
                                    PendingSteerState::Unconfirmed
                                } else {
                                    PendingSteerState::NotRecorded
                                };
                            } else if steer.state == PendingSteerState::Sending {
                                steer.state = PendingSteerState::Unconfirmed;
                            }
                        }
                    }
                }

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
                                },
                            );
                        }
                    }
                }

                if view.needs_post_wait_history {
                    // Scenario B: We had an in-flight history when turn.wait completed.
                    // Now that history has completed, if the loop is not yet contained in history,
                    // we perform exactly ONE post-wait history fetch.
                    view.needs_post_wait_history = false;
                    if !loop_contained_in_history && view.live.is_some() {
                        view.loading = true;
                        view.reconcile_inflight = true;
                        NextChain::Reconcile {
                            offset: view.transcript.loaded_count,
                            gap_revision: Some(view.gap_revision),
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
            }
        };

        match next {
            NextChain::Page { offset } => vec![self.request(
                RequestKind::History {
                    session_id: session_id.clone(),
                    offset,
                    limit: 20,
                    gap_revision,
                },
                |id| OutgoingRequest::session_history(id, session_id, Some(offset), Some(20)),
            )],
            NextChain::Reconcile {
                offset,
                gap_revision,
            } => vec![self.request(
                RequestKind::History {
                    session_id: session_id.clone(),
                    offset,
                    limit: 20,
                    gap_revision,
                },
                |id| OutgoingRequest::session_history(id, session_id, Some(offset), Some(20)),
            )],
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
            Some(turn) => vec![self.request(RequestKind::CancelTurn(turn.clone()), |id| {
                OutgoingRequest::cancel_turn(id, &turn)
            })],
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
                            view.transcript.invalidate();
                        }
                        Plan::Wait {
                            turn: result.turn,
                            cancel: live.cancel_requested,
                        }
                    }
                }
                Err(error) => {
                    let is_blocked_err = matches!(&error, crate::protocol::RpcResponseError::Agent(err) if err.code == -32004);
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
        };
        match plan {
            Plan::Wait { turn, cancel } => {
                if !self.can_send_requests()
                    && !matches!(self.connection, ConnectionState::ShuttingDown)
                {
                    return Vec::new();
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
                if self.sessions.active.as_ref() == Some(session_id) {
                    if let Some(text) = recovered {
                        if self.composer.content().trim().is_empty() {
                            self.composer.set_text(&text);
                        }
                    }
                }
                self.notice(NoticeLevel::Warning, format!("turn send failed: {error}"));
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
                }
            }
            return Vec::new();
        }

        if duplicate {
            return Vec::new();
        }
        self.reconcile_after_wait(&turn)
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
        let (fetch, gap_revision) = {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return commands;
            };
            if view.loading || pending_history {
                // If a history fetch is already in flight, flag that a post-wait
                // reconcile is required once the in-flight fetch completes (spec scenario B).
                view.needs_post_wait_history = true;
                (false, None)
            } else {
                view.loading = true;
                view.reconcile_inflight = true;
                (true, Some(view.gap_revision))
            }
        };
        if fetch {
            let offset = self
                .sessions
                .known
                .get(&turn.session_id)
                .map(|view| view.transcript.loaded_count)
                .unwrap_or(0);
            commands.push(self.request(
                RequestKind::History {
                    session_id: turn.session_id.clone(),
                    offset,
                    limit: 20,
                    gap_revision,
                },
                |id| OutgoingRequest::session_history(id, &turn.session_id, Some(offset), Some(20)),
            ));
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
                            if steer.state == PendingSteerState::Sending {
                                steer.state = PendingSteerState::Queued;
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
            return Vec::new();
        }

        // Steer rejected or failed:
        // Do NOT clear composer; remove from pending / completed so it cannot preempt history!
        let (queue_full, message) = match parsed {
            Ok(_) => (false, "agent rejected the steering request".to_owned()),
            Err(RpcResponseError::Agent(err)) => {
                let queue_full = err.code == crate::protocol::STEER_QUEUE_FULL;
                (queue_full, err.to_string())
            }
            Err(err) => (false, err.to_string()),
        };

        if let Some(view) = self.sessions.known.get_mut(session_id) {
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
                if self.sessions.active.as_ref() == Some(&session_id) {
                    if let Some(text) = recovered {
                        if self.composer.content().trim().is_empty() {
                            self.composer.set_text(&text);
                        }
                    }
                }
                self.notice(NoticeLevel::Warning, format!("turn send failed: {error}"));
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
                self.notice(
                    NoticeLevel::Warning,
                    format!("turn wait send failed: {error}; result/save unconfirmed"),
                );
            }
            RequestKind::SteerTurn {
                session_id,
                steer_id,
                ..
            } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    if let Some(live) = view.live.as_mut() {
                        live.pending_steers.retain(|s| s.local_id != steer_id);
                    }
                }
                self.notice(NoticeLevel::Warning, format!("turn.steer failed: {error}"));
            }
            RequestKind::CancelTurn(_) => {
                self.notice(NoticeLevel::Warning, format!("turn cancel failed: {error}"));
            }
            RequestKind::CloseSession { session_id } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.closing = false;
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.close failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::CloseVerifyState { session_id } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.closing = false;
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("close verification failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::DeleteSession { session_id } => {
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
                    self.sessions.list = result.sessions.clone();
                    for session in result.sessions {
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
            RequestKind::CreateSession { draft } => self.on_create_response(draft, &response),
            RequestKind::OpenSession {
                session_id,
                previous_retired_loop,
            } => self.on_open_response(session_id, previous_retired_loop, &response),
            RequestKind::SessionState { session_id, query } => {
                self.on_session_state_response(&session_id, query, &response)
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
            AgentEventWire::OutputDelta { data } => {
                (data.meta.dropped_before > 0).then(|| data.meta.session_id.clone())
            }
            AgentEventWire::ToolStarted { data } => {
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
        let gap_revision = view.gap_revision;
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.loading = true;
            view.reconcile_inflight = true;
        }
        vec![self.request(
            RequestKind::History {
                session_id: session_id.clone(),
                offset,
                limit: DEFAULT_HISTORY_LIMIT,
                gap_revision: Some(gap_revision),
            },
            |id| {
                OutgoingRequest::session_history(
                    id,
                    session_id,
                    Some(offset),
                    Some(DEFAULT_HISTORY_LIMIT),
                )
            },
        )]
    }

    fn mark_gap(&mut self, meta: &EventMetaWire) {
        if meta.dropped_before == 0 {
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
            OutputChannelWire::Text => request.text.push_str(delta),
            OutputChannelWire::Reasoning => request.reasoning_text.push_str(delta),
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
        let live = view.live.as_mut().expect("live turn was bound");
        live.event_gap |= request_missing;
        let request = live.ensure_request_mut(request_index, 0, String::new(), Reasoning::Auto);
        if let Some(tool) = request
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id)
        {
            tool.name = tool_name.to_owned();
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: tool_name.to_owned(),
                status: ToolStatus::Pending,
                progress: None,
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
            });
        }
    }

    fn on_tool_finished(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        tool_call_id: &str,
        outcome: ToolOutcomeWire,
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
        if let Some(tool) = request
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id)
        {
            tool.status = tool_outcome_status(outcome);
        } else {
            request.tools.push(LiveTool {
                tool_call_id: tool_call_id.to_owned(),
                name: "(unknown tool)".to_owned(),
                status: tool_outcome_status(outcome),
                progress: None,
            });
        }
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
                    view.transcript.invalidate();
                }
            }
            HistoryItemWire::Assistant(assistant) => {
                if has_item_index(&view.transcript.blocks, index) {
                    continue;
                }
                let mut parts = Vec::new();
                if !assistant.reasoning.is_empty() {
                    parts.push(AssistantPart::Reasoning(assistant.reasoning.clone()));
                }
                if !assistant.text.is_empty() {
                    parts.push(AssistantPart::Text(assistant.text.clone()));
                }
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
                            expanded: false,
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
                            expanded: false,
                        }));
                }
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
    use serde_json::{Value, json};

    use super::*;
    use crate::command::AppCommand;
    use crate::event::{AppEvent, RpcEvent};
    use crate::protocol::{AgentEventWire, IncomingFrame, RpcResponse, TurnRef};

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
        assert_eq!(requests.len(), 2);
        let state_req = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let history_req = requests
            .iter()
            .find(|r| r.method == "session.history")
            .unwrap();
        take_requests(respond(app, state_req, state_json(session_id, "idle")));
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
        assert_eq!(requests.len(), 2);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let history_request = requests
            .iter()
            .find(|r| r.method == "session.history")
            .unwrap();
        assert_eq!(app.sessions.active.as_deref(), Some("ses_1"));
        assert!(app.sessions.known["ses_1"].loading);

        take_requests(respond(
            &mut app,
            state_request,
            state_json("ses_1", "idle"),
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
                gap_revision: None,
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
        }

        app.composer.set_text("Please correct this direction");
        let commands = app.submit_composer();
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "turn.steer");
        assert_eq!(requests[0].params["session_id"], "ses_1");
        assert_eq!(requests[0].params["loop_id"], "loop_99");
        assert_eq!(requests[0].params["text"], "Please correct this direction");

        let view = &app.sessions.known["ses_1"];
        let live = view.live.as_ref().unwrap();
        assert_eq!(live.pending_steers.len(), 1);
        assert_eq!(live.pending_steers[0].state, PendingSteerState::Sending);
    }

    #[test]
    fn composer_clears_on_successful_steer_acknowledgment() {
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

        app.composer.set_text("Steer text");
        let reqs = take_requests(app.submit_composer());
        assert_eq!(reqs.len(), 1);
        assert_eq!(app.composer.content(), "Steer text");

        respond(&mut app, &reqs[0], json!({"ok": true}));
        assert!(app.composer.content().is_empty());
        let view = &app.sessions.known["ses_1"];
        assert_eq!(
            view.live.as_ref().unwrap().pending_steers[0].state,
            PendingSteerState::Queued
        );
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

        app.composer.set_text("Steer text");
        let reqs = take_requests(app.submit_composer());
        respond_error(&mut app, &reqs[0], -32016, "steer queue full");
        assert_eq!(app.composer.content(), "Steer text");

        let notice = app.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Warning);
        assert!(notice.text.contains("queue is full"));
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
}
