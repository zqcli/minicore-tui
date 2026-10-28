//! Applying the results of async operations and the Agent's events.

use super::tasks::{self, History, Window};
use super::{App, LiveTurn, NoticeLevel, Overlay, Queued, Scroll, Session, SessionOp, find};
use crate::protocol::stamp::unix_millis;
use crate::protocol::{
    self, AgentEvent, CompactStatus, CompactionResult, RpcError, SessionInfo, SessionState,
    SteerResult, TurnRef, TurnSendResult, TurnWaitResult,
};
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
            SessionOp::TurnItems(loop_id, result) => {
                if let Some((items, interrupted)) = self.ok(result)
                    && let Some((at, grown)) =
                        self.session_mut()
                            .reconcile_turn(&loop_id, &items, interrupted.as_ref())
                {
                    self.blocks_moved(at, grown);
                }
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
            SessionOp::Sent(text, result) => self.on_sent(text, result),
            SessionOp::Waited(result) => self.on_waited(result),
            SessionOp::Steered(loop_id, text, result) => self.on_steered(&loop_id, text, result),
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
        self.pump_queue();
    }

    /// Adopts a session and assigns its remote creator or opener's view without moving other devices.
    pub(super) fn adopt(&mut self, result: Result<SessionInfo, RpcError>, first: Option<String>) {
        let opening = self.remote.opening.take();
        let created = first.is_some();
        let id = result.as_ref().ok().map(|info| info.session_id.clone());
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
                if created {
                    self.session_mut().last_sender = Some(connection);
                }
            }
        }
        if let Some(previous) = previous {
            self.remote.notice_target = previous;
        }
    }

    fn adopt_session(&mut self, result: Result<SessionInfo, RpcError>, first: Option<String>) {
        self.loading_session = false;
        let Some(info) = self.ok(result) else {
            if let Some(text) = first {
                self.restore_draft(&text);
            }
            return;
        };
        self.merge_catalog_info(info.clone());
        // A second open of the same session: it is live already.
        if self.live(&info.session_id).is_some() {
            return;
        }
        self.epoch += 1;
        // What was typed while the draft's session was created belongs to it.
        let typed = first.is_some().then(|| self.composer.text().to_owned());
        self.show_session(Some(Session::new(info, self.epoch)));
        if let Some(typed) = typed {
            self.draft_text.clear();
            self.composer.set_text(&typed);
        }
        // A new session has no history to read, and the tail would clear the
        // prompt block of the first send.
        if first.is_none() {
            self.read_history(Window::Tail);
        }
        self.refresh_branch();
        self.refresh_context();
        // A new session already carries its settings; an opened one takes them
        // through `session.update`, so a flag is never silently dropped.
        let launch = std::mem::take(&mut self.launch);
        match first {
            Some(text) => self.submit_text(text, false),
            None if launch.model.is_some() || launch.reasoning.is_some() => {
                self.update_session(launch.fields(false));
            }
            None => {}
        }
    }

    fn on_tail(&mut self, result: Result<History, RpcError>) {
        if let Some(page) = self.ok(result) {
            let session = self.session_mut();
            session.install_tail(page);
            session.backfill = BACKFILL_PAGES;
            self.scroll = Scroll::Follow;
            // After the tail: a turn already running takes its place at the end.
            self.read_state();
            self.backfill();
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

    fn on_page(&mut self, result: Result<History, RpcError>) {
        let page = match result {
            Ok(page) => page,
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
        let (grown, cards) = self.session_mut().prepend_page(page);
        self.blocks_moved(0, grown);
        for key in cards {
            let hydrated = key.clone();
            self.read_tool(&key, move |read| {
                SessionOp::ToolHydrated(hydrated, read.ok())
            });
        }
        self.backfill();
    }

    /// Keeps the anchored view and the find matches on their blocks after
    /// `delta` blocks were added or removed in front of block `at`.
    fn blocks_moved(&mut self, at: usize, delta: isize) {
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

    fn on_state(&mut self, result: Result<SessionState, RpcError>) {
        let Some(state) = self.ok(result) else {
            return;
        };
        let session = self.session_mut();
        let was_compacting = session.compaction.is_some();
        session.compaction = state.compaction;
        if was_compacting && session.compaction.is_none() {
            self.refresh_context();
        }
        // A loop this process did not start has no `turn.wait` of its own.
        if let Some(active) = state.active_loop {
            let session_id = self.session_mut().info.session_id.clone();
            self.start_turn(TurnRef::new(session_id, active.loop_id));
        }
    }

    /// Makes `turn` the live turn and waits for it. The only place a
    /// `turn.wait` is registered, and only while no turn is live.
    pub(super) fn start_turn(&mut self, turn: TurnRef) {
        let session = self.session_mut();
        if session.live.is_some() {
            return;
        }
        let start = session
            .next_turn_start
            .take()
            .unwrap_or(session.transcript.len());
        let mut live = LiveTurn::new(turn.clone(), start);
        live.usage_base = session.usage.clone();
        session.live = Some(live);
        session.steers_paused = false;
        self.spawn(tasks::wait_turn(self.client.clone(), turn));
    }

    fn on_sent(&mut self, queued: Queued, result: Result<TurnSendResult, RpcError>) {
        let text = &queued.text;
        match result {
            Ok(sent) => {
                if let Some(at) = sent.accepted_at.as_deref().and_then(unix_millis) {
                    self.session_mut().stamp_prompt(text, at);
                }
                self.start_turn(sent.turn);
                if let Some(title) = self.session.as_ref().and_then(Session::auto_title) {
                    self.rename(&title);
                }
            }
            // The text never reached the Agent; it returns to the draft.
            Err(error) => {
                self.session_mut().unsend_prompt(text);
                self.restore_draft(text);
                self.error(error);
            }
        }
    }

    fn on_waited(&mut self, result: Result<TurnWaitResult, RpcError>) {
        let wait = match result {
            Ok(wait) => wait,
            Err(error) => {
                self.error(error);
                // Neither the wait nor the stored result answered: end the turn
                // as unknown rather than leave it running forever.
                let Some(live) = self.session.as_ref().and_then(|s| s.live.as_ref()) else {
                    return;
                };
                TurnWaitResult {
                    turn: live.turn.clone(),
                    outcome: crate::protocol::LoopOutcome::Unknown,
                    usage: None,
                    requests: None,
                    tool_rounds: None,
                }
            }
        };
        let Some(reconcile) = self.session_mut().end_turn(&wait) else {
            return;
        };
        self.stamp_activity();
        if self.background_op {
            self.background_turn_ended();
        }
        let TurnWaitResult { turn, .. } = wait;
        let client = self.client.clone();
        if reconcile {
            let (client, turn) = (client.clone(), turn.clone());
            self.spawn(async move {
                SessionOp::TurnItems(turn.loop_id.clone(), tasks::read_turn(client, turn).await)
            });
        }
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

    fn on_steered(&mut self, loop_id: &str, text: String, result: Result<SteerResult, RpcError>) {
        if !self
            .session_mut()
            .live
            .as_ref()
            .is_some_and(|live| live.turn.loop_id == loop_id)
        {
            return;
        }
        if result.is_err() {
            self.session_mut().refuse_steer(&text);
        }
        match result {
            Ok(steer) => {
                if let Some(at) = steer.accepted_at.as_deref().and_then(unix_millis) {
                    self.session_mut().stamp_steer(&text, at);
                }
            }
            Err(error) if error.code == protocol::TURN_NOT_FOUND => {
                self.notice(NoticeLevel::Info, "Turn already ended; queued as next");
            }
            Err(error) if error.code == protocol::STEER_QUEUE_FULL => {
                self.notice(
                    NoticeLevel::Error,
                    "Steer queue is full; queued for the next turn",
                );
            }
            Err(error) => self.error(error),
        }
    }

    fn on_compacted(&mut self, result: Result<CompactionResult, RpcError>) {
        self.session_mut().compaction = None;
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
    pub(super) fn on_agent_event(&mut self, event: AgentEvent) {
        let Some(meta) = event.meta() else {
            return;
        };
        let foreground = self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == meta.session_id);
        if !foreground && let Some(index) = self.live_index(&meta.session_id) {
            self.with_background(index, |app| app.on_agent_event(event));
        } else if foreground {
            let id = meta.session_id.clone();
            self.with_notice_for(&id, |app| app.apply_agent_event(event));
        } else if let AgentEvent::SessionOpened { session, .. } = event {
            self.merge_catalog_info(*session);
        }
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        let session = self.session_mut();
        let was_compacting = session.compaction.is_some();
        let usage = matches!(event, AgentEvent::RequestUsage { .. });
        let requested = matches!(event, AgentEvent::InteractionRequested { .. });
        let queue_changed = matches!(
            event,
            AgentEvent::SteerProgress { .. } | AgentEvent::TurnStarted { .. }
        );
        let started = session.apply_event(event);
        let compaction_ended = was_compacting && session.compaction.is_none();
        if let Some(turn) = started {
            self.start_turn(turn);
        }
        if requested {
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
        if queue_changed || compaction_ended {
            self.pump_queue();
        }
    }

    /// A finished turn is the session's latest activity, which the lists sort and
    /// show; the Agent's own stamp replaces this one, a few milliseconds apart,
    /// the next time the session's info is read.
    fn stamp_activity(&mut self) {
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
