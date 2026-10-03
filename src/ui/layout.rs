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
    pub right_gap: Rect,
    /// Dedicated page column; only scrollable main-view rows paint it.
    pub scrollbar: Rect,
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

impl ScreenLayout {
    /// Align a main view's track with its scrollable body, not its header or dock.
    pub fn scrollbar_for(self, body: Rect) -> Rect {
        fit_scrollbar(self.scrollbar, body)
    }
}

pub(crate) fn fit_scrollbar(track: Rect, body: Rect) -> Rect {
    track.intersection(Rect::new(track.x, body.y, track.width, body.height))
}

/// Stable page columns, independent of scrollbar visibility. At 80 columns:
/// gutter 0, body 1..=77, gap 78, scrollbar 79. Tiny widths saturate safely.
pub(crate) fn page_columns(area: Rect) -> [Rect; 4] {
    use crate::ui::rail::{APP_GUTTER_WIDTH, APP_RIGHT_GAP_WIDTH, APP_SCROLLBAR_WIDTH};
    let left = area.width.min(APP_GUTTER_WIDTH);
    let remaining = area.width - left;
    let track = remaining.min(APP_SCROLLBAR_WIDTH);
    let gap = (remaining - track).min(APP_RIGHT_GAP_WIDTH);
    let body = remaining - track - gap;
    [
        Rect::new(area.x, area.y, left, area.height),
        Rect::new(area.x + left, area.y, body, area.height),
        Rect::new(area.x + left + body, area.y, gap, area.height),
        Rect::new(area.x + left + body + gap, area.y, track, area.height),
    ]
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
    let [gutter, content, right_gap, scrollbar] = page_columns(area);
    let short = content.height < 24;
    let panel = match &app.dock {
        Dock::Composer => composer_height_phase5(app, content.width, content.height, short)
            .saturating_add(composer_completion_rows_for_height(app, content.height)),
        Dock::Help | Dock::Logs => help_panel_height(content.height),
        // The search panel is taller while results are listed so the
        // transcript above it stays visible (spec §17.1).
        Dock::Search(_) => search_panel_height(content.height),
        Dock::Workspace(_) => search_panel_height(content.height).max(8),
        // The export form is a compact fixed-height form.
        Dock::Export(form) => crate::ui::export::desired_height(form, content.width)
            .min(panel_height(short).saturating_add(4)),
        // Settings needs one row per fixed field plus the footer.
        Dock::Settings(_) => panel_height(short).saturating_add(7),
        Dock::SessionSelector(state) => {
            use crate::state::selection::SessionPanelMode;
            let rows = match &state.mode {
                SessionPanelMode::Browse => panel_height(short),
                SessionPanelMode::Rename { .. } => 6,
                SessionPanelMode::ConfirmClose | SessionPanelMode::ConfirmCloseForDelete => 7,
                SessionPanelMode::ConfirmDelete { .. } => 8,
            };
            rows + u16::from(
                state.error.is_some() && !matches!(state.mode, SessionPanelMode::Browse),
            )
        }
        Dock::NewSession(_) => panel_height(short).min(10),
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
        right_gap,
        scrollbar,
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

/// The compact menu reserves a shortcut row and a header for multi-row lists
/// or explicit dropdowns. Singleton objects need no repeated heading. Its body is
/// capped using the actual terminal height, including on the first render
/// before the app receives a resize event.
pub fn composer_completion_rows_for_height(app: &App, screen_height: u16) -> u16 {
    app.slash_completion.as_ref().map_or(0, |completion| {
        let visible = completion
            .items
            .len()
            .clamp(1, completion.visible_limit_for_height(screen_height));
        (visible
            + 1
            + usize::from(completion.popup.is_some() || completion.items.len() != 1)
            + usize::from(completion.parameter_hint().is_some())) as u16
    })
}

/// Event handlers use the last observed terminal size; renderers should pass
/// their frame height to `composer_completion_rows_for_height` instead.
pub fn composer_completion_rows(app: &App) -> u16 {
    composer_completion_rows_for_height(app, app.terminal_size().1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlashCompletionGeometry {
    pub show_header: bool,
    pub show_hint: bool,
    pub show_parameter: bool,
    pub start: usize,
    pub end: usize,
}

/// Shared scroll window and row allocation for drawing and mouse hit tests.
/// The dropdown replaces the compact menu body, so both use this geometry.
pub fn slash_completion_geometry(
    completion: &crate::app::SlashCompletionState,
    area_height: u16,
    screen_height: u16,
) -> SlashCompletionGeometry {
    let show_header =
        area_height >= 3 && (completion.popup.is_some() || completion.items.len() != 1);
    let show_hint = area_height >= 2;
    let show_parameter = completion.parameter_hint().is_some() && area_height >= 3;
    let available = (area_height as usize).saturating_sub(
        usize::from(show_header) + usize::from(show_hint) + usize::from(show_parameter),
    );
    let visible = available.min(completion.visible_limit_for_height(screen_height));
    let start = completion
        .selected
        .saturating_sub(visible / 2)
        .min(completion.items.len().saturating_sub(visible));
    SlashCompletionGeometry {
        show_header,
        show_hint,
        show_parameter,
        start,
        end: (start + visible).min(completion.items.len()),
    }
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

    #[test]
    fn page_columns_partition_translated_and_tiny_rectangles() {
        for x in [0, 7] {
            for width in 0..=120 {
                let area = Rect::new(x, 3, width, 16);
                let columns = page_columns(area);
                let mut next = area.x;
                for column in columns {
                    assert_eq!(column.x, next);
                    assert_eq!((column.y, column.height), (area.y, area.height));
                    next = column.right();
                }
                assert_eq!(next, area.right());
                assert_eq!(columns[1].width, width.saturating_sub(3));
                if width >= 3 {
                    assert_eq!(columns[0].width, 1);
                    assert_eq!(columns[2].width, 1);
                    assert_eq!(columns[3].width, 1);
                }
            }
        }
    }

    #[test]
    fn dock_and_main_views_share_body_edges_and_page_scrollbar_column() {
        for (width, height) in [(60, 16), (80, 24), (120, 40)] {
            for dock in [Dock::Composer, Dock::Help, Dock::Logs] {
                let mut app = crate::ui::testapp::fresh(ThemeKind::Dark);
                app.dock = dock;
                app.update(AppEvent::TerminalSize { width, height });
                let screen = screen_layout(&app, Rect::new(0, 0, width, height));
                assert_eq!(app.terminal_content_width(), width - 3);
                assert_eq!(screen.content, Rect::new(1, 0, width - 3, height));
                for body in [screen.transcript, screen.dock, screen.panel, screen.footer] {
                    assert_eq!(
                        (body.x, body.width),
                        (screen.content.x, screen.content.width)
                    );
                    assert_eq!(body.right(), screen.right_gap.x);
                }
                assert_eq!(screen.right_gap, Rect::new(width - 2, 0, 1, height));
                assert_eq!(screen.scrollbar, Rect::new(width - 1, 0, 1, height));
                for body in [
                    screen.transcript,
                    crate::ui::workspace::file_body(screen.transcript),
                    crate::ui::tool_detail::body_area(screen.transcript),
                ] {
                    let track = screen.scrollbar_for(body);
                    assert_eq!(track, Rect::new(width - 1, body.y, 1, body.height));
                    assert!(!body.intersects(track));
                }
            }
        }
    }
    fn app() -> App {
        let mut app = App::new(PathBuf::from("/ws"));
        app.update(AppEvent::SetTheme(ThemeKind::Dark));
        app
    }

    fn typed(app: &mut App, text: &str) {
        app.composer.type_text(text);
    }

    fn type_command(app: &mut App, text: &str) {
        for character in text.chars() {
            app.update(AppEvent::Terminal(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(character),
                    crossterm::event::KeyModifiers::NONE,
                ),
            )));
        }
    }

    #[test]
    fn compact_menu_geometry_shares_selection_window_at_every_height() {
        let mut app = app();
        type_command(&mut app, "/");
        for (height, limit) in [(16, 3), (24, 5), (48, 5)] {
            let rows = composer_completion_rows_for_height(&app, height);
            assert_eq!(rows, limit + 2);
            let completion = app.slash_completion.as_mut().unwrap();
            for selected in 0..completion.items.len() {
                completion.selected = selected;
                let geometry = slash_completion_geometry(completion, rows, height);
                assert!(geometry.show_header && geometry.show_hint);
                assert_eq!(geometry.end - geometry.start, limit as usize);
                assert!((geometry.start..geometry.end).contains(&selected));
            }
        }
    }

    #[test]
    fn zero_height_completion_has_no_hit_test_rows() {
        let mut app = app();
        type_command(&mut app, "/");
        let completion = app.slash_completion.as_ref().unwrap();
        let geometry = slash_completion_geometry(completion, 0, 16);
        assert!(!geometry.show_header && !geometry.show_hint && !geometry.show_parameter);
        assert_eq!(geometry.start, geometry.end);
    }

    #[test]
    fn singleton_object_uses_one_body_row_without_repeated_heading() {
        let mut app = app();
        type_command(&mut app, "/session");
        let completion = app.slash_completion.as_ref().unwrap();
        assert_eq!(completion.items.len(), 1);
        for height in [16, 24, 48] {
            let rows = composer_completion_rows_for_height(&app, height);
            assert_eq!(rows, 2);
            let geometry = slash_completion_geometry(completion, rows, height);
            assert!(!geometry.show_header);
            assert!(geometry.show_hint);
            assert_eq!((geometry.start, geometry.end), (0, 1));
        }
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

/// Keep the insertion point visible without clipping a wide Unicode character.
pub(crate) fn text_window(
    text: &str,
    cursor: usize,
    width: usize,
    style: Style,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let display_char = |character: char| if character == '\n' { '↵' } else { character };
    let mut characters = text[cursor..].chars();
    let character = display_char(characters.next().unwrap_or(' '));
    let mut cursor_text = character.to_string();
    if crate::markdown::char_width(character) == 0 {
        cursor_text.insert(0, ' ');
    } else if crate::markdown::char_width(character) > width {
        // One-cell viewports cannot display a CJK cursor glyph intact.
        cursor_text = " ".to_owned();
    }
    let cursor_width = crate::markdown::column_width(&cursor_text);
    let available = width.saturating_sub(cursor_width);
    let start_for = |budget: usize| {
        let mut start = cursor;
        let mut used = 0;
        for (index, character) in text[..cursor].char_indices().rev() {
            let next = crate::markdown::char_width(display_char(character));
            if used + next > budget {
                break;
            }
            used += next;
            start = index;
        }
        start
    };
    let mut start = start_for(available);
    let prefix = if start > 0 && available > 0 {
        start = start_for(available - 1);
        "…"
    } else {
        ""
    };
    let before = text[start..cursor]
        .chars()
        .map(display_char)
        .collect::<String>();
    let used = crate::markdown::column_width(prefix)
        + crate::markdown::column_width(&before)
        + cursor_width;
    let remaining = width.saturating_sub(used);
    // Only materialize enough suffix characters for this visible window.
    let after = characters
        .take(remaining + 1)
        .map(display_char)
        .collect::<String>();
    vec![
        Span::styled(format!("{prefix}{before}"), style),
        Span::styled(
            cursor_text,
            style.add_modifier(ratatui::style::Modifier::REVERSED),
        ),
        Span::styled(truncate(&after, remaining), style),
    ]
}

/// Safe single-line projection plus the terminal cell of the insertion point.
/// Byte cursors remain in the raw field; only display text is sanitized.
pub(crate) fn single_line_window(
    text: &str,
    cursor: usize,
    width: usize,
    style: Style,
) -> (Vec<Span<'static>>, usize) {
    let cursor = crate::state::text_input::boundary(text, cursor);
    let safe = |text: &str| {
        crate::safe_text::safe_display(text)
            .replace('\t', "    ")
            .replace('\n', "↵")
    };
    let before = safe(&text[..cursor]);
    let safe_cursor = before.len();
    let display = before + &safe(&text[cursor..]);
    let spans = text_window(&display, safe_cursor, width, style);
    let cell = spans.first().map_or(0, Span::width);
    (spans, cell)
}

#[cfg(test)]
mod single_line_input_tests {
    use super::*;
    #[test]
    fn safe_single_line_window_keeps_every_utf8_cursor_in_bounds() {
        for text in [
            "",
            "ascii",
            "中🙂e\u{301}",
            "👨‍👩‍👧‍👦tail",
            "\t中",
            "unsafe\u{202e}text",
            "a\u{301}\u{301}\u{301}",
        ] {
            for cursor in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                for width in [1, 2, 3, 8, 47, 140] {
                    let (spans, cell) = single_line_window(text, cursor, width, Style::default());
                    assert!(
                        spans.iter().map(Span::width).sum::<usize>() <= width,
                        "{text:?} {cursor} {width}"
                    );
                    assert!(cell < width, "cursor must remain visible");
                    for span in spans {
                        assert!(
                            !span
                                .content
                                .chars()
                                .any(crate::safe_text::is_unsafe_display_control)
                        );
                        assert!(!span.content.contains(['\t', '\n']));
                    }
                }
            }
        }
    }
    #[test]
    fn long_single_line_window_shows_the_edited_middle_and_tail() {
        let text = format!("{}中🙂tail", "prefix/".repeat(100));
        for width in [47, 140] {
            let (spans, _) = single_line_window(&text, text.len(), width, Style::default());
            let rendered = spans.iter().map(|s| s.content.as_ref()).collect::<String>();
            assert!(rendered.starts_with('…'));
            assert!(rendered.ends_with("中🙂tail "));
            let cursor = text.find('中').unwrap();
            let (spans, _) = single_line_window(&text, cursor, width, Style::default());
            assert_eq!(spans[1].content, "中");
        }
    }
}
