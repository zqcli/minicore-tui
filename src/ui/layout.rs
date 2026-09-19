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

/// Clear only graphemes straddling an overlay edge, retaining their background.
pub(crate) fn clear_wide_overlay_edges(buffer: &mut ratatui::buffer::Buffer, area: Rect) {
    if area.width == 0 {
        return;
    }
    for row in area.y..area.bottom() {
        let background = area.x.checked_sub(1).and_then(|column| {
            let cell = buffer.cell_mut((column, row))?;
            if unicode_width::UnicodeWidthStr::width(cell.symbol()) <= 1 {
                return None;
            }
            let background = cell.bg;
            cell.set_symbol(" ")
                .set_style(Style::reset().bg(background));
            Some(background)
        });
        if let Some(background) = background {
            if let Some(cell) = buffer.cell_mut((area.x, row)) {
                cell.set_bg(background);
            }
        }
        let background = buffer.cell((area.right() - 1, row)).and_then(|cell| {
            (unicode_width::UnicodeWidthStr::width(cell.symbol()) > 1).then_some(cell.bg)
        });
        if let Some(background) = background {
            if let Some(cell) = buffer.cell_mut((area.right(), row)) {
                cell.set_symbol(" ")
                    .set_style(Style::reset().bg(background));
            }
        }
    }
}

pub fn is_too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenLayout {
    pub gutter: Rect,
    pub content: Rect,
    pub transcript: Rect,
    pub dock: Rect,
    pub status: Option<Rect>,
    pub notice: Option<Rect>,
    /// Gray `Steering: …` queue rows in the dock, ABOVE the status
    /// row (participates in viewport/hit/editor/footer geometry).
    pub queue: Option<Rect>,
    pub panel: Rect,
    pub footer: Rect,
}

/// Bound for the dock's steering queue display; more entries are summarized
/// with an overflow line so 60x16 never consumes unbounded height.
pub const MAX_DOCK_QUEUE_ROWS: u16 = 3;

/// The number of pending steering queue entries for the active session (the
/// single source of truth shared by the height reservation and the renderer):
/// locally-unsent plus in-flight (accepted-but-not-applied) entries.
pub fn steer_queue_count(app: &App) -> usize {
    let Some(view) = app.active_view() else {
        return 0;
    };
    let inflight = view
        .live
        .as_ref()
        .map(|live| {
            live.pending_steers
                .iter()
                .filter(|steer| {
                    !matches!(
                        steer.state,
                        crate::state::turn::PendingSteerState::Persisted
                            | crate::state::turn::PendingSteerState::NotRecorded
                    )
                })
                .count()
        })
        .unwrap_or(0);
    view.steer_queue.len() + inflight
}

/// Rows the dock reserves for the pending steering queue: up to
/// `MAX_DOCK_QUEUE_ROWS` content lines, one overflow line when there are more,
/// one functional Alt+Up hint whenever a withdrawable unsent item exists
/// (paused is only a prefix), and exactly one blank gap. 0 when empty or when a
/// modal/selector owns the dock (the queue belongs to the composer surface and
/// must not squeeze selectors at 60x16).
pub fn steer_queue_rows(app: &App) -> u16 {
    if !matches!(app.dock, Dock::Composer) {
        return 0;
    }
    let count = steer_queue_count(app);
    if count == 0 {
        return 0;
    }
    let content = count.min(MAX_DOCK_QUEUE_ROWS as usize) as u16;
    let overflow = u16::from(count > MAX_DOCK_QUEUE_ROWS as usize);
    let hint = u16::from(crate::ui::steer_queue::hint_label(app).is_some());
    content + overflow + hint
}

