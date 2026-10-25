//! The context sheet's report lines and plain text.

use ratatui::text::Line;

use crate::app::Session;
use crate::protocol::{CompactStatus, SessionContext, SessionPresentation};

use super::styled;
use crate::ui::format_tokens;
use crate::ui::theme::Styles;

/// The summary lines of the context sheet.
pub fn lines(session: Option<&Session>, styles: Styles) -> Vec<Line<'static>> {
    summary_lines(
        session.and_then(|session| session.context.as_ref()),
        session.and_then(|session| session.presentation.as_ref()),
        styles,
    )
}

/// The context report as plain text, for `c`.
pub fn text(session: Option<&Session>, styles: Styles) -> String {
    let lines = lines(session, styles);
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

pub use crate::app::session::context_percent as percent;
use crate::app::session::input_budget;

/// Labels estimates as estimates and never prints an unknown number as zero.
fn summary_lines(
    context: Option<&SessionContext>,
    presentation: Option<&SessionPresentation>,
    styles: Styles,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let usage = presentation.and_then(|view| view.context.as_ref());

    let reported = usage.and_then(|usage| {
        let tokens = usage.tokens?;
        let window = usage.window.filter(|window| *window > 0);
        let line = match (input_budget(context), window) {
            (Some(budget), window) => {
                let percent = tokens as f64 / budget as f64 * 100.0;
                let window = window
                    .filter(|window| *window != budget)
                    .map_or(String::new(), |window| {
                        format!(" · model window {}", format_tokens(window))
                    });
                format!(
                    "Context: {} of {} input budget ({percent:.0}%){window} — reported by the last request",
                    format_tokens(tokens),
                    format_tokens(budget)
                )
            }
            (None, Some(window)) => {
                let percent = usage
                    .percent
                    .unwrap_or(tokens as f64 / window as f64 * 100.0);
                format!(
                    "Context: {} of {} tokens ({percent:.0}%) — reported by the last request",
                    format_tokens(tokens),
                    format_tokens(window)
                )
            }
            (None, None) => return None,
        };
        Some(line)
    });
    let budget = context.and_then(|context| context.budget.as_ref());
    let estimated = budget.and_then(|budget| {
        // The request estimate is the last request's and goes stale once a
        // compaction shrinks the history, so only the history estimate is shown.
        let used = budget.estimated_history_tokens?;
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
                format!(
                    " ({:.0}% of the input budget)",
                    trigger as f64 / limit as f64 * 100.0
                )
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
                Some(origin @ ("automatic" | "manual")) => format!(" ({origin})"),
                _ => String::new(),
            };
            // Coverage is the Summary line's; equal figures say nothing.
            let detail = match (last.before_tokens, last.after_tokens) {
                (Some(before), Some(after))
                    if last.status == CompactStatus::Compacted
                        && format_tokens(before) != format_tokens(after) =>
                {
                    format!(
                        "{} → {} tokens",
                        format_tokens(before),
                        format_tokens(after)
                    )
                }
                _ => compact_status(last.status).to_owned(),
            };
            // An automatic check that found nothing to do is not a compaction;
            // the summary line below says what the last real one covered.
            let label = match (last.status, last.origin.as_deref()) {
                (CompactStatus::Noop, Some("automatic")) => "Last automatic check".to_owned(),
                _ => format!("Last compaction{origin}"),
            };
            lines.push(styled(&format!("{label}: {detail}"), styles.dim()));
        }
        if let Some(last) = context
            .automatic
            .as_ref()
            .and_then(|view| view.last.as_ref())
        {
            let detail = match (last.before_tokens, last.after_tokens) {
                (Some(before), Some(after)) if last.outcome == "compacted" => {
                    format!(
                        "{} → {} tokens",
                        format_tokens(before),
                        format_tokens(after)
                    )
                }
                _ => last.outcome.replace('_', " "),
            };
            lines.push(styled(
                &format!("Last compaction between requests: {detail}"),
                styles.dim(),
            ));
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
        assert!(
            text[0].contains("31.2k of 40k input budget (78%)"),
            "{text:?}"
        );
        assert!(text[0].contains("reported"), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.contains("Auto-compacts at 32k tokens (80% of the input budget)")),
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
    fn a_compaction_that_changed_nothing_shows_its_status_not_coverage() {
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "coverage": {"covered_loop_count": 6, "covered_item_count": 379, "retained_item_count": 0},
            "last_result": {"operation_id": "auto-1", "origin": "automatic", "status": "compacted",
                            "before_tokens": 14812, "after_tokens": 14790,
                            "covered_loop_count": 0, "covered_item_count": 379,
                            "retained_item_count": 0}
        }))
        .unwrap();
        let text: Vec<String> = summary_lines(Some(&context), None, styles)
            .iter()
            .map(Line::to_string)
            .collect();
        assert!(
            text.contains(&"Last compaction (automatic): compacted".to_owned()),
            "{text:?}"
        );
        assert!(text.iter().all(|line| !line.contains('→')), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line == "Summary covers 6 turns · 379 items · 0 kept"),
            "{text:?}"
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

    fn lines_of(lines: Vec<Line<'static>>) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    #[test]
    fn the_footer_percent_is_of_the_input_budget_the_agent_reports() {
        let mut session = Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
        );
        session.presentation = Some(
            serde_json::from_value(json!({
                "session_id": "s",
                "context": {"tokens": 18000, "window": 64000, "percent": 28.1, "kind": "reported"}
            }))
            .unwrap(),
        );
        assert_eq!(percent(&session), Some(28.1), "no budget yet: the window");
        session.context = Some(
            serde_json::from_value(json!({
                "session_id": "s",
                "budget": {"input_budget_tokens": 40000, "trigger_tokens": 32000}
            }))
            .unwrap(),
        );
        assert_eq!(percent(&session), Some(45.0));
        let text = lines_of(lines(Some(&session), Styles::of(Palette::dark())));
        assert!(
            text[0].starts_with("Context: 18k of 40k input budget (45%) · model window 64k"),
            "{text:?}"
        );
    }

    #[test]
    fn an_automatic_check_that_found_nothing_is_not_called_a_compaction() {
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "coverage": {"covered_loop_count": 5, "covered_item_count": 64, "retained_item_count": 0},
            "last_result": {"operation_id": "auto-2", "origin": "automatic", "status": "noop"}
        }))
        .unwrap();
        let text = lines_of(summary_lines(
            Some(&context),
            None,
            Styles::of(Palette::dark()),
        ));
        assert!(
            text.contains(&"Last automatic check: nothing to compact".to_owned()),
            "{text:?}"
        );
        assert!(
            text.contains(&"Summary covers 5 turns · 64 items · 0 kept".to_owned()),
            "{text:?}"
        );
    }

    #[test]
    fn a_compaction_between_requests_is_listed_beside_the_last_check() {
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-2", "origin": "automatic", "status": "noop"},
            "automatic": {"last": {"operation_id": "auto-boundary-l1-1", "before_tokens": 35300,
                                   "after_tokens": 8000, "outcome": "compacted"}}
        }))
        .unwrap();
        let text = lines_of(summary_lines(
            Some(&context),
            None,
            Styles::of(Palette::dark()),
        ));
        assert!(
            text.contains(&"Last compaction between requests: 35.3k → 8k tokens".to_owned()),
            "{text:?}"
        );
    }
}
