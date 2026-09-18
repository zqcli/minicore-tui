//! Sessions, their views, and the scroll state (spec r2).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::protocol::{
    CompactResultWire, Reasoning, SessionContextWire, SessionInfo, SessionPresentationWire,
    SessionStateWire, TurnPersistenceWire, TurnRef, UsageWire,
};
use crate::state::tool::{ToolKey, ToolPresentationState};
use crate::state::transcript::{TranscriptBlock, TranscriptState};
use crate::state::turn::{LiveLoop, UnsavedLoop};
use crate::state::view::{FoldOverride, ReasoningKey};

/// Session identity on the wire; a plain string like `"ses_1"`.
pub type SessionId = String;

/// How the final outcome of a live or retired turn is known.
///
/// This is a provenance type, not a boolean: each variant names the evidence
/// that produced it and the action that can settle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultConfirmation {
    /// A `turn.wait` response or `turn.result` read-back reported the outcome.
    /// `persistence = failed` is still a *known* failure: it is carried by the
    /// result itself and never relabeled as transport uncertainty.
    #[default]
    Confirmed,
    /// A read owns the question but has not answered it yet: the wait was
    /// lost, the wait/read omitted `persistence`, the page was undecodable, or
    /// the turn is still `pending`. Recovery: continue the `turn.result` chain
    /// for the exact `TurnRef` and reconcile history when it settles.
    NeedsRead,
    /// The transport or the loop ended before any read-back could be trusted.
    /// Recovery: a fresh `session.state` + `turn.result`/history read after
    /// reconnecting; never `turn.send`.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageCompleteness {
    #[default]
    Unknown,
    Partial,
    Complete,
}

/// Cached usage projection for the footer. It is rebuilt at history/result
/// boundaries, never by the renderer. Failed unsaved turns remain in their
/// own bucket and are not silently added to persisted totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageProjection {
    pub usage: UsageWire,
    pub completeness: UsageCompleteness,
    pub unsaved_usage: Option<UsageWire>,
    pub unsaved_completeness: UsageCompleteness,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManualCompactState {
    pub operation_id: String,
    pub cancel_requested: bool,
    pub result: Option<CompactResultWire>,
    /// An unknown write outcome remains fenced until both fresh state and
    /// context snapshots have been observed.
    pub state_refresh_confirmed: bool,
    pub context_refresh_confirmed: bool,
}

/// Why one durable-history chain runs (spec §3.4/§6.3). The trigger is kept
/// with the chain instead of in parallel booleans, so a fetch can never be
/// "post-wait" and "refresh" at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryTrigger {
    /// First page or explicit refresh; nothing has to be aligned.
    Refresh,
    /// A dropped event or a preserved gap must be re-read.
    Gap,
    /// A finished turn must appear in the durable window.
    PostWait,
}

/// The read side of one session's durable history.
///
/// At most one chain runs per session. `pending` records a chain that is owed
/// but cannot start yet (a page is in flight, admission is deferred, or an
/// open/reopen preserved a gap), which used to be two separate flags that
/// could disagree with `active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryRead {
    active: Option<HistoryTrigger>,
    pending: Option<HistoryTrigger>,
}

impl HistoryRead {
    pub fn is_loading(&self) -> bool {
        self.active.is_some()
    }

    /// True while a gap or post-wait chain is running or owed. This is the
    /// former `reconcile_inflight`: destructive or secondary readers stay
    /// away until the authoritative window is aligned again.
    pub fn is_reconciling(&self) -> bool {
        matches!(
            self.active,
            Some(HistoryTrigger::Gap | HistoryTrigger::PostWait)
        ) || self.pending == Some(HistoryTrigger::Gap)
    }

    /// A finished turn still owes a chain once the current one ends.
    pub fn post_wait_pending(&self) -> bool {
        self.pending == Some(HistoryTrigger::PostWait)
    }

