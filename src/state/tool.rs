//! Live tool call state (spec 12.7). One `LiveTool` per tool_call_id;
//! duplicate started/progress/finished events are idempotent.

use std::sync::Arc;

use crate::protocol::ToolDisplayWire;

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct ToolKey {
    pub session_id: String,
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
}

impl ToolKey {
    pub fn new(session_id: &str, loop_id: &str, request_index: u32, tool_call_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            loop_id: loop_id.to_owned(),
            request_index,
            tool_call_id: tool_call_id.to_owned(),
        }
    }
}

/// The single semantic owner for one tool call. The presentation map owns
/// these facts; live and durable cards retain the shared result `Arc<str>`
/// projected from them instead of copying the body. `status`/`outcome` are
/// monotonic: a late `started` event cannot move a terminal fact back to
/// running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFacts {
    pub display: ToolDisplayWire,
    pub result: Option<Arc<str>>,
    pub result_truncated: bool,
    pub status: ToolStatus,
    pub outcome: Option<crate::protocol::ToolOutcomeWire>,
}

/// Compatibility name retained for existing render/source APIs. The map in
/// `SessionView` is now explicitly a `ToolKey -> ToolFacts` owner.
pub type ToolPresentationState = ToolFacts;

impl ToolFacts {
    pub fn retained_bytes(&self) -> usize {
        self.display.detail.len()
            + self.display.expanded_input.as_ref().map_or(0, String::len)
            + self.result.as_ref().map_or(0, |result| result.len())
    }

    pub fn accept_started(&mut self, name: &str) {
        self.display.detail = name.to_owned();
        if matches!(self.status, ToolStatus::Pending | ToolStatus::Running) {
            self.status = ToolStatus::Running;
        }
    }

    pub fn accept_finished(
        &mut self,
        outcome: crate::protocol::ToolOutcomeWire,
        result: Option<Arc<str>>,
        truncated: bool,
    ) {
        let same_terminal = self.is_terminal() && self.outcome == Some(outcome);
        if !self.is_terminal() {
            self.status = match outcome {
                crate::protocol::ToolOutcomeWire::Success
                | crate::protocol::ToolOutcomeWire::InputProvided => ToolStatus::Succeeded,
                crate::protocol::ToolOutcomeWire::Failed
                | crate::protocol::ToolOutcomeWire::Unknown => ToolStatus::Failed,
                crate::protocol::ToolOutcomeWire::Denied => ToolStatus::Denied,
                crate::protocol::ToolOutcomeWire::Cancelled => ToolStatus::Cancelled,
            };
            self.outcome = Some(outcome);
        }
        if result.is_some() && (self.result.is_none() || same_terminal) {
            self.result = result;
        }
        if self.display.hidden_line_count.is_none() {
            self.display.hidden_line_count = self
                .result
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(|text| text.split('\n').count());
        }
        self.result_truncated |= truncated;
        self.display.truncated |= truncated;
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            ToolStatus::Succeeded | ToolStatus::Failed | ToolStatus::Denied | ToolStatus::Cancelled
        )
    }

    pub fn truncate_to_bytes(&mut self, budget: usize) {
        let mut used = 0;
        truncate_string(&mut self.display.detail, budget, &mut used);
        if let Some(input) = &mut self.display.expanded_input {
            truncate_string(input, budget, &mut used);
        }
        if let Some(result) = &mut self.result {
            let available = budget.saturating_sub(used);
            if result.len() > available {
                let mut end = available;
                while end > 0 && !result.is_char_boundary(end) {
                    end -= 1;
                }
                *result = Arc::<str>::from(&result[..end]);
                self.result_truncated = true;
            }
        }
    }
}

fn truncate_string(value: &mut String, budget: usize, used: &mut usize) {
    let available = budget.saturating_sub(*used);
    if value.len() > available {
        let mut end = available;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    *used = (*used).saturating_add(value.len());
}

/// Tool call lifecycle as shown in the live, provisional view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTool {
    pub tool_call_id: String,
    pub name: String,
    pub status: ToolStatus,
    pub progress: Option<String>,
    /// Agent-owned bounded display data, merged by full loop/request/call
    /// identity and never used to execute a tool.
    pub display: Option<ToolDisplayWire>,
    pub result: Option<Arc<str>>,
    pub result_truncated: bool,
    pub expanded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolOutcomeWire;

    fn facts() -> ToolFacts {
        ToolFacts {
            display: ToolDisplayWire {
                detail: "tool".to_owned(),
                expanded_input: None,
                input_line_count: None,
                hidden_line_count: None,
                truncated: false,
            },
            result: None,
            result_truncated: false,
            status: ToolStatus::Pending,
            outcome: None,
        }
    }

    #[test]
    fn terminal_facts_ignore_late_started_and_conflicting_finished_events() {
        let mut facts = facts();
        let first: Arc<str> = Arc::from("first");
        let conflicting: Arc<str> = Arc::from("conflicting");
        facts.accept_finished(ToolOutcomeWire::Success, Some(first.clone()), false);
        facts.accept_started("late-name");
        facts.accept_finished(ToolOutcomeWire::Failed, Some(conflicting), true);

        assert_eq!(facts.status, ToolStatus::Succeeded);
        assert_eq!(facts.outcome, Some(ToolOutcomeWire::Success));
        assert!(Arc::ptr_eq(facts.result.as_ref().unwrap(), &first));
        assert!(facts.result_truncated);
        assert!(facts.display.truncated);
        assert_eq!(facts.display.detail, "late-name");
    }

    #[test]
    fn finished_facts_reuse_an_existing_result_owner() {
        let mut facts = facts();
        let result: Arc<str> = Arc::from("shared");
        facts.result = Some(result.clone());
        facts.accept_finished(
            ToolOutcomeWire::Success,
            Some(Arc::from("duplicate")),
            false,
        );

        assert!(Arc::ptr_eq(facts.result.as_ref().unwrap(), &result));
    }
}
