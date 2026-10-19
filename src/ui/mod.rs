//! Rendering from `&App`: the transcript or a full-screen view, an optional
//! bottom sheet, then the status band. Only the transcript's layout cache and
//! the rows recorded for copy are written.

pub mod blocks;
pub mod composer;
pub mod files;
pub mod markdown;
pub mod panel;
pub mod picker;
pub mod rails;
pub mod screens;
pub mod selection;
pub mod sheets;
#[cfg(test)]
pub mod snapshots;
pub mod status;
pub mod theme;
pub mod transcript;
pub mod views;
pub mod wrap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;

use crate::app::App;

use self::theme::Styles;

/// Draws one frame. Fatal errors take the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if let Some(fatal) = &app.fatal {
        draw_fatal(frame, area, fatal);
        return;
    }
    let styles = Styles::of(app.palette);
    // An approval prompt is a sheet even though it does not open an overlay:
    // it owns the keyboard until answered, so it must be visible.
    let approval = app.overlay.is_none() && app.has_interaction();
    let sheet = if approval {
        Some(screens::approval(app, &app.palette))
    } else {
        screens::sheet(app, &app.palette)
    };
    let (rows, cursor) = status::band(app, styles, area.width);
    let sheet_rows = sheet.as_ref().map_or(0, panel::Sheet::height);
    // One blank row keeps the last transcript line off the stack below it,
    // when the terminal has the room.
    let gap = u16::from(area.height > 8);
    let bottom = (rows.len() as u16 + sheet_rows)
        .min(area.height.saturating_sub(1 + gap))
        .max(1);
    let [main, _, bottom] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(gap),
        Constraint::Length(bottom),
    ])
    .areas(area);
    if !screens::draw_view(frame, main, app) {
        transcript::draw(frame, main, app, styles);
    }
    let band = if let Some(sheet) = sheet {
        let [sheet_area, band] =
            Layout::vertical([Constraint::Length(sheet.height()), Constraint::Min(1)])
                .areas(bottom);
        panel::render(frame, sheet_area, &app.palette, &sheet);
        band
    } else {
        bottom
    };
    // Row by row: a `Line` widget paints its style across the whole row, so
    // the composer's tint is a full-width band, not just the typed cells.
    for (index, line) in rows.iter().enumerate() {
        let y = band.y + index as u16;
        if y >= band.bottom() {
            break;
        }
        frame.render_widget(
            line,
            Rect {
                y,
                height: 1,
                ..band
            },
        );
    }
    if app.overlay.is_none() && !app.has_interaction() {
        frame.set_cursor_position((
            band.x + cursor.1,
            band.y + cursor.0.min(band.height.saturating_sub(1)),
        ));
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
    let styles = Styles::of(crate::theme::Palette::dark());
    let mut lines = vec![
        Line::styled("Fatal error", styles.error()),
        Line::raw(""),
        Line::styled(fatal.reason.clone(), styles.text()),
    ];
    if !fatal.stderr_tail.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("agent stderr (tail)", styles.dim()));
        for line in fatal.stderr_tail.iter().rev().take(20).rev() {
            lines.push(Line::styled(
                wrap::sanitize(line).into_owned(),
                styles.subtle(),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Press q to quit", styles.dim()));
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::format_tokens;

    #[test]
    fn token_counts_drop_a_trailing_zero() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_300), "12.3k");
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_800_000), "1.8M");
    }
}