    /// Starts a chain. A pending trigger of the same kind is satisfied by the
    /// start; an unrelated pending trigger (a finished turn) is preserved so
    /// its own chain still runs later.
    pub fn begin(&mut self, trigger: HistoryTrigger) {
        self.active = Some(trigger);
        if self.pending == Some(trigger) {
            self.pending = None;
        }
    }

    /// Continues the current chain after a page; a chain with no recorded
    /// trigger falls back to a refresh.
    pub fn continue_loading(&mut self) {
        if self.active.is_none() {
            self.active = Some(HistoryTrigger::Refresh);
        }
    }

    /// The chain ended: no page is in flight. A pending trigger survives for
    /// the next chain.
    pub fn finish(&mut self) {
        self.active = None;
    }

    /// Records a chain that must start later.
    pub fn defer(&mut self, trigger: HistoryTrigger) {
        self.pending = Some(trigger);
    }

    /// Consumes the owed trigger, if any.
    pub fn take_pending(&mut self) -> Option<HistoryTrigger> {
        self.pending.take()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

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
    /// Delete requests that have not received a response. Refresh results do
    /// not re-add these IDs while their outcome is pending.
    pub pending_deletes: HashSet<SessionId>,
    /// IDs removed by a successful delete. A later stale session.list cannot
    /// resurrect them in this TUI process.
    pub deleted: HashSet<SessionId>,
    /// A successful close acknowledged by this TUI process. A stale session
    /// list cannot mark the session loaded again until a later open ACK.
    pub closed: HashSet<SessionId>,
    /// Metadata acknowledged by a local mutation. A stale session.list
    /// response cannot roll an acknowledged rename back to its old title.
    pub title_overrides: HashMap<SessionId, Option<String>>,
}

/// Per-session UI state.
#[derive(Debug)]
pub struct SessionView {
    pub info: SessionInfo,
    /// Lifecycle epoch for submissions; an old deferred response cannot
    /// restore text into a newer open/reopen draft.
    pub session_epoch: u64,
    /// The latest real `session.context` snapshot. It is separate from the
    /// ordinary state DTO because context is also authoritative during
    /// deferred preparation.
    pub context: Option<SessionContextWire>,
    pub context_query_generation: u64,
    pub manual_compact: Option<ManualCompactState>,
    /// Read-only Agent presentation snapshot for the footer/detail surface.
    pub presentation: Option<SessionPresentationWire>,
    /// Coalesces the one in-flight `session.presentation` request.
    pub presentation_pending: bool,
    /// A request boundary arrived while the snapshot request was in flight;
    /// the response handler issues one follow-up request for the newer view.
    pub presentation_refresh_pending: bool,
    /// User acceptance times keyed by durable history item index. Missing
    /// entries remain unknown rather than being replaced by local `now()`.
    pub user_timestamps: HashMap<usize, String>,
    /// Acceptance time for the currently pending live prompt, if returned.
    pub live_user_timestamp: Option<String>,
    /// Distinguishes an accepted prompt with an unavailable clock from the
    /// short interval before `turn.send` is acknowledged.
    pub live_user_time_accepted: bool,
    pub state: Option<SessionStateWire>,
    /// Monotonic query token for the newest session.state request. Older
    /// responses may arrive after a newer snapshot and must not regress it.
    pub latest_state_query: Option<u64>,
    pub last_request: Option<RequestConfigEvidence>,
    /// The most recent model/reasoning update and the boundary evidence that
    /// has been observed for it.
    pub config_update: Option<PendingConfigUpdate>,
    pub last_result: Option<crate::protocol::TurnResultViewWire>,
    pub usage_projection: UsageProjection,
    /// Real per-request usage reported by the Agent while its loop is still
    /// running (spec 9.4/12.4), keyed `(loop_id, request_index)`. Persisted
    /// history rows and the loop total supersede these live rows; they are
    /// only shown when no persisted source owns the request.
    pub live_request_usage: HashMap<(String, u32), crate::protocol::UsageWire>,
    /// One bounded fence for events from the loop retired by close/reopen.
    /// This is not a result registry.
    pub retired_loop: Option<TurnRef>,
    /// Whether the last turn outcome is known, and from which source
    /// (spec §3.4 `Confirmation`). Replaces the old `result_unconfirmed`
    /// boolean so a failed save is no longer conflated with an unknown
    /// outcome.
    pub result_confirmation: ResultConfirmation,
    pub transcript: TranscriptState,
    pub live: Option<LiveLoop>,
    pub unsaved_loop: Option<UnsavedLoop>,
    pub scroll: ScrollState,
    /// A local generation identifies one pinned history chain. It changes at
    /// lifecycle boundaries so an old in-flight read can retain its slot while
    /// a fresh recovery chain uses a distinct query key.
    pub history_query_generation: u64,
    /// The in-flight `session.read` page, if any. Owns the chunk assembler so
    /// a page outside the window never contaminates the window.
    pub read_page: Option<crate::app::history::ReadPage>,
    /// Durable history is unconfirmed after a dropped event or history
    /// failure; destructive lifecycle actions must wait for aligned history.
    pub event_gap: bool,
    /// The durable-history read state: what is loading and what chain is owed
    /// (spec §3.4). Replaces the former `loading`/`reconcile_inflight`/
    /// `needs_post_wait_history` triplet.
    pub history_read: HistoryRead,
    /// Whether an explicit session.close is currently pending.
    pub closing: bool,
    /// The last close attempt ended without an authoritative unload proof or
    /// current state snapshot. Lifecycle actions must reread state before
    /// relying on the retained SessionInfo/state projection.
    pub close_verification_unknown: bool,
    /// A retained live loop cannot be steered until a matching fresh state
    /// response confirms that the same loop is still running. This is
    /// independent of the durable History/event gap: an authoritative live
    /// TurnRef may steer through an ordinary History gap once this fence is
    /// clear.
    pub steer_state_unconfirmed: bool,
    /// Retained steer completion notices across loop boundaries.
    pub completed_steers: Vec<CompletedSteerNotice>,
    /// Locally admitted, not-yet-sent steering instructions. Lives outside
    /// `LiveLoop` so a finished loop never drops it; bounded and FIFO.
    pub steer_queue: Vec<crate::state::turn::SteerQueueItem>,
    /// Receipt-proven (applied) steers awaiting durable replacement.
    pub applied_steers: Vec<crate::state::turn::AppliedSteer>,
    /// Queue paused (cancel/refusal/error/blocked/unsaved/transport-unknown):
    /// unsent entries must not auto-send, ambiguous accepted ones not retried.
    pub steer_queue_paused: bool,
    /// Monotonic steering receipt observed for the CURRENT loop (reset on
    /// loop start): the highest applied count and the request at which it was
    /// first observed. ACK `steer_index` values pair against it; dropped
    /// `steer_progress` events reconcile from `session.presentation`.
    pub steer_receipt: Option<crate::state::turn::SteerReceiptObserved>,
    /// Ticked on every dropped event.
    pub gap_revision: u64,
    /// Toggle-all tools preview state.
    pub tools_expanded: bool,
    /// Live/history bounded tool display data keyed by the full tool identity.
    pub tool_presentations: HashMap<ToolKey, ToolPresentationState>,
    /// Stable per-section fold choices. These are local UI state only.
    pub tool_folds: HashMap<ToolKey, FoldOverride>,
    pub reasoning_folds: HashMap<ReasoningKey, FoldOverride>,
}

impl SessionView {
    pub fn new(info: SessionInfo) -> Self {
        Self {
            info,
            session_epoch: 0,
            context: None,
            context_query_generation: 0,
            manual_compact: None,
            presentation: None,
            presentation_pending: false,
            presentation_refresh_pending: false,
            user_timestamps: HashMap::new(),
            live_user_timestamp: None,
            live_user_time_accepted: false,
            state: None,
            latest_state_query: None,
            last_request: None,
            config_update: None,
            last_result: None,
            usage_projection: UsageProjection::default(),
            live_request_usage: HashMap::new(),
            history_query_generation: 0,
            retired_loop: None,
            result_confirmation: ResultConfirmation::default(),
            transcript: TranscriptState::default(),
            live: None,
            unsaved_loop: None,
            scroll: ScrollState::default(),
            read_page: None,
            event_gap: false,
            history_read: HistoryRead::default(),
            closing: false,
            close_verification_unknown: false,
            steer_state_unconfirmed: false,
            completed_steers: Vec::new(),
            steer_queue: Vec::new(),
            applied_steers: Vec::new(),
            steer_queue_paused: false,
            steer_receipt: None,
            gap_revision: 0,
            tools_expanded: false,
            tool_presentations: HashMap::new(),
            tool_folds: HashMap::new(),
            reasoning_folds: HashMap::new(),
        }
    }

