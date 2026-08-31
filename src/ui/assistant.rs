//! The assistant message (development spec 15.3): no background, horizontal
//! padding 1, one blank line above, full markdown for durable text. Tool
//! calls render as separate tool cards, never inside the markdown.

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
    let mut out = vec![Line::default()];
    let renderer = MarkdownRenderer::new(theme);
    let base = Style::new().fg(theme.text);
    let mut in_hidden_run = false;
    for part in &block.parts {
        match part {
            AssistantPart::Text(text) => {
                for line in renderer.render(text, width.saturating_sub(1).max(1), base) {
                    out.push(layout::left_pad(line, 1));
                }
                in_hidden_run = false;
            }
            AssistantPart::Reasoning(reasoning) => {
                out.extend(reasoning::reasoning_lines(
                    theme,
                    reasoning,
                    width,
                    reasoning_visible,
                    in_hidden_run,
                ));
                in_hidden_run = !reasoning_visible;
            }
        }
    }
    out
}
