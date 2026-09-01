//! The Help panel (development spec 24.1, 37): keybindings, slash commands,
//! and the honest safety notes. Read-only; scroll lives in `App.panel_scroll`.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::markdown::column_width;
use crate::theme::Theme;
use crate::ui::layout;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    frame.render_widget(
        Block::bordered().border_style(Style::new().fg(theme.border_accent)),
        area,
    );
    let inner = area.inner(Margin::new(1, 1));
    let width = inner.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(
            "Help",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
    ];
    lines.push(section(theme, "Global", width));
    for (key, what) in [
        ("Ctrl+C", "clear the composer; empty: press again to quit"),
        ("Ctrl+D", "quit when the composer is empty and idle"),
        ("F1", "help; q closes it (q also quits on fatal errors)"),
        ("Ctrl+R", "session selector"),
        ("Ctrl+L", "model selector (creates a new session)"),
        ("Shift+Tab", "reasoning selector (creates a new session)"),
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
        "/new  /resume  /sessions  /model  /reasoning",
        "/theme dark|light  /clear  /help  /logs  /quit",
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
        "No approval UI, no steering, no compaction in v0.1.",
        "A model/reasoning change creates a new session; the active",
        "session is never modified.",
    ] {
        lines.push(Line::from(Span::styled(
            layout::truncate(&format!("· {note}"), width),
            Style::new().fg(theme.muted),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Esc or F1 closes this panel",
        Style::new().fg(theme.dim),
    )));

    let scroll = app
        .panel_scroll
        .min(lines.len().saturating_sub(inner.height as usize));
    render_scrollable(frame, inner, &lines, scroll);
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

fn render_scrollable(frame: &mut Frame, inner: Rect, lines: &[Line<'static>], scroll: usize) {
    let height = inner.height as usize;
    let window: Vec<Line<'static>> = lines.iter().skip(scroll).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(window), inner);
}
