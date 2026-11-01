//! Terminal facts, canonical turn bodies and completion ordering.

use std::time::Duration;

use super::{BodyRecovery, COMPLETIONS, CompletionMark, Session, turn_bounds};
use crate::app::read::TurnDetails;
use crate::app::recovery::RecoveryStatus;
#[cfg(feature = "e2e-observe")]
use crate::protocol::read::UserKind;
use crate::protocol::read::{InterruptedRequest, RawHistoryItem, TurnPersistence};
use crate::protocol::{TurnWaitResult, Usage};
use crate::transcript::{Block, EXPANDED_ROWS, Transcript, append_history_item, push_interrupted};

impl Session {
    pub(in crate::app) fn note_body_recovery(
        &mut self,
        loop_id: &str,
        status: RecoveryStatus,
        message: String,
    ) {
        self.body_recovery.retain(|note| note.loop_id != loop_id);
        self.body_recovery.push_back(BodyRecovery {
            loop_id: loop_id.to_owned(),
            status,
            message,
        });
        if self.body_recovery.len() > 256 {
            self.body_recovery.pop_front();
        }
        let at =
            self.transcript.blocks().iter().position(
                |block| matches!(block,Block::TurnEnd { loop_id:id,.. } if id == loop_id),
            );
        if let Some(at) = at {
            self.transcript.block_mut(at);
        }
    }

