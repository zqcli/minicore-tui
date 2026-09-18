//! The turn state machine: submission, the bounded steer queue, deferred
//! `turn.wait`/`turn.result` slots, cancellation, manual compaction and the
//! local clipboard/stderr jobs (spec §5.3, §6).
//!
//! These are `pub(super)` methods on `App`; the owner, the event router and
//! navigation stay in `app.rs`.

use super::*;

impl App {
    /// Starts one owned clipboard capture for already-sanitized
    /// presentation text. The command carries the precise session/revision
    /// identity so a stale completion cannot decorate a newer selection
    /// (spec §5.5). The job runs off the UI loop; `App::update` applies the
    /// result.
    pub(crate) fn capture_copy(&mut self, text: String) -> AppCommand {
        self.selection_revision = self.selection_revision.wrapping_add(1);
        let session_id = self.sessions.active.clone().unwrap_or_default();
        AppCommand::CopySelection(crate::command::ClipboardText::new(
            text,
            session_id,
            self.selection_revision,
        ))
    }

    pub(super) fn cancel_active_turn(&mut self) -> Vec<AppCommand> {
        let Some(active) = self.sessions.active.clone() else {
            self.notice(NoticeLevel::Warning, "no active turn to cancel");
            return Vec::new();
        };
        self.cancel_turn(&active)
    }

    /// Routes cancellation to the exact operation currently owned by the
    /// session: manual compact, deferred preparation, or a concrete TurnRef.
    pub fn request_cancel(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        self.cancel_turn(session_id)
    }

    /// Submitting the composer: slash lines are parsed locally; plain text
    /// goes to the active session and clears the composer. Nothing is ever
    /// silently swallowed; a missing agent or session gets a notice.
    pub fn submit_composer(&mut self) -> Vec<AppCommand> {
        self.editor_selection = None;
        let text = self.composer.content().trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        if text.len() > MAX_COMPOSER_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
            );
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if is_slash_command(&text) {
            let commands = self.run_command(&text);
            self.composer.clear();
            return commands;
        }
        let Some(active) = self.sessions.active.clone() else {
            self.notice(
                NoticeLevel::Info,
                "Open or create a session first — /new or Ctrl+R.",
            );
            return Vec::new();
        };
        let is_running = self
            .sessions
            .known
            .get(&active)
            .map(|v| v.is_running())
            .unwrap_or(false);
        let status = self
            .sessions
            .known
            .get(&active)
            .and_then(|view| view.state.as_ref().map(|state| state.status));
        let is_blocked = status == Some(SessionStatusWire::Blocked);
        let is_waiting = status == Some(SessionStatusWire::WaitingForInput)
            || self
                .sessions
                .known
                .get(&active)
                .and_then(|view| view.live.as_ref())
                .is_some_and(|live| live.waiting);

        if is_blocked {
            self.notice(
                NoticeLevel::Error,
                "session is blocked; resolve or reset before submitting",
            );
            return Vec::new();
        }

