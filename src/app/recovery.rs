//! Completion discovery and body recovery.
//!
//! The snapshot's completion directory says which turns reached the Agent's
//! completion boundary. Every entry the client has not settled yet is read back
//! through `turn.result`, and only a fully verified body replaces transcript
//! content.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::protocol::{CompletedTurn, CompletionDirectory, SessionSnapshot, TurnRef};

use super::read::{HistoryRead, TurnRead};
use super::{App, NoticeLevel, Session, SessionOp};
use crate::protocol::RpcError;

// Leave one of the two read workspaces available for foreground history and export.
pub const CONCURRENT: usize = 1;
/// Unsettled identities kept per load.
pub const IDENTITIES_PER_LOAD: usize = 256;
/// Recovery identities shared by resident loads.
pub const IDENTITIES_TOTAL: usize = 1024;
/// Automatic attempts for one completion before it stays failed.
pub const MAX_RETRIES: u32 = 3;

#[derive(Default)]
pub struct RecoveryTasks(Vec<(Option<u64>, tokio::task::AbortHandle)>);

impl RecoveryTasks {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn history(&self) -> bool {
        self.0.iter().any(|(seq, _)| seq.is_none())
    }
    pub fn finish(&mut self, seq: Option<u64>) {
        self.0.retain(|(known, _)| *known != seq);
    }
}

impl Drop for RecoveryTasks {
    fn drop(&mut self) {
        for (_, task) in &self.0 {
            task.abort();
        }
    }
}

/// Where a completion's body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStatus {
    /// Known from the directory; not started.
    Pending,
    /// A read is in flight.
    Fetching,
    /// Read and installed as canonical content.
    Complete,
    /// Read, but only part of the body could be restored.
    Partial,
    /// The body cannot be read; the reason is shown once.
    Unavailable,
    /// The read failed in a way worth retrying.
    Retryable {
        due: tokio::time::Instant,
    },
    Exhausted,
}

impl RecoveryStatus {
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Complete | Self::Partial | Self::Unavailable)
    }
}

/// One discovered completion of the current load.
#[derive(Clone, Debug)]
pub struct Recovery {
    pub turn: CompletedTurn,
    pub status: RecoveryStatus,
    attempts: u32,
}

/// The bounded recovery state of one resident instance.
#[derive(Default)]
pub struct RecoveryState {
    /// The last applied snapshot revision of the installed load.
    pub revision: u64,
    /// The highest completion sequence the directory has reported.
    pub observed_through: u64,
    /// Every sequence at or below this is recovered or explicitly unavailable.
    pub settled_through: u64,
    /// Unsettled completions by sequence.
    pub entries: BTreeMap<u64, Recovery>,
    /// The directory's window moved past completions this client never read.
    pub truncated_gap: bool,
    /// Retained directory entries are waiting for recovery identity capacity.
    pub behind: bool,
    pub gap_through: u64,
    pub history_status: Option<RecoveryStatus>,
    pub history_coverage: Option<super::read::HistoryCoverage>,
    history_attempts: u32,
}

impl RecoveryState {
    pub fn accept_revision(&mut self, revision: u64) -> bool {
        if revision <= self.revision {
            return false;
        }
        self.revision = revision;
        true
    }

    /// Takes a snapshot's completion directory, keeping only unsettled work.
    pub fn discover(&mut self, directory: &CompletionDirectory, limit: usize) {
        self.observed_through = self.observed_through.max(directory.through_seq);
        if directory.truncated && directory.retained_from_seq > self.settled_through + 1 {
            self.truncated_gap = true;
            let through = directory.retained_from_seq - 1;
            if through > self.gap_through {
                self.gap_through = through;
                self.history_status = Some(RecoveryStatus::Pending);
                self.history_attempts = 0;
            }
        }
        // A read may still hold the only copy of an unsaved turn after its
        // descriptor leaves the catalog, so keep its identity until it finishes.
        let oldest_retained = directory.retained_from_seq;
        let dropped: Vec<u64> = self
            .entries
            .iter()
            .filter_map(|(seq, entry)| {
                (*seq < oldest_retained && entry.status != RecoveryStatus::Fetching).then_some(*seq)
            })
            .collect();
        let mut lost = false;
        for seq in dropped {
            if let Some(entry) = self.entries.remove(&seq)
                && !entry.status.is_settled()
            {
                lost = true;
            }
        }
        if lost {
            self.truncated_gap = true;
        }
        self.behind = false;
        for entry in &directory.entries {
            if entry.seq <= self.settled_through {
                continue;
            }
            let can_admit = self.entries.len() < limit.min(IDENTITIES_PER_LOAD);
            match self.entries.get_mut(&entry.seq) {
                Some(known) => {
                    if known.turn.availability != entry.availability {
                        known.attempts = 0;
                        if matches!(
                            known.status,
                            RecoveryStatus::Exhausted | RecoveryStatus::Retryable { .. }
                        ) {
                            known.status = RecoveryStatus::Pending;
                        }
                    }
                    known.turn = entry.clone();
                }
                None if can_admit => {
                    self.entries.insert(
                        entry.seq,
                        Recovery {
                            status: RecoveryStatus::Pending,
                            attempts: 0,
                            turn: entry.clone(),
                        },
                    );
                }
                None => self.behind = true,
            }
        }
        self.settle_prefix();
    }

