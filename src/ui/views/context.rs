//! The context view: how full the context is and what compaction did.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::Session;
use crate::protocol::{SessionContext, SessionPresentation};

use super::{chrome, styled};
use crate::ui::format_tokens;
use crate::ui::theme::Styles;
use crate::ui::wrap::wrap;

/// The context view's lines: one readable summary, not a ledger of numbers.
pub fn lines(session: Option<&Session>, styles: Styles) -> Vec<Line<'static>> {
    summary_lines(
        session.and_then(|session| session.context.as_ref()),
        session.and_then(|session| session.presentation.as_ref()),
        styles,
    )
}

/// The context view as plain text, for `c`.
pub fn text(session: Option<&Session>, styles: Styles) -> String {
    let lines = lines(session, styles);
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// How full the context is, in percent: as the Agent reported it after the
/// last request, else estimated against the input budget.
pub fn percent(session: &Session) -> Option<f64> {
    let reported = session
        .presentation
        .as_ref()
        .and_then(|presentation| presentation.context.as_ref())
        .filter(|usage| usage.tokens.is_some());
    if let Some(usage) = reported {
        return usage
            .percent
            .or_else(|| match (usage.tokens, usage.window) {
                (Some(tokens), Some(window)) if window > 0 => {
                    Some(tokens as f64 / window as f64 * 100.0)
                }
                _ => None,
            });
    }
    let budget = session.context.as_ref()?.budget.as_ref()?;
    let tokens = budget
        .estimated_request_context_tokens
        .or(budget.estimated_history_tokens)?;
    let limit = budget.input_budget_tokens.filter(|limit| *limit > 0)?;
    Some(tokens as f64 / limit as f64 * 100.0)
}

/// Builds the summary from the two authoritative views, labelling estimates
/// as estimates and keeping unknowns as `?` instead of zero.
fn summary_lines(
    context: Option<&SessionContext>,
    presentation: Option<&SessionPresentation>,
    styles: Styles,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let usage = presentation.and_then(|view| view.context.as_ref());

    let reported = usage.and_then(|usage| match (usage.tokens, usage.window) {
        (Some(tokens), Some(window)) if window > 0 => {
            let percent = usage
                .percent
                .unwrap_or(tokens as f64 / window as f64 * 100.0);
            Some(format!(
                "Context: {} of {} tokens ({percent:.0}%) — reported by the last request",
                format_tokens(tokens),
                format_tokens(window)
            ))
        }
        _ => None,
    });
    let budget = context.and_then(|context| context.budget.as_ref());
    let estimated = budget.and_then(|budget| {
        let used = budget
            .estimated_request_context_tokens
            .or(budget.estimated_history_tokens)?;
        let limit = budget.input_budget_tokens?;
        let percent = if limit > 0 {
            format!(" ({:.0}%)", used as f64 / limit as f64 * 100.0)
        } else {
            String::new()
        };
        Some(format!(
            "History estimate: {} of {} input tokens{percent}",
            format_tokens(used),
            format_tokens(limit)
        ))
    });
    match reported.or(estimated) {
        Some(line) => lines.push(styled(&line, styles.text())),
        None => lines.push(styled("Context usage is not reported yet.", styles.dim())),
    }

    if let Some(budget) = budget {
        if let (Some(trigger), Some(limit)) = (budget.trigger_tokens, budget.input_budget_tokens) {
            let percent = if limit > 0 {
                format!(" ({:.0}%)", trigger as f64 / limit as f64 * 100.0)
            } else {
                String::new()
            };
            lines.push(styled(
                &format!(
                    "Auto-compacts at {} tokens{percent}{}",
                    format_tokens(trigger),
                    match budget.target_tokens {
                        Some(target) => format!(", keeping about {}", format_tokens(target)),
                        None => String::new(),
                    }
                ),
                styles.dim(),
            ));
        } else {
            lines.push(styled("Automatic compaction is disabled.", styles.dim()));
        }
    }

    if let Some(context) = context {
        if let Some(operation) = &context.current_operation {
            lines.push(styled(
                &format!(
                    "Compacting now: {} · {}",
                    operation_label(&operation.phase),
                    phase_counts(operation)
                ),
                styles.warning(),
            ));
        }
        if let Some(last) = &context.last_result {
            let origin = match last.origin.as_deref() {
                Some("automatic") => "automatic",
                Some("manual") => "manual",
                _ => "last",
            };
            let mut summary = match (last.before_tokens, last.after_tokens) {
                (Some(before), Some(after)) => match origin {
                    "last" => format!(
                        "Last compaction: {} → {} tokens",
                        format_tokens(before),
                        format_tokens(after)
                    ),
                    other => format!(
                        "Last compaction ({other}): {} → {} tokens",
                        format_tokens(before),
                        format_tokens(after)
                    ),
                },
                _ => format!("Last compaction: {}", compact_status(last.status)),
            };
            if let Some(covered) = last.covered_loop_count {
                summary.push_str(&format!(" · covers {covered} turns"));
            }
            if let (Some(items), Some(retained)) =
                (last.covered_item_count, last.retained_item_count)
            {
                summary.push_str(&format!(" · {items} items, {retained} kept"));
            }
            lines.push(styled(&summary, styles.dim()));
        }
        if let Some(coverage) = &context.coverage {
            lines.push(styled(
                &format!(
                    "Summary covers {} turns · {} items · {} kept",
                    coverage.covered_loop_count,
                    coverage.covered_item_count,
                    coverage.retained_item_count
                ),
                styles.dim(),
            ));
        }
        if let Some(failure) = &context.last_prepare_failure {
            lines.push(styled(
                &format!("Last preparation failed: {failure}"),
                styles.failure(),
            ));
        }
    } else {
        lines.push(styled(
            "No context report is loaded for this session.",
            styles.dim(),
        ));
    }
    lines.push(Line::from(""));
    lines.push(styled(
        "Estimates are byte-based and exclude the system prompt and tool schemas.",
        styles.dim(),
    ));

    lines
}

fn phase_counts(operation: &crate::protocol::CompactionProgress) -> String {
    format!(
        "{} items prepared · {} kept",
        operation.covered_item_count, operation.retained_item_count
    )
}

fn operation_label(phase: &crate::protocol::CompactionPhase) -> &'static str {
    match phase {
        crate::protocol::CompactionPhase::Preparing => "preparing",
        crate::protocol::CompactionPhase::Summarizing => "summarizing",
        crate::protocol::CompactionPhase::Merging => "merging",
        crate::protocol::CompactionPhase::Committing => "committing",
        crate::protocol::CompactionPhase::Unknown => "working",
    }
}

