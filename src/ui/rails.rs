//! The row builders every block renderer shares: a bare rail, a rail on a
//! surface, and the inset of rail-less text. Rows are cut at their width with
//! no ellipsis. They also tell a selection which cells hold content.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::wrap;

pub const RAIL: &str = "▎";

/// The rail, then `spans` on the terminal background.
pub fn rail_row(rail: Style, spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut row = vec![Span::styled(RAIL, rail)];
    row.extend(clip(spans, width.saturating_sub(1)));
    Line::from(row)
}

/// The rail, then `spans` on `surface` filled out to `width`; the rail cell
/// itself keeps the terminal background.
pub fn surface_row(
    rail: Style,
    surface: Style,
    spans: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    let mut row = vec![Span::styled(RAIL, rail)];
    row.extend(fill(surface, spans, width.saturating_sub(1)));
    Line::from(row)
}

/// `spans` on `surface`, cut or filled out to exactly `width` cells.
pub fn fill(surface: Style, spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut row: Vec<Span<'static>> = clip(spans, width)
        .into_iter()
        .map(|span| {
            let style = surface.patch(span.style);
            span.style(style)
        })
        .collect();
    let used: usize = row.iter().map(|span| wrap::width(&span.content)).sum();
    if used < width {
        row.push(Span::styled(" ".repeat(width - used), surface));
    }
    row
}

/// A rail-less line moved right by `columns`.
pub fn indent(line: Line<'static>, columns: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(columns))];
    spans.extend(line.spans);
    Line {
        style: line.style,
        spans,
        alignment: None,
    }
}

// Removing a modifier no cell has changes nothing on screen, so this style
// can tag a row without a field of its own.
const CHROME: Style = Style::new().remove_modifier(Modifier::HIDDEN);

/// Tags a row the UI adds around content, a hint, a timestamp or a spacer,
/// which a selection skips.
pub fn chrome(line: Line<'static>) -> Line<'static> {
    line.patch_style(CHROME)
}

/// Tags a span the UI adds before or after a row's content, such as a card's duration.
pub fn chrome_span(span: Span<'static>) -> Span<'static> {
    span.patch_style(CHROME)
}

fn is_chrome(style: Style) -> bool {
    style.sub_modifier.contains(Modifier::HIDDEN)
}

/// The cells of `line` a selection covers: none on a chrome row, otherwise
/// from past the rail, its pad or indent and a leading chrome span to the last
/// visible character before the next chrome span.
pub fn content_cells(line: &Line) -> Range<u16> {
    if is_chrome(line.style) {
        return 0..0;
    }
    let mut spans = line.spans.as_slice();
    let mut start = 0;
    if let [first, rest @ ..] = spans
        && first.content == RAIL
    {
        start += 1;
        spans = rest;
    }
    if let [first, rest @ ..] = spans
        && !first.content.is_empty()
        && first.content.trim().is_empty()
    {
        start += wrap::width(&first.content);
        spans = rest;
    }
    // A quote rail leads the content it frames.
    if let [first, rest @ ..] = spans
        && is_chrome(first.style)
    {
        start += wrap::width(&first.content);
        spans = rest;
    }
    let text: String = spans
        .iter()
        .take_while(|span| !is_chrome(span.style))
        .map(|span| span.content.as_ref())
        .collect();
    let end = start + wrap::width(text.trim_end());
    start as u16..end as u16
}

/// `spans` cut at `width` cells.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut left = width;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let cells = wrap::width(&span.content);
        if cells <= left {
            left -= cells;
            out.push(span);
            continue;
        }
        if left > 0 {
            let end = wrap::char_breaks(&span.content, left)[0].end;
            out.push(Span::styled(span.content[..end].to_owned(), span.style));
        }
        break;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_row_is_exactly_its_width_and_the_rail_has_no_surface() {
        let surface = Style::new().bg(ratatui::style::Color::Blue);
        let long = surface_row(Style::new(), surface, vec![Span::raw("x".repeat(30))], 10);
        assert_eq!(long.to_string(), format!("{RAIL}{}", "x".repeat(9)));
        let short = surface_row(Style::new(), surface, vec![Span::raw("ab")], 10);
        assert_eq!(wrap::width(&short.to_string()), 10);
        assert_eq!(short.spans[0].style.bg, None);
        assert!(
            short.spans[1..]
                .iter()
                .all(|span| span.style.bg == surface.bg)
        );
    }

    #[test]
    fn content_starts_past_the_rail_and_its_pad_and_chrome_has_none() {
        let surface = Style::new().bg(ratatui::style::Color::Blue);
        let card = surface_row(
            Style::new(),
            surface,
            vec![Span::raw(" "), Span::raw("  indented"), Span::raw(" ")],
            20,
        );
        assert_eq!(content_cells(&card), 2..12, "{card}");
        let reply = indent(Line::from("text"), 2);
        assert_eq!(content_cells(&reply), 2..6);
        assert_eq!(content_cells(&chrome(card)), 0..0);
        let titled = rail_row(
            Style::new(),
            vec![
                Span::raw(" "),
                Span::raw("bash"),
                chrome_span(Span::raw("   4.2s")),
            ],
            20,
        );
        assert_eq!(content_cells(&titled), 2..6);
        let quoted = indent(
            Line::from(vec![chrome_span(Span::raw("│ ")), Span::raw("quoted")]),
            2,
        );
        assert_eq!(content_cells(&quoted), 4..10);
    }
}
