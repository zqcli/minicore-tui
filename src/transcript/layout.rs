//! Per-block line cache, prefix heights, viewport and scroll anchoring.
//!
//! A frame renders only the visible window: the first visible block comes from
//! a binary search over prefix heights, and each block's lines come from its
//! cache. A block re-renders only when its own revision, the width or the fold
//! flags change, so a streaming delta re-renders exactly one block.

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

/// Ordered blocks plus their render cache and prefix heights.
#[derive(Default)]
pub struct Layout {
    cached: Vec<Cached>,
    /// `prefix[i]` is the first screen row of block `i`; `prefix[blocks.len()]`
    /// is the total height. Empty until the first successful prepare.
    prefix: Vec<usize>,
    width: u16,
    fold: Option<Fold>,
    /// Set when the block vector no longer lines up with the cache.
    invalid: bool,
}

impl Layout {
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
        F: FnMut(usize, &Block) -> Vec<Line<'static>>,
    {
        if self.width != width || self.fold != Some(fold) {
            self.width = width;
            self.fold = Some(fold);
            self.cached.clear();
            self.invalid = true;
        }
        if self.cached.len() > blocks.len() {
            // Blocks were removed or spliced; positions after the change are
            // no longer comparable.
            self.cached.truncate(blocks.len());
            self.invalid = true;
        }
        let mut first_changed = usize::MAX;
        for (index, block) in blocks.iter().enumerate() {
            match self.cached.get(index) {
                Some(cached) if !self.invalid && cached.revision == revisions[index] => {}
                _ => {
                    let lines: Arc<[Line<'static>]> = render_block(index, block).into();
                    let entry = Cached {
                        revision: revisions[index],
                        lines,
                    };
                    match self.cached.get_mut(index) {
                        Some(cached) => *cached = entry,
                        None => self.cached.push(entry),
                    }
                    first_changed = first_changed.min(index);
                }
            }
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

    pub fn lines(&self, block: usize) -> &[Line<'static>] {
        self.cached
            .get(block)
            .map_or(&[], |cached| cached.lines.as_ref())
    }

    pub fn height(&self, block: usize) -> usize {
        self.cached
            .get(block)
            .map_or(0, |cached| cached.lines.len())
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
        (block, offset as u16)
    }

    /// The screen row an anchor lands on in the current layout.
    pub fn anchor_row(&self, block: usize, offset: u16) -> usize {
        self.row_of(block.min(self.cached.len().saturating_sub(1))) + offset as usize
    }

    /// Maps a screen row to the block it belongs to and the line inside it.
    pub fn line_at(&self, row: usize) -> Option<(usize, usize)> {
        if self.cached.is_empty() || row >= self.total_rows() {
            return None;
        }
        Some(self.block_at(row))
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
        let mut layout = Layout::default();
        let blocks = [user("a"), user("b"), user("c")];
        let revisions = [1, 2, 3];
        let mut rendered = vec![];
        layout.prepare(&blocks, &revisions, 40, fold(), |index, _| {
            rendered.push(index);
            lines(2)
        });
        assert_eq!(rendered, vec![0, 1, 2]);
        rendered.clear();
        layout.prepare(&blocks, &revisions, 40, fold(), |index, _| {
            rendered.push(index);
            lines(2)
        });
        assert!(rendered.is_empty(), "nothing changed, nothing re-rendered");
        assert_eq!(layout.total_rows(), 6);
    }

    #[test]
    fn only_the_changed_block_re_renders() {
        let mut layout = Layout::default();
        let blocks = [user("a"), user("b")];
        let revisions = [1, 2];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _| lines(2));
        let revisions = [1, 9];
        let mut rendered = vec![];
        layout.prepare(&blocks, &revisions, 40, fold(), |index, _| {
            rendered.push(index);
            lines(3)
        });
        assert_eq!(rendered, vec![1]);
        assert_eq!(layout.total_rows(), 5);
        assert_eq!(layout.row_of(1), 2);
    }

    #[test]
    fn width_and_fold_changes_invalidate_the_cache() {
        let mut layout = Layout::default();
        let blocks = [user("a")];
        let revisions = [1];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _| lines(1));
        let mut count = 0;
        layout.prepare(&blocks, &revisions, 50, fold(), |_, _| {
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
            |_, _| {
                count += 1;
                lines(1)
            },
        );
        assert_eq!(count, 2);
    }

    #[test]
    fn appending_a_block_extends_the_prefix() {
        let mut layout = Layout::default();
        let mut blocks = vec![user("a")];
        let mut revisions = vec![1];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _| lines(2));
        blocks.push(user("b"));
        revisions.push(2);
        let mut rendered = vec![];
        layout.prepare(&blocks, &revisions, 40, fold(), |index, _| {
            rendered.push(index);
            lines(4)
        });
        assert_eq!(rendered, vec![1]);
        assert_eq!(layout.total_rows(), 6);
        assert_eq!(layout.row_of(1), 2);
    }

    #[test]
    fn block_lookup_and_anchor_agree() {
        let mut layout = Layout::default();
        let blocks = [user("a"), user("b"), user("c")];
        let revisions = [1, 2, 3];
        layout.prepare(&blocks, &revisions, 40, fold(), |_, _| lines(3));
        assert_eq!(layout.block_at(0), (0, 0));
        assert_eq!(layout.block_at(2), (0, 2));
        assert_eq!(layout.block_at(3), (1, 0));
        assert_eq!(layout.block_at(8), (2, 2));
        let (block, offset) = layout.anchor(4);
        assert_eq!((block, offset), (1, 1));
        assert_eq!(layout.anchor_row(block, offset), 4);
        assert_eq!(layout.line_at(9), None);
    }
}
