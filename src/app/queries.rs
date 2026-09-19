//! The two read-only in-flight slots (spec §5.3).
//!
//! The pinned Agent allows at most four concurrent read queries; this TUI uses
//! two: one shared by the foreground and by history/result read-back. Execution
//! waits (`turn.wait`) are counted separately and never consume a slot.
//!
//! The module owns exactly one thing: which read keys are in flight, and which
//! ones were asked to refresh again while in flight. It is deliberately not a
//! scheduler: a repeated refresh of the same key records one `refresh_needed`
//! flag, never a queue of duplicate requests.

use std::collections::{HashSet, VecDeque};

use crate::protocol::RequestId;

use super::*;

/// One of this application's finite set of read-only objects. A tool name,
/// path, or "most recent call" is never a key because those repeat.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum QueryKey {
    Changes {
        session_id: String,
    },
    WorkspaceStatus {
        session_id: String,
    },
    Workspace {
        session_id: String,
        file: bool,
    },
    Tool {
        key: crate::state::tool::ToolKey,
    },
    /// A `session.read` page chain for one session (main history and its
    /// read-back share the key so a second chain cannot start for one view).
    History {
        session_id: String,
        generation: u64,
    },
    /// An authoritative `turn.result` read-back, keyed by the exact turn.
    TurnResult {
        session_id: String,
        loop_id: String,
    },
    /// A bounded context snapshot used by preparation/compaction polling.
    Context {
        session_id: String,
        generation: u64,
    },
    /// One explicit full-session search scan chain (spec §17.1). It uses the
    /// same two read-only slots as every other read.
    Search {
        session_id: String,
        generation: u64,
    },
    /// One explicit export read chain (spec §17.4). It shares the same two
    /// read-only slots and holds its pin until the export finishes.
    Export {
        session_id: String,
        export_id: u64,
    },
}

/// The result of asking to start a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryAdmission {
    /// A slot was free and the caller may send the request.
    Admitted,
    /// The same key is already in flight; one refresh is remembered so the
    /// chain can run again when the current one finishes.
    Coalesced,
    /// Both slots are busy with other keys; the caller must wait for a finish.
    Busy,
}

/// The two read-only slots. `in_flight` maps an admitted request to its key so
/// a late response still frees the slot it actually owned.
#[derive(Debug, Default)]
pub struct QuerySlots {
    in_flight: Vec<(RequestId, QueryKey)>,
    refresh_needed: HashSet<QueryKey>,
    waiting: VecDeque<QueryKey>,
    waiting_set: HashSet<QueryKey>,
    ready: HashSet<QueryKey>,
}

impl QuerySlots {
    /// The number of read-only slots (spec §5.3).
    pub const CAPACITY: usize = 2;
    /// A finite backlog prevents a burst of background/session refreshes from
    /// growing without bound while the two remote slots are occupied.
    pub const MAX_WAITING: usize = 16;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }
    pub fn owns_request(&self, id: RequestId) -> bool {
        self.in_flight.iter().any(|(request, _)| *request == id)
    }

    pub fn waiting_len(&self) -> usize {
        self.waiting.len()
    }

    pub fn is_empty(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Whether this exact key is currently in flight.
    pub fn contains(&self, key: &QueryKey) -> bool {
        self.in_flight.iter().any(|(_, existing)| existing == key)
    }

    /// Admits a read for `key`, recording `request_id` as its owner. A repeated
    /// key does not consume a second slot; it sets one refresh flag instead.
    pub fn request_query(&mut self, key: QueryKey, request_id: RequestId) -> QueryAdmission {
        if self.contains(&key) {
            self.refresh_needed.insert(key);
            return QueryAdmission::Coalesced;
        }
        if self.ready.remove(&key) {
            if self.in_flight.len() < Self::CAPACITY {
                self.in_flight.push((request_id, key));
                return QueryAdmission::Admitted;
            }
            self.ready.insert(key.clone());
        }
        if self.waiting_set.contains(&key) {
            return QueryAdmission::Coalesced;
        }
        if !self.waiting.is_empty() {
            if self.waiting.len() >= Self::MAX_WAITING {
                return QueryAdmission::Busy;
            }
            self.waiting_set.insert(key.clone());
            self.waiting.push_back(key);
            return QueryAdmission::Busy;
        }
        if self.in_flight.len() >= Self::CAPACITY {
            if self.waiting.len() >= Self::MAX_WAITING {
                return QueryAdmission::Busy;
            }
            self.waiting_set.insert(key.clone());
            self.waiting.push_back(key);
            return QueryAdmission::Busy;
        }
        self.in_flight.push((request_id, key));
        QueryAdmission::Admitted
    }

    /// Frees the slot owned by `request_id`, returning its key and whether a
    /// refresh was requested while it was in flight. An unknown id is a no-op
    /// (a stale or duplicated response must not corrupt the counters).
    pub fn on_query_finished(
        &mut self,
        request_id: RequestId,
    ) -> Option<(QueryKey, bool, Option<QueryKey>)> {
        let position = self
            .in_flight
            .iter()
            .position(|(id, _)| *id == request_id)?;
        let (_, key) = self.in_flight.remove(position);
        let refresh = self.refresh_needed.remove(&key);
        let ready = self.waiting.pop_front();
        if let Some(ready) = ready.as_ref() {
            self.waiting_set.remove(ready);
            self.ready.insert(ready.clone());
        }
        Some((key, refresh, ready))
    }

    /// Drops every key in the scope and forgets its pending refresh, so a late
    /// response for it is ignored rather than applied to a newer view. The
    /// request keeps its slot until it actually finishes (spec §5.3).
    pub fn invalidate_scope(&mut self, scope: &QueryScope) {
        self.refresh_needed.retain(|key| !scope.matches(key));
        self.ready.retain(|key| !scope.matches(key));
        let mut retained = VecDeque::new();
        while let Some(key) = self.waiting.pop_front() {
            if scope.matches(&key) {
                self.waiting_set.remove(&key);
            } else {
                retained.push_back(key);
            }
        }
        self.waiting = retained;
    }

    /// Whether a refresh was requested for `key` while it was in flight.
    pub fn take_refresh(&mut self, key: &QueryKey) -> bool {
        self.refresh_needed.remove(key)
    }
}

