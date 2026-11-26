//! Fixed-height recent transcript windows inside the original native tool rail.

use minicore_host_core::InteractionRequestData;
use minicore_host_protocol::{Scheduling, TurnOutcome, TurnPhase, TurnState};
use ratatui::text::{Line, Span};

use super::{
    blocks, markdown,
    rails::{chrome, chrome_span, indent, surface_row},
    theme::Styles,
};
use crate::transcript::agent_card::AgentCard;
use crate::transcript::{Block, ToolCall, ToolState};
use crate::wrap;

pub fn render(
    call: &ToolCall,
    styles: Styles,
    width: usize,
    expanded: bool,
    _reasoning: bool,
) -> Vec<Line<'static>> {
    let Some(card) = call.agent_card.as_deref() else {
        return receipt(call, styles, width, expanded);
    };
    if expanded {
        dispatch_card_rows(call, card, styles, width)
    } else {
        vec![dispatch_row(call, card, styles, width)]
    }
}

/// How far an expanded dispatch card sits in under its kept header row, aligning
/// its left rail with `agent`.
const CARD_INDENT: usize = blocks::INSET;

/// One immutable sync dispatch row in the transcript: reflects the dispatch RPC
/// itself rather than mutating in place across later `Working` groups.
fn dispatch_row(call: &ToolCall, card: &AgentCard, styles: Styles, width: usize) -> Line<'static> {
    let (glyph, glyph_style) = blocks::tool_glyph(call, styles);
    let label = card
        .agent_name
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| short(card.key.child.agent_id.as_str()));
    let mut parts = vec![
        wrap::truncate(&wrap::sanitize(&label), 18),
        short(card.key.child.session_id.as_str()),
    ];
    if let Some(duration) = call.duration {
        parts.push(blocks::format_duration(duration));
    } else if matches!(call.state, ToolState::Pending | ToolState::Running) {
        parts.push("dispatching".to_owned());
    }
    let right = vec![
        Span::styled(parts.join(" · "), styles.meta()),
        Span::styled(" · ", styles.meta()),
        Span::styled("ctrl+b", styles.accent()),
    ];
    let left = vec![
        chrome_span(Span::styled(format!("{glyph} "), glyph_style)),
        Span::styled("agent  ", styles.tool_title()),
        Span::styled(task(card), styles.tool_output()),
    ];
    Line::from(blocks::row_spans(left, right, width))
}

/// Expanded dispatch card (via `Ctrl+O`): shows the sync dispatch details and
/// points to `Ctrl+B` for live child monitoring and full Markdown results.
fn dispatch_card_rows(
    call: &ToolCall,
    card: &AgentCard,
    styles: Styles,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![dispatch_row(call, card, styles, width)];
    let card_width = width.saturating_sub(CARD_INDENT);
    if card_width < blocks::MIN_WIDTH {
        return lines;
    }
    let failed = matches!(call.state, ToolState::Failed) || call.result_error.is_some();
    let (rail, surface) = if failed {
        (styles.card_failure_rail(), styles.card_failure())
    } else {
        (styles.card_rail(), styles.card())
    };
    let inner = blocks::text_width(card_width);
    let row = |spans: Vec<Span<'static>>| {
        indent(
            surface_row(rail, surface, blocks::padded(spans), card_width),
            CARD_INDENT,
        )
    };
    let blank = || {
        chrome(indent(
            surface_row(rail, surface, Vec::new(), card_width),
            CARD_INDENT,
        ))
    };
    let label = |text: &str| chrome(row(vec![Span::styled(text.to_owned(), styles.dim())]));

    lines.push(blank());
    lines.push(chrome(row(vec![Span::styled(
        wrap::truncate(&facts(card, true), inner),
        styles.meta(),
    )])));
    let raw_task = card.task.trim();
    if !raw_task.is_empty() {
        let wrapped = wrap::wrapped(&wrap::sanitize(raw_task), inner, styles.dim());
        if !wrapped.is_empty() {
            lines.push(blank());
            lines.push(label("Task"));
            for line in wrapped.iter().take(TASK_ROWS) {
                lines.push(row(line.spans.clone()));
            }
            if wrapped.len() > TASK_ROWS {
                lines.push(chrome(row(vec![Span::styled(
                    format!(
                        "… {}",
                        blocks::plural((wrapped.len() - TASK_ROWS) as u64, "more line")
                    ),
                    styles.faint(),
                )])));
            }
        }
    }
    if let Some(error) = &call.result_error {
        lines.push(blank());
        for line in wrap::wrapped(&wrap::sanitize(error), inner, styles.failure()) {
            lines.push(row(line.spans));
        }
    }
    lines.push(blank());
    lines.push(chrome(row(vec![Span::styled(
        wrap::truncate("Inspect live progress & result · click or ctrl+b", inner),
        styles.accent(),
    )])));
    lines.push(blank());
    lines
}

