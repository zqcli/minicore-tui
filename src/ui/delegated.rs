//! A child Session's settled result as delivered to its parent. The parent
//! model receives Host's JSON notification; people read the outcome and the
//! child's final answer instead of message and Turn identifiers.

use minicore_host_protocol::Timestamp;
use ratatui::text::{Line, Span};
use serde::Deserialize;

use super::blocks::{local_zone, padded, stamp_label, text_width};
use super::markdown;
use super::rails::{chrome, surface_row};
use super::theme::Styles;
use crate::wrap;

/// Rows of a folded result body, matching other long blocks.
const RESULT_ROWS: usize = 12;

#[derive(Deserialize)]
struct Notification {
    source: Child,
    status: String,
    #[serde(default)]
    results: Vec<Outcome>,
}

#[derive(Deserialize)]
struct Child {
    session_id: String,
}

#[derive(Deserialize)]
struct Outcome {
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    output: Option<Vec<String>>,
    #[serde(default)]
    fault: Option<Fault>,
    #[serde(default)]
    partials: Vec<Partial>,
}

#[derive(Deserialize)]
struct Fault {
    message: String,
}

#[derive(Deserialize)]
struct Partial {
    #[serde(default)]
    text: Vec<String>,
}

fn parse(text: &str) -> Option<Notification> {
    serde_json::from_str(text).ok()
}

/// Whether an Agent-sourced input is a delegation result notification.
pub fn is_result(text: &str) -> bool {
    text.starts_with('{') && parse(text).is_some()
}

/// The result card, or `None` when the text is not a notification.
pub fn render(
    text: &str,
    at: Option<Timestamp>,
    styles: Styles,
    width: usize,
    expanded: bool,
) -> Option<Vec<Line<'static>>> {
    let notification = parse(text)?;
    let last = notification.results.last();
    let outcome = last
        .and_then(|result| result.outcome.as_deref())
        .unwrap_or(if notification.status == "not_executed" {
            "not executed"
        } else {
            "ended"
        })
        .to_owned();
    let (rail, surface, tone) = match outcome.as_str() {
        "completed" => (
            styles.card_success_rail(),
            styles.card_success(),
            styles.success(),
        ),
        "failed" | "cancelled" | "not executed" => (
            styles.card_failure_rail(),
            styles.card_failure(),
            styles.failure(),
        ),
        _ => (styles.card_rail(), styles.card(), styles.meta()),
    };
    let row = |spans: Vec<Span<'static>>| surface_row(rail, surface, spans, width);
    let short: String = notification.source.session_id.chars().take(6).collect();
    let mut header = vec![
        Span::styled("↳ Delegated result", styles.bold()),
        Span::styled(format!(" · Session {short} · "), styles.meta()),
        Span::styled(outcome, tone),
    ];
    if let Some(at) = at {
        let stamp = stamp_label(at, local_zone());
        let used: usize = header.iter().map(|span| wrap::width(&span.content)).sum();
        let room = super::blocks::text_width(width);
        if used + wrap::width(&stamp) + 2 <= room {
            header.push(Span::raw(" ".repeat(room - used - wrap::width(&stamp))));
            header.push(Span::styled(stamp, styles.meta()));
        }
    }
    let mut lines = vec![row(padded(header))];
    let answer = last
        .and_then(|result| result.output.as_ref())
        .map(|parts| parts.join("\n\n"))
        .filter(|answer| !answer.trim().is_empty())
        .or_else(|| {
            let partial = last?
                .partials
                .iter()
                .flat_map(|partial| partial.text.iter().cloned())
                .collect::<Vec<_>>()
                .join("");
            (!partial.trim().is_empty()).then(|| format!("{partial}\n\n(partial response)"))
        });
    let mut body = Vec::new();
    if let Some(fault) = last.and_then(|result| result.fault.as_ref()) {
        body.extend(wrap::wrapped(
            &wrap::sanitize(&fault.message),
            text_width(width),
            styles.failure(),
        ));
    }
    match answer {
        Some(answer) => body.extend(markdown::render(
            &answer,
            text_width(width),
            &styles,
            styles.text(),
        )),
        None if body.is_empty() => body.push(Line::styled("No final answer", styles.dim())),
        None => {}
    }
    let hidden = body.len().saturating_sub(RESULT_ROWS);
    if !expanded && hidden > 0 {
        body.truncate(RESULT_ROWS - 1);
    }
    lines.extend(body.into_iter().map(|line| row(padded(line.spans))));
    if !expanded && hidden > 0 {
        lines.push(chrome(row(padded(vec![Span::styled(
            format!("… {} more lines", hidden + 1),
            styles.dim(),
        )]))));
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTICE: &str = r#"{"source":{"agent_id":"j9c4znd0fkvb5qbz70be","session_id":"js4v6g27e0es9cmaksxk"},"task_message_id":"arkb17trvgsdedkfgmm9","origin":{},"inputs":[],"discarded_inputs":[],"status":"ended","results":[{"turn_id":"88zq7v2z2k0vd0avat3m","outcome":"completed","persistence":"persisted","fault":null,"persistence_fault":null,"output":["已新增 docs/API.md。\n\n检查结果：23 项通过。"],"partials":[]}]}"#;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_result_shows_outcome_and_answer_without_identifiers() {
        let rows = text(
            &render(
                NOTICE,
                None,
                Styles::of(super::super::theme::Palette::dark()),
                60,
                false,
            )
            .unwrap(),
        );
        assert!(rows[0].contains("Delegated result · Session js4v6g · completed"));
        assert!(rows.iter().any(|row| row.contains("已新增 docs/API.md。")));
        assert!(rows.iter().any(|row| row.contains("检查结果：23 项通过。")));
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("task_message_id") || row.contains("\\n"))
        );
        assert!(is_result(NOTICE));
        assert!(!is_result("please review src/http.js"));
    }
}
