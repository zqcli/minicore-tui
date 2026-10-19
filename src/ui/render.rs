//! Rendering one block into cached lines.
//!
//! The layout cache asks for a block only when it changed, so this module keeps
//! the per-block state that must survive between frames: one streaming markdown
//! renderer per live assistant block. The renderer caches its own stable
//! prefix, which is what makes a delta cost one tail parse instead of a full
//! re-parse of the message.

use std::collections::HashMap;

use ratatui::text::Line;

use super::markdown::StreamRenderer;
use super::theme::Styles;
use super::{blocks, rails};
use crate::transcript::block::Block;
use crate::transcript::layout::Fold;

/// Streaming renderers, keyed by transcript position.
#[derive(Default)]
pub struct RenderState {
    streams: HashMap<usize, Stream>,
    width: usize,
    fold: Option<Fold>,
    /// The theme the cached streams were rendered with.
    styles: Option<Styles>,
}

struct Stream {
    text: StreamRenderer,
    reasoning: StreamRenderer,
}

impl RenderState {
    /// Refreshes the per-block state before a frame: drops renderers whose block
    /// stopped streaming and resets on a width or fold change.
    pub fn prepare(&mut self, width: u16, fold: Fold, blocks: &[Block], styles: Styles) {
        if self.width != width as usize || self.fold != Some(fold) || self.styles != Some(styles) {
            self.width = width as usize;
            self.fold = Some(fold);
            self.styles = Some(styles);
            self.streams.clear();
        }
        self.streams
            .retain(|index, _| blocks.get(*index).is_some_and(Block::is_streaming));
    }

    /// Renders block `index`. A live assistant block renders its markdown
    /// incrementally; everything else renders through [`blocks::render`].
    pub fn render(&mut self, index: usize, block: &Block, styles: Styles) -> Vec<Line<'static>> {
        let width = self.width.max(3);
        let fold = self.fold.unwrap_or(Fold {
            expanded_tools: false,
            show_reasoning: false,
        });
        self.styles = Some(styles);
        let Block::Assistant {
            reasoning,
            text,
            streaming,
            ..
        } = block
        else {
            return blocks::render(block, styles, width as u16, fold);
        };
        if !streaming {
            self.streams.remove(&index);
            return blocks::render(block, styles, width as u16, fold);
        }
        let inner = width - blocks::INSET;
        let stream = self.streams.entry(index).or_insert_with(|| Stream {
            text: StreamRenderer::new(inner, styles.text()),
            reasoning: StreamRenderer::new(inner, styles.dim()),
        });
        stream.text.set_width(inner);
        stream.reasoning.set_width(inner);
        stream.text.reset(text);
        stream.reasoning.reset(&blocks::reasoning_source(reasoning));
        let mut lines = reasoning_lines(&mut stream.reasoning, styles, fold);
        if !lines.is_empty() && !text.trim().is_empty() {
            lines.push(Line::default());
        }
        for line in stream.text.lines(&styles).iter() {
            lines.push(rails::rail(line.clone(), styles));
        }
        lines
    }
}

/// The live reasoning tail: the last few source lines, wrapped. Long reasoning
/// must not push the answer off screen while it is being produced.
fn reasoning_lines(
    reasoning: &mut StreamRenderer,
    styles: Styles,
    fold: Fold,
) -> Vec<Line<'static>> {
    let lines = reasoning.lines(&styles);
    if fold.show_reasoning {
        return lines
            .iter()
            .map(|line| rails::reasoning_rail(line.clone(), styles))
            .collect();
    }
    let skip = lines.len().saturating_sub(3);
    lines[skip..]
        .iter()
        .map(|line| rails::reasoning_rail(line.clone(), styles))
        .collect()
}