    /// Advances the settled watermark over the contiguous settled prefix.
    fn settle_prefix(&mut self) {
        while let Some(entry) = self.entries.get(&(self.settled_through + 1)) {
            if !entry.status.is_settled() {
                break;
            }
            self.settled_through += 1;
            self.entries.remove(&self.settled_through);
        }
    }

    pub fn record(&mut self, seq: u64, status: RecoveryStatus) {
        if let Some(entry) = self.entries.get_mut(&seq) {
            entry.status = status;
        }
        self.settle_prefix();
    }

    fn defer_unstarted(&mut self) -> bool {
        let seq = self.entries.iter().rev().find_map(|(seq, entry)| {
            (entry.status == RecoveryStatus::Pending && entry.attempts == 0).then_some(*seq)
        });
        if let Some(seq) = seq {
            self.entries.remove(&seq);
            self.behind = true;
        }
        seq.is_some()
    }

    pub fn cover_history(&mut self, covered: super::read::HistoryCoverage, authoritative: bool) {
        match &mut self.history_coverage {
            Some(known)
                if known.captured_end == covered.captured_end
                    && known.history_revision == covered.history_revision
                    && known.total == covered.total =>
            {
                if known.from == known.end {
                    *known = covered;
                } else if covered.from < covered.end
                    && covered.from <= known.end
                    && known.from <= covered.end
                {
                    known.from = known.from.min(covered.from);
                    known.end = known.end.max(covered.end);
                }
            }
            _ if authoritative => self.history_coverage = Some(covered),
            _ => {}
        }
    }

    pub fn settle_history_gap(&mut self) -> bool {
        if !self
            .history_coverage
            .as_ref()
            .is_some_and(super::read::HistoryCoverage::complete)
        {
            return false;
        }
        self.settled_through = self.settled_through.max(self.gap_through);
        self.history_status = Some(RecoveryStatus::Partial);
        self.settle_prefix();
        true
    }

    /// The oldest completion ready to use the scheduler's next slot.
    fn ready(&self, now: tokio::time::Instant) -> Option<CompletedTurn> {
        self.entries
            .values()
            .find(|entry| match entry.status {
                RecoveryStatus::Pending => true,
                RecoveryStatus::Retryable { due, .. } => due <= now,
                _ => false,
            })
            .map(|entry| entry.turn.clone())
    }

    /// The next retry deadline, for the loop's wake-up.
    pub fn next_deadline(&self) -> Option<tokio::time::Instant> {
        self.entries
            .values()
            .filter_map(|entry| match entry.status {
                RecoveryStatus::Retryable { due, .. } => Some(due),
                _ => None,
            })
            .chain(self.history_status.and_then(|status| match status {
                RecoveryStatus::Retryable { due, .. } => Some(due),
                _ => None,
            }))
            .min()
    }

    /// Schedules another attempt, or gives up after the bounded rounds.
    pub fn retry_later(&mut self, seq: u64) -> Option<RecoveryStatus> {
        let entry = self.entries.get_mut(&seq)?;
        entry.attempts += 1;
        let attempts = entry.attempts;
        if attempts > MAX_RETRIES {
            entry.status = RecoveryStatus::Exhausted;
        } else {
            entry.status = RecoveryStatus::Retryable {
                due: tokio::time::Instant::now() + backoff(attempts),
            };
        }
        let status = entry.status;
        self.settle_prefix();
        Some(status)
    }

    fn retry_retained(&mut self, seq: u64, retained_from: u64) -> Option<RecoveryStatus> {
        if seq < retained_from {
            self.record(seq, RecoveryStatus::Exhausted);
            Some(RecoveryStatus::Exhausted)
        } else {
            self.retry_later(seq)
        }
    }

