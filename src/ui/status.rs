//! The busy status row (development spec 15.6): a 10-frame spinner plus a
//! Working / `Running <tool>` / Cancelling label, only rendered while busy.

use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::protocol::{CancelReasonWire, LoopOutcomeWire, TurnPersistenceWire, TurnResultViewWire};
use crate::state::tool::ToolStatus;
use crate::theme::Theme;

/// Spinner frames advance with `App.frame_count` via `AppEvent::Tick`.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A compact description of the last Agent-confirmed turn result. The
/// persistence suffix is deliberately separate from the execution outcome.
pub(crate) fn result_summary(result: &TurnResultViewWire) -> String {
    let outcome = match &result.outcome {
        LoopOutcomeWire::Completed => "completed".to_owned(),
        LoopOutcomeWire::Cancelled { reason } => {
            let reason = match reason {
                CancelReasonWire::User => "user".to_owned(),
                CancelReasonWire::OwnerDropped => "owner dropped".to_owned(),
                CancelReasonWire::Shutdown => "shutdown".to_owned(),
                CancelReasonWire::Deadline => "deadline".to_owned(),
                CancelReasonWire::Unknown(reason) => reason.clone(),
            };
            format!("cancelled ({reason})")
        }
        LoopOutcomeWire::Failed { kind, model_error } => {
            if let Some(model_error) = model_error {
                format!("failed: {kind}: {}", model_error.kind)
            } else {
                format!("failed: {kind}")
            }
        }
    };
    let persistence = match result.persistence {
        Some(TurnPersistenceWire::Persisted) => "persisted",
        Some(TurnPersistenceWire::Failed) => "persistence failed",
        None => "persistence unknown",
    };
    format!("{outcome} · {persistence}")
}

pub(crate) fn result_color(result: &TurnResultViewWire, theme: &Theme) -> ratatui::style::Color {
    match &result.outcome {
        LoopOutcomeWire::Failed { .. } => theme.error,
        LoopOutcomeWire::Cancelled { .. } => theme.warning,
        LoopOutcomeWire::Completed => match result.persistence {
            Some(TurnPersistenceWire::Persisted) => theme.success,
            Some(TurnPersistenceWire::Failed) => theme.error,
            None => theme.warning,
        },
    }
}

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App, theme: &Theme) {
    let frame_index = (app.frame_count % SPINNER.len() as u64) as usize;
    let line = Line::from(vec![
        Span::styled(SPINNER[frame_index], Style::new().fg(theme.accent)),
        Span::styled(
            format!(" {}", busy_label(app)),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn busy_label(app: &App) -> String {
    let Some(view) = app.active_view() else {
        return "Working".to_owned();
    };
    if view
        .live
        .as_ref()
        .is_some_and(|live| live.cancel_requested && !live.waiting)
    {
        return "Cancelling".to_owned();
    }
    if view.is_preparing() {
        return "Preparing".to_owned();
    }
    if let Some(state) = view.state.as_ref() {
        if let Some(active) = state.active_loop.as_ref().filter(|active| {
            active.status == crate::protocol::LoopStatusWire::Starting
                && state.status == crate::protocol::SessionStatusWire::Running
        }) {
            return format!(
                "Preparing · request {}",
                active.request_index.saturating_add(1)
            );
        }
        match state.status {
            crate::protocol::SessionStatusWire::WaitingForInput => {
                return "Waiting for input".to_owned();
            }
            crate::protocol::SessionStatusWire::Finishing => {
                if view.can_show_last_result() {
                    if let Some(result) = view.last_result.as_ref() {
                        return result_summary(result);
                    }
                }
                return if view.live.as_ref().is_some_and(|live| live.waiting) {
                    "Result unconfirmed".to_owned()
                } else {
                    "Saving".to_owned()
                };
            }
            crate::protocol::SessionStatusWire::Blocked => {
                let reason = match state.block_reason {
                    Some(crate::protocol::SessionBlockReasonWire::Persistence) => "persistence",
                    Some(crate::protocol::SessionBlockReasonWire::Internal) => "internal",
                    None => "unknown",
                };
                return if view.can_show_last_result() {
                    if let Some(result) = view.last_result.as_ref() {
                        format!("Blocked · {reason} · {}", result_summary(result))
                    } else {
                        format!("Blocked · {reason}")
                    }
                } else {
                    format!("Blocked · {reason}")
                };
            }
            crate::protocol::SessionStatusWire::Idle
            | crate::protocol::SessionStatusWire::Running => {}
        }
    }
    let Some(live) = &view.live else {
        return if view.can_show_last_result() {
            view.last_result
                .as_ref()
                .map_or_else(|| "Working".to_owned(), result_summary)
        } else {
            "Working".to_owned()
        };
    };
    if live.waiting {
        return if view.can_show_last_result() {
            live.last_result
                .as_ref()
                .map_or_else(|| "Result unconfirmed".to_owned(), result_summary)
        } else {
            "Result unconfirmed".to_owned()
        };
    }
    if live.cancel_requested {
        return "Cancelling".to_owned();
    }
    if let Some(tool) = live
        .requests
        .iter()
        .flat_map(|request| request.tools.iter())
        .find(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        return format!("Running {}…", tool.name);
    }
    if let Some(request) = live
        .requests
        .last()
        .filter(|request| request.parts.is_empty() && request.tools.is_empty())
    {
        return format!(
            "Waiting for model · request {}",
            request.request_index.saturating_add(1)
        );
    }
    "Working".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LoopStatusWire, SessionStatusWire};
    use crate::theme::ThemeKind;

    #[test]
    fn request_preparation_and_model_wait_are_distinct_and_cancellable() {
        let mut app = crate::ui::testapp::live_turn(ThemeKind::Dark);
        crate::ui::testapp::set_session_running(&mut app, "ses_1", "loop_live");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        let state = view.state.as_mut().unwrap();
        state.status = SessionStatusWire::Running;
        state.compaction = None;
        let active = state.active_loop.as_mut().unwrap();
        active.status = LoopStatusWire::Starting;
        active.request_index = 7;
        view.context = None;
        assert_eq!(busy_label(&app), "Preparing · request 8");

        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live.as_mut().unwrap().cancel_requested = true;
        assert_eq!(busy_label(&app), "Cancelling");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live.as_mut().unwrap().cancel_requested = false;
        view.state
            .as_mut()
            .unwrap()
            .active_loop
            .as_mut()
            .unwrap()
            .status = LoopStatusWire::RunningModel;
        let live = view.live.as_mut().unwrap();
        live.requests.clear();
        live.requests.push(crate::state::turn::LiveRequest::new(
            7,
            0,
            "model".into(),
            crate::protocol::Reasoning::High,
        ));
        assert_eq!(busy_label(&app), "Waiting for model · request 8");
        let live = app
            .sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .live
            .as_mut()
            .unwrap();
        live.cancel_requested = true;
        live.waiting = true;
        assert_eq!(busy_label(&app), "Result unconfirmed");
    }
}
