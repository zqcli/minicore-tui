//! Sessions, their views, and the scroll state (spec r2).

use std::collections::BTreeMap;

use crate::protocol::{Reasoning, SessionInfo, SessionStateWire, TurnRef};
use crate::state::transcript::TranscriptState;
use crate::state::turn::{LiveLoop, UnsavedLoop};

/// Session identity on the wire; a plain string like `"ses_1"`.
pub type SessionId = String;

/// All sessions known to the app.
#[derive(Debug, Default)]
pub struct SessionsState {
    /// Every session this process knows, keyed by `SessionId`.
    pub known: BTreeMap<SessionId, SessionView>,
    /// The session currently displayed; background sessions keep their views.
    pub active: Option<SessionId>,
    /// The newest `session.list` snapshot, augmented with sessions opened or
    /// created after bootstrap.
    pub list: Vec<SessionInfo>,
}

/// Per-session UI state.
#[derive(Debug)]
pub struct SessionView {
    pub info: SessionInfo,
    pub state: Option<SessionStateWire>,
    /// Monotonic query token for the newest session.state request. Older
    /// responses may arrive after a newer snapshot and must not regress it.
    pub latest_state_query: Option<u64>,
    pub last_request: Option<RequestConfigEvidence>,
    /// The most recent model/reasoning update and the boundary evidence that
    /// has been observed for it.
    pub config_update: Option<PendingConfigUpdate>,
    pub last_result: Option<crate::protocol::TurnResultViewWire>,
    /// One bounded fence for events from the loop retired by close/reopen.
    /// This is not a result registry.
    pub retired_loop: Option<TurnRef>,
    /// Set when transport loss prevents confirming the live turn result.
    pub result_unconfirmed: bool,
    pub transcript: TranscriptState,
    pub live: Option<LiveLoop>,
    pub unsaved_loop: Option<UnsavedLoop>,
    pub scroll: ScrollState,
    /// A history chain is being fetched page by page.
    pub loading: bool,
    /// `dropped_before > 0` was observed on the event stream.
    pub event_gap: bool,
    /// A history chain driven by a finished turn is being fetched; the
    /// live turn is removed when it completes.
    pub reconcile_inflight: bool,
    /// A turn.wait completed while a previous history page was inflight;
    /// requires a fresh history fetch after that page finishes.
    pub needs_post_wait_history: bool,
    /// Whether an explicit session.close is currently pending.
    pub closing: bool,
    /// Retained steer completion notices across loop boundaries.
    pub completed_steers: Vec<CompletedSteerNotice>,
    /// Ticked on every dropped event.
    pub gap_revision: u64,
    /// Toggle-all tools preview state.
    pub tools_expanded: bool,
}

impl SessionView {
    pub fn new(info: SessionInfo) -> Self {
        Self {
            info,
            state: None,
            latest_state_query: None,
            last_request: None,
            config_update: None,
            last_result: None,
            retired_loop: None,
            result_unconfirmed: false,
            transcript: TranscriptState::default(),
            live: None,
            unsaved_loop: None,
            scroll: ScrollState::default(),
            loading: false,
            event_gap: false,
            reconcile_inflight: false,
            needs_post_wait_history: false,
            closing: false,
            completed_steers: Vec::new(),
            gap_revision: 0,
            tools_expanded: false,
        }
    }

    /// Whether the session is currently blocked.
    pub fn is_blocked(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.status == crate::protocol::SessionStatusWire::Blocked)
    }

    /// Whether the retained completion belongs to the currently live loop.
    /// A pending new prompt keeps the previous result as an event fence, but
    /// it must not take precedence over the new live display.
    pub fn can_show_last_result(&self) -> bool {
        match (&self.live, &self.last_result) {
            (_, None) | (None, Some(_)) => true,
            (Some(live), Some(result)) => live
                .reference
                .as_ref()
                .is_some_and(|reference| result.turn == *reference),
        }
    }

    pub fn is_running(&self) -> bool {
        if self.closing {
            return false;
        }
        match self.state.as_ref().map(|state| state.status) {
            Some(crate::protocol::SessionStatusWire::Running) => {
                self.live.as_ref().is_none_or(|live| !live.waiting)
            }
            Some(
                crate::protocol::SessionStatusWire::WaitingForInput
                | crate::protocol::SessionStatusWire::Finishing
                | crate::protocol::SessionStatusWire::Blocked,
            ) => false,
            Some(crate::protocol::SessionStatusWire::Idle) | None => {
                self.live.as_ref().is_some_and(|live| !live.waiting)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedSteerNotice {
    pub session_id: String,
    pub loop_id: String,
    pub local_id: u64,
    pub text: String,
    pub state: crate::state::turn::PendingSteerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConfigUpdate {
    pub loop_id: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
    pub revision: Option<u64>,
    pub state: ConfigUpdateState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigUpdateState {
    WaitingBoundary,
    Applied,
    SavedNextTurn,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestConfigEvidence {
    pub loop_id: Option<String>,
    pub request_index: u32,
    pub revision: u64,
    pub model: String,
    pub reasoning: Reasoning,
}

/// Scroll bookkeeping for the transcript renderer; the render
/// phase owns the offset math. New sessions follow the tail by default.
#[derive(Debug)]
pub struct ScrollState {
    pub offset: usize,
    pub follow_tail: bool,
    pub new_content: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset: 0,
            follow_tail: true,
            new_content: false,
        }
    }
}