/// Full scrollable detail lines for the `Ctrl+B` Async Tasks Inspector right pane.
pub(crate) fn detail_lines(
    card: &AgentCard,
    styles: Styles,
    width: usize,
    reasoning: bool,
) -> Vec<Line<'static>> {
    let width = width.max(12);
    let mut lines = Vec::new();
    let (glyph_str, glyph_style) = glyph(card, styles);
    let state_label = status(card);
    lines.push(Line::from(vec![
        Span::styled(format!("{glyph_str} "), glyph_style),
        Span::styled("agent", styles.tool_title()),
        Span::styled(" · ", styles.meta()),
        Span::styled(
            wrap::truncate(&task(card), width.saturating_sub(24)),
            styles.bold(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(facts(card, true), styles.meta()),
        Span::styled(" · ", styles.meta()),
        Span::styled(state_label, state_style(card, styles)),
    ]));

    if needs_input(card)
        && let Some(state) = &card.state
    {
        lines.push(Line::raw(""));
        let bar = |spans: Vec<Span<'static>>| {
            let mut out = vec![Span::styled("▎ ", styles.warning())];
            out.extend(spans);
            Line::from(out)
        };
        lines.push(bar(vec![Span::styled(
            "Input required · press enter to open child Session and respond",
            styles.warning(),
        )]));
        for pending in &state.data.pending_interactions {
            let summary = format!(
                "{} · {}",
                pending.name,
                match &pending.request {
                    InteractionRequestData::Approval { .. } => "approval",
                    InteractionRequestData::ToolInput { .. } => "input",
                }
            );
            for wrapped in wrap::wrapped(
                &wrap::sanitize(&summary),
                width.saturating_sub(2),
                styles.text(),
            ) {
                lines.push(bar(wrapped.spans));
            }
        }
    }

    let raw_task = card.task.trim();
    if !raw_task.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Task", styles.heading()));
        lines.extend(wrap::wrapped(
            &wrap::sanitize(raw_task),
            width,
            styles.text(),
        ));
    }

    let activity = activity_rows(card, styles);
    if !activity.is_empty() || card.truncated {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("Activity ({})", activity.len()),
            styles.heading(),
        ));
        if card.truncated {
            lines.push(Line::styled("… earlier steps truncated", styles.faint()));
        }
        for (glyph_str, glyph_style, text) in &activity {
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph_str} "), *glyph_style),
                Span::styled(
                    wrap::truncate(text, width.saturating_sub(2)),
                    styles.tool_output(),
                ),
            ]));
        }
    }

    let (heading, body) = answer(card, width, reasoning, styles);
    if !body.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(heading, styles.heading()));
        lines.extend(body);
    } else if card.outcome().is_none() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Result", styles.heading()));
        lines.push(Line::styled(
            "(running — final answer will appear here upon completion)",
            styles.faint(),
        ));
    }

    if card.history_unavailable {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Child history unavailable · showing retained observed preview",
            styles.meta(),
        ));
    } else if let Some(error) = &card.observation_error {
        lines.push(Line::raw(""));
        lines.extend(wrap::wrapped(
            &wrap::sanitize(error),
            width,
            styles.warning(),
        ));
    }
    lines
}

