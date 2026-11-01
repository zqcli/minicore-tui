//! Applying the results of async operations and the Agent's events.

use super::read::{History, HistoryRead, Window};
use super::tasks;
use super::{App, NoticeLevel, Overlay, Scroll, Session, SessionOp, find};
use crate::protocol::{
    self, AgentEvent, CompactStatus, CompactionResult, InputRecallResult, InputResumeResult,
    RpcError, SessionInfo, TurnWaitResult,
};
use crate::title::LONG_TURN;
use crate::transcript::{Block, ChangeSummary};

/// Pages read on open to reach the latest answer behind a compaction summary.
const BACKFILL_PAGES: u8 = 4;

impl App {
    pub(super) fn on_op(&mut self, epoch: u64, op: SessionOp) {
        match op {
            // These answer pickers and views of the foreground only.
            SessionOp::ItemCount(_)
            | SessionOp::Files(_)
            | SessionOp::ChangeFiles(_)
            | SessionOp::DiffFile(..)
                if !self.resident.is_current(epoch) => {}
            SessionOp::Tail(result) => self.on_tail(epoch, result),
            SessionOp::Page(result) => self.on_page(epoch, result),
            SessionOp::Recovered(outcome) => self.on_recovered(epoch, outcome),
            SessionOp::HistoryRecovered(through, result) => {
                self.on_history_recovered(epoch, through, result);
            }
            SessionOp::State(result) => self.on_state(epoch, result),
            SessionOp::Presentation(result) => {
                self.session_mut(epoch).presentation_in_flight = false;
                if let Some(presentation) = self.ok(result) {
                    self.session_mut(epoch).presentation = Some(presentation);
                }
            }
            // A failed status still leaves the footer's other figures to read.
            SessionOp::WorkspaceStatus(_) => self.refresh_presentation_for(epoch),
            SessionOp::Context(result) => {
                if let Some(context) = self.ok(result)
                    && let Some(reason) = self.session_mut(epoch).apply_context(context)
                {
                    self.notice(
                        NoticeLevel::Error,
                        format!("Automatic compaction failed: {reason}"),
                    );
                }
            }
            SessionOp::Resumed(result) => self.on_resumed(epoch, result),
            SessionOp::Recalled(result) => self.on_recalled(epoch, result),
            SessionOp::Waited(result) => self.on_waited(epoch, result),
            SessionOp::Operated(result) => {
                if let Some(outcome) = self.ok(result) {
                    self.apply_operation(Some(epoch), outcome);
                }
                self.refresh_control_for(epoch);
            }
            SessionOp::ItemCount(result) => {
                if let Some(total) = self.ok(result)
                    && let Overlay::SessionInfo { items } = &mut self.overlay
                {
                    *items = Some(total);
                }
            }
            SessionOp::Changes(loop_id, result) => self.on_changes(epoch, &loop_id, result),
            // A call the Agent no longer knows keeps its card as it is.
            SessionOp::ToolHydrated(key, read) => {
                if let Some(read) = read
                    && let Some(card) = self.session_mut(epoch).transcript.find_tool_mut(&key)
                {
                    card.apply_read(&read);
                }
            }
            SessionOp::Files(result) => {
                if let Some(found) = self.ok(result)
                    && let Overlay::Files { selected, paths } = &mut self.overlay
                {
                    *paths = found;
                    *selected = 0;
                }
            }
            SessionOp::ChangeFiles(result) => self.on_change_files(result),
            SessionOp::DiffFile(query, result) => self.on_diff_file(query, result),
        }
    }

    /// Adopts a session and assigns its remote creator or opener's view without moving other devices.
    pub(super) fn adopt(
        &mut self,
        result: Result<crate::protocol::OpenedSession, RpcError>,
        first: Option<String>,
    ) {
        let opening = self.remote.opening.take();
        let created = first.is_some();
        let previous = opening.filter(|_| created).map(|connection| {
            std::mem::replace(
                &mut self.remote.notice_target,
                crate::remote::NoticeTarget::Phone(connection),
            )
        });
        if let Some(id) = self.adopt_session(result, first) {
            self.remote.draft = None;
            if let Some(connection) = opening {
                self.set_phone_view(connection, crate::remote::PhoneView::Session(id));
            }
        }
        if let Some(previous) = previous {
            self.remote.notice_target = previous;
        }
    }

