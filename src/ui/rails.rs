//! The row builders every block renderer shares: a bare rail, a rail on a
//! surface, and the inset of rail-less text. Rows are cut at their width with
//! no ellipsis.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::wrap;

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
}
