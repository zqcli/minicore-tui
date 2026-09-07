//! Reasoning rendering (development spec 15.4, 30): transparent background,
//! purple Rail, native Markdown rows, and a single hidden label when the user
//! disables reasoning. Auto-folding is applied by the assistant section
//! builder so live/history use the same raw-line rule.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::markdown::MarkdownRenderer;
use crate::theme::Theme;

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
    reasoning_lines_with_fold(theme, text, width, visible, already_hidden_run, None)
}

pub fn reasoning_lines_with_fold(
    theme: &Theme,
    text: &str,
    width: usize,
    visible: bool,
    already_hidden_run: bool,
    expanded: Option<bool>,
) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let rows = if visible {
        let raw_lines = text.trim().split('\n').count();
        let folded = expanded.map_or(raw_lines > 3, |expanded| !expanded);
        if folded && raw_lines > 3 {
            collapsed_lines(theme, text, width, raw_lines - 3)
        } else {
            visible_lines(theme, text, width)
        }
    } else if already_hidden_run {
        Vec::new()
    } else {
        hidden_line(theme, width)
    };
    if rows.is_empty() {
        return Vec::new();
    }
    // A thinking run is a vertically padded section like assistant text: one
    // transparent blank above and below, shared with neighbors by the
    // transcript boundary logic (0.2.2 user-card/thinking spacing).
    let mut padded = Vec::with_capacity(rows.len() + 2);
    padded.push(Line::default());
    padded.extend(rows);
    padded.push(Line::default());
    padded
}

fn collapsed_lines(theme: &Theme, text: &str, width: usize, hidden: usize) -> Vec<Line<'static>> {
    let preview = visible_lines(theme, text, width);
    let mut rows = preview.into_iter().take(3).collect::<Vec<_>>();
    let hint = Line::from(vec![
        ratatui::text::Span::styled(
            format!("... ({hidden} earlier lines, "),
            Style::new().fg(theme.muted),
        ),
        ratatui::text::Span::styled("ctrl+o", Style::new().fg(theme.dim)),
        ratatui::text::Span::styled(" to expand)", Style::new().fg(theme.muted)),
    ]);
    rows.push(crate::ui::rail::surface_row(
        width,
        crate::ui::rail::thinking_colors(theme),
        crate::ui::rail::RAIL_WIDTH,
        hint,
    ));
    rows
}

/// A visible reasoning run: gray, italic, Markdown-rendered, and padded.
pub fn visible_lines(theme: &Theme, text: &str, width: usize) -> Vec<Line<'static>> {
    markdown_section(theme, text, width)
}

/// A single hidden-run label.
pub fn thinking_line(theme: &Theme) -> Vec<Line<'static>> {
    let style = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
    vec![Line::from(vec![
        ratatui::text::Span::styled(
            crate::ui::rail::RAIL_GLYPH,
            Style::new().fg(theme.rail_thinking),
        ),
        ratatui::text::Span::styled(" Thinking...", style),
    ])]
}

fn hidden_line(theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let line = Line::styled(
        " Thinking...",
        Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
    );
    vec![crate::ui::rail::surface_row(
        width,
        crate::ui::rail::thinking_colors(theme),
        crate::ui::rail::RAIL_WIDTH,
        line,
    )]
}

/// Live reasoning uses the same vertically padded gray italic section as a
/// durable reasoning run. Each frame parses only this request's accumulated
/// reasoning, so incomplete Markdown remains renderable without touching the
/// prepared conversation snapshot.
pub fn live_lines(theme: &Theme, text: &str, width: usize, visible: bool) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    if !visible {
        return hidden_line(theme, width);
    }
    markdown_section(theme, text, width)
}

fn markdown_section(theme: &Theme, text: &str, width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    // Thinking preserves raw single newlines as visual row breaks; all other
    // markdown fidelity (bold/code/lists/CJK/links/blank paragraphs) is
    // unchanged (0.2.2 reasoning line contract).
    let mut lines = MarkdownRenderer::preserving_breaks(theme).render(
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
    lines
        .into_iter()
        .map(|line| {
            crate::ui::rail::surface_row(
                width,
                crate::ui::rail::thinking_colors(theme),
                crate::ui::rail::RAIL_WIDTH,
                line,
            )
        })
        .collect()
}
