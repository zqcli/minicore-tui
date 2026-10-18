//! Rendering. `ui` draws from `&App` and never mutates domain state beyond the
//! layout cache it owns (`App::layout`), so drawing stays a pure function of
//! the app for everything else.

pub mod blocks;
pub mod composer;
pub mod files;
pub mod markdown;
pub mod rails;
pub mod render;
#[cfg(test)]
pub mod snapshots;
pub mod status;
pub mod theme;
pub mod transcript;
pub mod wrap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;

use crate::app::App;
use crate::transcript::layout::Fold;

use self::render::RenderState;
use self::theme::Styles;

/// The transcript cache plus the streaming renderers it feeds, kept together
/// so the draw path can borrow them without splitting `App`.
#[derive(Default)]
pub struct View {
    pub cache: crate::transcript::layout::Layout<RenderState>,
}

/// Draws one frame: transcript, then the bottom band (status, queue, notices,
/// composer, footer). Fatal errors take the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let styles = Styles::of(app.palette);
    if let Some(fatal) = &app.fatal {
        draw_fatal(frame, area, styles, fatal);
        return;
    }
    let bottom = bottom_height(app, area.width).min(area.height.saturating_sub(1));
    let [transcript, bottom] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(bottom)]).areas(area);
    draw_transcript(frame, transcript, app, styles);
    let rows = status::lines(app, styles, bottom.width);
    frame.render_widget(ratatui::widgets::Paragraph::new(rows), bottom);
    if app.overlay.is_none() && !app.has_interaction() && app.fatal.is_none() {
        let (row, column) = status::cursor(app, styles, bottom.width);
        frame.set_cursor_position((bottom.x + column, bottom.y + row.min(bottom.height)));
    }
}

/// Rows the composer, status line, notices and footer need at `width`. The
/// composer wraps, so the band's height depends on the terminal width.
pub fn bottom_height(app: &App, width: u16) -> u16 {
    (status::lines(app, Styles::of(app.palette), width).len() as u16).max(1)
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App, styles: Styles) {
    let width = area.width.max(1);
    let Some(session) = app.session.as_ref() else {
        let lines = if app.ready {
            vec![
                Line::styled("No session yet", styles.dim()),
                Line::raw(""),
                Line::styled("Press Enter to start one", styles.subtle()),
            ]
        } else {
            vec![Line::styled("Connecting to minicore-agent…", styles.dim())]
        };
        frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
        return;
    };
    let fold = Fold {
        expanded_tools: app.tools_expanded,
        show_reasoning: app.show_reasoning,
    };
    let blocks = session.transcript.blocks();
    app.layout.cache.state.prepare(width, fold, blocks, styles);
    let transcript = self::transcript::Transcript {
        blocks,
        revisions: session.transcript.revisions(),
        width,
        fold,
    };
    let scroll = app.scroll;
    let lines = self::transcript::draw(
        &mut app.layout.cache,
        transcript,
        area,
        scroll,
        styles,
        |state, index, block| state.render(index, block, styles),
    );
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}

fn draw_fatal(frame: &mut Frame, area: Rect, styles: Styles, fatal: &crate::app::Fatal) {
    let mut lines = vec![
        Line::styled("Fatal error", styles.error()),
        Line::raw(""),
        Line::raw(fatal.reason.clone()),
    ];
    if !fatal.stderr_tail.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("agent stderr (tail)", styles.dim()));
        for line in fatal.stderr_tail.iter().rev().take(20).rev() {
            lines.push(Line::styled(line.clone(), styles.subtle()));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Press q to quit", styles.dim()));
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}
