//! The transcript pane: block rendering through the layout cache, scroll
//! resolution, the welcome header and the overlays drawn over the rows: drag
//! highlight, jump label, scrollbar and copy flash. A frame touches only the
//! visible rows.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

use super::markdown::StreamRenderer;
use super::theme::Styles;
use super::{blocks, selection, views};
use crate::app::{App, Scroll};
use crate::transcript::layout::{Fold, Viewport};
use crate::transcript::{Block, RequestKey};

/// Composited on the last row while the view is detached from the tail.
pub const JUMP_LABEL: &str = " ↓ Jump to latest message · End ";

/// The bindings the welcome header advertises, as `(key, action)`.
const HEADER_HINTS: &[(&str, &str)] = &[
    ("esc", "interrupt"),
    ("ctrl+c", "clear"),
    ("/", "commands"),
    ("@", "files"),
    ("ctrl+o", "expand"),
    ("ctrl+r", "sessions"),
];

/// Streaming markdown renderers of live assistant blocks, by request. Each
/// caches its stable prefix, so a delta re-parses only the tail.
#[derive(Default)]
pub struct RenderState {
    streams: HashMap<RequestKey, Stream>,
    /// What the streams were rendered for; any change restarts them.
    key: Option<(u16, Fold, Styles)>,
    /// Rows of the welcome header cached in front of block 0.
    pub header_rows: usize,
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
        // Live blocks sit at the end, so a match is found after a few steps.
        self.streams.retain(|key, _| {
            blocks.iter().rev().any(|block| {
                matches!(block, Block::Assistant { key: live, streaming: true, .. } if live == key)
            })
        });
    }

    fn render(
        &mut self,
        block: &Block,
        (width, fold, styles): (u16, Fold, Styles),
    ) -> Vec<Line<'static>> {
        let Block::Assistant {
            key,
            reasoning,
            text,
            streaming: true,
            expanded,
        } = block
        else {
            return blocks::render(block, styles, width, fold);
        };
        let inner = blocks::text_width(width as usize);
        let stream = self.streams.entry(key.clone()).or_insert_with(|| Stream {
            text: StreamRenderer::new(inner, styles.text()),
            reasoning: StreamRenderer::new(inner, blocks::thinking_style(styles)),
        });
        stream.text.reset(text);
        stream.reasoning.reset(&blocks::reasoning_source(reasoning));
        let thinking = stream.reasoning.lines(&styles);
        let reply = stream.text.lines(&styles);
        blocks::assistant_rows(
            reasoning,
            &thinking,
            &reply,
            expanded.unwrap_or(fold.expanded),
            fold.show_reasoning,
            styles,
            width as usize,
        )
    }
}

/// The transcript's part of `area`: everything right of the one-column gutter.
pub fn inner(area: Rect) -> Rect {
    let gutter = area.width.min(1);
    Rect {
        x: area.x + gutter,
        width: area.width - gutter,
        ..area
    }
}

/// Draws the transcript into `area` and records the drawn rows for copy.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App, styles: Styles) {
    let area = inner(area);
    let header = header(styles, area.width, &app.workspace);
    let Some(session) = app.session.as_ref() else {
        let status = if !app.ready {
            "Connecting to minicore-agent…"
        } else if app.loading_session {
            "Opening the session…"
        } else {
            "No session yet · enter starts one"
        };
        let mut lines = header;
        lines.push(Line::styled(format!(" {status}"), styles.dim()));
        frame.render_widget(Paragraph::new(lines), area);
        draw_flash(frame, area, app, styles);
        return;
    };
    // Block renderers subtract their inset from the width.
    let width = area.width.max(blocks::INSET as u16 + 1);
    let fold = Fold {
        expanded: app.tools_expanded,
        show_reasoning: app.show_reasoning,
    };
    let key = (width, fold, styles);
    let layout = &mut app.layout;
    let blocks = session.transcript.blocks();
    layout.state.prepare(key, blocks);
    layout.state.header_rows = header.len();
    layout.prepare(
        blocks,
        session.transcript.revisions(),
        width,
        fold,
        // The header rides in front of block 0, so it scrolls and anchors with
        // the transcript without being a block.
        |state, index, block| match index {
            0 => [header.clone(), state.render(block, key)].concat(),
            _ => state.render(block, key),
        },
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
    // A selection marks screen cells, so it means nothing once the rows move.
    if top != layout.viewport.top {
        app.selection = None;
    }
    layout.viewport = Viewport {
        top,
        height,
        width: area.width,
        origin: (area.x, area.y),
    };
    let mut lines = match blocks.is_empty() {
        true => header,
        false => layout.visible(top, height),
    };
    if let Some(drag) = app.selection {
        for (index, line) in lines.iter_mut().enumerate() {
            if let Some(span) = selection::row_span(area.y + index as u16, drag, area) {
                *line = highlight(line, span.start, span.end);
            }
        }
    }
    // Copy reads exactly these rows, so a drag copies what is on screen even
    // while a turn streams.
    app.rendered
        .set(area.y, lines.iter().map(Line::to_string).collect());
    frame.render_widget(Paragraph::new(lines), area);
    let buffer = frame.buffer_mut();
    if top + height < total && area.height > 0 {
        let cells = jump_label(area.width);
        buffer.set_stringn(
            area.x + cells.start,
            area.bottom() - 1,
            JUMP_LABEL,
            cells.len(),
            styles.highlight().patch(styles.text()),
        );
    }
    if app.scrollbar_shown()
        && let Some((start, len)) = thumb(height, total, top)
    {
        let glyph = match app.scrollbar_grab {
            Some(_) => "█",
            None => "┃",
        };
        for row in 0..height {
            let cell = &mut buffer[(area.right() - 1, area.y + row as u16)];
            match (start..start + len).contains(&row) {
                true => cell.set_symbol(glyph).set_style(styles.text()),
                false => cell.set_symbol("│").set_style(styles.faint()),
            };
        }
    }
    draw_flash(frame, area, app, styles);
}