/// Computes the complete normal-screen geometry once. Rendering and the
/// viewport/hit-test callers can use these same rectangles instead of
/// independently re-deriving gutter, dock, and footer boundaries.
pub fn screen_layout(app: &App, area: Rect) -> ScreenLayout {
    let [gutter, content] = ratatui::layout::Layout::horizontal([
        ratatui::layout::Constraint::Length(crate::ui::rail::APP_GUTTER_WIDTH),
        ratatui::layout::Constraint::Min(1),
    ])
    .areas(area);
    let short = content.height < 24;
    let panel = match &app.dock {
        Dock::Composer => composer_height_phase5(app, content.width, content.height, short)
            .saturating_add(composer_completion_rows(app)),
        Dock::Help | Dock::Logs => help_panel_height(content.height),
        // The search panel is taller while results are listed so the
        // transcript above it stays visible (spec §17.1).
        Dock::Search(_) => search_panel_height(content.height),
        Dock::SessionSelector(state) => panel_height(short).saturating_add(u16::from(!matches!(
            &state.mode,
            crate::state::selection::SessionPanelMode::Browse
        ))),
        _ => panel_height(short),
    };
    let footer_height = footer_height(content.width, content.height);
    let status_height = u16::from(busy(app));
    let notice_height = u16::from(!app.notices.is_empty());
    let queue_height = steer_queue_rows(app);
    // One explicit blank row separates the queue from the Working status
    // (the reference requires queue.bottom < status.y, so the gap is not part
    // of the queue rect).
    let queue_gap = u16::from(queue_height > 0);
    let dock_height =
        status_height + notice_height + queue_height + queue_gap + panel + footer_height;
    let [transcript, dock] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(dock_height),
    ])
    .areas(content);
    let mut rows = Vec::new();
    if queue_height > 0 {
        // The gray queue sits ABOVE the Working status row (per the user's
        // reference image), followed by exactly one blank gap row.
        rows.push(ratatui::layout::Constraint::Length(queue_height));
        rows.push(ratatui::layout::Constraint::Length(1));
    }
    if status_height == 1 {
        rows.push(ratatui::layout::Constraint::Length(1));
    }
    if notice_height == 1 {
        rows.push(ratatui::layout::Constraint::Length(1));
    }
    rows.push(ratatui::layout::Constraint::Length(panel));
    rows.push(ratatui::layout::Constraint::Length(footer_height));
    let chunks = ratatui::layout::Layout::vertical(rows).split(dock);
    let mut index = 0;
    let queue = (queue_height > 0).then(|| {
        let rect = chunks[index];
        index += 1;
        rect
    });
    if queue_height > 0 {
        // Skip the explicit blank gap row reserved between queue and status.
        index += 1;
    }
    let status = (status_height == 1).then(|| {
        let rect = chunks[index];
        index += 1;
        rect
    });
    let notice = (notice_height == 1).then(|| {
        let rect = chunks[index];
        index += 1;
        rect
    });
    let panel_rect = chunks[index];
    index += 1;
    let footer = chunks[index];
    ScreenLayout {
        gutter,
        content,
        transcript,
        dock,
        status,
        notice,
        queue,
        panel: panel_rect,
        footer,
    }
}

/// True while the active session needs a status row for live state. A normal
/// completed result is retained for details/history but does not reserve a
/// permanent conversation row.
pub fn busy(app: &App) -> bool {
    app.active_view().is_some_and(|view| {
        view.live.is_some()
            || view.is_preparing()
            || view
                .state
                .as_ref()
                .is_some_and(|state| state.status != crate::protocol::SessionStatusWire::Idle)
    })
}

/// Minimum Rail editor surface height. The actual dock height is derived
/// from the buffer and terminal height by `composer_height_phase5`.
pub fn composer_height(_short: bool) -> u16 {
    crate::ui::rail::EDITOR_MIN_ROWS
}

/// The wrapped content rows of the composer buffer (minimum 1 for the
/// placeholder), using the same `wrap_plain` width math as the renderer.
pub fn composer_content_rows(app: &App, width: u16) -> usize {
    let width = width.max(1) as usize;
    let display = app.composer.display_content();
    let lines = display.split('\n').map(str::to_owned).collect::<Vec<_>>();
    crate::ui::editor_layout::EditorLayout::row_count_with_atomic_ranges(
        &lines,
        width,
        &app.composer.display_paste_markers(),
    )
}

/// The dock height the composer occupies. Rail has no border rows: the
/// surface itself is 4–12 rows, capped at roughly 32% of the terminal and
/// centered when the native editor body is shorter than the target.
pub fn composer_height_phase5(app: &App, width: u16, screen_height: u16, short: bool) -> u16 {
    let _ = short;
    let rows = composer_content_rows(app, width.saturating_sub(1));
    crate::ui::rail::editor_target_rows(rows, screen_height)
}

