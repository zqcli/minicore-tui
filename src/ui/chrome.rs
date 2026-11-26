//! Shared UI chrome primitives reused across bottom sheets, inline popups,
//! split workspaces (`Ctrl+B`, `/diff`) and scrollable single-pane views.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::scrollbar;
use crate::ui::panel::truncate;
use crate::ui::theme::Styles;
use crate::wrap;

/// One row in one style. The style sits on the span, which is what `set_line` paints.
pub fn styled(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), style))
}

/// Key hints: key in faint, verb in dim, ` · ` between pairs.
pub fn key_hints(hints: &[(&str, &str)], styles: Styles) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for (index, (key, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", styles.dim()));
        }
        spans.push(Span::styled((*key).to_owned(), styles.faint()));
        if !action.is_empty() {
            spans.push(Span::styled(format!(" {action}"), styles.dim()));
        }
    }
    spans
}

/// Unified `/ query▌` filter prompt used across sheets, split workspaces and Help.
pub fn filter_line(
    query: &str,
    placeholder: &str,
    focused: bool,
    width: usize,
    styles: Styles,
) -> Line<'static> {
    let slash_style = if focused {
        styles.accent()
    } else {
        styles.meta()
    };
    if query.is_empty() {
        Line::from(vec![
            Span::styled("/ ", slash_style),
            Span::styled(
                truncate(placeholder, width.saturating_sub(2)),
                styles.faint(),
            ),
        ])
    } else {
        let mut spans = vec![
            Span::styled("/ ", slash_style),
            Span::styled(
                truncate(query, width.saturating_sub(if focused { 3 } else { 2 })),
                styles.text(),
            ),
        ];
        if focused {
            spans.push(Span::styled("▌", styles.accent()));
        }
        Line::from(spans)
    }
}

/// Full-width horizontal divider rule, accented when its owning pane has focus.
pub fn draw_rule(buf: &mut Buffer, x: u16, y: u16, width: u16, focused: bool, styles: Styles) {
    if width == 0 {
        return;
    }
    let rule = "─".repeat(usize::from(width));
    let style = if focused {
        styles.accent()
    } else {
        styles.card_rail()
    };
    buf.set_line(x, y, &styled(&rule, style), width);
}

/// Draws the 2-row filter header (`/ query▌` + `───`) at the top of `area`
/// and returns the remaining body rect below it.
pub fn draw_filter_header(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    placeholder: &str,
    focused: bool,
    styles: Styles,
) -> Rect {
    if area.height == 0 || area.width == 0 {
        return Rect::default();
    }
    let buf = frame.buffer_mut();
    let line = filter_line(query, placeholder, focused, usize::from(area.width), styles);
    buf.set_line(area.x, area.y, &line, area.width);
    if area.height == 1 {
        return Rect::default();
    }
    draw_rule(buf, area.x, area.y + 1, area.width, focused, styles);
    Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: area.height.saturating_sub(2),
    }
}

/// Paints the full-width selection surface behind a highlighted list row.
pub fn paint_highlight_band(buf: &mut Buffer, area: Rect, styles: Styles) {
    if !area.is_empty() {
        buf.set_style(area, styles.highlight());
    }
}

/// Renders scrollable `lines` into `area` with an automatic right-edge scrollbar
/// thumb whenever content exceeds the viewport. Returns the clamped scroll top.
pub fn render_scrollable_lines(
    frame: &mut Frame,
    area: Rect,
    lines: &[Line<'static>],
    scroll: usize,
    styles: Styles,
) -> usize {
    if area.is_empty() {
        return 0;
    }
    let visible = usize::from(area.height);
    let start = scroll.min(lines.len().saturating_sub(visible));
    let thumb = scrollbar::thumb(visible, lines.len(), start);
    let content_width = if thumb.is_some() {
        area.width.saturating_sub(2).max(1)
    } else {
        area.width
    };
    let buf = frame.buffer_mut();
    for (offset, line) in lines.iter().skip(start).take(visible).enumerate() {
        buf.set_line(area.x, area.y + offset as u16, line, content_width);
    }
    if let Some((thumb_start, thumb_len)) = thumb {
        let bar_x = area.right().saturating_sub(1);
        for row in thumb_start..(thumb_start + thumb_len).min(visible) {
            buf.set_string(bar_x, area.y + row as u16, "┃", styles.dim());
        }
    }
    start
}

/// Renders aligned `label  value` rows with clean hanging-indent wrapping.
pub fn kv_lines(
    rows: &[(&str, String, Option<Style>)],
    styles: Styles,
    width: usize,
) -> Vec<Line<'static>> {
    let widest = rows
        .iter()
        .map(|(label, _, _)| wrap::width(label))
        .max()
        .unwrap_or(0);
    let col = (widest + 2).clamp(10, 22);
    let mut out = Vec::with_capacity(rows.len());
    for (label, value, value_style) in rows {
        let style = value_style.unwrap_or_else(|| styles.text());
        let clean = wrap::sanitize(value);
        if width >= col + 16 {
            let room = width - col;
            let mut wrapped = wrap::wrap(&clean, room).into_iter();
            let first = wrapped.next().unwrap_or_default();
            let pad = " ".repeat(col.saturating_sub(wrap::width(label)));
            out.push(Line::from(vec![
                Span::styled((*label).to_owned(), styles.dim()),
                Span::raw(pad),
                Span::styled(first, style),
            ]));
            for cont in wrapped {
                out.push(Line::from(vec![
                    Span::raw(" ".repeat(col)),
                    Span::styled(cont, style),
                ]));
            }
        } else {
            out.push(Line::from(Span::styled((*label).to_owned(), styles.dim())));
            for row in wrap::wrap(&clean, width.saturating_sub(2).max(1)) {
                out.push(Line::from(Span::styled(format!("  {row}"), style)));
            }
        }
    }
    out
}
