//! The transcript pane: block rendering through the layout cache, scroll
//! resolution and the drag highlight. A frame touches only the visible rows.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

use super::markdown::StreamRenderer;
use super::theme::Styles;
use super::{blocks, rails, selection};
use crate::app::{App, Scroll};
use crate::transcript::Block;
use crate::transcript::layout::{Fold, Viewport};

/// Streaming markdown renderers of live assistant blocks, by transcript
/// position. Each caches its stable prefix, so a delta re-parses only the tail.
#[derive(Default)]
pub struct RenderState {
    streams: HashMap<usize, Stream>,
    /// What the streams were rendered for; any change restarts them.
    key: Option<(u16, Fold, Styles)>,
}

struct Stream {
    text: StreamRenderer,
    reasoning: StreamRenderer,
}

impl RenderState {
    fn prepare(&mut self, key: (u16, Fold, Styles), blocks: &[Block]) {
        if self.key != Some(key) {
            self.key = Some(key);
            self.streams.clear();
        }
        self.streams
            .retain(|index, _| blocks.get(*index).is_some_and(Block::is_streaming));
    }

    fn render(
        &mut self,
        index: usize,
        block: &Block,
        (width, fold, styles): (u16, Fold, Styles),
    ) -> Vec<Line<'static>> {
        let Block::Assistant {
            reasoning,
            text,
            streaming: true,
            ..
        } = block
        else {
            self.streams.remove(&index);
            return blocks::render(block, styles, width, fold);
        };
        let inner = width as usize - blocks::INSET;
        let stream = self.streams.entry(index).or_insert_with(|| Stream {
            text: StreamRenderer::new(inner, styles.text()),
            reasoning: StreamRenderer::new(inner, styles.dim()),
        });
        stream.text.reset(text);
        stream.reasoning.reset(&blocks::reasoning_source(reasoning));
        // Long reasoning must not push the answer off screen while it streams.
        let reasoning = stream.reasoning.lines(&styles);
        let shown = if fold.show_reasoning {
            &reasoning[..]
        } else {
            &reasoning[reasoning.len().saturating_sub(3)..]
        };
        let mut lines: Vec<Line<'static>> = shown
            .iter()
            .map(|line| rails::reasoning_rail(line.clone(), styles))
            .collect();
        if !lines.is_empty() && !text.trim().is_empty() {
            lines.push(Line::default());
        }
        for line in stream.text.lines(&styles).iter() {
            lines.push(rails::rail(line.clone(), styles));
        }
        lines
    }
}

/// Draws the transcript into `area` and records the drawn rows for copy.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App, styles: Styles) {
    let Some(session) = app.session.as_ref() else {
        let lines = if app.ready {
            vec![
                Line::styled("No session yet", styles.dim()),
                Line::raw(""),
                Line::styled("Press Enter to start one", styles.subtle()),
            ]
        } else {
            vec![Line::styled("Connecting to minicore-agent…", styles.dim())]
        };
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };
    // Block renderers subtract their inset from the width.
    let width = area.width.max(blocks::INSET as u16 + 1);
    let fold = Fold {
        expanded_tools: app.tools_expanded,
        show_reasoning: app.show_reasoning,
    };
    let key = (width, fold, styles);
    let layout = &mut app.layout;
    let blocks = session.transcript.blocks();
    layout.state.prepare(key, blocks);
    layout.prepare(
        blocks,
        session.transcript.revisions(),
        width,
        fold,
        |state, index, block| state.render(index, block, key),
    );
    let total = layout.total_rows();
    let height = area.height as usize;
    let top = match app.scroll {
        Scroll::Follow => total.saturating_sub(height),
        // An anchor is resolved against the current layout, so content growing
        // below it does not move the view.
        Scroll::Anchor { block, offset } => layout.anchor_row(block, offset),
    }
    .min(total.saturating_sub(1));
    layout.viewport = Viewport {
        top,
        height,
        width: area.width,
        origin: (area.x, area.y),
    };
    let mut lines = layout.visible(top, height);
    let drag = app.selection_start.map(|from| selection::Drag {
        from,
        to: app.selection_end.unwrap_or(from),
    });
    if let Some(drag) = drag.filter(|drag| !drag.is_click()) {
        for (index, line) in lines.iter_mut().enumerate() {
            if let Some(span) = selection::row_span(area.y + index as u16, drag, area) {
                *line = highlight(line, span.start, span.end, styles.selection());
            }
        }
    }
    if app.scroll != Scroll::Follow
        && top + height < total
        && let Some(pill) = lines.last_mut()
    {
        *pill = Line::styled("↓ new output", styles.accent());
    }
    // Copy reads exactly these rows, so a drag copies what is on screen even
    // while a turn streams.
    app.rendered
        .set(area.y, lines.iter().map(Line::to_string).collect());
    frame.render_widget(Paragraph::new(lines), area);
}

/// Restyles cells `[from, to)` of one row, splitting spans at the boundaries;
/// a wide character is selected whole when any of its cells is.
fn highlight(line: &Line<'static>, from: u16, to: u16, style: Style) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    let mut column = 0u16;
    for span in &line.spans {
        let mut run = String::new();
        let mut run_selected: Option<bool> = None;
        for grapheme in span.content.graphemes(true) {
            let cells = (super::wrap::width(grapheme) as u16).max(1);
            let selected = column + cells > from && column < to;
            if let Some(picked) = run_selected.filter(|current| *current != selected) {
                spans.push(Span::styled(
                    std::mem::take(&mut run),
                    if picked { style } else { span.style },
                ));
            }
            run_selected = Some(selected);
            run.push_str(grapheme);
            column += cells;
        }
        if let Some(selected) = run_selected {
            spans.push(Span::styled(run, if selected { style } else { span.style }));
        }
    }
    Line::from(spans)
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
