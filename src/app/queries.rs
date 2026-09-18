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

use std::collections::HashSet;

use crate::protocol::RequestId;

/// One of this application's finite set of read-only objects. A tool name,
/// path, or "most recent call" is never a key because those repeat.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum QueryKey {
    /// A `session.read` page chain for one session (main history and its
    /// read-back share the key so a second chain cannot start for one view).
    History(String),
    /// An authoritative `turn.result` read-back, keyed by the exact turn.
    TurnResult { session_id: String, loop_id: String },
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
}

impl QuerySlots {
    /// The number of read-only slots (spec §5.3).
    pub const CAPACITY: usize = 2;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
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
        if self.in_flight.len() >= Self::CAPACITY {
            return QueryAdmission::Busy;
        }
        self.in_flight.push((request_id, key));
        QueryAdmission::Admitted
    }

    /// Frees the slot owned by `request_id`, returning its key and whether a
    /// refresh was requested while it was in flight. An unknown id is a no-op
    /// (a stale or duplicated response must not corrupt the counters).
    pub fn on_query_finished(&mut self, request_id: RequestId) -> Option<(QueryKey, bool)> {
        let position = self
            .in_flight
            .iter()
            .position(|(id, _)| *id == request_id)?;
        let (_, key) = self.in_flight.remove(position);
        let refresh = self.refresh_needed.remove(&key);
        Some((key, refresh))
    }

    /// Drops every key in the scope and forgets its pending refresh, so a late
    /// response for it is ignored rather than applied to a newer view. The
    /// request keeps its slot until it actually finishes (spec §5.3).
    pub fn invalidate_scope(&mut self, scope: &QueryScope) {
        self.refresh_needed.retain(|key| !scope.matches(key));
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
    Session(String),
    All,
}

impl QueryScope {
    fn matches(&self, key: &QueryKey) -> bool {
        match self {
            Self::All => true,
            Self::Session(session_id) => match key {
                QueryKey::History(id) => id == session_id,
                QueryKey::TurnResult { session_id: id, .. } => id == session_id,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(session: &str) -> QueryKey {
        QueryKey::History(session.to_owned())
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
            Some((history("ses_1"), true))
        );
        assert_eq!(slots.on_query_finished(RequestId(1)), None);
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
            Some((history("ses_1"), false)),
            "a closed view does not schedule a follow-up read"
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
