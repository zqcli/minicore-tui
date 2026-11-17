//! Fixed-height recent transcript windows inside the original native tool rail.

use minicore_host_core::InteractionRequestData;
use minicore_host_protocol::{Scheduling, TurnOutcome, TurnPhase, TurnState};
use ratatui::text::{Line, Span};

use super::{
    blocks, markdown,
    rails::{chrome, chrome_span, surface_row},
    theme::Styles,
};
use crate::transcript::agent_card::AgentCard;
use crate::transcript::{Block, ToolCall, ToolState};
use crate::wrap;

pub const COLLAPSED_ROWS: usize = 6;
pub const EXPANDED_ROWS: usize = 18;

pub fn render(
    call: &ToolCall,
    styles: Styles,
    width: usize,
    expanded: bool,
    reasoning: bool,
) -> Vec<Line<'static>> {
    let Some(card) = call.agent_card.as_deref() else {
        return receipt(call, styles, width, expanded);
    };
    let (rail, surface) = match card.outcome() {
        Some(TurnOutcome::Completed) => (styles.card_success_rail(), styles.card_success()),
        Some(_) => (styles.card_failure_rail(), styles.card_failure()),
        None => (styles.card_rail(), styles.card()),
    };
    let row = |line: Line<'static>| {
        surface_row(
            rail,
            surface,
            std::iter::once(Span::raw(" ")).chain(line.spans).collect(),
            width,
        )
    };
    let inner = blocks::text_width(width);
    let label = card
        .agent_name
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| short(card.key.child.agent_id.as_str()));
    let label = wrap::truncate(&wrap::sanitize(&label), 20);
    let state = status(card);
    let state = if inner < 18 && state == "approval required" {
        "approval".into()
    } else {
        state
    };
    let state = wrap::truncate(&state, inner);
    let identity = format!(
        "Agent {label} · Session {}",
        short(card.key.child.session_id.as_str())
    );
    let identity = wrap::truncate(&identity, inner.saturating_sub(wrap::width(&state) + 1));
    let padding = inner.saturating_sub(wrap::width(&identity) + wrap::width(&state));
    let header = format!("{identity}{}{state}", " ".repeat(padding));
    let task = format!(
        "Task {} · {}",
        short(card.key.task_message_id.as_str()),
        if card.task.is_empty() {
            "input accepted"
        } else {
            card.task.lines().next().unwrap_or("")
        }
    );
    let mut lines = vec![
        chrome(row(Line::styled(
            wrap::truncate(&wrap::sanitize(&header), inner),
            styles.tool_title(),
        ))),
        row(Line::from(vec![
            chrome_span(Span::styled(
                format!("Task {} · ", short(card.key.task_message_id.as_str())),
                styles.meta(),
            )),
            Span::styled(
                wrap::truncate(
                    &wrap::sanitize(if card.task.is_empty() {
                        "input accepted"
                    } else {
                        card.task.lines().next().unwrap_or("")
                    }),
                    inner.saturating_sub(14),
                ),
                styles.tool_output(),
            ),
        ])),
    ];
    // At very narrow widths clip the complete identity row as a unit.
    if wrap::width(&lines[1].to_string()) > width {
        lines[1] = chrome(row(Line::styled(
            wrap::truncate(&wrap::sanitize(&task), inner),
            styles.meta(),
        )));
    }
    let budget = if expanded {
        EXPANDED_ROWS
    } else {
        COLLAPSED_ROWS
    };
    let mut body = preview(card, inner, budget, reasoning, styles);
    for line in body.drain(..) {
        lines.push(row(line));
    }
    while lines.len() < budget + 2 {
        lines.push(chrome(row(Line::default())));
    }
    let footer = if card.history_unavailable {
        "Child history unavailable · retained preview"
    } else if card.observation_error.is_some() {
        "Observation incomplete · Open child Session"
    } else if expanded {
        "Open child Session · click header to collapse"
    } else {
        "Open child Session · click header to expand"
    };
    lines.push(chrome(row(Line::styled(
        wrap::truncate(footer, inner),
        styles.meta(),
    ))));
    lines
}

pub fn receipt(
    call: &ToolCall,
    styles: Styles,
    width: usize,
    expanded: bool,
) -> Vec<Line<'static>> {
    let inner = blocks::text_width(width);
    let row = |text: String| {
        chrome(surface_row(
            styles.card_rail(),
            styles.card(),
            vec![
                Span::raw(" "),
                Span::styled(wrap::truncate(&wrap::sanitize(&text), inner), styles.meta()),
            ],
            width,
        ))
    };
    let mut lines = if expanded
        || matches!(
            call.state,
            ToolState::Failed | ToolState::Denied | ToolState::Cancelled
        ) {
        blocks::tool(call, styles, width, expanded)
    } else {
        vec![row(format!("{} · input accepted", call.name))]
    };
    if let Some(association) = call.delegations.first() {
        if !expanded && call.state == ToolState::Succeeded {
            lines.push(row(format!(
                "Session {} · Task {}",
                short(association.child.session_id.as_str()),
                short(association.task_message_id.as_str())
            )));
        }
        lines.push(row("Locate original Agent card".into()));
    }
    lines
}

pub fn footer_offset(expanded: bool) -> usize {
    2 + if expanded {
        EXPANDED_ROWS
    } else {
        COLLAPSED_ROWS
    }
}

fn short(id: &str) -> String {
    id.chars().take(6).collect()
}