pub fn composer_completion_rows(app: &App) -> u16 {
    app.slash_completion.as_ref().map_or(0, |completion| {
        let visible = completion.items.len().min(5);
        (visible + usize::from(completion.items.len() > visible)) as u16
    })
}

/// Help/Logs panels take at most 60% of the screen (spec 24.2).
pub fn help_panel_height(screen_height: u16) -> u16 {
    (screen_height * 6 / 10).clamp(4, screen_height)
}

/// Search panel height: a one-line query, a coverage line, the bounded match
/// list, and one hint row. It never consumes more than 60% of the screen, so
/// the conversation stays visible while searching (spec §17.1).
pub fn search_panel_height(screen_height: u16) -> u16 {
    let desired = 4 + 10;
    desired
        .min((screen_height * 6 / 10).max(6))
        .min(screen_height)
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
    screen_layout(
        app,
        Rect {
            x: 0,
            y: 0,
            width,
            height: screen_height,
        },
    )
    .dock
    .height
}

/// Rail footer is always one row. Narrow terminals truncate the two aligned
/// groups instead of switching to a second row.
pub fn footer_height(width: u16, height: u16) -> u16 {
    let _ = (width, height);
    1
}

/// Prepends `width` blank cells to a line.
pub fn left_pad(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(width))];
    spans.extend(line.spans);
    Line::from(spans)
}

/// Wraps non-empty section content with one blank row above and below.
pub fn vertical_section(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut section = Vec::with_capacity(lines.len() + 2);
    section.push(Line::default());
    section.extend(lines);
    section.push(Line::default());
    section
}

/// Appends one vertically padded section while sharing an adjacent blank
/// boundary with the previous section. Each content run still has one blank
/// row above and below, without accumulating duplicate spacer rows.
pub fn append_section(out: &mut Vec<Line<'static>>, mut section: Vec<Line<'static>>) {
    if section.is_empty() {
        return;
    }
    if shares_blank_boundary(out, &section) {
        section.remove(0);
    }
    out.extend(section);
}

fn shares_blank_boundary(out: &[Line<'static>], section: &[Line<'static>]) -> bool {
    out.last().is_some_and(line_is_blank) && section.first().is_some_and(line_is_blank)
}

/// True only for a genuinely empty spacer row: styled spaces are filled
/// surface rows and must not be consumed as an external spacer. The frame
/// composer uses the same rule as [`append_section`].
pub(crate) fn line_is_blank(line: &Line<'_>) -> bool {
    is_transparent_blank(line)
}

/// True when a line is a genuinely empty spacer (no styled or filled
/// surface): the only row that can count as an external transparent gap.
pub(crate) fn is_transparent_blank(line: &Line<'_>) -> bool {
    line.spans.is_empty()
}

/// Appends background cells so the line is exactly `width` cells wide.
pub fn fill_line(line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    let fill = width.saturating_sub(line_width(&line));
    let mut spans = line.spans;
    spans.push(Span::styled(" ".repeat(fill), style));
    Line::from(spans)
}

/// One background-styled row: `text` truncated to `width`, then padding.
/// Control characters are escaped here as well, so every caller that builds
/// a row from backend text inherits the same safe-display boundary.
pub fn filled(text: &str, width: usize, style: Style) -> Line<'static> {
    let text = crate::safe_text::safe_display(text);
    let text = truncate(&text, width);
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
        app.composer.type_text(text);
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
        assert_eq!(composer_height_phase5(&app, 80, 24, false), 4);
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
        assert_eq!(short, 7, "short terminals use the responsive Rail maximum");
        // Running still caps the composer at the responsive maximum.
        let running = crate::ui::testapp::live_turn(ThemeKind::Dark);
        assert_eq!(composer_height_phase5(&running, 80, 24, false), 4);
    }

    #[test]
    fn dock_rows_derives_status_notice_panel_and_footer_consistently() {
        let a = app();
        assert_eq!(
            dock_rows(&a, 80, 24),
            5,
            "idle fresh app: four-row composer plus one-row footer"
        );
        assert_eq!(
            dock_rows(&a, 60, 16),
            5,
            "short: minimum composer plus one-row footer"
        );
    }
}
