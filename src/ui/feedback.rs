//! Small shared primitives for the dock's one-line status and notices.
//! Terminal font size is external; these rows share ordinary-weight text.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::theme::Theme;
use crate::ui::rail;

pub(crate) fn neutral_style(theme: &Theme) -> Style {
    Style::new().fg(theme.muted)
}

/// Preserve the optional activity/severity prefix and safely clip the body to
/// one row. Whole graphemes share the same clipping rule as other Rail text.
pub(crate) fn render_row(
    frame: &mut Frame<'_>,
    area: Rect,
    prefix: Option<Span<'_>>,
    text: &str,
    style: Style,
) {
    if area.is_empty() {
        return;
    }
    let mut remaining = area.width as usize;
    let mut spans = Vec::new();
    if let Some(prefix) = prefix {
        let content = rail::clip_cells(&prefix.content, remaining);
        remaining = remaining.saturating_sub(crate::markdown::column_width(&content));
        spans.push(Span::styled(content, prefix.style));
    }
    let safe = crate::safe_text::safe_display(text);
    spans.push(Span::styled(rail::clip_cells(&safe, remaining), style));
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
}
