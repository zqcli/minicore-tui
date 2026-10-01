//! Conversation search panel (spec §17.1): a one-line query input, the
//! explicit scope/coverage line, and bounded match summaries. The transcript
//! above the panel stays visible.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::App;
use crate::state::search::{SearchPanelMode, SearchPanelState};
use crate::theme::Theme;
use crate::ui::layout::{fill_line, truncate};

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(panel) = app.search_panel() else {
        return;
    };
    frame.render_widget(Clear, area);
    let border = Style::new().fg(theme.border);
    let block = Block::bordered()
        .title(panel_title(panel))
        .border_style(border)
        .style(Style::new().bg(theme.page_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    render_query(frame, rows[0], panel, app, theme);
    render_status(frame, rows[1], panel, theme);
    render_matches(frame, rows[2], panel, app, theme);
    render_hints(frame, rows[3], panel, theme);
}

fn panel_title(panel: &SearchPanelState) -> String {
    format!(
        " Search — {} · {} ",
        panel.scope.label(),
        match panel.mode {
            SearchPanelMode::Input if panel.has_query() => "edit literal",
            SearchPanelMode::Input => "type a literal",
            SearchPanelMode::Results => "results",
        }
    )
}

fn render_query(frame: &mut Frame, area: Rect, panel: &SearchPanelState, app: &App, theme: &Theme) {
    let active = panel.mode == SearchPanelMode::Input;
    let prompt = Span::styled(
        " search> ",
        Style::new()
            .fg(if active { theme.accent } else { theme.muted })
            .add_modifier(if active {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
    );
    let query_text = crate::safe_text::safe_display(&panel.query).into_owned();
    let query = if query_text.is_empty() && active {
        Span::styled(
            "type a literal to search".to_owned(),
            Style::new().fg(theme.muted),
        )
    } else {
        Span::styled(query_text, Style::new().fg(theme.text))
    };
    let line = fill_line(
        Line::from(vec![prompt, query]),
        area.width as usize,
        Style::new().bg(theme.page_bg),
    );
    frame.render_widget(Paragraph::new(line), area);
    let _ = app;
}

fn render_status(frame: &mut Frame, area: Rect, panel: &SearchPanelState, theme: &Theme) {
    let status = truncate(
        &crate::safe_text::safe_display(&panel.status_label()),
        area.width as usize,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            status,
            Style::new().fg(theme.muted),
        ))),
        area,
    );
}

fn render_matches(
    frame: &mut Frame,
    area: Rect,
    panel: &SearchPanelState,
    app: &App,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    let visible = area.height as usize;
    let first = match panel.mode {
        SearchPanelMode::Results | SearchPanelMode::Input => panel
            .cursor
            .saturating_sub(visible.saturating_sub(1) / 2)
            .min(panel.matches.len().saturating_sub(visible)),
    };
    let mut lines = Vec::with_capacity(visible);
    for (offset, target) in panel.matches.iter().skip(first).take(visible).enumerate() {
        let index = first + offset;
        let selected = index == panel.cursor && panel.mode == SearchPanelMode::Results;
        let marker = if selected { "▶ " } else { "  " };
        let location = match target.index {
            Some(index) => format!("#{index}"),
            None => "live".to_owned(),
        };
        let preview = truncate(
            &crate::safe_text::safe_display(&target.preview.replace('\n', " ")),
            (area.width as usize).saturating_sub(location.len() + target.source.label().len() + 6),
        );
        let style = if selected {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_owned(), style),
            Span::styled(
                format!("{} ", target.source.label()),
                Style::new().fg(theme.muted),
            ),
            Span::styled(format!("{location} "), Style::new().fg(theme.muted)),
            Span::styled(preview, style),
        ]));
    }
    if lines.is_empty() && !panel.has_query() {
        lines.push(Line::from(Span::styled(
            "Search keeps the conversation visible; the default scope is the loaded content.",
            Style::new().fg(theme.muted),
        )));
    }
    let _ = app;
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_hints(frame: &mut Frame, area: Rect, panel: &SearchPanelState, theme: &Theme) {
    let hints = if panel.mode == SearchPanelMode::Input {
        "Enter search · Ctrl+A scope · Esc close · Ctrl+U clear"
    } else if panel.scanning() {
        if area.width < 90 {
            "s stop · Enter jump · n/p · Ctrl+A scope · Esc close"
        } else {
            "Enter jump · n/p next/prev · s stop · Ctrl+A scope · type to edit · Esc edit/close"
        }
    } else if area.width < 90 {
        "Enter jump · n/p move · Ctrl+A scope · Esc edit/close"
    } else {
        "Enter jump · n/p next/prev · Ctrl+A scope · type to edit · Ctrl+U clear · Esc edit/close"
    };
    let hints = truncate(hints, area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hints,
            Style::new().fg(theme.muted),
        ))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::search::{SearchScope, SearchStatus};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn no_match_input_discloses_scope_switch_without_discarding_query() {
        let mut panel = SearchPanelState::new(
            "session".into(),
            1,
            "missing literal".into(),
            SearchScope::Loaded,
        );
        panel.mode = SearchPanelMode::Input;
        panel.status = SearchStatus::Ready;
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| render_hints(frame, frame.area(), &panel, &Theme::dark()))
            .unwrap();
        let text: String = (0..60)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        assert!(text.contains("Ctrl+A scope"));
        assert!(text.contains("Enter search"));
        assert!(text.contains("Esc close"));
        assert_eq!(panel.query, "missing literal");
        assert_eq!(panel.scope, SearchScope::Loaded);
    }

    #[test]
    fn narrow_search_hints_keep_navigation_scope_and_escape_visible() {
        for status in [SearchStatus::Ready, SearchStatus::ScanningFull] {
            let mut panel = SearchPanelState::new(
                "session".into(),
                1,
                "literal".into(),
                SearchScope::FullSession,
            );
            panel.mode = SearchPanelMode::Results;
            panel.status = status;
            let mut terminal = Terminal::new(TestBackend::new(57, 1)).unwrap();
            terminal
                .draw(|frame| render_hints(frame, frame.area(), &panel, &Theme::dark()))
                .unwrap();
            let text: String = (0..57)
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect();
            assert!(text.contains("Enter jump"));
            assert!(text.contains("Ctrl+A scope"));
            assert!(text.contains("Esc"));
            if panel.scanning() {
                assert!(text.contains("s stop"));
            }
        }
    }

    #[test]
    fn search_title_distinguishes_editing_from_results() {
        let mut panel =
            SearchPanelState::new("session".into(), 1, "missing".into(), SearchScope::Loaded);
        panel.mode = SearchPanelMode::Input;
        assert!(panel_title(&panel).contains("edit literal"));
        panel.mode = SearchPanelMode::Results;
        assert!(panel_title(&panel).contains("results"));
    }
}
