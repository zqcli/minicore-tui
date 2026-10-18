//! The transcript view: layout cache, viewport and scroll resolution.
//!
//! One frame renders only the visible window: the first visible block comes
//! from a binary search over prefix heights and each row from the block's
//! cached lines, so the cost of a frame does not depend on history length.

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::render::RenderState;
use super::theme::Styles;
use crate::app::Scroll;
use crate::transcript::Block;
use crate::transcript::layout::{Fold, Layout, Viewport};

/// Everything a transcript frame needs.
pub struct Transcript<'a> {
    pub blocks: &'a [Block],
    pub revisions: &'a [u64],
    pub width: u16,
    pub fold: Fold,
}

/// Draws the transcript window and records where it landed.
pub fn draw<S>(
    cache: &mut Layout<S>,
    transcript: Transcript<'_>,
    area: Rect,
    scroll: Scroll,
    styles: Styles,
    render: impl FnMut(&mut S, usize, &Block) -> Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    cache.prepare(
        transcript.blocks,
        transcript.revisions,
        transcript.width,
        transcript.fold,
        render,
    );
    let total = cache.total_rows();
    let height = area.height as usize;
    let top = match scroll {
        Scroll::Follow => total.saturating_sub(height),
        // An anchored view must not move when content grows below it, so the
        // stored anchor is resolved against the current layout.
        Scroll::Anchor { block, offset } => cache.anchor_row(block, offset),
    }
    .min(total.saturating_sub(1));
    cache.viewport = Viewport {
        top,
        height,
        origin: (area.x, area.y),
    };
    let mut lines = cache.visible(top, height);
    // The pill replaces the last row while an anchored view has newer content
    // below it; it disappears the moment the view is at the bottom again.
    if scroll != Scroll::Follow
        && top + height < total
        && let Some(pill) = lines.last_mut()
    {
        *pill = Line::styled("↓ new output", styles.accent());
    }
    lines
}

/// The default render callback: per-block rendering with streaming markdown.
pub fn render_block(
    state: &mut RenderState,
    index: usize,
    block: &Block,
    styles: Styles,
) -> Vec<Line<'static>> {
    state.render(index, block, styles)
}
