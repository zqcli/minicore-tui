//! Full-screen views: diff, tool output, context and help. Each replaces the
//! transcript area right of its gutter and keeps the footer; the app owns
//! scrolling and selection.

pub mod context;
pub mod diff;
pub mod help;
pub mod tool;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::commands::COMMANDS;
use crate::app::{App, Overlay};
use crate::wrap;

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

/// The column where `text` starts when it ends at the right edge of `area`.
fn right_edge(area: Rect, text: &str) -> u16 {
    area.right()
        .saturating_sub(wrap::width(text) as u16)
        .max(area.x)
}

/// Shared view chrome: the title row, its name bold and its context in the
/// meta colour, `right` in meta at the row's right edge, and the key-hint row
/// at the bottom.
fn chrome(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    title: &str,
    right: &str,
    hints: &[(&str, &str)],
) {
    let (name, context) = title.split_once(" · ").unwrap_or((title, ""));
    let mut spans = vec![
        Span::styled("← ", styles.meta()),
        Span::styled(name.to_owned(), styles.bold()),
    ];
    if !context.is_empty() {
        spans.push(Span::styled(format!(" · {context}"), styles.meta()));
    }
    let buffer = frame.buffer_mut();
    // The right part keeps its cells; the title gives way, its context first.
    let right_x = right_edge(area, right);
    let title_width = if right.is_empty() {
        area.width
    } else {
        (right_x - area.x).saturating_sub(1)
    };
    buffer.set_line(area.x, area.y, &Line::from(spans), title_width);
    buffer.set_line(
        right_x,
        area.y,
        &styled(right, styles.meta()),
        area.right() - right_x,
    );
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
pub fn draw(frame: &mut Frame, area: Rect, app: &App, styles: Styles) -> bool {
    let area = super::transcript::inner(area);
    match &app.overlay {
        Overlay::Diff(diff) => diff::render(frame, area, styles, diff),
        Overlay::Tool(tool) => tool::render(frame, area, styles, tool, app.session.as_ref()),
        Overlay::Context => {
            let lines = context::lines(app.session.as_ref(), styles);
            context::render(frame, area, styles, &lines);
        }
        Overlay::Help { scroll } => help::render(frame, area, styles, COMMANDS, *scroll),
        _ => return false,
    }
    super::transcript::draw_flash(frame, area, app, styles);
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
