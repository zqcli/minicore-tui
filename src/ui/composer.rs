//! The composer (development spec 15.7, 21, 22.2): a rounded border colored
//! by the active session's reasoning level, the buffered lines rendered
//! read-only from the `Composer` wrapper (wrapped with the same
//! `unicode-width` math as the transcript), a block cursor, and the
//! hardware cursor positioned from the (row, column) cell so IME lands
//! correctly on multi-line buffers.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::App;
use crate::markdown::{char_width, wrap_plain};
use crate::state::composer::MAX_COMPOSER_BYTES;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let view = app.active_view();
    let waiting = view.is_some_and(|view| {
        view.live.as_ref().is_some_and(|live| live.waiting)
            || view.state.as_ref().is_some_and(|state| {
                state.status == crate::protocol::SessionStatusWire::WaitingForInput
            })
    });
    let finishing = view.is_some_and(|view| {
        view.state
            .as_ref()
            .is_some_and(|state| state.status == crate::protocol::SessionStatusWire::Finishing)
    });
    let running = view.is_some_and(|view| view.is_running());
    let border_color = match view {
        Some(view) => {
            let reasoning = view
                .live
                .as_ref()
                .and_then(|l| l.requests.last().map(|r| r.reasoning))
                .or_else(|| view.last_request.as_ref().map(|r| r.reasoning))
                .unwrap_or(view.info.reasoning);
            theme.reasoning_color(reasoning)
        }
        None => theme.thinking_disabled,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border_color).add_modifier(Modifier::BOLD));
    frame.render_widget(block, area);

    let inner = area.inner(Margin::new(1, 1));
    let width = inner.width as usize;
    let contents = compose_lines(app, theme, width, running, waiting, finishing);
    let (cursor_row, cursor_col) = cursor_cell(app, width);

    // Keep the cursor row visible when the buffer overflows the panel.
    let height = inner.height as usize;
    let top = if contents.is_empty() {
        0
    } else {
        cursor_row
            .min(contents.len().saturating_sub(1))
            .saturating_sub(height - 1)
    };
    let rows: Vec<Line<'static>> = contents.iter().skip(top).take(height).cloned().collect();
    // Flat for the paragraph model does not apply here: forced rows.
    let mut padded = rows;
    while padded.len() < height {
        padded.push(Line::default());
    }
    frame.render_widget(Paragraph::new(padded), inner);

    // Block cursor + hardware cursor at the (row, column) cell, so IME
    // composition and multi-line editing land on the right cell. The cursor
    // column can sit exactly at the wrap boundary (== width), which has no
    // cell: clamp drawing into the inner area while the helper keeps the
    // true boundary column for logic.
    let (cell_row, cell_col) = (cursor_row.saturating_sub(top), cursor_col);
    let inner_w = inner.width as usize;
    let draw_col = cell_col.min(inner_w.saturating_sub(1));
    let x = inner.x + draw_col as u16;
    let y = inner.y + cell_row as u16;
    if x < inner.x + inner.width && y < inner.y + inner.height && inner_w > 0 && inner.height > 0 {
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            cell.set_fg(theme.page_bg);
            cell.set_bg(theme.text);
        }
        frame.set_cursor_position((x, y));
    }
}

/// The wrapped composer rows, styled as plain text (the buffer may be
/// empty, in which case a placeholder row is returned by `compose_lines`).
fn compose_lines(
    app: &App,
    theme: &Theme,
    width: usize,
    running: bool,
    waiting: bool,
    finishing: bool,
) -> Vec<Line<'static>> {
    let style = Style::new().fg(theme.text);
    if app.composer.is_empty() {
        let blocked = app.active_view().is_some_and(|view| {
            view.state
                .as_ref()
                .is_some_and(|state| state.status == crate::protocol::SessionStatusWire::Blocked)
        });
        let placeholder = if blocked {
            "Session blocked"
        } else if running {
            "Steer current turn…"
        } else if waiting {
            "Unsupported interaction — Esc to cancel"
        } else if finishing {
            "Saving turn…"
        } else if app.active_view().is_none() {
            "Create or open a session"
        } else {
            "Type a message…"
        };
        return vec![Line::styled(placeholder, Style::new().fg(theme.muted))];
    }
    let mut rows = Vec::new();
    for raw in app.composer.lines() {
        rows.extend(wrap_plain(raw, width, style));
    }
    if app.composer.content().len() >= MAX_COMPOSER_BYTES * 9 / 10 {
        rows.push(Line::styled(
            format!(
                "{}/{} bytes",
                app.composer.content().len(),
                MAX_COMPOSER_BYTES
            ),
            Style::new().fg(theme.muted),
        ));
    }
    rows
}

