//! Completion discovery and body recovery.
//!
//! The snapshot's completion directory says which turns reached the Agent's
//! completion boundary. Every entry the client has not settled yet is read back
//! through `turn.result`, and only a fully verified body replaces transcript
//! content.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::protocol::{CompletedTurn, CompletionDirectory, SessionSnapshot, TurnRef};

use super::tasks::TurnRead;
use super::{App, NoticeLevel, Session, SessionOp};
use crate::protocol::RpcError;

/// Turns recovering at once.
pub const CONCURRENT: usize = 2;
/// Unsettled identities kept per load.
const IDENTITIES_PER_LOAD: usize = 256;
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
        attempts: u32,
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

/// What a snapshot's revision means for the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Revision {
    /// Newer than what was applied; apply it.
    Fresh,
    /// Already applied, or delayed behind a newer observation.
    Stale,
    /// A different resident instance of the same session.
    OtherLoad,
    /// This session has no installed instance yet.
    UnknownLoad,
}

/// The bounded recovery state of one resident instance.
#[derive(Default)]
pub struct RecoveryState {
    /// A copy of the installed load, for the tests that read it alone.
    load: Option<String>,
    /// The last applied snapshot revision of the installed load.
    pub revision: u64,
    /// The highest completion sequence the directory has reported.
    pub observed_through: u64,
    /// Every sequence at or below this is recovered or explicitly unavailable.
    pub settled_through: u64,
    /// A recovery generation; a read from an older one is stale.
    pub generation: u64,
    /// Unsettled completions by sequence.
    pub entries: BTreeMap<u64, Recovery>,
    /// The directory's window moved past completions this client never read.
    pub truncated_gap: bool,
    /// The identity cap dropped work that still has to catch up.
    pub behind: bool,
    pub gap_through: u64,
    pub history_status: Option<RecoveryStatus>,
    pub history_coverage: Option<super::tasks::HistoryCoverage>,
    history_attempts: u32,
}

impl RecoveryState {
    /// Resets every watermark and identity for a newly installed instance.
    pub fn begin(&mut self, load: &str, generation: u64) {
        let same = self.load.as_deref() == Some(load);
        self.generation = generation;
        self.load = Some(load.to_owned());
        if same {
            return;
        }
        self.revision = 0;
        self.observed_through = 0;
        self.settled_through = 0;
        self.entries.clear();
        self.truncated_gap = false;
        self.behind = false;
        self.gap_through = 0;
        self.history_status = None;
        self.history_coverage = None;
        self.history_attempts = 0;
    }

