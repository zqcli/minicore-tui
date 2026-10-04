//! Rendering: the Pi-style fullscreen conversation layout (development spec
//! 14-20, 29-31). `ui::render` is a pure read-only view: it never mutates
//! `App` or writes caches. The complete conversation snapshot is prepared
//! read-only and installed only through `App::update`; header, notices, layout
//! and the snapshot are consumed without interior mutability.
//!
//! Phase 3 covers the transcript + fixed dock (status/composer/footer), the
//! durable/live blocks, and markdown. Phase 4 replaces the composer in the
//! dock with the new-session form and the session/model/reasoning/profile
//! selectors; Phase 5 adds full input and scrolling; the update-installed
//! conversation snapshot is shared by all transcript consumers.

pub mod assistant;
pub mod changes;
pub mod composer;
pub mod context;
pub mod editor_layout;
pub mod error;
pub mod export;
mod feedback;
pub mod footer;
pub mod header;
pub mod help;
pub mod layout;
pub mod logs;
pub mod new_session;
pub mod panel;
pub mod rail;
pub mod reasoning;
pub mod scrollbar;
pub mod search;
pub mod selector;
pub mod settings;
pub mod status;
pub mod steer_queue;
pub mod tool;
pub mod tool_detail;
pub mod transcript;
pub mod user;
pub mod workspace;

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
mod feedback_tests;
#[cfg(test)]
mod render_cache_tests;
#[cfg(test)]
mod section_gap_tests;
#[cfg(test)]
mod settings_footer_steer_tests;
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
                    .is_some_and(|view| view.needs_result_confirmation()),
            },
        );
        return;
    }
    let screen = layout::screen_layout(app, area);
    let scrollbar = screen.scrollbar_for(screen.transcript);
    if app.context_panel().is_some() {
        context::render(frame, screen.transcript, scrollbar, app, &theme);
    } else if app.changes().is_some() {
        changes::render(frame, screen.transcript, scrollbar, app, &theme);
    } else if app.file_preview().is_some() {
        workspace::render_file(frame, screen.transcript, scrollbar, app, &theme);
    } else if app.tool_detail().is_some() {
        tool_detail::render(frame, screen.transcript, scrollbar, app, &theme);
    } else {
        transcript::render(frame, screen.transcript, scrollbar, app, &theme);
    }

    if let Some(status_area) = screen.status {
        status::render(frame, status_area, app, &theme);
    }
    if let Some(queue_area) = screen.queue {
        steer_queue::render(frame, queue_area, app, &theme);
    }
    if let Some(notice_area) = screen.notice {
        error::render_notice(frame, notice_area, &theme, app.notices.back().unwrap());
    }
    match &app.dock {
        Dock::Workspace(browser) => workspace::render_browser(frame, screen.panel, browser, &theme),
        Dock::Composer => composer::render(frame, screen.panel, app, &theme),
        Dock::NewSession(draft) => new_session::render(frame, screen.panel, &theme, draft),
        Dock::SessionSelector(_)
        | Dock::ModelSelector(_)
        | Dock::ReasoningSelector(_)
        | Dock::ProfileSelector(_) => selector::render(frame, screen.panel, app, &theme),
        Dock::Help => help::render(frame, screen.panel, app, &theme),
        Dock::Logs => logs::render(frame, screen.panel, app, &theme),
        Dock::Search(_) => search::render(frame, screen.panel, app, &theme),
        Dock::Export(form) => export::render(frame, screen.panel, &theme, form),
        Dock::Settings(settings) => settings::render(frame, screen.panel, &theme, settings),
    }
    footer::render(frame, screen.footer, app, &theme);
}

fn render_small_terminal_hint(frame: &mut Frame, area: Rect, theme: &Theme) {
    let [_, hint_area, _] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(4),
        Constraint::Min(0),
    ])
    .areas(area);
    let hint = Paragraph::new(vec![
        Line::from(vec![Span::styled(
            "Terminal too small",
            Style::new().fg(theme.warning),
        )]),
        Line::from(Span::styled(
            format!("Minimum: {}x{}", layout::MIN_WIDTH, layout::MIN_HEIGHT),
            Style::new().fg(theme.warning).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Resize · Ctrl+C: clear / quit",
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
        assert!(text.contains("Ctrl+C: clear / quit"));
        assert!(!text.contains("press q"));
    }

    #[test]
    fn tiny_hint_keeps_required_dimensions_visible_at_thirty_columns() {
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        let app = tiny_app();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Minimum: 60x16"));
        assert!(text.contains("Ctrl+C: clear / quit"));
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
    fn right_gap_stays_blank_and_scrollbar_never_reflows_body_editor_or_footer() {
        use crate::event::AppEvent;
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = Theme::for_kind(kind);
            for (width, height) in [(60, 16), (80, 24), (120, 40)] {
                let mut app = crate::ui::testapp::tools(kind);
                app.update(AppEvent::TerminalSize { width, height });
                app.composer_mut().set_text(&"中文🙂e\u{301} ".repeat(35));
                let screen = layout::screen_layout(&app, Rect::new(0, 0, width, height));
                let prepared = transcript::prepare_conversation(&app, screen.content.width);
                let total = prepared.total_rows();
                app.update(AppEvent::Viewport {
                    total_lines: total,
                    visible_rows: screen.transcript.height as usize,
                });
                app.update(AppEvent::ConversationPrepared(prepared));
                assert!(!app.scrollbar_visible(total, screen.transcript.height as usize));

                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render(frame, &app)).unwrap();
                let hidden = terminal.backend().buffer().clone();
                app.update(AppEvent::Terminal(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: screen.scrollbar.x,
                    row: screen.transcript.y,
                    modifiers: KeyModifiers::NONE,
                })));
                assert!(app.scrollbar_active());
                terminal.draw(|frame| render(frame, &app)).unwrap();
                let visible = terminal.backend().buffer();

                for row in 0..height {
                    for column in screen.content.x..screen.content.right() {
                        assert_eq!(hidden[(column, row)], visible[(column, row)]);
                    }
                    for column in [screen.gutter.x, screen.right_gap.x] {
                        let cell = &visible[(column, row)];
                        assert_eq!(cell.symbol(), " ", "{kind:?} {width}x{height} row {row}");
                        assert_eq!(cell.bg, theme.page_bg);
                    }
                    let track = &visible[(screen.scrollbar.x, row)];
                    assert_eq!(track.bg, theme.page_bg);
                    if row < screen.transcript.bottom() {
                        assert!(matches!(track.symbol(), "│" | "█"));
                    } else {
                        assert_eq!(track.symbol(), " ", "dock must not paint the track");
                    }
                    assert_eq!(hidden[(screen.scrollbar.x, row)].symbol(), " ");
                }
            }
        }
    }

    #[test]
    fn dock_is_below_the_transcript_with_a_rail_composer() {
        let app = crate::ui::testapp::fresh(ThemeKind::Dark);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let has_rounded_corner = buffer.content().iter().any(|cell| cell.symbol() == "╭");
        assert!(
            !has_rounded_corner,
            "Rail composer has no rectangular border"
        );
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "▎"));
        // The empty transcript still shows the startup header.
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("MINICORE"));
        assert!(text.contains("Coding agent TUI"));
    }
}