    /// Opens one new round of retries for every failed entry, for a user
    /// retry or a restored connection.
    pub fn retry_failed(&mut self) {
        if matches!(
            self.history_status,
            Some(RecoveryStatus::Retryable { .. } | RecoveryStatus::Exhausted)
        ) {
            self.history_attempts = 0;
            self.history_status = Some(RecoveryStatus::Pending);
        }
        for entry in self.entries.values_mut() {
            entry.status = match entry.status {
                RecoveryStatus::Retryable { .. } | RecoveryStatus::Exhausted => {
                    entry.attempts = 0;
                    RecoveryStatus::Pending
                }
                other => other,
            };
        }
    }

    pub fn retry_history(&mut self) {
        self.history_attempts += 1;
        self.history_status = Some(if self.history_attempts > MAX_RETRIES {
            RecoveryStatus::Exhausted
        } else {
            RecoveryStatus::Retryable {
                due: tokio::time::Instant::now() + backoff(self.history_attempts),
            }
        });
    }
}

fn backoff(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_millis(250),
        2 => Duration::from_secs(1),
        _ => Duration::from_secs(4),
    }
}

/// One recovery read's identity and result.
pub struct RecoveryOutcome {
    pub seq: u64,
    pub turn: TurnRef,
    pub result: Result<TurnRead, RpcError>,
}

impl Session {
    /// How many completions are still to be recovered or explicitly settled.
    pub fn recovery_pending(&self) -> usize {
        self.recovery
            .entries
            .values()
            .filter(|entry| !entry.status.is_settled())
            .count()
    }
}

/// At most this many snapshots are held for sessions this client has not
/// opened; two frame ceilings also cover two maximum snapshots.
pub const PRE_INSTALL_SNAPSHOTS: usize = 2;
/// Total bytes the held snapshots may occupy.
pub const PRE_INSTALL_BUDGET_BYTES: usize = 2 * crate::rpc::CONTROL_FRAME_MAX_BYTES;

/// Snapshots that arrived before their session was opened, oldest first.
#[derive(Default)]
pub struct PreInstall {
    held: Vec<(SessionSnapshot, usize)>,
}

impl PreInstall {
    /// Holds one snapshot; past the count or byte budget the oldest go.
    pub fn hold(&mut self, snapshot: SessionSnapshot, bytes: usize) {
        if self.held.iter().any(|(held, _)| {
            held.session_id == snapshot.session_id
                && held.load == snapshot.load
                && held.revision >= snapshot.revision
        }) {
            return;
        }
        self.held
            .retain(|(held, _)| held.session_id != snapshot.session_id);
        self.held.push((snapshot, bytes));
        let mut total: usize = self.held.iter().map(|(_, bytes)| *bytes).sum();
        while self.held.len() > PRE_INSTALL_SNAPSHOTS || total > PRE_INSTALL_BUDGET_BYTES {
            let (_, dropped) = self.held.remove(0);
            total -= dropped;
        }
    }

    /// Takes the held snapshot for `session_id`, if one arrived.
    pub fn take(&mut self, session_id: &str) -> Option<SessionSnapshot> {
        let at = self
            .held
            .iter()
            .position(|(held, _)| held.session_id == session_id)?;
        Some(self.held.remove(at).0)
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

impl App {
    pub(super) fn recovery_capacity_for(&self, epoch: u64) -> usize {
        let other: usize = self
            .resident
            .iter()
            .filter(|session| session.epoch != epoch)
            .map(|session| session.recovery.entries.len())
            .sum();
        IDENTITIES_TOTAL.saturating_sub(other)
    }

    fn spawn_recovery(
        &mut self,
        epoch: u64,
        seq: Option<u64>,
        work: impl Future<Output = SessionOp> + Send + 'static,
    ) {
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            #[cfg(feature = "e2e-observe")]
            let work = crate::rpc::metrics::recovery_scope(work);
            let _ = tx.send(super::Msg::Session(epoch, Box::new(work.await)));
        });
        self.session_mut(epoch)
            .recovery_tasks
            .0
            .push((seq, task.abort_handle()));
    }
    /// Admits recovery reads in resident-session rotation under the shared slot limit.
    pub(super) fn recover_completions(&mut self) {
        let mut epochs: Vec<u64> = self.resident.iter().map(|session| session.epoch).collect();
        epochs.sort_unstable();
        let next = self
            .recovery_after
            .map_or(0, |last| epochs.partition_point(|epoch| *epoch <= last));
        if next < epochs.len() {
            epochs.rotate_left(next);
        }
        let mut slots = CONCURRENT
            - self
                .resident
                .iter()
                .map(|session| session.recovery_tasks.len())
                .sum::<usize>();
        for _ in 0..CONCURRENT {
            let before = slots;
            for epoch in &epochs {
                if slots == 0 {
                    return;
                }
                let started = self.recover_resident(*epoch);
                if started {
                    slots -= 1;
                    self.recovery_after = Some(*epoch);
                }
            }
            if slots == before {
                return;
            }
        }
    }

