//! The transcript view: layout cache, viewport and scroll resolution.
//!
//! One frame renders only the visible window: the first visible block comes
//! from a binary search over prefix heights and each row from the block's
//! cached lines, so the cost of a frame does not depend on history length.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::render::RenderState;
use super::selection;
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

/// Draws the transcript window and records where it landed. `drag` is the live
/// mouse selection, if any; its cells are re-styled in place.
pub fn draw<S>(
    cache: &mut Layout<S>,
    transcript: Transcript<'_>,
    area: Rect,
    scroll: Scroll,
    styles: Styles,
    drag: Option<selection::Drag>,
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
    if let Some(drag) = drag.filter(|drag| !drag.is_click()) {
        for (index, line) in lines.iter_mut().enumerate() {
            if let Some(span) = selection::row_span(area.y + index as u16, drag, area) {
                *line = highlight(line, span.start, span.end, styles.selection());
            }
        }
    }
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

/// Rewrites one row so the cells in `[from, to)` carry the selection style,
/// splitting spans at the boundaries and leaving wide characters intact.
fn highlight(line: &Line<'static>, from: u16, to: u16, style: Style) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    let mut column = 0u16;
    for span in &line.spans {
        let mut run = String::new();
        let mut run_selected: Option<bool> = None;
        for grapheme in UnicodeSegmentation::graphemes(span.content.as_ref(), true) {
            let cells = super::wrap::width(grapheme) as u16;
            // A grapheme is selected when any of its cells is: wide characters
            // are all or nothing.
            let selected = column + cells.max(1) > from && column < to;
            if run_selected.is_some_and(|current| current != selected) {
                let picked = run_selected.unwrap_or(false);
                spans.push(Span::styled(
                    std::mem::take(&mut run),
                    if picked { style } else { span.style },
                ));
            }
            run_selected = Some(selected);
            run.push_str(grapheme);
            column += cells.max(1);
        }
        if let Some(selected) = run_selected {
            spans.push(Span::styled(run, if selected { style } else { span.style }));
        }
    }
    Line::from(spans)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn band() -> Style {
        Style::default()
            .bg(Color::Blue)
            .add_modifier(Modifier::REVERSED)
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_selection_keeps_the_row_text_and_styles_only_its_cells() {
        let line = Line::from(vec![
            Span::raw("abcd"),
            Span::styled("efgh", Style::default().fg(Color::Red)),
        ]);
        let marked = highlight(&line, 2, 6, band());
        let parts: Vec<(&str, Option<Color>)> = marked
            .spans
            .iter()
            .map(|span| (span.content.as_ref(), span.style.bg))
            .collect();
        assert_eq!(text(&marked), "abcdefgh");
        assert_eq!(
            parts,
            vec![
                ("ab", None),
                ("cd", Some(Color::Blue)),
                ("ef", Some(Color::Blue)),
                ("gh", None),
            ],
            "only the dragged cells take the band, and the red span survives"
        );
        assert_eq!(marked.spans[3].style.fg, Some(Color::Red));
    }

    #[test]
    fn a_selection_over_a_whole_row_styles_it_without_splitting() {
        let line = Line::raw("row");
        let marked = highlight(&line, 0, 3, band());
        assert_eq!(marked.spans.len(), 1);
        assert_eq!(marked.spans[0].content, "row");
        assert_eq!(marked.spans[0].style.bg, Some(Color::Blue));
    }

    #[test]
    fn a_selection_never_splits_a_wide_character() {
        let line = Line::raw("日本");
        let marked = highlight(&line, 0, 2, band());
        assert_eq!(text(&marked), "日本");
        assert_eq!(marked.spans[0].content, "日");
        assert_eq!(marked.spans[0].style.bg, Some(Color::Blue));
        assert_eq!(marked.spans[1].style.bg, None);
    }
}
