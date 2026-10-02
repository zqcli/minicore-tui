//! Reasoning rendering (development spec 15.4, 30): transparent background,
//! purple Rail, native Markdown rows, and a single hidden label when the user
//! disables reasoning. Auto-folding is applied by the assistant section
//! builder so live/history use the same rendered-height rule.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

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
    render_with_state(
        theme,
        text,
        width,
        visible,
        already_hidden_run,
        expanded,
        false,
    )
    .0
}

/// Returns rendered rows and whether their full content exceeds the preview.
#[allow(clippy::too_many_arguments)]
pub fn render_with_state(
    theme: &Theme,
    text: &str,
    width: usize,
    visible: bool,
    already_hidden_run: bool,
    expanded: Option<bool>,
    generating: bool,
) -> (RenderedMarkdown, bool) {
    if text.is_empty() || (!visible && already_hidden_run) {
        return (RenderedMarkdown::default(), false);
    }
    let mut rendered = if visible {
        markdown_with_metadata(theme, text, width)
    } else {
        let lines = hidden_line(theme, width, generating);
        let len = lines.len();
        RenderedMarkdown {
            lines,
            link_cells: vec![Vec::new(); len],
            hard_breaks: vec![false; len],
            copy_cells: vec![Some(CopyCells::decoration()); len],
        }
    };
    let content_rows = rendered.lines.len();
    let collapsible = visible && content_rows > 3;
    let folded = !expanded.unwrap_or(false);
    if collapsible {
        if folded {
            rendered.lines.truncate(3);
            rendered.link_cells.truncate(3);
            rendered.hard_breaks.truncate(3);
            rendered.copy_cells.truncate(3);
        }
        let hint = Line::from(Span::styled(
            if folded {
                format!(
                    "... ({} more rows; click or ctrl+o to expand)",
                    content_rows - 3
                )
            } else {
                "click to collapse reasoning (ctrl+o: all details)".to_owned()
            },
            Style::new().fg(theme.muted),
        ));
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
    (rendered, collapsible)
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

fn hidden_line(theme: &Theme, width: usize, generating: bool) -> Vec<Line<'static>> {
    let line = Line::from(Span::styled(
        if generating {
            " Thinking... (ctrl+t to show)"
        } else {
            " Reasoning hidden (ctrl+t to show)"
        },
        Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
    ));
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
        return hidden_line(theme, width, true);
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

#[cfg(test)]
mod fold_tests {
    use super::*;

    #[test]
    fn wrapped_single_line_folds_by_rendered_height_and_preserves_metadata() {
        let source = "思考内容".repeat(80);
        for width in [20, 59, 79] {
            let (folded, collapsible) =
                render_with_state(&Theme::dark(), &source, width, true, false, None, false);
            assert!(collapsible);
            assert_eq!(folded.lines.len(), 6);
            assert_eq!(folded.lines.len(), folded.copy_cells.len());
            assert_eq!(folded.lines.len(), folded.link_cells.len());
            assert_eq!(folded.lines.len(), folded.hard_breaks.len());
            let (expanded, _) = render_with_state(
                &Theme::dark(),
                &source,
                width,
                true,
                false,
                Some(true),
                false,
            );
            assert!(expanded.lines.len() > folded.lines.len());
            assert!(
                expanded
                    .lines
                    .iter()
                    .any(|line| line.to_string().contains("collapse"))
            );
            assert!(
                folded.copy_cells[4]
                    .as_ref()
                    .is_some_and(|copy| copy.columns.is_empty())
            );
        }
    }

    #[test]
    fn hidden_labels_and_fold_hints_retain_theme_foreground_after_rail_layout() {
        for theme in [Theme::dark(), Theme::light()] {
            for generating in [false, true] {
                let (rendered, _) =
                    render_with_state(&theme, "thought", 79, false, false, None, generating);
                let label = rendered
                    .lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .find(|span| span.content.contains("ctrl+t"))
                    .expect("hidden reasoning label");
                assert_eq!(label.style.fg, Some(theme.muted));
                assert!(label.style.add_modifier.contains(Modifier::ITALIC));
            }
            for expanded in [false, true] {
                let (rendered, _) = render_with_state(
                    &theme,
                    "one\ntwo\nthree\nfour",
                    79,
                    true,
                    false,
                    Some(expanded),
                    false,
                );
                let hint = rendered
                    .lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .find(|span| span.content.contains("ctrl+o"))
                    .expect("fold action hint");
                assert_eq!(hint.style.fg, Some(theme.muted));
            }
        }
    }

    #[test]
    fn hidden_reasoning_label_distinguishes_completed_and_generating() {
        let (completed, _) =
            render_with_state(&Theme::dark(), "thought", 79, false, false, None, false);
        let (generating, _) =
            render_with_state(&Theme::dark(), "thought", 79, false, false, None, true);
        assert!(
            completed
                .lines
                .iter()
                .any(|line| line.to_string().contains("Reasoning hidden"))
        );
        assert!(
            completed
                .lines
                .iter()
                .all(|line| !line.to_string().contains("Thinking..."))
        );
        assert!(
            generating
                .lines
                .iter()
                .any(|line| line.to_string().contains("Thinking..."))
        );
    }
}