/// The visual (row, col) of the block cursor in wrapped-cell space.
/// `Composer::cursor()` is (row, **char index**, as tui-textarea reports);
/// this converts to display columns here using the exact greedy rule as
/// `wrap_plain`/`chunk_line`, so the rendered row/col always matches the
/// wrapping the renderer (and the height estimator) use.
fn cursor_cell(app: &App, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let (line_index, char_col) = app.composer.cursor();
    let mut cell_row = 0;
    for (index, raw) in app.composer.lines().iter().enumerate() {
        if index == line_index {
            let (rows, col) = cursor_wrap_pos(raw, char_col, width);
            return (cell_row + rows, col);
        }
        cell_row += wrap_plain(raw, width, Style::new()).len();
    }
    (0, 0)
}

/// Simulates the greedy wrap exactly like `chunk_line`: the visual row and
/// display column of the cursor after `cursor_col` characters of `line`
/// wrapped to `width`. The column may equal `width` at a wrap boundary.
fn cursor_wrap_pos(line: &str, cursor_col: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut rows = 0usize;
    let mut used = 0usize;
    for (index, ch) in line.chars().enumerate() {
        if index == cursor_col {
            break;
        }
        let cw = char_width(ch);
        if used + cw > width && used > 0 {
            rows += 1;
            used = 0;
        }
        used += cw;
    }
    (rows, used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::event::AppEvent;
    use crate::theme::ThemeKind;
    use ratatui::Terminal;
    use ratatui::backend::{Backend, TestBackend};

    fn app_with(text: &str) -> App {
        let mut app = App::new(std::path::PathBuf::from("/ws"));
        app.update(AppEvent::SetTheme(ThemeKind::Dark));
        // Paste is a single bounded edit through App::update.
        if !text.is_empty() {
            app.update(AppEvent::Terminal(crossterm::event::Event::Paste(
                text.to_owned(),
            )));
        }
        app
    }

    #[test]
    fn cursor_wrap_pos_matches_the_greedy_rule() {
        // ASCII exactly at the wrap boundary (col == width).
        assert_eq!(cursor_wrap_pos("abcdef", 6, 3), (1, 3));
        assert_eq!(cursor_wrap_pos("abcdef", 5, 3), (1, 2));
        assert_eq!(
            cursor_wrap_pos("abcdef", 3, 3),
            (0, 3),
            "boundary col can equal width"
        );
        // CJK counts 2 columns each.
        assert_eq!(cursor_wrap_pos("你好世界", 4, 4), (1, 4));
        assert_eq!(cursor_wrap_pos("你好", 2, 4), (0, 4));
        // Emoji + combining marks use display widths.
        assert_eq!(cursor_wrap_pos("😀a\u{301}", 3, 4), (0, 3));
        // width 1 and 0 are defensive.
        assert_eq!(cursor_wrap_pos("ab", 2, 1), (1, 1));
        assert_eq!(cursor_wrap_pos("ab", 2, 0), (1, 1));
        // Multi logical lines: cursor_row counts whole previous lines.
        // A trailing cursor past the end lands at the end.
        assert_eq!(cursor_wrap_pos("hello world", 11, 4), (2, 3));
    }

    #[test]
    fn cursor_cell_is_the_visual_wrapped_position() {
        let app = app_with("abcdef\nghijkl");
        // cursor() col is a char index from tui-textarea.
        let (row, col) = app.composer.cursor();
        assert_eq!((row, col), (1, 6));
        assert_eq!(
            cursor_cell(&app, 3),
            (1 + 2, 3),
            "row 0 wraps to 2 rows then row 1 ends at col 3"
        );
    }

    #[test]
    fn long_wrapped_line_cursor_is_visible_with_correct_hardware_position() {
        let mut app = app_with("x".repeat(70).as_str());
        // Move the cursor to the very end (char index 70) — row 0, col 70.
        // Inner width is 78 -> 0..  ok, not wrapping. Now use a 79-char line
        // so it wraps into two visual rows inside the 78-column inner area.
        let long = "a".repeat(79);
        app.composer.set_text(&long);
        let (row, col) = app.composer.cursor();
        assert_eq!((row, col), (0, 79));
        let (cursor_row, cursor_col) = cursor_cell(&app, 78);
        assert_eq!(
            (cursor_row, cursor_col),
            (1, 1),
            "78 cols on row 0, cursor at 79th char starts row 1 col 1"
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let pos = terminal.backend_mut().get_cursor_position().unwrap();
        // Composer area: dock = footer(2) + composer(5) => composer at
        // y=17..23, inner y=18..21, inner.x=1. Cursor row 1 -> y=19, col 1 -> x=2.
        assert_eq!((pos.x, pos.y), (2, 19));
    }

    #[test]
    fn cjk_cursor_hardware_position_uses_display_columns() {
        let mut app = app_with("你好abc");
        assert_eq!(app.composer.cursor(), (0, 5));
        assert_eq!(
            cursor_cell(&app, 78),
            (0, 7),
            "prefix 你好abc is 7 display columns"
        );
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Left,
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
        assert_eq!(app.composer.cursor(), (0, 4));
        assert_eq!(
            cursor_cell(&app, 78),
            (0, 6),
            "col 4 = prefix 你好ab, 6 display columns"
        );
    }
}
