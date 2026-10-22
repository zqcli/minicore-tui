//! Rendering from `&App`: the transcript or a full-screen view, an optional
//! the dock below it, where a bottom sheet takes the editor's place. Only the
//! transcript's layout cache and the rows recorded for copy are written.

pub mod blocks;
pub mod composer;
pub mod markdown;
pub mod panel;
#[cfg(test)]
mod perf_tests;
pub mod rails;
pub mod sheets;
#[cfg(test)]
pub mod snapshots;
pub mod status;
pub mod theme;
pub mod transcript;
pub mod views;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Overlay};
use crate::wrap;

use self::theme::Styles;

/// Draws one frame and returns the caret to `terminal::draw_frame`.
/// Fatal errors take the whole screen. The terminal owns cursor visibility.
pub fn draw(frame: &mut Frame, app: &mut App) -> Option<Position> {
    let area = frame.area();
    app.composer_area = Rect::default();
    if let Some(fatal) = &app.fatal {
        draw_fatal(frame, area, fatal);
        return None;
    }
    let styles = app.styles;
    let sheet = sheets::sheet(app, styles);
    let dock = status::dock(app, styles, area.width.saturating_sub(1), area.height);
    // The transcript keeps a row and the footer its own; the rows above the
    // slot give way first.
    let room = area.height.saturating_sub(2);
    let slot = sheet
        .as_ref()
        .map_or(dock.editor.len() as u16, panel::Sheet::height)
        .min(room);
    let above = (dock.above.len() as u16).min(room - slot);
    let footer = area.height.min(1);
    let main = Rect {
        height: area.height - above - slot - footer,
        ..area
    };
    let above_area = Rect {
        y: main.bottom(),
        height: above,
        ..area
    };
    let slot_area = Rect {
        y: above_area.bottom(),
        height: slot,
        ..area
    };
    let footer_area = Rect {
        y: slot_area.bottom(),
        height: footer,
        ..area
    };
    app.view_area = transcript::inner(main);
    if !views::draw(frame, main, app, styles) {
        transcript::draw(frame, main, app, styles);
    }
    let skip = dock.above.len() - usize::from(above);
    draw_rows(frame, gutter(above_area), &dock.above[skip..]);
    draw_rows(
        frame,
        gutter(footer_area),
        std::slice::from_ref(&dock.footer),
    );
    let slot_area = gutter(slot_area);
    if let Some(sheet) = sheet {
        panel::render(frame, slot_area, styles, &sheet);
        return None;
    }
    draw_rows(frame, slot_area, &dock.editor);
    // The composer has the keys while no view or picker does; a completion
    // list is typed into through it.
    if matches!(
        app.overlay,
        Overlay::None | Overlay::Palette { .. } | Overlay::Files { .. }
    ) && slot_area.height > 0
    {
        app.composer_max_rows = composer::max_rows(area.height);
        app.composer_area = Rect {
            height: slot_area.height.min(dock.editor_rows),
            ..slot_area
        };
        return Some(Position {
            x: (slot_area.x + dock.cursor.1).min(area.right().saturating_sub(1)),
            y: slot_area.y + dock.cursor.0.min(slot_area.height - 1),
        });
    }
    None
}

/// `area` without the left gutter column, which the dock never paints.
fn gutter(area: Rect) -> Rect {
    let inset = area.width.min(1);
    Rect {
        x: area.x + inset,
        width: area.width - inset,
        ..area
    }
}

