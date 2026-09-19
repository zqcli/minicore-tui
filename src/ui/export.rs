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
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
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
    let cursor = if form.running() { "" } else { "▏" };
    let line = Line::from(vec![
        Span::styled(" target> ", Style::new().fg(theme.accent)),
        Span::styled(
            truncate(
                &format!("{}{cursor}", form.target),
                area.width.saturating_sub(9) as usize,
            ),
            Style::new().fg(theme.text),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(fill_line(
            line,
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
}

fn render_toggles(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    let line = Line::from(vec![
        toggle("Ctrl+T thinking", form.spec.include_thinking, theme),
        Span::raw("   "),
        toggle("Ctrl+P tools", form.spec.include_tool, theme),
        Span::raw("   "),
        toggle("Ctrl+N unsaved turn", form.include_unsaved, theme),
        Span::raw("   "),
        toggle("Ctrl+Y overwrite", form.overwrite, theme),
    ]);
    frame.render_widget(
        Paragraph::new(fill_line(
            line,
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
}

fn toggle(label: &str, on: bool, theme: &Theme) -> Span<'static> {
    let style = if on {
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(theme.dim)
    };
    Span::styled(format!("[{}] {label}", if on { "x" } else { " " }), style)
}

fn render_progress(frame: &mut Frame, area: Rect, form: &ExportFormState, theme: &Theme) {
    let text = match form.phase {
        ExportPhase::Editing => "saved history only; nothing has been written yet".to_owned(),
        ExportPhase::Running => format!("writing… {} item(s) forwarded", form.items),
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
            "Enter export · Ctrl+T/P/N/Y options · Esc close"
        }
    };
    frame.render_widget(
        Paragraph::new(fill_line(
            Line::from(Span::styled(hint, Style::new().fg(theme.dim))),
            area.width as usize,
            Style::new().bg(theme.page_bg),
        )),
        area,
    );
}
