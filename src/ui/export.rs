//! Export form panel (spec §17.4): a local target path plus the explicit
//! content choices. Nothing is written until the form is submitted, the
//! overwrite decision is its own toggle, and every limitation the file will
//! carry is visible here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::state::export::{ExportFormState, ExportPhase};
use crate::theme::Theme;
use crate::ui::layout::{fill_line, truncate};

pub fn render(frame: &mut Frame, area: Rect, theme: &Theme, form: &ExportFormState) {
    frame.render_widget(Clear, area);
    let border = Style::new().fg(theme.border);
    let block = Block::bordered()
        .title(panel_title(form))
        .border_style(border)
        .style(Style::new().bg(theme.page_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let option_rows = toggle_lines(form, theme, inner.width as usize).len() as u16;
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(option_rows),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    render_target(frame, rows[0], form, theme);
    render_toggles(frame, rows[1], form, theme);
    render_progress(frame, rows[2], form, theme);
    render_notice(frame, rows[3], form, theme);
    render_notes(frame, rows[4], form, theme);
    render_hints(frame, rows[5], form, theme);
}

fn panel_title(form: &ExportFormState) -> String {
    let phase = match form.phase {
        ExportPhase::Editing => "form",
        ExportPhase::Running => "writing",
        ExportPhase::Cancelling => "cancelling",
        ExportPhase::Done => "done",
        ExportPhase::Failed => "stopped",
    };
    format!(" Export — {phase} ")
}

fn render_target(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    let width = area.width.saturating_sub(9) as usize;
    let (target, cursor) = if form.running() {
        (
            vec![Span::styled(
                truncate(
                    &crate::safe_text::safe_display(&form.target).replace('\t', "    "),
                    width,
                ),
                Style::new().fg(theme.text),
            )],
            0,
        )
    } else {
        crate::ui::layout::single_line_window(
            &form.target,
            form.target_cursor,
            width,
            Style::new().fg(theme.text),
        )
    };
    let mut spans = vec![Span::styled(" target> ", Style::new().fg(theme.accent))];
    spans.extend(target);
    frame.render_widget(
        Paragraph::new(fill_line(
            Line::from(spans),
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
    if !form.running() && area.width > 9 && area.height > 0 {
        frame.set_cursor_position((area.x + 9 + cursor as u16, area.y));
    }
}

fn render_toggles(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    frame.render_widget(
        Paragraph::new(toggle_lines(form, theme, area.width as usize)),
        area,
    );
}

/// Keep every option and its checked state together, including at the
/// supported 60-column minimum. The form already reserves space for these
/// rows, so narrow layouts need not hide the overwrite decision.
fn toggle_lines(form: &ExportFormState, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let options = [
        ("Ctrl+T thinking", form.spec.include_thinking),
        ("Ctrl+P tools", form.spec.include_tool),
        ("Ctrl+N unsaved turn", form.include_unsaved),
        ("Ctrl+Y overwrite", form.overwrite),
        ("Ctrl+R raw oversized", form.spec.raw_oversized),
    ];
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut used = 0;
    for (label, enabled) in options {
        let option = toggle(label, enabled, theme);
        let length = option.content.len();
        if !spans.is_empty() && used + 3 + length > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        if !spans.is_empty() {
            spans.push(Span::raw("   "));
            used += 3;
        }
        used += length;
        spans.push(option);
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn toggle(label: &str, on: bool, theme: &Theme) -> Span<'static> {
    let style = if on {
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.text)
    };
    Span::styled(format!("[{}] {label}", if on { "x" } else { " " }), style)
}

fn render_progress(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    let text = match form.phase {
        ExportPhase::Editing => {
            if form.include_unsaved {
                "saved history + unsaved turn; nothing written yet".to_owned()
            } else {
                "saved history only; nothing has been written yet".to_owned()
            }
        }
        ExportPhase::Running => format!(
            "writing… {} item(s), {} bytes staged; not committed",
            form.items, form.bytes
        ),
        ExportPhase::Cancelling => {
            "cancelling: waiting for the writer to confirm whether it committed".to_owned()
        }
        ExportPhase::Done => format!("wrote {} item(s), {} bytes", form.items, form.bytes),
        ExportPhase::Failed => "no file was committed".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(fill_line(
            Line::from(Span::styled(text, Style::new().fg(theme.dim))),
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
}

fn render_notice(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    let Some(notice) = form.notice.as_deref() else {
        return;
    };
    frame.render_widget(
        Paragraph::new(fill_line(
            Line::from(Span::styled(
                truncate(notice, area.width as usize),
                Style::new().fg(theme.warning),
            )),
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
}

fn render_notes(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let notes = form.limitations.notes();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for note in notes.iter().take(area.height as usize) {
        lines.push(Line::from(Span::styled(
            truncate(note, area.width.saturating_sub(2) as usize),
            Style::new().fg(theme.warning),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "the target file records its source and any limitation",
            Style::new().fg(theme.dim),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_hints(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    let hint = match form.phase {
        ExportPhase::Running => "Esc cancel · the temporary file is removed",
        ExportPhase::Cancelling => "waiting for the writer's typed outcome…",
        ExportPhase::Editing | ExportPhase::Done | ExportPhase::Failed => {
            if area.width >= 100 {
                "Enter export · ←→ Home/End edit · Del delete · Ctrl+T/P/N/Y/R options · Esc close"
            } else {
                "Enter export · Ctrl+T/P/N/Y/R options · Esc close"
            }
        }
    };
    let color = if form.phase == ExportPhase::Cancelling {
        theme.dim
    } else {
        theme.text
    };
    frame.render_widget(
        Paragraph::new(fill_line(
            Line::from(Span::styled(hint, Style::new().fg(color))),
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};

    fn drawn_buffer(form: &ExportFormState, width: u16, height: u16, theme: &Theme) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), theme, form))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn drawn(form: &ExportFormState, width: u16, height: u16) -> String {
        let buffer = drawn_buffer(form, width, height, &Theme::dark());
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_text_style(buffer: &Buffer, text: &str, color: Color, bold: bool) {
        for row in buffer.content().chunks(buffer.area.width as usize) {
            let line: String = row.iter().map(|cell| cell.symbol()).collect();
            if let Some(offset) = line.find(text) {
                let start = line[..offset].chars().count();
                for cell in &row[start..start + text.chars().count()] {
                    assert_eq!(cell.fg, color, "{text}: {cell:?}");
                    assert_eq!(cell.modifier.contains(Modifier::BOLD), bold, "{text}");
                }
                return;
            }
        }
        panic!("rendered text missing: {text}");
    }

    #[test]
    fn unchecked_export_options_are_readable_and_checked_options_keep_their_emphasis() {
        for theme in [Theme::dark(), Theme::light()] {
            for width in [57, 77, 157] {
                let mut form = ExportFormState::new("conversation.md".into());
                let buffer = drawn_buffer(&form, width, 12, &theme);
                let labels = [
                    "Ctrl+T thinking",
                    "Ctrl+P tools",
                    "Ctrl+N unsaved turn",
                    "Ctrl+Y overwrite",
                    "Ctrl+R raw oversized",
                ];
                for label in labels {
                    assert_text_style(&buffer, &format!("[ ] {label}"), theme.text, false);
                }
                form.spec.include_thinking = true;
                form.spec.include_tool = true;
                form.include_unsaved = true;
                form.overwrite = true;
                form.spec.raw_oversized = true;
                let buffer = drawn_buffer(&form, width, 12, &theme);
                for label in labels {
                    assert_text_style(&buffer, &format!("[x] {label}"), theme.accent, true);
                }
            }
        }
    }

    #[test]
    fn export_hints_emphasize_actions_without_promoting_informational_text() {
        for theme in [Theme::dark(), Theme::light()] {
            for width in [57, 77, 157] {
                let mut form = ExportFormState::new("conversation.md".into());
                let buffer = drawn_buffer(&form, width, 12, &theme);
                assert_text_style(&buffer, "nothing has been written yet", theme.dim, false);
                assert_text_style(
                    &buffer,
                    "the target file records its source",
                    theme.dim,
                    false,
                );
                for (phase, hint, color) in [
                    (ExportPhase::Editing, "Enter export", theme.text),
                    (ExportPhase::Running, "Esc cancel", theme.text),
                    (ExportPhase::Cancelling, "waiting for the writer", theme.dim),
                    (ExportPhase::Done, "Enter export", theme.text),
                    (ExportPhase::Failed, "Enter export", theme.text),
                ] {
                    form.phase = phase;
                    let buffer = drawn_buffer(&form, width, 12, &theme);
                    assert_text_style(&buffer, hint, color, false);
                }
            }
        }
    }

    #[test]
    fn export_options_remain_visible_at_supported_widths() {
        for width in [57, 60, 77, 80, 157] {
            let form = ExportFormState::new("conversation.md".into());
            let text = drawn(&form, width, 12);
            for label in [
                "[ ] Ctrl+T thinking",
                "[ ] Ctrl+P tools",
                "[ ] Ctrl+N unsaved turn",
                "[ ] Ctrl+Y overwrite",
                "[ ] Ctrl+R raw oversized",
            ] {
                assert!(text.contains(label), "width {width} lost {label}: {text}");
            }
            assert!(text.contains("Enter export"));
            assert!(text.contains("Esc close"));
        }
    }

    #[test]
    fn raw_and_unsaved_choices_have_explicit_current_state() {
        let mut form = ExportFormState::new("conversation.md".into());
        form.spec.raw_oversized = true;
        form.include_unsaved = true;
        form.overwrite = true;
        let text = drawn(&form, 60, 12);
        assert!(text.contains("[x] Ctrl+R raw oversized"));
        assert!(text.contains("[x] Ctrl+N unsaved turn"));
        assert!(text.contains("[x] Ctrl+Y overwrite"));
        assert!(text.contains("saved history + unsaved turn"));
        assert!(!text.contains("saved history only"));
    }

    #[test]
    fn writing_progress_is_staged_and_does_not_claim_a_commit() {
        let mut form = ExportFormState::new("conversation.md".into());
        form.phase = ExportPhase::Running;
        form.items = 123;
        form.bytes = 45678;
        let text = drawn(&form, 60, 12);
        assert!(text.contains("123 item(s), 45678 bytes staged"));
        assert!(text.contains("not committed"));
        assert!(!text.contains("wrote 123"));
    }
}
