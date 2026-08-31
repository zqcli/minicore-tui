//! Dock geometry and small line-layout helpers shared by the renderers
//! (development spec 14, 21, 31).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::markdown::{char_width, column_width, line_width};

pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 16;

pub fn is_too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

/// True while the active session has a running live turn (spec 14.1: the
/// busy status row only exists then).
pub fn busy(app: &App) -> bool {
    app.active_view().is_some_and(|view| view.live.is_some())
}

/// Composer total height: 3 content lines plus 2 border lines, fixed to 3
/// rows on short terminals (spec 14.3, 21.2).
pub fn composer_height(short: bool) -> u16 {
    if short { 3 } else { 5 }
}

/// Selector / new-session panel height: 8-14 rows, short terminals get the
/// minimum (spec 24.2). The panel replaces the composer in the dock.
pub fn panel_height(short: bool) -> u16 {
    if short { 8 } else { 14 }
}

/// Footer row count: one row below 80 columns or below 24 rows, otherwise
/// two (spec 14.3).
pub fn footer_height(width: u16, height: u16) -> u16 {
    if width < 80 || height < 24 { 1 } else { 2 }
}

/// Prepends `width` blank cells to a line.
pub fn left_pad(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(width))];
    spans.extend(line.spans);
    Line::from(spans)
}

/// Appends background cells so the line is exactly `width` cells wide.
pub fn fill_line(line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    let fill = width.saturating_sub(line_width(&line));
    let mut spans = line.spans;
    spans.push(Span::styled(" ".repeat(fill), style));
    Line::from(spans)
}

/// One background-styled row: `text` truncated to `width`, then padding.
pub fn filled(text: &str, width: usize, style: Style) -> Line<'static> {
    let text = truncate(text, width);
    let fill = width.saturating_sub(column_width(&text));
    Line::from(vec![
        Span::styled(text, style),
        Span::styled(" ".repeat(fill), style),
    ])
}

/// Truncates `text` to `width` display cells without splitting a character.
pub fn truncate(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = char_width(ch);
        if used + cw > width {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out
}
