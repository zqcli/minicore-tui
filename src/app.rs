//! The app state machine. Every state change happens inside
//! `App::update(AppEvent)`; RPC tasks and the command executor only send
//! `AppEvent`s or run `AppCommand`s (development spec 9.1). Request ids are
//! allocated inside `update` and registered in `pending_requests` before any
//! command leaves it, so a response can never beat its registration.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use crate::command::AppCommand;
use crate::event::{AppEvent, RpcEvent};
use crate::protocol::{
    AgentEventWire, ConversationEntryWire, IncomingFrame, METHOD_LIST_MODELS, METHOD_LIST_PROFILES,
    METHOD_LIST_SESSIONS, METHOD_PING, OutgoingRequest, OutputChannelWire, Reasoning, RequestId,
    RpcNotification, RpcResponse, RpcResponseError, SessionStateWire, SessionStatusWire,
    ToolOutcomeWire, ToolProgressWire, TranscriptPageWire, TurnRef,
};
use crate::rpc::RpcError;
use crate::state::catalog::CatalogState;
use crate::state::composer::Composer;
use crate::state::selection::{
    Dock, NewSessionField, NewSessionState, SELECTOR_PAGE, SelectorKind, SelectorState,
    filtered_models, filtered_profiles, filtered_sessions, supported_reasoning,
};
use crate::state::session::{SessionId, SessionView, SessionsState};
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::{
    AssistantBlock, AssistantPart, SummaryBlock, TerminalBlock, ToolBlock, TranscriptBlock,
    UserBlock,
};
use crate::state::turn::{LiveTurn, LocalSubmissionId};
use crate::theme::ThemeKind;

/// The agent's stderr ring size, App side (spec 10.8).
pub const MAX_AGENT_LOG_LINES: usize = 200;

const MAX_NOTICES: usize = 32;

/// Fixed text for interactions this TUI cannot answer (spec 11.7, 37.4).
pub const UNSUPPORTED_INTERACTION_NOTICE: &str =
    "This session is waiting for an interaction that this TUI version does not support.";

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
    fn new(level: NoticeLevel, text: String, sticky: bool) -> Self {
        Self {
            level,
            text,
            created_at: Instant::now(),
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
        /// The `NewSessionState::draft_id` this create was issued for, to
        /// route the response back to the matching draft (spec 25.5).
        /// Programmatic creates use a sentinel that never matches a real
        /// draft.
        draft: u64,
    },
    OpenSession(SessionId),
    SessionState(SessionId),
    Transcript {
        session_id: SessionId,
        after: Option<u64>,
        /// The session's `gap_revision` when this chain was issued to heal
        /// an event gap; a completion only clears the gap when no newer gap
        /// arrived while the chain was in flight (spec 13.7).
        gap_revision: Option<u64>,
    },
    SendTurn {
        session_id: SessionId,
        local_submission: LocalSubmissionId,
    },
    WaitTurn(TurnRef),
    CancelTurn(TurnRef),
}

