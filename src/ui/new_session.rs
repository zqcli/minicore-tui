//! The new-session form (development spec 25): a bordered panel below the
//! transcript with the workspace/profile/model/reasoning/title fields and
//! the Create action; profile/model/reasoning open their selector on Enter.
//! Read-only renderer — `DockFieldStep`, `NewSessionSetField`, and
//! `ConfirmDock` drive it through `App::update`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::selection::{NewSessionField, NewSessionState, reasoning_label};
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::panel::{self, PanelSpec};
use crate::ui::selector::highlight;

const LABEL_WIDTH: usize = 11;

const FIELDS: [NewSessionField; 6] = [
    NewSessionField::Workspace,
    NewSessionField::Profile,
    NewSessionField::Model,
    NewSessionField::Reasoning,
    NewSessionField::Title,
    NewSessionField::Create,
];

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, draft: &NewSessionState) {
    let panel = panel::layout(area, PanelSpec::new(0, false, 1));
    panel::render_frame(frame, panel, theme);
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "New session",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))]),
        panel.title,
    );
    let width = panel.content.width as usize;
    let selected = FIELDS
        .iter()
        .position(|field| *field == draft.field)
        .unwrap_or(0);
    let visible = panel::visible_window(
        &vec![1; FIELDS.len()],
        selected,
        panel.content.height as usize,
    );
    let mut lines = FIELDS
        .iter()
        .enumerate()
        .take(visible.end)
        .skip(visible.start)
        .map(|(_, field)| field_line(theme, draft, *field, width))
        .collect::<Vec<_>>();
    while lines.len() < panel.content.height as usize {
        lines.push(Line::default());
    }
    frame.render_widget(Paragraph::new(lines), panel.content);
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
    frame.render_widget(Paragraph::new(vec![status]), panel.footer);

    // A block cursor on the editable workspace/title field so IME and
    // editing land visibly (read-only; the buffer lives in the draft).
    if matches!(
        draft.field,
        NewSessionField::Workspace | NewSessionField::Title
    ) {
        let Some(row_in_window) = selected.checked_sub(visible.start) else {
            return;
        };
        let row = panel.content.y + row_in_window as u16;
        let value = match draft.field {
            NewSessionField::Workspace => &draft.workspace,
            _ => &draft.title,
        };
        let col = crate::markdown::column_width(&value[..char_to_byte(value, draft.field_cursor)]);
        let x = panel.content.x + LABEL_WIDTH as u16 + col as u16;
        if x < panel.content.x + panel.content.width && row < panel.content.bottom() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, row)) {
                cell.set_fg(theme.page_bg);
                cell.set_bg(theme.text);
            }
        }
    }
}

/// Maps a terminal cell in the shared form geometry to a field and, for an
/// editable field, a Unicode character cursor position.
pub(crate) fn field_at(
    area: Rect,
    draft: &NewSessionState,
    column: u16,
    row: u16,
) -> Option<(NewSessionField, Option<usize>)> {
    let panel = panel::layout(area, PanelSpec::new(0, false, 1));
    if column < panel.content.x || column >= panel.content.right() {
        return None;
    }
    let row = panel.content_row(row)?;
    let selected = FIELDS
        .iter()
        .position(|field| *field == draft.field)
        .unwrap_or(0);
    let visible = panel::visible_window(
        &vec![1; FIELDS.len()],
        selected,
        panel.content.height as usize,
    );
    let index = visible.start + row;
    let field = *FIELDS.get(index)?;
    let cursor = match field {
        NewSessionField::Workspace => Some(cursor_at_column(
            &draft.workspace,
            column.saturating_sub(panel.content.x) as usize,
        )),
        NewSessionField::Title => Some(cursor_at_column(
            &draft.title,
            column.saturating_sub(panel.content.x) as usize,
        )),
        _ => None,
    };
    Some((field, cursor))
}

fn cursor_at_column(value: &str, column: usize) -> usize {
    let mut used = LABEL_WIDTH;
    let mut cursor = 0;
    for character in value.chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if column < used + width.max(1) / 2 {
            break;
        }
        used += width;
        cursor += 1;
    }
    cursor.min(value.chars().count())
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