    fn recover_resident(&mut self, epoch: u64) -> bool {
        let Some(session) = self.resident.get(epoch) else {
            return false;
        };
        let load = session.load.clone();
        let mut limit = self.recovery_capacity_for(epoch);
        if session.recovery.behind
            && limit < IDENTITIES_PER_LOAD
            && session.recovery.entries.len() == limit
            && session
                .recovery
                .ready(tokio::time::Instant::now())
                .is_none()
        {
            for other in self
                .resident
                .iter_mut()
                .filter(|other| other.epoch != epoch)
            {
                if other.recovery.defer_unstarted() {
                    limit += 1;
                    break;
                }
            }
        }
        let session = self.session_mut(epoch);
        session.recovery.discover(&session.catalog, limit);
        let session = self.resident.get(epoch).unwrap();
        if !session.recovery_tasks.history()
            && match session.recovery.history_status {
                Some(RecoveryStatus::Pending) => true,
                Some(RecoveryStatus::Retryable { due, .. }) => due <= tokio::time::Instant::now(),
                _ => false,
            }
        {
            let through = session.recovery.gap_through;
            let covered = session.recovery.history_coverage.clone();
            let id = session.info.session_id.clone();
            self.session_mut(epoch).recovery.history_status = Some(RecoveryStatus::Fetching);
            let client = self.client.clone();
            self.spawn_recovery(epoch, None, async move {
                SessionOp::HistoryRecovered(
                    through,
                    super::read::read_history(client, id, super::read::Window::CatchUp(covered))
                        .await,
                )
            });
            return true;
        }
        let session = self.resident.get(epoch).unwrap();
        let now = tokio::time::Instant::now();
        let ready = session.recovery.ready(now);
        let session_id = session.info.session_id.clone();
        if let Some(turn) = ready {
            self.session_mut(epoch)
                .recovery
                .record(turn.seq, RecoveryStatus::Fetching);
            let turn_ref = TurnRef::new(session_id.clone(), turn.loop_id.clone());
            let seq = turn.seq;
            let client = self.client.clone();
            let load = load.clone();
            self.spawn_recovery(epoch, Some(seq), async move {
                let result = super::read::read_turn(client, turn_ref.clone(), load).await;
                SessionOp::Recovered(RecoveryOutcome {
                    seq,
                    turn: turn_ref,
                    result,
                })
            });
            return true;
        }
        false
    }

    pub(super) fn on_history_recovered(
        &mut self,
        epoch: u64,
        through: u64,
        result: Result<HistoryRead, RpcError>,
    ) {
        self.session_mut(epoch).recovery_tasks.finish(None);
        let superseded = through != self.session_mut(epoch).recovery.gap_through;
        let (page, limit) = match result {
            Ok(HistoryRead::Complete(page)) => (Some(page), None),
            Ok(HistoryRead::Limited(page, limit)) => (Some(page), Some(limit)),
            Ok(HistoryRead::Partial(partial)) => {
                self.session_mut(epoch).recovery.history_status = Some(RecoveryStatus::Exhausted);
                self.notice(NoticeLevel::Error, partial.message());
                (None, None)
            }
            Err(error) => {
                self.session_mut(epoch).recovery.retry_history();
                if self.session_mut(epoch).recovery.history_status
                    == Some(RecoveryStatus::Exhausted)
                {
                    self.error(error);
                }
                (None, None)
            }
        };
        if let Some(page) = page {
            let covered = page.coverage();
            self.install_history(epoch, *page);
            self.session_mut(epoch)
                .recovery
                .cover_history(covered, true);
            if superseded {
                self.session_mut(epoch).recovery.history_status = Some(RecoveryStatus::Pending);
            } else if !self.session_mut(epoch).recovery.settle_history_gap() {
                self.session_mut(epoch).recovery.history_status = Some(RecoveryStatus::Exhausted);
                self.notice(
                    NoticeLevel::Info,
                    limit.map_or_else(
                        || "More history remains to recover; /recover to continue".into(),
                        |limit| limit.message(),
                    ),
                );
            } else {
                let gap = self.session_mut(epoch).recovery.gap_through > 0;
                if gap {
                    self.notice(NoticeLevel::Error, "Saved history recovered; unsaved turns outside the completion catalog may be missing");
                }
                self.session_mut(epoch).recovery.retry_failed();
                self.session_mut(epoch).ordering_gap = false;
            }
        }
        self.recover_completions();
    }

