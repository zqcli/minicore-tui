//! Rendering: the Pi-style fullscreen conversation layout (development spec
//! 14-20, 29-31). `ui::render` is a pure read-only view: it never mutates
//! `App` or writes caches. Durable transcript lines are prepared by the
//! read-only `transcript::prepare_cache` function and installed only through
//! `App::update`; header, notices, layout and live streaming remain per-frame
//! derivations without interior mutability.
//!
//! Phase 3 covers the transcript + fixed dock (status/composer/footer), the
//! durable/live blocks, and markdown. Phase 4 replaces the composer in the
//! dock with the new-session form and the session/model/reasoning/profile
//! selectors; Phase 5 adds full input and scrolling; Phase 7 adds the
//! update-installed durable line cache.

pub mod assistant;
pub mod composer;
pub mod error;
pub mod footer;
pub mod header;
pub mod help;
pub mod layout;
pub mod logs;
pub mod new_session;
pub mod reasoning;
pub mod selector;
pub mod status;
pub mod tool;
pub mod transcript;
pub mod user;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, ConnectionState};
use crate::state::selection::Dock;
use crate::theme::Theme;

#[cfg(test)]
mod component_tests;
#[cfg(test)]
mod snapshots;
#[cfg(test)]
pub(crate) mod testapp;

/// Renders the fullscreen page: the page background, the transcript above a
/// fixed dock, or the safety hint / fatal overlay when they apply.
pub fn render(frame: &mut Frame, app: &App) {
    let theme = Theme::for_kind(app.theme);
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(theme.page_bg)), area);
    if layout::is_too_small(area) {
        render_small_terminal_hint(frame, area, &theme);
        return;
    }
    if let ConnectionState::Failed(reason) = &app.connection {
        let known_result = app.active_view().and_then(|view| {
            view.can_show_last_result()
                .then_some(view.last_result.as_ref())
                .flatten()
                .map(status::result_summary)
        });
        error::render_fatal(
            frame,
            area,
            &theme,
            reason,
            app.child_exit_status.as_deref(),
            &app.agent_logs,
            error::FatalResultState {
                known_result: known_result.as_deref(),
                unconfirmed: app
                    .active_view()
                    .is_some_and(|view| view.result_unconfirmed),
            },
        );
        return;
    }
    let short = area.height < 24;
    let busy = layout::busy(app);
    // Selector / new-session panels replace the composer in the dock and
    // are taller (spec 24.2); the composer keeps its Phase 3 height.
    // The panel is the composer (which now grows with its content), one of
    // the selectors, or Help/Logs (spec 24.2).
    let panel = match &app.dock {
        Dock::Composer => layout::composer_height_phase5(app, area.width, area.height, short),
        Dock::Help | Dock::Logs => layout::help_panel_height(area.height),
        _ => layout::panel_height(short),
    };
    let footer_h = layout::footer_height(area.width, area.height);
    let notice_h = u16::from(!app.notices.is_empty());
    let status_h = u16::from(busy);
    let dock_h = status_h + notice_h + panel + footer_h;

    let [transcript_area, dock_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(dock_h)]).areas(area);
    transcript::render(frame, transcript_area, app, &theme);

    let mut rows: Vec<Constraint> = Vec::new();
    if busy {
        rows.push(Constraint::Length(status_h));
    }
    if notice_h == 1 {
        rows.push(Constraint::Length(1));
    }
    rows.push(Constraint::Length(panel));
    rows.push(Constraint::Length(footer_h));
    let chunks = Layout::vertical(rows).split(dock_area);
    let mut index = 0;
    if busy {
        status::render(frame, chunks[index], app, &theme);
        index += 1;
    }
    if notice_h == 1 {
        error::render_notice(frame, chunks[index], &theme, app.notices.back().unwrap());
        index += 1;
    }
    match &app.dock {
        Dock::Composer => composer::render(frame, chunks[index], app, &theme),
        Dock::NewSession(draft) => new_session::render(frame, chunks[index], &theme, draft),
        Dock::SessionSelector(_)
        | Dock::ModelSelector(_)
        | Dock::ReasoningSelector(_)
        | Dock::ProfileSelector(_) => selector::render(frame, chunks[index], app, &theme),
        Dock::Help => help::render(frame, chunks[index], app, &theme),
        Dock::Logs => logs::render(frame, chunks[index], app, &theme),
    }
    index += 1;
    footer::render(frame, chunks[index], app, &theme);
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
                format!("{}x{}", layout::MIN_WIDTH, layout::MIN_HEIGHT),
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
    use crate::theme::ThemeKind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn tiny_app() -> App {
        let mut app = App::new(std::path::PathBuf::from("/project"));
        app.update(crate::event::AppEvent::SetTheme(ThemeKind::Dark));
        app
    }

    #[test]
    fn small_terminal_renders_the_centered_hint() {
        let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();
        let app = tiny_app();
        terminal.draw(|frame| render(frame, &app)).unwrap();
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

    #[test]
    fn fullscreen_background_is_page_color_and_draw_never_panics() {
        for size in [(60, 16), (80, 24), (120, 40)] {
            let app = tiny_app();
            let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let bg = terminal.backend().buffer().cell((0, 0)).unwrap().bg;
            assert_eq!(bg, Theme::dark().page_bg, "page bg at {:?}", size);
        }
    }

    #[test]
    fn dock_is_below_the_transcript_with_a_composer_border() {
        let app = crate::ui::testapp::fresh(ThemeKind::Dark);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let has_rounded_corner = buffer.content().iter().any(|cell| cell.symbol() == "╭");
        assert!(has_rounded_corner, "rounded composer border must be drawn");
        // The empty transcript still shows the startup header.
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("MINICORE"));
        assert!(text.contains("Coding agent TUI"));
    }
}
