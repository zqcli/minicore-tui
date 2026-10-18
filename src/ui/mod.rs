//! Rendering. `ui` draws from `&App` and never mutates domain state beyond the
//! layout cache it owns (`App::layout`), so drawing stays a pure function of
//! the app for everything else.
//!
//! This module is the ui member's surface: it owns the transcript renderer,
//! the composer, the status line, panels, views, markdown and wrapping. The
//! only thing core fixes here is the entry point [`draw`] and the block
//! rendering contract it calls into.

pub mod blocks;
pub mod files;
pub mod wrap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::app::{App, Overlay, Scroll};
use crate::theme::Palette;
use crate::transcript::layout::Fold;

/// Draws one frame: transcript, status line, composer, footer.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let palette = app.palette;
    if let Some(fatal) = &app.fatal {
        draw_fatal(frame, area, palette, fatal);
        return;
    }
    let bottom = bottom_height(app).min(area.height.saturating_sub(1));
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(bottom)]).split(area);
    draw_transcript(frame, chunks[0], app);
    draw_bottom(frame, chunks[1], app, palette);
}

/// Rows the composer, status line, notices and footer need.
fn bottom_height(app: &App) -> u16 {
    let composer = app.composer.lines().count().max(1) as u16;
    let status = u16::from(app.status_label().is_some());
    let queue = app.queue_len().min(3) as u16;
    let notice = u16::from(!app.notices.is_empty());
    let interaction = app.interaction_lines().len() as u16;
    composer + status + queue + notice + interaction + 1
}

fn draw_transcript(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let palette = app.palette;
    let width = area.width.max(1);
    let Some(session) = app.session.as_ref() else {
        let lines = if app.ready {
            vec![
                Line::styled("No session yet", palette.dim()),
                Line::raw(""),
                Line::styled("Press Enter to start one", palette.faint()),
            ]
        } else {
            vec![Line::styled("Connecting to minicore-agent…", palette.dim())]
        };
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };
    let fold = Fold {
        expanded_tools: app.tools_expanded,
        show_reasoning: app.show_reasoning,
    };
    app.viewport = area.height as usize;
    let blocks = session.transcript.blocks();
    let revisions = session.transcript.revisions();
    app.layout
        .prepare(blocks, revisions, width, fold, |_, block| {
            crate::ui::blocks::render(block, palette, width, fold)
        });
    let total = app.layout.total_rows();
    let height = area.height as usize;
    let top = match app.scroll {
        Scroll::Follow => total.saturating_sub(height),
        Scroll::Anchor { block, offset } => app.layout.anchor_row(block, offset),
    }
    .min(total.saturating_sub(1));
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut row = top;
    while lines.len() < height && row < total {
        let (block, offset) = app.layout.block_at(row);
        match app.layout.lines(block).get(offset) {
            Some(line) => lines.push(line.clone()),
            None => lines.push(Line::raw("")),
        }
        row += 1;
    }
    if app.scroll != Scroll::Follow && total > height {
        let pill = lines.last_mut();
        let text = "↓ new output";
        if let Some(pill) = pill {
            *pill = Line::styled(text, palette.accent());
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_bottom(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App, palette: Palette) {
    let mut rows: Vec<Line<'static>> = Vec::new();
    if let Some(label) = app.status_label() {
        let mut text = format!("⠋ {label}");
        if let Some(elapsed) = app.busy_elapsed() {
            text.push_str(&format!(" · {}", crate::app::format_duration(elapsed)));
        }
        if app.is_busy() {
            text.push_str(" · esc to interrupt");
        }
        rows.push(Line::styled(text, palette.accent()));
    }
    for line in app.queue_lines() {
        rows.push(Line::styled(line, palette.dim()));
    }
    if !app.interaction_lines().is_empty() {
        for (index, line) in app.interaction_lines().into_iter().enumerate() {
            let style = if index == 0 {
                palette.warning()
            } else if line.starts_with('▶') {
                palette.selected()
            } else {
                palette.text()
            };
            rows.push(Line::styled(line, style));
        }
    }
    if let Some(notice) = app.notices.back() {
        let style = match notice.level {
            crate::transcript::block::NoticeLevel::Error => palette.failure(),
            crate::transcript::block::NoticeLevel::Info => palette.dim(),
        };
        rows.push(Line::styled(notice.text.clone(), style));
    }
    let composer_start = rows.len();
    for line in app.composer.lines() {
        rows.push(Line::styled(format!("▎ {line}"), palette.composer()));
    }
    if app.composer.is_empty() {
        rows.push(Line::styled(
            "▎ Ask anything · / commands · @ files",
            palette.composer(),
        ));
    }
    rows.push(Line::styled(footer_text(app), palette.dim()));
    frame.render_widget(Paragraph::new(rows).style(Style::default()), area);

    let cursor_row = area.y + composer_start as u16;
    let last = app.composer.lines().last().unwrap_or("");
    let cursor_column = area.x + 2 + last.chars().count() as u16;
    frame.set_cursor_position((
        cursor_column.min(area.x + area.width.saturating_sub(1)),
        cursor_row.min(area.y + area.height.saturating_sub(1)),
    ));
}

/// Footer line: workspace, branch, session, model, reasoning, state, tokens.
fn footer_text(app: &App) -> String {
    let workspace = app
        .workspace
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.workspace.display().to_string());
    let branch = app
        .session
        .as_ref()
        .and_then(|session| session.presentation.as_ref())
        .and_then(|presentation| presentation.git_branch.clone())
        .unwrap_or_else(|| "-".into());
    let model = app
        .session
        .as_ref()
        .map_or("-".to_owned(), |session| session.info.model.clone());
    let reasoning = app
        .session
        .as_ref()
        .map_or("-", |session| session.info.reasoning.label());
    let state = if app.is_busy() {
        "● working"
    } else if app.compacting() {
        "● compacting"
    } else if app.ready {
        "● ready"
    } else {
        "● connecting"
    };
    let mut text = format!("{workspace}@{branch} · {model} · {reasoning} · {state}");
    if let Some(usage) = app.session.as_ref().map(|session| &session.usage)
        && usage.input + usage.output > 0
    {
        text.push_str(&format!(
            "   ↑{} ↓{} · ctx {}",
            crate::app::format_tokens(usage.input),
            crate::app::format_tokens(usage.output),
            app.context_usage()
                .percent
                .map(|percent| format!("{percent:.0}%"))
                .unwrap_or_else(|| "?".into()),
        ));
    }
    text
}

fn draw_fatal(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    palette: Palette,
    fatal: &crate::app::Fatal,
) {
    let mut lines = vec![
        Line::styled("Fatal error", palette.failure()),
        Line::raw(""),
        Line::raw(fatal.reason.clone()),
    ];
    if !fatal.stderr_tail.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("agent stderr (tail)", palette.dim()));
        for line in fatal.stderr_tail.iter().rev().take(20).rev() {
            lines.push(Line::styled(line.clone(), palette.faint()));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Press q to quit", palette.dim()));
    frame.render_widget(Paragraph::new(lines), area);
}

/// Whether the transcript area is replaced by a full-screen view.
pub fn view_open(app: &App) -> bool {
    app.overlay.is_view()
}

/// Every overlay is the ui member's to render; the core keeps the state only.
pub fn overlay(app: &App) -> Option<&Overlay> {
    (!app.overlay.is_none()).then_some(&app.overlay)
}