    /// Settles one recovered completion; the watermark moves only when the
    /// body is complete or explicitly unavailable.
    pub(super) fn on_recovered(&mut self, epoch: u64, outcome: RecoveryOutcome) {
        {
            let session = self.session_mut(epoch);
            session.recovery_tasks.finish(Some(outcome.seq));
            if !session.recovery.entries.contains_key(&outcome.seq) {
                self.recover_completions();
                return;
            }
        }
        let mut moved = None;
        let unreadable = {
            let session = self.session_mut(epoch);
            let retained_from = session.catalog.retained_from_seq;
            let retired = outcome.seq < retained_from;
            match outcome.result {
                Ok(TurnRead::Complete(content)) => {
                    let directory = session.recovery.entries[&outcome.seq].turn.clone();
                    let loop_id = &directory.loop_id;
                    moved = session.reconcile_completion(
                        &directory,
                        &content.details,
                        &content.items,
                        content.interrupted.as_ref(),
                        content.persistence,
                        content.persistence_uncertain,
                    );
                    // Retained identities can retry failed placement; a retired
                    // identity needs a visible failure instead of a futile retry.
                    let failure = match moved.is_some() || session.is_closed(loop_id) {
                        true => {
                            session
                                .recovery
                                .record(outcome.seq, RecoveryStatus::Complete);
                            None
                        }
                        false => (session.recovery.retry_retained(outcome.seq, retained_from)
                            == Some(RecoveryStatus::Exhausted)).then(|| {
                            if retired {
                                "The recovered body could not be placed after its completion left the catalog; existing content was kept".to_owned()
                            } else {
                                "The completed body could not be placed; /recover to retry".to_owned()
                            }
                        }),
                    };
                    if let Some(message) = &failure {
                        session.note_body_recovery(
                            loop_id,
                            RecoveryStatus::Exhausted,
                            message.clone(),
                        );
                    }
                    if moved.is_none()
                        && session.ordering_gap
                        && session.recovery.history_status.is_none()
                    {
                        session.recovery.history_status = Some(RecoveryStatus::Pending);
                    }
                    if moved.is_some() {
                        session
                            .body_recovery
                            .retain(|note| note.loop_id != *loop_id);
                        for (key, facts) in &content.tool_facts {
                            if let Some(card) = session.transcript.find_tool_mut(key) {
                                card.apply_read(facts);
                            }
                        }
                    }
                    #[cfg(feature = "e2e-observe")]
                    if moved.is_some() {
                        self.observe_install(epoch, &content, Some(&directory));
                    }
                    failure
                }
                Ok(TurnRead::Partial(partial)) => {
                    let message = partial.message();
                    let turn = session.recovery.entries[&outcome.seq].turn.clone();
                    moved = session.incomplete_completion(
                        &turn,
                        RecoveryStatus::Partial,
                        message.clone(),
                    );
                    session
                        .recovery
                        .record(outcome.seq, RecoveryStatus::Partial);
                    Some(message)
                }
                Ok(TurnRead::Unavailable(reason)) => {
                    let mut turn = session.recovery.entries[&outcome.seq].turn.clone();
                    turn.persistence = reason.persistence.or(turn.persistence);
                    turn.persistence_uncertain |= reason.persistence_uncertain;
                    moved = session.incomplete_completion(
                        &turn,
                        RecoveryStatus::Unavailable,
                        reason.message(),
                    );
                    session
                        .recovery
                        .record(outcome.seq, RecoveryStatus::Unavailable);
                    Some(reason.message())
                }
                // A pending answer is not a completed turn: retry within bounds.
                Ok(TurnRead::Pending) => {
                    if session.recovery.retry_retained(outcome.seq, retained_from)
                        == Some(RecoveryStatus::Exhausted)
                    {
                        session.note_body_recovery(
                            &outcome.turn.loop_id,
                            RecoveryStatus::Exhausted,
                            if retired {
                                "The completed result was still pending after its completion left the catalog".into()
                            } else {
                                "The completed turn result is still pending; /recover to retry".into()
                            },
                        );
                    }
                    None
                }
                Err(error) => {
                    if session.recovery.retry_retained(outcome.seq, retained_from)
                        == Some(RecoveryStatus::Exhausted)
                    {
                        session.note_body_recovery(
                            &outcome.turn.loop_id,
                            RecoveryStatus::Exhausted,
                            if retired {
                                format!(
                                    "Recovery failed after its completion left the catalog: {}",
                                    error.message
                                )
                            } else {
                                format!("Recovery failed; /recover to retry: {}", error.message)
                            },
                        );
                        Some(error.message)
                    } else {
                        None
                    }
                }
            }
        };
        if let Some((start, grown, end)) = moved {
            self.moved_region(epoch, start, end, grown);
        }
        #[cfg(feature = "e2e-observe")]
        self.observe_recovery(epoch, &outcome.turn.loop_id);
        if let Some(message) = unreadable {
            self.notice(NoticeLevel::Error, message);
        }
        self.recover_completions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ResultAvailability, TurnPersistence, UnavailableReason};
    use serde_json::json;

