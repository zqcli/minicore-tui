//! Rendering. Phase 0 renders an empty fullscreen page and a safety hint on
//! undersized terminals; the header, messages, dock, and selectors arrive in
//! later phases.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::theme::Theme;

/// Minimum terminal size before real content is drawn (development spec 14.2).
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 16;

/// Renders the fullscreen page for the current frame: the page background,
/// plus a centered hint instead of content when the terminal is smaller than
/// the supported minimum. `q` and `Ctrl+C` still quit from the hint state.
pub fn render(frame: &mut Frame, theme: &Theme) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(theme.page_bg)), area);
    if is_too_small(area) {
        render_small_terminal_hint(frame, area, theme);
    }
}

fn is_too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

fn render_small_terminal_hint(frame: &mut Frame, area: Rect, theme: &Theme) {
    let [_, hint_area, _] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(4),
        Constraint::Min(0),
    ])
    .areas(area);
    let hint = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "Terminal too small — minimum size ",
                Style::new().fg(theme.warning),
            ),
            Span::styled(
                format!("{MIN_WIDTH}x{MIN_HEIGHT}"),
                Style::new().fg(theme.warning).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            "Enlarge the window, or press q / Ctrl+C to quit",
            Style::new().fg(theme.muted),
        )),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(hint, hint_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn size_thresholds_match_the_minimum_terminal_spec() {
        assert!(is_too_small(Rect::new(0, 0, 59, 16)));
        assert!(is_too_small(Rect::new(0, 0, 60, 15)));
        assert!(!is_too_small(Rect::new(0, 0, 60, 16)));
        assert!(!is_too_small(Rect::new(0, 0, 120, 40)));
    }

    #[test]
    fn empty_fullscreen_fills_the_page_background() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = Theme::dark();
        terminal.draw(|frame| render(frame, &theme)).unwrap();
        for cell in terminal.backend().buffer().content() {
            assert_eq!(cell.bg, theme.page_bg);
        }
    }

    #[test]
    fn small_terminal_renders_the_centered_hint() {
        let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();
        let theme = Theme::dark();
        terminal.draw(|frame| render(frame, &theme)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Terminal too small"));
        assert!(text.contains("60x16"));
        assert!(text.contains("q / Ctrl+C"));
    }
}