    fn adopt_session(
        &mut self,
        result: Result<crate::protocol::OpenedSession, RpcError>,
        first: Option<String>,
    ) -> Option<String> {
        self.loading_session = false;
        let typed = first.is_some().then(|| self.composer.text().to_owned());
        let installed =
            result.and_then(|adopted| self.install_session(adopted, true, first.is_none()));
        let Some(epoch) = self.ok(installed) else {
            if let Some(text) = first {
                self.first_submission = None;
                self.restore_draft(&text);
            }
            return None;
        };
        let id = self.resident.get(epoch).unwrap().info.session_id.clone();
        if let Some(typed) = typed {
            self.draft_text.clear();
            self.composer.set_text(&typed);
        }
        // A new session already carries its settings; an opened one takes them
        // through `session.update`, so a flag is never silently dropped.
        let launch = std::mem::take(&mut self.launch);
        match first {
            Some(text) => match self.first_submission.take() {
                Some(entry) => self.submit_entry(entry, false, None),
                None => self.submit_text(text, false),
            },
            None if launch.model.is_some() || launch.reasoning.is_some() => {
                self.update_session(launch.fields(false));
            }
            None => {}
        }
        Some(id)
    }

    pub(super) fn install_session(
        &mut self,
        adopted: crate::protocol::OpenedSession,
        select: bool,
        history: bool,
    ) -> Result<u64, RpcError> {
        let crate::protocol::OpenedSession {
            session: info,
            load,
            snapshot,
        } = adopted;
        if let Some(session) = self.live(&info.session_id) {
            if session.load != load {
                return Err(RpcError::new(
                    protocol::INVALID_PARAMS,
                    "stale_load",
                    "The open response belongs to another loaded instance",
                ));
            }
            let epoch = session.epoch;
            self.merge_catalog_info(info);
            if select && !self.resident.is_current(epoch) {
                self.select_session(Some(epoch));
            }
            self.apply_snapshot(snapshot);
            return Ok(epoch);
        }
        self.merge_catalog_info(info.clone());
        self.epoch = self.epoch.checked_add(1).expect("session epoch exhausted");
        let epoch = self.epoch;
        let id = info.session_id.clone();
        self.resident.insert(Session::new(info, epoch, load));
        if select {
            self.select_session(Some(epoch));
        }
        let snapshot = match self.pre_install.take(&id) {
            Some(held) if held.load == snapshot.load && held.revision > snapshot.revision => held,
            _ => snapshot,
        };
        self.apply_snapshot(snapshot);
        if history {
            self.read_history_for(epoch, Window::Tail);
        }
        self.refresh_control_for(epoch);
        self.refresh_branch_for(epoch);
        self.refresh_context_for(epoch);
        Ok(epoch)
    }

    fn on_tail(&mut self, epoch: u64, result: Result<HistoryRead, RpcError>) {
        let page = match result {
            Ok(HistoryRead::Complete(page)) => *page,
            Ok(HistoryRead::Partial(partial) | HistoryRead::Limited(_, partial)) => {
                self.notice(NoticeLevel::Error, partial.message());
                return;
            }
            Err(error) => {
                self.error(error);
                return;
            }
        };
        self.install_history(epoch, page);
        self.session_mut(epoch).backfill = BACKFILL_PAGES;
        self.refresh_control_for(epoch);
        self.backfill(epoch);
    }