/// All app and UI state. Only `App::update` mutates it; render code reads
/// the public fields, tasks and executor never touch the app at all.
pub struct App {
    pub connection: ConnectionState,
    pub catalogs: CatalogState,
    pub sessions: SessionsState,
    pub notices: VecDeque<Notice>,
    /// The agent's stderr ring, newest last (spec 10.8).
    pub agent_logs: VecDeque<String>,
    /// Text of the last failed turn submission, to restore into the
    /// composer (Phase 5) when the send itself failed.
    pub recovered_input: Option<String>,
    /// Visual state (spec 16, 30): palette, reasoning visibility, the frame
    /// counter for the spinner, and the minimal Phase 3 composer.
    pub theme: ThemeKind,
    pub reasoning_visible: bool,
    pub frame_count: u64,
    pub composer: Composer,
    /// The dock panel below the transcript (spec 24.1).
    pub dock: Dock,
    /// The new-session draft while a model/reasoning/profile selector sits
    /// on top of the form; `Some` only then (spec 26.4).
    draft: Option<NewSessionState>,
    /// Clock for session-relative ages; injectable so render output is
    /// deterministic in tests. Read-only, never mutated by `update`.
    pub now: fn() -> SystemTime,
    pub pending_requests: HashMap<RequestId, RequestKind>,
    next_request_id: RequestId,
    next_submission: u64,
    next_draft_id: u64,
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

/// What a completed transcript chain should do next.
enum NextChain {
    Page {
        after: u64,
    },
    Reconcile {
        after: Option<u64>,
        gap_revision: Option<u64>,
    },
    /// `complete=false` without a cursor: the agent could not confirm
    /// durability. Stop the chain and let a later open/reconcile retry from
    /// the last merged sequence.
    Stopped,
    Done,
}

impl App {
    /// A fresh app in `Starting` state. `default_workspace` is the working
    /// directory the TUI was started in.
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
            agent_logs: VecDeque::new(),
            recovered_input: None,
            theme: ThemeKind::Dark,
            reasoning_visible: true,
            frame_count: 0,
            composer: Composer::default(),
            dock: Dock::Composer,
            draft: None,
            now: SystemTime::now,
            pending_requests: HashMap::new(),
            next_request_id: RequestId(0),
            next_submission: 0,
            next_draft_id: 0,
            bootstrap: BootstrapProgress::default(),
            blocked_notice: false,
        }
    }

    /// The single state-mutation entry point. Returns the side effects the
    /// main loop must execute; commands are never executed here.
    pub fn update(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Bootstrap => self.bootstrap(),
            AppEvent::SubmitTurn { session_id, text } => self.submit_turn(&session_id, text),
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
            AppEvent::CancelTurn { session_id } => self.cancel_turn(&session_id),
            AppEvent::Rpc(event) => self.on_rpc_event(event),
            AppEvent::RpcSendFailed { id, error } => self.on_send_failed(id, error),
            AppEvent::Tick => {
                self.frame_count = self.frame_count.wrapping_add(1);
                Vec::new()
            }
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
                }
                Vec::new()
            }
            AppEvent::ToggleTool {
                session_id,
                turn_id,
                tool_call_id,
            } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    for block in &mut view.transcript.blocks {
                        if let TranscriptBlock::Tool(tool) = block {
                            if tool.turn_id == turn_id && tool.tool_call_id == tool_call_id {
                                tool.expanded = !tool.expanded;
                            }
                        }
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
        }
    }

    /// The active session's view, for read-only render access.
    pub fn active_view(&self) -> Option<&SessionView> {
        self.sessions
            .active
            .as_deref()
            .and_then(|session_id| self.sessions.known.get(session_id))
    }

    /// The current new-session draft, whether the form or a selector is
    /// showing it (read-only).
    pub fn new_session(&self) -> Option<&NewSessionState> {
        self.draft.as_ref().or(match &self.dock {
            Dock::NewSession(draft) => Some(draft),
            _ => None,
        })
    }

    // ---- dock & selectors (spec 24-28) -------------------------------

    fn make_new_session_draft(&mut self) -> NewSessionState {
        let draft_id = self.next_draft_id;
        self.next_draft_id = self
            .next_draft_id
            .checked_add(1)
            .expect("draft ids exhausted");
        NewSessionState {
            // Workspace is a plain string the agent validates (spec 25.4).
            workspace: self
                .catalogs
                .default_workspace
                .to_string_lossy()
                .into_owned(),
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
        // While a create is in flight the draft is frozen: opening a
        // model/reasoning/profile selector, or confirming one from a field,
        // must not leave the form or mutate the drafting session (spec
        // 25.5). The session selector is unrelated and stays available.
        if kind != SelectorKind::Session && self.new_session().is_some_and(|draft| draft.submitting)
        {
            return Vec::new();
        }
        let mut state = SelectorState::new(kind);
        if kind != SelectorKind::Session {
            self.ensure_new_session_draft();
            let model = self
                .new_session()
                .map(|draft| draft.model.clone())
                .unwrap_or_default();
            let profile = self
                .new_session()
                .map(|draft| draft.profile.clone())
                .unwrap_or_default();
            let reasoning = self.new_session().map(|draft| draft.reasoning);
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
        let (kind, query, cursor) = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            (state.kind, state.query.clone(), state.cursor)
        };
        let count = self.selector_count(kind, &query);
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

    fn selector_count(&self, kind: SelectorKind, query: &str) -> usize {
        match kind {
            SelectorKind::Model => filtered_models(&self.catalogs.models, query).len(),
            SelectorKind::Profile => filtered_profiles(&self.catalogs.profiles, query).len(),
            SelectorKind::Reasoning => supported_reasoning(
                &self.catalogs.models,
                &self
                    .new_session()
                    .map(|draft| draft.model.clone())
                    .unwrap_or_default(),
            )
            .len(),
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
        enum Target {
            Composer,
            SessionSelector,
            NewSession,
            Form,
        }
        let target = match &self.dock {
            Dock::Composer => Target::Composer,
            Dock::SessionSelector(_) => Target::SessionSelector,
            Dock::NewSession(_) => Target::NewSession,
            Dock::ModelSelector(_) | Dock::ReasoningSelector(_) | Dock::ProfileSelector(_) => {
                Target::Form
            }
        };
        match target {
            Target::Composer => {}
            Target::SessionSelector => self.dock = Dock::Composer,
            Target::NewSession => {
                self.draft = None;
                self.dock = Dock::Composer;
            }
            Target::Form => self.close_selector_to_form(),
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
        if let Some(state) = self.selector_state_mut() {
            state.submitting = true;
            state.error = None;
        }
        vec![
            self.request(RequestKind::OpenSession(selected.clone()), |id| {
                OutgoingRequest::session_open(id, &selected)
            }),
        ]
    }

    fn confirm_model_item(&mut self) -> Vec<AppCommand> {
        let selected = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.kind != SelectorKind::Model {
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
        // The chosen reasoning is never downgraded; the reasoning selector
        // (opened below) forces an explicit choice when unsupported (spec
        // 26.4).
        let incompatible = self
            .new_session()
            .is_some_and(|draft| !selected.supported_reasoning.contains(&draft.reasoning));
        if let Some(draft) = self.draft_mut() {
            draft.model = selected.id.clone();
        }
        if incompatible {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "{} may not support the current reasoning level; choose a supported one.",
                    selected.id
                ),
            );
        }
        // Even when compatible the user explicitly confirms the level.
        self.open_selector(SelectorKind::Reasoning)
    }

    fn confirm_reasoning_item(&mut self) -> Vec<AppCommand> {
        let (cursor, kind) = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            (state.cursor, state.kind)
        };
        if kind != SelectorKind::Reasoning {
            return Vec::new();
        }
        let model = self
            .new_session()
            .map(|draft| draft.model.clone())
            .unwrap_or_default();
        let Some(selected) = supported_reasoning(&self.catalogs.models, &model)
            .get(cursor)
            .copied()
        else {
            // No supported values (unknown model): nothing to confirm.
            return Vec::new();
        };
        if let Some(draft) = self.draft_mut() {
            draft.reasoning = selected;
        }
        self.close_selector_to_form();
        Vec::new()
    }

    fn confirm_profile_item(&mut self) -> Vec<AppCommand> {
        let selected = {
            let Some(state) = self.selector_state() else {
                return Vec::new();
            };
            if state.kind != SelectorKind::Profile {
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
        if let Some(draft) = self.draft_mut() {
            draft.profile = selected.id.clone();
            draft.model = selected.model.clone();
            draft.reasoning = selected.reasoning;
        }
        self.close_selector_to_form();
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

    fn next_request_id(&mut self) -> RequestId {
        let next = self
            .next_request_id
            .0
            .checked_add(1)
            .expect("request id space exhausted");
        self.next_request_id = RequestId(next);
        self.next_request_id
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

    fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.push_notice(Notice::new(level, text.into(), false));
    }

    fn sticky_notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.push_notice(Notice::new(level, text.into(), true));
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
        if self.connection != ConnectionState::Starting {
            return Vec::new();
        }
        vec![
            self.request(RequestKind::Ping, OutgoingRequest::ping),
            self.request(RequestKind::ListModels, OutgoingRequest::list_models),
            self.request(RequestKind::ListProfiles, OutgoingRequest::list_profiles),
            self.request(RequestKind::ListSessions, OutgoingRequest::list_sessions),
        ]
    }

    fn bootstrap_progress(&mut self, part: BootstrapPart) {
        match part {
            BootstrapPart::Ping => self.bootstrap.ping = true,
            BootstrapPart::Models => self.bootstrap.models = true,
            BootstrapPart::Profiles => self.bootstrap.profiles = true,
            BootstrapPart::Sessions => self.bootstrap.sessions = true,
        }
        // Ready only after all four succeeded; a latched failure stays.
        if self.connection == ConnectionState::Starting && self.bootstrap.done() {
            self.connection = ConnectionState::Ready;
            self.catalogs.loaded = true;
            self.blocked_notice = false;
        }
    }

    /// User actions (submit/create/open/cancel) need a live connection;
    /// anything else is a no-op with a single notice per connection state.
    fn guard_ready(&mut self) -> bool {
        if self.connection == ConnectionState::Ready {
            return true;
        }
        if !self.blocked_notice {
            self.blocked_notice = true;
            self.notice(
                NoticeLevel::Info,
                "That action is unavailable until the agent is connected.",
            );
        }
        false
    }

    /// Catalog/list failures are fatal per spec: the TUI cannot choose a
    /// profile or session, and there is no auto-retry.
    fn bootstrap_failure(&mut self, method: &str, error: RpcResponseError) -> Vec<AppCommand> {
        self.connection = ConnectionState::Failed(format!("bootstrap failed at {method}: {error}"));
        self.sticky_notice(
            NoticeLevel::Error,
            "Bootstrap failed. No further requests will be sent.",
        );
        Vec::new()
    }

    // ---- sessions ------------------------------------------------------

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
        vec![
            self.request(RequestKind::OpenSession(session_id.clone()), |id| {
                OutgoingRequest::session_open(id, session_id)
            }),
        ]
    }

    fn on_session_response(
        &mut self,
        response: &RpcResponse,
    ) -> Result<Vec<AppCommand>, RpcResponseError> {
        let session = response.parse_session()?.session;
        let session_id = session.session_id.clone();
        match self.sessions.known.get_mut(&session_id) {
            Some(view) => view.info = session,
            None => {
                self.sessions
                    .known
                    .insert(session_id.clone(), SessionView::new(session));
            }
        }
        self.sessions.active = Some(session_id.clone());
        let mut commands = vec![
            self.request(RequestKind::SessionState(session_id.clone()), |id| {
                OutgoingRequest::session_state(id, &session_id)
            }),
        ];
        // Re-opening (or switching back to) a session heals an event gap
        // even over a complete transcript, and continues an unfinished
        // transcript, always from the last merged sequence (spec 13.7).
        let (fetch, after, gap_revision) = {
            let Some(view) = self.sessions.known.get(&session_id) else {
                return Ok(commands);
            };
            if view.loading {
                (false, None, None)
            } else if view.event_gap {
                (true, view.transcript.last_seq, Some(view.gap_revision))
            } else if !view.transcript.complete {
                (true, view.transcript.last_seq, None)
            } else {
                (false, None, None)
            }
        };
        if fetch {
            if let Some(view) = self.sessions.known.get_mut(&session_id) {
                view.loading = true;
            }
            commands.push(self.request(
                RequestKind::Transcript {
                    session_id: session_id.clone(),
                    after,
                    gap_revision,
                },
                |id| OutgoingRequest::transcript(id, &session_id, after),
            ));
        }
        Ok(commands)
    }

    /// Create response: success closes the matching form and keeps the
    /// activated session; failure keeps every draft field, unblocks
    /// submitting, and reports the agent message on the form (spec 25.5).
    fn on_create_response(&mut self, draft_id: u64, response: &RpcResponse) -> Vec<AppCommand> {
        match self.on_session_response(response) {
            Ok(commands) => {
                // Success closes the new-session flow whenever the matching
                // draft is around, including the defensive case of the app
                // sitting on a selector while the create was in flight; a
                // stale response (different id) never closes a newer draft.
                let matches = self
                    .draft
                    .as_ref()
                    .is_some_and(|draft| draft.draft_id == draft_id)
                    || matches!(&self.dock, Dock::NewSession(draft) if draft.draft_id == draft_id);
                if matches {
                    self.draft = None;
                    self.dock = Dock::Composer;
                }
                commands
            }
            Err(error) => match self.draft_matching(draft_id) {
                Some(draft) => {
                    draft.submitting = false;
                    draft.error = Some(format!("{error}"));
                    Vec::new()
                }
                None => {
                    self.notice(
                        NoticeLevel::Error,
                        format!("session.create failed: {error}"),
                    );
                    Vec::new()
                }
            },
        }
    }

    /// Open response: success activates the session and closes the
    /// selector; failure keeps the selector, its query and selection, and
    /// shows the error on the panel (spec 28.6). Programmatic opens (no
    /// submitting selector) fall back to a notice.
    fn on_open_response(&mut self, response: &RpcResponse) -> Vec<AppCommand> {
        match self.on_session_response(response) {
            Ok(commands) => {
                let close = matches!(&self.dock, Dock::SessionSelector(state) if state.submitting);
                if close {
                    self.dock = Dock::Composer;
                }
                commands
            }
            Err(error) => {
                let from_selector = matches!(&self.dock, Dock::SessionSelector(_));
                if from_selector {
                    if let Dock::SessionSelector(state) = &mut self.dock {
                        state.submitting = false;
                        state.error = Some(format!("{error}"));
                    }
                } else {
                    self.notice(NoticeLevel::Error, format!("session.open failed: {error}"));
                }
                Vec::new()
            }
        }
    }

    fn on_session_state_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        match response.parse_session_state() {
            Ok(state) => self.apply_session_state(&state),
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("malformed session state for {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    fn apply_session_state(&mut self, state: &SessionStateWire) -> Vec<AppCommand> {
        let show_unsupported = {
            let Some(view) = self.sessions.known.get_mut(&state.session_id) else {
                return Vec::new();
            };
            let was_waiting = view
                .state
                .as_ref()
                .is_some_and(|current| current.status == SessionStatusWire::WaitingForInput);
            view.state = Some(state.clone());
            !was_waiting && state.status == SessionStatusWire::WaitingForInput
        };
        if show_unsupported {
            self.sticky_notice(NoticeLevel::Warning, UNSUPPORTED_INTERACTION_NOTICE);
        }
        Vec::new()
    }

    fn on_transcript_response(
        &mut self,
        session_id: &SessionId,
        gap_revision: Option<u64>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let page = match response.parse_transcript() {
            Ok(page) => page,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("malformed transcript for {session_id}: {error}"),
                );
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.loading = false;
                    view.reconcile_inflight = false;
                }
                return Vec::new();
            }
        };
        self.continue_transcript_chain(session_id, gap_revision, &page)
    }

    /// Merges one page and decides whether the chain continues. Pages are
    /// merged immediately; completion also finishes any pending
    /// reconciliation (spec 13.6) and heals event gaps observed before the
    /// chain was issued (spec 13.7).
    fn continue_transcript_chain(
        &mut self,
        session_id: &SessionId,
        gap_revision: Option<u64>,
        page: &TranscriptPageWire,
    ) -> Vec<AppCommand> {
        let next = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            merge_entries(view, &page.entries);
            // last_seq is the highest durable entry seq actually merged into
            // blocks — never the wire's observed_head, which can sit above
            // the last merged entry on compaction/summary projections or on
            // pages that stop without a cursor. The next incremental fetch
            // must start from the real durable tail so nothing is skipped.
            if let Some(merged) = page.entries.iter().map(entry_seq).max() {
                view.transcript.last_seq = Some(
                    view.transcript
                        .last_seq
                        .map_or(merged, |last| last.max(merged)),
                );
            }
            if page.complete {
                view.transcript.next_after = None;
                view.transcript.complete = true;
                view.loading = false;
                // Only heal a gap that existed when this chain was issued; a
                // gap that arrived mid-chain needs a fresh chain (there is no
                // event replay anywhere in the app).
                if view.event_gap
                    && gap_revision.is_some_and(|revision| revision == view.gap_revision)
                {
                    view.event_gap = false;
                }
                let reconciling = view.reconcile_inflight;
                view.reconcile_inflight = false;
                if reconciling {
                    // The reconcile chain finished; the durable blocks are
                    // now the final truth for the turn.
                    view.live = None;
                    NextChain::Done
                } else if view.live.as_ref().is_some_and(|live| live.waiting) {
                    // A turn finished while another chain was running; fetch
                    // the durable tail once before reconciling.
                    view.reconcile_inflight = true;
                    let after = view.transcript.last_seq;
                    NextChain::Reconcile {
                        after,
                        gap_revision: Some(view.gap_revision),
                    }
                } else {
                    NextChain::Done
                }
            } else {
                view.transcript.next_after = page.next_after;
                match page.next_after {
                    Some(after) => NextChain::Page { after },
                    None => {
                        // `complete=false` with no cursor is reachable while
                        // the agent cannot confirm durability: keep the
                        // merged entries, stop this chain, and leave the
                        // transcript incomplete so a later open/wait
                        // reconcile retries from last_seq. Nothing is
                        // fabricated, no live turn or gap is touched.
                        view.loading = false;
                        view.reconcile_inflight = false;
                        view.transcript.complete = false;
                        NextChain::Stopped
                    }
                }
            }
        };
        match next {
            NextChain::Page { after } => vec![self.request(
                RequestKind::Transcript {
                    session_id: session_id.clone(),
                    after: Some(after),
                    gap_revision,
                },
                |id| OutgoingRequest::transcript(id, session_id, Some(after)),
            )],
            NextChain::Reconcile {
                after,
                gap_revision,
            } => vec![self.request(
                RequestKind::Transcript {
                    session_id: session_id.clone(),
                    after,
                    gap_revision,
                },
                |id| OutgoingRequest::transcript(id, session_id, after),
            )],
            NextChain::Stopped => {
                self.notice(
                    NoticeLevel::Warning,
                    "The agent could not confirm the transcript is durable yet; it can be \
                     retried when the session is stable.",
                );
                Vec::new()
            }
            NextChain::Done => Vec::new(),
        }
    }

    // ---- turns ---------------------------------------------------------

    fn submit_turn(&mut self, session_id: &SessionId, text: String) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let submission = LocalSubmissionId(self.next_submission);
        self.next_submission = self
            .next_submission
            .checked_add(1)
            .expect("submission ids exhausted");
        {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            // At most one live turn per session; a second submit is ignored.
            if view.live.is_some() {
                return Vec::new();
            }
            view.live = Some(LiveTurn {
                reference: None,
                local_submission: submission,
                user_text: trimmed.to_owned(),
                text: String::new(),
                reasoning: String::new(),
                tools: Vec::new(),
                waiting: false,
                cancel_requested: false,
                event_gap: false,
            });
            view.transcript
                .blocks
                .push(TranscriptBlock::User(UserBlock {
                    seq: None,
                    turn_id: None,
                    text: trimmed.to_owned(),
                    pending: true,
                }));
        }
        vec![self.request(
            RequestKind::SendTurn {
                session_id: session_id.clone(),
                local_submission: submission,
            },
            |id| OutgoingRequest::send_turn(id, session_id, trimmed),
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
            let Some(live) = view.live.as_mut() else {
                return Vec::new();
            };
            live.cancel_requested = true;
            live.reference.clone()
        };
        match reference {
            // Cancellation always carries the exact TurnRef; unknown yet, so
            // the send response issues the cancel instead (spec 13.3).
            Some(turn) => vec![self.request(RequestKind::CancelTurn(turn.clone()), |id| {
                OutgoingRequest::cancel_turn(id, &turn)
            })],
            None => Vec::new(),
        }
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
        }
        let plan = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            let Some(live) = view
                .live
                .as_mut()
                .filter(|live| live.local_submission == local_submission)
            else {
                return Vec::new();
            };
            match response.parse_turn() {
                Ok(result) => {
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
                        card.turn_id = Some(result.turn.turn_id.clone());
                    }
                    Plan::Wait {
                        turn: result.turn,
                        cancel: live.cancel_requested,
                    }
                }
                Err(error) => {
                    let recovered = view.live.take().map(|live| live.user_text);
                    view.transcript.blocks.retain(
                        |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                    );
                    Plan::Failed { recovered, error }
                }
            }
        };
        match plan {
            // The wait is registered in this same update, before any event
            // can race ahead of it; TurnFinished events are never awaited.
            Plan::Wait { turn, cancel } => {
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
                if let Some(text) = recovered {
                    self.recovered_input = Some(text);
                }
                self.notice(NoticeLevel::Error, format!("turn send failed: {error}"));
                Vec::new()
            }
        }
    }

    fn on_wait_response(&mut self, turn: TurnRef, response: &RpcResponse) -> Vec<AppCommand> {
        enum Plan {
            Reconcile,
        }
        let (plan, notice_text) = {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return Vec::new();
            };
            let Some(live) = view
                .live
                .as_mut()
                .filter(|live| live.reference.as_ref() == Some(&turn))
            else {
                return Vec::new();
            };
            match response.parse_turn_wait() {
                Ok(_outcome) => {
                    live.waiting = true;
                    (Plan::Reconcile, None)
                }
                // turn_not_found and friends: the durable transcript and the
                // session state are authoritative, so the same reconciliation
                // restores the truth (spec 13.6).
                Err(RpcResponseError::Agent(error)) => {
                    live.waiting = true;
                    (
                        Plan::Reconcile,
                        Some(format!(
                            "turn wait failed ({error}); recovering from the durable transcript"
                        )),
                    )
                }
                Err(RpcResponseError::Parse(error)) => {
                    live.waiting = true;
                    (
                        Plan::Reconcile,
                        Some(format!(
                            "malformed turn.wait result ({error}); recovering from the durable \
                             transcript"
                        )),
                    )
                }
                Err(RpcResponseError::Malformed) => {
                    live.waiting = true;
                    (
                        Plan::Reconcile,
                        Some(
                            "turn.wait response has no payload; recovering from the durable \
                             transcript"
                                .to_owned(),
                        ),
                    )
                }
            }
        };
        match plan {
            Plan::Reconcile => {
                if let Some(text) = notice_text {
                    self.notice(NoticeLevel::Warning, text);
                }
                self.reconcile_after_wait(&turn)
            }
        }
    }

    /// After a wait response: fetch the session state plus everything after
    /// the last durable sequence; the live turn ends when that chain
    /// completes (spec 13.4).
    fn reconcile_after_wait(&mut self, turn: &TurnRef) -> Vec<AppCommand> {
        let mut commands = vec![
            self.request(RequestKind::SessionState(turn.session_id.clone()), |id| {
                OutgoingRequest::session_state(id, &turn.session_id)
            }),
        ];
        let fetch = self
            .sessions
            .known
            .get(&turn.session_id)
            .is_some_and(|view| !view.loading);
        if fetch {
            let after = self
                .sessions
                .known
                .get(&turn.session_id)
                .and_then(|view| view.transcript.last_seq);
            let gap_revision = self
                .sessions
                .known
                .get(&turn.session_id)
                .map(|view| view.gap_revision);
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.reconcile_inflight = true;
            }
            commands.push(self.request(
                RequestKind::Transcript {
                    session_id: turn.session_id.clone(),
                    after,
                    gap_revision,
                },
                |id| OutgoingRequest::transcript(id, &turn.session_id, after),
            ));
        }
        commands
    }

    fn on_cancel_response(&mut self, response: &RpcResponse) -> Vec<AppCommand> {
        if let Err(error) = response.parse_cancel() {
            self.notice(NoticeLevel::Warning, format!("cancel failed: {error}"));
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
                    if view
                        .live
                        .as_ref()
                        .is_some_and(|live| live.local_submission == local_submission)
                    {
                        if let Some(live) = view.live.take() {
                            recovered = Some(live.user_text);
                        }
                    }
                    view.transcript.blocks.retain(
                        |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                    );
                    recovered
                };
                if let Some(text) = recovered {
                    self.recovered_input = Some(text);
                }
                self.notice(
                    NoticeLevel::Error,
                    format!("failed to send the turn: {error}"),
                );
            }
            RequestKind::CreateSession { draft } => match self.draft_matching(draft) {
                Some(draft) => {
                    draft.submitting = false;
                    draft.error = Some(format!("failed to send session.create: {error}"));
                }
                None => self.notice(
                    NoticeLevel::Error,
                    format!("failed to send session.create: {error}"),
                ),
            },
            RequestKind::OpenSession(_) => {
                let from_selector = matches!(&self.dock, Dock::SessionSelector(_));
                if from_selector {
                    if let Dock::SessionSelector(state) = &mut self.dock {
                        state.submitting = false;
                        state.error = Some(format!("failed to send session.open: {error}"));
                    }
                } else {
                    self.notice(
                        NoticeLevel::Error,
                        format!("failed to send session.open: {error}"),
                    );
                }
            }
            _ => {
                self.notice(
                    NoticeLevel::Error,
                    format!("failed to send a request: {error}"),
                );
            }
        }
        Vec::new()
    }

    // ---- incoming events ----------------------------------------------

    fn on_rpc_event(&mut self, event: RpcEvent) -> Vec<AppCommand> {
        match event {
            RpcEvent::Frame(frame) => self.on_frame(frame),
            RpcEvent::AgentLogLine(line) => {
                self.push_log(line);
                Vec::new()
            }
            RpcEvent::ConnectionClosed => self.connection_terminated("agent stdout closed"),
            RpcEvent::ProtocolError(error) => {
                self.connection_terminated(&format!("RPC protocol error: {error}"))
            }
            RpcEvent::Exited(status) => {
                let reason = match status {
                    Some(status) => format!("agent exited: {status}"),
                    None => "agent exited".to_owned(),
                };
                self.connection_terminated(&reason)
            }
        }
    }

    /// The first connection-terminating event latches `Failed`; later
    /// termination events are idempotent and never overwrite the first
    /// cause (see `crate::event`).
    fn connection_terminated(&mut self, reason: &str) -> Vec<AppCommand> {
        match self.connection {
            ConnectionState::Failed(_) | ConnectionState::ShuttingDown => Vec::new(),
            _ => {
                self.connection = ConnectionState::Failed(reason.to_owned());
                Vec::new()
            }
        }
    }

    fn on_frame(&mut self, frame: IncomingFrame) -> Vec<AppCommand> {
        match frame {
            IncomingFrame::Response(response) => self.on_response(response),
            IncomingFrame::Notification(notification) => self.on_notification(notification),
        }
    }

    fn on_response(&mut self, response: RpcResponse) -> Vec<AppCommand> {
        let Some(kind) = self.pending_requests.remove(&response.id) else {
            self.notice(
                NoticeLevel::Warning,
                format!("ignored response for unknown request id {}", response.id.0),
            );
            return Vec::new();
        };
        match kind {
            RequestKind::Ping => match response.parse_ping() {
                Ok(_) => {
                    self.bootstrap_progress(BootstrapPart::Ping);
                    Vec::new()
                }
                Err(error) => self.bootstrap_failure(METHOD_PING, error),
            },
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
                    if self.catalogs.next_profile.is_none() {
                        if let Some(first) = self.catalogs.profiles.first() {
                            self.catalogs.next_profile = Some(first.id.clone());
                            self.catalogs.next_model = Some(first.model.clone());
                            self.catalogs.next_reasoning = Some(first.reasoning);
                        }
                    }
                    self.bootstrap_progress(BootstrapPart::Profiles);
                    Vec::new()
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_PROFILES, error),
            },
            RequestKind::ListSessions => match response.parse_sessions() {
                Ok(result) => {
                    self.sessions.list = result.sessions;
                    for info in &self.sessions.list {
                        match self.sessions.known.get_mut(&info.session_id) {
                            Some(view) => view.info = info.clone(),
                            None => {
                                self.sessions.known.insert(
                                    info.session_id.clone(),
                                    SessionView::new(info.clone()),
                                );
                            }
                        }
                    }
                    self.bootstrap_progress(BootstrapPart::Sessions);
                    Vec::new()
                }
                Err(error) => self.bootstrap_failure(METHOD_LIST_SESSIONS, error),
            },
            RequestKind::CreateSession { draft } => self.on_create_response(draft, &response),
            RequestKind::OpenSession(_) => self.on_open_response(&response),
            RequestKind::SessionState(session_id) => {
                self.on_session_state_response(&session_id, &response)
            }
            RequestKind::Transcript {
                session_id,
                gap_revision,
                ..
            } => self.on_transcript_response(&session_id, gap_revision, &response),
            RequestKind::SendTurn {
                session_id,
                local_submission,
            } => self.on_send_response(&session_id, local_submission, &response),
            RequestKind::WaitTurn(turn) => self.on_wait_response(turn, &response),
            RequestKind::CancelTurn(_) => self.on_cancel_response(&response),
        }
    }

    fn on_notification(&mut self, notification: RpcNotification) -> Vec<AppCommand> {
        match notification {
            RpcNotification::AgentEvent(event) => self.on_agent_event(event),
            RpcNotification::Unknown { .. } => Vec::new(),
        }
    }

    /// Routes agent events to the session they belong to; background
    /// sessions keep updating. Events whose instance does not match the
    /// session's current state are stale and ignored (spec 13.6).
    fn on_agent_event(&mut self, event: AgentEventWire) -> Vec<AppCommand> {
        match event {
            AgentEventWire::SessionOpened { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                let session_id = data.session.session_id.clone();
                match self.sessions.known.get_mut(&session_id) {
                    Some(view) => view.info = data.session,
                    None => {
                        self.sessions
                            .known
                            .insert(session_id, SessionView::new(data.session));
                    }
                }
            }
            AgentEventWire::SessionClosed { data } => {
                self.mark_gap(&data.session_id, data.meta.dropped_before);
                let stale = self
                    .sessions
                    .known
                    .get(&data.session_id)
                    .and_then(|view| view.state.as_ref())
                    .is_some_and(|state| state.instance_id != data.meta.instance_id);
                if !stale {
                    if let Some(view) = self.sessions.known.get_mut(&data.session_id) {
                        view.live = None;
                    }
                }
            }
            AgentEventWire::SessionState { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                let stale = self
                    .sessions
                    .known
                    .get(&data.state.session_id)
                    .and_then(|view| view.state.as_ref())
                    .is_some_and(|current| current.instance_id != data.state.instance_id);
                if !stale {
                    self.apply_session_state(&data.state);
                }
            }
            AgentEventWire::TurnStarted { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                self.adopt_turn_started(&data.turn);
            }
            AgentEventWire::OutputDelta { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                self.append_delta(&data.turn, &data.channel, &data.delta);
            }
            AgentEventWire::ToolStarted { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                self.on_tool_started(&data.turn, &data.tool_call_id, &data.tool_name);
            }
            AgentEventWire::ToolProgress { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                self.on_tool_progress(&data.turn, &data.tool_call_id, &data.progress);
            }
            AgentEventWire::ToolFinished { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                self.on_tool_finished(&data.turn, &data.tool_call_id, data.result.outcome);
            }
            AgentEventWire::InteractionRequested { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                let show = match self.sessions.known.get(&data.session_id) {
                    None => false,
                    Some(view) => view
                        .state
                        .as_ref()
                        .is_none_or(|state| state.status != SessionStatusWire::WaitingForInput),
                };
                if show {
                    self.sticky_notice(NoticeLevel::Warning, UNSUPPORTED_INTERACTION_NOTICE);
                }
            }
            AgentEventWire::InteractionResolved { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
            }
            AgentEventWire::TurnFinished { data } => {
                self.mark_gap(&data.meta.session_id, data.meta.dropped_before);
                // Not authoritative: the wait response plus the durable
                // transcript drive completion even if this event is lost,
                // early, or late (spec 13.6).
            }
            AgentEventWire::Unknown => {}
        }
        Vec::new()
    }

    fn mark_gap(&mut self, session_id: &SessionId, dropped_before: u64) {
        if dropped_before == 0 {
            return;
        }
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.event_gap = true;
            // Every new dropped event invalidates heal chains already in
            // flight: their completion must not clear this newer gap.
            view.gap_revision += 1;
            if let Some(live) = view.live.as_mut() {
                live.event_gap = true;
            }
        }
    }

    /// TurnStarted may beat the send response; it fills the reference and
    /// links the pending user card. A mismatched reference is stale or
    /// foreign and leaves the live turn alone.
    fn adopt_turn_started(&mut self, turn: &TurnRef) {
        // The session's known instance is read before the mutable live
        // borrow, so the strictness check cannot fight the borrow checker.
        let known = {
            let Some(view) = self.sessions.known.get(&turn.session_id) else {
                return;
            };
            Self::known_instance(view).map(str::to_owned)
        };
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        let Some(live) = view.live.as_mut() else {
            return;
        };
        if live.reference.is_none() {
            // Before the send response, the only identity available is the
            // session's instance; a stale instance's start event must never
            // be adopted (spec 13.6).
            if let Some(expected) = &known {
                if expected != &turn.instance_id {
                    return;
                }
            }
            live.reference = Some(turn.clone());
            for block in &mut view.transcript.blocks {
                if let TranscriptBlock::User(card) = block {
                    if card.pending {
                        card.turn_id = Some(turn.turn_id.clone());
                    }
                }
            }
        }
    }

    /// The session's current instance: the freshest known state wins, then
    /// the info snapshot.
    fn known_instance(view: &SessionView) -> Option<&str> {
        view.state
            .as_ref()
            .map(|state| state.instance_id.as_str())
            .or(view.info.instance_id.as_deref())
    }

    /// Live text/reasoning deltas; appended only when the event's turn
    /// matches the live reference exactly (session + instance + turn).
    fn append_delta(&mut self, turn: &TurnRef, channel: &OutputChannelWire, delta: &str) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        let Some(live) = view
            .live
            .as_mut()
            .filter(|live| live.reference.as_ref() == Some(turn))
        else {
            return;
        };
        match channel {
            OutputChannelWire::Text => live.text.push_str(delta),
            OutputChannelWire::Reasoning => live.reasoning.push_str(delta),
        }
    }

    fn on_tool_started(&mut self, turn: &TurnRef, tool_call_id: &str, tool_name: &str) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        let Some(live) = view
            .live
            .as_mut()
            .filter(|live| live.reference.as_ref() == Some(turn))
        else {
            return;
        };
        // Idempotent per tool_call_id: duplicate started events never create
        // a second LiveTool.
        if !live
            .tools
            .iter()
            .any(|tool| tool.tool_call_id == tool_call_id)
        {
            live.tools.push(LiveTool {
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
        tool_call_id: &str,
        progress: &ToolProgressWire,
    ) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        let Some(live) = view
            .live
            .as_mut()
            .filter(|live| live.reference.as_ref() == Some(turn))
        else {
            return;
        };
        let Some(tool) = live
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id)
        else {
            return;
        };
        // Progress only advances a Pending/Running tool; a terminal status
        // (Succeeded/Failed/Denied/Cancelled) is never downgraded, and its
        // recorded progress text is left untouched.
        if matches!(tool.status, ToolStatus::Pending | ToolStatus::Running) {
            tool.status = ToolStatus::Running;
            if let Some(message) = &progress.message {
                tool.progress = Some(message.clone());
            }
        }
    }

    fn on_tool_finished(&mut self, turn: &TurnRef, tool_call_id: &str, outcome: ToolOutcomeWire) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        let Some(live) = view
            .live
            .as_mut()
            .filter(|live| live.reference.as_ref() == Some(turn))
        else {
            return;
        };
        if let Some(tool) = live
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == tool_call_id)
        {
            tool.status = tool_outcome_status(outcome);
        }
    }
}

