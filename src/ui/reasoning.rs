//! Reasoning rendering (development spec 15.4, 30): transparent background,
//! purple Rail, native Markdown rows, and a single hidden label when the user
//! disables reasoning. Auto-folding is applied by the assistant section
//! builder so live/history use the same raw-line rule.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::markdown::{CopyCells, MarkdownRenderer, RenderedMarkdown};
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
    reasoning_with_metadata(theme, text, width, visible, already_hidden_run, expanded).lines
}

/// Preserve link/copy geometry through the same fold and rail layout as the rows.
pub fn reasoning_with_metadata(
    theme: &Theme,
    text: &str,
    width: usize,
    visible: bool,
    already_hidden_run: bool,
    expanded: Option<bool>,
) -> RenderedMarkdown {
    if text.is_empty() || (!visible && already_hidden_run) {
        return RenderedMarkdown::default();
    }
    let mut rendered = if visible {
        markdown_with_metadata(theme, text, width)
    } else {
        let lines = hidden_line(theme, width);
        let len = lines.len();
        RenderedMarkdown {
            lines,
            link_cells: vec![Vec::new(); len],
            hard_breaks: vec![false; len],
            copy_cells: vec![Some(CopyCells::decoration()); len],
        }
    };
    let raw_lines = text.trim().split('\n').count();
    let folded = expanded.map_or(raw_lines > 3, |expanded| !expanded);
    if visible && folded && raw_lines > 3 {
        rendered.lines.truncate(3);
        rendered.link_cells.truncate(3);
        rendered.hard_breaks.truncate(3);
        rendered.copy_cells.truncate(3);
        let hint = Line::from(vec![
            ratatui::text::Span::styled(
                format!("... ({} earlier lines, ", raw_lines - 3),
                Style::new().fg(theme.muted),
            ),
            ratatui::text::Span::styled("ctrl+o", Style::new().fg(theme.dim)),
            ratatui::text::Span::styled(" to expand)", Style::new().fg(theme.muted)),
        ]);
        rendered.lines.push(crate::ui::rail::surface_row(
            width,
            crate::ui::rail::thinking_colors(theme),
            crate::ui::rail::RAIL_WIDTH,
            hint,
        ));
        rendered.link_cells.push(Vec::new());
        rendered.hard_breaks.push(false);
        rendered.copy_cells.push(Some(CopyCells::decoration()));
    }
    if !rendered.lines.is_empty() {
        rendered.lines.insert(0, Line::default());
        rendered.lines.push(Line::default());
        rendered.link_cells.insert(0, Vec::new());
        rendered.link_cells.push(Vec::new());
        rendered.hard_breaks.insert(0, false);
        rendered.hard_breaks.push(false);
        rendered.copy_cells.insert(0, Some(CopyCells::decoration()));
        rendered.copy_cells.push(Some(CopyCells::decoration()));
    }
    rendered
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
    markdown_with_metadata(theme, text, width).lines
}

fn markdown_with_metadata(theme: &Theme, text: &str, width: usize) -> RenderedMarkdown {
    if text.is_empty() {
        return RenderedMarkdown::default();
    }
    // Thinking preserves raw single newlines as visual row breaks; all other
    // markdown fidelity (bold/code/lists/CJK/links/blank paragraphs) is
    // unchanged (0.2.2 reasoning line contract).
    let mut rendered = MarkdownRenderer::preserving_breaks(theme).render_with_metadata(
        text,
        width.saturating_sub(1).max(1),
        Style::new().add_modifier(Modifier::ITALIC),
    );
    // `Style::patch` lets the base foreground override a Markdown span's
    // explicit color. Fill only uncolored spans here so code, list, heading,
    // and fenced-code colors from MarkdownRenderer remain visible.
    for line in &mut rendered.lines {
        for span in &mut line.spans {
            if span.style.fg.is_none() {
                span.style = span.style.fg(theme.muted);
            }
        }
    }
    rendered.lines = rendered
        .lines
        .into_iter()
        .map(|line| {
            crate::ui::rail::surface_row(
                width,
                crate::ui::rail::thinking_colors(theme),
                crate::ui::rail::RAIL_WIDTH,
                line,
            )
        })
        .collect();
    let inset = crate::ui::rail::RAIL_WIDTH;
    for row in &mut rendered.link_cells {
        for range in row {
            *range = range.start + inset..range.end + inset;
        }
    }
    for copy in rendered.copy_cells.iter_mut().flatten() {
        copy.columns = copy.columns.start + inset..copy.columns.end + inset;
    }
    rendered
}
