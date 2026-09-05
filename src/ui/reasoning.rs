//! Reasoning rendering (development spec 15.4, 30): gray italic, padding 1,
//! natural block order. Hidden runs collapse to a single "Thinking..." per
//! continuous run; live reasoning uses the same bounded Markdown renderer as
//! durable reasoning.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::markdown::MarkdownRenderer;
use crate::theme::Theme;
use crate::ui::layout;

/// One part of a durable assistant. `already_hidden_run` is true when the
/// previous part was also hidden reasoning, so a run shows a single
/// "Thinking..." (spec 30.2).
pub fn reasoning_lines(
    theme: &Theme,
    text: &str,
    width: usize,
    visible: bool,
    already_hidden_run: bool,
) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    if visible {
        visible_lines(theme, text, width)
    } else if already_hidden_run {
        Vec::new()
    } else {
        thinking_line(theme)
    }
}

/// A visible reasoning run: gray, italic, Markdown-rendered, and padded.
pub fn visible_lines(theme: &Theme, text: &str, width: usize) -> Vec<Line<'static>> {
    markdown_section(theme, text, width)
}

/// A single hidden-run label.
pub fn thinking_line(theme: &Theme) -> Vec<Line<'static>> {
    let style = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
    layout::vertical_section(vec![Line::styled(" Thinking...", style)])
}

/// Live reasoning uses the same vertically padded gray italic section as a
/// durable reasoning run. Each frame parses only this request's accumulated
/// reasoning, so incomplete Markdown remains renderable without touching the
/// durable transcript cache.
pub fn live_lines(theme: &Theme, text: &str, width: usize, visible: bool) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    if !visible {
        return thinking_line(theme);
    }
    markdown_section(theme, text, width)
}

fn markdown_section(theme: &Theme, text: &str, width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = MarkdownRenderer::new(theme).render(
        text,
        width.saturating_sub(1).max(1),
        Style::new().add_modifier(Modifier::ITALIC),
    );
    // `Style::patch` lets the base foreground override a Markdown span's
    // explicit color. Fill only uncolored spans here so code, list, heading,
    // and fenced-code colors from MarkdownRenderer remain visible.
    for line in &mut lines {
        for span in &mut line.spans {
            if span.style.fg.is_none() {
                span.style = span.style.fg(theme.muted);
            }
        }
    }
    let lines = lines
        .into_iter()
        .map(|line| layout::left_pad(line, 1))
        .collect();
    layout::vertical_section(lines)
}
