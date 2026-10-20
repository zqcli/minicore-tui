//! Rendering from `&App`: the transcript or a full-screen view, an optional
//! the dock below it, where a bottom sheet takes the editor's place. Only the
//! transcript's layout cache and the rows recorded for copy are written.

pub mod blocks;
pub mod composer;
pub mod markdown;
pub mod panel;
#[cfg(test)]
mod perf_tests;
pub mod picker;
pub mod rails;
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

use crate::app::{App, Overlay};

use self::theme::Styles;

/// Draws one frame. Fatal errors take the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if let Some(fatal) = &app.fatal {
        draw_fatal(frame, area, fatal);
        return;
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
    let [main, above_area, slot_area, footer_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(above),
        Constraint::Length(slot),
        Constraint::Length(1),
    ])
    .areas(area);
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
        return;
    }
    draw_rows(frame, slot_area, &dock.editor);
    // The composer has the keys while no view or picker does; a completion
    // list is typed into through it.
    if matches!(
        app.overlay,
        Overlay::None | Overlay::Palette { .. } | Overlay::Files { .. }
    ) && slot_area.height > 0
    {
        frame.set_cursor_position((
            slot_area.x + dock.cursor.1,
            slot_area.y + dock.cursor.0.min(slot_area.height - 1),
        ));
    }
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
        frame.render_widget(
            line,
            Rect {
                y,
                height: 1,
                ..area
            },
        );
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
    use super::format_tokens;

    #[test]
    fn token_counts_drop_a_trailing_zero() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_300), "12.3k");
        assert_eq!(format_tokens(200_000), "200k");
        assert_eq!(format_tokens(1_800_000), "1.8M");
    }
}