fn status(card: &AgentCard) -> String {
    if let Some(outcome) = card.outcome() {
        return match outcome {
            TurnOutcome::Completed => "completed",
            TurnOutcome::Failed => "failed",
            TurnOutcome::Cancelled => "cancelled",
        }
        .into();
    }
    if card.history_unavailable {
        return "history unavailable".into();
    }
    if card.historical {
        return "historical · completion unavailable".into();
    }
    if card.state.is_none() {
        return if card.observation_error.is_some() {
            "status unavailable"
        } else {
            "accepted · observing child"
        }
        .into();
    }
    if let Some(state) = &card.state {
        if let Some(pending) = state.data.pending_interactions.first() {
            return match pending.request {
                InteractionRequestData::Approval { .. } => "approval required",
                InteractionRequestData::ToolInput { .. } => "input required",
            }
            .into();
        }
        if state.data.scheduling == Scheduling::Paused {
            return "paused".into();
        }
        if state.data.preparing {
            return "preparing".into();
        }
        if state.data.block_reason.is_some() {
            return "blocked · open child Session".into();
        }
    }
    if let Some((_, turn)) = &card.turn {
        if turn.model_retry.is_some() {
            return "model retry".into();
        }
        if turn.state == TurnState::Cancelling {
            return "cancelling".into();
        }
        if turn.state != TurnState::Finished {
            return match turn.phase {
                Some(TurnPhase::WaitingForInput) => "waiting for input",
                Some(TurnPhase::Tools) => "running tools",
                Some(TurnPhase::Finalizing) => "finalizing",
                _ => "working",
            }
            .into();
        }
    }
    if card
        .state
        .as_ref()
        .is_some_and(|state| state.data.active_turn_id.is_some())
    {
        "working"
    } else {
        "waiting for child"
    }
    .into()
}

fn preview(
    card: &AgentCard,
    width: usize,
    budget: usize,
    reasoning: bool,
    styles: Styles,
) -> Vec<Line<'static>> {
    let mut groups = Vec::new();
    let mut used = 0;
    if let Some(fault) = card.fault() {
        let body = markdown::render(
            &blocks::reasoning_source(crate::transcript::agent_card::tail(
                fault,
                crate::transcript::agent_card::PREVIEW_BYTES,
            )),
            width,
            &styles,
            styles.failure(),
        );
        let error_budget = if card.transcript.last_answer().is_some() {
            (budget / 2).max(2)
        } else {
            budget
        };
        push_group(&mut groups, &mut used, error_budget, "Error", body, styles);
    } else if card.transcript.is_empty()
        && let Some(error) = &card.observation_error
    {
        let body = wrap::wrap_line(&wrap::sanitize(error), width)
            .into_iter()
            .map(|text| Line::styled(text, styles.warning()))
            .collect();
        push_group(
            &mut groups,
            &mut used,
            budget,
            "Observation unavailable",
            body,
            styles,
        );
    }
    if card.outcome() == Some(TurnOutcome::Completed)
        && let Some(answer) = card.transcript.last_answer()
    {
        let body = markdown::render(
            &blocks::reasoning_source(answer),
            width,
            &styles,
            styles.text(),
        );
        push_group(&mut groups, &mut used, budget, "Final answer", body, styles);
    } else {
        for block in card.transcript.blocks().iter().rev() {
            if used >= budget {
                break;
            }
            match block {
                Block::Assistant {
                    reasoning: thinking,
                    text,
                    partial,
                    ..
                } => {
                    if !text.is_empty() {
                        let body = markdown::render(
                            &blocks::reasoning_source(text),
                            width,
                            &styles,
                            styles.text(),
                        );
                        push_group(
                            &mut groups,
                            &mut used,
                            budget,
                            if *partial {
                                "Partial response"
                            } else {
                                "Response"
                            },
                            body,
                            styles,
                        );
                    }
                    if reasoning && !thinking.is_empty() && used < budget {
                        let body = markdown::render(
                            &blocks::reasoning_source(thinking),
                            width,
                            &styles,
                            blocks::thinking_style(styles),
                        );
                        push_group(&mut groups, &mut used, budget, "Thinking", body, styles);
                    }
                }
                Block::Tool(call) => {
                    let outcome = match call.state {
                        ToolState::Pending => "pending",
                        ToolState::Running => "running",
                        ToolState::Succeeded => "completed",
                        ToolState::Failed => "failed",
                        ToolState::Denied => "denied",
                        ToolState::Cancelled => "cancelled",
                    };
                    let mut label = format!("{} · {outcome}", call.name);
                    if !call.detail.is_empty() {
                        label.push_str(" · ");
                        label.push_str(&call.detail);
                    }
                    let body = if let Some(nested) = call.delegations.first() {
                        vec![Line::styled(
                            wrap::truncate(
                                &format!(
                                    "Child Session {} · open from this child",
                                    nested.child.session_id
                                ),
                                width,
                            ),
                            styles.meta(),
                        )]
                    } else {
                        call.output_content()
                            .into_iter()
                            .flat_map(|line| wrap::wrap_line(&wrap::sanitize(line), width))
                            .map(|line| Line::styled(line, styles.tool_output()))
                            .collect()
                    };
                    push_group(&mut groups, &mut used, budget, &label, body, styles);
                }
                _ => {}
            }
        }
    }
    groups.into_iter().rev().flatten().collect()
}

fn push_group(
    groups: &mut Vec<Vec<Line<'static>>>,
    used: &mut usize,
    budget: usize,
    label: &str,
    body: Vec<Line<'static>>,
    styles: Styles,
) {
    let remaining = budget.saturating_sub(*used);
    if remaining == 0 {
        return;
    }
    let take = body.len().min(remaining.saturating_sub(1));
    let skipped = body.len() - take;
    let mut group = vec![Line::from(chrome_span(Span::styled(
        wrap::sanitize(&format!("[{label}]")),
        styles.meta(),
    )))];
    group.extend(body.into_iter().skip(skipped));
    *used += group.len();
    groups.push(group);
}