/// Plain text copied by `Ctrl+Y` in the `Ctrl+B` Async Tasks Inspector.
pub(crate) fn copy_text(card: &AgentCard) -> String {
    let delivered = card.result.as_deref().and_then(super::delegated::summary);
    if let Some(summary) = delivered {
        let mut parts = Vec::new();
        if let Some(fault) = summary.fault {
            parts.push(fault);
        }
        if let Some(answer) = summary.answer {
            parts.push(answer);
        }
        if !parts.is_empty() {
            return parts.join("\n\n");
        }
    }
    if let Some(answer) = card.transcript.last_answer() {
        return answer.to_owned();
    }
    if let Some(fault) = card.fault() {
        return fault.to_owned();
    }
    card.task.clone()
}

const TASK_ROWS: usize = 4;

/// The card's one-line name: the task's first line.
pub(crate) fn task(card: &AgentCard) -> String {
    let first = card
        .task
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    if first.is_empty() {
        "delegated task".to_owned()
    } else {
        wrap::sanitize(first.trim()).into_owned()
    }
}

/// `Assistant · 2 tool rounds`, and in the open card also the Session and
/// later sends.
fn facts(card: &AgentCard, full: bool) -> String {
    let label = card
        .agent_name
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| short(card.key.child.agent_id.as_str()));
    let mut facts = vec![wrap::truncate(&wrap::sanitize(&label), 20)];
    if full {
        facts.push(format!(
            "Session {}",
            short(card.key.child.session_id.as_str())
        ));
    }
    if let Some((_, turn)) = &card.turn
        && turn.tool_rounds > 0
    {
        facts.push(blocks::plural(turn.tool_rounds, "tool round"));
    }
    if full {
        let parent = &card.parent;
        if parent.sends > 0 {
            facts.push(blocks::plural(parent.sends as u64, "later send"));
        }
        if parent.inspections > 0 {
            facts.push(blocks::plural(parent.inspections as u64, "inspection"));
        }
        if parent.changes > 0 {
            facts.push(blocks::plural(parent.changes as u64, "control"));
        }
        if parent.waiting {
            facts.push("parent waiting".to_owned());
        } else if parent.waits > 0 {
            facts.push(if parent.waited.is_zero() {
                "parent waited".to_owned()
            } else {
                format!("parent waited {}", blocks::format_duration(parent.waited))
            });
        }
    }
    facts.join(" · ")
}

pub(crate) fn glyph(card: &AgentCard, styles: Styles) -> (&'static str, ratatui::style::Style) {
    match card.outcome() {
        Some(TurnOutcome::Completed) => ("✓", styles.success()),
        Some(TurnOutcome::Failed) => ("✗", styles.failure()),
        Some(TurnOutcome::Cancelled) => ("⊘", styles.meta()),
        None if needs_input(card) => ("●", styles.warning()),
        None if card.history_unavailable || card.historical => ("○", styles.meta()),
        None => ("●", styles.accent()),
    }
}

fn state_style(card: &AgentCard, styles: Styles) -> ratatui::style::Style {
    match card.outcome() {
        Some(TurnOutcome::Failed) => styles.failure(),
        None if needs_input(card) => styles.warning(),
        _ => styles.meta(),
    }
}

pub(crate) fn needs_input(card: &AgentCard) -> bool {
    card.state
        .as_ref()
        .is_some_and(|state| !state.data.pending_interactions.is_empty())
}