    pub(super) fn install_history(&mut self, epoch: u64, page: History) {
        let source = match self.scroll_for(epoch) {
            Scroll::Anchor { block, offset } => self
                .session_mut(epoch)
                .transcript
                .history_source(block)
                .map(|source| (source, offset)),
            Scroll::Follow => None,
        };
        let mapping = self.session_mut(epoch).install_tail(page);
        if let Some((source, offset)) = source
            && let Some(block) = self
                .session_mut(epoch)
                .transcript
                .find_history_source(source)
        {
            self.set_scroll_for(epoch, Scroll::Anchor { block, offset });
            return;
        }
        if let Scroll::Anchor { block, offset } = self.scroll_for(epoch)
            && let Some((old, start, replaced)) =
                mapping.iter().find(|(range, _, _)| range.contains(&block))
        {
            self.set_scroll_for(
                epoch,
                Scroll::Anchor {
                    block: if *replaced {
                        *start
                    } else {
                        start + block - old.start
                    },
                    offset,
                },
            );
        }
    }

    /// Reads on behind the summary until the latest answer is loaded, so a
    /// session that opens on its compaction marker does not look empty.
    fn backfill(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        if session.transcript.last_answer().is_some() {
            session.backfill = 0;
        }
        if let Some(left) = session.backfill.checked_sub(1) {
            session.backfill = left;
            self.load_older_for(epoch);
        }
    }

