//! The new-session form (development spec 25): a bordered panel below the
//! transcript with the workspace/profile/model/reasoning/title fields and
//! the Create action; profile/model/reasoning open their selector on Enter.
//! Read-only renderer — `DockFieldStep`, `NewSessionSetField`, and
//! `ConfirmDock` drive it through `App::update`.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::state::selection::{NewSessionField, NewSessionState, reasoning_label};
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::selector::highlight;

const LABEL_WIDTH: usize = 11;

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, draft: &NewSessionState) {
    frame.render_widget(
        Block::bordered().border_style(Style::new().fg(theme.border_accent)),
        area,
    );
    let inner = area.inner(Margin::new(1, 1));
    let width = inner.width as usize;
    let tall = inner.height >= 8;

    let mut lines: Vec<Line<'static>> = Vec::new();
    if tall {
        lines.push(Line::from(Span::styled(
            "New session",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(field_line(theme, draft, NewSessionField::Workspace, width));
    lines.push(field_line(theme, draft, NewSessionField::Profile, width));
    lines.push(field_line(theme, draft, NewSessionField::Model, width));
    lines.push(field_line(theme, draft, NewSessionField::Reasoning, width));
    lines.push(field_line(theme, draft, NewSessionField::Title, width));
    lines.push(field_line(theme, draft, NewSessionField::Create, width));
    if tall {
        let status = if draft.submitting {
            Line::from(Span::styled(
                "Creating session…",
                Style::new().fg(theme.dim),
            ))
        } else if let Some(error) = &draft.error {
            Line::from(Span::styled(
                format!("⚠ {error}"),
                Style::new().fg(theme.error),
            ))
        } else {
            Line::from(Span::styled(
                "Tab moves · Enter confirms · Esc closes",
                Style::new().fg(theme.dim),
            ))
        };
        lines.push(status);
    }
    while lines.len() < inner.height as usize {
        lines.push(Line::default());
    }
    frame.render_widget(Paragraph::new(lines), inner);

    // A block cursor on the editable workspace/title field so IME and
    // editing land visibly (read-only; the buffer lives in the draft).
    if matches!(
        draft.field,
        NewSessionField::Workspace | NewSessionField::Title
    ) {
        let row_offset = if tall { 1 } else { 0 };
        let row = inner.y + row_offset;
        let value = match draft.field {
            NewSessionField::Workspace => &draft.workspace,
            _ => &draft.title,
        };
        let col = crate::markdown::column_width(&value[..char_to_byte(value, draft.field_cursor)]);
        let x = inner.x + LABEL_WIDTH as u16 + col as u16;
        if x < inner.x + inner.width && row < inner.y + inner.height {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, row)) {
                cell.set_fg(theme.page_bg);
                cell.set_bg(theme.text);
            }
        }
    }
}

fn char_to_byte(text: &str, cursor: usize) -> usize {
    text.chars().take(cursor).map(char::len_utf8).sum::<usize>()
}

/// One form row: a dim label, the field value (or a dim placeholder), a
/// `→` on profile/model/reasoning (they open a selector on Enter), and the
/// highlighted background for the active field under the cursor.
fn field_line(
    theme: &Theme,
    draft: &NewSessionState,
    field: NewSessionField,
    width: usize,
) -> Line<'static> {
    let selected = draft.field == field;
    let label = match field {
        NewSessionField::Workspace => "workspace",
        NewSessionField::Profile => "profile",
        NewSessionField::Model => "model",
        NewSessionField::Reasoning => "reasoning",
        NewSessionField::Title => "title",
        NewSessionField::Create => "action",
    };
    let mut spans = vec![Span::styled(
        format!("{label:<LABEL_WIDTH$}"),
        Style::new().fg(theme.dim),
    )];
    let value_cap = width.saturating_sub(LABEL_WIDTH + 3);
    match field {
        NewSessionField::Create => {
            spans.push(Span::styled(
                "Create session",
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ));
        }
        NewSessionField::Reasoning => {
            spans.push(Span::styled(
                reasoning_label(draft.reasoning),
                Style::new().fg(theme.reasoning_color(draft.reasoning)),
            ));
        }
        NewSessionField::Workspace => {
            push_value(theme, &mut spans, &draft.workspace, "(empty)", value_cap);
        }
        NewSessionField::Title => {
            push_value(theme, &mut spans, &draft.title, "(optional)", value_cap);
        }
        NewSessionField::Profile => {
            push_value(
                theme,
                &mut spans,
                &draft.profile,
                "(agent default)",
                value_cap,
            );
        }
        NewSessionField::Model => {
            push_value(
                theme,
                &mut spans,
                &draft.model,
                "(agent default)",
                value_cap,
            );
        }
    }
    if matches!(
        field,
        NewSessionField::Profile | NewSessionField::Model | NewSessionField::Reasoning
    ) {
        spans.push(Span::styled("  →", Style::new().fg(theme.accent)));
    }
    highlight(Line::from(spans), selected, theme, width)
}

fn push_value(
    theme: &Theme,
    spans: &mut Vec<Span<'static>>,
    value: &str,
    placeholder: &str,
    cap: usize,
) {
    if value.is_empty() {
        spans.push(Span::styled(
            placeholder.to_owned(),
            Style::new().fg(theme.muted),
        ));
    } else {
        let value = layout::truncate(value, cap);
        spans.push(Span::styled(value, Style::new().fg(theme.text)));
    }
}
