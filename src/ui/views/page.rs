//! Shared single-pane scrollable full-screen page template reused by
//! `F1` Help, `/error`, `/context` and `/session`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::ui::chrome;
use crate::ui::theme::Styles;

use super::chrome as draw_chrome;

/// Renders a full-screen single-pane view with `← Title`, optional top
/// `/ filter▌` bar, scrollable content with right-edge scrollbar, and bottom
/// key hints. Returns `(clamped_scroll, visible_rows)`.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    title: &str,
    hints: &[(&str, &str)],
    filter: Option<(&str, &str)>,
    lines: &[Line<'static>],
    scroll: usize,
) -> (usize, usize) {
    draw_chrome(frame, area, styles, title, hints);
    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    if body.is_empty() {
        return (0, 0);
    }
    let content = match filter {
        Some((query, placeholder)) => {
            chrome::draw_filter_header(frame, body, query, placeholder, true, styles)
        }
        None => Rect {
            y: body.y + 1,
            height: body.height.saturating_sub(1),
            ..body
        },
    };
    if content.is_empty() {
        return (0, 0);
    }
    let clamped = chrome::render_scrollable_lines(frame, content, lines, scroll, styles);
    (clamped, usize::from(content.height))
}
