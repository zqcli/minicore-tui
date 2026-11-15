//! Complete terminal-safe error text, independent of transient notices.

use ratatui::{Frame, layout::Rect};

use super::{chrome, styled};
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
    let lines = wrap::wrap(&wrap::sanitize(text), usize::from(area.width).max(1));
    let rows = usize::from(area.height.saturating_sub(2));
    *scroll = (*scroll).min(lines.len().saturating_sub(rows));
    *visible_rows = rows;
    chrome(
        frame,
        area,
        styles,
        "Error details",
        &[
            ("pgup/pgdn", "scroll"),
            ("c", "copy full message"),
            ("esc", "back"),
        ],
    );
    for (row, text) in lines.iter().skip(*scroll).take(rows).enumerate() {
        frame.buffer_mut().set_line(
            area.x,
            area.y + 1 + row as u16,
            &styled(text, styles.text()),
            area.width,
        );
    }
}
