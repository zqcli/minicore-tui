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
    if expanded {
        card_rows(card, styles, width, reasoning)
    } else {
        folded(card, styles, width)
    }
}

/// The whole delegation as one step row: task on the left, who and how far on
/// the right; while it runs, its newest activity underneath.
fn folded(card: &AgentCard, styles: Styles, width: usize) -> Vec<Line<'static>> {
    let (glyph, glyph_style) = glyph(card, styles);
    let mut right = vec![Span::styled(facts(card, false), styles.meta())];
    let state = status(card);
    right.push(Span::styled(" · ", styles.meta()));
    right.push(Span::styled(state, state_style(card, styles)));
    let left = vec![
        chrome_span(Span::styled(format!("{glyph} "), glyph_style)),
        Span::styled("agent  ", styles.tool_title()),
        Span::styled(task(card), styles.tool_output()),
    ];
    let mut lines = vec![Line::from(blocks::row_spans(left, right, width))];
    if card.outcome().is_none() && !card.history_unavailable {
        let activity = activity_rows(card, styles);
        let start = activity.len().saturating_sub(LIVE_ROWS);
        for (glyph, glyph_style, text) in &activity[start..] {
            lines.push(Line::from(vec![
                chrome_span(Span::styled("  │ ", styles.meta())),
                chrome_span(Span::styled(format!("{glyph} "), *glyph_style)),
                Span::styled(
                    wrap::truncate(text, width.saturating_sub(7)),
                    styles.tool_output(),
                ),
            ]));
        }
    }
    lines
}

/// The opened card: the same task and state, then what the child did, its
/// answer and the way into its Session.
fn card_rows(
    card: &AgentCard,
    styles: Styles,
    width: usize,
    reasoning: bool,
) -> Vec<Line<'static>> {
    let (rail, surface) = match card.outcome() {
        Some(TurnOutcome::Completed) | None => (styles.card_rail(), styles.card()),
        Some(_) => (styles.card_failure_rail(), styles.card_failure()),
    };
    let inner = blocks::text_width(width);
    let row = |spans: Vec<Span<'static>>| surface_row(rail, surface, blocks::padded(spans), width);
    let blank = || chrome(surface_row(rail, surface, Vec::new(), width));
    let label = |text: &str| chrome(row(vec![Span::styled(text.to_owned(), styles.dim())]));

    let (glyph, glyph_style) = glyph(card, styles);
    let state = status(card);
    let title = wrap::truncate(&task(card), inner.saturating_sub(wrap::width(&state) + 4));
    let pad = inner.saturating_sub(wrap::width(&title) + wrap::width(&state) + 2);
    let mut lines = vec![
        chrome(row(vec![
            Span::styled(title, styles.tool_title()),
            Span::raw(" ".repeat(pad)),
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(state, state_style(card, styles)),
        ])),
        chrome(row(vec![Span::styled(
            wrap::truncate(&facts(card, true), inner),
            styles.meta(),
        )])),
    ];

    // The rest of a multi-line task, which the title cuts.
    let rest: Vec<&str> = card
        .task
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect();
    if !rest.is_empty() {
        lines.push(blank());
        for text in rest.iter().take(TASK_ROWS) {
            lines.push(row(vec![Span::styled(
                wrap::truncate(&wrap::sanitize(text), inner),
                styles.dim(),
            )]));
        }
        if rest.len() > TASK_ROWS {
            lines.push(chrome(row(vec![Span::styled(
                format!("… {} more lines", rest.len() - TASK_ROWS),
                styles.faint(),
            )])));
        }
    }

    let activity = activity_rows(card, styles);
    if !activity.is_empty() || card.truncated {
        lines.push(blank());
        lines.push(label("Activity"));
        let start = activity.len().saturating_sub(ACTIVITY_ROWS);
        let earlier = start + usize::from(card.truncated);
        if earlier > 0 {
            lines.push(chrome(row(vec![Span::styled(
                if card.truncated {
                    "… earlier steps".to_owned()
                } else {
                    format!("… {start} earlier steps")
                },
                styles.faint(),
            )])));
        }
        for (glyph, glyph_style, text) in &activity[start..] {
            lines.push(row(vec![
                chrome_span(Span::styled(format!("{glyph} "), *glyph_style)),
                Span::styled(
                    wrap::truncate(text, inner.saturating_sub(2)),
                    styles.tool_output(),
                ),
            ]));
        }
    }

    let (heading, body) = answer(card, inner, reasoning, styles);
    if !body.is_empty() {
        lines.push(blank());
        lines.push(label(heading));
        let hidden = body.len().saturating_sub(ANSWER_ROWS);
        for line in body.into_iter().take(ANSWER_ROWS) {
            lines.push(row(line.spans));
        }
        if hidden > 0 {
            lines.push(chrome(row(vec![Span::styled(
                format!("… {hidden} more lines · open the child Session"),
                styles.faint(),
            )])));
        }
    }

    let footer = if card.history_unavailable {
        "Child history unavailable · this card keeps what was observed"
    } else if card.observation_error.is_some() {
        "Observation incomplete · open the child Session (click or /child)"
    } else {
        "Open the child Session · click or /child"
    };
    lines.push(blank());
    lines.push(chrome(row(vec![Span::styled(
        wrap::truncate(footer, inner),
        styles.meta(),
    )])));
    lines
}