        if is_running {
            let editor_revision = self.composer.editor_revision();
            let mut commands = self.steer_turn_with_revision(&active, text, Some(editor_revision));
            // Enter path issues the single in-flight steer via the same central
            // FIFO advance used by every other event (one RPC per session).
            commands.extend(self.advance_steer_queues());
            commands
        } else if is_waiting || status == Some(SessionStatusWire::Finishing) {
            self.notice(
                NoticeLevel::Warning,
                "session is not accepting input right now",
            );
            Vec::new()
        } else {
            let submitted_text = text.clone();
            let commands = self.submit_turn(active.clone(), text);
            if !commands.is_empty() {
                self.composer.submit_pushed(&submitted_text);
                self.composer.clear();
                let revision = self.composer.editor_revision();
                let local_submission = self
                    .sessions
                    .known
                    .get(&active)
                    .and_then(|view| view.live.as_ref())
                    .map(|live| live.local_submission);
                if let Some(local_submission) = local_submission {
                    if let Some(submission) = self.submissions.get_mut(&local_submission) {
                        submission.editor_revision = revision;
                    }
                }
            }
            commands
        }
    }

    pub fn steer_turn(&mut self, session_id: &SessionId, text: String) -> Vec<AppCommand> {
        self.steer_turn_with_revision(session_id, text, None)
    }

    pub(super) fn steer_turn_with_revision(
        &mut self,
        session_id: &SessionId,
        text: String,
        editor_revision: Option<u64>,
    ) -> Vec<AppCommand> {
        if self.reload.is_some() {
            if self.composer.is_empty() {
                self.composer.set_text(&text);
            }
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if !self.can_send_requests() {
            return Vec::new();
        }
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        if text.len() > MAX_COMPOSER_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
            );
            return Vec::new();
        }
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.close_verification_unknown || view.steer_state_unconfirmed {
            self.notice(
                NoticeLevel::Warning,
                "session state/history is not reconciled; cannot steer",
            );
            return Vec::new();
        }
        if view
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .is_none()
        {
            self.notice(NoticeLevel::Warning, "session is not running; cannot steer");
            return Vec::new();
        }
        if view.closing {
            self.notice(NoticeLevel::Warning, "session is closing; cannot steer");
            return Vec::new();
        }
        if view.is_blocked() {
            self.notice(NoticeLevel::Warning, "session is blocked; cannot steer");
            return Vec::new();
        }
        if view.is_preparing() {
            self.notice(
                NoticeLevel::Info,
                "session is preparing context; cannot steer",
            );
            return Vec::new();
        }
        if !view.is_running() {
            self.notice(NoticeLevel::Warning, "session is not running; cannot steer");
            return Vec::new();
        }
        // FIFO admission: a bounded per-session queue. No silent Sending-guard
        // drop; a full queue keeps the editor text and pauses instead.
        let queued_bytes = view
            .steer_queue
            .iter()
            .map(|item| item.text.len())
            .sum::<usize>();
        if view.steer_queue.len() >= MAX_STEER_QUEUE_LEN
            || queued_bytes.saturating_add(text.len()) > MAX_STEER_QUEUE_BYTES
        {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.steer_queue_paused = true;
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "steer queue is full ({} messages / {} bytes); keep the message and retry after the turn",
                    MAX_STEER_QUEUE_LEN,
                    MAX_STEER_QUEUE_BYTES
                ),
            );
            return Vec::new();
        }
        self.next_steer_id = self
            .next_steer_id
            .checked_add(1)
            .expect("steering ids exhausted");
        let steer_id = self.next_steer_id;
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            // An explicit user admission is deliberate: it re-opens the gate
            // (the pause only blocks AUTOMATIC advance after cancel/failure).
            view.steer_queue_paused = false;
            view.steer_queue.push(crate::state::turn::SteerQueueItem {
                local_id: steer_id,
                text: text.clone(),
                state: crate::state::turn::SteerQueueState::Unsent,
                editor_revision,
                handoff: false,
            });
        }
        // A late ACK must never clear new editor content: clear only when the
        // submitting revision and text still match the composer exactly.
        let composer_text = self.composer.content().trim().to_owned();
        if self.sessions.active.as_ref() == Some(session_id)
            && editor_revision.is_some_and(|revision| revision == self.composer.editor_revision())
            && composer_text == text
        {
            self.composer.submit_pushed(&composer_text);
            self.composer.clear();
        }
        // The RPC is issued by the central FIFO advance (at most one in
        // flight per session), not inline.
        Vec::new()
    }

    /// Central FIFO queue advance for every session. At most ONE steer RPC (or
    /// accepted-but-unconfirmed item) per session until a receipt proves the
    /// previous one was issued into a request. A loop that sealed before the
    /// next unsent steer could be sent falls back to a fresh turn once the
    /// previous loop completed, persisted, and settled.
    pub(super) fn advance_steer_queues(&mut self) -> Vec<AppCommand> {
        // Never issue a NEW steer (or fresh-turn) RPC once the connection is
        // failed, shutting down, or staging a configuration reload.
        if self.reload.is_some() || !self.can_send_requests() {
            return Vec::new();
        }
        let session_ids: Vec<SessionId> = self.sessions.known.keys().cloned().collect();
        let mut commands = Vec::new();
        for session_id in session_ids {
            commands.extend(self.advance_steer_queue_for(&session_id));
        }
        commands
    }

    pub(super) fn advance_steer_queue_for(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.close_verification_unknown {
            return Vec::new();
        }
        if view.steer_queue_paused || view.steer_queue.is_empty() || view.closing {
            return Vec::new();
        }
        if view
            .steer_queue
            .iter()
            .any(|item| item.state == SteerQueueState::Unconfirmed)
        {
            // An uncertain outcome blocks the WHOLE queue advance: never
            // auto-resend it, and never let a newer unsent item skip past it.
            return Vec::new();
        }
        let running = view.is_running();
        let loop_id = view
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|r| r.loop_id.clone());
        if running {
            if view.steer_state_unconfirmed {
                return Vec::new();
            }
            let Some(loop_id) = loop_id else {
                return Vec::new();
            };
            let has_inflight = view.live.as_ref().is_some_and(|live| {
                live.pending_steers.iter().any(|steer| {
                    matches!(
                        steer.state,
                        PendingSteerState::Sending
                            | PendingSteerState::Queued
                            | PendingSteerState::Unconfirmed
                    )
                })
            }) || view.steer_queue.iter().any(|item| item.handoff);
            let Some(head) = view.steer_queue.iter().find(|item| {
                item.state == crate::state::turn::SteerQueueState::Unsent && !item.handoff
            }) else {
                return Vec::new();
            };
            if has_inflight {
                return Vec::new();
            }
            let head = head.clone();
            // Pop from the local queue; the RPC lifecycle (Sending/Accepted,
            // then receipts) takes over from `pending_steers`.
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.steer_queue
                    .retain(|item| item.local_id != head.local_id);
                if let Some(live) = view.live.as_mut() {
                    live.pending_steers.push(PendingSteer {
                        local_id: head.local_id,
                        text: head.text.clone(),
                        state: PendingSteerState::Sending,
                        accepted_at: None,
                        steer_index: None,
                    });
                }
            }
            let req_loop_id = loop_id.clone();
            let req_text = head.text.clone();
            let req_revision = head.editor_revision;
            vec![self.request(
                RequestKind::SteerTurn {
                    session_id: session_id.clone(),
                    loop_id: loop_id.clone(),
                    steer_id: head.local_id,
                    text: head.text.clone(),
                    editor_revision: req_revision,
                },
                |id| {
                    OutgoingRequest::steer_turn(
                        id,
                        &TurnRef {
                            session_id: session_id.clone(),
                            loop_id: req_loop_id.clone(),
                        },
                        &req_text,
                    )
                },
            )]
        } else {
            // A sealed loop cannot turn an unsent steer into a new prompt.
            // Keep it bounded and paused until the user deliberately
            // withdraws it into the editor.
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                if view
                    .steer_queue
                    .iter()
                    .any(|item| item.state == SteerQueueState::Unsent)
                {
                    view.steer_queue_paused = true;
                }
            }
            Vec::new()
        }
    }

    /// Withdraws the NEXT UNSENT queue item into the empty editor (only when
    /// the composer is empty; accepted/in-flight messages cannot be withdrawn
    /// because they are already owned by the loop). Returns true when taken.
    pub fn retrieve_next_queued_steer(&mut self) -> bool {
        if !self.composer.is_empty() {
            return false;
        }
        let Some(session_id) = self.sessions.active.clone() else {
            return false;
        };
        let Some(head) = self.sessions.known.get(&session_id).and_then(|view| {
            view.steer_queue
                .iter()
                .find(|item| item.state == SteerQueueState::Unsent)
                .cloned()
        }) else {
            return false;
        };
        let text = head.text.clone();
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.steer_queue
                .retain(|item| item.local_id != head.local_id);
        }
        self.composer.set_text(&text);
        true
    }

    pub(super) fn start_manual_compact(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        let Some(session_id) = self.sessions.active.clone() else {
            self.notice(NoticeLevel::Info, "no active session to compact");
            return Vec::new();
        };
        let allowed = self.sessions.known.get(&session_id).is_some_and(|view| {
            view.info.loaded
                && !view.closing
                && !view.is_blocked()
                && !view.is_preparing()
                && view.live.is_none()
                && view.unsaved_loop.is_none()
                && !view.event_gap
                && view.transcript.complete
                && view.state.as_ref().map(|state| state.status) == Some(SessionStatusWire::Idle)
                && view
                    .manual_compact
                    .as_ref()
                    .is_none_or(|compact| compact.result.is_some())
        });
        if !allowed {
            self.notice(
                NoticeLevel::Warning,
                "manual compaction requires a loaded, idle, settled, unblocked session",
            );
            return Vec::new();
        }
        let counter = self.next_operation_id;
        self.next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .expect("operation ids exhausted");
        let operation_id = format!("tui-compact-{counter}");
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.manual_compact = Some(ManualCompactState {
                operation_id: operation_id.clone(),
                cancel_requested: false,
                result: None,
                state_refresh_confirmed: false,
                context_refresh_confirmed: false,
            });
        }
        let mut commands = vec![self.request(
            RequestKind::Compact {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
            },
            |id| OutgoingRequest::session_compact(id, &session_id, &operation_id),
        )];
        if let Some(command) = self.arm_context_poll(
            &session_id,
            ContextQueryOwner::ManualCompact(operation_id.clone()),
            true,
        ) {
            commands.push(command);
        }
        self.notice(
            NoticeLevel::Info,
            format!("context compaction {operation_id} started"),
        );
        commands
    }

    /// The precise retry target of a request kind, if it may be retried after
    /// a full-FIFO refusal. Kinds without a target have no safe automatic
    /// retry and are reported instead.
    pub(super) fn retry_key(kind: &RequestKind) -> Option<RetryKey> {
        Some(match kind {
            RequestKind::SendTurn {
                local_submission, ..
            } => RetryKey::Submission(*local_submission),
            RequestKind::WaitTurn(turn) => RetryKey::Wait(turn.clone()),
            RequestKind::TurnResult(turn) => RetryKey::TurnResult(turn.clone()),
            RequestKind::SteerTurn {
                session_id,
                steer_id,
                ..
            } => RetryKey::Steer {
                session_id: session_id.clone(),
                steer_id: *steer_id,
            },
            RequestKind::CancelTurn(turn) => RetryKey::CancelTurn(turn.clone()),
            RequestKind::Compact {
                session_id,
                operation_id,
            } => RetryKey::Compact {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
            },
            RequestKind::CompactCancel {
                session_id,
                operation_id,
            } => RetryKey::CancelCompact {
                session_id: session_id.clone(),
                operation_id: operation_id.clone(),
            },
            RequestKind::History { session_id, .. } => RetryKey::History(session_id.clone()),
            RequestKind::SessionState { session_id, .. } => RetryKey::SessionRead {
                session_id: session_id.clone(),
                label: "state",
            },
            RequestKind::SessionPresentation { session_id } => RetryKey::SessionRead {
                session_id: session_id.clone(),
                label: "presentation",
            },
            RequestKind::SessionContext { session_id, .. } => RetryKey::SessionRead {
                session_id: session_id.clone(),
                label: "context",
            },
            _ => return None,
        })
    }

    /// Whether an identical retry intent is already retained for this target.
    pub(super) fn retry_pending(&self, key: &RetryKey) -> bool {
        self.pending_retries.contains_key(key)
    }

    /// Handles a request whose synchronous admission found the FIFO full. The
    /// id is revoked (it was never written) and nothing is replayed from the
    /// wire.
    ///
    /// - A control intent that was never written (`turn.cancel`,
    ///   `session.compact.cancel`) is retained by exact target and re-emitted
    ///   when the FIFO admits it; a cancel is never abandoned because the
    ///   queue stayed full (spec §5.2).
    /// - A settlement read that was never written (`turn.wait`,
    ///   `turn.result`) is retained the same way; if the retention bound is
    ///   reached it falls back to the result-confirmation recovery path.
    /// - An ordinary user intent (`turn.send`, `turn.steer`,
    ///   `session.update`) is never auto-retried: the input is restored and
    ///   the UI reports Busy so the user decides whether to submit again.
    /// - Read queries merge into the normal refresh path and are re-read
    ///   through their generation, never through this queue.
    pub(super) fn on_queue_full(
        &mut self,
        request: OutgoingRequest,
        _class: SendClass,
    ) -> Vec<AppCommand> {
        let Some(kind) = self.pending_requests.remove(&request.id) else {
            return Vec::new();
        };
        self.free_query_slot(request.id);
        let entry = RetryEntry { kind, request };
        if Self::is_retained_intent(&entry.kind) {
            let key = Self::retry_key(&entry.kind).expect("retained intents have a precise target");
            let is_cancel = matches!(
                entry.kind,
                RequestKind::CancelTurn(_) | RequestKind::CompactCancel { .. }
            );
            if self.pending_retries.len() >= MAX_RPC_RETRIES
                && !self.pending_retries.contains_key(&key)
            {
                // Make room without ever abandoning a cancel: drop the oldest
                // settlement intent (recoverable through turn.result/state).
                let victim = self
                    .pending_retries
                    .iter()
                    .find(|(_, retained)| !Self::is_cancel_intent(&retained.kind))
                    .map(|(key, _)| key.clone());
                match victim {
                    Some(victim) => {
                        if let Some(older) = self.pending_retries.remove(&victim) {
                            self.abandon_retry(older);
                        }
                    }
                    None if !is_cancel => {
                        // Only cancels are retained and this is a settlement
                        // read: recover through the result read-back instead.
                        self.abandon_retry(entry);
                        return Vec::new();
                    }
                    None => {}
                }
            }
            self.pending_retries.insert(key, entry);
            self.notice(
                NoticeLevel::Info,
                "the send queue is busy; the cancel stays pending until it is admitted",
            );
            return Vec::new();
        }
        self.abandon_retry(entry);
        Vec::new()
    }

    /// A control intent whose loss would strand real work, or a settlement read
    /// whose loss would stall a live turn. Only these may keep waiting for
    /// transport space after a refusal; ordinary sends are the user's call.
    fn is_retained_intent(kind: &RequestKind) -> bool {
        matches!(
            kind,
            RequestKind::CancelTurn(_)
                | RequestKind::CompactCancel { .. }
                | RequestKind::WaitTurn(_)
                | RequestKind::TurnResult(_)
        )
    }

    fn is_cancel_intent(kind: &RequestKind) -> bool {
        matches!(
            kind,
            RequestKind::CancelTurn(_) | RequestKind::CompactCancel { .. }
        )
    }

    /// One request that was refused admission for the last time: restore the
    /// user-visible input (never silently drop it) and say so.
    pub(super) fn abandon_retry(&mut self, entry: RetryEntry) {
        match entry.kind {
            RequestKind::SendTurn {
                session_id,
                local_submission,
            } => {
                self.restore_unsent_turn(&session_id, local_submission);
                self.notice(
                    NoticeLevel::Warning,
                    "the send queue is busy; the prompt stays in the editor and was not sent",
                );
            }
            RequestKind::SteerTurn {
                session_id,
                steer_id,
                ..
            } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    let returned = view.live.as_mut().and_then(|live| {
                        let position = live
                            .pending_steers
                            .iter()
                            .position(|steer| steer.local_id == steer_id)?;
                        Some(live.pending_steers.remove(position))
                    });
                    if let Some(steer) = returned {
                        view.steer_queue.insert(
                            0,
                            crate::state::turn::SteerQueueItem {
                                local_id: steer.local_id,
                                text: steer.text,
                                state: crate::state::turn::SteerQueueState::Unsent,
                                editor_revision: None,
                                handoff: false,
                            },
                        );
                    }
                    // Never auto-resend a message the Agent never saw.
                    view.steer_queue_paused = true;
                }
                self.notice(
                    NoticeLevel::Warning,
                    "the send queue is busy; the steer stays in the paused queue and was not sent",
                );
            }
            RequestKind::UpdateSession { session_id, .. } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.config_update = None;
                }
                self.notice(
                    NoticeLevel::Warning,
                    "the send queue is busy; the model/reasoning update was not sent",
                );
            }
            RequestKind::WaitTurn(turn) | RequestKind::TurnResult(turn) => {
                // A settlement read that was never written. Mark the result as
                // needing an authoritative read and let the recovery path
                // (`turn.result`) settle it; never fabricate a registered wait.
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    view.result_confirmation = ResultConfirmation::NeedsRead;
                }
                self.notice(
                    NoticeLevel::Warning,
                    "the send queue is busy; the turn result will be read back instead",
                );
            }
            RequestKind::History { session_id, .. }
            | RequestKind::SessionState { session_id, .. }
            | RequestKind::SessionPresentation { session_id }
            | RequestKind::SessionContext { session_id, .. } => {
                self.mark_session_uncalibrated(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    "a read request was not admitted; the session stays uncalibrated",
                );
            }
            _ => {
                self.notice(
                    NoticeLevel::Warning,
                    "a request was never admitted and cannot be retried automatically",
                );
            }
        }
    }

    /// A `turn.send` that was never written: remove its submission and pending
    /// card, and return its text to the editor. An existing draft is kept and
    /// the unsent prompt is appended, so neither copy is lost.
    pub(super) fn restore_unsent_turn(
        &mut self,
        session_id: &SessionId,
        local_submission: LocalSubmissionId,
    ) {
        self.submissions.remove(&local_submission);
        let recovered = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return;
            };
            let mut recovered = None;
            let current_submission = view
                .live
                .as_ref()
                .is_some_and(|live| live.local_submission == local_submission);
            if current_submission {
                if !view.is_blocked() {
                    if let Some(live) = view.live.take() {
                        recovered = Some(live.user_text);
                    }
                } else if let Some(live) = view.live.as_ref() {
                    recovered = Some(live.user_text.clone());
                }
                view.transcript
                    .blocks_mut()
                    .retain(|block| !matches!(block, TranscriptBlock::User(card) if card.pending));
                view.transcript.invalidate();
            }
            recovered
        };
        // A handoff item owns its queued text: only plain (non-queue)
        // submissions restore the editor (a second copy would duplicate it on
        // the next Enter).
        let is_handoff = self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.steer_queue.iter().any(|item| item.handoff));
        if self.sessions.active.as_ref() == Some(session_id) && !is_handoff {
            if let Some(text) = recovered {
                let existing = self.composer.content();
                if existing.trim().is_empty() {
                    self.composer.set_text(&text);
                } else {
                    self.composer.set_text(&format!("{existing}\n{text}"));
                }
            }
        }
        // The fresh-turn handoff could not be written: keep its queued entry
        // as Unsent, clear the handoff, and PAUSE (a definitive pre-write
        // failure may only be deliberately re-sent).
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            for item in &mut view.steer_queue {
                item.handoff = false;
            }
            view.steer_queue_paused = true;
            if view
                .live
                .as_ref()
                .is_some_and(|live| live.local_submission == local_submission)
            {
                view.state
                    .as_mut()
                    .and_then(|state| state.compaction.take());
            }
        }
        self.context_polls.remove(session_id);
    }

    /// Records one content-free stderr notice. Only the byte length and any
    /// collapsed drop count are kept; the text itself never enters app state
    /// or logs (spec §19).
    pub(super) fn push_stderr(&mut self, bytes: usize, dropped: usize) {
        if dropped > 0 {
            self.agent_logs
                .push_back(format!("agent stderr: {dropped} line(s) dropped"));
        }
        self.agent_logs
            .push_back(format!("agent stderr: {bytes} bytes"));
        while self.agent_logs.len() > MAX_AGENT_LOG_LINES {
            self.agent_logs.pop_front();
        }
    }

    /// Applies a finished owned local job. The result carries its capture
    /// identity so a stale completion cannot decorate a newer selection.
    pub(super) fn on_job_finished(&mut self, outcome: JobOutcome) -> Vec<AppCommand> {
        match outcome {
            JobOutcome::Clipboard {
                session_id,
                revision,
                result,
            } => match result {
                Ok(()) => {
                    // A copy belongs to the selection generation it was
                    // taken from. The session check only guards a switch
                    // between two sessions; with no active session the
                    // capture's empty id still matches.
                    let same_session = match self.sessions.active.as_deref() {
                        Some(active) => active == session_id,
                        None => session_id.is_empty(),
                    };
                    if same_session && revision == self.selection_revision {
                        self.selection_copied_until = Some(
                            self.instant_now()
                                .checked_add(Duration::from_millis(1_800))
                                .expect("copy feedback deadline is representable"),
                        );
                    }
                }
                Err(error) => self.notice(NoticeLevel::Warning, error),
            },
        }
        Vec::new()
    }

    pub(super) fn can_send_requests(&self) -> bool {
        match self.connection {
            ConnectionState::Starting | ConnectionState::Ready => true,
            ConnectionState::ShuttingDown | ConnectionState::Failed(_) => false,
        }
    }

    pub(super) fn maybe_clear_unknown_compact_fence(&mut self, session_id: &SessionId) {
        let clear = self.sessions.known.get(session_id).is_some_and(|view| {
            let unknown = view.manual_compact.as_ref().is_some_and(|compact| {
                compact.result.as_ref().is_some_and(|result| {
                    result.status == crate::protocol::CompactStatusWire::UnknownWrite
                })
            });
            unknown
                && view.manual_compact.as_ref().is_some_and(|compact| {
                    compact.state_refresh_confirmed && compact.context_refresh_confirmed
                })
                && view
                    .state
                    .as_ref()
                    .is_none_or(|state| state.compaction.is_none())
                && view
                    .context
                    .as_ref()
                    .is_none_or(|context| context.current_operation.is_none())
        });
        if clear {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.manual_compact = None;
            }
        }
    }

    pub(super) fn on_compact_response(
        &mut self,
        session_id: &SessionId,
        operation_id: &str,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let result = match response.parse_session_compact() {
            Ok(result) if result.operation_id == operation_id => result,
            Ok(_) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.compact response does not match {operation_id}"),
                );
                return Vec::new();
            }
            Err(error) => {
                self.finish_compact_failure(session_id, operation_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.compact {operation_id} failed: {error}"),
                );
                return Vec::new();
            }
        };
        let owns_operation = self.sessions.known.get(session_id).is_some_and(|view| {
            view.manual_compact
                .as_ref()
                .is_some_and(|compact| compact.operation_id == operation_id)
        });
        if !owns_operation {
            self.notice(
                NoticeLevel::Warning,
                format!("stale session.compact result ignored for {operation_id}"),
            );
            return Vec::new();
        }
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            let compact = view
                .manual_compact
                .as_mut()
                .expect("manual compact ownership was checked");
            compact.result = Some(result.clone());
            view.state
                .as_mut()
                .and_then(|state| state.compaction.take());
            view.context_query_generation = view
                .context_query_generation
                .checked_add(1)
                .expect("context query generations exhausted");
        }
        self.context_polls.remove(session_id);
        match result.status {
            crate::protocol::CompactStatusWire::Compacted
            | crate::protocol::CompactStatusWire::Noop => {
                self.notice(
                    NoticeLevel::Info,
                    format!("context compaction {operation_id} completed"),
                );
                self.arm_context_poll(session_id, ContextQueryOwner::Explicit, true)
                    .into_iter()
                    .collect()
            }
            crate::protocol::CompactStatusWire::Failed => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "context compaction {operation_id} failed ({})",
                        result.failure_kind.as_deref().unwrap_or("unknown failure")
                    ),
                );
                self.arm_context_poll(session_id, ContextQueryOwner::Explicit, true)
                    .into_iter()
                    .collect()
            }
            crate::protocol::CompactStatusWire::UnknownWrite => {
                self.notice(
                    NoticeLevel::Error,
                    format!(
                        "context compaction {operation_id} has unknown write outcome; state/context reread required"
                    ),
                );
                let mut commands = vec![self.request_session_state(session_id)];
                if let Some(command) =
                    self.arm_context_poll(session_id, ContextQueryOwner::Explicit, true)
                {
                    commands.push(command);
                }
                commands
            }
        }
    }

    pub(super) fn finish_compact_failure(&mut self, session_id: &SessionId, operation_id: &str) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            if view
                .manual_compact
                .as_ref()
                .is_some_and(|compact| compact.operation_id == operation_id)
            {
                view.manual_compact = None;
                view.state
                    .as_mut()
                    .and_then(|state| state.compaction.take());
                view.context_query_generation = view
                    .context_query_generation
                    .checked_add(1)
                    .expect("context query generations exhausted");
            }
        }
        self.context_polls.remove(session_id);
    }

    pub(super) fn on_compact_cancel_response(
        &mut self,
        session_id: &SessionId,
        operation_id: &str,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let owns_operation = self.sessions.known.get(session_id).is_some_and(|view| {
            view.manual_compact.as_ref().is_some_and(|compact| {
                compact.operation_id == operation_id && compact.result.is_none()
            })
        });
        if !owns_operation {
            self.notice(
                NoticeLevel::Warning,
                format!("stale compaction cancel ignored for {operation_id}"),
            );
            return Vec::new();
        }
        match response.result_as::<crate::protocol::CancelledResult>() {
            Ok(result) if result.cancelled => {
                self.notice(
                    NoticeLevel::Info,
                    format!("cancellation requested for compaction {operation_id}"),
                );
            }
            Ok(_) => self.notice(
                NoticeLevel::Warning,
                format!("compaction {operation_id} was not cancellable"),
            ),
            Err(error) => self.notice(
                NoticeLevel::Warning,
                format!("compaction cancel {operation_id} failed: {error}"),
            ),
        }
        self.arm_context_poll(
            session_id,
            ContextQueryOwner::ManualCompact(operation_id.to_owned()),
            true,
        )
        .into_iter()
        .collect()
    }

    pub(super) fn request_compact_cancel(
        &mut self,
        session_id: &SessionId,
        operation_id: &str,
    ) -> Option<AppCommand> {
        if self.pending_requests.values().any(|kind| {
            matches!(kind, RequestKind::CompactCancel { session_id: pending, operation_id: id } if pending == session_id && id == operation_id)
        }) || self.retry_pending(&RetryKey::CancelCompact {
            session_id: session_id.to_owned(),
            operation_id: operation_id.to_owned(),
        }) {
            return None;
        }
        Some(self.request(
            RequestKind::CompactCancel {
                session_id: session_id.to_owned(),
                operation_id: operation_id.to_owned(),
            },
            |id| OutgoingRequest::session_compact_cancel(id, session_id, operation_id),
        ))
    }

    pub(super) fn submit_turn(&mut self, session_id: SessionId, text: String) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            if self.composer.is_empty() {
                self.composer.set_text(&text);
            }
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        if trimmed.len() > MAX_COMPOSER_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.closing)
        {
            self.notice(
                NoticeLevel::Warning,
                "session is closing; cannot submit a new turn",
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.is_blocked())
        {
            self.notice(
                NoticeLevel::Error,
                "session is blocked; resolve or reset before submitting",
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.is_preparing())
        {
            self.notice(
                NoticeLevel::Info,
                "session is preparing context; wait for preparation to finish",
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.live.is_some())
        {
            self.notice(
                NoticeLevel::Warning,
                "session already has a submitted turn; wait for it to finish",
            );
            return Vec::new();
        }
        // A known-invalid state authority or durable history gap blocks a new
        // turn. A pending normal state read or an incomplete history read
        // without a gap retains the legacy admission behavior.
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.event_gap || view.close_verification_unknown)
        {
            self.notice(
                NoticeLevel::Warning,
                "session state/history is not reconciled; cannot submit a new turn",
            );
            return Vec::new();
        }
        if self.sessions.known.get(&session_id).is_some_and(|view| {
            view.state
                .as_ref()
                .is_some_and(|state| state.status != SessionStatusWire::Idle)
        }) {
            self.notice(
                NoticeLevel::Warning,
                "session is not idle; cannot submit a new turn",
            );
            return Vec::new();
        }
        let submission = LocalSubmissionId(self.next_submission);
        self.next_submission = self
            .next_submission
            .checked_add(1)
            .expect("submission ids exhausted");
        let editor_revision = self.composer.editor_revision();
        let session_epoch = self
            .sessions
            .known
            .get(&session_id)
            .map_or(0, |view| view.session_epoch);
        self.submissions.insert(
            submission,
            Submission {
                request_id: None,
                local_id: submission,
                session_epoch,
                editor_revision,
                text: std::sync::Arc::<str>::from(trimmed),
                preparation: None,
                cancel_requested: false,
            },
        );
        {
            let Some(view) = self.sessions.known.get_mut(&session_id) else {
                self.submissions.remove(&submission);
                return Vec::new();
            };
            // Keep the previous last_result as a bounded fence until this
            // new submission receives its own loop reference. The UI hides a
            // result that does not belong to the live loop.
            view.last_request = None;
            view.completed_steers.clear();
            view.result_confirmation = ResultConfirmation::Confirmed;
            if view.config_update.as_ref().is_some_and(|u| {
                u.loop_id.is_some() || u.state == crate::state::session::ConfigUpdateState::Applied
            }) {
                view.config_update = None;
            }
            view.live = Some(LiveLoop {
                reference: None,
                local_submission: submission,
                user_text: trimmed.to_owned(),
                requests: Vec::new(),
                pending_steers: Vec::new(),
                waiting: false,
                cancel_requested: false,
                event_gap: false,
                last_result: None,
            });
            view.steer_state_unconfirmed = false;
            // A fresh loop never inherits unsent steering from the sealed
            // loop. The user must withdraw an old item into the editor before
            // it can become a new prompt.
            view.steer_queue_paused = !view.steer_queue.is_empty();
            view.steer_receipt = None;
            view.applied_steers.clear();
            view.transcript
                .blocks_mut()
                .push(TranscriptBlock::User(UserBlock {
                    index: None,
                    loop_id: None,
                    kind: UserMessageKindWire::Prompt,
                    text: trimmed.to_owned(),
                    pending: true,
                }));
            view.transcript.invalidate();
        }
        let send = self.request(
            RequestKind::SendTurn {
                session_id: session_id.clone(),
                local_submission: submission,
            },
            |id| OutgoingRequest::send_turn(id, &session_id, trimmed),
        );
        if let AppCommand::Rpc(request) = &send {
            if let Some(submission_state) = self.submissions.get_mut(&submission) {
                submission_state.request_id = Some(request.id);
            }
        }
        let mut commands = vec![send];
        if let Some(command) = self.arm_context_poll(
            &session_id,
            ContextQueryOwner::Submission(submission),
            false,
        ) {
            commands.push(command);
        }
        commands
    }

    pub(super) fn cancel_turn(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let manual_operation = self
            .sessions
            .known
            .get(session_id)
            .and_then(|view| {
                view.manual_compact
                    .as_ref()
                    .filter(|compact| compact.result.is_none())
            })
            .map(|compact| compact.operation_id.clone());
        if let Some(operation_id) = manual_operation {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                if let Some(compact) = view.manual_compact.as_mut() {
                    compact.cancel_requested = true;
                }
            }
            return self
                .request_compact_cancel(session_id, &operation_id)
                .into_iter()
                .collect();
        }
        let (reference, submission_id) = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            if view.state.as_ref().is_some_and(|state| {
                matches!(
                    state.status,
                    SessionStatusWire::Finishing | SessionStatusWire::Blocked
                )
            }) {
                return Vec::new();
            }
            if view.live.as_ref().is_some_and(|live| live.waiting) {
                return Vec::new();
            }
            if let Some(live) = view.live.as_mut() {
                live.cancel_requested = true;
                (live.reference.clone(), Some(live.local_submission))
            } else {
                (
                    view.state.as_ref().and_then(|state| {
                        state.active_loop.as_ref().map(|loop_state| TurnRef {
                            session_id: session_id.clone(),
                            loop_id: loop_state.loop_id.clone(),
                        })
                    }),
                    None,
                )
            }
        };
        let preparation = submission_id.and_then(|local_id| {
            self.submissions.get_mut(&local_id).and_then(|submission| {
                submission.cancel_requested = true;
                submission.preparation.clone()
            })
        });
        if reference.is_none() {
            if let Some(operation) = preparation {
                return self
                    .request_compact_cancel(session_id, &operation.operation_id)
                    .into_iter()
                    .collect();
            }
            if submission_id.is_some() {
                self.notice(
                    NoticeLevel::Info,
                    "cancellation requested; waiting for the preparation operation identity",
                );
            }
            return Vec::new();
        }
        let turn = reference.expect("checked above");
        // A cancellation pauses the unsent queue: never auto-send an
        // ambiguous message after an explicit user cancel.
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.steer_queue_paused = true;
        }
        if self
            .pending_requests
            .values()
            .any(|kind| matches!(kind, RequestKind::CancelTurn(pending) if pending == &turn))
            || self.retry_pending(&RetryKey::CancelTurn(turn.clone()))
        {
            return Vec::new();
        }
        vec![self.request(RequestKind::CancelTurn(turn.clone()), |id| {
            OutgoingRequest::cancel_turn(id, &turn)
        })]
    }

    pub(super) fn retained_turn(&self, session_id: &SessionId) -> Option<TurnRef> {
        let view = self.sessions.known.get(session_id)?;
        if let Some(unsaved) = view.unsaved_loop.as_ref() {
            return Some(unsaved.turn.clone());
        }
        if let Some(live) = view.live.as_ref() {
            return live.reference.clone();
        }
        view.last_result.as_ref().map(|result| result.turn.clone())
    }

    pub(super) fn wait_targets_current_turn(view: &SessionView, turn: &TurnRef) -> bool {
        if let Some(live) = view.live.as_ref() {
            return live.reference.as_ref() == Some(turn);
        }
        match (view.unsaved_loop.as_ref(), view.last_result.as_ref()) {
            (Some(unsaved), Some(result)) => &unsaved.turn == turn && &result.turn == turn,
            (Some(unsaved), None) => &unsaved.turn == turn,
            (None, Some(result)) => &result.turn == turn,
            (None, None) => false,
        }
    }

    /// Explicitly reads a retained completion once. A repeated request for
    /// the same turn is ignored while one wait is already registered; the
    /// response reducer also ignores an identical completion, so this cannot
    /// duplicate history or live cards.
    pub(super) fn refresh_turn(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        let Some(turn) = self.retained_turn(session_id) else {
            return Vec::new();
        };
        self.request_wait(turn).into_iter().collect()
    }

    pub(super) fn request_wait(&mut self, turn: TurnRef) -> Option<AppCommand> {
        if self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::WaitTurn(pending) if pending == &turn
            )
        }) {
            return None;
        }
        let kind = RequestKind::WaitTurn(turn.clone());
        if let Some(key) = Self::retry_key(&kind) {
            if self.retry_pending(&key) {
                return None;
            }
        }
        if !self.deferred_admission_ok() {
            if self.defer_request(kind, |id| OutgoingRequest::wait_turn(id, &turn)) {
                self.notice(
                    NoticeLevel::Info,
                    "deferred request limit reached; turn.wait will be registered when a slot frees",
                );
            }
            return None;
        }
        Some(self.request(kind, |id| OutgoingRequest::wait_turn(id, &turn)))
    }

    /// Reads a turn's authoritative result once (spec §7.2). Used when a `wait`
    /// result was lost or its save is unconfirmed, so the retained report can
    /// be read back without rerunning any tool. Only one read per turn is in
    /// flight, and an already-complete live result is not re-fetched.
    pub(super) fn recover_turn(&mut self, turn: TurnRef) -> Option<AppCommand> {
        if !self.can_send_requests()
            || self
                .pending_requests
                .values()
                .any(|kind| matches!(kind, RequestKind::TurnResult(pending) if pending == &turn))
        {
            return None;
        }
        if self
            .sessions
            .known
            .get(&turn.session_id)
            .is_some_and(|view| {
                view.live.as_ref().is_some_and(|live| {
                    live.reference.as_ref() == Some(&turn) && live.last_result.is_some()
                }) || view.last_result.as_ref().is_some_and(|r| {
                    r.turn == turn && r.persistence == Some(TurnPersistenceWire::Persisted)
                })
            })
            || self
                .retained_results
                .get(&turn)
                .is_some_and(|result| result.persistence == Some(TurnPersistenceWire::Persisted))
        {
            return None;
        }
        let cursor = self
            .turn_results
            .entry(turn.clone())
            .or_insert_with(|| crate::app::history::TurnResultWindow::new(turn.clone()))
            .cursor;
        self.request_turn_result_page(turn, cursor)
    }

    pub(super) fn on_send_response(
        &mut self,
        session_id: &SessionId,
        local_submission: LocalSubmissionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let submission_state = self.submissions.remove(&local_submission);
        let submission_cancel_requested = submission_state
            .as_ref()
            .is_some_and(|submission| submission.cancel_requested);
        enum Plan {
            Wait {
                turn: TurnRef,
                cancel: bool,
                first_binding: bool,
            },
            Failed {
                recovered: Option<String>,
                error: RpcResponseError,
            },
            Stale,
            Mismatch,
        }
        let plan = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            if submission_state
                .as_ref()
                .is_some_and(|submission| submission.session_epoch != view.session_epoch)
            {
                Plan::Stale
            } else {
                let pending_user_text =
                    view.transcript.blocks.iter().find_map(|block| match block {
                        TranscriptBlock::User(card) if card.pending => Some(card.text.clone()),
                        _ => None,
                    });
                let Some(live) = view.live.as_mut() else {
                    return Vec::new();
                };
                let parsed = response.parse_turn_send();
                if live.local_submission == LocalSubmissionId(u64::MAX)
                    && parsed.as_ref().is_ok_and(|result| {
                        result.turn.session_id.as_str() == session_id.as_str()
                            && live
                                .reference
                                .as_ref()
                                .is_some_and(|reference| reference == &result.turn)
                    })
                {
                    live.local_submission = local_submission;
                    if live.user_text.is_empty() {
                        live.user_text = pending_user_text.unwrap_or_default();
                    }
                }
                if live.local_submission != local_submission {
                    return Vec::new();
                }
                match parsed {
                    Ok(result) => {
                        if result.turn.session_id.as_str() != session_id.as_str()
                            || live
                                .reference
                                .as_ref()
                                .is_some_and(|reference| reference != &result.turn)
                        {
                            Plan::Mismatch
                        } else {
                            let first_binding = live.reference.is_none();
                            if view
                                .last_result
                                .as_ref()
                                .is_some_and(|previous| previous.turn != result.turn)
                            {
                                view.last_result = None;
                            }
                            view.result_confirmation = ResultConfirmation::Confirmed;
                            live.reference = Some(result.turn.clone());
                            view.live_user_timestamp = result.accepted_at.clone();
                            view.live_user_time_accepted = true;
                            let pending_user = view.transcript.blocks_mut().iter_mut().find_map(
                                |block| match block {
                                    TranscriptBlock::User(card) if card.pending => Some(card),
                                    _ => None,
                                },
                            );
                            if let Some(card) = pending_user {
                                card.loop_id = Some(result.turn.loop_id.clone());
                            }
                            view.transcript.invalidate();
                            Plan::Wait {
                                turn: result.turn,
                                cancel: live.cancel_requested || submission_cancel_requested,
                                first_binding,
                            }
                        }
                    }
                    Err(error) => {
                        let is_blocked_err = matches!(&error, crate::protocol::RpcResponseError::Agent(err) if err.code == -32004);
                        // A fresh-turn handoff whose loop the Agent ALREADY started
                        // (a TurnStarted event bound the reference) is a PROVEN
                        // accept: keep the running loop and wait; never abandon an
                        // accepted turn nor retry its message.
                        let is_handoff_send = view.steer_queue.iter().any(|item| item.handoff);
                        if is_handoff_send
                            && view
                                .live
                                .as_ref()
                                .is_some_and(|live| live.reference.is_some())
                        {
                            let turn = view
                                .live
                                .as_ref()
                                .and_then(|live| live.reference.as_ref())
                                .cloned()
                                .expect("checked above");
                            let cancel =
                                view.live.as_ref().is_some_and(|live| live.cancel_requested)
                                    || submission_cancel_requested;
                            view.transcript.blocks_mut().retain(
                            |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                        );
                            view.transcript.invalidate();
                            // The started loop owns the text: drop the queue entry
                            // exactly like the accepted-Wait path.
                            view.steer_queue.retain(|item| !item.handoff);
                            Plan::Wait {
                                turn,
                                cancel,
                                first_binding: false,
                            }
                        } else {
                            let recovered = if view.is_blocked() || is_blocked_err {
                                view.live.as_ref().map(|live| live.user_text.clone())
                            } else {
                                view.live.take().map(|live| live.user_text)
                            };
                            view.transcript.blocks_mut().retain(
                            |block| !matches!(block, TranscriptBlock::User(card) if card.pending),
                        );
                            view.transcript.invalidate();
                            Plan::Failed { recovered, error }
                        }
                    }
                }
            }
        };
        match plan {
            Plan::Wait {
                turn,
                cancel,
                first_binding,
            } => {
                if !self.can_send_requests()
                    && !matches!(self.connection, ConnectionState::ShuttingDown)
                {
                    return Vec::new();
                }
                // A fresh-turn handoff is now an owned, accepted turn: drop the
                // queue entry it represented (durable history owns it).
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.steer_queue.retain(|item| !item.handoff);
                }
                let mut commands = Vec::new();
                if let Some(command) = self.request_wait(turn.clone()) {
                    commands.push(command);
                }
                if cancel {
                    commands.push(self.request(RequestKind::CancelTurn(turn.clone()), |id| {
                        OutgoingRequest::cancel_turn(id, &turn)
                    }));
                }
                if let Some(command) =
                    self.maybe_request_state_after_turn_binding(&turn, first_binding)
                {
                    commands.push(command);
                }
                commands
            }
            Plan::Failed { recovered, error } => {
                // A handoff item already owns its queued text: restoring a
                // second copy into the composer would duplicate it on the next
                // Enter. Plain (non-queue) submissions keep the editor restore.
                let is_handoff_send = self
                    .sessions
                    .known
                    .get(session_id)
                    .is_some_and(|view| view.steer_queue.iter().any(|item| item.handoff));
                // A response that reached the Agent but cannot be decoded
                // (Parse/Malformed) is UNCERTAIN: never auto-resend it.
                let uncertain = is_handoff_send
                    && matches!(
                        &error,
                        crate::protocol::RpcResponseError::Parse(_)
                            | crate::protocol::RpcResponseError::Malformed
                    );
                let revision_unchanged = submission_state.as_ref().is_none_or(|submission| {
                    submission.editor_revision == self.composer.editor_revision()
                });
                if self.sessions.active.as_ref() == Some(session_id)
                    && !is_handoff_send
                    && revision_unchanged
                {
                    if let Some(text) =
                        recovered.filter(|_| self.composer.content().trim().is_empty())
                    {
                        self.composer.set_text(&text);
                    }
                }
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    for item in &mut view.steer_queue {
                        if item.handoff {
                            item.handoff = false;
                            if uncertain {
                                item.state = SteerQueueState::Unconfirmed;
                            }
                        }
                    }
                    view.steer_queue_paused = true;
                    view.state
                        .as_mut()
                        .and_then(|state| state.compaction.take());
                }
                self.context_polls.remove(session_id);
                let message = if uncertain {
                    "turn send response could not be decoded; the queued steering is unconfirmed and will not be resubmitted automatically".to_owned()
                } else if let crate::protocol::RpcResponseError::Agent(agent_error) = &error {
                    if let Some(data) = agent_error.data.as_ref() {
                        format!(
                            "turn preparation failed ({}): {}",
                            data.kind, agent_error.message
                        )
                    } else {
                        format!("turn preparation failed: {agent_error}")
                    }
                } else {
                    format!("turn send failed: {error}")
                };
                self.notice(NoticeLevel::Warning, message);
                Vec::new()
            }
            Plan::Mismatch => {
                self.connection_terminated("turn.send response does not match the live loop");
                Vec::new()
            }
            Plan::Stale => Vec::new(),
        }
    }

    pub(super) fn on_wait_response(
        &mut self,
        turn: TurnRef,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let parsed = response.parse_turn_wait();
        let mismatched_turn = parsed.as_ref().is_ok_and(|result| result.turn != turn);
        if let Ok(result) = parsed.as_ref() {
            if result.turn == turn {
                self.retain_result_summary(result.clone());
            }
        }
        let current_turn = self
            .sessions
            .known
            .get(&turn.session_id)
            .is_some_and(|view| Self::wait_targets_current_turn(view, &turn));
        if !current_turn {
            if parsed.as_ref().is_ok_and(|result| {
                result.turn == turn && result.persistence == Some(TurnPersistenceWire::Failed)
            }) {
                return self.recover_turn(turn).into_iter().collect();
            }
            return Vec::new();
        }
        let (persistence_failed, persistence_unknown, result, duplicate) = {
            let view = self
                .sessions
                .known
                .get_mut(&turn.session_id)
                .expect("current turn session exists");
            let old_result = view.last_result.clone();
            let live_matches = view
                .live
                .as_ref()
                .is_some_and(|live| live.reference.as_ref() == Some(&turn));
            match &parsed {
                Ok(_) if mismatched_turn => {
                    if live_matches {
                        let live = view.live.as_mut().expect("matching live turn exists");
                        live.waiting = true;
                    }
                    (false, false, None, false)
                }
                Ok(result) => {
                    let failed = result.persistence == Some(TurnPersistenceWire::Failed);
                    let duplicate = old_result.as_ref() == Some(result);
                    if live_matches {
                        let live = view.live.as_mut().expect("matching live turn exists");
                        if !duplicate {
                            live.waiting = true;
                            live.last_result = Some(result.clone());
                        }
                    }
                    (
                        failed,
                        result.persistence.is_none(),
                        Some(result.clone()),
                        duplicate,
                    )
                }
                Err(_) => {
                    if live_matches {
                        let live = view.live.as_mut().expect("matching live turn exists");
                        live.waiting = true;
                    }
                    (false, false, None, false)
                }
            }
        };
        if let Some(result) = result.as_ref() {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.last_result = Some(result.clone());
                view.recompute_usage_projection();
            }
        }

        if result.is_none() {
            let message = if mismatched_turn {
                "malformed turn.wait result (turn reference does not match request); result/save unconfirmed".to_owned()
            } else {
                match parsed {
                    Err(RpcResponseError::Agent(error)) => {
                        format!("turn wait failed ({error}); result/save unconfirmed")
                    }
                    Err(RpcResponseError::Parse(error)) => {
                        format!("malformed turn.wait result ({error}); result/save unconfirmed")
                    }
                    Err(RpcResponseError::Malformed) => {
                        "turn.wait response has no payload; result/save unconfirmed".to_owned()
                    }
                    Ok(_) => unreachable!(),
                }
            };
            self.notice(NoticeLevel::Warning, message);
            // The wait itself was lost: read the authoritative result back once
            // instead of assuming the turn never completed (spec §7.2/§8.4).
            // This never reruns a tool.
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.result_confirmation = ResultConfirmation::NeedsRead;
                if let Some(live) = view.live.as_mut() {
                    live.waiting = true;
                }
            }
            return self.recover_turn(turn).into_iter().collect();
        }

        if persistence_unknown {
            self.notice(
                NoticeLevel::Warning,
                "turn.wait did not report persistence; result remains unconfirmed",
            );
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.result_confirmation = ResultConfirmation::NeedsRead;
            }
            return self.recover_turn(turn).into_iter().collect();
        }

        if persistence_failed {
            if !duplicate {
                self.notice(
                    NoticeLevel::Error,
                    "Turn completed but persistence failed; session is blocked.",
                );
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    if let Some(state) = view.state.as_mut() {
                        state.status = SessionStatusWire::Blocked;
                        state.active_loop = None;
                        state.block_reason =
                            Some(crate::protocol::SessionBlockReasonWire::Persistence);
                    } else {
                        view.state = Some(SessionStateWire {
                            session_id: turn.session_id.clone(),
                            status: SessionStatusWire::Blocked,
                            active_loop: None,
                            block_reason: Some(
                                crate::protocol::SessionBlockReasonWire::Persistence,
                            ),
                            compaction: None,
                        });
                    }
                    if let Some(live) = view.live.as_ref() {
                        let user_text = live.user_text.clone();
                        let requests = live.requests.clone();
                        let event_gap = live.event_gap;
                        view.unsaved_loop = Some(UnsavedLoop {
                            turn: turn.clone(),
                            user_text,
                            requests,
                            result: result.clone(),
                            event_gap,
                        });
                    }
                    Self::mark_pending_steers_unconfirmed(view);
                    view.recompute_usage_projection();
                }
            }
            return self.recover_turn(turn).into_iter().collect();
        }

        if duplicate {
            return Vec::new();
        }
        let mut commands = self.reconcile_after_wait(&turn);
        if let Some(command) = self.request_session_presentation(&turn.session_id) {
            commands.push(command);
        }
        commands
    }

    pub(super) fn pending_wait_for(&self, turn: &TurnRef) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::WaitTurn(pending) if pending == turn
            )
        })
    }

    pub(super) fn on_steer_response(
        &mut self,
        session_id: &SessionId,
        loop_id: &str,
        steer_id: u64,
        steer_text: &str,
        editor_revision: Option<u64>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let composer_text = self.composer.content().trim().to_owned();
        let composer_session = self.sessions.active.as_ref() == Some(session_id);
        let composer_revision_matches =
            editor_revision.is_some_and(|revision| revision == self.composer.editor_revision());

        let parsed = response.parse_steer();
        let is_ok = parsed.as_ref().is_ok_and(|res| res.ok);

        if is_ok {
            if composer_session && composer_revision_matches && composer_text == steer_text {
                self.composer.submit_pushed(&composer_text);
                self.composer.clear();
            }
            let mut acked_live = false;
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                let history_proves_not_recorded =
                    Self::history_proves_steer_not_recorded(view, loop_id, steer_text);
                let is_live_matching = view
                    .live
                    .as_ref()
                    .and_then(|l| l.reference.as_ref())
                    .is_some_and(|r| r.loop_id.as_str() == loop_id);

                if is_live_matching {
                    if let Some(live) = view.live.as_mut() {
                        if let Some(steer) = live
                            .pending_steers
                            .iter_mut()
                            .find(|s| s.local_id == steer_id)
                        {
                            if let PendingSteerState::Sending = steer.state {
                                steer.state = PendingSteerState::Queued;
                                steer.accepted_at = parsed
                                    .as_ref()
                                    .ok()
                                    .and_then(|result| result.accepted_at.clone());
                                steer.steer_index =
                                    parsed.as_ref().ok().and_then(|result| result.steer_index);
                                acked_live = true;
                            } else if steer.state == PendingSteerState::Unconfirmed
                                && history_proves_not_recorded
                            {
                                steer.state = PendingSteerState::NotRecorded;
                            }
                        }
                    }
                } else if let Some(steer) = view
                    .completed_steers
                    .iter_mut()
                    .find(|s| s.loop_id == loop_id && s.local_id == steer_id)
                {
                    if steer.state == PendingSteerState::Sending {
                        steer.state = PendingSteerState::Queued;
                        steer.accepted_at = parsed
                            .as_ref()
                            .ok()
                            .and_then(|result| result.accepted_at.clone());
                    } else if steer.state == PendingSteerState::Unconfirmed
                        && history_proves_not_recorded
                    {
                        // A complete persisted History with no matching item
                        // is authoritative: a late ok only confirms the
                        // request was accepted, not that it was recorded.
                        steer.state = PendingSteerState::NotRecorded;
                    }
                }
            }
            // The ACK may now pair against an already-observed receipt
            // (progress can race ahead of the ACK); reconcile by identity.
            if acked_live {
                self.try_apply_steer_receipts(session_id);
            }
            return Vec::new();
        }

        // Steer rejected or failed. Decide first whether the rejection is
        // DEFINITIVE (typed agent error / decoded ok:false) or AMBIGUOUS (the
        // frame could not be decoded and the agent may still have accepted it).
        let definitive_rejection = !matches!(
            parsed,
            Err(RpcResponseError::Parse(_) | RpcResponseError::Malformed)
        );
        let (queue_full, message) = match parsed {
            Ok(_) => (false, "agent rejected the steering request".to_owned()),
            Err(RpcResponseError::Agent(err)) => {
                let queue_full = err.code == crate::protocol::STEER_QUEUE_FULL;
                (queue_full, err.to_string())
            }
            Err(err) => (false, err.to_string()),
        };

        // Definite -> remove it from in-flight and restore the exact message
        // into the unsent queue (FIFO front, paused); ambiguous -> keep the
        // entry Unconfirmed, paused, and never auto-resend or copy it into the
        // unsent queue.
        if definitive_rejection {
            self.remove_pending_steer(session_id, loop_id, steer_id);
            self.pause_steer_queue(session_id);
            self.restore_steer_unsent(session_id, steer_id, steer_text, editor_revision);
            if self.sessions.active.as_ref() == Some(session_id) && self.composer.is_empty() {
                self.composer.set_text(steer_text);
            }
        } else {
            self.pause_steer_queue(session_id);
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                for steer in view
                    .live
                    .as_mut()
                    .into_iter()
                    .flat_map(|live| live.pending_steers.iter_mut())
                {
                    if steer.local_id == steer_id
                        && matches!(
                            steer.state,
                            PendingSteerState::Sending | PendingSteerState::Queued
                        )
                    {
                        steer.state = PendingSteerState::Unconfirmed;
                    }
                }
            }
        }
        if queue_full {
            self.notice(
                NoticeLevel::Warning,
                "Steering queue is full; cannot queue more steers.",
            );
        } else {
            self.notice(
                NoticeLevel::Warning,
                format!("turn.steer failed: {message}"),
            );
        }
        Vec::new()
    }

    /// Removes an in-flight steer from the live or completed registry.
    pub(super) fn remove_pending_steer(
        &mut self,
        session_id: &SessionId,
        loop_id: &str,
        steer_id: u64,
    ) {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        let is_live_matching = view
            .live
            .as_ref()
            .and_then(|l| l.reference.as_ref())
            .is_some_and(|r| r.loop_id.as_str() == loop_id);
        if is_live_matching {
            if let Some(live) = view.live.as_mut() {
                live.pending_steers.retain(|s| s.local_id != steer_id);
            }
        } else {
            view.completed_steers
                .retain(|s| !(s.loop_id == loop_id && s.local_id == steer_id));
        }
    }

    /// Restores a definitely-not-accepted steer into the local unsent queue at
    /// the FIFO front, preserving its local id and text so nothing is dropped
    /// regardless of the current editor content or active session.
    pub(super) fn restore_steer_unsent(
        &mut self,
        session_id: &SessionId,
        steer_id: u64,
        text: &str,
        editor_revision: Option<u64>,
    ) {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        if view
            .steer_queue
            .iter()
            .any(|item| item.local_id == steer_id)
        {
            return;
        }
        view.steer_queue.insert(
            0,
            crate::state::turn::SteerQueueItem {
                local_id: steer_id,
                text: text.to_owned(),
                state: crate::state::turn::SteerQueueState::Unsent,
                editor_revision,
                handoff: false,
            },
        );
    }

    pub(super) fn pause_steer_queue(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.steer_queue_paused = true;
        }
    }

    pub(super) fn clear_steer_handoffs(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            for item in &mut view.steer_queue {
                item.handoff = false;
            }
        }
    }

    pub(super) fn on_cancel_response(&mut self, response: &RpcResponse) -> Vec<AppCommand> {
        if let Err(error) = response.parse_cancel() {
            self.notice(NoticeLevel::Warning, format!("turn cancel failed: {error}"));
        }
        Vec::new()
    }

    pub(super) fn on_send_failed(&mut self, id: RequestId, error: RpcError) -> Vec<AppCommand> {
        let kind = match self.pending_requests.remove(&id) {
            Some(kind) => kind,
            None => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("send failure for unknown request id {}", id.0),
                );
                return Vec::new();
            }
        };
        self.free_query_slot(id);
        if let Some(key) = Self::retry_key(&kind) {
            self.pending_retries.remove(&key);
        }
        if Self::request_session_id(&kind)
            .is_some_and(|session_id| self.session_pending_deletion(session_id))
        {
            return Vec::new();
        }
        let mut commands = Vec::new();
        match kind {
            RequestKind::SendTurn {
                session_id,
                local_submission,
            } => {
                self.restore_unsent_turn(&session_id, local_submission);
                self.notice(NoticeLevel::Warning, format!("turn send failed: {error}"));
            }
            RequestKind::WaitTurn(turn) => {
                let wait_is_current = self
                    .sessions
                    .known
                    .get(&turn.session_id)
                    .is_some_and(|view| Self::wait_targets_current_turn(view, &turn));
                if wait_is_current {
                    if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                        if let Some(live) = view.live.as_mut() {
                            live.waiting = true;
                        }
                        Self::mark_pending_steers_unconfirmed(view);
                    }
                    self.clear_steer_handoffs(&turn.session_id);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("turn wait send failed: {error}; result/save unconfirmed"),
                );
            }
            RequestKind::TurnResult(turn) => {
                let result_is_current = self
                    .sessions
                    .known
                    .get(&turn.session_id)
                    .is_some_and(|view| Self::wait_targets_current_turn(view, &turn));
                if result_is_current {
                    self.mark_result_read_failed(&turn.session_id);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "result read-back for {}/{} could not be sent: {error}; outcome remains unconfirmed",
                        turn.session_id, turn.loop_id
                    ),
                );
            }
            RequestKind::SessionContext { session_id, .. } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.context_query_generation = view
                        .context_query_generation
                        .checked_add(1)
                        .expect("context query generations exhausted");
                }
                self.context_polls.remove(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.context failed for {session_id}: {error}"),
                );
            }
            RequestKind::Compact {
                session_id,
                operation_id,
            } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    if view
                        .manual_compact
                        .as_ref()
                        .is_some_and(|compact| compact.operation_id == operation_id)
                    {
                        view.manual_compact = None;
                    }
                }
                self.context_polls.remove(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.compact {operation_id} failed to send: {error}"),
                );
            }
            RequestKind::CompactCancel {
                session_id,
                operation_id,
            } => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "cancel for compaction {operation_id} in {session_id} failed to send: {error}"
                    ),
                );
            }
            RequestKind::SteerTurn {
                session_id,
                loop_id,
                steer_id,
                text,
                editor_revision,
            } => {
                // A channel/serialization send failure means the steer was
                // NEVER written to the Agent (definitively not accepted):
                // restore it into the local unsent queue (exact FIFO front)
                // as PAUSED so the message is never dropped, whatever the
                // editor currently holds.
                self.remove_pending_steer(&session_id, &loop_id, steer_id);
                self.restore_steer_unsent(&session_id, steer_id, &text, editor_revision);
                self.pause_steer_queue(&session_id);
                self.notice(NoticeLevel::Warning, format!("turn.steer failed: {error}"));
            }
            RequestKind::CancelTurn(_) => {
                self.notice(NoticeLevel::Warning, format!("turn cancel failed: {error}"));
            }
            RequestKind::CloseSession { session_id } => {
                self.mark_close_verification_unknown(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.close failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::CloseVerifyState { session_id } => {
                self.mark_close_verification_unknown(&session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("close verification failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::DeleteSession { session_id } => {
                self.sessions.pending_deletes.remove(&session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if matches!(&state.mode, SessionPanelMode::ConfirmDelete { .. }) {
                            state.mode = SessionPanelMode::ConfirmDelete {
                                choice: SessionConfirmChoice::Cancel,
                                submitting: false,
                            };
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.delete failed to send for {session_id}: {error}"),
                );
            }
            RequestKind::UpdateSession {
                session_id,
                loop_id,
                model,
                reasoning,
            } => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.update failed for {session_id}: {error}"),
                );
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.config_update = Some(crate::state::session::PendingConfigUpdate {
                        loop_id,
                        model,
                        reasoning,
                        revision: None,
                        state: crate::state::session::ConfigUpdateState::Failed(error.to_string()),
                    });
                }
                if let Dock::ModelSelector(state) | Dock::ReasoningSelector(state) = &mut self.dock
                {
                    state.submitting = false;
                    state.error = Some(error.to_string());
                }
            }
            RequestKind::RenameSession { session_id } => {
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                            *submitting = false;
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.rename failed for {session_id}: {error}"),
                );
            }
            RequestKind::History { session_id, .. } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    Self::mark_history_unconfirmed(view);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("history request failed: {error}"),
                );
            }
            RequestKind::Ping
            | RequestKind::ListModels
            | RequestKind::ListProfiles
            | RequestKind::ListSessions => {
                if self.connection == ConnectionState::ShuttingDown {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("bootstrap request failed during shutdown: {error}"),
                    );
                } else {
                    commands.extend(
                        self.connection_terminated(&format!("bootstrap request failed: {error}")),
                    );
                }
            }
            RequestKind::RefreshSessions { .. } => {
                if let Dock::SessionSelector(state) = &mut self.dock {
                    state.error = Some(format!("refresh failed: {error}"));
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session list refresh failed: {error}"),
                );
            }
            RequestKind::CreateSession { draft } => {
                if let Some(draft_state) = self.draft_matching(draft) {
                    draft_state.submitting = false;
                    draft_state.error = Some(format!("failed to send session.create: {error}"));
                } else {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("create session failed: {error}"),
                    );
                }
            }
            RequestKind::OpenSession {
                session_id,
                previous_retired_loop,
            } => {
                if let Some(retired_loop) = previous_retired_loop {
                    if let Some(view) = self.sessions.known.get_mut(&session_id) {
                        view.retired_loop = Some(retired_loop);
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("open session failed for {session_id}: {error}"),
                );
            }
            RequestKind::SessionState { session_id, query } => {
                let current = self
                    .sessions
                    .known
                    .get(&session_id)
                    .and_then(|view| view.latest_state_query)
                    == Some(query);
                if current {
                    self.mark_session_uncalibrated(&session_id);
                    self.notice(
                        NoticeLevel::Warning,
                        format!("state fetch failed for {session_id}: {error}"),
                    );
                }
            }
            RequestKind::SessionPresentation { session_id } => {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.presentation_pending = false;
                }
                if self.connection != ConnectionState::ShuttingDown {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("session presentation request failed for {session_id}: {error}"),
                    );
                }
            }
            RequestKind::StaleRead => {}
            RequestKind::Reload { generation }
            | RequestKind::ReloadModels { generation }
            | RequestKind::ReloadProfiles { generation }
            | RequestKind::ReloadSessions { generation } => {
                commands.extend(self.reload_failed(
                    generation,
                    format!("configuration reload request failed: {error}"),
                ));
            }
            RequestKind::Shutdown => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("shutdown request failed: {error}"),
                );
                commands.push(AppCommand::KillChild);
            }
        }
        commands
    }

    pub(super) fn loop_event_turn(event: &AgentEventWire) -> Option<TurnRef> {
        match event {
            AgentEventWire::TurnStarted { data } => Some(data.turn.clone()),
            AgentEventWire::RequestStarted { data } => Some(data.turn.clone()),
            AgentEventWire::OutputDelta { data } => Some(data.turn.clone()),
            AgentEventWire::ToolStarted { data } => Some(data.turn.clone()),
            AgentEventWire::ToolPresentation { data } => Some(data.turn.clone()),
            AgentEventWire::ToolProgress { data } => Some(data.turn.clone()),
            AgentEventWire::ToolInvocation { data } => Some(data.turn.clone()),
            AgentEventWire::ToolExecution { data } => Some(data.turn.clone()),
            AgentEventWire::ToolProcess { data } => Some(data.turn.clone()),
            AgentEventWire::ToolFinished { data } => Some(data.turn.clone()),
            _ => None,
        }
    }

    pub(super) fn maybe_request_state_after_turn_binding(
        &mut self,
        turn: &TurnRef,
        first_binding: bool,
    ) -> Option<AppCommand> {
        if !first_binding || self.reload.is_some() || !self.can_send_requests() {
            return None;
        }
        let should_request = self
            .sessions
            .known
            .get(&turn.session_id)
            .is_some_and(|view| {
                view.steer_state_unconfirmed
                    && view.latest_state_query.is_none()
                    && view
                        .live
                        .as_ref()
                        .and_then(|live| live.reference.as_ref())
                        .is_some_and(|reference| reference == turn)
            });
        should_request.then(|| self.request_session_state(&turn.session_id))
    }

    pub(super) fn mark_pending_steers_unconfirmed(view: &mut SessionView) {
        if let Some(live) = view.live.as_mut() {
            for steer in &mut live.pending_steers {
                if matches!(
                    steer.state,
                    PendingSteerState::Sending | PendingSteerState::Queued
                ) {
                    steer.state = PendingSteerState::Unconfirmed;
                }
            }
        }
    }

    pub(super) fn adopt_turn_started(&mut self, turn: &TurnRef) {
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        Self::bind_live_turn(view, turn);
    }

    /// Binds the first loop-scoped event to the local pending start. Agent
    /// events may precede the `turn.send` response, so RequestStarted and
    /// OutputDelta must be able to establish the same TurnRef as well.
    pub(super) fn bind_live_turn(view: &mut SessionView, turn: &TurnRef) -> bool {
        let event_gap = view.event_gap;
        // Check the lifecycle fence before an existing live reference. During
        // close→reopen, the old LiveLoop is intentionally retained until the
        // new open response so the old wait can still be handled, but late
        // old notifications must not mutate that retired view.
        if Self::is_prior_loop(view, &turn.loop_id) && view.retired_loop.is_some() {
            return false;
        }
        if let Some(reference) = view.live.as_ref().and_then(|live| live.reference.as_ref()) {
            return reference == turn;
        }
        if view.live.is_some() && Self::is_prior_loop(view, &turn.loop_id) {
            return false;
        }
        let Some(live) = view.live.as_mut() else {
            return false;
        };
        live.event_gap |= event_gap;
        live.reference = Some(turn.clone());
        let mut changed = false;
        for block in view.transcript.blocks_mut() {
            if let TranscriptBlock::User(card) = block {
                if card.pending {
                    card.loop_id = Some(turn.loop_id.clone());
                    changed = true;
                }
            }
        }
        if changed {
            view.transcript.invalidate();
        }
        true
    }

    /// Read-only steering receipt from `steer_progress` (real Model.start):
    /// records the highest observed applied count and pairs it against ACK
    /// `steer_index` values (identity, never queue position). Receipts for
    /// other loops (late/stale after a switch) are ignored.
    pub(super) fn on_steer_progress(
        &mut self,
        turn: &TurnRef,
        request_index: u32,
        applied_count: u64,
    ) {
        {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return;
            };
            let loop_matches = view
                .live
                .as_ref()
                .and_then(|live| live.reference.as_ref())
                .is_some_and(|reference| reference.loop_id == turn.loop_id);
            if !loop_matches {
                return;
            }
            // Monotonic: only a higher count advances the cache, keeping the
            // request_index of the FIRST observation of the current max.
            let grew = view
                .steer_receipt
                .is_none_or(|cached| applied_count > cached.applied_count);
            if grew {
                view.steer_receipt = Some(crate::state::turn::SteerReceiptObserved {
                    request_index,
                    applied_count,
                });
            }
        }
        self.try_apply_steer_receipts(&turn.session_id);
    }

    /// Applies pending accepted entries whose ACK `steer_index` is covered by
    /// the observed receipt. Identity-paired: never inferred by queue position
    /// while an entry is Sending; older Agents (index None) hold conservatively
    /// until terminal History. Applied cards keep the ACK `accepted_at`.
    pub(super) fn try_apply_steer_receipts(&mut self, session_id: &SessionId) {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        let Some(receipt) = view.steer_receipt else {
            return;
        };
        let (applied, applied_count) = {
            let Some(live) = view.live.as_mut() else {
                return;
            };
            let mut applied = Vec::new();
            let mut remaining = Vec::new();
            let mut applied_count = 0usize;
            for steer in live.pending_steers.drain(..) {
                let covered = matches!(steer.state, PendingSteerState::Queued)
                    && steer
                        .steer_index
                        .is_some_and(|index| index <= receipt.applied_count);
                if covered {
                    applied.push(AppliedSteer {
                        local_id: steer.local_id,
                        text: steer.text,
                        accepted_at: steer.accepted_at,
                        request_index: receipt.request_index,
                    });
                    applied_count += 1;
                } else {
                    remaining.push(steer);
                }
            }
            live.pending_steers = remaining;
            (applied, applied_count)
        };
        if applied_count == 0 {
            return;
        }
        let view = self.sessions.known.get_mut(session_id).expect("view");
        for applied in applied {
            view.applied_steers.push(applied);
        }
    }
}