    fn directory(
        through: u64,
        retained: u64,
        seqs: &[u64],
        truncated: bool,
    ) -> CompletionDirectory {
        CompletionDirectory {
            through_seq: through,
            retained_from_seq: retained,
            truncated,
            entries: seqs
                .iter()
                .map(|seq| CompletedTurn {
                    seq: *seq,
                    loop_id: format!("l{seq}"),
                    input_id: None,
                    completion_kind: crate::protocol::CompletionKind::Reported,
                    outcome_kind: Some(crate::protocol::CompletionOutcome::Completed),
                    persistence: Some(TurnPersistence::Persisted),
                    persistence_uncertain: false,
                    availability: ResultAvailability::Stored,
                    unavailable_reason: None,
                    after_saved_loop_id: None,
                })
                .collect(),
        }
    }

    #[test]
    fn discovery_keeps_only_unsettled_completions_and_reports_truncation() {
        let mut state = RecoveryState::default();
        state.discover(&directory(2, 1, &[1, 2], false), IDENTITIES_PER_LOAD);
        assert_eq!(state.entries.len(), 2);
        state.record(1, RecoveryStatus::Complete);
        assert_eq!(state.settled_through, 1, "only the settled prefix advances");
        assert_eq!(state.entries.keys().copied().collect::<Vec<_>>(), vec![2]);
        // A window that stopped listing 2 while it was unsettled lost it.
        state.discover(&directory(5, 4, &[4, 5], true), IDENTITIES_PER_LOAD);
        assert_eq!(
            state.settled_through, 1,
            "discovery alone cannot settle an unread gap"
        );
        assert_eq!(
            state.entries.keys().copied().collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert!(state.truncated_gap, "the window skipped 2 and 3");
        assert!(
            !state.settle_history_gap(),
            "discovery alone proves no history coverage"
        );
        state.cover_history(
            super::super::read::HistoryCoverage {
                captured_end: 10,
                history_revision: "history".into(),
                total: 4,
                from: 0,
                end: 4,
            },
            true,
        );
        assert!(state.settle_history_gap());
        assert_eq!(state.settled_through, 3);
        state.record(4, RecoveryStatus::Complete);
        state.record(5, RecoveryStatus::Complete);
        assert_eq!(
            state.settled_through, 5,
            "a lost entry moves the watermark past the gap it can never settle"
        );
        assert!(state.entries.is_empty());
        state.discover(&directory(9, 8, &[8, 9], true), IDENTITIES_PER_LOAD);
        assert!(state.truncated_gap, "the window skipped 6 and 7");
    }

    #[test]
    fn a_newer_revision_wins_and_duplicates_are_ignored() {
        let mut state = RecoveryState::default();
        assert!(state.accept_revision(2));
        assert!(!state.accept_revision(2));
        assert!(!state.accept_revision(1));
    }

    #[test]
    fn only_contiguous_coverage_of_the_newer_history_can_settle_a_catalog_gap() {
        let mut state = RecoveryState::default();
        state.discover(&directory(5, 4, &[4, 5], true), IDENTITIES_PER_LOAD);
        let newer = super::super::read::HistoryCoverage {
            captured_end: 100,
            history_revision: "H1".into(),
            total: 100,
            from: 80,
            end: 100,
        };
        state.cover_history(newer.clone(), true);
        assert!(
            !state.settle_history_gap(),
            "the verified suffix does not include the missing prefix"
        );
        state.cover_history(
            super::super::read::HistoryCoverage {
                captured_end: 80,
                history_revision: "H0".into(),
                total: 80,
                from: 0,
                end: 80,
            },
            false,
        );
        assert!(
            !state.settle_history_gap(),
            "old coverage to zero cannot complete a newer history"
        );
        assert_eq!(state.history_coverage.as_ref(), Some(&newer));
        state.cover_history(
            super::super::read::HistoryCoverage {
                from: 0,
                end: 80,
                ..newer
            },
            false,
        );
        assert!(state.settle_history_gap());
        assert_eq!(state.settled_through, 3);
    }

    #[test]
    fn recovery_selects_the_oldest_ready_completion_and_bounds_retries() {
        let mut state = RecoveryState::default();
        state.discover(&directory(6, 1, &[1, 2, 3, 4], false), IDENTITIES_PER_LOAD);
        let now = tokio::time::Instant::now();
        assert_eq!(state.ready(now).unwrap().seq, 1);
        state.record(1, RecoveryStatus::Fetching);
        assert_eq!(state.ready(now).unwrap().seq, 2);
        // Entry 2 has an unsettled predecessor, so its retries cannot retire
        // the prefix and stay observable.
        for attempt in 1..=MAX_RETRIES {
            state.record(2, RecoveryStatus::Fetching);
            state.retry_later(2);
            let entry = &state.entries[&2];
            assert!(matches!(
                entry.status,
                RecoveryStatus::Retryable { due } if due > now
            ));
            assert_eq!(entry.attempts, attempt);
        }
        state.retry_later(2);
        assert_eq!(state.entries[&2].status, RecoveryStatus::Exhausted);
        assert_eq!(state.settled_through, 0);
        // An entry declared unreadable still settles, so it is never re-read.
        state.record(1, RecoveryStatus::Complete);
        assert_eq!(state.settled_through, 1, "exhausted is not unavailable");
        state.record(2, RecoveryStatus::Unavailable);
        assert_eq!(state.settled_through, 2);
        // A lost connection retries whatever is still unsettled, and only that.
        state.retry_later(4);
        state.retry_failed();
        assert_eq!(state.entries[&4].status, RecoveryStatus::Pending);
        assert!(!state.entries.contains_key(&2));
    }

    #[test]
    fn the_identity_cap_marks_the_load_as_behind() {
        let mut state = RecoveryState::default();
        let seqs: Vec<u64> = (1..=(IDENTITIES_PER_LOAD as u64 + 4)).collect();
        state.discover(
            &directory(*seqs.last().unwrap(), 1, &seqs, false),
            IDENTITIES_PER_LOAD,
        );
        assert_eq!(state.entries.len(), IDENTITIES_PER_LOAD);
        assert!(state.behind);
        assert_eq!(state.observed_through, *seqs.last().unwrap());
        assert_eq!(state.settled_through, 0);
        let catalog = directory(*seqs.last().unwrap(), 1, &seqs, false);
        for seq in seqs {
            state.record(seq, RecoveryStatus::Complete);
            state.discover(&catalog, IDENTITIES_PER_LOAD);
        }
        assert!(
            !state.behind,
            "retained cap debt clears without a new snapshot"
        );
        assert_eq!(state.settled_through, catalog.through_seq);
    }

    #[test]
    fn capacity_deferral_keeps_attempted_and_settled_identities_intact() {
        let catalog = directory(5, 1, &[1, 2, 3, 4, 5], false);
        let mut state = RecoveryState::default();
        state.discover(&catalog, IDENTITIES_PER_LOAD);
        state.record(1, RecoveryStatus::Fetching);
        state.retry_later(2);
        for _ in 0..=MAX_RETRIES {
            state.retry_later(3);
        }
        state.record(4, RecoveryStatus::Partial);
        let retained: Vec<_> = state
            .entries
            .iter()
            .take(4)
            .map(|(seq, entry)| (*seq, entry.status, entry.attempts))
            .collect();
        assert!(state.defer_unstarted());
        assert!(!state.entries.contains_key(&5));
        for _ in 0..3 {
            state.discover(&catalog, 4);
            assert!(state.behind);
            assert!(!state.defer_unstarted());
            assert_eq!(
                state
                    .entries
                    .iter()
                    .map(|(seq, entry)| (*seq, entry.status, entry.attempts))
                    .collect::<Vec<_>>(),
                retained
            );
        }
        state.discover(&catalog, 5);
        assert!(!state.behind);
        assert_eq!(state.entries[&5].status, RecoveryStatus::Pending);
    }

    #[test]
    fn a_retry_deadline_is_armed_only_while_an_entry_waits() {
        let mut state = RecoveryState::default();
        state.discover(&directory(1, 1, &[1], false), IDENTITIES_PER_LOAD);
        assert!(
            state.next_deadline().is_none(),
            "a fresh entry is ready now"
        );
        state.retry_later(1);
        assert!(state.next_deadline().is_some());
        state.record(1, RecoveryStatus::Complete);
        assert!(state.next_deadline().is_none());
    }

    #[test]
    fn an_unavailable_directory_entry_is_displayed_before_it_can_settle() {
        let mut state = RecoveryState::default();
        let mut gone = directory(1, 1, &[1], false);
        gone.entries[0].availability = ResultAvailability::Unavailable;
        gone.entries[0].unavailable_reason = Some(UnavailableReason::Expired);
        state.discover(&gone, IDENTITIES_PER_LOAD);
        assert_eq!(state.settled_through, 0);
        assert_eq!(state.entries[&1].status, RecoveryStatus::Pending);
        state.record(1, RecoveryStatus::Unavailable);
        assert_eq!(state.settled_through, 1);
        assert!(state.next_deadline().is_none());
    }

    #[test]
    fn a_snapshot_without_a_completed_directory_claims_nothing() {
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "session_id": "s", "load": "l", "revision": 1,
            "execution": {"type": "none"},
            "queue": {"items": [], "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap();
        let mut state = RecoveryState::default();
        state.discover(&snapshot.completions, IDENTITIES_PER_LOAD);
        assert_eq!(state.observed_through, 0);
        assert!(state.entries.is_empty());
        assert!(!state.truncated_gap);
    }
}

#[cfg(test)]
mod pre_install_tests {
    use super::*;
    use serde_json::json;

    fn snapshot(session_id: &str, revision: u64) -> SessionSnapshot {
        serde_json::from_value(json!({
            "session_id": session_id, "load": format!("load_{session_id}"), "revision": revision,
            "execution": {"type": "none"}, "queue": {"items": [], "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .expect("a snapshot")
    }

    #[test]
    fn at_most_two_unopened_snapshots_are_held() {
        let mut held = PreInstall::default();
        held.hold(snapshot("a", 1), 1024);
        held.hold(snapshot("b", 1), 1024);
        assert!(!held.is_empty());
        held.hold(snapshot("c", 1), 1024);
        assert!(!held.is_empty(), "only the newest two are kept");
        assert!(held.take("a").is_none(), "the oldest was dropped");
        assert!(held.take("b").is_some());
        assert!(held.take("c").is_some());
        assert!(held.is_empty());
    }

    #[test]
    fn the_byte_budget_bounds_two_frame_ceilings() {
        let mut held = PreInstall::default();
        held.hold(snapshot("a", 1), PRE_INSTALL_BUDGET_BYTES);
        assert!(!held.is_empty());
        held.hold(snapshot("b", 1), 1);
        assert!(
            !held.is_empty(),
            "the byte budget evicts, not only the count"
        );
        assert!(
            held.take("a").is_none(),
            "the oldest exceeded the byte budget"
        );
        assert!(held.take("b").is_some());
        assert!(held.is_empty());
        // Two maximum control frames are the budget: two of them plus a byte is not.
        assert_eq!(
            PRE_INSTALL_BUDGET_BYTES,
            2 * crate::rpc::CONTROL_FRAME_MAX_BYTES
        );
        // The ceiling is what admits the measured 52,237,130-byte snapshot;
        // the budget holds two of them.
        let measured = PRE_INSTALL_BUDGET_BYTES / 2;
        assert!(measured >= 52_237_130, "two measured snapshots must fit");
        assert_eq!(measured, crate::rpc::CONTROL_FRAME_MAX_BYTES);
    }

    #[test]
    fn a_later_snapshot_for_the_same_session_replaces_the_held_one() {
        let mut held = PreInstall::default();
        held.hold(snapshot("a", 1), 10);
        held.hold(snapshot("a", 5), 10);
        assert!(!held.is_empty());
        let taken = held.take("a").expect("the held snapshot");
        assert_eq!(taken.revision, 5, "the newest observation is the one kept");
    }
}
