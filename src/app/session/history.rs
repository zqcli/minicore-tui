//! Saved history windows, retained turn regions and compaction markers.

use std::collections::BTreeSet;

use super::{Session, turn_bounds};
use crate::app::read::History;
use crate::protocol::read::{RuntimeItem, TurnRecord, UserKind};
use crate::protocol::{
    CompactStatus, CompactionResult, SessionContext, ToolRef, Usage, UsageTotals,
};
use crate::transcript::{Block, Transcript, append_history_item};

impl Session {
    /// The source items not loaded as themselves. The summary stands in for
    /// the item at the display floor, which still has to be read.
    pub fn hidden_items(&self) -> usize {
        self.loaded_from + usize::from(self.loaded_from == self.first_item)
    }

    /// Replaces the transcript with the history tail.
    pub fn install_tail(&mut self, page: History) -> Vec<(std::ops::Range<usize>, usize, bool)> {
        let old = std::mem::take(&mut self.transcript);
        let pending = std::mem::take(&mut self.reconcile_from);
        let mut regions = pending.clone();
        let mut from = 0;
        for (at, block) in old.blocks().iter().enumerate() {
            if let Block::TurnEnd { loop_id, .. } = block {
                if !regions.iter().any(|(id, _)| id == loop_id) {
                    regions.push((loop_id.clone(), from));
                }
                from = at + 1;
            }
        }
        if let Some(live) = &self.live
            && !regions.iter().any(|(id, _)| id == &live.turn.loop_id)
        {
            regions.push((live.turn.loop_id.clone(), live.start));
        }
        regions.sort_by_key(|(_, start)| *start);
        let mut full_turns = BTreeSet::new();
        let mut bounded_end = page.end == page.pin.total;
        for item in page.items.iter().rev() {
            if let RuntimeItem::User(user) = &item.item
                && user.kind == UserKind::Prompt
            {
                if bounded_end
                    && page
                        .records
                        .iter()
                        .any(|record| record.loop_id == user.loop_id)
                {
                    full_turns.insert(user.loop_id.clone());
                }
                bounded_end = true;
            }
        }
        if !page.canonical {
            self.first_item = page.pin.first_item;
        }
        if page.from <= self.loaded_end && self.loaded_from <= page.end {
            self.loaded_from = self.loaded_from.min(page.from);
            self.loaded_end = self.loaded_end.max(page.end);
        } else {
            self.loaded_from = page.from;
            self.loaded_end = page.end;
        }
        self.transcript =
            Transcript::from_history(&page.items, &page.records, &self.info.session_id, page.from)
                .with_previous_history(&old, page.from..page.end);
        for item in &page.items {
            if matches!(item.item, RuntimeItem::ToolResult(_)) {
                append_history_item(&mut self.transcript, item, &self.info.session_id);
            }
        }
        // With the whole display window loaded, its turns and the summary's
        // cover are every turn there is; anything less would be a wrong total.
        let covered = match page.pin.first_item {
            0 => Some(Usage::default()),
            _ => page
                .pin
                .covered_usage
                .as_ref()
                .filter(|covered| !covered.partial)
                .map(|covered| covered.usage.clone()),
        };
        if page.from <= page.pin.first_item
            && self.usage == UsageTotals::default()
            && let Some(mut total) = covered
        {
            for record in &page.records {
                total.add(&record.usage);
            }
            self.usage.add(&total);
        }
        if let Some(coverage) = self.summary_coverage {
            self.cover_replayed_compaction(coverage);
        }
        // `from` counts even when an automatic compaction covers the whole
        // display tail: the history behind it is still there to read.
        if self.loaded_from > 0 {
            let hidden_items = self.hidden_items();
            self.transcript
                .insert(0, Block::EarlierHistory { hidden_items });
        }
        let mut placements: Vec<(String, usize)> = Vec::new();
        for (index, (loop_id, start)) in regions.iter().enumerate() {
            let end = regions
                .get(index + 1)
                .map_or(old.len(), |(_, start)| *start);
            let existing = turn_bounds(self.transcript.blocks(), loop_id);
            if (full_turns.contains(loop_id)
                || (*start..end).any(|at| old.history_source(at).is_some()))
                && let Some(range) = existing
            {
                let mut durable = Transcript::default();
                durable.splice_copy(0..0, &self.transcript, range.clone());
                let merged = Transcript::merged(durable, &old, *start..end);
                let delta = merged.blocks.len() as isize - range.len() as isize;
                for (_, at) in &mut placements {
                    if *at >= range.end {
                        *at = at.saturating_add_signed(delta);
                    }
                }
                placements.push((loop_id.clone(), range.start));
                self.transcript.splice_merged(range, merged);
            } else {
                if let Some(range) = existing {
                    for (_, at) in &mut placements {
                        if *at >= range.end {
                            *at -= range.len();
                        }
                    }
                    self.transcript.splice(range, Vec::new());
                }
                let description = self
                    .catalog
                    .entries
                    .iter()
                    .find(|turn| turn.loop_id == *loop_id)
                    .cloned()
                    .or_else(|| {
                        self.recovery
                            .entries
                            .values()
                            .find(|entry| entry.turn.loop_id == *loop_id)
                            .map(|entry| entry.turn.clone())
                    });
                let at = if self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.turn.loop_id == *loop_id)
                {
                    self.transcript.len()
                } else if let Some(turn) = description {
                    self.completion_position(&turn).unwrap_or_else(|| {
                        self.ordering_gap = true;
                        self.transcript.len()
                    })
                } else {
                    // A retained earlier region is not covered by a tail page.
                    placements
                        .last()
                        .and_then(|(id, _)| {
                            turn_bounds(self.transcript.blocks(), id).map(|range| range.end)
                        })
                        .unwrap_or_else(|| {
                            self.transcript
                                .blocks()
                                .iter()
                                .position(|block| !matches!(block, Block::EarlierHistory { .. }))
                                .unwrap_or(self.transcript.len())
                        })
                };
                for (_, placed) in &mut placements {
                    if *placed >= at {
                        *placed += end - *start;
                    }
                }
                placements.push((loop_id.clone(), at));
                self.transcript.splice_copy(at..at, &old, *start..end);
            }
        }
        if regions.is_empty() && !old.is_empty() {
            let durable = std::mem::take(&mut self.transcript);
            let merged = Transcript::merged(durable, &old, 0..old.len());
            self.transcript.splice_merged(0..0, merged);
        }
        let mut mapping = Vec::new();
        for (index, (loop_id, start)) in regions.iter().enumerate() {
            let end = regions
                .get(index + 1)
                .map_or(old.len(), |(_, start)| *start);
            let new_start = placements
                .iter()
                .find(|(id, _)| id == loop_id)
                .expect("every retained region was placed")
                .1;
            if let Some(live) = self.tracked_turn_mut(loop_id) {
                live.start = new_start;
            }
            mapping.push((*start..end, new_start, full_turns.contains(loop_id)));
        }
        self.reconcile_from = pending
            .into_iter()
            .map(|(id, _)| {
                let start = placements
                    .iter()
                    .find(|(placed, _)| placed == &id)
                    .expect("every pending region was placed")
                    .1;
                (id, start)
            })
            .collect();
        mapping
    }

    /// Prepends an older page and returns its block growth and cards needing detail reads.
    pub fn prepend_page(&mut self, page: History) -> (isize, Vec<ToolRef>) {
        self.loading_older = false;
        // A turn cut by the page's end already has its line from the later window.
        let records: Vec<TurnRecord> = page
            .records
            .iter()
            .filter(|record| {
                !self.transcript.blocks().iter().any(
                    |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == record.loop_id),
                )
            })
            .cloned()
            .collect();
        let scratch =
            Transcript::from_history(&page.items, &records, &self.info.session_id, page.from);
        let bare_cards = match page.canonical {
            true => scratch.tool_calls().map(|call| call.key.clone()).collect(),
            false => Vec::new(),
        };
        let added = scratch.len();
        let replaced = usize::from(matches!(
            self.transcript.blocks().first(),
            Some(Block::EarlierHistory { .. })
        ));
        self.transcript.remove_front(replaced);
        self.transcript.splice_transcript(0..0, scratch);
        let exhausted = added == 0 && (page.canonical || replaced == 0);
        // A compaction can raise the display floor above the loaded rows; the
        // read position never moves past them, or the items between are lost.
        self.loaded_from = match exhausted {
            true => 0,
            false => page.from.min(self.loaded_from),
        };
        if !page.canonical {
            self.first_item = page.pin.first_item;
        }
        let marker = self.loaded_from > 0
            && (page.canonical
                || (page.from > page.pin.first_item)
                || self.loaded_from <= self.first_item);
        if marker {
            let hidden_items = self.hidden_items();
            self.transcript
                .insert(0, Block::EarlierHistory { hidden_items });
        }
        let grown = added as isize - replaced as isize + isize::from(marker);
        self.rebase(0, grown);
        (grown, bare_cards)
    }

    /// Applies context and compaction coverage, returning a newly reported automatic failure.
    pub fn apply_context(&mut self, context: SessionContext) -> Option<String> {
        let automatic = context.automatic.as_ref();
        // The first report only sets the baseline: an earlier attempt is
        // already in the history that was read.
        let mut failure = None;
        if let Some(last) = automatic.and_then(|view| view.last.as_ref())
            && self.auto_seen.replace(last.operation_id.clone()).as_ref()
                != Some(&last.operation_id)
            && self.context.is_some()
        {
            if last.outcome == "compacted" {
                let result = CompactionResult {
                    operation_id: last.operation_id.clone(),
                    status: CompactStatus::Compacted,
                    before_tokens: last.before_tokens,
                    after_tokens: last.after_tokens,
                    covered_loop_count: None,
                    covered_item_count: None,
                    retained_item_count: None,
                    failure_kind: None,
                };
                self.push_compaction(&result, None);
            } else if !last.outcome.starts_with("compacted") && last.outcome != "cancelled" {
                failure = Some(last.outcome.clone());
            }
        }
        let coverage = context
            .coverage
            .as_ref()
            .filter(|coverage| coverage.covered_item_count > 0)
            .map(|coverage| (coverage.covered_item_count, coverage.retained_item_count));
        if let Some(result) = &context.last_result {
            self.push_compaction(result, coverage);
        }
        if let Some(coverage) = coverage {
            self.cover_replayed_compaction(coverage);
        }
        self.context = Some(context);
        failure
    }

    /// Adds the one-line compaction marker, once per operation: the manual
    /// response and later context reads can report the same compaction.
    pub fn push_compaction(&mut self, result: &CompactionResult, coverage: Option<(usize, usize)>) {
        let known = self.transcript.blocks().iter().any(|block| {
            matches!(block, Block::Compaction { operation_id, .. } if *operation_id == result.operation_id)
        });
        if result.status != CompactStatus::Compacted || known {
            return;
        }
        self.transcript.push(Block::Compaction {
            operation_id: result.operation_id.clone(),
            before: result.before_tokens,
            after: result.after_tokens,
            coverage: result
                .covered_item_count
                .map(|covered| (Some(covered), result.retained_item_count))
                .or(coverage.map(|(covered, kept)| (Some(covered), Some(kept)))),
            summary: None,
        });
    }

    /// Gives the summary replayed from history the counts `session.context`
    /// reports for it.
    fn cover_replayed_compaction(&mut self, coverage: (usize, usize)) {
        self.summary_coverage = Some(coverage);
        let index = self.transcript.blocks().iter().rposition(|block| {
            matches!(block, Block::Compaction { operation_id, coverage: None, .. } if operation_id.is_empty())
        });
        if let Some(Block::Compaction { coverage: slot, .. }) =
            index.and_then(|index| self.transcript.block_mut(index))
        {
            *slot = Some((Some(coverage.0), Some(coverage.1)));
        }
    }
}
