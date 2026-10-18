//! Per-block line cache, prefix heights, viewport and scroll anchoring.
//!
//! A frame renders only the visible window: the first visible block comes from
//! a binary search over prefix heights, and each block's lines come from its
//! cache. A block re-renders only when its own revision, the width or the fold
//! flags change, so a streaming delta re-renders exactly one block.
//!
//! `S` is renderer state the UI keeps per layout (the streaming markdown
//! parsers). The cache never inspects it; it is dropped and reset whenever the
//! width or the fold flags invalidate the whole cache.

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
    /// Renderer state owned by the caller.
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

    pub fn len(&self) -> usize {
        self.cached.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cached.is_empty()
    }

    pub fn lines(&self, block: usize) -> &[Line<'static>] {
        self.cached
            .get(block)
            .map_or(&[], |cached| cached.lines.as_ref())
    }

    /// Height of a block in rows.
    pub fn height(&self, block: usize) -> usize {
        self.cached
            .get(block)
            .map_or(0, |cached| cached.lines.len().max(1))
    }

    /// First screen row of `block`.
    pub fn row_of(&self, block: usize) -> usize {
        self.prefix
            .get(block)
            .copied()
            .unwrap_or_else(|| self.total_rows())
    }

    /// The block containing `row`, and the row's offset inside it.
    pub fn block_at(&self, row: usize) -> (usize, usize) {
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
    pub fn line_at(&self, row: usize) -> Option<(usize, usize)> {
        if self.cached.is_empty() || row >= self.total_rows() {
            return None;
        }
        Some(self.block_at(row))
    }

    /// One row of a block, if it is cached.
    pub fn line(&self, block: usize, offset: usize) -> Option<&Line<'static>> {
        self.lines(block).get(offset)
    }

    /// The layout row under an absolute screen cell.
    pub fn row_at(&self, column: u16, row: u16) -> Option<usize> {
        if row < self.viewport.origin.1 {
            return None;
        }
        if column < self.viewport.origin.0 {
            return None;
        }
        let offset = (row - self.viewport.origin.1) as usize;
        if offset >= self.viewport.height {
            return None;
        }
        let row = self.viewport.top + offset;
        (row < self.total_rows()).then_some(row)
    }

    /// Cell column of an absolute screen column inside a layout row.
    pub fn cell_at(&self, column: u16) -> usize {
        column.saturating_sub(self.viewport.origin.0) as usize
    }

    /// Rows of the visible window, top row first.
    pub fn visible(&self, top: usize, height: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(height);
        let mut row = top;
        while lines.len() < height && row < self.total_rows() {
            let (block, offset) = self.block_at(row);
            lines.push(
                self.line(block, offset)
                    .cloned()
                    .unwrap_or_else(|| Line::raw("")),
            );
            row += 1;
        }
        lines
    }

    /// Plain text of a screen row, for selection copy.
    pub fn row_text(&self, row: usize) -> Option<&Line<'static>> {
        let (block, offset) = self.line_at(row)?;
        self.line(block, offset)
    }

    /// The transcript text between two absolute cells. The two cells are
    /// ordered by row and then column, so dragging in either direction copies
    /// the same cells. Cells outside the drawn window are clamped into it, so a
    /// drag that leaves the screen still copies a contiguous range. Rows are
    /// joined with newlines, the way a clipboard paste reads.
    pub fn selection_text(&self, from: (u16, u16), to: (u16, u16)) -> String {
        let (first, last) = if from.0 <= to.0 {
            (from, to)
        } else {
            (to, from)
        };
        let Some(first_row) = self.row_at(0, first.0) else {
            return String::new();
        };
        let last_row = self.row_at(0, last.0).unwrap_or(first_row);
        let mut out = String::new();
        for row in first_row..=last_row {
            let Some(line) = self.row_text(row) else {
                continue;
            };
            let row = row as u16;
            let text = line.to_string();
            let start = if row == first_row as u16 {
                self.cell_at(first.1)
            } else {
                0
            };
            let end = if row == last_row as u16 {
                self.cell_at(last.1).max(1)
            } else {
                usize::MAX
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&slice_cells(&text, start, end));
        }
        out
    }
}

/// Cuts cells `[start, end)` out of a row of text.
fn slice_cells(text: &str, start: usize, end: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut column = 0;
    for character in text.chars() {
        if column >= end {
            break;
        }
        if column >= start {
            out.push(character);
        }
        column += UnicodeWidthChar::width(character).unwrap_or(0);
    }
    out.trim_end().to_owned()
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
    fn selection_copy_joins_rows_and_respects_cell_bounds() {
        let mut layout = Layout::<()>::default();
        let blocks = [user("alpha"), user("beta")];
        let revisions = [1, 2];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, index, _| {
            if index == 0 {
                vec![Line::raw("hello world"), Line::raw("second")]
            } else {
                vec![Line::raw("third")]
            }
        });
        layout.viewport = Viewport {
            top: 0,
            height: 4,
            origin: (0, 0),
        };
        assert_eq!(layout.selection_text((0, 6), (0, 11)), "world");
        assert_eq!(
            layout.selection_text((0, 6), (3, 5)),
            "world\nsecond\n\nthird",
            "the last row stops at the release column"
        );
        assert_eq!(
            layout.selection_text((1, 0), (0, 5)),
            " world\ns",
            "the drag copies from the earlier cell to the later one"
        );
        assert_eq!(
            layout.selection_text((0, 0), (0, 5)),
            "hello",
            "a single-row selection stops at the release column"
        );
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
