//! Notices (a single row above the composer) and the fatal overlay
//! (development spec 33): an error summary, the reason, and the last few
//! agent stderr lines. Content is agent/DTO text only, never raw frames.

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::app::{Notice, NoticeLevel};
use crate::theme::Theme;
use crate::ui::layout;

pub fn render_notice(frame: &mut Frame, area: Rect, theme: &Theme, notice: &Notice) {
    let fg = match notice.level {
        NoticeLevel::Error => theme.error,
        NoticeLevel::Warning => theme.warning,
        NoticeLevel::Info => theme.dim,
    };
    let prefix = if notice.sticky { "⚠ " } else { "" };
    let text = format!("{prefix}{}", notice.text);
    let line = Line::styled(
        layout::truncate(&text, area.width as usize),
        Style::new().fg(fg),
    );
    frame.render_widget(Paragraph::new(line), area);
}

pub fn render_fatal(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    reason: &str,
    exit_status: Option<&str>,
    logs: &VecDeque<String>,
) {
    let text_style = Style::new().fg(theme.error);
    let mut lines = vec![
        Line::styled(
            "Fatal error",
            Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
        ),
        Line::styled("First failure:", Style::new().fg(theme.muted)),
        Line::styled(reason.to_owned(), text_style),
        Line::styled(
            format!("Exit status: {}", exit_status.unwrap_or("unavailable")),
            text_style,
        ),
        Line::default(),
        Line::styled("Recent agent output:", Style::new().fg(theme.muted)),
    ];
    let width = area.width as usize;
    let start = logs.len().saturating_sub(20);
    for line in logs.iter().skip(start) {
        lines.push(Line::styled(
            layout::truncate(line, width.saturating_sub(2)),
            Style::new().fg(theme.muted),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::styled("Press q to quit", Style::new().fg(theme.dim)));

    let height = lines.len() as u16;
    let top = area.y + area.height.saturating_sub(height) / 2;
    let para_area = Rect::new(area.x, top, area.width, height.min(area.height));
    frame.render_widget(Paragraph::new(lines), para_area);
}
