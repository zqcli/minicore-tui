//! Applying the results of async operations and the Agent's events.

use super::tasks::{self, HistoryRead, Window};
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
    pub(super) fn on_op(&mut self, op: SessionOp) {
        match op {
            // These answer pickers and views of the foreground only.
            SessionOp::ItemCount(_)
            | SessionOp::Files(_)
            | SessionOp::ChangeFiles(_)
            | SessionOp::DiffFile(..)
                if self.background_op => {}
            SessionOp::Tail(result) => self.on_tail(result),
            SessionOp::Page(result) => self.on_page(result),
            SessionOp::Recovered(outcome) => self.on_recovered(outcome),
            SessionOp::HistoryRecovered(generation, through, result) => {
                if !self.session_mut().recovery.accepts(generation) {
                    return;
                }
                self.session_mut().recovery_tasks.finish(None);
                let superseded = through != self.session_mut().recovery.gap_through;
                let (page, limit) = match result {
                    Ok(HistoryRead::Complete(page)) => (Some(page), None),
                    Ok(HistoryRead::Limited(page, limit)) => (Some(page), Some(limit)),
                    Ok(HistoryRead::Partial(partial)) => {
                        self.session_mut().recovery.history_status =
                            Some(super::recovery::RecoveryStatus::Exhausted);
                        self.notice(NoticeLevel::Error, partial.message());
                        (None, None)
                    }
                    Err(error) => {
                        self.session_mut().recovery.retry_history();
                        if self.session_mut().recovery.history_status
                            == Some(super::recovery::RecoveryStatus::Exhausted)
                        {
                            self.error(error);
                        }
                        (None, None)
                    }
                };
                if let Some(page) = page {
                    let covered = page.coverage();
                    let prepend = self
                        .session_mut()
                        .recovery
                        .history_coverage
                        .as_ref()
                        .is_some_and(|known| known.matches(&page.pin) && page.end == known.from);
                    if prepend {
                        let (grown, _) = self.session_mut().prepend_page(*page);
                        self.blocks_moved(0, grown);
                    } else {
                        self.install_history(*page);
                    }
                    self.session_mut().recovery.cover_history(covered, true);
                    if superseded {
                        self.session_mut().recovery.history_status =
                            Some(super::recovery::RecoveryStatus::Pending);
                    } else if !self.session_mut().recovery.settle_history_gap() {
                        self.session_mut().recovery.history_status =
                            Some(super::recovery::RecoveryStatus::Exhausted);
                        self.notice(
                            NoticeLevel::Info,
                            limit.map_or_else(
                                || "More history remains to recover; /recover to continue".into(),
                                |limit| limit.message(),
                            ),
                        );
                    } else {
                        let gap = self.session_mut().recovery.gap_through > 0;
                        if gap {
                            self.notice(NoticeLevel::Error, "Saved history recovered; unsaved turns outside the completion catalog may be missing");
                        }
                        self.session_mut().recovery.retry_failed();
                        self.session_mut().ordering_gap = false;
                    }
                }
                self.recover_completions();
            }
            SessionOp::State(result) => self.on_state(result),
            SessionOp::Presentation(result) => {
                self.session_mut().presentation_in_flight = false;
                if let Some(presentation) = self.ok(result) {
                    self.session_mut().presentation = Some(presentation);
                }
            }
            // A failed status still leaves the footer's other figures to read.
            SessionOp::WorkspaceStatus(_) => self.refresh_presentation(),
            SessionOp::Context(result) => {
                if let Some(context) = self.ok(result)
                    && let Some(reason) = self.session_mut().apply_context(context)
                {
                    self.notice(
                        NoticeLevel::Error,
                        format!("Automatic compaction failed: {reason}"),
                    );
                }
            }
            SessionOp::Resumed(result) => self.on_resumed(result),
            SessionOp::Recalled(result) => self.on_recalled(result),
            SessionOp::Queue(result) => {
                self.ok(result);
                self.refresh_control();
            }
            SessionOp::Waited(result) => self.on_waited(result),
            SessionOp::Done(result) => {
                self.ok(result);
            }
            SessionOp::Compacted(result) => self.on_compacted(result),
            SessionOp::Info(result) => {
                if let Some(info) = self.ok(result) {
                    self.session_mut().info = info.clone();
                    self.merge_catalog_info(info);
                }
            }
            SessionOp::ItemCount(result) => {
                if let Some(total) = self.ok(result)
                    && let Overlay::SessionInfo { items } = &mut self.overlay
                {
                    *items = Some(total);
                }
            }
            SessionOp::Changes(loop_id, result) => self.on_changes(&loop_id, result),
            // A call the Agent no longer knows keeps its card as it is.
            SessionOp::ToolHydrated(key, read) => {
                if let Some(read) = read
                    && let Some(card) = self.session_mut().transcript.find_tool_mut(&key)
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
        result: Result<tasks::Adopted, RpcError>,
        first: Option<String>,
    ) {
        let opening = self.remote.opening.take();
        let created = first.is_some();
        let id = result
            .as_ref()
            .ok()
            .map(|adopted| adopted.info.session_id.clone());
        let previous = opening.filter(|_| created).map(|connection| {
            std::mem::replace(
                &mut self.remote.notice_target,
                crate::remote::NoticeTarget::Phone(connection),
            )
        });
        self.adopt_session(result, first);
        if let Some(id) = id {
            self.remote.draft = None;
            if let Some(connection) = opening {
                self.set_phone_view(connection, crate::remote::PhoneView::Session(id));
            }
        }
        if let Some(previous) = previous {
            self.remote.notice_target = previous;
        }
    }

    fn adopt_session(&mut self, result: Result<tasks::Adopted, RpcError>, first: Option<String>) {
        self.loading_session = false;
        let Some(tasks::Adopted {
            info,
            load,
            snapshot,
        }) = self.ok(result)
        else {
            if let Some(text) = first {
                self.first_submission = None;
                self.restore_draft(&text);
            }
            return;
        };
        self.merge_catalog_info(info.clone());
        // A second open of the same session: it is live already.
        if self.live(&info.session_id).is_some() {
            return;
        }
        self.epoch = self.epoch.checked_add(1).expect("session epoch exhausted");
        // What was typed while the draft's session was created belongs to it.
        let typed = first.is_some().then(|| self.composer.text().to_owned());
        let session_id = info.session_id.clone();
        self.show_session(Some(Session::new(info, self.epoch)));
        // The instance identity comes from the open itself; its snapshot then
        // supplies the current control state.
        self.install_load(load);
        // The open's own snapshot is newest; a held early one is the fallback,
        // and a dropped hold is why the refresh below is unconditional.
        // The open's own snapshot is the same observation the Agent would have
        // pushed, so a held one that is newer is the better state to install.
        let held = self.pre_install.take(&session_id);
        let snapshot = match (snapshot, held) {
            (Some(own), Some(held)) if held.load == own.load && held.revision > own.revision => {
                Some(held)
            }
            (own, held) => own.or(held),
        };
        if let Some(snapshot) = snapshot {
            self.apply_snapshot(snapshot);
        }
        if let Some(typed) = typed {
            self.draft_text.clear();
            self.composer.set_text(&typed);
        }
        // A new session has no history to read.
        if first.is_none() {
            self.read_history(Window::Tail);
        }
        self.refresh_control();
        self.refresh_branch();
        self.refresh_context();
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
    }

    fn on_tail(&mut self, result: Result<HistoryRead, RpcError>) {
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
        self.install_history(page);
        self.session_mut().backfill = BACKFILL_PAGES;
        self.read_state();
        self.backfill();
    }

    fn install_history(&mut self, page: tasks::History) {
        let mapping = self.session_mut().install_tail(page);
        if let Scroll::Anchor { block, offset } = self.scroll
            && let Some((old, start, replaced)) =
                mapping.iter().find(|(range, _, _)| range.contains(&block))
        {
            self.scroll = Scroll::Anchor {
                block: if *replaced {
                    *start
                } else {
                    start + block - old.start
                },
                offset,
            };
        }
    }

    /// Reads on behind the summary until the latest answer is loaded, so a
    /// session that opens on its compaction marker does not look empty.
    fn backfill(&mut self) {
        let session = self.session_mut();
        if session.transcript.last_answer().is_some() {
            session.backfill = 0;
        }
        if let Some(left) = session.backfill.checked_sub(1) {
            session.backfill = left;
            self.load_older();
        }
    }

    fn on_page(&mut self, result: Result<HistoryRead, RpcError>) {
        let page = match result {
            Ok(HistoryRead::Complete(page)) => *page,
            Ok(HistoryRead::Partial(partial) | HistoryRead::Limited(_, partial)) => {
                let session = self.session_mut();
                session.loading_older = false;
                session.backfill = 0;
                self.notice(NoticeLevel::Error, partial.message());
                return;
            }
            Err(error) => {
                let session = self.session_mut();
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
            .session_mut()
            .recovery
            .history_coverage
            .as_ref()
            .is_some_and(|known| !known.complete() && !known.matches(&page.pin))
        {
            self.session_mut().loading_older = false;
            self.session_mut().backfill = 0;
            self.notice(
                NoticeLevel::Info,
                "History changed; existing recovery coverage was kept",
            );
            return;
        }
        let covered = page.coverage();
        let (grown, cards) = self.session_mut().prepend_page(page);
        self.blocks_moved(0, grown);
        self.session_mut().recovery.cover_history(covered, false);
        if self.session_mut().recovery.gap_through > self.session_mut().recovery.settled_through
            && self.session_mut().recovery.settle_history_gap()
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
        self.backfill();
    }

    /// Keeps a view anchored inside a replaced region on that region's start,
    /// and one anchored after it on its block, after `grown` more blocks.
    pub(super) fn moved_region(&mut self, start: usize, end: usize, grown: isize) {
        if let Scroll::Anchor { block, offset } = self.scroll
            && block >= start
            && block < end
        {
            self.scroll = Scroll::Anchor {
                block: start,
                offset,
            };
        }
        self.blocks_moved(end, grown);
    }

    /// Keeps the anchored view and the find matches on their blocks after
    /// `delta` blocks were added or removed in front of block `at`.
    pub(super) fn blocks_moved(&mut self, at: usize, delta: isize) {
        let moved = |block: usize| match block >= at {
            true => block.saturating_add_signed(delta),
            false => block,
        };
        if let Scroll::Anchor { block, offset } = self.scroll {
            self.scroll = Scroll::Anchor {
                block: moved(block),
                offset,
            };
        }
        if self.background_op {
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
                self.session
                    .as_ref()
                    .map_or(&[][..], |session| session.transcript.blocks()),
                query,
                self.show_reasoning,
            );
            *current = shown
                .and_then(|block| matches.iter().position(|found| found.block == block))
                .unwrap_or(0);
        }
    }

    fn on_state(&mut self, result: Result<crate::protocol::SessionStateResult, RpcError>) {
        self.session_mut().control_in_flight = false;
        let again = std::mem::take(&mut self.session_mut().control_due);
        let Some(state) = self.ok(result) else {
            // A failed read must not lose the change it was fetched for.
            self.session_mut().control_due = true;
            return;
        };
        if state.snapshot.session_id != self.session_mut().info.session_id {
            self.notice(
                NoticeLevel::Error,
                "The control response belongs to another session",
            );
            return;
        }
        let was_compacting = self
            .session
            .as_ref()
            .is_some_and(|session| session.compacting());
        self.apply_snapshot(state.snapshot);
        // The snapshot just read answers every hint that was outstanding when
        // it was asked for; only a hint that arrived since needs another read.
        if again {
            self.refresh_control();
        }
        let compacting = self
            .session
            .as_ref()
            .is_some_and(|session| session.compacting());
        if was_compacting && !compacting {
            self.refresh_context();
        }
        self.start_adopted_turn();
    }

    fn on_resumed(&mut self, result: Result<InputResumeResult, RpcError>) {
        let Some(resumed) = self.ok(result) else {
            return;
        };
        if let Some(error) = resumed.start_error {
            self.notice(NoticeLevel::Error, error.message);
        }
        self.refresh_control();
    }

    /// Every queued message back as one draft, in the order they would have run.
    fn on_recalled(&mut self, result: Result<InputRecallResult, RpcError>) {
        let Some(recalled) = self.ok(result) else {
            return;
        };
        self.refresh_control();
        if recalled.texts.is_empty() {
            return;
        }
        let text = recalled.texts.join("\n\n");
        // A session left meanwhile shows it when it comes back.
        match self.background_op {
            true => self.session_mut().returned.push(text),
            false => self.restore_to_composer(&text),
        }
    }

    fn on_waited(&mut self, result: Result<TurnWaitResult, RpcError>) {
        let wait = match result {
            Ok(wait) => wait,
            Err(error) => {
                self.error(error);
                self.refresh_control();
                return;
            }
        };
        let background = self.background_op;
        let mut completion_background = None;
        let mut busy = None;
        let session = self.session_mut();
        if let Some(live) = session
            .live
            .iter_mut()
            .chain(&mut session.ended_live)
            .find(|live| live.turn.loop_id == wait.turn.loop_id)
        {
            completion_background = live.completion_background;
            busy = Some(live.started.elapsed());
            live.completion_background = Some(background);
        }
        let Some(_) = self.session_mut().end_turn(&wait) else {
            return;
        };
        let turn = wait.turn;
        self.refresh_control();
        if completion_background.is_none() {
            self.ring |= busy >= Some(LONG_TURN);
        }
        self.stamp_activity();
        if self.background_op && completion_background != Some(false) {
            self.background_turn_ended(&turn.loop_id);
        }
        let client = self.client.clone();
        self.spawn(async move {
            let changes = tasks::turn_changes(client, turn.session_id, turn.loop_id.clone()).await;
            SessionOp::Changes(turn.loop_id, changes)
        });
        // A turn-start compaction has no response of its own: `session.context`
        // is where it is reported, and the footer's figure moved too. The turn
        // may also have switched branches.
        self.refresh_branch();
        self.refresh_context();
    }

    fn on_compacted(&mut self, result: Result<CompactionResult, RpcError>) {
        self.refresh_control();
        let Some(result) = self.ok(result) else {
            return;
        };
        match result.status {
            CompactStatus::Compacted => {
                self.session_mut().push_compaction(&result, None);
                self.refresh_presentation();
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

    fn on_changes(&mut self, loop_id: &str, result: Result<ChangeSummary, RpcError>) {
        let Ok(summary) = result else {
            return;
        };
        let transcript = &mut self.session_mut().transcript;
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
        if let AgentEvent::SessionSnapshot { snapshot, .. } = &event
            && (snapshot.session_id != id || meta.load.as_deref() != Some(snapshot.load.as_str()))
        {
            return;
        }
        if self
            .live(&id)
            .and_then(|session| session.load.as_deref())
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
            .session
            .iter()
            .chain(&self.background)
            .map(|session| session.early_bytes)
            .sum::<usize>()
            + extra
            > 8 * 1024 * 1024
        {
            let Some(session) = self
                .session
                .iter_mut()
                .chain(&mut self.background)
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
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == id)
        {
            self.with_notice_for(&id, |app| app.apply_agent_event(event, bytes));
        } else if let Some(index) = self.live_index(&id) {
            self.with_background(index, |app| app.apply_agent_event(event, bytes));
        } else if let AgentEvent::SessionOpened { session, .. } = event {
            self.merge_catalog_info(*session);
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent, bytes: usize) {
        // The control snapshot is the one event that writes control state.
        if let AgentEvent::SessionSnapshot { snapshot, .. } = event {
            let was_compacting = self
                .session
                .as_ref()
                .is_some_and(|session| session.compacting());
            self.apply_snapshot(*snapshot);
            // The context report holds the finished compaction's marker, and
            // the footer's figure moved with it.
            if was_compacting
                && self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.compacting())
            {
                // The context report holds the finished compaction's marker,
                // and the footer's figure moved too.
                self.refresh_context();
                self.refresh_presentation();
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
            self.session_mut().control_due = true;
        }
        let seen = self.approval_seen.clone();
        let delivered = matches!(event, AgentEvent::InputDelivered { .. });
        let delivery_anchor = if delivered {
            match self.scroll {
                Scroll::Anchor { block, offset } => self
                    .session
                    .as_ref()
                    .and_then(|session| session.transcript.revisions().get(block))
                    .map(|revision| (*revision, offset)),
                Scroll::Follow => None,
            }
        } else {
            None
        };
        let session = self.session_mut();
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
        if let Some((revision, offset)) = delivery_anchor
            && let Some(block) = session
                .transcript
                .revisions()
                .iter()
                .position(|known| *known == revision)
        {
            self.scroll = Scroll::Anchor { block, offset };
        }
        if requested.is_some() {
            self.approval_seen = requested;
            self.on_approval_requested();
        }
        // The footer's context figure moves with each request and each
        // compaction, not only at the turn's end.
        if compaction_ended || (usage && self.session_mut().presentation_due()) {
            self.refresh_presentation();
        }
        // The context report holds the finished compaction's marker.
        if compaction_ended {
            self.refresh_context();
        }
        self.pull_control();
        #[cfg(feature = "e2e-observe")]
        if delivered {
            self.observe_checkpoint(false);
        }
    }

    /// Dates the session's latest activity now; the Agent's own stamp replaces it on the next info read.
    pub(super) fn stamp_activity(&mut self) {
        let session = self.session_mut();
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
