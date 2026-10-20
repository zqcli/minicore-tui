//! Applying the results of async operations and the Agent's events.

use serde_json::{Value, json};

use super::tasks::{self, History, Window};
use super::{App, LiveTurn, Overlay, Scroll, Session, SessionOp, find};
use crate::protocol::{
    self, AgentEvent, CompactStatus, CompactionResult, RpcError, SessionInfo, SessionState,
    TurnRef, TurnWaitResult,
};
use crate::transcript::{Block, ChangeSummary, NoticeLevel};

/// Pages read on open to reach the latest answer behind a compaction summary.
const BACKFILL_PAGES: u8 = 4;

impl App {
    pub(super) fn on_op(&mut self, op: SessionOp) {
        match op {
            SessionOp::Tail(result) => self.on_tail(result),
            SessionOp::Page(result) => self.on_page(result),
            SessionOp::TurnItems(loop_id, result) => {
                if let Some(items) = self.ok(result)
                    && let Some((at, grown)) = self.session_mut().reconcile_turn(&loop_id, &items)
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
                if let Some(context) = self.ok(result) {
                    self.session_mut().apply_context(context);
                }
            }
            SessionOp::Sent(text, result) => self.on_sent(text, result),
            SessionOp::Waited(result) => self.on_waited(result),
            SessionOp::Steered(result) => self.on_steered(result),
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
            SessionOp::Changes(loop_id, result) => self.on_changes(&loop_id, result),
            SessionOp::ToolRead(key, result) => {
                if let Some(read) = self.ok(result)
                    && let Some(card) = self.session_mut().transcript.find_tool_mut(&key)
                {
                    card.apply_read(&read);
                }
            }
            // A call the Agent no longer knows keeps its card as it is.
            SessionOp::ToolHydrated(key, read) => {
                if let Some(read) = read
                    && let Some(card) = self.session_mut().transcript.find_tool_mut(&key)
                {
                    card.apply_read(&read);
                }
            }
            SessionOp::ToolOutput(key, stream, result) => {
                // Pages for a call the view no longer shows are dropped.
                let Overlay::Tool(tool) = &mut self.overlay else {
                    return;
                };
                if tool.key != key {
                    return;
                }
                match result {
                    Ok(text) => tool.set_stream(&stream, text),
                    // The other streams still show; the view says what failed.
                    Err(error) => {
                        tool.set_stream(&stream, Default::default());
                        tool.status = error.message;
                    }
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
            SessionOp::ChangeFiles(scope, result) => self.on_change_files(scope, result),
            SessionOp::DiffFile(query, result) => self.on_diff_file(query, result),
        }
    }

    /// Makes the opened or created session resident and loads its tail.
    pub(super) fn adopt(&mut self, result: Result<SessionInfo, RpcError>, created: bool) {
        self.loading_session = false;
        let Some(info) = self.ok(result) else {
            return;
        };
        self.merge_catalog_info(info.clone());
        let epoch = self.session.as_ref().map_or(0, |old| old.epoch) + 1;
        self.session = Some(Session::new(info, epoch));
        self.layout.clear();
        self.scroll = Scroll::Follow;
        self.waiting_for_compaction = false;
        self.read_history(Window::Tail);
        self.refresh_branch();
        self.refresh_context();
        // A new session already carries its settings; an opened one takes them
        // through `session.update`, so a flag is never silently dropped.
        let launch = std::mem::take(&mut self.launch);
        if !created && (launch.model.is_some() || launch.reasoning.is_some()) {
            self.update_session(launch.fields(false));
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
        let idle = state.accepts_turn();
        let session = self.session_mut();
        let was_compacting = session.compaction.is_some();
        session.compaction = state.compaction;
        if was_compacting && session.compaction.is_none() {
            self.refresh_context();
        }
        if idle {
            self.waiting_for_compaction = false;
            self.flush_queue();
        } else if self.waiting_for_compaction {
            // Post-turn compaction still owns the session: keep asking until
            // it settles, or the queue never flushes.
            self.retry_when_idle();
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
        self.spawn(tasks::wait_turn(self.client.clone(), turn));
    }

    fn on_sent(&mut self, text: String, result: Result<TurnRef, RpcError>) {
        match result {
            Ok(turn) => {
                self.start_turn(turn);
                if let Some(title) = self.session.as_ref().and_then(Session::auto_title) {
                    self.rename(&title);
                }
            }
            // Post-turn compaction owns the session: the turn was refused, not
            // sent, so the text waits as a follow-up until the Agent is idle.
            Err(error) if error.is_busy() => {
                self.waiting_for_compaction = true;
                let session = self.session_mut();
                session.unsend_prompt(&text);
                session.push_follow_up(text);
                self.retry_when_idle();
            }
            // The text never reached the Agent; it returns to the draft.
            Err(error) => {
                self.session_mut().unsend_prompt(&text);
                self.restore_draft(&text);
                self.error(error);
            }
        }
    }

    fn retry_when_idle(&mut self) {
        let client = self.client.clone();
        let params = json!({ "session_id": self.session_mut().info.session_id });
        self.spawn(async move {
            tokio::time::sleep(super::BUSY_RETRY).await;
            SessionOp::State(client.call(protocol::METHOD_SESSION_STATE, &params).await)
        });
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
        // An automatic compaction has no response of its own: `session.context`
        // is where it is reported, and the footer's figure moved too. The turn
        // may also have switched branches.
        self.refresh_branch();
        self.refresh_context();
        self.flush_queue();
    }

    fn on_steered(&mut self, result: Result<Value, RpcError>) {
        match result {
            Ok(_) => {}
            Err(error) if error.code == protocol::TURN_NOT_FOUND => {
                self.session_mut().abandon_steers();
                self.notice(NoticeLevel::Info, "Turn already ended; queued as next");
            }
            Err(error) if error.code == protocol::STEER_QUEUE_FULL => {
                self.session_mut().demote_last_steer();
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
                self.notice(NoticeLevel::Info, "Compaction cancelled");
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

    /// Applies an Agent event: to the resident session, or to the catalog for
    /// another one.
    pub(super) fn on_agent_event(&mut self, event: AgentEvent) {
        let Some(meta) = event.meta() else {
            return;
        };
        let resident = self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == meta.session_id);
        if resident {
            let session = self.session_mut();
            let was_compacting = session.compaction.is_some();
            let usage = matches!(event, AgentEvent::RequestUsage { .. });
            let started = session.apply_event(event);
            let compaction_ended = was_compacting && session.compaction.is_none();
            if let Some(turn) = started {
                self.start_turn(turn);
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
        } else if let AgentEvent::SessionOpened { session, .. } = event {
            self.merge_catalog_info(*session);
        }
    }

    /// Updates the known sessions with `info`, adding it when new.
    pub(super) fn merge_catalog_info(&mut self, info: SessionInfo) {
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
