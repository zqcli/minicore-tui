//! The startup header, the transcript's first block so it scrolls away
//! naturally (development spec 17). It never uses the Pi logo or name.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, ConnectionState};
use crate::theme::Theme;

pub fn lines(theme: &Theme, app: &App) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(vec![
            Span::styled(
                "MINICORE",
                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  v{}", env!("CARGO_PKG_VERSION")),
                Style::new().fg(theme.dim),
            ),
        ]),
        Line::default(),
        Line::styled("Coding agent TUI", Style::new().fg(theme.muted)),
        Line::styled("q / Ctrl+C to quit", Style::new().fg(theme.dim)),
        Line::default(),
    ];
    let status = match app.connection {
        ConnectionState::Starting => Span::styled("Starting agent…", Style::new().fg(theme.muted)),
        ConnectionState::ShuttingDown => {
            Span::styled("Shutting down…", Style::new().fg(theme.muted))
        }
        ConnectionState::Failed(_) => Span::styled("Disconnected", Style::new().fg(theme.error)),
        ConnectionState::Ready => Span::styled(
            "Open a session — /new, Ctrl+R, or F1 for help",
            Style::new().fg(theme.muted),
        ),
    };
    out.push(Line::from(status));
    out.push(Line::default());
    out
}
