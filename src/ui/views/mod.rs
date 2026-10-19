//! Full-screen views: diff, tool output, context and help. Each replaces the
//! transcript area and keeps the footer; the app owns scrolling and selection.

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

use super::theme::Styles;

/// One row in one style. The style sits on the span, which is what `set_line` paints.
fn styled(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), style))
}

/// Shared view chrome: the title row and the dim hint row.
fn chrome(frame: &mut Frame, area: Rect, styles: Styles, title: &str, hint: &str) {
    let buffer = frame.buffer_mut();
    buffer.set_line(area.x, area.y, &styled(title, styles.title()), area.width);
    if area.height > 1 {
        buffer.set_line(
            area.x,
            area.bottom() - 1,
            &styled(hint, styles.dim()),
            area.width,
        );
    }
}

/// Draws whichever view owns the transcript area; false when none is open.
pub fn draw(frame: &mut Frame, area: Rect, app: &App, styles: Styles) -> bool {
    match &app.overlay {
        Overlay::Diff(diff) => diff::render(frame, area, styles, diff),
        Overlay::Tool(tool) => tool::render(frame, area, styles, tool, app.session.as_ref()),
        Overlay::Context => {
            let lines = context::lines(app.session.as_ref(), styles);
            context::render(frame, area, styles, &lines);
        }
        Overlay::Help { scroll } => {
            let lines = help::lines(COMMANDS, styles);
            help::render(frame, area, styles, &lines, *scroll);
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
