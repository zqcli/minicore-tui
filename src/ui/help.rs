//! The Help panel (development spec 24.1, 37): keybindings, slash commands,
//! and the honest safety notes. Read-only; scroll lives in `App.panel_scroll`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::markdown::{column_width, wrap_plain};
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::panel::{self, PanelSpec};

/// The real content height of the Help panel. The command table grows with
/// the implemented command surface, so the scroll bound is derived from the
/// same lines the renderer builds instead of a hardcoded count.
pub(crate) fn content_line_count(width: usize) -> usize {
    content_lines(&Theme::dark(), width).len()
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
            Style::new().fg(theme.text),
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
        lines.extend(described_lines(
            key,
            what,
            width,
            0,
            Style::new().fg(theme.accent),
            Style::new().fg(theme.text),
        ));
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
        lines.extend(described_lines(
            key,
            what,
            width,
            0,
            Style::new().fg(theme.accent),
            Style::new().fg(theme.text),
        ));
    }
    lines.push(Line::default());
    lines.push(section(theme, "Slash commands", width));
    for spec in crate::command::COMMANDS {
        lines.extend(described_lines(
            spec.usage,
            spec.summary,
            width,
            26,
            Style::new().fg(theme.md_code),
            Style::new().fg(theme.muted),
        ));
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
        lines.extend(wrap_words(
            &format!("· {note}"),
            width,
            Style::new().fg(theme.muted),
        ));
    }
    lines
}

fn section(theme: &Theme, title: &str, _width: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("{title}:"),
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
    ))
}

/// Keep compact rows when they fit, otherwise put the complete explanation
/// below the key/usage. Long usages always retain a visible separator.
fn described_lines(
    label: &str,
    description: &str,
    width: usize,
    minimum_label_width: usize,
    label_style: Style,
    description_style: Style,
) -> Vec<Line<'static>> {
    let label_width = column_width(label);
    let padded = (label_width + 2).max(minimum_label_width);
    if padded + column_width(description) <= width {
        return vec![Line::from(vec![
            Span::styled(label.to_owned(), label_style),
            Span::raw(" ".repeat(padded - label_width)),
            Span::styled(description.to_owned(), description_style),
        ])];
    }
    let mut lines = wrap_words(label, width, label_style);
    let indent = 2.min(width.saturating_sub(1));
    lines.extend(
        wrap_words(description, width.saturating_sub(indent), description_style)
            .into_iter()
            .map(|line| layout::left_pad(line, indent)),
    );
    lines
}

/// Static Help prose wraps at word boundaries. Only an overlong token uses
/// the existing Unicode cell wrapper; no user content or Markdown is parsed.
fn wrap_words(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut row = String::new();
    let mut cells = 0;
    for word in text.split_whitespace() {
        let word_cells = column_width(word);
        if !row.is_empty() && cells + 1 + word_cells > width {
            lines.push(Line::from(Span::styled(std::mem::take(&mut row), style)));
            cells = 0;
        }
        if word_cells > width {
            lines.extend(wrap_plain(word, width, style));
            continue;
        }
        if !row.is_empty() {
            row.push(' ');
            cells += 1;
        }
        row.push_str(word);
        cells += word_cells;
    }
    if !row.is_empty() {
        lines.push(Line::from(Span::styled(row, style)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::line_width;

    #[test]
    fn help_keeps_complete_descriptions_inside_supported_panel_widths() {
        for width in [57, 77, 117, 157] {
            let lines = content_lines(&Theme::dark(), width);
            for line in &lines {
                assert!(
                    line_width(line) <= width,
                    "Help row exceeds {width}: {line}"
                );
            }
            let joined = lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            let normalized = joined.split_whitespace().collect::<Vec<_>>().join(" ");
            for command in crate::command::COMMANDS {
                assert!(
                    normalized.contains(command.usage),
                    "missing full /{} usage at {width}",
                    command.name
                );
                assert!(
                    normalized.contains(command.summary),
                    "missing full /{} explanation at {width}",
                    command.name
                );
            }
            for note in [
                "model selector; updates active session at a request boundary",
                "delete the selected session after close and confirmation",
                "No approval UI; no plugin, MCP, or subagent management UI.",
                "persisted means appended by this Agent process, not fsync-safe.",
            ] {
                assert!(
                    normalized.contains(note),
                    "missing complete Help note at {width}: {note}"
                );
            }
        }
    }

    #[test]
    fn help_long_usage_has_a_separator_and_unicode_tokens_keep_all_cells() {
        let theme = Theme::dark();
        let tool = crate::command::command_spec("tool").unwrap();
        let wide = described_lines(
            tool.usage,
            tool.summary,
            160,
            26,
            Style::new().fg(theme.md_code),
            Style::new().fg(theme.muted),
        );
        assert_eq!(wide.len(), 1);
        assert!(
            wide[0]
                .to_string()
                .contains(&format!("{}  {}", tool.usage, tool.summary))
        );
        assert_eq!(wide[0].spans[0].style.fg, Some(theme.md_code));
        assert_eq!(wide[0].spans[2].style.fg, Some(theme.muted));
        for text in ["界".repeat(30), "e\u{301}".repeat(30)] {
            for width in [3, 5, 12] {
                let rows = wrap_words(&text, width, Style::default());
                assert!(rows.iter().all(|row| line_width(row) <= width));
                assert_eq!(
                    rows.iter().map(ToString::to_string).collect::<String>(),
                    text
                );
            }
        }
    }

    #[test]
    fn help_paging_and_end_use_the_rendered_width_after_resize() {
        use crate::event::AppEvent;
        use crate::theme::ThemeKind;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = crate::ui::testapp::help(ThemeKind::Dark);
        let key = |code| AppEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::empty())));
        let mut previous_count = None;
        for (width, height) in [(160, 48), (80, 24), (60, 16), (160, 48)] {
            app.update(AppEvent::TerminalSize { width, height });
            let screen = layout::screen_layout(&app, Rect::new(0, 0, width, height));
            let panel = panel::layout(screen.panel, PanelSpec::new(0, false, 1));
            let count = content_line_count(panel.content.width as usize);
            let last = count.saturating_sub(panel.content.height as usize);
            app.update(key(KeyCode::Home));
            assert_eq!(app.panel_scroll, 0);
            for _ in 0..count {
                app.update(key(KeyCode::PageDown));
            }
            assert_eq!(
                app.panel_scroll, last,
                "paging reaches exactly the wrapped tail"
            );
            app.update(key(KeyCode::Home));
            app.update(key(KeyCode::End));
            assert_eq!(app.panel_scroll, last);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &app))
                .unwrap();
            let visible = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                visible.contains("fsync-safe."),
                "last safety note is reachable at {width}x{height}"
            );
            assert!(visible.contains(&format!("/{count}")));
            let footer_cell = terminal
                .backend()
                .buffer()
                .cell((panel.footer.x, panel.footer.y))
                .unwrap();
            assert_eq!(
                footer_cell.fg,
                Theme::dark().text,
                "Help controls must not use the low-contrast dim color"
            );
            if width == 60 {
                assert!(
                    count > previous_count.unwrap(),
                    "narrow width adds real wrapped rows"
                );
            }
            previous_count = Some(count);
        }
    }
}
