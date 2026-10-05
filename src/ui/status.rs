//! The busy status row (development spec 15.6): a 10-frame spinner plus a
//! Working / compaction phase / Cancelling label, only rendered while busy.

use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::app::App;
use crate::protocol::{
    CancelReasonWire, CompactionPhaseWire, LoopOutcomeWire, TurnPersistenceWire, TurnResultViewWire,
};
use crate::theme::Theme;
use crate::ui::feedback;

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
                if let Some(budget) = &model_error.local_context_budget {
                    format!(
                        "failed: {kind}: {} · estimated {} / input budget {} tokens · try /compact or a model with a larger context window",
                        model_error.kind, budget.estimated_tokens, budget.input_budget_tokens
                    )
                } else {
                    format!("failed: {kind}: {}", model_error.kind)
                }
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
    feedback::render_row(
        frame,
        area,
        Some(Span::styled(
            format!("{} ", SPINNER[frame_index]),
            Style::new().fg(theme.accent),
        )),
        &busy_label(app),
        feedback::neutral_style(theme),
    );
}

/// English display names shared by the primary status and context details.
/// Only observed wire phases use these names; admission has no guessed phase.
pub(crate) fn compaction_phase_label(phase: CompactionPhaseWire) -> &'static str {
    match phase {
        CompactionPhaseWire::Preparing => "Preparing",
        CompactionPhaseWire::Summarizing => "Summarizing",
        CompactionPhaseWire::Merging => "Merging",
        CompactionPhaseWire::Committing => "Committing",
    }
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
    // Recovery is a latest-read Context snapshot, not a live phase: its
    // completion has no independent notification to clear a cached label.
    if let Some(operation) = view
        .context
        .as_ref()
        .and_then(|context| context.current_operation.as_ref())
        .or_else(|| {
            view.state
                .as_ref()
                .and_then(|state| state.compaction.as_ref())
        })
    {
        let cancelling = app.compaction_cancelling(&view.info.session_id, &operation.operation_id)
            || view.manual_compact.as_ref().is_some_and(|compact| {
                compact.operation_id == operation.operation_id && compact.cancel_requested
            });
        let label = if cancelling {
            "Cancelling compaction".to_owned()
        } else {
            format!("Compacting · {}", compaction_phase_label(operation.phase))
        };
        return if view.live.as_ref().is_none_or(|live| live.waiting) && view.can_show_last_result()
        {
            view.last_result.as_ref().map_or_else(
                || label.to_owned(),
                |result| format!("{label} · Last turn: {}", result_summary(result)),
            )
        } else {
            label.to_owned()
        };
    }
    if view
        .manual_compact
        .as_ref()
        .is_some_and(|compact| compact.result.is_none())
    {
        return if view
            .manual_compact
            .as_ref()
            .is_some_and(|compact| compact.cancel_requested)
        {
            "Cancelling compaction"
        } else {
            "Compacting"
        }
        .to_owned();
    }
    if view.is_preparing() {
        return if view
            .context
            .as_ref()
            .is_some_and(|context| context.automatic.current.is_some())
            || view
                .manual_compact
                .as_ref()
                .and_then(|compact| compact.result.as_ref())
                .is_some_and(|result| {
                    result.status == crate::protocol::CompactStatusWire::UnknownWrite
                })
        {
            "Preparing"
        } else {
            "Working"
        }
        .to_owned();
    }
    if let Some(state) = view.state.as_ref() {
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
    "Working".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LoopStatusWire, SessionStatusWire};
    use crate::theme::ThemeKind;

    #[test]
    fn observed_phases_use_context_first_then_state_and_keep_cancel_priority() {
        for (phase, label) in [
            (CompactionPhaseWire::Preparing, "Preparing"),
            (CompactionPhaseWire::Summarizing, "Summarizing"),
            (CompactionPhaseWire::Merging, "Merging"),
            (CompactionPhaseWire::Committing, "Committing"),
        ] {
            let mut app = crate::ui::feedback_tests::compacting(ThemeKind::Dark, phase);
            assert_eq!(busy_label(&app), format!("Compacting · {label}"));
            let view = app.sessions.known.get_mut("ses_1").unwrap();
            view.context = Some(
                serde_json::from_value(serde_json::json!({
                    "session_id": "ses_1",
                    "current_operation": {"operation_id": "observed", "phase": phase,
                        "covered_item_count": 2, "retained_item_count": 0},
                    "coverage": {"covered_loop_count": 1, "covered_item_count": 2,
                        "retained_item_count": 0},
                    "budget": {}, "automatic": {"current": null, "last": null}
                }))
                .unwrap(),
            );
            view.state
                .as_mut()
                .unwrap()
                .compaction
                .as_mut()
                .unwrap()
                .phase = CompactionPhaseWire::Preparing;
            assert_eq!(busy_label(&app), format!("Compacting · {label}"));
            app.open_context();
            assert!(
                crate::ui::context::rows(&app)
                    .iter()
                    .any(|row| row.contains(&format!("Current compaction: observed {label}")))
            );
            app.sessions.known.get_mut("ses_1").unwrap().manual_compact =
                Some(crate::state::session::ManualCompactState {
                    operation_id: "observed".into(),
                    cancel_requested: true,
                    result: None,
                    state_refresh_confirmed: false,
                    context_refresh_confirmed: false,
                });
            assert_eq!(busy_label(&app), "Cancelling compaction");
        }
        let mut app = crate::ui::testapp::live_turn(ThemeKind::Dark);
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.state.as_mut().unwrap().compaction = Some(crate::protocol::CompactionProgressWire {
            operation_id: "observed".into(),
            phase: CompactionPhaseWire::Committing,
            covered_item_count: 2,
            retained_item_count: 0,
        });
        view.live.as_mut().unwrap().cancel_requested = true;
        assert_eq!(busy_label(&app), "Cancelling");
    }

    #[test]
    fn unobserved_manual_operation_does_not_guess_a_phase() {
        let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
        app.sessions.known.get_mut("ses_1").unwrap().manual_compact =
            Some(crate::state::session::ManualCompactState {
                operation_id: "pending".into(),
                cancel_requested: false,
                result: None,
                state_refresh_confirmed: false,
                context_refresh_confirmed: false,
            });
        assert_eq!(busy_label(&app), "Compacting");
        app.sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .manual_compact
            .as_mut()
            .unwrap()
            .cancel_requested = true;
        assert_eq!(busy_label(&app), "Cancelling compaction");
        assert_eq!(
            busy_label(&crate::ui::testapp::fresh(ThemeKind::Dark)),
            "Working"
        );
    }

    #[test]
    fn cached_recovery_does_not_override_live_work_or_compaction() {
        let mut app = crate::ui::testapp::live_turn(ThemeKind::Dark);
        let value = serde_json::json!({
            "session_id": "ses_1", "coverage": {"covered_loop_count": 0,
                "covered_item_count": 0, "retained_item_count": 0},
            "budget": {}, "automatic": {"current": null, "last": null},
            "recovery": {"loop_id": "loop_live", "request_index": 0, "outcome": "recovering"}
        });
        app.sessions.known.get_mut("ses_1").unwrap().context =
            Some(serde_json::from_value(value).unwrap());
        assert_eq!(busy_label(&app), "Working");
        assert!(app.active_view().unwrap().live.is_some());
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        for request in &mut view.live.as_mut().unwrap().requests {
            request.tools.clear();
        }
        assert_eq!(busy_label(&app), "Working");
        // The same loop/request can keep running after the sampled recovery
        // ends without another recovery notification. Redraws cannot turn
        // that cached snapshot into a live phase.
        app.frame_count += 1;
        assert_eq!(busy_label(&app), "Working");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.context.as_mut().unwrap().current_operation =
            Some(crate::protocol::CompactionProgressWire {
                operation_id: "recover-compact".into(),
                phase: CompactionPhaseWire::Summarizing,
                covered_item_count: 2,
                retained_item_count: 0,
            });
        assert_eq!(busy_label(&app), "Compacting · Summarizing");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live = None;
        view.last_result = Some(
            serde_json::from_value(serde_json::json!({
                "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                "outcome": {"type": "completed"}, "persistence": "persisted"
            }))
            .unwrap(),
        );
        view.context.as_mut().unwrap().current_operation =
            Some(crate::protocol::CompactionProgressWire {
                operation_id: "auto-loop_live".into(),
                phase: crate::protocol::CompactionPhaseWire::Summarizing,
                covered_item_count: 2,
                retained_item_count: 0,
            });
        assert_eq!(
            busy_label(&app),
            "Compacting · Summarizing · Last turn: completed · persisted"
        );
    }

    #[test]
    fn request_preparation_and_model_wait_keep_working_and_remain_cancellable() {
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
        assert_eq!(busy_label(&app), "Working");

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
        assert_eq!(busy_label(&app), "Working");
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

    #[test]
    fn ordinary_admission_and_tool_progress_keep_working_while_spinner_advances() {
        let mut app = crate::ui::testapp::live_turn(ThemeKind::Dark);
        assert_eq!(busy_label(&app), "Working");
        let first = crate::ui::component_tests::draw(&app, 80, 24);
        app.frame_count += 1;
        assert_eq!(busy_label(&app), "Working");
        let second = crate::ui::component_tests::draw(&app, 80, 24);
        let screen =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let status = screen.status.unwrap();
        assert_ne!(
            first.backend().buffer()[(status.x, status.y)].symbol(),
            second.backend().buffer()[(status.x, status.y)].symbol()
        );
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live.as_mut().unwrap().reference = None;
        assert!(view.is_preparing());
        assert_eq!(busy_label(&app), "Working");
    }

    #[test]
    fn automatic_preparation_and_unknown_manual_write_keep_their_status() {
        let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.context = Some(serde_json::from_value(serde_json::json!({
            "session_id":"ses_1", "coverage":{"covered_loop_count":0,"covered_item_count":0,"retained_item_count":0},
            "budget":{}, "automatic":{"current":{"operation_id":"auto"},"last":null}
        })).unwrap());
        assert_eq!(busy_label(&app), "Preparing");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.context = None;
        view.manual_compact = Some(crate::state::session::ManualCompactState {
            operation_id: "manual".into(),
            cancel_requested: false,
            result: Some(
                serde_json::from_value(
                    serde_json::json!({"operation_id":"manual", "status":"unknown_write"}),
                )
                .unwrap(),
            ),
            state_refresh_confirmed: false,
            context_refresh_confirmed: false,
        });
        assert_eq!(busy_label(&app), "Preparing");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.manual_compact = None;
        view.state.as_mut().unwrap().status = SessionStatusWire::WaitingForInput;
        assert_eq!(busy_label(&app), "Waiting for input");
        app.sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .state
            .as_mut()
            .unwrap()
            .status = SessionStatusWire::Blocked;
        assert!(busy_label(&app).starts_with("Blocked"));
    }
}
