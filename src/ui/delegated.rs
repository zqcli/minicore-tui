//! A child Session's settled result as delivered to its parent. The parent
//! model receives Host's JSON notification; people read the outcome and the
//! child's final answer instead of message and Turn identifiers.

use minicore_host_protocol::Timestamp;
use ratatui::text::{Line, Span};
use serde::Deserialize;

use super::blocks::{local_zone, stamp_label};
use super::markdown;
use super::rails::chrome;
use super::theme::Styles;
use crate::wrap;

#[derive(Deserialize)]
struct Notification {
    source: Child,
    #[serde(default)]
    task_message_id: Option<String>,
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

/// What a result notification says, for the Agent card it belongs to.
pub struct Summary {
    pub session_id: String,
    pub task_message_id: Option<String>,
    pub outcome: String,
    pub fault: Option<String>,
    /// The final answer, or a partial response marked as such.
    pub answer: Option<String>,
}

pub fn summary(text: &str) -> Option<Summary> {
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
    Some(Summary {
        session_id: notification.source.session_id,
        task_message_id: notification.task_message_id,
        outcome,
        fault: last
            .and_then(|result| result.fault.as_ref())
            .map(|fault| fault.message.clone()),
        answer,
    })
}

/// One-line chronological arrival banner in the main transcript; clicking it
/// opens the `Ctrl+B` Async Tasks Inspector focused on this child Session.
pub fn render(
    text: &str,
    at: Option<Timestamp>,
    styles: Styles,
    width: usize,
    _expanded: bool,
) -> Option<Vec<Line<'static>>> {
    let Summary {
        session_id,
        outcome,
        ..
    } = summary(text)?;
    let tone = match outcome.as_str() {
        "completed" => styles.success(),
        "failed" => styles.failure(),
        _ => styles.meta(),
    };
    let short: String = session_id.chars().take(6).collect();
    let mut middle = vec![
        Span::styled("↳ ", styles.meta()),
        Span::styled(format!("agent {outcome}"), tone),
        Span::styled(format!(" · Session {short}"), styles.dim()),
    ];
    if let Some(at) = at {
        let stamp = stamp_label(at, local_zone());
        middle.push(Span::styled(format!(" · {stamp}"), styles.meta()));
    }
    middle.push(Span::styled(" · ", styles.meta()));
    middle.push(Span::styled("click or ctrl+b", styles.accent()));

    let prefix = "─ ";
    let mid_width: usize = middle.iter().map(|span| wrap::width(&span.content)).sum();
    let prefix_width = wrap::width(prefix);
    let mut spans = vec![Span::styled(prefix.to_owned(), styles.card_rail())];
    if prefix_width + mid_width >= width {
        let plain: String = middle.iter().map(|span| span.content.as_ref()).collect();
        spans.push(Span::styled(
            wrap::truncate(&plain, width.saturating_sub(prefix_width)),
            tone,
        ));
    } else {
        spans.extend(middle);
        let used = prefix_width + mid_width;
        let tail = width.saturating_sub(used + 1);
        if tail > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("─".repeat(tail), styles.card_rail()));
        }
    }
    Some(vec![chrome(Line::from(spans))])
}

/// Full scrollable detail lines for a standalone delegated result shown inside
/// the `Ctrl+B` Async Tasks Inspector when its origin dispatch call is unloaded.
pub fn detail_lines(summary: &Summary, styles: Styles, width: usize) -> Vec<Line<'static>> {
    let width = width.max(12);
    let tone = match summary.outcome.as_str() {
        "completed" => styles.success(),
        "failed" => styles.failure(),
        _ => styles.meta(),
    };
    let glyph = match summary.outcome.as_str() {
        "completed" => "✓",
        "failed" => "✗",
        "cancelled" => "⊘",
        _ => "○",
    };
    let short: String = summary.session_id.chars().take(6).collect();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} "), tone),
            Span::styled("agent", styles.tool_title()),
            Span::styled(format!(" · Session {short} · "), styles.meta()),
            Span::styled(summary.outcome.clone(), tone),
        ]),
        Line::styled(
            "Origin dispatch is in earlier unloaded history · showing delivered result",
            styles.meta(),
        ),
        Line::raw(""),
        Line::styled("Result", styles.heading()),
    ];
    if let Some(fault) = &summary.fault {
        lines.extend(wrap::wrapped(
            &wrap::sanitize(fault),
            width,
            styles.failure(),
        ));
    }
    match &summary.answer {
        Some(answer) => lines.extend(markdown::render(answer, width, &styles, styles.text())),
        None if summary.fault.is_none() => {
            lines.push(Line::styled("No final answer", styles.dim()));
        }
        None => {}
    }
    lines
}

/// Plain text copied by `Ctrl+Y` for a standalone delegated result.
pub fn copy_text(summary: &Summary) -> String {
    let mut parts = Vec::new();
    if let Some(fault) = &summary.fault {
        parts.push(fault.clone());
    }
    if let Some(answer) = &summary.answer {
        parts.push(answer.clone());
    }
    parts.join("\n\n")
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
    fn a_result_shows_a_one_line_banner_and_detail_lines_keep_answer_without_identifiers() {
        let styles = Styles::of(super::super::theme::Palette::dark());
        let rows = text(&render(NOTICE, None, styles, 72, false).unwrap());
        assert_eq!(rows.len(), 1, "arrival banner is one compact row: {rows:?}");
        assert!(rows[0].contains("agent completed · Session js4v6g"));
        assert!(rows[0].contains("click or ctrl+b"));
        assert!(!rows[0].contains("task_message_id"));

        let parsed = summary(NOTICE).unwrap();
        let detail = text(&detail_lines(&parsed, styles, 60));
        assert!(
            detail
                .iter()
                .any(|row| row.contains("已新增 docs/API.md。"))
        );
        assert!(
            detail
                .iter()
                .any(|row| row.contains("检查结果：23 项通过。"))
        );
        assert!(
            !detail
                .iter()
                .any(|row| row.contains("task_message_id") || row.contains("\\n"))
        );
        assert!(is_result(NOTICE));
        assert!(!is_result("please review src/http.js"));
    }

    #[test]
    fn cancelled_result_uses_meta_tone_not_failure_red() {
        let cancelled = r#"{"source":{"agent_id":"a","session_id":"js4v6g27e0es9cmaksxk"},"status":"ended","results":[{"outcome":"cancelled","output":[]}]}"#;
        let styles = Styles::of(super::super::theme::Palette::dark());
        let rows = render(cancelled, None, styles, 72, false).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0]
                .spans
                .iter()
                .all(|span| span.style.fg != styles.failure().fg)
        );
    }
}
