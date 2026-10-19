//! Per-block line cache, prefix heights, viewport and scroll anchoring. A block
//! re-renders only when its revision, the width or the fold flags change, and a
//! frame reads only the visible rows.

use std::sync::Arc;

use ratatui::text::Line;

use super::Block;

/// The render inputs that invalidate the whole cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub expanded_tools: bool,
    pub show_reasoning: bool,
}

#[derive(Clone, Debug)]
struct Cached {
    revision: u64,
    lines: Arc<[Line<'static>]>,
}

/// Where the transcript was last drawn: absolute screen rows map to layout
/// rows through this, which is what mouse selection hit-tests against.
#[derive(Clone, Copy, Debug, Default)]
pub struct Viewport {
    /// First layout row on screen.
    pub top: usize,
    pub height: usize,
    /// Screen cell of layout row 0, column 0.
    pub origin: (u16, u16),
}

/// Ordered blocks plus their render cache and prefix heights.
pub struct Layout<S = ()> {
    cached: Vec<Cached>,
    /// `prefix[i]` is the first screen row of block `i`; `prefix[blocks.len()]`
    /// is the total height. Empty until the first successful prepare.
    prefix: Vec<usize>,
    width: u16,
    fold: Option<Fold>,
    /// Set when the block vector no longer lines up with the cache.
    invalid: bool,
    /// Renderer state the cache hands to the render callback, never inspects.
    pub state: S,
    /// The last drawn window, for selection and scrolling.
    pub viewport: Viewport,
}

impl<S: Default> Default for Layout<S> {
    fn default() -> Self {
        Self {
            cached: Vec::new(),
            prefix: Vec::new(),
            width: 0,
            fold: None,
            invalid: true,
            state: S::default(),
            viewport: Viewport::default(),
        }
    }
}

impl<S> Layout<S> {
    /// Brings the cache up to date. `render_block` runs only for blocks that
    /// changed or are new.
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
        if self.width != width || self.fold != Some(fold) {
            self.width = width;
            self.fold = Some(fold);
            self.cached.clear();
            self.invalid = true;
        }
        if self.cached.len() > blocks.len() {
            // Blocks were removed or spliced; positions after the change are no
            // longer comparable.
            self.cached.truncate(blocks.len());
            self.invalid = true;
        }
        let mut first_changed = usize::MAX;
        for (index, block) in blocks.iter().enumerate() {
            let unchanged = self
                .cached
                .get(index)
                .is_some_and(|cached| !self.invalid && cached.revision == revisions[index]);
            if unchanged {
                continue;
            }
            let lines = render_block(&mut self.state, index, block);
            // One blank row separates blocks. It belongs to the block below so
            // the last block does not leave a trailing gap, and a block that
            // renders nothing still occupies its row.
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
            let entry = Cached {
                revision: revisions[index],
                lines: lines.into(),
            };
            match self.cached.get_mut(index) {
                Some(cached) => *cached = entry,
                None => self.cached.push(entry),
            }
            first_changed = first_changed.min(index);
        }
        if self.invalid || self.prefix.len() != self.cached.len() + 1 || first_changed == 0 {
            self.rebuild_prefix(0);
            self.invalid = false;
        } else if first_changed != usize::MAX {
            self.rebuild_prefix(first_changed);
        }
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

    /// The block under an absolute screen cell and the index of the clicked
    /// row among the block's own lines. `None` off the drawn window and on the
    /// blank row that separates a block from the one above it.
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
            expanded_tools: false,
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

    #[test]
    fn width_and_fold_changes_invalidate_the_cache() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("a")];
        let revisions = [1];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _, _| lines(1));
        let mut count = 0;
        layout.prepare(&blocks, &revisions, 50, fold(), |_, _, _| {
            count += 1;
            lines(1)
        });
        assert_eq!(count, 1);
        layout.prepare(
            &blocks,
            &revisions,
            50,
            Fold {
                expanded_tools: true,
                show_reasoning: true,
            },
            |_, _, _| {
                count += 1;
                lines(1)
            },
        );
        assert_eq!(count, 2);
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