    /// Whether a read from `generation` still belongs to this instance.
    pub fn accepts(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// Decides what a snapshot revision means for the installed instance.
    pub fn accept_revision(&mut self, load: &str, revision: u64) -> Revision {
        match self.load.as_deref() {
            Some(current) if current == load => match revision <= self.revision {
                true => Revision::Stale,
                false => {
                    self.revision = revision;
                    Revision::Fresh
                }
            },
            Some(_) => Revision::OtherLoad,
            None => Revision::UnknownLoad,
        }
    }

    /// Takes a snapshot's completion directory, keeping only unsettled work.
    pub fn discover(&mut self, directory: &CompletionDirectory) {
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
        for entry in &directory.entries {
            if entry.seq <= self.settled_through {
                continue;
            }
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
                None => {
                    self.entries.insert(
                        entry.seq,
                        Recovery {
                            status: RecoveryStatus::Pending,
                            attempts: 0,
                            turn: entry.clone(),
                        },
                    );
                }
            }
        }
        // The directory is a contiguous suffix: an entry it stopped listing was
        // evicted from the Agent's window and can only be shown as a gap.
        let oldest_retained = directory.retained_from_seq;
        let dropped: Vec<u64> = self
            .entries
            .keys()
            .copied()
            .filter(|seq| *seq < oldest_retained)
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
        while self.entries.len() > IDENTITIES_PER_LOAD {
            let Some(oldest) = self.entries.keys().next_back().copied() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest)
                && !entry.status.is_settled()
            {
                self.behind = true;
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

    fn trim(&mut self, limit: usize) {
        while self.entries.len() > limit {
            let Some(seq) = self
                .entries
                .iter()
                .rev()
                .find(|(_, entry)| entry.status != RecoveryStatus::Fetching)
                .map(|(seq, _)| *seq)
            else {
                break;
            };
            self.entries.remove(&seq);
            self.behind = true;
        }
    }

    pub fn cover_history(&mut self, covered: super::tasks::HistoryCoverage, authoritative: bool) {
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
            .is_some_and(super::tasks::HistoryCoverage::complete)
        {
            return false;
        }
        self.settled_through = self.settled_through.max(self.gap_through);
        self.history_status = Some(RecoveryStatus::Partial);
        self.settle_prefix();
        true
    }

    /// Bounded work ready now; `in_flight` reads already hold a slot.
    pub fn ready(&self, now: tokio::time::Instant, in_flight: usize) -> Vec<CompletedTurn> {
        let room = CONCURRENT.saturating_sub(in_flight);
        self.entries
            .values()
            .filter(|entry| match entry.status {
                RecoveryStatus::Pending => true,
                RecoveryStatus::Retryable { due, .. } => due <= now,
                _ => false,
            })
            .take(room)
            .map(|entry| entry.turn.clone())
            .collect()
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
    pub fn retry_later(&mut self, seq: u64) {
        let Some(entry) = self.entries.get_mut(&seq) else {
            return;
        };
        entry.attempts += 1;
        let attempts = entry.attempts;
        if attempts > MAX_RETRIES {
            entry.status = RecoveryStatus::Exhausted;
        } else {
            entry.status = RecoveryStatus::Retryable {
                attempts,
                due: tokio::time::Instant::now() + backoff(attempts),
            };
        }
        self.settle_prefix();
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
                attempts: self.history_attempts,
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
    pub generation: u64,
    pub result: Result<TurnRead, RpcError>,
}

impl Session {
    /// How many reads of this session's completions are in flight.
    pub fn recovery_fetching(&self) -> usize {
        self.recovery_tasks
            .0
            .iter()
            .filter(|(seq, _)| seq.is_some())
            .count()
    }

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
    /// The budget forced a drop, so the install must refresh its control.
    pub overflowed: bool,
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
            self.overflowed = true;
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
    fn spawn_recovery(
        &mut self,
        seq: Option<u64>,
        work: impl Future<Output = SessionOp> + Send + 'static,
    ) {
        let epoch = self.session.as_ref().unwrap().epoch;
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            #[cfg(feature = "e2e-observe")]
            let work = crate::rpc::metrics::recovery_scope(work);
            let _ = tx.send(super::Msg::Session(epoch, Box::new(work.await)));
        });
        self.session_mut()
            .recovery_tasks
            .0
            .push((seq, task.abort_handle()));
    }
    /// Applies one authoritative control snapshot; it is the only entry that
    /// writes execution, queue, pause, interaction, compaction and blocking.
    pub fn apply_snapshot(&mut self, snapshot: SessionSnapshot) {
        let id = snapshot.session_id.clone();
        let Some(target) = self.snapshot_target(&id) else {
            return;
        };
        if let Some(index) = target {
            self.with_background(index, |app| app.apply_snapshot(snapshot));
            return;
        }
        let observed_before = self.session.as_ref().unwrap().recovery.observed_through;
        let applied = self.apply_to_foreground(&snapshot);
        if applied {
            let background = self.background_op;
            let mut ring = false;
            for turn in snapshot
                .completions
                .entries
                .iter()
                .filter(|turn| turn.seq > observed_before)
            {
                let session = self.session_mut();
                if let Some(live) = session
                    .live
                    .iter_mut()
                    .chain(&mut session.ended_live)
                    .find(|live| live.turn.loop_id == turn.loop_id)
                    && live.completion_background.is_none()
                {
                    live.completion_background = Some(background);
                    ring |= live.started.elapsed() >= crate::title::LONG_TURN;
                }
            }
            self.ring |= ring;
            let other: usize = self
                .background
                .iter()
                .map(|session| session.recovery.entries.len())
                .sum();
            self.session_mut()
                .recovery
                .trim(1024usize.saturating_sub(other));
            if snapshot.completions.through_seq > observed_before {
                self.stamp_activity();
            }
            if let Some(title) = self
                .session
                .as_ref()
                .filter(|session| !session.auto_title_asked)
                .and_then(Session::auto_title)
            {
                self.session_mut().auto_title_asked = true;
                self.rename(&title);
            }
            #[cfg(feature = "e2e-observe")]
            self.observe_control(&snapshot);
            self.start_adopted_turn();
            // A newly observed approval is announced from the snapshot itself,
            // whether or not its event reached the client first.
            let pending = self
                .live(&id)
                .and_then(|session| session.interaction.as_ref())
                .map(|interaction| interaction.interaction_id.clone());
            if pending.is_some() && pending != self.approval_seen {
                self.approval_seen = pending;
                self.on_approval_requested();
            }
            self.recover_completions();
        }
    }

    /// Holds a snapshot whose session is not open yet.
    pub(super) fn hold_snapshot(&mut self, snapshot: SessionSnapshot, bytes: usize) {
        self.pre_install.hold(snapshot, bytes);
    }

    /// Where a snapshot belongs: `Some(index)` background, `None` foreground,
    /// and `Option::None` when this client has no live instance of it.
    fn snapshot_target(&self, id: &str) -> Option<Option<usize>> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == id)
        {
            return Some(None);
        }
        self.live_index(id).map(Some)
    }

