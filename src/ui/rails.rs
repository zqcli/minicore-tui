//! The rail decorations shared by every block renderer.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::Styles;

/// Puts a rail in front of a styled line.
pub fn rail(line: Line<'static>, styles: Styles) -> Line<'static> {
    decorate(line, "  ", styles.text())
}

/// The reasoning rail: purple, one cell narrower than the answer inset.
pub fn reasoning_rail(line: Line<'static>, styles: Styles) -> Line<'static> {
    decorate(line, "▏ ", styles.reasoning_rail())
}

/// The user rail: an editor-blue block.
pub fn user_rail(line: Line<'static>, styles: Styles) -> Line<'static> {
    decorate(line, "▎ ", styles.user_rail())
}

fn decorate(line: Line<'static>, rail: &'static str, rail_style: Style) -> Line<'static> {
    let style = line.style;
    let mut spans = vec![Span::styled(rail, rail_style)];
    spans.extend(line.spans);
    Line {
        style,
        spans,
        alignment: None,
    }
}
