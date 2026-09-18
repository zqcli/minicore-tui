//! The Logs panel (development spec 24.1, 33.1): the agent's captured
//! stderr ring, newest entries first. No raw RPC frames are ever shown.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::panel::{self, PanelSpec};

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let panel = panel::layout(area, PanelSpec::new(1, false, 1));
    panel::render_frame(frame, panel, theme);
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "Agent logs",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))]),
        panel.title,
    );
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "Recent stderr activity (newest first)",
            Style::new().fg(theme.dim),
        ))]),
        panel.header,
    );
    let width = panel.content.width as usize;

    let mut lines = vec![Line::default()];
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
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "Esc closes this panel",
            Style::new().fg(theme.dim),
        ))]),
        panel.footer,
    );

    // panel_scroll counts from the top of `lines`; flipping for newest-first
    // is unnecessary since the list is short and the offset just slices.
    let scroll = app
        .panel_scroll
        .min(lines.len().saturating_sub(panel.content.height as usize));
    panel::render_window(frame, panel.content, &lines, scroll);
}
