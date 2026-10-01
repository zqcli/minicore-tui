//! The Help panel (development spec 24.1, 37): keybindings, slash commands,
//! and the honest safety notes. Read-only; scroll lives in `App.panel_scroll`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::markdown::column_width;
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::panel::{self, PanelSpec};

/// The real content height of the Help panel. The command table grows with
/// the implemented command surface, so the scroll bound is derived from the
/// same lines the renderer builds instead of a hardcoded count.
pub(crate) fn content_line_count() -> usize {
    content_lines(&Theme::dark(), 80).len()
}

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let panel = panel::layout(area, PanelSpec::new(0, false, 1));
    panel::render_frame(frame, panel, theme);
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "Help",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))]),
        panel.title,
    );
    let width = panel.content.width as usize;
    let lines = content_lines(theme, width);
    let scroll = app
        .panel_scroll
        .min(lines.len().saturating_sub(panel.content.height as usize));
    panel::render_window(frame, panel.content, &lines, scroll);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            layout::truncate(
                &format!(
                    "↑↓/PgUp/PgDn scroll · Esc/F1 close · q quit · {}–{}/{}",
                    scroll + 1,
                    (scroll + panel.content.height as usize).min(lines.len()),
                    lines.len()
                ),
                panel.footer.width as usize,
            ),
            Style::new().fg(theme.dim),
        ))),
        panel.footer,
    );
}

/// Builds the Help panel body. Shared by the renderer and the scroll bound so
/// the two can never disagree (spec §18.3).
fn content_lines(theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    lines.push(section(theme, "Global", width));
    for (key, what) in [
        ("Ctrl+C", "clear the composer; empty: press again to quit"),
        ("Ctrl+D", "quit when the composer is empty and idle"),
        ("F1", "open/close help; q quits the application"),
        ("Ctrl+R", "session selector"),
        (
            "Ctrl+L",
            "model selector; updates active session at a request boundary",
        ),
        (
            "Shift+Tab",
            "reasoning selector; updates active session at a request boundary",
        ),
        ("Ctrl+N", "open the new-session form"),
        ("F2", "rename the selected session"),
        ("F5", "refresh the session list"),
        ("Ctrl+G (Sessions)", "continue a browsed closed session"),
        ("Ctrl+W", "close the selected session after confirmation"),
        (
            "Delete / Ctrl+D",
            "delete the selected session after close and confirmation",
        ),
        ("Ctrl+O", "expand/collapse all tool cards"),
        ("Ctrl+T", "show/hide reasoning"),
        ("PageUp/PageDown", "scroll; page selectors when focused"),
        ("Ctrl+Home / Ctrl+End", "transcript top / tail"),
        ("Esc", "close a panel; cancel the running turn"),
    ] {
        lines.push(key_value(theme, key, what, width));
    }
    lines.push(Line::default());
    lines.push(section(theme, "Composer", width));
    for (key, what) in [
        ("Enter", "send"),
        ("Ctrl+G", "external editor; read-only session: Continue"),
        ("Shift+Enter / Ctrl+J", "newline"),
        ("Ctrl+A / Ctrl+E", "line start / line end"),
        ("Ctrl+W", "delete previous word"),
        ("Ctrl+Z / Ctrl+Y", "undo / redo"),
        ("Up / Down", "message history at the buffer edges"),
    ] {
        lines.push(key_value(theme, key, what, width));
    }
    lines.push(Line::default());
    lines.push(section(theme, "Slash commands", width));
    for spec in crate::command::COMMANDS {
        let usage = layout::truncate(spec.usage, width);
        lines.push(Line::from(vec![
            Span::styled(format!("{usage:<26}"), Style::new().fg(theme.md_code)),
            Span::styled(spec.summary, Style::new().fg(theme.muted)),
        ]));
    }
    lines.push(Line::default());
    lines.push(section(theme, "Scope", width));
    for note in [
        "Tools run automatically.",
        "Bash is not sandboxed.",
        "No approval UI; no plugin, MCP, or subagent management UI.",
        "Steering and session.update apply at request boundaries.",
        "persisted means appended by this Agent process, not fsync-safe.",
    ] {
        lines.push(Line::from(Span::styled(
            layout::truncate(&format!("· {note}"), width),
            Style::new().fg(theme.muted),
        )));
    }
    lines
}

fn section(theme: &Theme, title: &str, _width: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("{title}:"),
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
    ))
}

fn key_value(theme: &Theme, key: &str, what: &str, width: usize) -> Line<'static> {
    let key_width = column_width(key) + 2;
    let rest = layout::truncate(what, width.saturating_sub(key_width));
    Line::from(vec![
        Span::styled(
            layout::truncate(key, key_width),
            Style::new().fg(theme.accent),
        ),
        Span::styled("  ", Style::new()),
        Span::styled(rest, Style::new().fg(theme.text)),
    ])
}
