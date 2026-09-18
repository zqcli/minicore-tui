//! The startup header, the transcript's first block so it scrolls away
//! naturally (development spec 17). It never uses the Pi logo or name.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, ConnectionState};
use crate::protocol::SessionStatusWire;
use crate::theme::Theme;

/// Whether the startup guidance belongs in the current transcript.
///
/// An active session may show it only for a genuinely confirmed empty idle
/// view. History being empty by itself is not sufficient: state, event-gap,
/// reconciliation, and persistence uncertainty remain authoritative fences.
pub fn visible(app: &App) -> bool {
    if app.sessions.active.is_none() || app.new_session().is_some() {
        return true;
    }
    app.active_view().is_some_and(|view| {
        view.state.as_ref().is_some_and(|state| {
            state.status == SessionStatusWire::Idle
                && state.active_loop.is_none()
                && state.block_reason.is_none()
        }) && view.transcript.complete
            && !view.loading
            && view.transcript.window.is_empty()
            && view.transcript.blocks.is_empty()
            && view.live.is_none()
            && !view.event_gap
            && !view.reconcile_inflight
            && !view.needs_post_wait_history
            && view.unsaved_loop.is_none()
            && !view.result_unconfirmed
            && !view.closing
            && !view.close_verification_unknown
            && view.latest_state_query.is_none()
            && view.last_result.is_none()
            && view.completed_steers.is_empty()
            && view.applied_steers.is_empty()
            && view.steer_queue.is_empty()
            && view.live_request_usage.is_empty()
    })
}

pub fn lines(theme: &Theme, app: &App) -> Vec<Line<'static>> {
    if !visible(app) {
        return Vec::new();
    }
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