/// One entry per child tool call, oldest first: its glyph and `name subject`.
fn activity_rows(
    card: &AgentCard,
    styles: Styles,
) -> Vec<(&'static str, ratatui::style::Style, String)> {
    let blocks = card.transcript.blocks();
    // Settled, the last response is the Result; it is not activity too.
    let answer = (card.outcome().is_some() || card.result.is_some())
        .then(|| {
            blocks.iter().rposition(
                |block| matches!(block, Block::Assistant { text, .. } if !text.trim().is_empty()),
            )
        })
        .flatten();
    let line = |text: &str| {
        text.lines()
            .map(|line| line.trim().trim_start_matches('#').trim().replace("**", ""))
            .find(|line| !line.is_empty())
            .unwrap_or_default()
    };
    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            Block::Tool(call) => {
                let (glyph, style) = blocks::tool_glyph(call, styles);
                Some((glyph, style, blocks::one_line(&activity(call))))
            }
            Block::Assistant { text, .. } if !text.trim().is_empty() && Some(index) != answer => {
                Some(("›", styles.meta(), wrap::sanitize(&line(text)).into_owned()))
            }
            Block::Assistant {
                reasoning, text, ..
            } if text.trim().is_empty() && !reasoning.trim().is_empty() => {
                let heading = reasoning
                    .lines()
                    .rev()
                    .map(|line| line.trim().replace("**", ""))
                    .find(|line| !line.is_empty())
                    .unwrap_or_default();
                Some(("∴", styles.meta(), wrap::sanitize(&heading).into_owned()))
            }
            _ => None,
        })
        .collect()
}

