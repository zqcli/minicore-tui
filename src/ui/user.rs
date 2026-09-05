//! The user message card: prompt cards and compact steering cards (spec r2).

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::{MarkdownRenderer, line_width};
use crate::protocol::UserMessageKindWire;
use crate::state::transcript::UserBlock;
use crate::theme::Theme;

pub fn lines(theme: &Theme, block: &UserBlock, width: usize) -> Vec<Line<'static>> {
    if block.text.trim().is_empty() {
        return Vec::new();
    }

    if block.kind == UserMessageKindWire::Steering {
        return steering_lines(theme, &block.text, width);
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

pub fn steering_lines(theme: &Theme, text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::default()];
    let bg = theme.card_bg;
    let label_style = Style::new().fg(theme.accent).bg(bg);
    let text_style = Style::new().fg(theme.text).bg(bg);
    let prefix = " ↪ Steering: ";

    let available = width.saturating_sub(prefix.len() + 1).max(1);
    let lines = crate::markdown::wrap_plain(text, available, text_style);

    for (i, line) in lines.into_iter().enumerate() {
        let mut spans = Vec::new();
        if i == 0 {
            spans.push(Span::styled(prefix, label_style));
        } else {
            spans.push(Span::styled(" ".repeat(prefix.len()), label_style));
        }
        let used = prefix.len() + line_width(&line);
        spans.extend(line.spans);
        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), text_style));
        }
        out.push(Line::from(spans));
    }
    out.push(Line::default());
    out
}