const LIVE_ROWS: usize = 3;
const TASK_ROWS: usize = 4;
const ACTIVITY_ROWS: usize = 8;
const ANSWER_ROWS: usize = 16;

/// The card's one-line name: the task's first line.
fn task(card: &AgentCard) -> String {
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
    if full && card.sends > 0 {
        facts.push(format!(
            "{} later send{}",
            card.sends,
            if card.sends == 1 { "" } else { "s" }
        ));
    }
    facts.join(" · ")
}

fn glyph(card: &AgentCard, styles: Styles) -> (&'static str, ratatui::style::Style) {
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

fn needs_input(card: &AgentCard) -> bool {
    card.state
        .as_ref()
        .is_some_and(|state| !state.data.pending_interactions.is_empty())
}

/// One entry per child tool call, oldest first: its glyph and `name subject`.
fn activity_rows(
    card: &AgentCard,
    styles: Styles,
) -> Vec<(&'static str, ratatui::style::Style, String)> {
    card.transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Tool(call) => {
                let (glyph, style) = blocks::tool_glyph(call, styles);
                Some((glyph, style, blocks::one_line(&activity(call))))
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
        body.extend(markdown::render(
            &blocks::reasoning_source(&text),
            width,
            &styles,
            styles.text(),
        ));
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
    use crate::ui::rails::content_cells;
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
        let rows = render(&call, styles(), 80, true, true);
        assert!(
            rows.last()
                .unwrap()
                .to_string()
                .contains("Child history unavailable")
        );
    }

    #[test]
    fn a_folded_card_is_one_row_and_running_adds_its_newest_steps() {
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
        assert!(
            running[0].starts_with("● agent  Read the files"),
            "{running:?}"
        );
        assert!(
            running[0].ends_with("Reader 日本語 · accepted · observing child"),
            "{running:?}"
        );
        assert_eq!(running[1..], ["  │ ✓ bash", "  │ ✓ edit", "  │ ✓ bash"]);

        completed(call.agent_card.as_mut().unwrap());
        let done = text(&render(&call, styles(), 80, false, true));
        assert_eq!(done.len(), 1, "a settled card folds to one row: {done:?}");
        assert!(done[0].starts_with("✓ agent  Read the files"), "{done:?}");
        assert!(done[0].ends_with("· completed"), "{done:?}");
    }

    #[test]
    fn an_open_card_holds_activity_and_the_delivered_result_within_its_bounds() {
        let mut call = call();
        let card = call.agent_card.as_mut().unwrap();
        response(card, "seen while running".into());
        completed(card);
        card.sends = 1;
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
                rows.iter()
                    .all(|row| wrap::width(&row.to_string()) == width)
            );
            if width == 80 {
                let rows = text(&rows);
                assert!(rows[0].starts_with("▎ Read the files"), "{rows:?}");
                assert!(rows[0].ends_with("✓ completed"), "{rows:?}");
                assert!(
                    rows[1].contains("Session 000000 · 1 later send"),
                    "{rows:?}"
                );
                assert!(rows.iter().any(|row| row == "▎ Result"), "{rows:?}");
                assert!(rows.iter().any(|row| row.contains("answer 0")));
                assert!(!rows.iter().any(|row| row.contains("seen while running")));
                assert!(
                    rows.iter()
                        .any(|row| row.contains("more lines · open the child"))
                );
                assert!(rows.last().unwrap().contains("Open the child Session"));
                assert!(rows.len() <= ANSWER_ROWS + 12, "{}", rows.len());
            }
        }
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
        let rows = render(&call, styles(), 80, true, true);
        let label = rows
            .iter()
            .position(|row| row.to_string().trim_end() == "▎ Activity")
            .unwrap();
        assert!(content_cells(&rows[label]).is_empty(), "labels are chrome");
        assert!(rows[label + 1].to_string().starts_with("▎ ✓ bash"));
        // Output stays in the child Session; the card lists what was done.
        assert!(rows.iter().all(|row| !row.to_string().contains("first")));
        assert_eq!(content_cells(rows.last().unwrap()), 0..0);
        assert!(rows.iter().all(|row| !row.to_string().contains('\u{1b}')));
    }

    #[test]
    fn later_sends_and_the_delivered_result_live_in_the_one_card() {
        use crate::transcript::{Transcript, UserKind};
        let start = call();
        let association = start.agent_card.as_ref().unwrap().association.clone();
        // A later send into the same child: a receipt from another call.
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
        };
        let mut transcript = Transcript::default();
        transcript.push(Block::Tool(Box::new(start)));
        transcript.push(Block::Tool(Box::new(send)));
        transcript.push(result("00000000000000000004"));
        transcript.push(result("00000000000000000099"));
        transcript.link_delegations();
        assert!(!transcript.absorbed(0));
        assert!(transcript.absorbed(1), "the later send is in the card");
        assert!(transcript.absorbed(2), "the result is in the card");
        assert!(
            !transcript.absorbed(3),
            "another child's result keeps its block"
        );
        let Block::Tool(card) = &transcript.blocks()[0] else {
            unreachable!()
        };
        let card = card.agent_card.as_deref().unwrap();
        assert_eq!(card.sends, 1);
        assert!(card.result.as_deref().unwrap().contains("\"done\""));
        let before: Vec<u64> = transcript.revisions().collect();
        transcript.link_delegations();
        assert_eq!(
            transcript.revisions().collect::<Vec<_>>(),
            before,
            "linking again changes nothing"
        );
    }
}