/// The child's answer: the delivered result when the parent has it, else the
/// latest response it observed; a failure leads with its reason.
fn answer(
    card: &AgentCard,
    width: usize,
    reasoning: bool,
    styles: Styles,
) -> (&'static str, Vec<Line<'static>>) {
    let mut body = Vec::new();
    let delivered = card.result.as_deref().and_then(super::delegated::summary);
    let fault = delivered
        .as_ref()
        .and_then(|summary| summary.fault.clone())
        .or_else(|| card.fault().map(str::to_owned));
    if let Some(fault) = fault {
        body.extend(wrap::wrapped(
            &wrap::sanitize(&fault),
            width,
            styles.failure(),
        ));
    }
    let settled = card.outcome().is_some() || delivered.is_some();
    let text = delivered
        .and_then(|summary| summary.answer)
        .or_else(|| card.transcript.last_answer().map(str::to_owned));
    // Not being able to watch the child matters only while nothing else is known.
    if text.is_none()
        && body.is_empty()
        && let Some(error) = &card.observation_error
    {
        body.extend(wrap::wrapped(
            &wrap::sanitize(error),
            width,
            styles.warning(),
        ));
    }
    if let Some(text) = text {
        body.extend(markdown::render(&text, width, &styles, styles.text()));
    } else if reasoning
        && !settled
        && let Some(Block::Assistant { reasoning, .. }) = card.transcript.blocks().last()
        && !reasoning.trim().is_empty()
    {
        body.extend(markdown::render(
            &blocks::reasoning_source(reasoning),
            width,
            &styles,
            blocks::thinking_style(styles),
        ));
        return ("Thinking", body);
    }
    (if settled { "Result" } else { "Latest response" }, body)
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

/// `bash node --test`, `read src/store.js`: the tool and its main argument.
fn activity(call: &ToolCall) -> String {
    let detail = call.detail.lines().next().unwrap_or("").trim();
    if detail.is_empty() {
        call.name.clone()
    } else {
        format!("{} {detail}", call.name)
    }
}

fn short(id: &str) -> String {
    id.chars().take(6).collect()
}

pub(crate) fn status(card: &AgentCard) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::RequestKey;
    use crate::ui::theme::Palette;
    use minicore_host_protocol::{
        DelegationAssociation, DelegationTerminalData, DelegationTurnOutcome, SessionRef,
        ToolCallRef, TurnRef,
    };

    fn styles() -> Styles {
        Styles::of(Palette::dark())
    }

    fn call() -> ToolCall {
        let origin = ToolCallRef {
            turn_id: "00000000000000000001".parse().unwrap(),
            tool_call_id: "start".to_owned().into(),
        };
        let parent = SessionRef {
            agent_id: "00000000000000000002".parse().unwrap(),
            session_id: "00000000000000000003".parse().unwrap(),
        };
        let child = SessionRef {
            agent_id: parent.agent_id.clone(),
            session_id: "00000000000000000004".parse().unwrap(),
        };
        let task_message_id: minicore_host_protocol::MessageId =
            "00000000000000000005".parse().unwrap();
        let association = DelegationAssociation {
            parent,
            origin: origin.clone(),
            child,
            accepted_message_ids: vec![task_message_id.clone()],
            task_message_id,
            inputs: Vec::new(),
            completion: None,
        };
        let mut card = AgentCard::new("00000000000000000006".parse().unwrap(), association);
        card.agent_name = Some("Reader 日本語".into());
        card.task = "Read the files".into();
        let mut call = ToolCall::new(origin, "agent");
        call.set_state(ToolState::Succeeded);
        call.agent_card = Some(Box::new(card));
        call
    }

    fn response(card: &mut AgentCard, text: String) {
        card.transcript.push(Block::Assistant {
            key: RequestKey::response("00000000000000000007".parse().unwrap(), 0),
            text,
            reasoning: String::new(),
            streaming: false,
            partial: false,
            expanded: None,
        });
    }

    fn completed(card: &mut AgentCard) {
        card.association.completion = Some(DelegationTerminalData {
            turns: vec![DelegationTurnOutcome {
                turn: TurnRef {
                    session_id: card.key.child.session_id.clone(),
                    turn_id: "00000000000000000007".parse().unwrap(),
                },
                outcome: TurnOutcome::Completed,
                fault: None,
            }],
            discarded_inputs: Vec::new(),
        });
    }

    fn text(rows: &[Line<'static>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.to_string().trim_end().to_owned())
            .collect()
    }

    #[test]
    fn a_successful_startup_receipt_is_not_child_completion() {
        let mut call = call();
        assert_eq!(
            status(call.agent_card.as_ref().unwrap()),
            "accepted · observing child"
        );
        let card = call.agent_card.as_mut().unwrap();
        card.history_unavailable = true;
        assert_eq!(status(card), "history unavailable");
        let rows = detail_lines(card, styles(), 80, true);
        assert!(
            rows.iter()
                .any(|row| row.to_string().contains("Child history unavailable"))
        );
    }

    #[test]
    fn a_folded_dispatch_stays_one_immutable_sync_row() {
        let mut call = call();
        let card = call.agent_card.as_mut().unwrap();
        for name in ["read", "bash", "edit", "bash"] {
            let mut tool = ToolCall::new(
                ToolCallRef {
                    turn_id: "00000000000000000007".parse().unwrap(),
                    tool_call_id: format!("{name}{}", card.transcript.len()).into(),
                },
                name,
            );
            tool.set_state(ToolState::Succeeded);
            card.transcript.push(Block::Tool(Box::new(tool)));
        }
        let running = text(&render(&call, styles(), 80, false, true));
        assert_eq!(
            running.len(),
            1,
            "dispatch row stays one line while child runs: {running:?}"
        );
        assert!(
            running[0].starts_with("✓ agent  Read the files"),
            "{running:?}"
        );
        assert!(running[0].ends_with("000000 · ctrl+b"), "{running:?}");

        completed(call.agent_card.as_mut().unwrap());
        let done = text(&render(&call, styles(), 80, false, true));
        assert_eq!(
            done, running,
            "dispatch row never mutates in place across later runs"
        );
    }

    #[test]
    fn detail_lines_hold_full_activity_and_delivered_result_for_ctrl_b_inspector() {
        let mut call = call();
        let card = call.agent_card.as_mut().unwrap();
        response(card, "seen while running".into());
        completed(card);
        card.parent.sends = 1;
        card.result = Some(format!(
            r#"{{"source":{{"agent_id":"a","session_id":"00000000000000000004"}},"status":"ended","results":[{{"outcome":"completed","output":["{}"]}}]}}"#,
            (0..40)
                .map(|i| format!("answer {i}"))
                .collect::<Vec<_>>()
                .join("\\n\\n")
        ));
        for width in [8, 20, 80] {
            let rows = render(&call, styles(), width, true, true);
            assert!(
                wrap::width(&rows[0].to_string()) <= width,
                "header fits width {width}: {:?}",
                rows[0]
            );
            assert!(
                rows[1..]
                    .iter()
                    .all(|row| wrap::width(&row.to_string()) == width),
                "surface rows fill width {width}: {rows:?}"
            );
            if width == 80 {
                let rows = text(&rows);
                assert!(rows[0].starts_with("✓ agent  Read the files"), "{rows:?}");
                assert!(rows.iter().any(|row| row.contains("ctrl+b")), "{rows:?}");
            }
        }
        let detail = text(&detail_lines(
            call.agent_card.as_ref().unwrap(),
            styles(),
            80,
            true,
        ));
        assert!(
            detail
                .iter()
                .any(|row| row.contains("Session 000000 · 1 later send")),
            "{detail:?}"
        );
        assert!(detail.iter().any(|row| row == "Result"), "{detail:?}");
        assert!(detail.iter().any(|row| row.contains("answer 0")));
        assert!(detail.iter().any(|row| row.contains("answer 39")));
        assert!(!detail.iter().any(|row| row.contains("seen while running")));
    }

    #[test]
    fn activity_lists_steps_without_output_or_escapes() {
        let mut call = call();
        let card = call.agent_card.as_mut().unwrap();
        let mut tool = ToolCall::new(
            ToolCallRef {
                turn_id: "00000000000000000007".parse().unwrap(),
                tool_call_id: "read".to_owned().into(),
            },
            "bash\u{1b}[31m",
        );
        tool.set_state(ToolState::Succeeded);
        tool.push_output("first\n\nlast");
        card.transcript.push(Block::Tool(Box::new(tool)));
        let detail = text(&detail_lines(card, styles(), 80, true));
        assert!(detail.iter().any(|row| row == "Activity (1)"), "{detail:?}");
        assert!(detail.iter().any(|row| row.starts_with("✓ bash")));
        assert!(detail.iter().all(|row| !row.contains("first")));
        assert!(detail.iter().all(|row| !row.contains('\u{1b}')));
    }

    #[test]
    fn later_sends_and_the_delivered_result_link_while_keeping_arrival_banner() {
        use crate::transcript::{Transcript, UserKind};
        let start = call();
        let association = start.agent_card.as_ref().unwrap().association.clone();
        let mut send = ToolCall::new(
            ToolCallRef {
                turn_id: "00000000000000000001".parse().unwrap(),
                tool_call_id: "send".to_owned().into(),
            },
            "agent",
        );
        send.set_state(ToolState::Succeeded);
        send.delegations.push(DelegationAssociation {
            task_message_id: "00000000000000000008".parse().unwrap(),
            ..association
        });
        let result = |session: &str| Block::User {
            text: format!(
                r#"{{"source":{{"agent_id":"a","session_id":"{session}"}},"task_message_id":"00000000000000000008","status":"ended","results":[{{"outcome":"completed","output":["done"]}}]}}"#
            ),
            kind: UserKind::Delegated,
            at: None,
            expanded: None,
        };
        let mut transcript = Transcript::default();
        transcript.push(Block::Tool(Box::new(start)));
        transcript.push(Block::Tool(Box::new(send)));
        transcript.push(result("00000000000000000004"));
        transcript.push(result("00000000000000000099"));
        transcript.link_delegations();
        assert!(!transcript.absorbed(0));
        assert!(transcript.absorbed(1), "the later send links to its task");
        assert!(
            transcript.absorbed(2),
            "delivered result stays hidden in main transcript while linking to its task for Ctrl+B"
        );
        assert!(
            transcript.absorbed(3),
            "another child's delegated result also stays hidden in main transcript"
        );
        let Block::Tool(card) = &transcript.blocks()[0] else {
            unreachable!()
        };
        let card = card.agent_card.as_deref().unwrap();
        assert_eq!(card.parent.sends, 1);
        assert!(card.result.as_deref().unwrap().contains("\"done\""));
        let before: Vec<u64> = transcript.revisions().collect();
        transcript.link_delegations();
        assert_eq!(
            transcript.revisions().collect::<Vec<_>>(),
            before,
            "linking again changes nothing"
        );
    }

    #[test]
    fn the_parents_inspections_and_waits_join_the_card_and_failures_stay_out() {
        use crate::transcript::Transcript;
        let start = call();
        let agent = |id: &str, arguments: serde_json::Value, state| {
            let mut call = ToolCall::new(
                ToolCallRef {
                    turn_id: "00000000000000000001".parse().unwrap(),
                    tool_call_id: id.to_owned().into(),
                },
                "agent",
            );
            call.set_arguments(&arguments);
            call.set_state(state);
            call.duration = Some(std::time::Duration::from_secs(3));
            Block::Tool(Box::new(call))
        };
        let target = serde_json::json!({"agent_id":"00000000000000000002","session_id":"00000000000000000004"});
        let mut transcript = Transcript::default();
        transcript.push(Block::Tool(Box::new(start)));
        transcript.push(agent(
            "inspect",
            serde_json::json!({"action":"inspect","target":target}),
            ToolState::Succeeded,
        ));
        transcript.push(agent(
            "wait",
            serde_json::json!({"action":"wait","timeout_ms":1000}),
            ToolState::Succeeded,
        ));
        transcript.push(agent(
            "broken",
            serde_json::json!({"action":"inspect","target":target}),
            ToolState::Failed,
        ));
        transcript.link_delegations();
        assert!(transcript.absorbed(1) && transcript.absorbed(2));
        assert!(!transcript.absorbed(3), "a failed call keeps its row");
        let Block::Tool(card) = &transcript.blocks()[0] else {
            unreachable!()
        };
        let parent = card.agent_card.as_ref().unwrap().parent;
        assert_eq!((parent.inspections, parent.waits), (1, 1));
        assert_eq!(parent.waited, std::time::Duration::from_secs(3));
        let detail = text(&detail_lines(
            card.agent_card.as_ref().unwrap(),
            styles(),
            100,
            true,
        ));
        assert!(
            detail
                .iter()
                .any(|row| row.contains("1 inspection · parent waited 3.0s")),
            "{detail:?}"
        );
    }

    #[test]
    fn cancelled_task_uses_neutral_surface_and_live_waiting_label() {
        let mut call = call();
        let card = call.agent_card.as_mut().unwrap();
        card.association.completion = Some(DelegationTerminalData {
            turns: vec![DelegationTurnOutcome {
                turn: TurnRef {
                    session_id: card.key.child.session_id.clone(),
                    turn_id: "00000000000000000007".parse().unwrap(),
                },
                outcome: TurnOutcome::Cancelled,
                fault: None,
            }],
            discarded_inputs: Vec::new(),
        });
        card.parent.waiting = true;
        let rendered = render(&call, styles(), 80, true, true);
        assert_eq!(rendered[1].spans[2].style.bg, styles().card().bg);
        assert_ne!(rendered[1].spans[2].style.bg, styles().card_failure().bg);
        let detail = text(&detail_lines(
            call.agent_card.as_ref().unwrap(),
            styles(),
            80,
            true,
        ));
        assert!(detail.iter().any(|row| row.contains("parent waiting")));
        assert!(detail.iter().any(|row| row.contains("cancelled")));
    }

    #[test]
    fn expanded_dispatch_card_keeps_task_and_points_to_ctrl_b() {
        let mut multiline = call();
        multiline.agent_card.as_mut().unwrap().task =
            "Check Tokyo weather\nInclude UTC timestamp and wind speed".into();
        let multi_rows = text(&render(&multiline, styles(), 90, true, true));
        assert!(
            multi_rows.iter().any(|row| row == "  ▎ Task"),
            "task should expand inside dispatch card: {multi_rows:?}"
        );
        assert!(
            multi_rows
                .iter()
                .any(|row| row.contains("Include UTC timestamp and wind speed")),
            "{multi_rows:?}"
        );
        assert!(
            multi_rows.iter().any(|row| row.contains("ctrl+b")),
            "{multi_rows:?}"
        );
    }
}
