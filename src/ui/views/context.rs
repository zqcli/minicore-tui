//! Read-only Host context facts, with past response usage kept separate from context size.

use minicore_host_core::{SessionContextData, SessionContextSnapshot};
use minicore_host_protocol::{CompactionOutcome, CompactionState, PersistenceState};
use minicore_host_runtime::model::Usage;
use ratatui::text::Line;

use crate::app::Session;
use crate::ui::format_tokens;
use crate::ui::theme::Styles;

use super::styled;

pub fn lines(session: Option<&Session>, styles: Styles) -> Vec<Line<'static>> {
    report(session.and_then(|session| session.context.as_ref()), styles)
}

pub fn text(session: Option<&Session>, styles: Styles) -> String {
    lines(session, styles)
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn report(context: Option<&SessionContextSnapshot>, styles: Styles) -> Vec<Line<'static>> {
    let Some(context) = context else {
        return vec![styled("Context facts are not loaded yet.", styles.dim())];
    };
    let data = &context.data;
    let mut lines = vec![styled(
        "Exact context token usage and percentage are not reported.",
        styles.dim(),
    )];
    lines.push(styled(
        &format!("Configured model: {}", data.model),
        styles.text(),
    ));
    if let Some(active) = &data.active_turn {
        lines.push(styled(
            &format!(
                "Active model: {} · {} declared window",
                active.model.model,
                format_tokens(active.model.context_window)
            ),
            styles.text(),
        ));
    }
    match &data.history {
        Some(history) => {
            lines.push(styled(
                &format!(
                    "Native history: {} raw items · {} effective items · position {}",
                    history.raw_items, history.effective_items, history.position
                ),
                styles.text(),
            ));
            if let Some(projection) = &history.projection {
                lines.push(styled(
                    &format!(
                        "Committed summary: {} covered items · source position {}",
                        projection.covered_items, projection.source_position
                    ),
                    styles.dim(),
                ));
            }
            if let Some(response) = &history.latest_response {
                lines.push(styled(
                    &format!(
                        "Latest response: {} · {}",
                        response.model,
                        usage_text(&response.usage)
                    ),
                    styles.text(),
                ));
                lines.push(styled(
                    if response.covered_by_compaction {
                        "This past response is covered by compaction; its usage is not the current context size."
                    } else {
                        "Past response usage is not the current context size."
                    },
                    styles.dim(),
                ));
            } else {
                lines.push(styled(
                    "No complete response usage is available.",
                    styles.dim(),
                ));
            }
        }
        None => lines.push(styled(
            "Native history is not loaded; its size is unknown.",
            styles.dim(),
        )),
    }
    lines.push(styled(
        &format!(
            "Unbound inputs: {} next step · {} next turn",
            data.queued_inputs.next_step, data.queued_inputs.next_turn
        ),
        styles.dim(),
    ));
    compaction_lines(data, styles, &mut lines);
    if data.history_save.pending {
        lines.push(styled(
            if data.history_save.running {
                "History save is running."
            } else {
                "History save is pending confirmation."
            },
            styles.warning(),
        ));
    }
    if let Some(fault) = &data.history_save.fault {
        lines.push(styled(
            &format!("History save: {}", fault.message()),
            styles.failure(),
        ));
    }
    if let Some(fault) = &data.block_reason {
        lines.push(styled(
            &format!("Blocked: {}", fault.message()),
            styles.failure(),
        ));
    }
    lines
}

pub(crate) fn usage_text(usage: &Usage) -> String {
    let count = |value: Option<u64>| value.map_or_else(|| "?".to_owned(), format_tokens);
    let mut parts = vec![format!(
        "↑{} ↓{}",
        count(usage.input_tokens()),
        count(usage.output_tokens())
    )];
    if let Some(value) = usage.reasoning_tokens() {
        parts.push(format!("reasoning {}", format_tokens(value)));
    }
    if let Some(value) = usage.cache_read_tokens() {
        parts.push(format!("cache read {}", format_tokens(value)));
    }
    if let Some(value) = usage.cache_write_tokens() {
        parts.push(format!("cache write {}", format_tokens(value)));
    }
    if let Some(value) = usage.provider_total_tokens() {
        parts.push(format!("provider total {}", format_tokens(value)));
    }
    parts.join(" · ")
}

fn compaction_lines(data: &SessionContextData, styles: Styles, lines: &mut Vec<Line<'static>>) {
    let Some(activity) = &data.compaction else {
        return;
    };
    let mut change = None;
    let label = match &activity.state {
        CompactionState::Preparing {} => "preparing",
        CompactionState::Summarizing {} => "summarizing",
        CompactionState::Merging {} => "merging",
        CompactionState::Committing {
            change: value,
            persistence,
            fault,
        } => {
            change = Some(value);
            if let Some(fault) = fault {
                lines.push(styled(
                    &format!("Compaction save: {}", fault.message()),
                    styles.failure(),
                ));
            }
            match persistence {
                PersistenceState::Failed => "save failed",
                _ => "committing",
            }
        }
        CompactionState::Finished { result } => match result {
            CompactionOutcome::Compacted {
                change: value,
                persistence,
            } => {
                change = Some(value);
                match persistence {
                    PersistenceState::Persisted => "completed · saved",
                    PersistenceState::NotApplicable => "completed · save not applicable",
                    PersistenceState::Pending => "completed · save pending",
                    PersistenceState::Failed => "completed · save failed",
                }
            }
            CompactionOutcome::Unchanged => "nothing to compact",
            CompactionOutcome::Cancelled => "cancelled",
            CompactionOutcome::Failed { fault } => {
                lines.push(styled(
                    &format!("Compaction failed: {}", fault.message()),
                    styles.failure(),
                ));
                "failed"
            }
        },
    };
    lines.push(styled(
        &format!(
            "Compaction: {label}{}",
            if activity.cancellation_requested {
                " · cancellation requested"
            } else {
                ""
            }
        ),
        styles.dim(),
    ));
    if let Some(change) = change {
        lines.push(styled(
            &format!(
                "Compaction estimate: {} → {} request tokens · {} covered items",
                format_tokens(change.before_tokens),
                format_tokens(change.after_tokens),
                change.covered_items
            ),
            styles.dim(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_provider_counts_stay_unknown_instead_of_becoming_zero() {
        assert_eq!(usage_text(&Usage::default()), "↑? ↓?");
        assert_eq!(
            usage_text(&Usage::from_optional(Some(42), None, None)),
            "↑42 ↓?"
        );
    }

    #[test]
    fn provider_counts_remain_separate_instead_of_forming_a_context_estimate() {
        let usage = Usage::new(1_200, 50, 30)
            .with_cache_read_tokens(Some(400))
            .with_cache_write_tokens(Some(20))
            .with_provider_total_tokens(Some(1_280));
        assert_eq!(
            usage_text(&usage),
            "↑1.2k ↓50 · reasoning 30 · cache read 400 · cache write 20 · provider total 1.3k"
        );
    }
}