fn tool_outcome_status(outcome: ToolOutcomeWire) -> ToolStatus {
    match outcome {
        ToolOutcomeWire::Success => ToolStatus::Succeeded,
        // input_provided means the agent is waiting for user input; this
        // TUI answers no interactions, so it ends the tool without success.
        ToolOutcomeWire::Failed | ToolOutcomeWire::InputProvided => ToolStatus::Failed,
        ToolOutcomeWire::Denied => ToolStatus::Denied,
        ToolOutcomeWire::Cancelled => ToolStatus::Cancelled,
    }
}

fn tool_outcome_label(outcome: &ToolOutcomeWire) -> &'static str {
    match outcome {
        ToolOutcomeWire::Success => "success",
        ToolOutcomeWire::Failed => "failed",
        ToolOutcomeWire::Denied => "denied",
        ToolOutcomeWire::Cancelled => "cancelled",
        ToolOutcomeWire::InputProvided => "input_provided",
    }
}

fn has_seq(blocks: &[TranscriptBlock], seq: u64) -> bool {
    blocks.iter().any(|block| block.seq() == Some(seq))
}

/// Every durable entry carries a sequence number (spec 12.8).
fn entry_seq(entry: &ConversationEntryWire) -> u64 {
    match entry {
        ConversationEntryWire::UserMessage(user) => user.seq,
        ConversationEntryWire::AssistantMessage(assistant) => assistant.seq,
        ConversationEntryWire::ToolResult(result) => result.seq,
        ConversationEntryWire::Summary(summary) => summary.seq,
        ConversationEntryWire::TurnTerminal(terminal) => terminal.seq,
    }
}