    /// Whether the last known result still needs the user's attention before a
    /// destructive lifecycle action: an unread outcome (`NeedsRead`/`Unknown`)
    /// or a known failed save. This is not "history is fully loaded".
    pub fn needs_result_confirmation(&self) -> bool {
        self.result_confirmation != ResultConfirmation::Confirmed
            || self
                .last_result
                .as_ref()
                .is_some_and(|result| result.persistence == Some(TurnPersistenceWire::Failed))
    }

    /// Whether the session is currently blocked.
    pub fn is_blocked(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.status == crate::protocol::SessionStatusWire::Blocked)
    }

    /// Automatic or manual context preparation owns the Session even when the
    /// backend reports its ordinary status as idle. It is a visible busy state,
    /// not permission to admit another turn.
    pub fn is_preparing(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.compaction.is_some())
            || self.context.as_ref().is_some_and(|context| {
                context.current_operation.is_some() || context.automatic.current.is_some()
            })
            || self.manual_compact.as_ref().is_some_and(|compact| {
                compact.result.as_ref().is_none_or(|result| {
                    result.status == crate::protocol::CompactStatusWire::UnknownWrite
                })
            })
            || self.live.as_ref().is_some_and(|live| {
                live.reference.is_none()
                    && live.local_submission != crate::state::turn::LocalSubmissionId(u64::MAX)
            })
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
        if self.closing || self.is_preparing() {
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

    /// Rebuilds the read-only usage projection from the current state
    /// boundary. Assistant request rows are keyed by `(loop_id,
    /// request_index)`; a persisted loop total replaces those rows, while a
    /// failed/unsaved result is kept separate.
    /// Records one real per-request usage row from the Agent's live stream
    /// and refreshes the footer projection. The row is dropped from display
    /// as soon as a persisted source owns the same request.
    pub fn set_live_request_usage(
        &mut self,
        loop_id: &str,
        request_index: u32,
        usage: crate::protocol::UsageWire,
    ) {
        self.live_request_usage
            .insert((loop_id.to_owned(), request_index), usage);
        self.recompute_usage_projection();
    }

    /// A new turn began: the previous loop's live rows are superseded by its
    /// persisted result or belong to history, so drop them now.
    pub fn discard_live_request_usage(&mut self) {
        self.live_request_usage.clear();
        self.recompute_usage_projection();
    }

    pub fn recompute_usage_projection(&mut self) {
        let persisted = self
            .last_result
            .as_ref()
            .filter(|result| result.persistence == Some(TurnPersistenceWire::Persisted));
        let persisted_loop = persisted.map(|result| result.turn.loop_id.as_str());
        let mut accumulator = UsageAccumulator::default();
        let mut request_keys = HashSet::new();
        let mut source_count = 0;
        for block in &self.transcript.blocks {
            let TranscriptBlock::Assistant(assistant) = block else {
                continue;
            };
            if persisted_loop == Some(assistant.loop_id.as_str())
                || !request_keys.insert((assistant.loop_id.as_str(), assistant.request_index))
            {
                continue;
            }
            source_count += 1;
            accumulator.add(assistant.usage);
        }
        if let Some(result) = persisted {
            if let Some(usage) = result.usage {
                source_count += 1;
                accumulator.add(usage);
            }
        }

        // Live per-request rows from an in-progress loop (spec 9.4/12.4): each
        // real reported usage shows as soon as the Agent emits it. A row is
        // skipped when a persisted source already owns that request — the
        // persisted loop total replaces the live rows rather than summing
        // with them, and history pages of older loops stay untouched.
        if self.live.is_some() {
            for ((loop_id, request_index), usage) in &self.live_request_usage {
                if persisted_loop == Some(loop_id.as_str()) {
                    continue;
                }
                if !request_keys.insert((loop_id.as_str(), *request_index)) {
                    continue;
                }
                source_count += 1;
                accumulator.add(*usage);
            }
        }

        let completeness = if source_count == 0 || !accumulator.has_known_value() {
            UsageCompleteness::Unknown
        } else if accumulator.has_unknown()
            || !self.transcript.complete
            || self.history_read.is_loading()
            || self.event_gap
            || self.live.is_some()
            || self.unsaved_loop.is_some()
        {
            UsageCompleteness::Partial
        } else {
            UsageCompleteness::Complete
        };

        let unsaved_result = self
            .unsaved_loop
            .as_ref()
            .and_then(|unsaved| unsaved.result.as_ref())
            .or_else(|| {
                self.last_result
                    .as_ref()
                    .filter(|result| result.persistence == Some(TurnPersistenceWire::Failed))
            });
        let mut unsaved_accumulator = UsageAccumulator::default();
        if let Some(result) = unsaved_result {
            if let Some(usage) = result.usage {
                unsaved_accumulator.add(usage);
            }
        }
        let unsaved_completeness = match (self.unsaved_loop.as_ref(), unsaved_result) {
            (None, None) => UsageCompleteness::Unknown,
            (Some(_), Some(_)) if unsaved_accumulator.has_known_value() => {
                if unsaved_accumulator.has_unknown() {
                    UsageCompleteness::Partial
                } else {
                    UsageCompleteness::Complete
                }
            }
            // An active or failed unsaved turn is itself evidence that a
            // usage bucket exists, even when no numeric field is available.
            // Keep that fact visible as unknown/partial instead of silently
            // dropping it from the Footer.
            (Some(_), _) => UsageCompleteness::Partial,
            (None, Some(_)) if unsaved_accumulator.has_known_value() => {
                if unsaved_accumulator.has_unknown() {
                    UsageCompleteness::Partial
                } else {
                    UsageCompleteness::Complete
                }
            }
            (None, Some(_)) => UsageCompleteness::Partial,
        };

        self.usage_projection = UsageProjection {
            usage: accumulator.finish(),
            completeness,
            unsaved_usage: unsaved_accumulator
                .has_known_value()
                .then(|| unsaved_accumulator.finish()),
            unsaved_completeness,
        };
    }
}

#[derive(Default)]
struct UsageAccumulator {
    usage: UsageWire,
    seen: bool,
    unknown_input: bool,
    unknown_output: bool,
    unknown_reasoning: bool,
    unknown_cache_read: bool,
    unknown_cache_write: bool,
    unknown_provider_total: bool,
}

impl UsageAccumulator {
    fn add(&mut self, next: UsageWire) {
        self.seen = true;
        add_usage_field(
            &mut self.usage.input_tokens,
            &mut self.unknown_input,
            next.input_tokens,
        );
        add_usage_field(
            &mut self.usage.output_tokens,
            &mut self.unknown_output,
            next.output_tokens,
        );
        add_usage_field(
            &mut self.usage.reasoning_tokens,
            &mut self.unknown_reasoning,
            next.reasoning_tokens,
        );
        add_usage_field(
            &mut self.usage.cache_read_tokens,
            &mut self.unknown_cache_read,
            next.cache_read_tokens,
        );
        add_usage_field(
            &mut self.usage.cache_write_tokens,
            &mut self.unknown_cache_write,
            next.cache_write_tokens,
        );
        add_usage_field(
            &mut self.usage.provider_total_tokens,
            &mut self.unknown_provider_total,
            next.provider_total_tokens,
        );
    }

