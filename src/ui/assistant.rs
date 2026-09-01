//! The assistant message (development spec 15.3): no background, horizontal
//! padding 1, one blank line above and below, full markdown for durable text.
//! Tool calls render as separate tool cards, never inside the markdown.

use ratatui::style::Style;
use ratatui::text::Line;

use crate::markdown::MarkdownRenderer;
use crate::state::transcript::{AssistantBlock, AssistantPart};
use crate::theme::Theme;
use crate::ui::{layout, reasoning};

pub fn lines(
    theme: &Theme,
    block: &AssistantBlock,
    width: usize,
    reasoning_visible: bool,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let renderer = MarkdownRenderer::new(theme);
    let base = Style::new().fg(theme.text);
    let mut in_hidden_run = false;
    for part in &block.parts {
        match part {
            AssistantPart::Text(text) => {
                let lines = renderer
                    .render(text, width.saturating_sub(1).max(1), base)
                    .into_iter()
                    .map(|line| layout::left_pad(line, 1))
                    .collect();
                layout::append_section(&mut out, layout::vertical_section(lines));
                in_hidden_run = false;
            }
            AssistantPart::Reasoning(reasoning) => {
                let section = reasoning::reasoning_lines(
                    theme,
                    reasoning,
                    width,
                    reasoning_visible,
                    in_hidden_run,
                );
                let has_section = !section.is_empty();
                layout::append_section(&mut out, section);
                if has_section {
                    in_hidden_run = !reasoning_visible;
                }
            }
        }
    }
    out
}