    fn on_page(&mut self, epoch: u64, result: Result<HistoryRead, RpcError>) {
        let page = match result {
            Ok(HistoryRead::Complete(page)) => *page,
            Ok(HistoryRead::Partial(partial) | HistoryRead::Limited(_, partial)) => {
                let session = self.session_mut(epoch);
                session.loading_older = false;
                session.backfill = 0;
                self.notice(NoticeLevel::Error, partial.message());
                return;
            }
            Err(error) => {
                let session = self.session_mut(epoch);
                session.loading_older = false;
                session.backfill = 0;
                // The read already restarted once on a fresh pin.
                if error.is_stale_pin() {
                    self.notice(
                        NoticeLevel::Info,
                        "Could not load older history yet; try again in a moment",
                    );
                } else {
                    self.error(error);
                }
                return;
            }
        };
        if self
            .session_mut(epoch)
            .recovery
            .history_coverage
            .as_ref()
            .is_some_and(|known| !known.complete() && !known.matches(&page.pin))
        {
            self.session_mut(epoch).loading_older = false;
            self.session_mut(epoch).backfill = 0;
            self.notice(
                NoticeLevel::Info,
                "History changed; existing recovery coverage was kept",
            );
            return;
        }
        let covered = page.coverage();
        let overlaps = {
            let transcript = &self.session_mut(epoch).transcript;
            (0..transcript.len()).any(|at| {
                transcript
                    .history_source(at)
                    .is_some_and(|source| (page.from..page.end).contains(&source.item))
            })
        };
        let cards = if overlaps || self.session_mut(epoch).recovery.history_coverage.is_some() {
            let window = page.from..page.end;
            let canonical = page.canonical;
            self.session_mut(epoch).loading_older = false;
            self.install_history(epoch, page);
            let transcript = &self.session_mut(epoch).transcript;
            if canonical {
                transcript
                    .blocks()
                    .iter()
                    .enumerate()
                    .filter_map(|(at, block)| match block {
                        Block::Tool(call)
                            if transcript
                                .history_source(at)
                                .is_some_and(|source| window.contains(&source.item)) =>
                        {
                            Some(call.key.clone())
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            let (grown, cards) = self.session_mut(epoch).prepend_page(page);
            self.blocks_moved(epoch, 0, grown);
            cards
        };
        self.session_mut(epoch)
            .recovery
            .cover_history(covered, false);
        if self.session_mut(epoch).recovery.gap_through
            > self.session_mut(epoch).recovery.settled_through
            && self.session_mut(epoch).recovery.settle_history_gap()
        {
            self.notice(NoticeLevel::Error,"Saved history recovered; unsaved turns outside the completion catalog may be missing");
            self.recover_completions();
        }
        for key in cards {
            let hydrated = key.clone();
            self.read_tool(&key, move |read| {
                SessionOp::ToolHydrated(hydrated, read.ok())
            });
        }
        self.backfill(epoch);
    }

    /// Keeps a view anchored inside a replaced region on that region's start,
    /// and one anchored after it on its block, after `grown` more blocks.
    pub(super) fn moved_region(&mut self, epoch: u64, start: usize, end: usize, grown: isize) {
        if let Scroll::Anchor { block, offset } = self.scroll_for(epoch)
            && block >= start
            && block < end
        {
            self.set_scroll_for(
                epoch,
                Scroll::Anchor {
                    block: start,
                    offset,
                },
            );
        }
        self.blocks_moved(epoch, end, grown);
    }

    /// Keeps the anchored view and the find matches on their blocks after
    /// `delta` blocks were added or removed in front of block `at`.
    pub(super) fn blocks_moved(&mut self, epoch: u64, at: usize, delta: isize) {
        let moved = |block: usize| match block >= at {
            true => block.saturating_add_signed(delta),
            false => block,
        };
        if let Scroll::Anchor { block, offset } = self.scroll_for(epoch) {
            self.set_scroll_for(
                epoch,
                Scroll::Anchor {
                    block: moved(block),
                    offset,
                },
            );
        }
        if !self.resident.is_current(epoch) {
            return;
        }
        if let Overlay::Find {
            query,
            matches,
            current,
        } = &mut self.overlay
        {
            let shown = matches.get(*current).map(|found| moved(found.block));
            *matches = find::find_matches(
                self.resident
                    .get(epoch)
                    .map_or(&[][..], |session| session.transcript.blocks()),
                query,
                self.show_reasoning,
            );
            *current = shown
                .and_then(|block| matches.iter().position(|found| found.block == block))
                .unwrap_or(0);
        }
    }

    fn on_state(
        &mut self,
        epoch: u64,
        result: Result<crate::protocol::SessionStateResult, RpcError>,
    ) {
        self.session_mut(epoch).control_in_flight = false;
        let again = std::mem::take(&mut self.session_mut(epoch).control_due);
        let Some(state) = self.ok(result) else {
            // A failed read must not lose the change it was fetched for.
            self.session_mut(epoch).control_due = true;
            return;
        };
        if state.snapshot.session_id != self.session_mut(epoch).info.session_id {
            self.notice(
                NoticeLevel::Error,
                "The control response belongs to another session",
            );
            return;
        }
        let was_compacting = self
            .resident
            .get(epoch)
            .is_some_and(|session| session.compacting());
        self.apply_snapshot(state.snapshot);
        // The snapshot just read answers every hint that was outstanding when
        // it was asked for; only a hint that arrived since needs another read.
        if again {
            self.refresh_control_for(epoch);
        }
        let compacting = self
            .resident
            .get(epoch)
            .is_some_and(|session| session.compacting());
        if was_compacting && !compacting {
            self.refresh_context_for(epoch);
        }
        self.start_adopted_turn_for(epoch);
    }

    fn on_resumed(&mut self, epoch: u64, result: Result<InputResumeResult, RpcError>) {
        let Some(resumed) = self.ok(result) else {
            return;
        };
        if let Some(error) = resumed.start_error {
            self.notice(NoticeLevel::Error, error.message);
        }
        self.refresh_control_for(epoch);
    }

    /// Every queued message back as one draft, in the order they would have run.
    fn on_recalled(&mut self, epoch: u64, result: Result<InputRecallResult, RpcError>) {
        let Some(recalled) = self.ok(result) else {
            return;
        };
        self.refresh_control_for(epoch);
        if recalled.texts.is_empty() {
            return;
        }
        let text = recalled.texts.join("\n\n");
        // A session left meanwhile shows it when it comes back.
        match !self.resident.is_current(epoch) {
            true => self.session_mut(epoch).returned.push(text),
            false => self.restore_to_composer(&text),
        }
    }

    fn on_waited(&mut self, epoch: u64, result: Result<TurnWaitResult, RpcError>) {
        let wait = match result {
            Ok(wait) => wait,
            Err(error) => {
                self.error(error);
                self.refresh_control_for(epoch);
                return;
            }
        };
        let background = !self.resident.is_current(epoch);
        let mut completion_background = None;
        let mut busy = None;
        let session = self.session_mut(epoch);
        if let Some(live) = session.tracked_turn_mut(&wait.turn.loop_id) {
            completion_background = live.completion_background;
            busy = Some(live.started.elapsed());
            live.completion_background = Some(background);
        }
        if !self.session_mut(epoch).end_turn(&wait) {
            return;
        }
        let turn = wait.turn;
        self.refresh_control_for(epoch);
        if completion_background.is_none() {
            self.ring |= busy >= Some(LONG_TURN);
        }
        self.stamp_activity(epoch);
        if !self.resident.is_current(epoch) && completion_background != Some(false) {
            self.background_turn_ended(epoch, &turn.loop_id);
        }
        let client = self.client.clone();
        self.spawn_for(epoch, async move {
            let changes = tasks::turn_changes(client, turn.session_id, turn.loop_id.clone()).await;
            SessionOp::Changes(turn.loop_id, changes)
        });
        // A turn-start compaction has no response of its own: `session.context`
        // is where it is reported, and the footer's figure moved too. The turn
        // may also have switched branches.
        self.refresh_branch_for(epoch);
        self.refresh_context_for(epoch);
    }

    pub(super) fn on_compacted(&mut self, epoch: u64, result: CompactionResult) {
        match result.status {
            CompactStatus::Compacted => {
                self.session_mut(epoch).push_compaction(&result, None);
                self.refresh_presentation_for(epoch);
            }
            CompactStatus::Noop => self.notice(NoticeLevel::Info, "Nothing to compact yet"),
            CompactStatus::Failed if result.failure_kind.as_deref() == Some("cancelled") => {
                self.flash = Some(("Compaction cancelled".into(), tokio::time::Instant::now()));
            }
            CompactStatus::Failed => self.notice(
                NoticeLevel::Error,
                format!(
                    "Compaction failed: {}",
                    result.failure_kind.as_deref().unwrap_or("no progress")
                ),
            ),
            CompactStatus::UnknownWrite | CompactStatus::Unknown => self.notice(
                NoticeLevel::Error,
                "Compaction result is unverified; reopen the session to reconcile",
            ),
        }
    }

    fn on_changes(&mut self, epoch: u64, loop_id: &str, result: Result<ChangeSummary, RpcError>) {
        let Ok(summary) = result else {
            return;
        };
        let transcript = &mut self.session_mut(epoch).transcript;
        let end = transcript.blocks().iter().rposition(
            |block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id),
        );
        if let Some(end) = end
            && let Some(Block::TurnEnd { changes, .. }) = transcript.block_mut(end)
        {
            *changes = Some(summary);
        }
    }

    /// Applies an Agent event: to the live session it is for, or to the
    /// catalog for another one.
    pub(super) fn on_agent_event(&mut self, event: AgentEvent, bytes: usize) {
        let Some(meta) = event.meta() else {
            return;
        };
        let id = meta.session_id.clone();
        if event.body_session().is_some_and(|body| body != id) {
            return;
        }
        let bytes = event.retained_bytes(bytes);
        if self
            .live(&id)
            .map(|session| session.load.as_str())
            .is_some_and(|installed| meta.load.as_deref() != Some(installed))
        {
            return;
        }
        let extra = event
            .loop_id()
            .filter(|loop_id| {
                self.live(&id)
                    .is_some_and(|session| !session.knows_turn(loop_id))
            })
            .filter(|_| bytes <= super::session::EARLY_BYTES)
            .map_or(0, |_| bytes);
        while self
            .resident
            .iter()
            .map(|session| session.early_bytes)
            .sum::<usize>()
            + extra
            > 8 * 1024 * 1024
        {
            let Some(session) = self
                .resident
                .iter_mut()
                .find(|session| session.early_bytes > 0)
            else {
                break;
            };
            session.evict_early();
        }
        // A snapshot for a session this client has not opened yet is held for
        // the open that will install it, within the pre-install budget.
        if self.live(&id).is_none()
            && let AgentEvent::SessionSnapshot { snapshot, .. } = event
        {
            if self.loading_session
                && self
                    .opening_target
                    .as_ref()
                    .is_none_or(|(_, target)| target == &id)
            {
                self.hold_snapshot(*snapshot, bytes);
            }
            return;
        }
        if let Some(epoch) = self.live(&id).map(|session| session.epoch) {
            self.with_notice_for(&id, |app| app.apply_agent_event(epoch, event, bytes));
        } else if let AgentEvent::SessionOpened { session, .. } = event {
            self.merge_catalog_info(*session);
        }
    }

    fn apply_agent_event(&mut self, epoch: u64, event: AgentEvent, bytes: usize) {
        // The control snapshot is the one event that writes control state.
        if let AgentEvent::SessionSnapshot { snapshot, .. } = event {
            let was_compacting = self
                .resident
                .get(epoch)
                .is_some_and(|session| session.compacting());
            self.apply_snapshot(*snapshot);
            // The context report holds the finished compaction's marker, and
            // the footer's figure moved with it.
            if was_compacting
                && self
                    .resident
                    .get(epoch)
                    .is_some_and(|session| !session.compacting())
            {
                // The context report holds the finished compaction's marker,
                // and the footer's figure moved too.
                self.refresh_context_for(epoch);
                self.refresh_presentation_for(epoch);
            }
            return;
        }
        // These events only say control changed. Until the Agent pushes the
        // change itself, one coalesced read fetches the snapshot behind them.
        if matches!(
            event,
            AgentEvent::InteractionRequested { .. }
                | AgentEvent::InteractionResolved { .. }
                | AgentEvent::InputQueue { .. }
                | AgentEvent::SessionState { .. }
        ) {
            self.session_mut(epoch).control_due = true;
        }
        let seen = self.resident.get(epoch).unwrap().approval_seen.clone();
        let delivered = matches!(event, AgentEvent::InputDelivered { .. });
        let delivery_anchor = if delivered {
            match self.scroll_for(epoch) {
                Scroll::Anchor { block, offset } => self
                    .resident
                    .get(epoch)
                    .and_then(|session| session.transcript.revisions().nth(block))
                    .map(|revision| (revision, offset)),
                Scroll::Follow => None,
            }
        } else {
            None
        };
        let session = self.session_mut(epoch);
        let was_compacting = session.compacting();
        let usage = matches!(event, AgentEvent::RequestUsage { .. });
        // The interaction itself comes from the control snapshot, but the
        // announcement runs once per interaction whichever arrived first.
        let requested = session
            .interaction
            .as_ref()
            .map(|interaction| interaction.interaction_id.clone())
            .filter(|id| seen.as_ref() != Some(id));
        session.apply_body_event(event, bytes);
        let compaction_ended = was_compacting && session.compaction.is_none();
        let anchor = delivery_anchor.and_then(|(revision, offset)| {
            session
                .transcript
                .revisions()
                .position(|known| known == revision)
                .map(|block| Scroll::Anchor { block, offset })
        });
        if let Some(anchor) = anchor {
            self.set_scroll_for(epoch, anchor);
        }
        if requested.is_some() {
            self.session_mut(epoch).approval_seen = requested;
            self.on_approval_requested(epoch);
        }
        // The footer's context figure moves with each request and each
        // compaction, not only at the turn's end.
        if compaction_ended || (usage && self.session_mut(epoch).presentation_due()) {
            self.refresh_presentation_for(epoch);
        }
        // The context report holds the finished compaction's marker.
        if compaction_ended {
            self.refresh_context_for(epoch);
        }
        self.pull_control_for(epoch);
        #[cfg(feature = "e2e-observe")]
        if delivered {
            self.observe_checkpoint(false);
        }
    }

    /// Dates the session's latest activity now; the Agent's own stamp replaces it on the next info read.
    pub(super) fn stamp_activity(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        session.info.updated_at = protocol::stamp::rfc3339_millis(protocol::stamp::now_millis());
        let info = session.info.clone();
        self.merge_catalog_info(info);
    }

    /// Updates the known sessions with `info`, adding it when new.
    pub(super) fn merge_catalog_info(&mut self, info: SessionInfo) {
        self.catalog_generation += 1;
        match self
            .sessions
            .iter_mut()
            .find(|known| known.session_id == info.session_id)
        {
            Some(known) => *known = info,
            None => self.sessions.push(info),
        }
    }
}