    pub(in crate::app) fn incomplete_completion(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
        status: RecoveryStatus,
        message: String,
    ) -> Option<(usize, isize, usize)> {
        let exists = self.transcript.blocks().iter().any(
            |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == turn.loop_id),
        );
        let moved = if exists {
            if self.region_start(&turn.loop_id).is_none() {
                let range = turn_bounds(self.transcript.blocks(), &turn.loop_id)
                    .expect("an incomplete turn has its terminal");
                self.record_region(&turn.loop_id, range.start);
            }
            None
        } else {
            let start = self
                .region_start(&turn.loop_id)
                .or_else(|| self.completion_position(turn))
                .unwrap_or_else(|| self.transcript.len());
            let before = self.transcript.len();
            self.record_region(&turn.loop_id, start);
            let insertion = self.region_end(&turn.loop_id);
            self.place_early(&turn.loop_id);
            let outcome = match (turn.completion_kind, turn.outcome_kind) {
                (crate::protocol::CompletionKind::InternalError, _) => {
                    crate::protocol::LoopOutcome::Unknown
                }
                (_, Some(crate::protocol::CompletionOutcome::Completed)) => {
                    crate::protocol::LoopOutcome::Completed
                }
                (_, Some(crate::protocol::CompletionOutcome::Cancelled)) => {
                    crate::protocol::LoopOutcome::Cancelled {
                        reason: crate::protocol::CancelReason::Unknown,
                    }
                }
                (_, Some(crate::protocol::CompletionOutcome::Failed)) => {
                    crate::protocol::LoopOutcome::Failed {
                        kind: "unknown".into(),
                        model_error: None,
                    }
                }
                _ => crate::protocol::LoopOutcome::Unknown,
            };
            self.push_in_turn(
                &turn.loop_id,
                Block::TurnEnd {
                    loop_id: turn.loop_id.clone(),
                    outcome,
                    duration: None,
                    requests: 0,
                    tools: 0,
                    usage: Usage::default(),
                    changes: None,
                    saving: None,
                },
            );
            Some((
                insertion,
                (self.transcript.len() - before) as isize,
                insertion,
            ))
        };
        let message = match turn.persistence {
            Some(persistence) => {
                self.saving_note(&turn.loop_id, persistence, turn.persistence_uncertain);
                message
            }
            None => format!("{message} · save unknown"),
        };
        self.remember_completion(turn, false);
        let message = if self.ordering_gap {
            format!("{message} · history anchor unavailable; region position is unresolved")
        } else {
            message
        };
        self.note_body_recovery(&turn.loop_id, status, message);
        moved
    }

    pub fn end_turn(&mut self, wait: &TurnWaitResult) -> bool {
        let loop_id = &wait.turn.loop_id;
        if self.transcript.blocks().iter().any(|block| matches!(block, Block::TurnEnd { loop_id: id, duration: Some(_), .. } if id == loop_id)) {
            return false;
        }
        let live = self.tracked_turn(loop_id);
        let Some(start) = live
            .map(|live| live.start)
            .or_else(|| self.region_start(loop_id))
        else {
            return false;
        };
        let elapsed = live.map(|live| live.started.elapsed());
        let end = self.region_end(loop_id);
        let finishing: Vec<usize> = (start..end)
            .filter(|index| {
                self.transcript.blocks()[*index].is_streaming()
                    || matches!(&self.transcript.blocks()[*index], Block::Tool(call)
                        if call.output_content().len() > EXPANDED_ROWS)
            })
            .collect();
        for index in finishing {
            match self.transcript.block_mut(index) {
                Some(Block::Assistant { streaming, .. }) => *streaming = false,
                Some(Block::Tool(call)) => call.settle(),
                _ => {}
            }
        }
        self.install_terminal(
            loop_id,
            &TurnDetails::from(wait),
            elapsed.or(Some(Duration::ZERO)),
            start..end,
        );
        true
    }

    /// Waits and canonical reads share one metadata installation. Duration is
    /// supplied only by the wait; reading a body must not extend a turn's timer.
    fn install_terminal(
        &mut self,
        loop_id: &str,
        details: &TurnDetails,
        elapsed: Option<Duration>,
        range: std::ops::Range<usize>,
    ) {
        let live = self.tracked_turn(loop_id);
        let requests = details
            .requests
            .unwrap_or_else(|| live.map_or(0, |live| live.requests));
        let usage = details.usage.clone().unwrap_or_else(|| {
            live.map(|live| live.accounted_usage.clone())
                .unwrap_or_default()
        });
        if let Some(previous) = live.map(|live| live.accounted_usage.clone())
            && details.usage.is_some()
        {
            self.usage.replace_contribution(&previous, &usage);
            if let Some(live) = self.tracked_turn_mut(loop_id) {
                live.accounted_usage = usage.clone();
                live.usage_final = true;
            }
        }
        let tools = self.transcript.blocks()[range]
            .iter()
            .filter(|block| matches!(block, Block::Tool(_)))
            .count() as u64;
        let terminal = self.transcript.blocks().iter().rposition(
            |block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id),
        );
        if let Some(at) = terminal {
            if let Some(Block::TurnEnd {
                outcome,
                duration,
                requests: count,
                tools: calls,
                usage: tokens,
                ..
            }) = self.transcript.block_mut(at)
            {
                outcome.clone_from(&details.outcome);
                *duration = duration.or(elapsed);
                *count = (*count).max(requests);
                *calls = (*calls).max(tools);
                if details.usage.is_some()
                    || (tokens.input_tokens.is_none() && tokens.output_tokens.is_none())
                {
                    *tokens = usage;
                }
            }
        } else {
            self.push_in_turn(
                loop_id,
                Block::TurnEnd {
                    loop_id: loop_id.to_owned(),
                    outcome: details.outcome.clone(),
                    duration: elapsed,
                    requests,
                    tools,
                    usage,
                    changes: None,
                    saving: None,
                },
            );
        }
    }

    /// Installs a verified body and saving fact in the region located by its completion descriptor.
    pub fn reconcile_completion(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
        details: &TurnDetails,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
        persistence: TurnPersistence,
        persistence_uncertain: bool,
    ) -> Option<(usize, isize, usize)> {
        // A turn's canonical body is installed once; a second read of it, from
        // the completion directory or a late outcome, changes nothing.
        if self.is_closed(&turn.loop_id) {
            self.saving_note(&turn.loop_id, persistence, persistence_uncertain);
            return None;
        }
        let streamed = self.transcript.blocks().iter().any(
            |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == turn.loop_id),
        );
        if self.region_start(&turn.loop_id).is_none() && self.tracked_turn(&turn.loop_id).is_some()
        {
            return None;
        }
        if streamed && self.region_start(&turn.loop_id).is_none() {
            let range = turn_bounds(self.transcript.blocks(), &turn.loop_id)
                .expect("a streamed turn has its end");
            self.record_region(&turn.loop_id, range.start);
        }
        let mut early_end = None;
        if !streamed && self.region_start(&turn.loop_id).is_none() {
            let start = self.completion_position(turn)?;
            let before = self.transcript.len();
            self.record_region(&turn.loop_id, start);
            self.place_early(&turn.loop_id);
            early_end = Some(start + self.transcript.len() - before);
        }
        let placed = match streamed {
            true => self.reconcile_turn(&turn.loop_id, items, interrupted),
            false => {
                Some(self.place_completion(turn, &details.outcome, items, interrupted, early_end))
            }
        };
        // A missing region remains repairable: closing it would freeze the
        // streamed body and label it saved without installing the result.
        if let Some((start, _, _)) = placed {
            #[cfg(feature = "e2e-observe")]
            for index in start..self.transcript.len() {
                if matches!(
                    self.transcript.blocks()[index],
                    Block::User {
                        kind: UserKind::Prompt,
                        ..
                    }
                ) && self
                    .transcript
                    .user_identity(index)
                    .is_some_and(|user| user.loop_id == turn.loop_id)
                {
                    self.transcript
                        .identify_user(index, &turn.loop_id, turn.input_id.as_deref());
                }
            }
            if let Some(live) = self.tracked_turn_mut(&turn.loop_id) {
                live.start = start;
            }
            let end = self.transcript.blocks().iter().position(
                |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == turn.loop_id),
            ).expect("an installed completion has its terminal");
            self.install_terminal(&turn.loop_id, details, None, start..end);
            self.saving_note(&turn.loop_id, persistence, persistence_uncertain);
            self.remember_completion(turn, true);
        }
        placed
    }

    /// Inserts a turn the client never streamed where the directory says it
    /// belongs: after the last turn that was committed before it.
    fn place_completion(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
        outcome: &crate::protocol::LoopOutcome,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
        early_end: Option<usize>,
    ) -> (usize, isize, usize) {
        let mut durable = self.canonical_body(&turn.loop_id, items, interrupted);
        durable.push(Block::TurnEnd {
            loop_id: turn.loop_id.clone(),
            outcome: outcome.clone(),
            duration: None,
            requests: 0,
            tools: 0,
            usage: Usage::default(),
            changes: None,
            saving: None,
        });
        // Even without a streamed terminal, the recorded region contains the
        // prompt and partial answer that the canonical body must replace.
        let start = self
            .region_start(&turn.loop_id)
            .expect("a confirmed completion has its region");
        let end = early_end.unwrap_or_else(|| self.region_end(&turn.loop_id));
        self.reconcile_from.retain(|(id, _)| id != &turn.loop_id);
        self.replace_turn_body(start..end, durable)
    }

    /// Locates an unseen completion from confirmed sequence and saved-history anchors.
    pub(super) fn completion_position(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
    ) -> Option<usize> {
        let mut start = match &turn.after_saved_loop_id {
            Some(anchor) => match self.transcript.blocks().iter().position(
                |block| matches!(block, Block::TurnEnd { loop_id, .. } if loop_id == anchor),
            ) {
                Some(at) => at + 1,
                None => {
                    self.ordering_gap = true;
                    return None;
                }
            },
            None if self.loaded_from > 0 => {
                self.ordering_gap = true;
                return None;
            }
            None => self
                .transcript
                .blocks()
                .iter()
                .position(|block| {
                    !matches!(
                        block,
                        Block::EarlierHistory { .. } | Block::Compaction { .. }
                    )
                })
                .unwrap_or(self.transcript.len()),
        };
        for prior in self.completions.iter().filter(|entry| entry.seq < turn.seq) {
            if let Some(at) = self.transcript.blocks().iter().position(
                |block| matches!(block,Block::TurnEnd { loop_id,.. } if loop_id == &prior.loop_id),
            ) {
                start = start.max(at + 1);
            }
        }
        let next = self
            .catalog
            .entries
            .iter()
            .chain(self.recovery.entries.values().map(|entry| &entry.turn))
            .filter(|entry| entry.seq > turn.seq)
            .filter_map(|next| {
                turn_bounds(self.transcript.blocks(), &next.loop_id)
                    .map(|range| range.start)
                    .or_else(|| self.region_start(&next.loop_id))
            })
            .min();
        if next.is_some_and(|next| start > next) {
            self.ordering_gap = true;
            return None;
        }
        Some(start)
    }

    /// Records on the turn-end line whether the result was saved, and whether
    /// an uncertain write is still in doubt.
    fn saving_note(
        &mut self,
        loop_id: &str,
        persistence: TurnPersistence,
        persistence_uncertain: bool,
    ) {
        let index = self.transcript.blocks().iter().rposition(
            |block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id),
        );
        let Some(Block::TurnEnd { saving: slot, .. }) =
            index.and_then(|index| self.transcript.block_mut(index))
        else {
            return;
        };
        *slot = Some(match (persistence, persistence_uncertain) {
            (TurnPersistence::Persisted, _) => crate::transcript::Saving::Saved,
            (TurnPersistence::Failed, true) => crate::transcript::Saving::Uncertain,
            (TurnPersistence::Failed, false) => crate::transcript::Saving::Unsaved,
        });
    }

    fn remember_completion(&mut self, turn: &crate::protocol::CompletedTurn, canonical: bool) {
        let canonical = canonical || self.is_closed(&turn.loop_id);
        self.completions
            .retain(|entry| entry.loop_id != turn.loop_id);
        self.completions.push_back(CompletionMark {
            loop_id: turn.loop_id.clone(),
            seq: turn.seq,
            canonical,
        });
        // Failed/partial reads must not evict the canonical markers that
        // suppress late deltas. The union stays within two identity budgets.
        let recent = self.completions.len().saturating_sub(COMPLETIONS);
        let mut canonical = 0;
        for entry in self.completions.iter_mut().rev() {
            if entry.canonical {
                canonical += 1;
                entry.canonical = canonical <= COMPLETIONS;
            }
        }
        let mut index = 0;
        self.completions.retain(|entry| {
            let keep = index >= recent || entry.canonical;
            index += 1;
            keep
        });
    }

    /// Whether a turn's body is already authoritative.
    pub fn is_closed(&self, loop_id: &str) -> bool {
        self.completions
            .iter()
            .any(|entry| entry.loop_id == loop_id && entry.canonical)
    }

    /// Replaces a streamed body before its terminal and returns the replaced range and growth.
    pub fn reconcile_turn(
        &mut self,
        loop_id: &str,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
    ) -> Option<(usize, isize, usize)> {
        let at = self
            .reconcile_from
            .iter()
            .position(|(id, _)| id == loop_id)?;
        let start = self.reconcile_from[at].1;
        // Keep the terminal's wait and saving facts, along with any successor
        // already streaming beyond it.
        let end = self.transcript.blocks()[start..]
            .iter()
            .position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id))
            .map(|at| start + at)?;
        self.reconcile_from.remove(at);
        let durable = self.canonical_body(loop_id, items, interrupted);
        Some(self.replace_turn_body(start..end, durable))
    }

    fn canonical_body(
        &self,
        loop_id: &str,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
    ) -> Transcript {
        let mut body = Transcript::default();
        for item in items {
            append_history_item(&mut body, item, &self.info.session_id);
        }
        if let Some(partial) = interrupted {
            push_interrupted(&mut body, loop_id, partial);
        }
        body
    }

    fn replace_turn_body(
        &mut self,
        range: std::ops::Range<usize>,
        body: Transcript,
    ) -> (usize, isize, usize) {
        let merged = Transcript::merged(body, &self.transcript, range.clone());
        let grown = merged.blocks.len() as isize - range.len() as isize;
        self.transcript.splice_merged(range.clone(), merged);
        self.rebase_range(range.start, range.end, grown);
        (range.start, grown, range.end)
    }
}
