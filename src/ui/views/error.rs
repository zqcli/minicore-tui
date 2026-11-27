//! Complete terminal-safe error text, independent of transient notices,
//! rendered through the shared [`super::page`] scrollable page engine.

use ratatui::{Frame, layout::Rect};

use super::page;
use crate::ui::chrome;
use crate::ui::theme::Styles;
use crate::wrap;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    text: &str,
    scroll: &mut usize,
    visible_rows: &mut usize,
) {
    let wrap_w = usize::from(area.width.saturating_sub(2)).max(1);
    let lines: Vec<_> = wrap::wrap(&wrap::sanitize(text), wrap_w)
        .into_iter()
        .map(|row| chrome::styled(&row, styles.text()))
        .collect();
    let (clamped, rows) = page::render(
        frame,
        area,
        styles,
        "Error details",
        &[
            ("↑↓/pgup/pgdn", "scroll"),
            ("c", "copy full message"),
            ("r", "refresh view"),
            ("esc", "back"),
        ],
        None,
        &lines,
        *scroll,
    );
    *scroll = clamped;
    *visible_rows = rows;
}
