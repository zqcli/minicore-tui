//! Sessions, their views, and the scroll state (spec 12.4, 12.5, 32.1).

use std::collections::BTreeMap;

use crate::protocol::{SessionInfo, SessionStateWire};
use crate::state::transcript::TranscriptState;
use crate::state::turn::LiveTurn;

/// Session identity on the wire; a plain string like `"ses_1"`.
pub type SessionId = String;

/// All sessions known to the app (spec 12.4).
#[derive(Debug, Default)]
pub struct SessionsState {
    /// Every session this process knows, keyed by `SessionId`; list results,
    /// create/open responses, and open events all land here.
    pub known: BTreeMap<SessionId, SessionView>,
    /// The session currently displayed; background sessions keep their views.
    pub active: Option<SessionId>,
    /// The newest `session.list` snapshot.
    pub list: Vec<SessionInfo>,
}

/// Per-session UI state (spec 12.5). The create/open response's
/// `SessionInfo` is the UI's truth for the session; live turn data is
/// provisional and the durable transcript replaces it after reconciliation.
#[derive(Debug)]
pub struct SessionView {
    pub info: SessionInfo,
    pub state: Option<SessionStateWire>,
    pub transcript: TranscriptState,
    pub live: Option<LiveTurn>,
    pub scroll: ScrollState,
    /// A transcript chain is being fetched page by page.
    pub loading: bool,
    /// `dropped_before > 0` was observed on the event stream; cleared once
    /// the durable transcript reconciles the gap.
    pub event_gap: bool,
    /// A transcript chain driven by a finished turn is being fetched; the
    /// live turn is removed when it completes.
    pub reconcile_inflight: bool,
    /// Ticked on every dropped event; transcript chains carry the revision
    /// they were issued under so a stale completion never clears a newer
    /// gap (spec 13.7).
    pub gap_revision: u64,
}

impl SessionView {
    pub fn new(info: SessionInfo) -> Self {
        Self {
            info,
            state: None,
            transcript: TranscriptState::default(),
            live: None,
            scroll: ScrollState::default(),
            loading: false,
            event_gap: false,
            reconcile_inflight: false,
            gap_revision: 0,
        }
    }
}

/// Scroll bookkeeping for the transcript renderer (spec 32.1); the render
/// phase owns the offset math.
#[derive(Debug, Default)]
pub struct ScrollState {
    pub offset: usize,
    pub follow_tail: bool,
    pub new_content: bool,
}
