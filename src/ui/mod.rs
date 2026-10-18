//! Rendering. `ui` draws from `&App` and never mutates domain state beyond the
//! layout cache it owns (`App::layout`) and the rows it records for copy.
//!
//! The composition is: the transcript window or a full-screen view, an optional
//! bottom sheet, the status line, the queue, notices, the composer band and the
//! footer. Rendering reads the app; the only mutable state is the transcript's
//! layout cache and the copy buffer.

pub mod blocks;
pub mod composer;
pub mod files;
pub mod markdown;
pub mod panel;
pub mod picker;
pub mod rails;
pub mod render;
pub mod screens;
pub mod selection;
pub mod sheets;
#[cfg(test)]
pub mod snapshots;
pub mod status;
pub mod theme;
pub mod transcript;
pub mod views;
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

impl View {
    /// Rebuilds the cache. The plain closure shape keeps the reducer's test
    /// fixtures (and any non-streaming caller) independent of the renderer
    /// state the draw path threads through.
    pub fn prepare<F>(
        &mut self,
        blocks: &[crate::transcript::Block],
        revisions: &[u64],
        width: u16,
        fold: Fold,
        mut render: F,
    ) where
        F: FnMut(usize, &crate::transcript::Block) -> Vec<Line<'static>>,
    {
        self.cache
            .prepare(blocks, revisions, width, fold, |_, index, block| {
                render(index, block)
            });
    }

    pub fn total_rows(&self) -> usize {
        self.cache.total_rows()
    }

    pub fn anchor(&self, top_row: usize) -> (usize, u16) {
        self.cache.anchor(top_row)
    }

    pub fn anchor_row(&self, block: usize, offset: u16) -> usize {
        self.cache.anchor_row(block, offset)
    }
}

/// Draws one frame. Fatal errors take the whole screen.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if let Some(fatal) = &app.fatal {
        draw_fatal(frame, area, fatal);
        return;
    }
    let styles = Styles::of(app.palette);
    let sheet = screens::sheet(app, &app.palette);
    let sheet_rows = sheet.as_ref().map_or(0, |sheet| sheet.height());
    let bottom = (band_height(app, area.width) + sheet_rows)
        .min(area.height.saturating_sub(1))
        .max(1);
    let [transcript, bottom] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(bottom)]).areas(area);
    if !screens::draw_view(frame, transcript, app) {
        draw_transcript(frame, transcript, app, styles);
    }
    let band = if let Some(sheet) = sheet {
        let [sheet_area, band] =
            Layout::vertical([Constraint::Length(sheet.height()), Constraint::Min(1)])
                .areas(bottom);
        panel::render(frame, sheet_area, &app.palette, &sheet);
        band
    } else {
        bottom
    };
    let rows = status::lines(app, styles, band.width);
    frame.render_widget(ratatui::widgets::Paragraph::new(rows), band);
    if app.overlay.is_none() && !app.has_interaction() {
        let (row, column) = status::cursor(app, styles, band.width);
        frame.set_cursor_position((
            band.x + column,
            band.y + row.min(band.height.saturating_sub(1)),
        ));
    }
}

/// A token count for the UI: `950`, `12.3k`, `200k`, `1.8M`. Whole thousands and
/// millions drop the `.0` so a budget reads as `200k`, not `200.0k`.
pub fn tokens(value: u64) -> String {
    fn scaled(value: f64, suffix: &str) -> String {
        let text = format!("{value:.1}");
        let text = text.strip_suffix(".0").unwrap_or(&text);
        format!("{text}{suffix}")
    }
    match value {
        value if value < 1_000 => value.to_string(),
        value if value < 1_000_000 => scaled(value as f64 / 1_000.0, "k"),
        value => scaled(value as f64 / 1_000_000.0, "M"),
    }
}

/// Rows the status line, queue, notices, composer and footer need at `width`.
/// The composer wraps, so the band's height depends on the terminal width.
pub fn band_height(app: &App, width: u16) -> u16 {
    status::lines(app, Styles::of(app.palette), width).len() as u16
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
    app.viewport = area.height as usize;
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
    // The copy path reads exactly these rows, so a drag copies what is on
    // screen even while a turn streams.
    app.rendered
        .set(area.y, lines.iter().map(|line| line.to_string()).collect());
    app.transcript_area = Some(area);
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}

fn draw_fatal(frame: &mut Frame, area: Rect, fatal: &crate::app::Fatal) {
    let styles = Styles::of(crate::theme::Palette::dark());
    let mut lines = vec![
        Line::styled("Fatal error", styles.error()),
        Line::raw(""),
        Line::styled(fatal.reason.clone(), styles.text()),
    ];
    if !fatal.stderr_tail.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("agent stderr (tail)", styles.dim()));
        for line in fatal.stderr_tail.iter().rev().take(20).rev() {
            lines.push(Line::styled(
                wrap::sanitize(line).into_owned(),
                styles.subtle(),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Press q to quit", styles.dim()));
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::tokens;

    #[test]
    fn token_counts_drop_a_trailing_zero() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_300), "12.3k");
        assert_eq!(tokens(200_000), "200k");
        assert_eq!(tokens(1_800_000), "1.8M");
    }
}
