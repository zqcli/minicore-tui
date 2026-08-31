//! Reasoning rendering (development spec 15.4, 30): gray italic, padding 1,
//! natural block order. Hidden runs collapse to a single "Thinking..." per
//! continuous run; live reasoning stays plain wrapped text.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::markdown::wrap_plain;
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
    if visible {
        visible_lines(theme, text, width)
    } else if already_hidden_run {
        Vec::new()
    } else {
        thinking_line(theme)
    }
}

/// A visible reasoning run: gray, italic, plain-wrapped (streaming-safe).
pub fn visible_lines(theme: &Theme, text: &str, width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = vec![Line::default()];
    let style = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
    for line in wrap_plain(text, width.saturating_sub(1).max(1), style) {
        out.push(layout::left_pad(line, 1));
    }
    out
}

/// A single hidden-run label.
pub fn thinking_line(theme: &Theme) -> Vec<Line<'static>> {
    let style = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
    vec![Line::styled(" Thinking...", style)]
}

/// Live reasoning: same gray italic plain text (no leading blank; it sits
/// after the live pending user card).
pub fn live_lines(theme: &Theme, text: &str, width: usize, visible: bool) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    if !visible {
        return thinking_line(theme);
    }
    let style = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
    wrap_plain(text, width.saturating_sub(1).max(1), style)
        .into_iter()
        .map(|line| layout::left_pad(line, 1))
        .collect()
}