/// The scope an invalidation covers: a whole session (close/reopen) or one
/// read object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryScope {
    Context(String),
    Changes(String),
    Workspace { session_id: String, file: bool },
    Tool(crate::state::tool::ToolKey),
    Session(String),
    All,
}

impl QueryScope {
    fn matches(&self, key: &QueryKey) -> bool {
        match self {
            Self::All => true,
            Self::Context(session) => {
                matches!(key, QueryKey::Context {session_id,..} if session_id==session)
            }
            Self::Changes(session) => {
                matches!(key, QueryKey::Changes { session_id } if session_id == session)
            }
            Self::Workspace { session_id, file } => {
                matches!(key, QueryKey::Workspace { session_id: id, file: f } if id == session_id && f == file)
            }
            Self::Tool(tool) => matches!(key, QueryKey::Tool { key } if key == tool),
            Self::Session(session_id) => match key {
                QueryKey::Changes { session_id: id }
                | QueryKey::WorkspaceStatus { session_id: id } => id == session_id,
                QueryKey::Workspace { session_id: id, .. } => id == session_id,
                QueryKey::Tool { key } => &key.session_id == session_id,
                QueryKey::History { session_id: id, .. } => id == session_id,
                QueryKey::TurnResult { session_id: id, .. } => id == session_id,
                QueryKey::Context { session_id: id, .. } => id == session_id,
                QueryKey::Search { session_id: id, .. } => id == session_id,
                QueryKey::Export { session_id: id, .. } => id == session_id,
            },
        }
    }
}

impl App {
    /// Releases the read-only slot owned by a finished request. A key that was
    /// asked to refresh while in flight runs once more, so a burst of requests
    /// coalesces into at most one follow-up read (spec §5.3).
    pub(super) fn free_query_slot(&mut self, id: RequestId) {
        let Some((key, refresh, ready)) = self.queries.on_query_finished(id) else {
            return;
        };
        if let Some(ready) = ready {
            self.pending_query_followups.push_back(ready);
        }
        if refresh {
            self.pending_query_followups.push_back(key);
        }
    }

