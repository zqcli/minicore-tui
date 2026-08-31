//! Dock geometry and small line-layout helpers shared by the renderers
//! (development spec 14, 21, 31).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::markdown::{char_width, column_width, line_width};
use crate::state::selection::Dock;

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

/// The wrapped content rows of the composer buffer (minimum 1 for the
/// placeholder), using the same `wrap_plain` width math as the renderer.
pub fn composer_content_rows(app: &App, width: u16) -> usize {
    let width = width.max(1) as usize;
    if app.composer.is_empty() {
        return 1;
    }
    app.composer
        .lines()
        .iter()
        .map(|line| crate::markdown::wrap_plain(line, width, Style::new()).len())
        .sum()
}

/// The dock height the composer occupies: it grows with the wrapped
/// content up to 40% of the screen (spec 21.2) and stays a fixed 3-row
/// bar on short screens or while a turn is running.
pub fn composer_height_phase5(app: &App, width: u16, screen_height: u16, short: bool) -> u16 {
    if short {
        return 3;
    }
    if app.active_view().is_some_and(|view| view.live.is_some()) {
        return 3 + 2;
    }
    let max_rows = (screen_height as usize * 2) / 5; // 40%
    // The renderer wraps content inside the 1-cell borders, so the height
    // estimate must use the inner width (never underestimate at the
    // 79/80-column boundary, spec 21.2).
    let rows = composer_content_rows(app, width.saturating_sub(2)).min(max_rows.max(3));
    (rows.max(3) + 2).min(screen_height as usize) as u16
}

/// Help/Logs panels take at most 60% of the screen (spec 24.2).
pub fn help_panel_height(screen_height: u16) -> u16 {
    (screen_height * 6 / 10).clamp(4, screen_height)
}

/// Selector / new-session panel height: 8-14 rows, short terminals get the
/// minimum (spec 24.2). The panel replaces the composer in the dock.
pub fn panel_height(short: bool) -> u16 {
    if short { 8 } else { 14 }
}

/// Total dock height for the current app state and terminal size; used by
/// both the renderer and the main loop's viewport measurement so they can
/// never disagree.
pub fn dock_rows(app: &App, width: u16, screen_height: u16) -> u16 {
    let short = screen_height < 24;
    let busy = busy(app);
    let panel = match &app.dock {
        Dock::Composer => composer_height_phase5(app, width, screen_height, short),
        Dock::Help | Dock::Logs => help_panel_height(screen_height),
        _ => panel_height(short),
    };
    let notice = u16::from(!app.notices.is_empty());
    let status = u16::from(busy);
    status + notice + panel + footer_height(width, screen_height)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::event::AppEvent;
    use crate::theme::ThemeKind;
    use std::path::PathBuf;

    fn app() -> App {
        let mut app = App::new(PathBuf::from("/ws"));
        app.update(AppEvent::SetTheme(ThemeKind::Dark));
        app
    }

    fn typed(app: &mut App, text: &str) {
        app.update(AppEvent::Terminal(crossterm::event::Event::Paste(
            text.to_owned(),
        )));
    }

    #[test]
    fn seventy_nine_column_content_needs_two_visual_rows_at_inner_width_78() {
        let mut app = app();
        typed(&mut app, &"x".repeat(90));
        // 79 columns inside the 78-wide inner area wrap to two rows; the
        // estimate must not undercount because the caller passed 80.
        let seventy_nine = "y".repeat(79);
        app.composer.set_text(&seventy_nine);
        assert_eq!(
            composer_content_rows(&app, 78),
            2,
            "inner width wraps 79 cols"
        );
        assert_eq!(composer_height_phase5(&app, 80, 24, false), 3 + 2);
        let eighty = "y".repeat(80);
        app.composer.set_text(&eighty);
        assert_eq!(composer_content_rows(&app, 78), 2);
        assert_eq!(
            composer_content_rows(&app, 80),
            1,
            "outer width would underestimate"
        );
    }

    #[test]
    fn composer_height_caps_at_forty_percent_and_short_is_fixed() {
        let mut app = app();
        typed(
            &mut app,
            &(0..60).map(|_| "full width line\n").collect::<String>(),
        );
        let height = composer_height_phase5(&app, 80, 40, false);
        assert!(
            height <= 40 * 2 / 5 + 2,
            "height caps around 40% of the screen"
        );
        assert!(height >= 8);
        let short = composer_height_phase5(&app, 80, 24, true);
        assert_eq!(short, 3, "short terminals keep the fixed 3-row bar");
        // Running keeps the fixed 5-row composer regardless of content.
        let running = crate::ui::testapp::live_turn(ThemeKind::Dark);
        assert_eq!(composer_height_phase5(&running, 80, 24, false), 3 + 2);
    }

    #[test]
    fn dock_rows_derives_status_notice_panel_and_footer_consistently() {
        let a = app();
        assert_eq!(
            dock_rows(&a, 80, 24),
            7,
            "idle fresh app: composer 5 + footer 2"
        );
        assert_eq!(
            dock_rows(&a, 60, 16),
            4,
            "short: 3-row composer + 1-row footer"
        );
    }
}