/// The welcome rows: name and version, the main bindings, the workspace.
fn header(styles: Styles, width: u16, workspace: &Path) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::default(),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("minicore-tui", styles.title()),
            Span::styled(concat!(" v", env!("CARGO_PKG_VERSION")), styles.faint()),
        ]),
    ];
    // Hints wrap whole, so a narrow terminal never splits a key from its action.
    let room = (width as usize).saturating_sub(1);
    let mut row: Vec<(&str, &str)> = Vec::new();
    let mut used = 0;
    for &(key, action) in HEADER_HINTS {
        let cells = key.len() + 1 + action.len();
        if !row.is_empty() && used + 3 + cells > room {
            lines.push(hint_line(&row, styles));
            row.clear();
        }
        used = match row.is_empty() {
            true => cells,
            false => used + 3 + cells,
        };
        row.push((key, action));
    }
    lines.push(hint_line(&row, styles));
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let shown = match home
        .as_deref()
        .and_then(|home| workspace.strip_prefix(home).ok())
    {
        Some(rest) => Path::new("~").join(rest),
        None => workspace.to_path_buf(),
    };
    lines.push(Line::styled(
        format!(" {}", shown.display()),
        styles.faint(),
    ));
    lines.push(Line::default());
    lines
}

fn hint_line(hints: &[(&str, &str)], styles: Styles) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    spans.extend(views::key_hints(hints, styles));
    Line::from(spans)
}

/// The cells of the jump label on the last row, centred.
pub fn jump_label(width: u16) -> Range<u16> {
    let len = (JUMP_LABEL.chars().count() as u16).min(width);
    let start = (width - len) / 2;
    start..start + len
}

/// The scrollbar thumb's first row and length on a `track`-row bar over
/// `total` rows scrolled to `top`; `None` when everything fits.
pub fn thumb(track: usize, total: usize, top: usize) -> Option<(usize, usize)> {
    if track == 0 || total <= track {
        return None;
    }
    let len = ((track * track + total / 2) / total).clamp(2.min(track), track);
    let (room, max_top) = (track - len, total - track);
    let start = (top.min(max_top) * room + max_top / 2) / max_top;
    Some((start, len))
}

/// The scroll top that puts a `len`-row thumb's first row on track row `row`.
pub fn thumb_top(track: usize, total: usize, len: usize, row: isize) -> usize {
    let room = track.saturating_sub(len);
    if room == 0 {
        return 0;
    }
    let row = row.clamp(0, room as isize) as usize;
    let max_top = total.saturating_sub(track);
    (row * max_top + room / 2) / room
}

/// ` Copied! ` in reverse video at the top-right corner while it is fresh.
pub fn draw_flash(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let Some((text, _)) = app.flash.as_ref().filter(|_| area.height > 0) else {
        return;
    };
    let text = format!(" {text} ");
    let len = (super::wrap::width(&text) as u16).min(area.width);
    frame.buffer_mut().set_stringn(
        area.right() - len,
        area.y,
        &text,
        len as usize,
        styles.text().add_modifier(Modifier::REVERSED),
    );
}

