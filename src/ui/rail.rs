//! Shared Rail surface primitives and display geometry.
//!
//! Renderers use these helpers for the same reasons that hit testing and
//! measurement use the section model: a rail is one cell, its content starts
//! at a known inset, and a surface row is padded to the exact available
//! width. This module contains no RPC or App mutation.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::markdown::{column_width, line_width};
use crate::theme::Theme;

pub const APP_GUTTER_WIDTH: u16 = 1;
pub const RAIL_GLYPH: &str = "▎";
pub const RAIL_WIDTH: usize = 1;
/// User and Tool native content has one transparent cell after the rail.
pub const SURFACE_CONTENT_START: usize = 2;
pub const EDITOR_MIN_ROWS: u16 = 4;
pub const EDITOR_MAX_ROWS: u16 = 12;
pub const EDITOR_MAX_RATIO_NUMERATOR: u16 = 32;
pub const EDITOR_MAX_RATIO_DENOMINATOR: u16 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolSurfaceState {
    Pending,
    Success,
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceColors {
    pub rail: Color,
    pub background: Color,
}

pub fn editor_colors(theme: &Theme) -> SurfaceColors {
    SurfaceColors {
        rail: theme.rail_editor,
        background: theme.user_message_bg,
    }
}

pub fn thinking_colors(theme: &Theme) -> SurfaceColors {
    SurfaceColors {
        rail: theme.rail_thinking,
        background: Color::Reset,
    }
}

pub fn user_colors(theme: &Theme) -> SurfaceColors {
    SurfaceColors {
        rail: theme.rail_editor,
        background: theme.user_message_bg,
    }
}

pub fn tool_colors(theme: &Theme, state: ToolSurfaceState) -> SurfaceColors {
    match state {
        ToolSurfaceState::Pending => SurfaceColors {
            rail: theme.tool_pending_rail,
            background: theme.tool_pending_bg,
        },
        ToolSurfaceState::Success => SurfaceColors {
            rail: theme.tool_success_rail,
            background: theme.tool_success_bg,
        },
        ToolSurfaceState::Error => SurfaceColors {
            rail: theme.tool_error_rail,
            background: theme.tool_error_bg,
        },
        ToolSurfaceState::Cancelled => SurfaceColors {
            rail: theme.tool_cancelled_rail,
            background: theme.tool_cancelled_bg,
        },
    }
}

/// Render one filled Rail row. `content_start` is measured from the start of
/// the conversation area and includes the rail cell; the standard Rail
/// surface uses `1`, while a user message may ask its native content for a
/// wider inset.
pub fn surface_row(
    width: usize,
    colors: SurfaceColors,
    content_start: usize,
    content: Line<'static>,
) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let content_start = content_start.max(RAIL_WIDTH).min(width);
    let mut spans = Vec::new();
    spans.push(Span::styled(
        RAIL_GLYPH,
        surface_style(Style::new().fg(colors.rail), colors.background),
    ));
    if content_start > RAIL_WIDTH {
        spans.push(Span::styled(
            " ".repeat(content_start - RAIL_WIDTH),
            surface_style(Style::default(), colors.background),
        ));
    }
    let content_width = width.saturating_sub(content_start);
    spans.extend(
        fit_spans(content.spans, content_width)
            .into_iter()
            .map(|span| Span {
                style: surface_style(span.style, colors.background),
                ..span
            }),
    );
    let used: usize = spans.iter().map(|span| column_width(&span.content)).sum();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            surface_style(Style::default(), colors.background),
        ));
    }
    Line::from(spans)
}

/// Render a transparent assistant row with the same content inset that a
/// thinking Rail uses. No background is painted for the prefix or fill.
pub fn inset_row(width: usize, content_start: usize, content: Line<'static>) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let inset = content_start.min(width);
    let mut spans = Vec::new();
    if inset > 0 {
        spans.push(Span::raw(" ".repeat(inset)));
    }
    let content_width = width.saturating_sub(inset);
    spans.extend(fit_spans(content.spans, content_width));
    let used: usize = spans.iter().map(|span| column_width(&span.content)).sum();
    if used < width {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    Line::from(spans)
}

pub fn transparent_row(width: usize, content: Line<'static>) -> Line<'static> {
    inset_row(width, 0, content)
}

pub fn surface_style(style: Style, background: Color) -> Style {
    if background == Color::Reset || style.bg.is_some() {
        style
    } else {
        style.bg(background)
    }
}

/// Rail's `collapsedSimpleLine`: control whitespace and runs of whitespace
/// become one ordinary space before the visual clip.
pub fn collapsed_simple_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Clip a string to terminal columns without splitting a wide character.
pub fn clip_cells(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > width {
            break;
        }
        result.push(ch);
        used += cw;
    }
    result
}

fn fit_spans<'a>(spans: Vec<Span<'a>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut fitted = Vec::new();
    for span in spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > width {
                break;
            }
            text.push(ch);
            used += cw;
        }
        if !text.is_empty() {
            fitted.push(Span::styled(text, span.style));
        }
        if used >= width {
            break;
        }
    }
    fitted
}

pub fn editor_responsive_max(terminal_rows: u16) -> u16 {
    ((terminal_rows as u32 * EDITOR_MAX_RATIO_NUMERATOR as u32)
        / EDITOR_MAX_RATIO_DENOMINATOR as u32)
        .clamp(EDITOR_MIN_ROWS as u32, EDITOR_MAX_ROWS as u32) as u16
}

pub fn editor_target_rows(body_rows: usize, terminal_rows: u16) -> u16 {
    let max_rows = editor_responsive_max(terminal_rows);
    body_rows
        .max(EDITOR_MIN_ROWS as usize)
        .min(max_rows as usize) as u16
}

pub fn content_width(width: usize, content_start: usize) -> usize {
    width.saturating_sub(content_start).max(1)
}

pub fn line_cells(line: &Line<'_>) -> usize {
    line_width(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn surface_row_has_one_rail_cell_and_exact_width() {
        let row = surface_row(
            12,
            user_colors(&Theme::dark()),
            1,
            Line::from(Span::raw("hello")),
        );
        assert_eq!(line_cells(&row), 12);
        assert_eq!(row.spans[0].content, RAIL_GLYPH);
        let content: String = row
            .spans
            .iter()
            .skip(1)
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(content, "hello      ");
    }

    #[test]
    fn surface_content_start_preserves_requested_inset() {
        let row = surface_row(
            8,
            user_colors(&Theme::dark()),
            2,
            Line::from(Span::raw("x")),
        );
        assert_eq!(row.spans[0].content, RAIL_GLYPH);
        assert_eq!(row.spans[1].content, " ");
        assert_eq!(line_cells(&row), 8);
    }

    #[test]
    fn simple_line_collapses_control_whitespace_and_clip_is_cell_safe() {
        assert_eq!(collapsed_simple_line("a\n\t b  c"), "a b c");
        assert_eq!(clip_cells("你a", 2), "你");
        assert_eq!(clip_cells("你a", 1), "");
    }

    #[test]
    fn editor_height_is_four_to_twelve_and_roughly_thirty_two_percent() {
        assert_eq!(editor_responsive_max(16), 5);
        assert_eq!(editor_responsive_max(24), 7);
        assert_eq!(editor_responsive_max(40), 12);
        assert_eq!(editor_target_rows(1, 24), 4);
        assert_eq!(editor_target_rows(20, 24), 7);
    }
}
