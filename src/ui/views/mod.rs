//! Full-screen views: diff, help, error details, context and session facts. Each
//! replaces the transcript area right of its gutter and keeps the footer; the
//! app owns scrolling and selection.

pub mod context;
pub mod diff;
mod error;
pub mod help;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::commands::COMMANDS;
use crate::app::{App, Overlay};

use super::theme::Styles;

/// One row in one style. The style sits on the span, which is what `set_line` paints.
fn styled(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), style))
}

/// Key hints as the welcome header draws them: the key faint, what it does
/// dim, `·` between pairs.
pub fn key_hints(hints: &[(&str, &str)], styles: Styles) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for (index, (key, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", styles.dim()));
        }
        spans.push(Span::styled((*key).to_owned(), styles.faint()));
        spans.push(Span::styled(format!(" {action}"), styles.dim()));
    }
    spans
}

/// The title row with its context in meta colour, and the bottom key hints.
fn chrome(frame: &mut Frame, area: Rect, styles: Styles, title: &str, hints: &[(&str, &str)]) {
    let (name, context) = title.split_once(" · ").unwrap_or((title, ""));
    let mut spans = vec![
        Span::styled("← ", styles.meta()),
        Span::styled(name.to_owned(), styles.bold()),
    ];
    if !context.is_empty() {
        spans.push(Span::styled(format!(" · {context}"), styles.meta()));
    }
    let buffer = frame.buffer_mut();
    buffer.set_line(area.x, area.y, &Line::from(spans), area.width);
    if area.height > 1 {
        buffer.set_line(
            area.x,
            area.bottom() - 1,
            &Line::from(key_hints(hints, styles)),
            area.width,
        );
    }
}

/// Draws whichever view owns the transcript area; false when none is open.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App, styles: Styles) -> bool {
    let area = super::transcript::inner(area);
    match &mut app.overlay {
        Overlay::Diff(diff) => diff::render(frame, area, styles, diff),
        Overlay::Help { scroll, filter } => {
            help::render(frame, area, styles, COMMANDS, *scroll, filter)
        }
        Overlay::Error {
            text,
            scroll,
            visible_rows,
        } => error::render(frame, area, styles, text, scroll, visible_rows),
        // Read-only facts share one container: the view, not a bottom sheet.
        Overlay::Context | Overlay::SessionInfo => {
            let Some(sheet) = super::sheets::sheet(app, styles) else {
                return false;
            };
            let hints: Vec<(&str, &str)> = sheet
                .hint
                .split(" · ")
                .filter_map(|pair| pair.split_once(' '))
                .collect();
            chrome(frame, area, styles, &sheet.title, &hints);
            let buffer = frame.buffer_mut();
            let rows = area.height.saturating_sub(3);
            for (offset, line) in sheet.body.iter().take(rows.into()).enumerate() {
                buffer.set_line(area.x, area.y + 2 + offset as u16, line, area.width);
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
fn capture(draw: impl FnOnce(&mut Frame), width: u16, height: u16) -> Vec<String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}