    pub(super) fn drain_query_followups(&mut self, commands: &mut Vec<AppCommand>) {
        while let Some(key) = self.pending_query_followups.pop_front() {
            let command = match key {
                QueryKey::Changes { .. } => {
                    commands.extend(self.poll_changes());
                    None
                }
                QueryKey::WorkspaceStatus { .. } => {
                    commands.extend(self.poll_workspace_status());
                    None
                }
                QueryKey::Workspace { .. } => {
                    commands.extend(self.poll_workspace());
                    None
                }
                QueryKey::Tool { key } => {
                    if self.tool_detail().is_some_and(|detail| detail.key == key) {
                        self.poll_tool_detail().into_iter().next()
                    } else {
                        None
                    }
                }
                crate::app::queries::QueryKey::History { session_id, .. } => {
                    if self.history_decode_pending(&session_id) {
                        self.pending_query_followups.push_front(
                            crate::app::queries::QueryKey::History {
                                session_id,
                                generation: 0,
                            },
                        );
                        break;
                    }
                    self.sessions
                        .known
                        .contains_key(&session_id)
                        .then(|| self.request_history(&session_id))
                        .flatten()
                }
                crate::app::queries::QueryKey::TurnResult {
                    session_id,
                    loop_id,
                } => {
                    let turn = TurnRef {
                        session_id,
                        loop_id,
                    };
                    if self.turn_result_decode_pending(&turn) {
                        self.pending_query_followups.push_front(
                            crate::app::queries::QueryKey::TurnResult {
                                session_id: turn.session_id,
                                loop_id: turn.loop_id,
                            },
                        );
                        break;
                    }
                    self.turn_results
                        .get(&turn)
                        .filter(|window| !window.complete)
                        .map(|window| (turn.clone(), window.cursor))
                        .and_then(|(turn, cursor)| self.request_turn_result_page(turn, cursor))
                }
                crate::app::queries::QueryKey::Context { session_id, .. } => {
                    self.request_session_context(&session_id)
                }
                crate::app::queries::QueryKey::Search {
                    session_id,
                    generation,
                } => self
                    .resume_search_scan(&session_id, generation)
                    .into_iter()
                    .next(),
                crate::app::queries::QueryKey::Export {
                    session_id,
                    export_id,
                } => {
                    let commands = self.resume_export_scan(&session_id, export_id);
                    commands.into_iter().next()
                }
            };
            if let Some(command) = command {
                commands.push(command);
            }
        }
    }

    pub(super) fn context_interval(&self, session_id: &SessionId) -> Duration {
        if self.sessions.active.as_ref() == Some(session_id) {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(2)
        }
    }