/// Merges durable entries into the view's blocks: dedupe by sequence
/// number, replace the pending live user card with its durable entry, and
/// patch tool blocks with their results by id (spec 18).
fn merge_entries(view: &mut SessionView, entries: &[ConversationEntryWire]) {
    for entry in entries {
        match entry {
            ConversationEntryWire::UserMessage(user) => {
                let replaced =
                    view.transcript
                        .blocks
                        .iter_mut()
                        .rev()
                        .find_map(|block| match block {
                            TranscriptBlock::User(card)
                                if card.pending
                                    && card.turn_id.as_deref() == Some(user.turn_id.as_str()) =>
                            {
                                Some(card)
                            }
                            _ => None,
                        });
                if let Some(card) = replaced {
                    card.seq = Some(user.seq);
                    card.text = user.text.clone();
                    card.pending = false;
                } else if !has_seq(&view.transcript.blocks, user.seq) {
                    view.transcript
                        .blocks
                        .push(TranscriptBlock::User(UserBlock {
                            seq: Some(user.seq),
                            turn_id: Some(user.turn_id.clone()),
                            text: user.text.clone(),
                            pending: false,
                        }));
                }
            }
            ConversationEntryWire::AssistantMessage(assistant) => {
                if has_seq(&view.transcript.blocks, assistant.seq) {
                    continue;
                }
                let mut parts = Vec::new();
                if let Some(text) = assistant.text.as_deref().filter(|text| !text.is_empty()) {
                    parts.push(AssistantPart::Text(text.to_owned()));
                }
                if let Some(reasoning) = assistant
                    .reasoning
                    .as_deref()
                    .filter(|reasoning| !reasoning.is_empty())
                {
                    parts.push(AssistantPart::Reasoning(reasoning.to_owned()));
                }
                view.transcript
                    .blocks
                    .push(TranscriptBlock::Assistant(AssistantBlock {
                        seq: assistant.seq,
                        turn_id: assistant.turn_id.clone(),
                        model: assistant.model.clone(),
                        parts,
                        terminal_error: None,
                    }));
                // Tool calls become separate blocks; the results patch them
                // by id. Arguments never leave the agent on the wire.
                for call in &assistant.tool_calls {
                    view.transcript
                        .blocks
                        .push(TranscriptBlock::Tool(ToolBlock {
                            tool_call_id: call.tool_call_id.clone(),
                            turn_id: assistant.turn_id.clone(),
                            name: call.name.clone(),
                            arguments: None,
                            result: None,
                            outcome: None,
                            live_status: None,
                            progress: None,
                            expanded: false,
                        }));
                }
            }
            ConversationEntryWire::ToolResult(result) => {
                if has_seq(&view.transcript.blocks, result.seq) {
                    continue;
                }
                let outcome = tool_outcome_label(&result.outcome).to_owned();
                let patched =
                    view.transcript
                        .blocks
                        .iter_mut()
                        .rev()
                        .find_map(|block| match block {
                            TranscriptBlock::Tool(tool)
                                if tool.tool_call_id == result.tool_call_id
                                    && tool.turn_id == result.turn_id =>
                            {
                                Some(tool)
                            }
                            _ => None,
                        });
                if let Some(tool) = patched {
                    tool.result = Some(result.content.clone());
                    tool.outcome = Some(outcome);
                } else {
                    view.transcript
                        .blocks
                        .push(TranscriptBlock::Tool(ToolBlock {
                            tool_call_id: result.tool_call_id.clone(),
                            turn_id: result.turn_id.clone(),
                            name: result.tool_name.clone(),
                            arguments: None,
                            result: Some(result.content.clone()),
                            outcome: Some(outcome),
                            live_status: None,
                            progress: None,
                            expanded: false,
                        }));
                }
            }
            ConversationEntryWire::Summary(summary) => {
                if has_seq(&view.transcript.blocks, summary.seq) {
                    continue;
                }
                view.transcript
                    .blocks
                    .push(TranscriptBlock::Summary(SummaryBlock {
                        seq: summary.seq,
                        through: summary.through,
                        summary: summary.summary.clone(),
                    }));
            }
            ConversationEntryWire::TurnTerminal(terminal) => {
                if has_seq(&view.transcript.blocks, terminal.seq) {
                    continue;
                }
                view.transcript
                    .blocks
                    .push(TranscriptBlock::Terminal(TerminalBlock {
                        seq: terminal.seq,
                        turn_id: terminal.turn_id.clone(),
                        terminal: terminal.terminal.clone(),
                        usage: terminal.usage,
                    }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::protocol::{
        FrameError, FrameErrorKind, RpcError as WireError, RpcErrorData, SessionInfo,
    };

    fn test_app() -> App {
        App::new(PathBuf::from("/workspace"))
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
        kind: &str,
        message: &str,
    ) -> Vec<AppCommand> {
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: None,
                error: Some(WireError {
                    code: -32000,
                    message: message.to_owned(),
                    data: Some(RpcErrorData {
                        kind: kind.to_owned(),
                        retryable: false,
                    }),
                }),
            },
        ))))
    }

    fn event(event: AgentEventWire) -> AppEvent {
        AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(event),
        )))
    }

    fn wire_event(raw: Value) -> AgentEventWire {
        serde_json::from_value(raw).expect("wire event fixture parses")
    }

    fn make_turn(session: &str, instance: &str, turn_id: &str) -> TurnRef {
        TurnRef {
            session_id: session.to_owned(),
            instance_id: instance.to_owned(),
            turn_id: turn_id.to_owned(),
        }
    }

    fn turn_ref_json(session: &str, instance: &str, turn_id: &str) -> Value {
        json!({"session_id": session, "instance_id": instance, "turn_id": turn_id})
    }

    fn meta_json(session: &str, instance: &str, dropped: u64) -> Value {
        json!({"session_id": session, "instance_id": instance, "dropped_before": dropped})
    }

    fn session_info(session_id: &str, instance: Option<&str>) -> Value {
        json!({
            "session_id": session_id,
            "title": null,
            "profile": "coding",
            "workspace": "/project",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "instance_id": instance,
            "created_at": "2026-01-02T03:04:05.006Z",
            "updated_at": "2026-01-02T03:04:05.006Z"
        })
    }

    fn state_json(session_id: &str, instance: &str, status: &str) -> Value {
        json!({
            "session_id": session_id,
            "instance_id": instance,
            "status": status,
            "health": "healthy",
            "active_turn": null,
            "pending_interaction": null,
            "conversation_seq": 0,
            "last_terminal": null
        })
    }

    fn page_json(
        entries: Vec<Value>,
        next_after: Option<u64>,
        observed_head: u64,
        complete: bool,
    ) -> Value {
        json!({
            "entries": entries,
            "next_after": next_after,
            "observed_head": observed_head,
            "complete": complete
        })
    }

    fn user_entry(seq: u64, turn_id: &str, text: &str) -> Value {
        json!({"user_message": {
            "seq": seq,
            "turn_id": turn_id,
            "text": text,
            "execution": {"model": "deep", "reasoning": "high", "max_tool_rounds": 8},
            "created_at": "2026-01-02T03:04:05.006Z"
        }})
    }

    fn assistant_entry(seq: u64, turn_id: &str, text: &str) -> Value {
        json!({"assistant_message": {
            "seq": seq,
            "turn_id": turn_id,
            "model": "deep",
            "text": text,
            "reasoning": null,
            "tool_calls": [],
            "usage": {},
            "finish_reason": "stop",
            "created_at": "2026-01-02T03:04:05.006Z"
        }})
    }

    fn terminal_entry(seq: u64, turn_id: &str) -> Value {
        json!({"turn_terminal": {
            "seq": seq,
            "turn_id": turn_id,
            "terminal": "completed",
            "usage": {"input_tokens": 3, "output_tokens": 2},
            "created_at": "2026-01-02T03:04:05.006Z"
        }})
    }

    fn outcome_json(turn_id: &str, terminal: &str) -> Value {
        json!({
            "turn_id": turn_id,
            "terminal": terminal,
            "usage": {"input_tokens": 3, "output_tokens": 2}
        })
    }

    /// Bootstraps to Ready by responding to the four requests in order.
    fn ready(app: &mut App) {
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        assert_eq!(requests.len(), 4);
        for request in &requests {
            let result = match request.method {
                "agent.ping" => json!({"version": "0.2.0"}),
                "model.list" => json!({"models": []}),
                "profile.list" => json!({"profiles": []}),
                "session.list" => json!({"sessions": []}),
                other => panic!("unexpected bootstrap request: {other}"),
            };
            take_requests(respond(app, request, result));
        }
        assert_eq!(app.connection, ConnectionState::Ready);
    }

    /// Opens a session and completes its initial state + transcript chain.
    fn open_session(app: &mut App, session_id: &str, instance: &str) {
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: session_id.into(),
        }));
        assert_eq!(open.len(), 1);
        let commands = respond(
            app,
            &open[0],
            json!({"session": session_info(session_id, Some(instance))}),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 2);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            app,
            state_request,
            state_json(session_id, instance, "idle"),
        ));
        let commands = respond(app, transcript_request, page_json(vec![], None, 0, true));
        assert!(take_requests(commands).is_empty());
        assert!(!app.sessions.known[session_id].loading);
        assert!(app.sessions.known[session_id].transcript.complete);
    }

    fn submit(app: &mut App, session_id: &str, text: &str) -> OutgoingRequest {
        let commands = app.update(AppEvent::SubmitTurn {
            session_id: session_id.into(),
            text: text.into(),
        });
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1, "one send request expected");
        requests.into_iter().next().expect("one request")
    }

    fn turn_started_event(turn_ref: &TurnRef) -> AppEvent {
        event(wire_event(json!({
            "type": "turn_started",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, 0)
            }
        })))
    }

    fn delta_event(turn_ref: &TurnRef, channel: &str, delta: &str) -> AppEvent {
        delta_event_dropped(turn_ref, channel, delta, 0)
    }

    fn delta_event_dropped(
        turn_ref: &TurnRef,
        channel: &str,
        delta: &str,
        dropped: u64,
    ) -> AppEvent {
        event(wire_event(json!({
            "type": "output_delta",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "channel": channel,
                "delta": delta,
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, dropped)
            }
        })))
    }

    fn tool_started_event(turn_ref: &TurnRef, id: &str, name: &str) -> AppEvent {
        event(wire_event(json!({
            "type": "tool_started",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "tool_call_id": id,
                "tool_name": name,
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, 0)
            }
        })))
    }

    fn tool_progress_event(turn_ref: &TurnRef, id: &str, message: &str) -> AppEvent {
        event(wire_event(json!({
            "type": "tool_progress",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "tool_call_id": id,
                "progress": {"message": message, "completed": 1, "total": 2},
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, 0)
            }
        })))
    }

    fn tool_finished_event(turn_ref: &TurnRef, id: &str, outcome: &str) -> AppEvent {
        event(wire_event(json!({
            "type": "tool_finished",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "tool_call_id": id,
                "result": {"outcome": outcome, "content_bytes": 8},
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, 0)
            }
        })))
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
        let ids: Vec<u64> = requests.iter().map(|r| r.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn bootstrap_reaches_ready_out_of_order_with_interleaved_events() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let by_method = |method: &str| {
            requests
                .iter()
                .find(|r| r.method == method)
                .expect("bootstrap request")
        };
        // A notification in the middle must not disturb id correlation.
        take_requests(app.update(event(wire_event(json!({
            "type": "session_closed",
            "data": {"session_id": "ses_x", "meta": meta_json("ses_x", "ins_x", 0)}
        })))));
        take_requests(respond(
            &mut app,
            by_method("session.list"),
            json!({"sessions": []}),
        ));
        assert_ne!(app.connection, ConnectionState::Ready);
        take_requests(respond(
            &mut app,
            by_method("profile.list"),
            json!({"profiles": []}),
        ));
        assert_ne!(app.connection, ConnectionState::Ready);
        take_requests(respond(
            &mut app,
            by_method("agent.ping"),
            json!({"version": "0.2.0"}),
        ));
        assert_ne!(app.connection, ConnectionState::Ready);
        take_requests(respond(
            &mut app,
            by_method("model.list"),
            json!({"models": []}),
        ));
        assert_eq!(app.connection, ConnectionState::Ready);
        assert!(app.catalogs.loaded);
        assert!(app.pending_requests.is_empty());
    }

    #[test]
    fn bootstrap_failure_is_fatal_and_latched() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let models = requests.iter().find(|r| r.method == "model.list").unwrap();
        take_requests(respond_error(
            &mut app,
            models,
            "models_unavailable",
            "no models",
        ));
        assert!(matches!(
            &app.connection,
            ConnectionState::Failed(reason) if reason.contains("model.list")
        ));
        // Later successes never override the failure.
        let ping = requests.iter().find(|r| r.method == "agent.ping").unwrap();
        take_requests(respond(&mut app, ping, json!({"version": "0.2.0"})));
        assert!(matches!(
            &app.connection,
            ConnectionState::Failed(reason) if reason.contains("model.list")
        ));
        assert!(
            app.notices
                .iter()
                .any(|n| n.text.contains("Bootstrap failed"))
        );
    }

    #[test]
    fn malformed_bootstrap_result_is_fatal() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let ping = requests.iter().find(|r| r.method == "agent.ping").unwrap();
        take_requests(respond(&mut app, ping, json!({"version": 1})));
        assert!(matches!(
            &app.connection,
            ConnectionState::Failed(reason) if reason.contains("agent.ping")
        ));
    }

    #[test]
    fn unknown_response_id_emits_a_warning_notice() {
        let mut app = test_app();
        let commands = app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: RequestId(99),
                result: Some(json!({})),
                error: None,
            },
        ))));
        assert!(take_requests(commands).is_empty());
        let notice = app.notices.back().unwrap();
        assert_eq!(notice.level, NoticeLevel::Warning);
        assert!(notice.text.contains("99"));
        assert!(app.pending_requests.is_empty());
    }

    #[test]
    fn create_session_activates_and_pages_the_transcript() {
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
        let commands = respond(
            &mut app,
            create,
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 2);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        assert_eq!(app.sessions.active.as_deref(), Some("ses_1"));
        assert!(app.sessions.known["ses_1"].loading);

        take_requests(respond(
            &mut app,
            state_request,
            state_json("ses_1", "ins_1", "idle"),
        ));
        assert_eq!(
            app.sessions.known["ses_1"]
                .state
                .as_ref()
                .unwrap()
                .instance_id,
            "ins_1"
        );

        // Page 1 is partial; the app pages on, one page at a time.
        let page1 = page_json(
            vec![
                user_entry(1, "trn_1", "hello"),
                assistant_entry(2, "trn_1", "hi back"),
            ],
            Some(2),
            2,
            false,
        );
        let commands = respond(&mut app, transcript_request, page1);
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.transcript");
        assert_eq!(
            app.pending_requests.get(&requests[0].id),
            Some(&RequestKind::Transcript {
                session_id: "ses_1".into(),
                after: Some(2),
                gap_revision: None,
            })
        );
        assert!(app.sessions.known["ses_1"].loading);

        // Page 2 completes the chain.
        let commands = respond(
            &mut app,
            &requests[0],
            page_json(vec![terminal_entry(3, "trn_1")], None, 3, true),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading);
        assert!(view.transcript.complete);
        assert_eq!(view.transcript.blocks.len(), 3);
        assert!(matches!(
            view.transcript.blocks.last(),
            Some(TranscriptBlock::Terminal(_))
        ));
    }

    #[test]
    fn reopening_a_loaded_session_is_idempotent() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        assert_eq!(open.len(), 1);
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        // No new transcript chain: only the state refresh.
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.state");
        assert_eq!(app.sessions.active.as_deref(), Some("ses_1"));
    }

    #[test]
    fn submit_creates_a_live_turn_and_a_pending_user_card() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let request = submit(&mut app, "ses_1", "  hello  ");
        assert_eq!(request.method, "turn.send");
        assert_eq!(request.params["text"], json!("hello"));
        assert_eq!(
            app.sessions.known["ses_1"].live.as_ref().unwrap().user_text,
            "hello"
        );
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .unwrap()
                .reference
                .is_none()
        );
        assert!(
            app.sessions.known["ses_1"]
                .transcript
                .blocks
                .iter()
                .any(|block| matches!(block, TranscriptBlock::User(card) if card.pending))
        );
        // Blank input never submits; a second submit while one turn is live
        // is ignored (one live turn per session).
        let commands = app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".into(),
            text: "   ".into(),
        });
        assert!(take_requests(commands).is_empty());
        let commands = app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".into(),
            text: "second".into(),
        });
        assert!(take_requests(commands).is_empty());
    }

    #[test]
    fn turn_started_before_the_send_response_fills_the_reference_and_wait_follows() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");

        // The start event beats the send response.
        take_requests(app.update(turn_started_event(&turn)));
        assert_eq!(
            app.sessions.known["ses_1"].live.as_ref().unwrap().reference,
            Some(turn.clone())
        );

        // The send response registers the wait in the very same update.
        let commands = respond(
            &mut app,
            &send_request,
            json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "turn.wait");
        assert_eq!(
            app.pending_requests.get(&requests[0].id),
            Some(&RequestKind::WaitTurn(turn.clone()))
        );
        // The pending card is now linked to the real turn.
        let card = app.sessions.known["ses_1"]
            .transcript
            .blocks
            .iter()
            .find_map(|block| match block {
                TranscriptBlock::User(card) if card.pending => Some(card),
                _ => None,
            })
            .unwrap();
        assert_eq!(card.turn_id.as_deref(), Some("trn_1"));
    }

    #[test]
    fn send_failure_removes_the_live_turn_and_recovers_the_input() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        take_requests(respond_error(
            &mut app,
            &send_request,
            "turn_rejected",
            "session is closing",
        ));
        let view = &app.sessions.known["ses_1"];
        assert!(view.live.is_none());
        assert_eq!(app.recovered_input.as_deref(), Some("hello"));
        assert!(
            !view
                .transcript
                .blocks
                .iter()
                .any(|block| matches!(block, TranscriptBlock::User(card) if card.pending))
        );
        assert!(!app.pending_requests.contains_key(&send_request.id));
        assert!(
            app.notices
                .iter()
                .any(|n| n.text.contains("turn send failed"))
        );
    }

    #[test]
    fn send_channel_failure_removes_the_pending_request_and_recovers_input() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let commands = app.update(AppEvent::RpcSendFailed {
            id: send_request.id,
            error: RpcError::Closed,
        });
        assert!(take_requests(commands).is_empty());
        assert!(app.sessions.known["ses_1"].live.is_none());
        assert_eq!(app.recovered_input.as_deref(), Some("hello"));
        assert!(!app.pending_requests.contains_key(&send_request.id));
    }

    #[test]
    fn deltas_append_text_and_reasoning_only_for_the_matching_turn() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        take_requests(app.update(delta_event(&turn, "text", "Hel")));
        take_requests(app.update(delta_event(&turn, "reasoning", "think")));
        let wrong = make_turn("ses_1", "ins_1", "OTHER");
        take_requests(app.update(delta_event(&wrong, "text", "WRONG")));
        take_requests(app.update(delta_event(&turn, "text", "lo")));
        let live = app.sessions.known["ses_1"].live.as_ref().unwrap();
        assert_eq!(live.text, "Hello");
        assert_eq!(live.reasoning, "think");
    }

    #[test]
    fn tool_events_are_idempotent_per_call_id() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        submit(&mut app, "ses_1", "use tools");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        take_requests(app.update(tool_started_event(&turn, "call-1", "read")));
        take_requests(app.update(tool_started_event(&turn, "call-1", "read")));
        take_requests(app.update(tool_started_event(&turn, "call-2", "write")));
        take_requests(app.update(tool_progress_event(&turn, "call-1", "50%")));
        take_requests(app.update(tool_finished_event(&turn, "call-1", "success")));
        take_requests(app.update(tool_finished_event(&turn, "call-1", "success")));
        let live = app.sessions.known["ses_1"].live.as_ref().unwrap();
        assert_eq!(live.tools.len(), 2);
        let first = live
            .tools
            .iter()
            .find(|t| t.tool_call_id == "call-1")
            .unwrap();
        assert_eq!(first.status, ToolStatus::Succeeded);
        assert_eq!(first.progress.as_deref(), Some("50%"));
        let second = live
            .tools
            .iter()
            .find(|t| t.tool_call_id == "call-2")
            .unwrap();
        assert_eq!(second.status, ToolStatus::Pending);
    }

    #[test]
    fn wait_response_drives_reconciliation_and_durable_replaces_live() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        take_requests(app.update(delta_event(&turn, "text", "live text")));
        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            let requests = take_requests(commands);
            assert_eq!(requests.len(), 1);
            requests.into_iter().next().unwrap()
        };

        // The wait response issues state + transcript fetches in one update.
        let commands = respond(&mut app, &wait_request, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 2);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        assert!(app.sessions.known["ses_1"].live.as_ref().unwrap().waiting);
        assert!(app.sessions.known["ses_1"].reconcile_inflight);

        // The durable text differs from the live text: durable wins.
        let durable = page_json(
            vec![
                user_entry(1, "trn_1", "hello"),
                assistant_entry(2, "trn_1", "durable text"),
                terminal_entry(3, "trn_1"),
            ],
            None,
            3,
            true,
        );
        let commands = respond(&mut app, transcript_request, durable);
        assert!(take_requests(commands).is_empty());
        take_requests(respond(
            &mut app,
            state_request,
            state_json("ses_1", "ins_1", "idle"),
        ));

        let view = &app.sessions.known["ses_1"];
        assert!(view.live.is_none());
        assert!(!view.reconcile_inflight);
        assert!(
            !view
                .transcript
                .blocks
                .iter()
                .any(|block| matches!(block, TranscriptBlock::User(card) if card.pending))
        );
        let assistant = view
            .transcript
            .blocks
            .iter()
            .find_map(|block| match block {
                TranscriptBlock::Assistant(assistant) => Some(assistant),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            assistant.parts,
            vec![AssistantPart::Text("durable text".into())]
        );
    }

    #[test]
    fn late_deltas_after_the_wait_response_are_tolerated() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond(&mut app, &wait_request, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        // A delta arriving after the wait response is still shown live...
        take_requests(app.update(delta_event(&turn, "text", "late delta")));
        assert_eq!(
            app.sessions.known["ses_1"].live.as_ref().unwrap().text,
            "late delta"
        );
        // ...but the durable transcript is the final truth.
        let durable = page_json(
            vec![
                user_entry(1, "trn_1", "hello"),
                assistant_entry(2, "trn_1", "durable text"),
            ],
            None,
            2,
            true,
        );
        take_requests(respond(&mut app, transcript_request, durable));
        let view = &app.sessions.known["ses_1"];
        assert!(view.live.is_none());
        let assistant = view
            .transcript
            .blocks
            .iter()
            .find_map(|block| match block {
                TranscriptBlock::Assistant(assistant) => Some(assistant),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            assistant.parts,
            vec![AssistantPart::Text("durable text".into())]
        );
    }

    #[test]
    fn turn_finished_events_are_never_authoritative() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        // The finished event fires before the wait response with a different
        // outcome: it must neither complete nor corrupt anything.
        take_requests(app.update(event(wire_event(json!({
            "type": "turn_finished",
            "data": {
                "turn": turn_ref_json("ses_1", "ins_1", "trn_1"),
                "outcome": outcome_json("trn_1", "cancelled_by_user"),
                "meta": meta_json("ses_1", "ins_1", 0)
            }
        })))));
        assert!(app.sessions.known["ses_1"].live.is_some());
        let commands = respond(&mut app, &wait_request, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            transcript_request,
            page_json(vec![terminal_entry(1, "trn_1")], None, 1, true),
        ));
        assert!(app.sessions.known["ses_1"].live.is_none());
        assert!(matches!(
            app.sessions.known["ses_1"].transcript.blocks.last(),
            Some(TranscriptBlock::Terminal(_))
        ));
    }

    #[test]
    fn event_gap_is_marked_and_cleared_by_reconciliation() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        take_requests(app.update(delta_event_dropped(&turn, "text", "abc", 3)));
        let view = &app.sessions.known["ses_1"];
        assert!(view.event_gap);
        assert!(view.live.as_ref().unwrap().event_gap);

        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond(&mut app, &wait_request, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            transcript_request,
            page_json(vec![user_entry(1, "trn_1", "hello")], None, 1, true),
        ));
        let view = &app.sessions.known["ses_1"];
        assert!(view.live.is_none());
        assert!(!view.event_gap);
    }

    #[test]
    fn cancel_sends_the_exact_reference_and_keeps_the_wait_pending() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        let commands = respond(
            &mut app,
            &send_request,
            json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
        );
        let requests = take_requests(commands);
        let wait_request = requests.into_iter().next().unwrap();

        let commands = app.update(AppEvent::CancelTurn {
            session_id: "ses_1".into(),
        });
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "turn.cancel");
        assert_eq!(
            requests[0].params,
            json!({"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_1"})
        );
        assert_eq!(
            app.pending_requests.get(&requests[0].id),
            Some(&RequestKind::CancelTurn(turn.clone()))
        );
        // The wait stays pending through the cancel.
        assert!(
            app.pending_requests
                .values()
                .any(|kind| matches!(kind, RequestKind::WaitTurn(t) if *t == turn))
        );
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .unwrap()
                .cancel_requested
        );
        take_requests(respond(&mut app, &requests[0], json!({"cancelled": true})));
        assert!(app.sessions.known["ses_1"].live.is_some());
        assert!(app.pending_requests.contains_key(&wait_request.id));
    }

    #[test]
    fn cancel_before_a_reference_is_deferred_to_the_send_response() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let commands = app.update(AppEvent::CancelTurn {
            session_id: "ses_1".into(),
        });
        assert!(take_requests(commands).is_empty());
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .unwrap()
                .cancel_requested
        );

        // The send response now issues both the wait and the deferred cancel.
        let commands = respond(
            &mut app,
            &send_request,
            json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "turn.wait");
        assert_eq!(requests[1].method, "turn.cancel");
        assert_eq!(
            requests[1].params,
            json!({"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_1"})
        );
    }

    #[test]
    fn wait_turn_not_found_recovers_from_state_and_transcript() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond_error(
            &mut app,
            &wait_request,
            "turn_not_found",
            "the turn is gone",
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().any(|r| r.method == "session.state"));
        assert!(requests.iter().any(|r| r.method == "session.transcript"));
        assert!(
            app.notices
                .iter()
                .any(|n| n.text.contains("turn wait failed"))
        );
        // The live turn survives until the durable truth arrives.
        assert!(app.sessions.known["ses_1"].live.is_some());
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            transcript_request,
            page_json(vec![user_entry(1, "trn_1", "hello")], None, 1, true),
        ));
        assert!(app.sessions.known["ses_1"].live.is_none());
        assert!(!app.sessions.known["ses_1"].event_gap);
    }

    #[test]
    fn stale_instance_events_never_pollute_the_current_instance() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        // A state event from an older instance is ignored.
        take_requests(app.update(event(wire_event(json!({
            "type": "session_state",
            "data": {
                "state": state_json("ses_1", "ins_0", "running"),
                "meta": meta_json("ses_1", "ins_0", 0)
            }
        })))));
        assert_eq!(
            app.sessions.known["ses_1"]
                .state
                .as_ref()
                .unwrap()
                .instance_id,
            "ins_1"
        );

        // A closing event from the old instance must not kill the live turn.
        submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        take_requests(app.update(event(wire_event(json!({
            "type": "session_closed",
            "data": {"session_id": "ses_1", "meta": meta_json("ses_1", "ins_0", 0)}
        })))));
        assert!(app.sessions.known["ses_1"].live.is_some());

        // Turn deltas for the old instance never reach the live view.
        let stale = make_turn("ses_1", "ins_0", "trn_1");
        take_requests(app.update(delta_event(&stale, "text", "WRONG")));
        assert_eq!(app.sessions.known["ses_1"].live.as_ref().unwrap().text, "");

        // The current instance keeps working.
        take_requests(app.update(delta_event(&turn, "text", "right")));
        assert_eq!(
            app.sessions.known["ses_1"].live.as_ref().unwrap().text,
            "right"
        );
    }

    #[test]
    fn background_sessions_keep_their_turns_while_switching() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_a", "ins_a");
        open_session(&mut app, "ses_b", "ins_b");
        assert_eq!(app.sessions.active.as_deref(), Some("ses_b"));

        let send_a = submit(&mut app, "ses_a", "a");
        let turn_a = make_turn("ses_a", "ins_a", "trn_a");
        take_requests(app.update(turn_started_event(&turn_a)));
        let wait_a = {
            let commands = respond(
                &mut app,
                &send_a,
                json!({"turn": turn_ref_json("ses_a", "ins_a", "trn_a")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let send_b = submit(&mut app, "ses_b", "b");
        let turn_b = make_turn("ses_b", "ins_b", "trn_b");
        take_requests(app.update(turn_started_event(&turn_b)));
        let wait_b = {
            let commands = respond(
                &mut app,
                &send_b,
                json!({"turn": turn_ref_json("ses_b", "ins_b", "trn_b")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };

        // Interleaved deltas never cross sessions.
        take_requests(app.update(delta_event(&turn_a, "text", "a1")));
        take_requests(app.update(delta_event(&turn_b, "text", "b1")));
        take_requests(app.update(delta_event(&turn_a, "text", "a2")));
        assert_eq!(
            app.sessions.known["ses_a"].live.as_ref().unwrap().text,
            "a1a2"
        );
        assert_eq!(
            app.sessions.known["ses_b"].live.as_ref().unwrap().text,
            "b1"
        );

        // Switching back to A keeps B's background state intact.
        let open_a = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_a".into(),
        }));
        take_requests(respond(
            &mut app,
            &open_a[0],
            json!({"session": session_info("ses_a", Some("ins_a"))}),
        ));
        assert_eq!(app.sessions.active.as_deref(), Some("ses_a"));
        assert_eq!(
            app.sessions.known["ses_b"].live.as_ref().unwrap().text,
            "b1"
        );

        // A's turn reconciles; B's wait is still pending and unaffected.
        let commands = respond(&mut app, &wait_a, outcome_json("trn_a", "completed"));
        let requests = take_requests(commands);
        let transcript_a = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            transcript_a,
            page_json(vec![user_entry(1, "trn_a", "a")], None, 1, true),
        ));
        assert!(app.sessions.known["ses_a"].live.is_none());
        assert!(app.pending_requests.contains_key(&wait_b.id));
        assert!(app.sessions.known["ses_b"].live.is_some());
    }

    #[test]
    fn waiting_for_input_shows_the_fixed_notice_and_never_answers() {
        let mut app = test_app();
        ready(&mut app);
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            transcript_request,
            page_json(vec![], None, 0, true),
        ));
        let waiting = json!({
            "session_id": "ses_1",
            "instance_id": "ins_1",
            "status": "waiting_for_input",
            "health": "healthy",
            "active_turn": "trn_1",
            "pending_interaction": {
                "interaction_id": "int_1",
                "turn_id": "trn_1",
                "tool_call_id": "call_1",
                "tool_name": "ask",
                "kind": {"type": "approval", "data": {}}
            },
            "conversation_seq": 1,
            "last_terminal": null
        });
        let commands = respond(&mut app, state_request, waiting.clone());
        // No interaction.answer or any other command is ever sent.
        assert!(take_requests(commands).is_empty());
        assert_eq!(app.notices.len(), 1);
        assert_eq!(app.notices[0].text, UNSUPPORTED_INTERACTION_NOTICE);
        assert!(app.notices[0].sticky);

        // The event path shows the same notice once, then stays quiet.
        take_requests(app.update(event(wire_event(json!({
            "type": "session_state",
            "data": {"state": waiting.clone(), "meta": meta_json("ses_1", "ins_1", 0)}
        })))));
        take_requests(app.update(event(wire_event(json!({
            "type": "interaction_requested",
            "data": {
                "session_id": "ses_1",
                "interaction": {
                    "interaction_id": "int_2",
                    "turn_id": "trn_1",
                    "tool_call_id": "call_2",
                    "tool_name": "ask",
                    "kind": {"type": "approval"}
                },
                "meta": meta_json("ses_1", "ins_1", 0)
            }
        })))));
        assert_eq!(
            app.notices
                .iter()
                .filter(|n| n.text == UNSUPPORTED_INTERACTION_NOTICE)
                .count(),
            1
        );
    }

    #[test]
    fn connection_termination_is_idempotent_and_first_fatal_wins() {
        let mut app = test_app();
        app.update(AppEvent::Rpc(RpcEvent::ProtocolError(FrameError::new(
            FrameErrorKind::InvalidEnvelope,
            "bad frame",
        ))));
        assert!(matches!(
            &app.connection,
            ConnectionState::Failed(reason) if reason.contains("RPC protocol error")
        ));
        app.update(AppEvent::Rpc(RpcEvent::ConnectionClosed));
        app.update(AppEvent::Rpc(RpcEvent::Exited(None)));
        assert!(matches!(
            &app.connection,
            ConnectionState::Failed(reason) if reason.contains("RPC protocol error")
        ));

        let mut second = test_app();
        second.update(AppEvent::Rpc(RpcEvent::Exited(None)));
        second.update(AppEvent::Rpc(RpcEvent::ConnectionClosed));
        second.update(AppEvent::Rpc(RpcEvent::ProtocolError(FrameError::new(
            FrameErrorKind::Io,
            "pipe",
        ))));
        assert!(matches!(
            &second.connection,
            ConnectionState::Failed(reason) if reason.contains("agent exited")
        ));
    }

    #[test]
    fn agent_log_ring_bounds_at_200_lines() {
        let mut app = test_app();
        for i in 0..250 {
            app.update(AppEvent::Rpc(RpcEvent::AgentLogLine(format!("line {i}"))));
        }
        assert_eq!(app.agent_logs.len(), MAX_AGENT_LOG_LINES);
        assert_eq!(app.agent_logs.front().map(String::as_str), Some("line 50"));
        assert_eq!(app.agent_logs.back().map(String::as_str), Some("line 249"));
    }

    #[test]
    fn malformed_transcript_result_notices_and_stops_pagination() {
        let mut app = test_app();
        ready(&mut app);
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        let commands = respond(
            &mut app,
            transcript_request,
            json!({"entries": [{"user_message": {"seq": 1}}], "next_after": null,
                   "observed_head": 1, "complete": true}),
        );
        assert!(take_requests(commands).is_empty());
        assert!(
            app.notices
                .iter()
                .any(|n| n.text.contains("malformed transcript"))
        );
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading);
        assert!(!view.reconcile_inflight);
    }

    #[test]
    fn app_has_no_external_mutation_entry_points() {
        // The only way to change state is `App::update`; everything else is
        // read-only. This test pins the constructor shape so future phases
        // cannot add a back door by accident (spec 9.1).
        let app = test_app();
        let _ = app.connection;
        let _ = app.catalogs.loaded;
        let _ = app.sessions.active.clone();
        let _ = app.pending_requests.len();
        let _ = app.notices.len();
        let _ = app.agent_logs.len();
        let _ = app.recovered_input.clone();
        let _ = std::mem::size_of::<SessionInfo>();
    }

    fn gapped_turn_started(turn_ref: &TurnRef, dropped: u64) -> AppEvent {
        event(wire_event(json!({
            "type": "turn_started",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, dropped)
            }
        })))
    }

    fn gapped_tool_started(turn_ref: &TurnRef, id: &str, name: &str, dropped: u64) -> AppEvent {
        event(wire_event(json!({
            "type": "tool_started",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "tool_call_id": id,
                "tool_name": name,
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, dropped)
            }
        })))
    }

    fn gapped_turn_finished(turn_ref: &TurnRef, dropped: u64) -> AppEvent {
        event(wire_event(json!({
            "type": "turn_finished",
            "data": {
                "turn": turn_ref_json(&turn_ref.session_id, &turn_ref.instance_id, &turn_ref.turn_id),
                "outcome": outcome_json(&turn_ref.turn_id, "completed"),
                "meta": meta_json(&turn_ref.session_id, &turn_ref.instance_id, dropped)
            }
        })))
    }

    fn gapped_session_state(session_id: &str, instance: &str, dropped: u64) -> AppEvent {
        event(wire_event(json!({
            "type": "session_state",
            "data": {
                "state": state_json(session_id, instance, "idle"),
                "meta": meta_json(session_id, instance, dropped)
            }
        })))
    }

    /// Opens a session and returns the id of the first transcript page
    /// request without answering it.
    fn open_transcript_id(app: &mut App, session_id: &str) -> RequestId {
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: session_id.into(),
        }));
        let commands = respond(
            app,
            &open[0],
            json!({"session": session_info(session_id, Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript request")
            .id
    }

    fn respond_by_id(app: &mut App, id: RequestId, result: Value) -> Vec<AppCommand> {
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id,
                result: Some(result),
                error: None,
            },
        ))))
    }

    #[test]
    fn transcript_without_cursor_does_not_stick_or_fabricate_completion() {
        let mut app = test_app();
        ready(&mut app);
        let transcript_id = open_transcript_id(&mut app, "ses_1");
        // complete=false with no cursor and no entries; the head is ahead of
        // anything merged (reachable until the agent confirms durability).
        let commands = respond_by_id(&mut app, transcript_id, page_json(vec![], None, 5, false));
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading, "the chain must not stay stuck");
        assert!(
            !view.transcript.complete,
            "completion must not be fabricated"
        );
        assert_eq!(
            view.transcript.last_seq, None,
            "an empty page must not advance last_seq (not even to observed_head)"
        );
        assert!(app.notices.iter().any(|n| n.text.contains("durable")));

        // A later open starts from scratch: nothing durable was merged, so
        // the fetch is a full one with no cursor.
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");
        assert_eq!(
            app.pending_requests.get(&transcript_request.id),
            Some(&RequestKind::Transcript {
                session_id: "ses_1".into(),
                after: None,
                gap_revision: None,
            })
        );
        // The resumed fetch merges normally and the chain completes.
        let commands = respond_by_id(
            &mut app,
            transcript_request.id,
            page_json(vec![user_entry(1, "trn_1", "hello")], None, 1, true),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(view.transcript.complete);
        assert_eq!(view.transcript.last_seq, Some(1));
    }

    #[test]
    fn transcript_without_cursor_keeps_the_merged_entries() {
        let mut app = test_app();
        ready(&mut app);
        let transcript_id = open_transcript_id(&mut app, "ses_1");
        let page = page_json(
            vec![
                user_entry(1, "trn_1", "hello"),
                assistant_entry(2, "trn_1", "back"),
            ],
            None,
            3,
            false,
        );
        let commands = respond_by_id(&mut app, transcript_id, page);
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading);
        assert!(!view.transcript.complete);
        assert_eq!(view.transcript.blocks.len(), 2);
        assert_eq!(
            view.transcript.last_seq,
            Some(2),
            "last_seq is the highest merged entry, not the observed head"
        );
    }

    #[test]
    fn reopening_a_gapped_session_heals_with_an_incremental_fetch() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        assert!(app.sessions.known["ses_1"].transcript.complete);
        // A gap over an already-complete transcript.
        take_requests(app.update(gapped_turn_started(
            &make_turn("ses_1", "ins_1", "ghost"),
            3,
        )));
        assert!(app.sessions.known["ses_1"].event_gap);
        assert_eq!(app.sessions.known["ses_1"].gap_revision, 1);

        // Switching back to the session must issue an incremental fetch even
        // though the transcript is complete (spec 13.7).
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");
        assert_eq!(
            app.pending_requests.get(&transcript_request.id),
            Some(&RequestKind::Transcript {
                session_id: "ses_1".into(),
                after: None,
                gap_revision: Some(1),
            })
        );
        // Once the heal chain completes, the gap is gone.
        let commands = respond(
            &mut app,
            transcript_request,
            page_json(vec![], None, 0, true),
        );
        assert!(take_requests(commands).is_empty());
        assert!(!app.sessions.known["ses_1"].event_gap);
    }

    #[test]
    fn a_stale_complete_response_never_clears_a_newer_gap() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        take_requests(app.update(gapped_turn_started(
            &make_turn("ses_1", "ins_1", "ghost"),
            3,
        )));
        assert_eq!(app.sessions.known["ses_1"].gap_revision, 1);

        // Heal chain issued under revision 1.
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let heal_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");

        // A new drop lands while the chain is in flight.
        take_requests(app.update(gapped_turn_started(
            &make_turn("ses_1", "ins_1", "ghost2"),
            2,
        )));
        assert_eq!(app.sessions.known["ses_1"].gap_revision, 2);

        // The stale completion must NOT clear the newer gap.
        let commands = respond(&mut app, heal_request, page_json(vec![], None, 0, true));
        assert!(take_requests(commands).is_empty());
        assert!(app.sessions.known["ses_1"].event_gap);

        // A fresh heal chain issued under revision 2 finally clears it.
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let heal_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");
        assert_eq!(
            app.pending_requests.get(&heal_request.id),
            Some(&RequestKind::Transcript {
                session_id: "ses_1".into(),
                after: None,
                gap_revision: Some(2),
            })
        );
        let commands = respond(&mut app, heal_request, page_json(vec![], None, 0, true));
        assert!(take_requests(commands).is_empty());
        assert!(!app.sessions.known["ses_1"].event_gap);
    }

    #[test]
    fn stale_instance_turn_started_is_rejected_before_the_send_response() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        submit(&mut app, "ses_1", "hello");
        let stale = make_turn("ses_1", "ins_0", "trn_1");
        let fresh = make_turn("ses_1", "ins_1", "trn_1");

        take_requests(app.update(turn_started_event(&stale)));
        assert!(
            app.sessions.known["ses_1"]
                .live
                .as_ref()
                .unwrap()
                .reference
                .is_none(),
            "a stale instance must never be adopted"
        );
        take_requests(app.update(delta_event(&stale, "text", "WRONG")));
        assert_eq!(app.sessions.known["ses_1"].live.as_ref().unwrap().text, "");

        take_requests(app.update(turn_started_event(&fresh)));
        assert_eq!(
            app.sessions.known["ses_1"].live.as_ref().unwrap().reference,
            Some(fresh.clone())
        );
        take_requests(app.update(delta_event(&fresh, "text", "right")));
        assert_eq!(
            app.sessions.known["ses_1"].live.as_ref().unwrap().text,
            "right"
        );
    }

    #[test]
    fn tool_progress_never_downgrades_a_terminal_status() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        submit(&mut app, "ses_1", "use tools");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        take_requests(app.update(tool_started_event(&turn, "call-1", "read")));
        take_requests(app.update(tool_progress_event(&turn, "call-1", "50%")));
        take_requests(app.update(tool_finished_event(&turn, "call-1", "success")));
        // Late progress must neither downgrade the status nor touch the text.
        take_requests(app.update(tool_progress_event(&turn, "call-1", "90%")));
        let first = app.sessions.known["ses_1"]
            .live
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .find(|t| t.tool_call_id == "call-1")
            .unwrap();
        assert_eq!(first.status, ToolStatus::Succeeded);
        assert_eq!(first.progress.as_deref(), Some("50%"));

        take_requests(app.update(tool_started_event(&turn, "call-2", "write")));
        take_requests(app.update(tool_finished_event(&turn, "call-2", "denied")));
        take_requests(app.update(tool_progress_event(&turn, "call-2", "10%")));
        let second = app.sessions.known["ses_1"]
            .live
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .find(|t| t.tool_call_id == "call-2")
            .unwrap();
        assert_eq!(second.status, ToolStatus::Denied);
        assert_eq!(second.progress, None);
    }

    #[test]
    fn user_actions_are_noops_outside_ready() {
        let mut app = test_app(); // Starting
        for event in [
            AppEvent::SubmitTurn {
                session_id: "s".into(),
                text: "hi".into(),
            },
            AppEvent::CreateSession {
                workspace: "/".into(),
                profile: None,
                model: None,
                reasoning: None,
                title: None,
            },
            AppEvent::OpenSession {
                session_id: "s".into(),
            },
            AppEvent::CancelTurn {
                session_id: "s".into(),
            },
        ] {
            assert!(
                take_requests(app.update(event)).is_empty(),
                "no command outside Ready"
            );
        }
        assert!(app.pending_requests.is_empty());
        assert_eq!(
            app.notices
                .iter()
                .filter(|n| n.text.contains("unavailable"))
                .count(),
            1,
            "one guarded notice, no flood"
        );

        // A failed connection stays fully gated.
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let models = requests.iter().find(|r| r.method == "model.list").unwrap();
        take_requests(respond_error(
            &mut app,
            models,
            "models_unavailable",
            "boom",
        ));
        for request in requests.iter().filter(|r| r.method != "model.list") {
            let result = match request.method {
                "agent.ping" => json!({"version": "0.2.0"}),
                "profile.list" => json!({"profiles": []}),
                "session.list" => json!({"sessions": []}),
                _ => unreachable!(),
            };
            take_requests(respond(&mut app, request, result));
        }
        assert!(matches!(app.connection, ConnectionState::Failed(_)));
        assert!(
            take_requests(app.update(AppEvent::SubmitTurn {
                session_id: "s".into(),
                text: "hi".into(),
            }))
            .is_empty()
        );
        assert!(
            take_requests(app.update(AppEvent::CreateSession {
                workspace: "/".into(),
                profile: None,
                model: None,
                reasoning: None,
                title: None,
            }))
            .is_empty()
        );
        assert!(
            take_requests(app.update(AppEvent::OpenSession {
                session_id: "s".into(),
            }))
            .is_empty()
        );
        assert!(
            take_requests(app.update(AppEvent::CancelTurn {
                session_id: "s".into(),
            }))
            .is_empty()
        );
        assert!(app.pending_requests.is_empty());
        assert_eq!(
            app.notices
                .iter()
                .filter(|n| n.text.contains("unavailable"))
                .count(),
            1
        );
    }

    #[test]
    fn wait_during_initial_pagination_defers_reconciliation_then_finishes() {
        let mut app = test_app();
        ready(&mut app);
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let state_request = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let first_page = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            state_request,
            state_json("ses_1", "ins_1", "idle"),
        ));
        // The initial chain is still paging.
        let commands = respond(&mut app, first_page, page_json(vec![], Some(2), 2, false));
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        let second_page = &requests[0];

        // A turn completes while the initial chain is in flight.
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond(&mut app, &wait_request, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        // The chain is busy, so only the state refresh goes out now.
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.state");

        // The initial chain completes; the deferred reconcile starts.
        let commands = respond(
            &mut app,
            second_page,
            page_json(vec![terminal_entry(1, "trn_1")], None, 1, true),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.transcript");
        let reconcile_request = requests.into_iter().next().unwrap();
        let commands = respond(
            &mut app,
            &reconcile_request,
            page_json(vec![], None, 1, true),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(
            view.live.is_none(),
            "reconcile finished and dropped the live turn"
        );
        assert!(!view.loading);
    }

    #[test]
    fn two_full_rounds_reconcile_into_one_durable_transcript() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");

        // Round 1.
        let send1 = submit(&mut app, "ses_1", "first");
        let turn1 = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn1)));
        let wait1 = {
            let commands = respond(
                &mut app,
                &send1,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond(&mut app, &wait1, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        let state1 = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let transcript1 = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            state1,
            state_json("ses_1", "ins_1", "idle"),
        ));
        take_requests(respond(
            &mut app,
            transcript1,
            page_json(
                vec![
                    user_entry(1, "trn_1", "first"),
                    assistant_entry(2, "trn_1", "answer one"),
                    terminal_entry(3, "trn_1"),
                ],
                None,
                3,
                true,
            ),
        ));
        assert!(app.sessions.known["ses_1"].live.is_none());

        // Round 2 over the same session.
        let send2 = submit(&mut app, "ses_1", "second");
        let turn2 = make_turn("ses_1", "ins_1", "trn_2");
        take_requests(app.update(turn_started_event(&turn2)));
        let wait2 = {
            let commands = respond(
                &mut app,
                &send2,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_2")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond(&mut app, &wait2, outcome_json("trn_2", "completed"));
        let requests = take_requests(commands);
        let state2 = requests
            .iter()
            .find(|r| r.method == "session.state")
            .unwrap();
        let transcript2 = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        take_requests(respond(
            &mut app,
            state2,
            state_json("ses_1", "ins_1", "idle"),
        ));
        take_requests(respond(
            &mut app,
            transcript2,
            page_json(
                vec![
                    user_entry(4, "trn_2", "second"),
                    assistant_entry(5, "trn_2", "answer two"),
                    terminal_entry(6, "trn_2"),
                ],
                None,
                6,
                true,
            ),
        ));

        let view = &app.sessions.known["ses_1"];
        assert!(view.live.is_none());
        assert_eq!(view.transcript.blocks.len(), 6);
        let users: Vec<&UserBlock> = view
            .transcript
            .blocks
            .iter()
            .filter_map(|block| match block {
                TranscriptBlock::User(user) => Some(user),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].text, "first");
        assert_eq!(users[1].text, "second");
    }

    #[test]
    fn duplicate_response_ids_are_noticed_but_never_applied_twice() {
        let mut app = test_app();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        let ping_id = requests
            .iter()
            .find(|r| r.method == "agent.ping")
            .unwrap()
            .id;
        for request in &requests {
            let result = match request.method {
                "agent.ping" => json!({"version": "0.2.0"}),
                "model.list" => json!({"models": []}),
                "profile.list" => json!({"profiles": []}),
                "session.list" => json!({"sessions": []}),
                other => panic!("unexpected request: {other}"),
            };
            take_requests(respond(&mut app, request, result));
        }
        assert_eq!(app.connection, ConnectionState::Ready);

        // Re-delivering an already-consumed id: notice only, state untouched.
        let commands = app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: ping_id,
                result: Some(json!({"version": "0.2.0"})),
                error: None,
            },
        ))));
        assert!(take_requests(commands).is_empty());
        assert!(
            app.notices
                .iter()
                .any(|n| n.text.contains("unknown request id"))
        );
        assert_eq!(app.connection, ConnectionState::Ready);
        assert!(app.pending_requests.is_empty());
    }

    #[test]
    fn dropped_before_marks_gaps_across_all_event_kinds() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let turn = make_turn("ses_1", "ins_1", "trn_1");

        take_requests(app.update(gapped_turn_started(&turn, 2)));
        take_requests(app.update(gapped_tool_started(&turn, "c1", "read", 1)));
        take_requests(app.update(gapped_turn_finished(&turn, 1)));
        take_requests(app.update(gapped_session_state("ses_1", "ins_1", 1)));

        let view = &app.sessions.known["ses_1"];
        assert!(view.event_gap);
        assert_eq!(
            view.gap_revision, 4,
            "each dropped event bumps the revision"
        );
        assert!(
            view.transcript.blocks.is_empty(),
            "gap marking has no other side effects"
        );
    }

    #[test]
    fn tool_result_without_its_call_becomes_a_durable_block() {
        let mut app = test_app();
        ready(&mut app);
        let transcript_id = open_transcript_id(&mut app, "ses_1");
        let page = page_json(
            vec![json!({"tool_result": {
                "seq": 1,
                "turn_id": "trn_1",
                "tool_call_id": "call-9",
                "tool_name": "write",
                "outcome": "success",
                "content": "durable result",
                "created_at": "2026-01-02T03:04:05.006Z"
            }})],
            None,
            1,
            true,
        );
        let commands = respond_by_id(&mut app, transcript_id, page);
        assert!(take_requests(commands).is_empty());
        let blocks = &app.sessions.known["ses_1"].transcript.blocks;
        let tool = match &blocks[0] {
            TranscriptBlock::Tool(tool) => tool,
            other => panic!("expected a tool block, got: {other:?}"),
        };
        assert_eq!(tool.tool_call_id, "call-9");
        assert_eq!(tool.result.as_deref(), Some("durable result"));
        assert_eq!(tool.outcome.as_deref(), Some("success"));
    }

    #[test]
    fn summary_entries_dedupe_by_seq_across_pages() {
        let mut app = test_app();
        ready(&mut app);
        let transcript_id = open_transcript_id(&mut app, "ses_1");
        let summary = |seq: u64| {
            json!({"summary": {
                "seq": seq,
                "through": 3,
                "summary": "durable summary",
                "created_at": "2026-01-02T03:04:05.006Z"
            }})
        };
        let commands = respond_by_id(
            &mut app,
            transcript_id,
            page_json(vec![summary(4)], Some(4), 4, false),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        let continuation_id = requests[0].id;
        take_requests(respond_by_id(
            &mut app,
            continuation_id,
            page_json(vec![summary(4), user_entry(5, "trn_1", "x")], None, 5, true),
        ));
        let view = &app.sessions.known["ses_1"];
        let summaries = view
            .transcript
            .blocks
            .iter()
            .filter(|b| matches!(b, TranscriptBlock::Summary(_)))
            .count();
        assert_eq!(summaries, 1, "the summary from both pages is stored once");
        assert_eq!(view.transcript.blocks.len(), 2);
    }

    #[test]
    fn oversized_send_failure_recovers_input_through_the_executor_event() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let big = "x".repeat(crate::rpc::MAX_REQUEST_LINE_BYTES + 1_000);
        let send_request = submit(&mut app, "ses_1", &big);
        let commands = app.update(AppEvent::RpcSendFailed {
            id: send_request.id,
            error: RpcError::RequestTooLarge {
                actual_bytes: big.len(),
                max_bytes: crate::rpc::MAX_REQUEST_LINE_BYTES,
            },
        });
        assert!(take_requests(commands).is_empty());
        assert!(app.sessions.known["ses_1"].live.is_none());
        assert_eq!(app.recovered_input.as_deref(), Some(big.as_str()));
        assert!(!app.pending_requests.contains_key(&send_request.id));
    }

    #[test]
    fn stopped_page_with_summary_keeps_the_merged_tail_as_cursor() {
        let mut app = test_app();
        ready(&mut app);
        let transcript_id = open_transcript_id(&mut app, "ses_1");
        // A compaction projection: one summary entry whose seq sits below the
        // observed head, delivered on a page that stops without a cursor.
        let commands = respond_by_id(
            &mut app,
            transcript_id,
            page_json(
                vec![json!({"summary": {
                    "seq": 3,
                    "through": 10,
                    "summary": "compact",
                    "created_at": "2026-01-02T03:04:05.006Z"
                }})],
                None,
                10,
                false,
            ),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading);
        assert!(!view.transcript.complete);
        assert_eq!(
            view.transcript.last_seq,
            Some(3),
            "the cursor is the last merged entry, not the observed head 10"
        );

        // Re-opening resumes from the actual durable tail: 3, never 10.
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");
        assert_eq!(
            app.pending_requests.get(&transcript_request.id),
            Some(&RequestKind::Transcript {
                session_id: "ses_1".into(),
                after: Some(3),
                gap_revision: None,
            })
        );
    }

    #[test]
    fn stopped_empty_page_preserves_the_prior_last_seq() {
        let mut app = test_app();
        ready(&mut app);
        let transcript_id = open_transcript_id(&mut app, "ses_1");
        // Page 1 merges one entry and returns a cursor.
        let commands = respond_by_id(
            &mut app,
            transcript_id,
            page_json(vec![user_entry(1, "trn_1", "hello")], Some(1), 1, false),
        );
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(app.sessions.known["ses_1"].transcript.last_seq, Some(1));
        // Page 2 stops without a cursor and without entries; the head has
        // moved ahead but nothing was merged.
        let commands = respond_by_id(&mut app, requests[0].id, page_json(vec![], None, 5, false));
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(!view.loading);
        assert!(!view.transcript.complete);
        assert_eq!(
            view.transcript.last_seq,
            Some(1),
            "an empty stopped page must not advance last_seq to 5"
        );
        assert_eq!(view.transcript.blocks.len(), 1);

        // Re-opening resumes after 1 so nothing is skipped.
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");
        assert_eq!(
            app.pending_requests.get(&transcript_request.id),
            Some(&RequestKind::Transcript {
                session_id: "ses_1".into(),
                after: Some(1),
                gap_revision: None,
            })
        );
        // Resuming with the missing entries heals the tail (2..5 never lost).
        let commands = respond_by_id(
            &mut app,
            transcript_request.id,
            page_json(
                vec![
                    user_entry(2, "trn_1", "resumed two"),
                    user_entry(5, "trn_1", "resumed five"),
                ],
                None,
                5,
                true,
            ),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(view.transcript.complete);
        assert_eq!(view.transcript.last_seq, Some(5));
        assert_eq!(view.transcript.blocks.len(), 3);
    }

    #[test]
    fn stopped_reconcile_chain_keeps_live_and_gap_for_a_later_fetch() {
        let mut app = test_app();
        ready(&mut app);
        open_session(&mut app, "ses_1", "ins_1");
        let send_request = submit(&mut app, "ses_1", "hello");
        let turn = make_turn("ses_1", "ins_1", "trn_1");
        take_requests(app.update(turn_started_event(&turn)));
        let wait_request = {
            let commands = respond(
                &mut app,
                &send_request,
                json!({"turn": turn_ref_json("ses_1", "ins_1", "trn_1")}),
            );
            take_requests(commands).into_iter().next().unwrap()
        };
        let commands = respond(&mut app, &wait_request, outcome_json("trn_1", "completed"));
        let requests = take_requests(commands);
        let transcript_request = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .unwrap();
        // A gap arrives around the reconcile.
        take_requests(app.update(gapped_turn_started(&turn, 2)));
        assert!(app.sessions.known["ses_1"].event_gap);

        // The reconcile chain stops without a cursor: the live turn and the
        // gap survive, and the flags reset so a later action can retry.
        let commands = respond(
            &mut app,
            transcript_request,
            page_json(vec![terminal_entry(1, "trn_1")], None, 1, false),
        );
        assert!(take_requests(commands).is_empty());
        let view = &app.sessions.known["ses_1"];
        assert!(
            view.live.is_some(),
            "a stopped reconcile must keep the live turn"
        );
        assert!(view.live.as_ref().unwrap().waiting);
        assert!(view.event_gap, "a stopped reconcile must keep the gap");
        assert!(!view.loading, "flags reset for a later retry");
        assert!(!view.reconcile_inflight);
        assert!(!view.transcript.complete, "completion is not fabricated");
        assert_eq!(view.transcript.last_seq, Some(1));

        // A later explicit open resumes and finishes the reconcile.
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_1".into(),
        }));
        let commands = respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_1", Some("ins_1"))}),
        );
        let requests = take_requests(commands);
        let first = requests
            .iter()
            .find(|r| r.method == "session.transcript")
            .expect("transcript");
        let commands = respond(&mut app, first, page_json(vec![], None, 1, true));
        // The open chain completed; the deferred reconcile fires because the
        // live turn is still waiting.
        let requests = take_requests(commands);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "session.transcript");
        let commands = respond(&mut app, &requests[0], page_json(vec![], None, 1, true));
        assert!(take_requests(commands).is_empty());

        let view = &app.sessions.known["ses_1"];
        assert!(
            view.live.is_none(),
            "the durable reconcile finally removed the live turn"
        );
        assert!(view.transcript.complete);
        assert!(!view.event_gap, "the healed chain cleared the gap");
        assert_eq!(
            view.transcript.last_seq,
            Some(1),
            "no empty page advances last_seq"
        );
    }

    // ---- Phase 4: dock and selectors ----------------------------------

    fn catalog_json() -> (Value, Value, Value) {
        (
            json!([
                {"id": "deep", "model_ref": "minicore/deep:v1", "context_window": 128000,
                 "supports_tools": true, "supported_reasoning": ["auto", "low", "medium", "high"]},
                {"id": "fast", "model_ref": "minicore/fast:v1", "context_window": 32000,
                 "supports_tools": false, "supported_reasoning": ["low", "medium"]},
                {"id": "tiny", "model_ref": "minicore/tiny:v1", "context_window": 8000,
                 "supports_tools": true, "supported_reasoning": ["disabled", "low"]}
            ]),
            json!([
                {"id": "coding", "model": "deep", "reasoning": "high",
                 "tools": ["read", "edit", "bash"]},
                {"id": "review", "model": "fast", "reasoning": "medium", "tools": ["read"]}
            ]),
            json!([
                {"session_id": "ses_a", "title": "Alpha", "profile": "coding",
                 "workspace": "/a", "model": "deep", "reasoning": "high",
                 "loaded": true, "instance_id": "i1",
                 "created_at": "2027-01-15T08:00:00.000Z", "updated_at": "2027-01-15T08:00:00.000Z"},
                {"session_id": "ses_b", "title": "Beta", "profile": "review",
                 "workspace": "/b", "model": "fast", "reasoning": "medium",
                 "loaded": false, "instance_id": null,
                 "created_at": "2027-01-15T07:00:00.000Z", "updated_at": "2027-01-15T07:00:00.000Z"}
            ]),
        )
    }

    fn ready_with_catalogs(app: &mut App) {
        let (models, profiles, sessions) = catalog_json();
        let requests = take_requests(app.update(AppEvent::Bootstrap));
        assert_eq!(requests.len(), 4);
        for request in &requests {
            let result = match request.method {
                "agent.ping" => json!({"version": "0.2.0"}),
                "model.list" => json!({"models": models.clone()}),
                "profile.list" => json!({"profiles": profiles.clone()}),
                "session.list" => json!({"sessions": sessions.clone()}),
                other => panic!("unexpected bootstrap request: {other}"),
            };
            take_requests(respond(app, request, result));
        }
        assert_eq!(app.connection, ConnectionState::Ready);
    }

    fn draft(app: &App) -> NewSessionState {
        app.new_session()
            .expect("a new-session draft exists")
            .clone()
    }

    #[test]
    fn open_new_session_seeds_the_draft_from_catalog_defaults() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let state = draft(&app);
        assert_eq!(state.workspace, "/workspace");
        assert_eq!(state.profile, "coding", "first profile becomes the default");
        assert_eq!(state.model, "deep", "profile default model");
        assert_eq!(
            state.reasoning,
            Reasoning::High,
            "profile default reasoning"
        );
        assert!(!state.submitting);
        assert!(matches!(&app.dock, Dock::NewSession(_)));
    }

    #[test]
    fn dock_actions_are_gated_until_ready() {
        let mut app = test_app();
        app.update(AppEvent::OpenNewSession);
        assert_eq!(app.dock, Dock::Composer, "not Ready yet");
        let models = take_requests(app.update(AppEvent::Bootstrap))
            .into_iter()
            .map(|r| r.method)
            .collect::<Vec<_>>();
        assert_eq!(models.len(), 4);
    }

    #[test]
    fn open_model_selector_from_the_composer_creates_a_draft_and_preselects() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenModelSelector);
        assert!(matches!(app.dock, Dock::ModelSelector(_)));
        // Keys off the draft model default.
        let cursor = match &app.dock {
            Dock::ModelSelector(state) => state.cursor,
            _ => panic!("model selector"),
        };
        assert_eq!(cursor, 0, "deep is the first model and the draft default");
    }

    #[test]
    fn selecting_a_model_never_touches_the_current_session() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        let open = take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_a".into(),
        }));
        take_requests(respond(
            &mut app,
            &open[0],
            json!({"session": session_info("ses_a", Some("i1"))}),
        ));
        assert_eq!(app.sessions.active.as_deref(), Some("ses_a"));
        let original_info = app.sessions.known["ses_a"].info.clone();

        app.update(AppEvent::OpenModelSelector);
        app.update(AppEvent::MoveSelector { delta: 1 }); // fast
        app.update(AppEvent::ConfirmDock); // -> draft.model=fast, opens reasoning
        assert!(matches!(app.dock, Dock::ReasoningSelector(_)));
        assert_eq!(draft(&app).model, "fast");
        // The current session is untouched in every layer.
        assert_eq!(app.sessions.known["ses_a"].info, original_info);
        assert_eq!(
            app.catalogs.next_model.as_deref(),
            Some("deep"),
            "catalog defaults untouched"
        );
    }

    #[test]
    fn incompatible_reasoning_is_kept_and_reasoning_selector_still_opens() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        // Draft.reasoning is high; fast only supports low/medium.
        app.update(AppEvent::OpenModelSelector);
        app.update(AppEvent::MoveSelector { delta: 1 }); // fast
        app.update(AppEvent::ConfirmDock);
        assert!(matches!(app.dock, Dock::ReasoningSelector(_)));
        assert_eq!(
            draft(&app).reasoning,
            Reasoning::High,
            "kept, never downgraded"
        );
        assert!(
            app.notices
                .iter()
                .any(|n| n.text.contains("may not support"))
        );
        // The reasoning selector only lists what fast supports.
        let supported = supported_reasoning(&app.catalogs.models, "fast");
        assert_eq!(supported, vec![Reasoning::Low, Reasoning::Medium]);
    }

    #[test]
    fn reasoning_selector_confirms_only_supported_values_into_the_draft() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenReasoningSelector);
        // The cursor preselects the draft value (high, index 3); step back
        // to medium (index 2) and confirm.
        app.update(AppEvent::MoveSelector { delta: -1 });
        app.update(AppEvent::ConfirmDock);
        assert!(matches!(app.dock, Dock::NewSession(_)));
        assert_eq!(draft(&app).reasoning, Reasoning::Medium);
    }

    #[test]
    fn reasoning_selector_with_unknown_model_is_empty_and_unconfirmable() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        if let Some(draft) = app.draft_mut() {
            draft.model = "no-such-model".into();
        }
        app.update(AppEvent::OpenReasoningSelector);
        assert!(matches!(app.dock, Dock::ReasoningSelector(_)));
        app.update(AppEvent::ConfirmDock);
        assert!(
            matches!(app.dock, Dock::ReasoningSelector(_)),
            "nothing to confirm for an unknown model"
        );
    }

    #[test]
    fn profile_selection_adopts_the_profile_defaults() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        app.update(AppEvent::OpenProfileSelector);
        app.update(AppEvent::MoveSelector { delta: 1 }); // review
        app.update(AppEvent::ConfirmDock);
        assert!(matches!(app.dock, Dock::NewSession(_)));
        let state = draft(&app);
        assert_eq!(state.profile, "review");
        assert_eq!(state.model, "fast", "profile default model adopted");
        assert_eq!(
            state.reasoning,
            Reasoning::Medium,
            "profile default reasoning adopted"
        );
        let _ = app.sessions.active; // the active session is absent: untouched
    }

    #[test]
    fn session_selector_sorts_and_filters_case_insensitively() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        let raw = filtered_sessions(&app.sessions.list, "");
        let ids: Vec<&str> = raw.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["ses_a", "ses_b"], "updated_at descending");
        let matching = filtered_sessions(&app.sessions.list, "BETA");
        assert_eq!(matching.len(), 1, "title match is case-insensitive");
        assert_eq!(matching[0].session_id, "ses_b");
        let workspace = filtered_sessions(&app.sessions.list, "/a");
        assert_eq!(workspace.len(), 1);
        assert_eq!(workspace[0].session_id, "ses_a");
    }

    #[test]
    fn session_selector_confirm_opens_and_closes_on_success() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenSessionSelector);
        app.update(AppEvent::MoveSelector { delta: 1 }); // ses_b (fast)
        let commands = take_requests(app.update(AppEvent::ConfirmDock));
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].method, "session.open");
        assert_eq!(commands[0].params["session_id"], json!("ses_b"));
        assert!(matches!(&app.dock, Dock::SessionSelector(state) if state.submitting));
        let commands = take_requests(respond(
            &mut app,
            &commands[0],
            json!({"session": session_info("ses_b", Some("i9"))}),
        ));
        assert!(
            matches!(app.dock, Dock::Composer),
            "success closes the selector"
        );
        assert_eq!(app.sessions.active.as_deref(), Some("ses_b"));
        // state + transcript chain follows the open.
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn session_open_failure_keeps_the_selector_query_and_selection() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenSessionSelector);
        app.update(AppEvent::SetSelectorQuery { query: "Al".into() });
        let request = take_requests(app.update(AppEvent::ConfirmDock)).remove(0);
        let commands = respond_error(&mut app, &request, "bad_session", "gone");
        assert!(take_requests(commands).is_empty());
        match &app.dock {
            Dock::SessionSelector(state) => {
                assert!(!state.submitting, "submit unblocks after failure");
                assert_eq!(state.query, "Al", "query survives");
                assert_eq!(state.cursor, 0, "selection survives");
                assert!(state.error.as_deref().is_some_and(|e| e.contains("gone")));
            }
            other => panic!("selector must stay open, dock = {other:?}"),
        }
    }

    #[test]
    fn create_failure_keeps_every_draft_field_and_reports_the_error() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        app.update(AppEvent::NewSessionSetField {
            field: NewSessionField::Title,
            value: "My task".into(),
        });
        app.update(AppEvent::DockFieldStep { delta: 5 }); // create
        let request = take_requests(app.update(AppEvent::ConfirmDock)).remove(0);
        assert_eq!(request.method, "session.create");
        assert_eq!(request.params["title"], json!("My task"));
        let commands = respond_error(&mut app, &request, "validation", "bad workspace");
        assert!(take_requests(commands).is_empty());
        let state = draft(&app);
        assert!(!state.submitting);
        assert_eq!(state.title, "My task", "fields are never cleared");
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|e| e.contains("bad workspace"))
        );
        assert!(matches!(app.dock, Dock::NewSession(_)));
    }

    #[test]
    fn submit_new_session_is_gated_while_in_flight() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let first = take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        let second = app.update(AppEvent::SubmitNewSession);
        assert!(take_requests(second).is_empty(), "no duplicate create");
        let command = take_requests(respond_error(&mut app, &first, "internal", "boom"));
        assert!(command.is_empty());
        // Now submitting is unblocked.
        let retry = take_requests(app.update(AppEvent::SubmitNewSession));
        assert_eq!(retry.len(), 1);
    }

    #[test]
    fn create_success_activates_the_new_session_and_closes_the_form() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let request = take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        let commands = take_requests(respond(
            &mut app,
            &request,
            json!({"session": session_info("ses_new", Some("n1"))}),
        ));
        assert!(matches!(app.dock, Dock::Composer));
        assert_eq!(app.sessions.active.as_deref(), Some("ses_new"));
        assert_eq!(commands.len(), 2, "state + transcript chain");
    }

    #[test]
    fn cancel_returns_to_the_composer_or_the_form() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        // NewSession -> composer.
        app.update(AppEvent::OpenNewSession);
        app.update(AppEvent::CancelDock);
        assert!(matches!(app.dock, Dock::Composer));
        // Model selector -> the form (never the composer when a draft flows).
        app.update(AppEvent::OpenModelSelector);
        app.update(AppEvent::CancelDock);
        assert!(matches!(app.dock, Dock::NewSession(_)));
        // Reasoning selector -> the form.
        app.update(AppEvent::OpenReasoningSelector);
        app.update(AppEvent::CancelDock);
        assert!(matches!(app.dock, Dock::NewSession(_)));
        // Session selector -> the composer.
        app.update(AppEvent::OpenSessionSelector);
        app.update(AppEvent::CancelDock);
        assert!(matches!(app.dock, Dock::Composer));
        // The composer text survives the whole dance.
        app.update(crate::event::AppEvent::SetTheme(ThemeKind::Dark));
    }

    #[test]
    fn stale_create_response_does_not_touch_a_newer_dock() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let stale = take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        // Cancel the form while the create is in flight, then open a fresh
        // one; the stale failure must not leak into the new draft.
        app.update(AppEvent::CancelDock);
        app.update(AppEvent::OpenNewSession);
        let commands = respond_error(&mut app, &stale, "validation", "stale boom");
        assert!(take_requests(commands).is_empty());
        let state = draft(&app);
        assert!(
            state.error.is_none(),
            "stale failure stayed off the new draft"
        );
        assert!(!state.submitting);
    }

    #[test]
    fn selectors_fields_and_field_edits_freeze_while_creating() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let in_flight = take_requests(app.update(AppEvent::SubmitNewSession));
        assert_eq!(in_flight.len(), 1);
        let workspace_before = draft(&app).workspace.clone();

        // All three new-session selectors are blocked while submitting.
        app.update(AppEvent::OpenModelSelector);
        assert!(
            matches!(app.dock, Dock::NewSession(_)),
            "model selector blocked"
        );
        app.update(AppEvent::OpenReasoningSelector);
        assert!(
            matches!(app.dock, Dock::NewSession(_)),
            "reasoning selector blocked"
        );
        app.update(AppEvent::OpenProfileSelector);
        assert!(
            matches!(app.dock, Dock::NewSession(_)),
            "profile selector blocked"
        );

        // Field confirm (Enter on a selector field) is blocked as well.
        app.update(AppEvent::DockFieldStep { delta: 1 }); // profile
        app.update(AppEvent::ConfirmDock);
        assert!(
            matches!(app.dock, Dock::NewSession(_)),
            "field confirm blocked"
        );

        // Field edits do not mutate the frozen draft.
        app.update(AppEvent::NewSessionSetField {
            field: NewSessionField::Workspace,
            value: "/changed".into(),
        });
        assert_eq!(draft(&app).workspace, workspace_before);
        assert!(draft(&app).submitting);

        // The failure response unblocks and keeps the untouched draft.
        let commands = take_requests(respond_error(&mut app, &in_flight[0], "internal", "boom"));
        assert!(commands.is_empty());
        assert!(matches!(app.dock, Dock::NewSession(_)));
        assert_eq!(draft(&app).workspace, workspace_before);
        assert!(!draft(&app).submitting);
    }

    #[test]
    fn unexpected_selector_does_not_survive_a_matching_create_response() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let request = take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        // Defensive: even if the app ends up on a selector while the create
        // is in flight, a matching success must resolve the whole flow.
        app.draft = Some(draft(&app));
        app.dock = Dock::ModelSelector(SelectorState::new(SelectorKind::Model));
        let commands = take_requests(respond(
            &mut app,
            &request,
            json!({"session": session_info("ses_new", Some("n1"))}),
        ));
        assert!(
            matches!(app.dock, Dock::Composer),
            "matching success closes the flow"
        );
        assert!(app.draft.is_none());
        assert_eq!(app.sessions.active.as_deref(), Some("ses_new"));
        assert!(commands.iter().any(|r| r.method == "session.state"));
    }

    #[test]
    fn stale_create_success_does_not_close_a_newer_draft() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let stale = take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        app.update(AppEvent::CancelDock);
        app.update(AppEvent::OpenNewSession);
        let model_before = draft(&app).model.clone();
        let commands = take_requests(respond(
            &mut app,
            &stale,
            json!({"session": session_info("ses_late", Some("n9"))}),
        ));
        assert!(
            matches!(app.dock, Dock::NewSession(_)),
            "an old create response never closes the newer draft"
        );
        assert_eq!(draft(&app).model, model_before);
        assert!(commands.iter().any(|r| r.method == "session.state"));
    }

    #[test]
    fn move_selector_wraps_and_pages() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenModelSelector);
        app.update(AppEvent::MoveSelector { delta: 5 });
        match &app.dock {
            Dock::ModelSelector(state) => assert_eq!(state.cursor, 5 % 3),
            _ => panic!("model selector"),
        }
        app.update(AppEvent::MoveSelector { delta: -1 });
        match &app.dock {
            Dock::ModelSelector(state) => assert_eq!(state.cursor, 4 % 3),
            _ => panic!("model selector"),
        }
        app.update(AppEvent::PageSelector { delta: 1 });
        match &app.dock {
            Dock::ModelSelector(state) => assert_eq!(state.cursor, (4 % 3 + 6) % 3),
            _ => panic!("model selector"),
        }
    }

    #[test]
    fn rpc_send_failure_on_create_clears_submitting_and_reports() {
        let mut app = test_app();
        ready_with_catalogs(&mut app);
        app.update(AppEvent::OpenNewSession);
        let request = take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        app.update(AppEvent::RpcSendFailed {
            id: request.id,
            error: RpcError::Closed,
        });
        let state = draft(&app);
        assert!(!state.submitting);
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|e| e.contains("RPC process"))
        );
    }
}
