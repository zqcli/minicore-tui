//! The user message card (development spec 15.2): `user_message_bg`,
//! horizontal padding 1, one blank line above and below, no "You" label.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::{MarkdownRenderer, line_width};
use crate::state::transcript::UserBlock;
use crate::theme::Theme;

pub fn lines(theme: &Theme, block: &UserBlock, width: usize) -> Vec<Line<'static>> {
    if block.text.trim().is_empty() {
        return Vec::new();
    }
    let mut out = vec![Line::default()];
    let base = Style::new().fg(theme.text).bg(theme.user_message_bg);
    let inner = width.saturating_sub(2).max(1);
    let renderer = MarkdownRenderer::new(theme);
    for line in renderer.render(&block.text, inner, base) {
        let mut spans = vec![Span::styled(" ", base)];
        let used = 1 + line_width(&line);
        spans.extend(line.spans);
        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), base));
        }
        out.push(Line::from(spans));
    }
    out.push(Line::default());
    out
}