/// Reverses cells `[from, to)` of one row, splitting spans at the boundaries;
/// a wide character is selected whole when any of its cells is.
fn highlight(line: &Line<'static>, from: u16, to: u16) -> Line<'static> {
    let reversed = |style: Style| style.add_modifier(Modifier::REVERSED);
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
                    if picked {
                        reversed(span.style)
                    } else {
                        span.style
                    },
                ));
            }
            run_selected = Some(selected);
            run.push_str(grapheme);
            column += cells;
        }
        if let Some(selected) = run_selected {
            let style = if selected {
                reversed(span.style)
            } else {
                span.style
            };
            spans.push(Span::styled(run, style));
        }
    }
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn reversed(style: Style) -> bool {
        style.add_modifier.contains(Modifier::REVERSED)
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_stream_follows_its_request_when_the_blocks_around_it_move() {
        use crate::ui::theme::Palette;
        let live = |request_index, text: &str| Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index,
            },
            reasoning: String::new(),
            text: text.into(),
            streaming: true,
            expanded: None,
        };
        let fold = Fold {
            expanded: false,
            show_reasoning: true,
        };
        let key = (60, fold, Styles::of(Palette::dark()));
        let first = live(0, "one\n\ntwo is long enough to leave a prefix\n\nthree");
        let second = live(1, "beta");
        let mut state = RenderState::default();
        state.prepare(key, &[first.clone(), second.clone()]);
        state.render(&first, key);
        state.render(&second, key);
        state.prepare(key, std::slice::from_ref(&second));
        let rows =
            |lines: Vec<Line<'static>>| lines.iter().map(Line::to_string).collect::<Vec<_>>();
        assert_eq!(
            rows(state.render(&second, key)),
            rows(blocks::render(&second, key.2, 60, fold))
        );
    }

    #[test]
    fn a_selection_keeps_the_row_text_and_styles_only_its_cells() {
        let line = Line::from(vec![
            Span::raw("abcd"),
            Span::styled("efgh", Style::default().fg(Color::Red)),
        ]);
        let marked = highlight(&line, 2, 6);
        let parts: Vec<(&str, bool)> = marked
            .spans
            .iter()
            .map(|span| (span.content.as_ref(), reversed(span.style)))
            .collect();
        assert_eq!(text(&marked), "abcdefgh");
        assert_eq!(
            parts,
            vec![("ab", false), ("cd", true), ("ef", true), ("gh", false),],
            "only the dragged cells are reversed"
        );
        assert_eq!(
            marked.spans[2].style.fg,
            Some(Color::Red),
            "a selected span keeps its colours"
        );
        assert_eq!(marked.spans[3].style.fg, Some(Color::Red));
    }

    #[test]
    fn a_selection_over_a_whole_row_styles_it_without_splitting() {
        let line = Line::raw("row");
        let marked = highlight(&line, 0, 3);
        assert_eq!(marked.spans.len(), 1);
        assert_eq!(marked.spans[0].content, "row");
        assert!(reversed(marked.spans[0].style));
    }

    #[test]
    fn a_selection_never_splits_a_wide_character() {
        let line = Line::raw("日本");
        let marked = highlight(&line, 0, 2);
        assert_eq!(text(&marked), "日本");
        assert_eq!(marked.spans[0].content, "日");
        assert!(reversed(marked.spans[0].style));
        assert!(!reversed(marked.spans[1].style));
    }

    #[test]
    fn the_gutter_is_one_column_when_there_is_room() {
        assert_eq!(inner(Rect::new(0, 2, 80, 10)), Rect::new(1, 2, 79, 10));
        assert_eq!(inner(Rect::new(0, 0, 0, 10)).width, 0);
    }

    #[test]
    fn the_thumb_is_sized_by_the_visible_share_and_never_below_two_rows() {
        assert_eq!(thumb(10, 10, 0), None, "nothing to scroll");
        assert_eq!(thumb(10, 20, 0), Some((0, 5)));
        assert_eq!(thumb(10, 20, 10), Some((5, 5)), "at the bottom");
        assert_eq!(thumb(10, 1000, 0), Some((0, 2)));
        assert_eq!(thumb(10, 1000, 990), Some((8, 2)));
        assert_eq!(thumb(10, 1000, 495), Some((4, 2)), "halfway");
    }

    #[test]
    fn dragging_the_thumb_maps_its_row_back_to_a_scroll_top() {
        assert_eq!(thumb_top(10, 20, 5, 0), 0);
        assert_eq!(thumb_top(10, 20, 5, 5), 10);
        assert_eq!(thumb_top(10, 20, 5, 9), 10, "clamped at the end");
        assert_eq!(thumb_top(10, 20, 5, -3), 0, "clamped at the start");
        for top in [0, 3, 7, 10] {
            let (start, len) = thumb(10, 20, top).unwrap();
            let back = thumb_top(10, 20, len, start as isize);
            assert_eq!(
                thumb(10, 20, back),
                Some((start, len)),
                "round trip from {top}"
            );
        }
    }

    #[test]
    fn the_jump_label_is_centred_and_clipped_to_narrow_rows() {
        let len = JUMP_LABEL.chars().count() as u16;
        assert_eq!(len, 32);
        assert_eq!(jump_label(100), 34..66);
        assert_eq!(jump_label(20), 0..20);
    }
}
