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

/// The help renderer builds a fixed, one-row-per-entry list. The reducer uses
/// this count with the shared content rectangle for Home/End/Page scrolling.
pub(crate) fn content_line_count() -> usize {
    37
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
    let mut lines = vec![Line::default()];
    lines.push(section(theme, "Global", width));
    for (key, what) in [
        ("Ctrl+C", "clear the composer; empty: press again to quit"),
        ("Ctrl+D", "quit when the composer is empty and idle"),
        ("F1", "help; q closes it (q also quits on fatal errors)"),
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
        ("Ctrl+W", "close the selected session after confirmation"),
        (
            "Delete / Ctrl+D",
            "delete the selected session after close and confirmation",
        ),
        ("Ctrl+O", "expand/collapse all tool cards"),
        ("Ctrl+T", "show/hide reasoning"),
        ("PageUp/PageDown", "scroll; page selectors when focused"),
        ("Home / End", "transcript top / tail"),
        ("Esc", "close a panel; cancel the running turn"),
    ] {
        lines.push(key_value(theme, key, what, width));
    }
    lines.push(Line::default());
    lines.push(section(theme, "Composer", width));
    for (key, what) in [
        ("Enter", "send"),
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
    for command in [
        "/new  /resume  /sessions  /model  /reasoning  /cancel  /refresh",
        "/close [confirm]  /delete [confirm]  /theme dark|light  /clear  /help  /logs  /quit",
    ] {
        lines.push(Line::from(Span::styled(
            command,
            Style::new().fg(theme.md_code),
        )));
    }
    lines.push(Line::default());
    lines.push(section(theme, "Scope", width));
    for note in [
        "Tools run automatically.",
        "Bash is not sandboxed.",
        "No approval UI; no compaction, plugin, MCP, or subagent UI.",
        "Steering and session.update apply at request boundaries.",
        "persisted means appended by this Agent process, not fsync-safe.",
    ] {
        lines.push(Line::from(Span::styled(
            layout::truncate(&format!("· {note}"), width),
            Style::new().fg(theme.muted),
        )));
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "Esc or F1 closes this panel",
            Style::new().fg(theme.dim),
        ))]),
        panel.footer,
    );

    let scroll = app
        .panel_scroll
        .min(lines.len().saturating_sub(panel.content.height as usize));
    panel::render_window(frame, panel.content, &lines, scroll);
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