fn compact_status(status: crate::protocol::CompactStatus) -> &'static str {
    match status {
        crate::protocol::CompactStatus::Compacted => "compacted",
        crate::protocol::CompactStatus::Noop => "nothing to compact",
        crate::protocol::CompactStatus::Failed => "failed",
        crate::protocol::CompactStatus::UnknownWrite => "write outcome unknown",
        crate::protocol::CompactStatus::Unknown => "unknown",
    }
}

pub fn render(frame: &mut Frame, area: Rect, styles: Styles, lines: &[Line<'static>]) {
    chrome(frame, area, styles, "← Context", "r refresh · esc back");
    let bottom = area.bottom().saturating_sub(1);
    let mut y = area.y + 1;
    for line in lines {
        if y >= bottom {
            break;
        }
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        for chunk in wrap(&text, area.width as usize) {
            if y >= bottom {
                return;
            }
            let style = line
                .spans
                .first()
                .map(|span| span.style)
                .unwrap_or_default();
            frame.buffer_mut().set_line(
                area.x,
                y,
                &Line::from(Span::styled(chunk, style)),
                area.width,
            );
            y += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::ui::theme::Palette;

    #[test]
    fn the_context_view_prefers_reported_numbers_and_labels_estimates() {
        let styles = Styles::of(Palette::dark());
        let presentation: SessionPresentation = serde_json::from_value(json!({
            "session_id": "ses_1",
            "context": {"tokens": 31200, "window": 40000, "percent": 78.0, "kind": "reported"}
        }))
        .unwrap();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "coverage": {"covered_loop_count": 6, "covered_item_count": 12, "retained_item_count": 3},
            "budget": {"estimated_history_tokens": 370, "input_budget_tokens": 40000,
                       "trigger_tokens": 32000, "target_tokens": 16000},
            "last_result": {"operation_id": "auto-1", "status": "compacted",
                            "before_tokens": 4200, "after_tokens": 900, "covered_loop_count": 6}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), Some(&presentation), styles);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(text[0].contains("31.2k of 40k tokens (78%)"), "{text:?}");
        assert!(text[0].contains("reported"), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.contains("Auto-compacts at 32k tokens (80%)")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Last compaction: 4.2k → 900 tokens")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Summary covers 6 turns")),
            "{text:?}"
        );
    }

    #[test]
    fn the_context_view_falls_back_to_estimates_and_says_so() {
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "budget": {"estimated_history_tokens": 1000, "input_budget_tokens": 4000}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(
            text.iter()
                .any(|line| line.contains("History estimate: 1k of 4k")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Estimates are byte-based")),
            "{text:?}"
        );
    }

    #[test]
    fn a_compaction_result_names_whether_it_was_automatic() {
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-1", "origin": "automatic", "status": "compacted",
                            "before_tokens": 32100, "after_tokens": 2700}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Last compaction (automatic): 32.1k → 2.7k tokens"),
            "{text}"
        );
    }

    #[test]
    fn the_context_view_never_prints_zero_for_unknown_numbers() {
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "budget": {"input_budget_tokens": 40000},
            "automatic": {}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("0 tokens"), "{text}");
        assert!(text.contains("not reported yet"), "{text}");
    }

    #[test]
    fn a_current_operation_is_shown_as_compacting_now() {
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "current_operation": {"operation_id": "auto-1", "phase": "summarizing",
                                  "covered_item_count": 12, "retained_item_count": 0}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Compacting now: summarizing"), "{text}");
    }

    #[test]
    fn a_failed_context_line_is_red() {
        let palette = Palette::dark();
        let styles = Styles::of(palette);
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_prepare_failure": "context_uncompressible"
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let failure = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("context_uncompressible"))
            })
            .expect("failure line");
        assert_eq!(failure.spans[0].style.fg, Some(palette.failure));
    }

    #[test]
    fn an_automatic_last_observation_without_numbers_does_not_panic() {
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-1", "status": "failed", "failure_kind": "no_progress"}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Last compaction: failed"), "{text}");
    }
}
