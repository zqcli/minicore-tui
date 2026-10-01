//! Renderer for the local `/settings` form.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

#[cfg(test)]
use crate::markdown::column_width;
use crate::state::settings::{SettingsField, SettingsState};
use crate::theme::Theme;
use crate::ui::layout::{self, text_window};
use crate::ui::panel::{self, PanelSpec};

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, state: &SettingsState) {
    let panel = panel::layout(area, PanelSpec::new(0, false, 1));
    panel::render_frame(frame, panel, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if state.submitting {
                "Settings · saving…"
            } else if state.is_dirty() {
                "Settings · unsaved changes"
            } else {
                "Settings"
            },
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))),
        panel.title,
    );

    let rows = SettingsField::ALL
        .iter()
        .map(|field| {
            let selected = *field == state.field;
            let value = value_for(*field, state);
            let style = if selected {
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme.text)
            };
            let label_width = 26;
            let value_width = (panel.content.width as usize).saturating_sub(3 + label_width);
            let mut spans = vec![
                Span::styled(if selected { "> " } else { "  " }, style),
                Span::styled(format!("{:<label_width$} ", field.label()), style),
            ];
            if selected && state.active_text().is_some() {
                spans.extend(text_window(
                    state.active_text().unwrap_or_default(),
                    state.cursor,
                    value_width,
                    style,
                ));
            } else {
                spans.push(Span::styled(layout::truncate(&value, value_width), style));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    let selected = SettingsField::ALL
        .iter()
        .position(|field| *field == state.field)
        .unwrap_or(0);
    let scroll = selected
        .saturating_add(1)
        .saturating_sub(panel.content.height as usize);
    panel::render_window(frame, panel.content, &rows, scroll);
    let footer = if let Some(error) = state.error.as_deref() {
        format!("{error} · Ctrl+S retry · Esc cancel")
    } else {
        let detail = match state.field {
            SettingsField::EditorArgs => "Enter/Ctrl+J new arg · ←→ Home/End edit · Ctrl+U clear",
            SettingsField::EditorExecutable
            | SettingsField::AgentExecutable
            | SettingsField::AgentConfig => "←→ Home/End edit · Ctrl+U clear",
            _ => "Enter toggle/apply",
        };
        format!("Ctrl+S save · Esc cancel · Tab/↑↓ field · {detail}")
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            layout::truncate(&footer, panel.footer.width as usize),
            Style::new().fg(theme.muted),
        ))),
        panel.footer,
    );
}

fn value_for(field: SettingsField, state: &SettingsState) -> String {
    match field {
        SettingsField::Theme => format!("{:?}", state.draft.theme).to_lowercase(),
        SettingsField::Thinking => yes_no(state.draft.thinking_visible),
        SettingsField::Tools => yes_no(state.draft.tools_expanded),
        SettingsField::EditorExecutable => empty_marker(&state.editor_executable),
        SettingsField::EditorArgs => empty_marker(&state.editor_args.replace('\n', " · ")),
        SettingsField::AgentExecutable => next_start_value(&state.agent_executable),
        SettingsField::AgentConfig => next_start_value(&state.agent_config),
        SettingsField::Apply => "write atomically".to_owned(),
    }
}

fn next_start_value(value: &str) -> String {
    if value.is_empty() {
        "(no saved override; next start)".to_owned()
    } else {
        format!("{value} (next start)")
    }
}

fn yes_no(value: bool) -> String {
    if value {
        "on".to_owned()
    } else {
        "off".to_owned()
    }
}

fn empty_marker(value: &str) -> String {
    if value.is_empty() {
        "(not set)".to_owned()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_cursor_window_reserves_wide_character_at_every_position() {
        for text in [
            "中文tail",
            "left中文right",
            "long-path/中文",
            "中文",
            "e\u{301}尾",
        ] {
            for cursor in text
                .char_indices()
                .map(|(index, _)| index)
                .chain([text.len()])
            {
                for width in [0, 1, 2, 3, 5, 12] {
                    let spans = text_window(text, cursor, width, Style::default());
                    let actual: usize = spans.iter().map(|span| column_width(&span.content)).sum();
                    assert!(
                        actual <= width,
                        "{text:?} cursor={cursor} width={width}: {spans:?}"
                    );
                    if width > 0 {
                        assert!(
                            spans
                                .iter()
                                .any(|span| span.style.add_modifier.contains(Modifier::REVERSED))
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn settings_long_path_window_is_bounded_and_keeps_cursor_context() {
        let text = format!("{}中文TAIL", "/long/".repeat(100_000));
        let spans = text_window(&text, text.len() - 4, 8, Style::default());
        let visible = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(visible.starts_with('…'));
        assert!(visible.contains('T'));
        assert!(column_width(&visible) <= 8);
        assert!(visible.len() < 30);
    }
}
