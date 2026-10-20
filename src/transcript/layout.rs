//! Per-block line cache, prefix heights, viewport and scroll anchoring. A block
//! re-renders only when its revision, the width or a fold flag it depends on
//! changes, and a frame reads only the visible rows.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::Block;

/// The render inputs that only some blocks depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    /// Ctrl+O: tool cards, thinking and compaction summaries open.
    pub expanded: bool,
    /// Off under Ctrl+T, which leaves one `Thinking...` row per block.
    pub show_reasoning: bool,
}

/// Which fold flags a block's rows were built from.
#[derive(Clone, Copy, Debug)]
struct Uses {
    expanded: bool,
    show_reasoning: bool,
}

impl Uses {
    fn of(block: &Block) -> Self {
        match block {
            Block::Assistant {
                reasoning,
                expanded,
                ..
            } => {
                let thinking = !reasoning.trim().is_empty();
                Self {
                    expanded: thinking && expanded.is_none(),
                    show_reasoning: thinking,
                }
            }
            Block::Tool(call) => Self {
                expanded: call.expanded.is_none(),
                show_reasoning: false,
            },
            Block::Compaction { .. } => Self {
                expanded: true,
                show_reasoning: false,
            },
            _ => Self {
                expanded: false,
                show_reasoning: false,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct Cached {
    revision: u64,
    /// The fold the rows were built under, and the part of it they read.
    fold: Fold,
    uses: Uses,
    lines: Arc<[Line<'static>]>,
}

impl Cached {
    fn is_current(&self, revision: u64, fold: Fold) -> bool {
        self.revision == revision
            && !(self.uses.expanded && self.fold.expanded != fold.expanded)
            && !(self.uses.show_reasoning && self.fold.show_reasoning != fold.show_reasoning)
    }
}

/// Work done by `prepare`, for the exit report under `MINICORE_TUI_PERF_STATS`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub blocks_rendered: u64,
    pub rows_rendered: u64,
    pub prepare_max: Duration,
}

/// Where the transcript was last drawn: absolute screen rows map to layout
/// rows through this, which is what mouse selection hit-tests against.
#[derive(Clone, Copy, Debug, Default)]
pub struct Viewport {
    /// First layout row on screen.
    pub top: usize,
    pub height: usize,
    pub width: u16,
    /// Screen cell of layout row 0, column 0.
    pub origin: (u16, u16),
}

impl Viewport {
    /// The screen area the transcript was drawn in.
    pub fn area(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, self.width, self.height as u16)
    }
}

/// Ordered blocks plus their render cache and prefix heights.
pub struct Layout<S = ()> {
    cached: Vec<Cached>,
    /// `prefix[i]` is the first screen row of block `i`; `prefix[blocks.len()]`
    /// is the total height. Empty until the first successful prepare.
    prefix: Vec<usize>,
    width: u16,
    /// Renderer state the cache hands to the render callback, never inspects.
    pub state: S,
    /// The last drawn window, for selection and scrolling.
    pub viewport: Viewport,
    pub stats: Stats,
}

impl<S: Default> Default for Layout<S> {
    fn default() -> Self {
        Self {
            cached: Vec::new(),
            prefix: Vec::new(),
            width: 0,
            state: S::default(),
            viewport: Viewport::default(),
            stats: Stats::default(),
        }
    }
}

impl<S> Layout<S> {
    /// Drops every cached block, for changes the cache key cannot see (theme, a
    /// different session).
    pub fn clear(&mut self) {
        self.cached.clear();
        self.prefix.clear();
    }

    /// Brings the cache up to date. `render_block` runs only for blocks that
    /// are new or changed. A block is found again by its revision wherever it
    /// moved to, except across the first position, which carries no separator.
    pub fn prepare<F>(
        &mut self,
        blocks: &[Block],
        revisions: &[u64],
        width: u16,
        fold: Fold,
        mut render_block: F,
    ) where
        F: FnMut(&mut S, usize, &Block) -> Vec<Line<'static>>,
    {
        let started = Instant::now();
        if self.width != width {
            self.width = width;
            self.clear();
        }
        let kept = self
            .cached
            .iter()
            .zip(revisions)
            .take_while(|(cached, revision)| cached.is_current(**revision, fold))
            .count();
        let moved = self.cached.split_off(kept);
        let by_revision: HashMap<u64, &Cached> = moved
            .iter()
            .enumerate()
            .filter(|(offset, _)| kept + offset > 0)
            .map(|(_, cached)| (cached.revision, cached))
            .collect();
        for (index, block) in blocks.iter().enumerate().skip(kept) {
            let revision = revisions[index];
            let reused = by_revision
                .get(&revision)
                .filter(|cached| index > 0 && cached.is_current(revision, fold));
            if let Some(cached) = reused {
                self.cached.push((*cached).clone());
                continue;
            }
            let lines = render_block(&mut self.state, index, block);
            // The separator row belongs to the block below, so the last block
            // leaves no trailing gap; an empty block still occupies a row.
            let mut lines: Vec<Line<'static>> = if index == 0 {
                lines
            } else {
                let mut spaced = Vec::with_capacity(lines.len() + 1);
                spaced.push(Line::raw(""));
                spaced.extend(lines);
                spaced
            };
            if lines.is_empty() {
                lines.push(Line::raw(""));
            }
            self.stats.blocks_rendered += 1;
            self.stats.rows_rendered += lines.len() as u64;
            self.cached.push(Cached {
                revision,
                fold,
                uses: Uses::of(block),
                lines: lines.into(),
            });
        }
        if kept < blocks.len() || self.prefix.len() != blocks.len() + 1 {
            self.rebuild_prefix(kept);
        }
        self.stats.prepare_max = self.stats.prepare_max.max(started.elapsed());
    }

    fn rebuild_prefix(&mut self, from: usize) {
        if from == 0 {
            self.prefix.clear();
            self.prefix.push(0);
        } else {
            self.prefix.truncate(from + 1);
        }
        let mut row = *self.prefix.last().unwrap_or(&0);
        for cached in &self.cached[from..] {
            row += cached.lines.len().max(1);
            self.prefix.push(row);
        }
    }

    /// Total rendered height across all blocks.
    pub fn total_rows(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    fn len(&self) -> usize {
        self.cached.len()
    }

    fn lines(&self, block: usize) -> &[Line<'static>] {
        self.cached
            .get(block)
            .map_or(&[], |cached| cached.lines.as_ref())
    }

    /// Height of a block in rows.
    fn height(&self, block: usize) -> usize {
        self.cached
            .get(block)
            .map_or(0, |cached| cached.lines.len().max(1))
    }

    /// First screen row of `block`.
    fn row_of(&self, block: usize) -> usize {
        self.prefix
            .get(block)
            .copied()
            .unwrap_or_else(|| self.total_rows())
    }

    /// The block containing `row`, and the row's offset inside it.
    fn block_at(&self, row: usize) -> (usize, usize) {
        if self.cached.is_empty() {
            return (0, 0);
        }
        let index = self
            .prefix
            .partition_point(|start| *start <= row)
            .saturating_sub(1)
            .min(self.cached.len() - 1);
        (index, row.saturating_sub(self.prefix[index]))
    }

    /// The block and intra-block row to keep under the viewport top.
    pub fn anchor(&self, top_row: usize) -> (usize, u16) {
        let (block, offset) = self.block_at(top_row);
        (block, offset.min(u16::MAX as usize) as u16)
    }

    /// The screen row an anchor lands on. An offset past the end of a block that
    /// shrank stays on the block's last row.
    pub fn anchor_row(&self, block: usize, offset: u16) -> usize {
        let block = block.min(self.len().saturating_sub(1));
        let offset = (offset as usize).min(self.height(block).saturating_sub(1));
        self.row_of(block) + offset
    }

    /// Maps a screen row to the block it belongs to and the line inside it.
    fn line_at(&self, row: usize) -> Option<(usize, usize)> {
        if self.cached.is_empty() || row >= self.total_rows() {
            return None;
        }
        Some(self.block_at(row))
    }

    /// The layout row under an absolute screen cell.
    fn row_at(&self, column: u16, row: u16) -> Option<usize> {
        let (left, top) = self.viewport.origin;
        let offset = row.checked_sub(top)? as usize;
        if column < left || offset >= self.viewport.height {
            return None;
        }
        let row = self.viewport.top + offset;
        (row < self.total_rows()).then_some(row)
    }

    /// The block under a screen cell and the clicked line within it; `None` off
    /// the window and on a block's separator row.
    pub fn hit(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let (block, offset) = self.line_at(self.row_at(column, row)?)?;
        let line = if block == 0 {
            offset
        } else {
            offset.checked_sub(1)?
        };
        Some((block, line))
    }

    /// Rows of the visible window, top row first.
    pub fn visible(&self, top: usize, height: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(height);
        let mut row = top;
        while lines.len() < height && row < self.total_rows() {
            let (block, offset) = self.block_at(row);
            lines.push(self.lines(block)[offset].clone());
            row += 1;
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;

    fn user(text: &str) -> Block {
        Block::User {
            text: text.into(),
            kind: UserKind::Prompt,
        }
    }

    fn fold() -> Fold {
        Fold {
            expanded: false,
            show_reasoning: false,
        }
    }

    fn lines(height: usize) -> Vec<Line<'static>> {
        (0..height)
            .map(|index| Line::raw(format!("row {index}")))
            .collect()
    }

    #[test]
    fn unchanged_blocks_are_not_rendered_again() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b"), user("c")];
        let revisions = [1, 2, 3];
        let mut rendered: Vec<usize> = vec![];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            rendered.push(index);
            lines(2)
        });
        assert_eq!(rendered, vec![0, 1, 2]);
        rendered.clear();
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            rendered.push(index);
            lines(2)
        });
        assert!(rendered.is_empty(), "nothing changed, nothing re-rendered");
        assert_eq!(
            layout.total_rows(),
            8,
            "two separators between three blocks"
        );
    }

    #[test]
    fn only_the_changed_block_re_renders() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b")];
        let revisions = [1, 2];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _, _| lines(2));
        let revisions = [1, 9];
        let mut rendered: Vec<usize> = vec![];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            rendered.push(index);
            lines(3)
        });
        assert_eq!(rendered, vec![1]);
        assert_eq!(layout.total_rows(), 6);
        assert_eq!(layout.row_of(1), 2);
    }

    fn card(expanded: Option<bool>) -> Block {
        let mut call = crate::transcript::ToolCall::new(
            crate::protocol::ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            "bash",
        );
        call.expanded = expanded;
        Block::Tool(Box::new(call))
    }

    fn answer(reasoning: &str) -> Block {
        Block::Assistant {
            key: crate::transcript::RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: reasoning.into(),
            text: "reply".into(),
            streaming: false,
            expanded: None,
        }
    }

    fn rendered_by<S: Default>(
        layout: &mut Layout<S>,
        blocks: &[Block],
        revisions: &[u64],
        fold: Fold,
    ) -> Vec<usize> {
        let mut rendered = vec![];
        layout.prepare(blocks, revisions, 40, fold, |_, index, _| {
            rendered.push(index);
            lines(2)
        });
        rendered
    }

    #[test]
    fn a_width_change_renders_every_block_again() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b")];
        layout.prepare(&blocks, &[1, 2], 40, fold(), |_, _, _| lines(1));
        let mut count = 0;
        layout.prepare(&blocks, &[1, 2], 50, fold(), |_, _, _| {
            count += 1;
            lines(1)
        });
        assert_eq!(count, 2);
    }

    #[test]
    fn a_fold_flag_renders_only_the_blocks_that_read_it() {
        let mut layout = Layout::<()>::default();
        let blocks = [
            user("a"),
            card(None),
            card(Some(true)),
            answer("thinking"),
            answer(""),
        ];
        let revisions = [1, 2, 3, 4, 5];
        rendered_by(&mut layout, &blocks, &revisions, fold());
        let expanded = Fold {
            expanded: true,
            ..fold()
        };
        assert_eq!(
            rendered_by(&mut layout, &blocks, &revisions, expanded),
            [1, 3],
            "Ctrl+O reaches cards and thinking that follow it, not a pinned card"
        );
        let reasoning = Fold {
            show_reasoning: true,
            ..expanded
        };
        assert_eq!(
            rendered_by(&mut layout, &blocks, &revisions, reasoning),
            [3],
            "Ctrl+T reaches only thinking"
        );
    }

    #[test]
    fn a_prepended_page_keeps_the_rows_of_the_blocks_it_moved() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b"), user("c")];
        rendered_by(&mut layout, &blocks, &[1, 2, 3], fold());
        let blocks = [user("x"), user("y"), user("a"), user("b"), user("c")];
        assert_eq!(
            rendered_by(&mut layout, &blocks, &[7, 8, 1, 2, 3], fold()),
            [0, 1, 2],
            "the old first block gains a separator, the others are reused"
        );
        assert_eq!(layout.total_rows(), 5 * 2 + 4);
        assert_eq!(layout.row_of(3), 8);
    }

    #[test]
    fn dropping_the_front_renders_the_new_first_block_without_a_separator() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b"), user("c")];
        rendered_by(&mut layout, &blocks, &[1, 2, 3], fold());
        let blocks = [user("b"), user("c")];
        assert_eq!(rendered_by(&mut layout, &blocks, &[2, 3], fold()), [0]);
        assert_eq!(layout.total_rows(), 2 * 2 + 1);
        assert_eq!(layout.visible(0, 1)[0].to_string(), "row 0");
    }

    #[test]
    fn a_removed_block_in_the_middle_shifts_the_ones_after_it() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b"), user("c"), user("d")];
        rendered_by(&mut layout, &blocks, &[1, 2, 3, 4], fold());
        let blocks = [user("a"), user("c"), user("d")];
        assert!(rendered_by(&mut layout, &blocks, &[1, 3, 4], fold()).is_empty());
        assert_eq!(layout.total_rows(), 3 * 2 + 2);
        layout.prepare(&blocks[..1], &[1], 40, fold(), |_, _, _| lines(9));
        assert_eq!(
            layout.total_rows(),
            2,
            "a shorter transcript drops the tail"
        );
    }

    #[test]
    fn appending_a_block_extends_the_prefix() {
        let mut layout = Layout::<()>::default();
        let mut blocks = vec![user("a")];
        let mut revisions = vec![1];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _, _| lines(2));
        blocks.push(user("b"));
        revisions.push(2);
        let mut rendered: Vec<usize> = vec![];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            rendered.push(index);
            lines(4)
        });
        assert_eq!(rendered, vec![1]);
        assert_eq!(layout.total_rows(), 7);
        assert_eq!(layout.row_of(1), 2);
    }

    #[test]
    fn block_lookup_and_anchor_agree() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b"), user("c")];
        let revisions = [1, 2, 3];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _, _| lines(3));
        assert_eq!(layout.block_at(0), (0, 0));
        assert_eq!(layout.block_at(2), (0, 2));
        assert_eq!(
            layout.block_at(3),
            (1, 0),
            "the separator belongs to block 1"
        );
        assert_eq!(layout.block_at(4), (1, 1));
        assert_eq!(layout.block_at(8), (2, 1));
        let (block, offset) = layout.anchor(5);
        assert_eq!((block, offset), (1, 2));
        assert_eq!(layout.anchor_row(block, offset), 5);
        assert_eq!(layout.line_at(10), Some((2, 3)));
        assert_eq!(layout.line_at(11), None);
    }

    #[test]
    fn a_shrunk_block_keeps_the_anchor_on_its_last_row() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b")];
        let revisions = [1, 2];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            lines(if index == 1 { 10 } else { 1 })
        });
        assert_eq!(layout.anchor_row(1, 8), 9);
        let revisions = [1, 3];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _, _| lines(1));
        assert_eq!(
            layout.anchor_row(1, 8),
            2,
            "the anchor cannot point past the shrunken block"
        );
    }

    #[test]
    fn the_visible_window_reads_only_cached_rows() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b"), user("c")];
        let revisions = [1, 2, 3];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            lines(index + 1)
        });
        let window: Vec<String> = layout.visible(1, 2).iter().map(Line::to_string).collect();
        assert_eq!(window, ["", "row 0"], "the separator row starts block 1");
        assert_eq!(layout.visible(5, 4).len(), 3, "clipped at the last row");
    }

    #[test]
    fn a_click_maps_to_the_block_row_it_landed_on() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a"), user("b")];
        layout.prepare(&blocks, &[1, 2], 40, fold(), |_, _, _| lines(2));
        layout.viewport = Viewport {
            top: 1,
            height: 4,
            width: 40,
            origin: (0, 10),
        };
        assert_eq!(layout.hit(5, 10), Some((0, 1)));
        assert_eq!(layout.hit(5, 11), None, "the separator belongs to no row");
        assert_eq!(layout.hit(5, 12), Some((1, 0)), "the first row of block 1");
        assert_eq!(layout.hit(5, 13), Some((1, 1)));
        assert_eq!(layout.hit(5, 9), None, "above the window");
        assert_eq!(layout.hit(5, 14), None, "below the window");
    }

    #[test]
    fn renderer_state_is_reset_with_the_cache() {
        let mut layout = Layout::<usize>::default();
        let blocks = [user("a")];
        let revisions = [1];
        layout.prepare(&blocks, &revisions, 40, fold(), |state, _, _| {
            *state += 1;
            lines(1)
        });
        assert_eq!(layout.state, 1);
        layout.prepare(&blocks, &revisions, 40, fold(), |state, _, _| {
            *state += 1;
            lines(1)
        });
        assert_eq!(layout.state, 1, "an unchanged block is not re-rendered");
        layout.prepare(&blocks, &revisions, 44, fold(), |state, _, _| {
            *state += 1;
            lines(1)
        });
        assert_eq!(layout.state, 2);
    }
}