    fn apply_to_foreground(&mut self, snapshot: &SessionSnapshot) -> bool {
        let session = self.session.as_mut().expect("a matched live session");
        let revision = session
            .recovery
            .accept_revision(&snapshot.load, snapshot.revision);
        match revision {
            Revision::Fresh => {}
            // A snapshot of another instance of this session never switches it,
            // and neither does a delayed or duplicate observation.
            Revision::Stale | Revision::OtherLoad | Revision::UnknownLoad => return false,
        }
        session.apply_control(snapshot);
        session.recovery.discover(&snapshot.completions);
        session.replay_early();
        true
    }

    /// Asks for the current control snapshot; receipts get their queue and
    /// execution from here, never from their own answer.
    pub(super) fn refresh_control(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.control_in_flight {
            session.control_due = true;
            return;
        }
        session.control_due = false;
        session.control_in_flight = true;

        self.call(
            crate::protocol::METHOD_SESSION_STATE,
            serde_json::json!({}),
            super::SessionOp::State,
        );
    }

    /// Fetches the snapshot behind control hints, at most one read at a time.
    pub(super) fn pull_control(&mut self) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.control_due)
        {
            self.refresh_control();
        }
    }

    /// Starts waiting on an execution a control snapshot adopted, so its
    /// outcome arrives even though no receipt named it.
    pub(super) fn start_adopted_turn(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(turn) = session.adopted.take() else {
            return;
        };
        let load = session
            .load
            .clone()
            .expect("adopted execution has an installed load");
        self.spawn(super::tasks::wait_turn(self.client.clone(), turn, load));
    }

    /// Installs the instance identity once an open or create completed.
    pub(super) fn install_load(&mut self, load: String) {
        let generation = self.next_recovery_generation();
        let session = self.session_mut();
        session.recovery_tasks = RecoveryTasks::default();
        session.load = Some(load.clone());
        session.recovery.begin(&load, generation);
        self.recover_completions();
    }

    fn next_recovery_generation(&mut self) -> u64 {
        self.recovery_generation = self
            .recovery_generation
            .checked_add(1)
            .expect("recovery generation exhausted");
        self.recovery_generation
    }

    /// Admits recovery reads in resident-session rotation under the shared slot limit.
    pub(super) fn recover_completions(&mut self) {
        let mut epochs: Vec<u64> = self
            .session
            .iter()
            .chain(&self.background)
            .map(|session| session.epoch)
            .collect();
        epochs.sort_unstable();
        let next = self
            .recovery_after
            .map_or(0, |last| epochs.partition_point(|epoch| *epoch <= last));
        if next < epochs.len() {
            epochs.rotate_left(next);
        }
        let mut slots = CONCURRENT
            - self
                .session
                .iter()
                .chain(&self.background)
                .map(|session| session.recovery_tasks.len())
                .sum::<usize>();
        for _ in 0..CONCURRENT {
            let before = slots;
            for epoch in &epochs {
                if slots == 0 {
                    return;
                }
                let mut started = false;
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.epoch == *epoch)
                {
                    started = self.recover_resident(CONCURRENT - slots);
                } else if let Some(index) = self
                    .background
                    .iter()
                    .position(|session| session.epoch == *epoch)
                {
                    self.with_background(index, |app| {
                        started = app.recover_resident(CONCURRENT - slots)
                    });
                }
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

    fn recover_resident(&mut self, in_flight: usize) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let Some(load) = session.load.clone() else {
            return false;
        };
        let other: usize = self
            .background
            .iter()
            .map(|session| session.recovery.entries.len())
            .sum();
        let session = self.session_mut();
        session.recovery.discover(&session.catalog);
        session.recovery.trim(1024usize.saturating_sub(other));
        let session = self.session.as_ref().unwrap();
        if !session.recovery_tasks.history()
            && match session.recovery.history_status {
                Some(RecoveryStatus::Pending) => true,
                Some(RecoveryStatus::Retryable { due, .. }) => due <= tokio::time::Instant::now(),
                _ => false,
            }
        {
            let generation = session.recovery.generation;
            let through = session.recovery.gap_through;
            let covered = session.recovery.history_coverage.clone();
            let id = session.info.session_id.clone();
            self.session_mut().recovery.history_status = Some(RecoveryStatus::Fetching);
            let client = self.client.clone();
            self.spawn_recovery(None, async move {
                SessionOp::HistoryRecovered(
                    generation,
                    through,
                    super::tasks::read_history(client, id, super::tasks::Window::CatchUp(covered))
                        .await,
                )
            });
            return true;
        }
        let session = self.session.as_ref().unwrap();
        let now = tokio::time::Instant::now();
        let ready = session.recovery.ready(now, in_flight);
        let session_id = session.info.session_id.clone();
        let generation = session.recovery.generation;
        if let Some(turn) = ready.into_iter().next() {
            self.session_mut()
                .recovery
                .record(turn.seq, RecoveryStatus::Fetching);
            let turn_ref = TurnRef::new(session_id.clone(), turn.loop_id.clone());
            let seq = turn.seq;
            let client = self.client.clone();
            let load = load.clone();
            self.spawn_recovery(Some(seq), async move {
                let result = super::tasks::read_turn(client, turn_ref.clone(), Some(load)).await;
                SessionOp::Recovered(RecoveryOutcome {
                    seq,
                    turn: turn_ref,
                    generation,
                    result,
                })
            });
            return true;
        }
        false
    }

    /// Settles one recovered completion; the watermark moves only when the
    /// body is complete or explicitly unavailable.
    pub(super) fn on_recovered(&mut self, outcome: RecoveryOutcome) {
        {
            let session = self.session_mut();
            if !session.recovery.accepts(outcome.generation) {
                return;
            }
            session.recovery_tasks.finish(Some(outcome.seq));
            if !session.recovery.entries.contains_key(&outcome.seq) {
                self.recover_completions();
                return;
            }
        }
        let mut moved = None;
        let unreadable = {
            let session = self.session_mut();
            match outcome.result {
                Ok(TurnRead::Complete(content)) => {
                    let directory = session
                        .recovery
                        .entries
                        .get(&outcome.seq)
                        .map(|entry| entry.turn.clone());
                    let loop_id = directory
                        .as_ref()
                        .map_or_else(|| outcome.turn.loop_id.clone(), |turn| turn.loop_id.clone());
                    moved = directory.as_ref().and_then(|turn| {
                        session.reconcile_completion(
                            turn,
                            &content.outcome,
                            &content.items,
                            content.interrupted.as_ref(),
                            content.persistence,
                            content.persistence_uncertain,
                        )
                    });
                    // A body that installed nothing (its streamed region was
                    // evicted) is not settled: the descriptor stays retryable
                    // so the turn can still be repaired.
                    match moved.is_some() || session.is_closed(&loop_id) {
                        true => session
                            .recovery
                            .record(outcome.seq, RecoveryStatus::Complete),
                        false => session.recovery.retry_later(outcome.seq),
                    }
                    if moved.is_none()
                        && session.ordering_gap
                        && session.recovery.history_status.is_none()
                    {
                        session.recovery.history_status = Some(RecoveryStatus::Pending);
                    }
                    if moved.is_some() {
                        session.body_recovery.retain(|note| note.loop_id != loop_id);
                        for (key, facts) in &content.tool_facts {
                            if let Some(card) = session.transcript.find_tool_mut(key) {
                                card.apply_read(facts);
                            }
                        }
                    }
                    #[cfg(feature = "e2e-observe")]
                    if moved.is_some() {
                        self.observe_install(&content, directory.as_ref());
                    }
                    None
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
                    session.recovery.retry_later(outcome.seq);
                    if session
                        .recovery
                        .entries
                        .get(&outcome.seq)
                        .is_some_and(|entry| entry.status == RecoveryStatus::Exhausted)
                    {
                        session.note_body_recovery(
                            &outcome.turn.loop_id,
                            RecoveryStatus::Exhausted,
                            "The completed turn result is still pending; /recover to retry".into(),
                        );
                    }
                    None
                }
                Ok(TurnRead::Failed(error)) | Err(error) => {
                    session.recovery.retry_later(outcome.seq);
                    if session
                        .recovery
                        .entries
                        .get(&outcome.seq)
                        .is_some_and(|entry| entry.status == RecoveryStatus::Exhausted)
                    {
                        session.note_body_recovery(
                            &outcome.turn.loop_id,
                            RecoveryStatus::Exhausted,
                            format!("Recovery failed; /recover to retry: {}", error.message),
                        );
                    }
                    match session.recovery.entries.get(&outcome.seq).map(|e| e.status) {
                        Some(RecoveryStatus::Exhausted) => Some(error.message),
                        _ => None,
                    }
                }
            }
        };
        if let Some((start, grown, end)) = moved {
            self.moved_region(start, end, grown);
        }
        #[cfg(feature = "e2e-observe")]
        self.observe_recovery(&outcome.turn.loop_id);
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
                    next_loop_id: None,
                    after_saved_loop_id: None,
                })
                .collect(),
        }
    }

    #[test]
    fn discovery_keeps_only_unsettled_completions_and_reports_truncation() {
        let mut state = RecoveryState::default();
        state.begin("load_1", 1);
        state.discover(&directory(2, 1, &[1, 2], false));
        assert_eq!(state.entries.len(), 2);
        state.record(1, RecoveryStatus::Complete);
        assert_eq!(state.settled_through, 1, "only the settled prefix advances");
        assert_eq!(state.entries.keys().copied().collect::<Vec<_>>(), vec![2]);
        // A window that stopped listing 2 while it was unsettled lost it.
        state.discover(&directory(5, 4, &[4, 5], true));
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
            super::super::tasks::HistoryCoverage {
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
        state.discover(&directory(9, 8, &[8, 9], true));
        assert!(state.truncated_gap, "the window skipped 6 and 7");
    }

    #[test]
    fn a_newer_revision_wins_and_duplicates_are_ignored() {
        let mut state = RecoveryState::default();
        assert_eq!(state.accept_revision("a", 2), Revision::UnknownLoad);
        state.begin("a", 1);
        assert_eq!(state.accept_revision("a", 2), Revision::Fresh);
        assert_eq!(state.accept_revision("a", 2), Revision::Stale);
        assert_eq!(state.accept_revision("a", 1), Revision::Stale);
        assert_eq!(state.accept_revision("b", 9), Revision::OtherLoad);
    }

    #[test]
    fn only_contiguous_coverage_of_the_newer_history_can_settle_a_catalog_gap() {
        let mut state = RecoveryState::default();
        state.begin("load", 1);
        state.discover(&directory(5, 4, &[4, 5], true));
        let newer = super::super::tasks::HistoryCoverage {
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
            super::super::tasks::HistoryCoverage {
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
            super::super::tasks::HistoryCoverage {
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
    fn recovery_bounds_retries_and_the_concurrent_reads() {
        let mut state = RecoveryState::default();
        state.begin("load", 1);
        state.discover(&directory(6, 1, &[1, 2, 3, 4], false));
        let now = tokio::time::Instant::now();
        assert_eq!(state.ready(now, 0).len(), CONCURRENT);
        assert!(state.ready(now, CONCURRENT).is_empty());
        // Entry 2 has an unsettled predecessor, so its retries cannot retire
        // the prefix and stay observable.
        for attempt in 1..=MAX_RETRIES {
            state.record(2, RecoveryStatus::Fetching);
            state.retry_later(2);
            let entry = &state.entries[&2];
            assert!(matches!(
                entry.status,
                RecoveryStatus::Retryable { attempts, .. } if attempts == attempt
            ));
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
        state.begin("load", 1);
        let seqs: Vec<u64> = (1..=(IDENTITIES_PER_LOAD as u64 + 4)).collect();
        state.discover(&directory(*seqs.last().unwrap(), 1, &seqs, false));
        assert_eq!(state.entries.len(), IDENTITIES_PER_LOAD);
        assert!(state.behind);
        assert_eq!(state.observed_through, *seqs.last().unwrap());
        assert_eq!(state.settled_through, 0);
    }

    #[test]
    fn a_retry_deadline_is_armed_only_while_an_entry_waits() {
        let mut state = RecoveryState::default();
        state.begin("load", 1);
        state.discover(&directory(1, 1, &[1], false));
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
    fn a_different_load_resets_the_watermarks() {
        let mut state = RecoveryState::default();
        state.begin("first", 1);
        state.discover(&directory(3, 1, &[1, 2, 3], false));
        state.record(1, RecoveryStatus::Complete);
        assert_eq!(state.settled_through, 1);
        state.begin("second", 2);
        assert_eq!(state.settled_through, 0);
        assert!(state.entries.is_empty());
        assert_eq!(state.generation, 2);
        assert!(!state.accepts(1));
    }

    #[test]
    fn an_unavailable_directory_entry_is_displayed_before_it_can_settle() {
        let mut state = RecoveryState::default();
        state.begin("load", 1);
        let mut gone = directory(1, 1, &[1], false);
        gone.entries[0].availability = ResultAvailability::Unavailable;
        gone.entries[0].unavailable_reason = Some(UnavailableReason::Expired);
        state.discover(&gone);
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
        state.begin("l", 1);
        state.discover(&snapshot.completions);
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
        assert!(!held.overflowed);
        held.hold(snapshot("c", 1), 1024);
        assert!(!held.is_empty(), "only the newest two are kept");
        assert!(held.overflowed, "a drop forces a refresh after install");
        assert!(held.take("a").is_none(), "the oldest was dropped");
        assert!(held.take("c").is_some());
    }

    #[test]
    fn the_byte_budget_bounds_two_frame_ceilings() {
        let mut held = PreInstall::default();
        held.hold(snapshot("a", 1), PRE_INSTALL_BUDGET_BYTES);
        assert!(!held.is_empty());
        assert!(!held.overflowed);
        held.hold(snapshot("b", 1), 1);
        assert!(
            !held.is_empty(),
            "the byte budget evicts, not only the count"
        );
        assert!(held.overflowed);
        assert!(held.take("b").is_some());
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
