//! Renderer for the local `/settings` form.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::settings::{SettingsField, SettingsState};
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::panel::{self, PanelSpec};

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, state: &SettingsState) {
    let panel = panel::layout(area, PanelSpec::new(0, false, 1));
    panel::render_frame(frame, panel, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Settings",
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
            Line::from(vec![
                Span::styled(if selected { "> " } else { "  " }, style),
                Span::styled(format!("{:<25}", field.label()), style),
                Span::styled(
                    layout::truncate(&value, panel.content.width as usize),
                    style,
                ),
            ])
        })
        .collect::<Vec<_>>();
    panel::render_window(frame, panel.content, &rows, 0);
    let footer = if let Some(error) = state.error.as_deref() {
        format!("{error} · Tab/↑↓ move · Ctrl+S apply · Esc close")
    } else {
        "Tab/↑↓ move · Enter toggle/apply · Ctrl+U clear text · Ctrl+S apply · Esc close".to_owned()
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
        SettingsField::AgentExecutable => empty_marker(&state.agent_executable),
        SettingsField::AgentConfig => empty_marker(&state.agent_config),
        SettingsField::Apply => "write atomically".to_owned(),
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