    fn has_known_value(&self) -> bool {
        self.seen
            && (self.usage.input_tokens.is_some()
                || self.usage.output_tokens.is_some()
                || self.usage.reasoning_tokens.is_some()
                || self.usage.cache_read_tokens.is_some()
                || self.usage.cache_write_tokens.is_some()
                || self.usage.provider_total_tokens.is_some())
    }

    fn has_unknown(&self) -> bool {
        // These are the fields projected by the one-row footer. Reasoning and
        // provider totals remain optional wire data but are not displayed by
        // that surface.
        self.unknown_input
            || self.unknown_output
            || self.unknown_cache_read
            || self.unknown_cache_write
    }

    fn finish(&self) -> UsageWire {
        self.usage
    }
}

fn add_usage_field(total: &mut Option<u64>, unknown: &mut bool, next: Option<u64>) {
    match next {
        Some(value) if !*unknown => {
            *total = Some(total.unwrap_or(0).saturating_add(value));
        }
        Some(_) => {}
        None => {
            *unknown = true;
            *total = None;
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
    pub accepted_at: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LoopOutcomeWire, TurnResultViewWire};

    fn view() -> SessionView {
        SessionView::new(SessionInfo {
            session_id: "ses_1".to_owned(),
            title: None,
            profile: "coding".to_owned(),
            workspace: "/project".to_owned(),
            model: "model".to_owned(),
            reasoning: Reasoning::High,
            loaded: true,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        })
    }

    fn assistant(
        loop_id: &str,
        request_index: u32,
        input: Option<u64>,
        output: Option<u64>,
    ) -> TranscriptBlock {
        TranscriptBlock::Assistant(crate::state::transcript::AssistantBlock {
            index: 0,
            loop_id: loop_id.to_owned(),
            request_index,
            model: "model".to_owned(),
            reasoning_level: Reasoning::High,
            parts: Vec::new(),
            tool_calls: Vec::new(),
            usage: UsageWire {
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                ..UsageWire::default()
            },
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        })
    }

    #[test]
    fn usage_deduplicates_request_rows_and_persisted_loop_totals_replace_them() {
        let mut session = view();
        session
            .transcript
            .blocks
            .push(assistant("loop_1", 0, Some(10), Some(20)));
        session
            .transcript
            .blocks
            .push(assistant("loop_1", 0, Some(100), Some(200)));
        session
            .transcript
            .blocks
            .push(assistant("loop_2", 0, Some(3), Some(4)));
        session.transcript.complete = true;
        session.last_result = Some(TurnResultViewWire {
            turn: TurnRef {
                session_id: "ses_1".to_owned(),
                loop_id: "loop_1".to_owned(),
            },
            outcome: LoopOutcomeWire::Completed,
            usage: Some(UsageWire {
                input_tokens: Some(7),
                output_tokens: Some(8),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                ..UsageWire::default()
            }),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            persistence: Some(TurnPersistenceWire::Persisted),
            accepted_at: None,
            completed_at: None,
        });

        session.recompute_usage_projection();

        assert_eq!(session.usage_projection.usage.input_tokens, Some(10));
        assert_eq!(session.usage_projection.usage.output_tokens, Some(12));
        assert_eq!(
            session.usage_projection.completeness,
            UsageCompleteness::Complete
        );
    }

    #[test]
    fn usage_marks_partial_fields_and_keeps_failed_unsaved_usage_separate() {
        let mut session = view();
        session
            .transcript
            .blocks
            .push(assistant("loop_1", 0, Some(10), None));
        session.transcript.complete = true;
        session.recompute_usage_projection();
        assert_eq!(session.usage_projection.usage.input_tokens, Some(10));
        assert_eq!(session.usage_projection.usage.output_tokens, None);
        assert_eq!(
            session.usage_projection.completeness,
            UsageCompleteness::Partial
        );

        let result = TurnResultViewWire {
            turn: TurnRef {
                session_id: "ses_1".to_owned(),
                loop_id: "loop_failed".to_owned(),
            },
            outcome: LoopOutcomeWire::Failed {
                kind: "model_error".to_owned(),
                model_error: None,
            },
            usage: Some(UsageWire {
                input_tokens: Some(5),
                output_tokens: Some(6),
                ..UsageWire::default()
            }),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            persistence: Some(TurnPersistenceWire::Failed),
            accepted_at: None,
            completed_at: None,
        };
        session.last_result = Some(result.clone());
        session.unsaved_loop = Some(UnsavedLoop {
            turn: result.turn.clone(),
            user_text: "prompt".to_owned(),
            requests: Vec::new(),
            result: Some(result),
            event_gap: false,
        });
        session.recompute_usage_projection();
        assert_eq!(
            session.usage_projection.unsaved_usage.unwrap().input_tokens,
            Some(5)
        );
        assert_eq!(session.usage_projection.usage.input_tokens, Some(10));
    }

    #[test]
    fn live_per_request_usage_shows_during_the_loop_and_persisted_total_replaces_it() {
        let mut session = view();
        session.live = Some(LiveLoop::new(
            crate::state::turn::LocalSubmissionId(1),
            "prompt".to_owned(),
        ));
        session.live.as_mut().unwrap().reference = Some(TurnRef {
            session_id: "ses_1".to_owned(),
            loop_id: "loop_1".to_owned(),
        });

        // Request 0 reports usage while the loop is still running: the footer
        // must show the known value, not a fake zero.
        session.set_live_request_usage(
            "loop_1",
            0,
            UsageWire {
                input_tokens: Some(10),
                output_tokens: Some(20),
                reasoning_tokens: Some(5),
                ..UsageWire::default()
            },
        );
        assert_eq!(session.usage_projection.usage.input_tokens, Some(10));
        assert_eq!(session.usage_projection.usage.output_tokens, Some(20));
        assert_eq!(session.usage_projection.usage.reasoning_tokens, Some(5));
        assert_eq!(
            session.usage_projection.completeness,
            UsageCompleteness::Partial
        );

        // Request 1 (e.g. after a mid-loop model swap) merges per request.
        session.set_live_request_usage(
            "loop_1",
            1,
            UsageWire {
                input_tokens: Some(3),
                output_tokens: Some(4),
                ..UsageWire::default()
            },
        );
        assert_eq!(session.usage_projection.usage.input_tokens, Some(13));
        assert_eq!(session.usage_projection.usage.output_tokens, Some(24));

        // The loop persists: its total replaces the live rows instead of
        // summing with them, and older-loop history stays untouched.
        session.live = None;
        session.transcript.complete = true;
        session
            .transcript
            .blocks
            .push(assistant("loop_0", 0, Some(100), Some(200)));
        session.last_result = Some(TurnResultViewWire {
            turn: TurnRef {
                session_id: "ses_1".to_owned(),
                loop_id: "loop_1".to_owned(),
            },
            outcome: LoopOutcomeWire::Completed,
            usage: Some(UsageWire {
                input_tokens: Some(7),
                output_tokens: Some(8),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                ..UsageWire::default()
            }),
            requests: Some(2),
            tool_rounds: Some(1),
            final_config_revision: Some(0),
            persistence: Some(TurnPersistenceWire::Persisted),
            accepted_at: None,
            completed_at: None,
        });
        session.recompute_usage_projection();
        // 100/200 (older loop) + 7/8 (loop_1 total) — the live rows 10+3/20+4
        // must NOT be added on top.
        assert_eq!(session.usage_projection.usage.input_tokens, Some(107));
        assert_eq!(session.usage_projection.usage.output_tokens, Some(208));
        assert_eq!(
            session.usage_projection.completeness,
            UsageCompleteness::Complete
        );

        // A new turn clears live rows so a stale event cannot resurrect them.
        session.discard_live_request_usage();
        assert!(session.live_request_usage.is_empty());
    }
}
