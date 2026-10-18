//! Rendering. `ui` draws from `&App` (or the parts it needs) and never mutates
//! domain state; the only thing it owns is the layout cache.

pub mod files;
pub mod panel;
pub mod picker;
pub mod selection;
pub mod sheets;
pub mod views;
pub mod wrap;

use ratatui::Frame;

use crate::app::App;

/// Draws one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Block::new(), area);
    let palette = app.palette;
    let line = ratatui::text::Line::styled("minicore-tui", palette.text());
    frame.render_widget(
        ratatui::widgets::Paragraph::new(vec![
            line,
            ratatui::text::Line::styled(format!("{}x{}", area.width, area.height), palette.dim()),
        ]),
        area,
    );
}