    pub(super) fn context_query_pending(&self, session_id: &SessionId) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(kind, RequestKind::SessionContext { session_id: pending, .. } if pending == session_id)
        })
    }

    pub(super) fn arm_context_poll(
        &mut self,
        session_id: &SessionId,
        owner: ContextQueryOwner,
        immediate: bool,
    ) -> Option<AppCommand> {
        let owner = if matches!(
            &owner,
            ContextQueryOwner::Explicit | ContextQueryOwner::Panel(_)
        ) {
            self.context_polls
                .get(session_id)
                .filter(|poll| {
                    matches!(
                        &poll.owner,
                        ContextQueryOwner::ManualCompact(_) | ContextQueryOwner::Submission(_)
                    ) || matches!(
                        (&owner, &poll.owner),
                        (ContextQueryOwner::Panel(_), ContextQueryOwner::Explicit)
                    )
                })
                .map_or(owner.clone(), |poll| poll.owner.clone())
        } else {
            owner
        };
        let due = if immediate {
            self.instant_now()
        } else {
            self.instant_now()
                .checked_add(self.context_interval(session_id))
                .expect("context poll deadline is representable")
        };
        self.context_polls
            .insert(session_id.clone(), ContextPoll { owner, due });
        if immediate {
            self.request_session_context(session_id)
        } else {
            None
        }
    }

    pub(super) fn poll_contexts(&mut self) -> Vec<AppCommand> {
        let now = self.instant_now();
        let due: Vec<SessionId> = self
            .context_polls
            .iter()
            .filter_map(|(session_id, poll)| (poll.due <= now).then_some(session_id.clone()))
            .collect();
        let mut commands = Vec::new();
        for session_id in due {
            if self.context_query_pending(&session_id) {
                continue;
            }
            if let Some(command) = self.request_session_context(&session_id) {
                commands.push(command);
            }
            let interval = self.context_interval(&session_id);
            if let Some(poll) = self.context_polls.get_mut(&session_id) {
                poll.due = now
                    .checked_add(interval)
                    .expect("context poll deadline is representable");
            }
        }
        commands
    }

    pub(super) fn reschedule_context_poll(
        &mut self,
        session_id: &SessionId,
        owner: &ContextQueryOwner,
    ) {
        if matches!(
            owner,
            ContextQueryOwner::Explicit | ContextQueryOwner::Panel(_)
        ) {
            self.context_polls.remove(session_id);
            return;
        }
        let due = self
            .instant_now()
            .checked_add(self.context_interval(session_id))
            .expect("context poll deadline is representable");
        self.context_polls.insert(
            session_id.clone(),
            ContextPoll {
                owner: owner.clone(),
                due,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(session: &str) -> QueryKey {
        QueryKey::History {
            session_id: session.to_owned(),
            generation: 0,
        }
    }

    #[test]
    fn a_second_chain_for_one_view_coalesces_instead_of_taking_a_slot() {
        let mut slots = QuerySlots::new();
        assert_eq!(
            slots.request_query(history("ses_1"), RequestId(1)),
            QueryAdmission::Admitted
        );
        assert_eq!(
            slots.request_query(history("ses_1"), RequestId(2)),
            QueryAdmission::Coalesced
        );
        assert_eq!(slots.in_flight_len(), 1, "one key owns one slot");
        // Finishing the first reports the coalesced refresh exactly once.
        assert_eq!(
            slots.on_query_finished(RequestId(1)),
            Some((history("ses_1"), true, None))
        );
        assert_eq!(slots.on_query_finished(RequestId(1)), None);
    }

    #[test]
    fn a_coalesced_read_does_not_emit_a_second_remote_request() {
        let mut slots = QuerySlots::new();
        let key = history("ses_1");
        assert_eq!(
            slots.request_query(key.clone(), RequestId(1)),
            QueryAdmission::Admitted
        );
        assert_eq!(
            slots.request_query(key, RequestId(2)),
            QueryAdmission::Coalesced
        );
        assert_eq!(slots.in_flight_len(), 1);
        assert_eq!(slots.waiting_len(), 0);
        assert_eq!(slots.on_query_finished(RequestId(2)), None);
        assert_eq!(slots.in_flight_len(), 1);
    }

    #[test]
    fn only_two_distinct_reads_run_and_a_third_is_refused() {
        let mut slots = QuerySlots::new();
        assert_eq!(
            slots.request_query(history("ses_1"), RequestId(1)),
            QueryAdmission::Admitted
        );
        assert_eq!(
            slots.request_query(
                QueryKey::TurnResult {
                    session_id: "ses_1".to_owned(),
                    loop_id: "loop_1".to_owned(),
                },
                RequestId(2),
            ),
            QueryAdmission::Admitted
        );
        assert_eq!(
            slots.request_query(history("ses_2"), RequestId(3)),
            QueryAdmission::Busy,
            "a third concurrent subject must wait for a finish, never queue"
        );
        // Freeing one slot admits the refused key on the next normal cycle.
        slots.on_query_finished(RequestId(1));
        assert_eq!(
            slots.request_query(history("ses_2"), RequestId(4)),
            QueryAdmission::Admitted
        );
    }

    #[test]
    fn invalidating_a_scope_drops_its_pending_refresh_but_keeps_the_slot() {
        let mut slots = QuerySlots::new();
        slots.request_query(history("ses_1"), RequestId(1));
        slots.request_query(history("ses_1"), RequestId(2));
        slots.invalidate_scope(&QueryScope::Session("ses_1".to_owned()));
        assert_eq!(slots.in_flight_len(), 1, "the remote read keeps its slot");
        assert_eq!(
            slots.on_query_finished(RequestId(1)),
            Some((history("ses_1"), false, None)),
            "a closed view does not schedule a follow-up read"
        );
    }

    #[test]
    fn waiting_queue_is_bounded_and_fifo_fair() {
        let mut slots = QuerySlots::new();
        slots.request_query(history("ses_0"), RequestId(1));
        slots.request_query(
            QueryKey::TurnResult {
                session_id: "ses_0".to_owned(),
                loop_id: "loop_0".to_owned(),
            },
            RequestId(2),
        );
        for index in 1..=QuerySlots::MAX_WAITING {
            assert_eq!(
                slots.request_query(
                    history(&format!("ses_{index}")),
                    RequestId(10 + index as u64)
                ),
                QueryAdmission::Busy
            );
        }
        assert_eq!(slots.waiting_len(), QuerySlots::MAX_WAITING);
        assert_eq!(
            slots.request_query(history("ses_overflow"), RequestId(99)),
            QueryAdmission::Busy
        );
        assert_eq!(slots.waiting_len(), QuerySlots::MAX_WAITING);

        slots.on_query_finished(RequestId(1));
        assert_eq!(
            slots.request_query(history("ses_1"), RequestId(100)),
            QueryAdmission::Admitted,
            "the oldest waiting key is admitted first"
        );
        assert_eq!(
            slots.request_query(history("ses_0"), RequestId(101)),
            QueryAdmission::Busy,
            "a newer key cannot bypass the waiting FIFO"
        );
    }

    #[test]
    fn an_unknown_finish_never_corrupts_the_counters() {
        let mut slots = QuerySlots::new();
        slots.request_query(history("ses_1"), RequestId(1));
        assert_eq!(slots.on_query_finished(RequestId(99)), None);
        assert_eq!(slots.in_flight_len(), 1);
        assert!(slots.contains(&history("ses_1")));
    }
}
