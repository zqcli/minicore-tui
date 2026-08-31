//! The Logs panel (development spec 24.1, 33.1): the agent's captured
//! stderr ring, newest entries first. No raw RPC frames are ever shown.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::theme::Theme;
use crate::ui::layout;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    frame.render_widget(
        Block::bordered().border_style(Style::new().fg(theme.border_accent)),
        area,
    );
    let inner = area.inner(Margin::new(1, 1));
    let width = inner.width as usize;

    let mut lines = vec![
        Line::from(Span::styled(
            "Agent logs",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Captured stderr (newest first)",
            Style::new().fg(theme.dim),
        )),
        Line::default(),
    ];
    // Newest log lines are at the back; render them newest-first by
    // iterating in reverse (still bounded by the 200-line ring).
    for line in app.agent_logs.iter().rev() {
        lines.push(Line::from(Span::styled(
            layout::truncate(line, width),
            Style::new().fg(theme.muted),
        )));
    }
    if app.agent_logs.is_empty() {
        lines.push(Line::from(Span::styled(
            "No agent output captured yet.",
            Style::new().fg(theme.dim),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Esc closes this panel",
        Style::new().fg(theme.dim),
    )));

    // panel_scroll counts from the top of `lines`; flipping for newest-first
    // is unnecessary since the list is short and the offset just slices.
    let height = inner.height as usize;
    let scroll = app.panel_scroll.min(lines.len().saturating_sub(height));
    let window: Vec<Line<'static>> = lines.iter().skip(scroll).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(window), inner);
}