fn draw_rows(frame: &mut Frame, area: Rect, rows: &[Line<'static>]) {
    for (y, line) in (area.y..area.bottom()).zip(rows) {
        put_line(
            frame.buffer_mut(),
            Rect {
                y,
                height: 1,
                ..area
            },
            line,
        );
    }
}

/// `Line::render` for an unaligned line, minus ratatui's grapheme walk over
/// printable ASCII and lone narrow characters (the rail), which is nearly every
/// span of a frame.
pub fn put_line(buf: &mut Buffer, area: Rect, line: &Line) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    if line.style != Style::default() {
        if line.spans.iter().all(|span| span.content.is_empty()) {
            return;
        }
        buf.set_style(area, line.style);
    }
    let mut x = area.x;
    for span in &line.spans {
        if one_cell_per_char(&span.content) {
            let room = usize::from(area.right() - x);
            let start = buf.index_of(x, area.y);
            let mut shown = 0;
            for (cell, ch) in buf.content[start..]
                .iter_mut()
                .zip(span.content.chars().take(room))
            {
                cell.set_char(ch).set_style(span.style);
                shown += 1;
            }
            x += shown;
        } else {
            let rest = Rect {
                x,
                width: area.right() - x,
                ..area
            };
            span.render(rest, buf);
            x = (x + span.width() as u16).min(area.right());
        }
        if x == area.right() {
            break;
        }
    }
}

/// Whether each char of `text` is a cell of its own: printable ASCII, or one
/// narrow character. Longer non-ASCII text can hold clusters wider than their chars.
fn one_cell_per_char(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => only.width() == Some(1),
        _ => text.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
    }
}

/// A token count for the UI: `950`, `12.3k`, `200k`, `1.8M`. Whole thousands and
/// millions drop the `.0` so a budget reads as `200k`, not `200.0k`.
pub fn format_tokens(value: u64) -> String {
    fn scaled(value: f64, suffix: &str) -> String {
        let text = format!("{value:.1}");
        let text = text.strip_suffix(".0").unwrap_or(&text);
        format!("{text}{suffix}")
    }
    match value {
        value if value < 1_000 => value.to_string(),
        value if value < 1_000_000 => scaled(value as f64 / 1_000.0, "k"),
        value => scaled(value as f64 / 1_000_000.0, "M"),
    }
}

fn draw_fatal(frame: &mut Frame, area: Rect, fatal: &crate::app::Fatal) {
    let styles = Styles::of(theme::Palette::dark());
    let mut lines = vec![
        Line::styled("Fatal error", styles.failure()),
        Line::raw(""),
        Line::styled(fatal.reason.clone(), styles.text()),
    ];
    if !fatal.stderr_tail.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("agent stderr (tail)", styles.dim()));
        for line in fatal.stderr_tail.iter().rev().take(20).rev() {
            lines.push(Line::styled(
                wrap::sanitize(line).into_owned(),
                styles.faint(),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Press q to quit", styles.dim()));
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};
    use ratatui::text::Span;

    use super::*;

    #[test]
    fn token_counts_drop_a_trailing_zero() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_300), "12.3k");
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_800_000), "1.8M");
    }

    #[test]
    fn put_line_paints_what_the_line_widget_paints() {
        let red = Style::default().fg(Color::Red);
        let lines = [
            Line::from(vec![Span::styled("▎", red), Span::raw("plain text")]),
            Line::from(vec![Span::raw("日本語 mixed"), Span::styled("tail", red)]),
            Line::styled("whole row", Style::default().bg(Color::Blue)),
            Line::styled("", Style::default().bg(Color::Blue)),
            Line::from(vec![
                Span::raw("ab"),
                Span::raw("xyz "),
                Span::raw("clipped"),
            ]),
            Line::from(vec![Span::raw("a日"), Span::raw("bcdef")]),
            Line::from(vec![
                Span::raw("日"),
                Span::raw("e\u{301}x"),
                Span::raw("▎│"),
            ]),
            Line::from(Span::styled(
                "bold",
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ];
        for width in [4, 7, 20] {
            for line in &lines {
                let area = Rect::new(0, 0, width, 1);
                let (mut fast, mut slow) = (Buffer::empty(area), Buffer::empty(area));
                put_line(&mut fast, area, line);
                line.render(area, &mut slow);
                assert_eq!(fast, slow, "{line:?} at width {width}");
            }
        }
    }
}
