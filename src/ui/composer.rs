//! The composer (development spec 15.7, 21): a rounded border colored by the
//! active session's reasoning level, placeholder text, and the hardware
//! cursor positioned by `unicode-width` column math. Phase 3 holds the
//! minimal text/cursor state; tui-textarea editing arrives in Phase 5.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::App;
use crate::markdown::{column_width, wrap_plain};
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let view = app.active_view();
    let border_color = match view {
        Some(view) => theme.reasoning_color(view.info.reasoning),
        None => theme.thinking_disabled,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border_color).add_modifier(Modifier::BOLD));
    frame.render_widget(block, area);

    let inner = area.inner(Margin::new(1, 1));
    let running = view.is_some_and(|view| view.live.is_some());
    let mut content: Vec<Line<'static>> = Vec::new();
    if app.composer.text.is_empty() {
        let placeholder = if running {
            "Agent is working — Esc to cancel"
        } else if view.is_none() {
            "Create or open a session"
        } else {
            "Type a message…"
        };
        content.push(Line::styled(placeholder, Style::new().fg(theme.muted)));
    } else {
        for line in wrap_plain(
            &app.composer.text,
            inner.width as usize,
            Style::new().fg(theme.text),
        ) {
            content.push(line);
        }
    }
    while content.len() < inner.height as usize {
        content.push(Line::default());
    }
    frame.render_widget(Paragraph::new(content), inner);

    // Hardware cursor at the composer's caret. The column uses display
    // cells (CJK = 2), never `String::len` (spec 8.4).
    if !running && view.is_some() {
        let prefix = app
            .composer
            .text
            .get(..app.composer.cursor)
            .unwrap_or(&app.composer.text);
        let col = (column_width(prefix) as u16).min(inner.width.saturating_sub(1));
        frame.set_cursor_position((inner.x + col, inner.y));
    }
}
