//! Fetch an existing Bash lifecycle timestamp once after real process start.
//! This is a metadata read through the shared Tool query slot, never an output
//! fetch, a render-time start clock, or a second execution path.

use super::queries::{QueryAdmission, QueryKey, QuerySlots};
use super::{App, AppCommand, RequestKind};
use crate::protocol::{CommandStatusWire, OutgoingRequest, RpcResponse, ToolExecutionStateWire};
use crate::state::tool::{ToolFacts, ToolKey};

fn needs_timing(facts: &ToolFacts) -> bool {
    facts.timing_read_epoch.is_none()
        && !facts.is_terminal()
        && facts
            .invocation
            .as_ref()
            .is_some_and(|invocation| invocation.name == "bash")
        && facts.command.as_ref().is_some_and(|command| {
            matches!(
                command.status,
                CommandStatusWire::Running | CommandStatusWire::Cancelling
            )
        })
        && facts.execution.as_ref().is_none_or(|execution| {
            execution
                .started_at
                .as_deref()
                .and_then(crate::state::selection::parse_rfc3339)
                .is_none()
        })
}

impl App {
    pub(super) fn poll_bash_timing_reads(&mut self) -> Vec<AppCommand> {
        if !self.can_send_requests()
            || self.reload.is_some()
            || self.queries.in_flight_len() >= QuerySlots::CAPACITY
            || self.queries.waiting_len() > 0
        {
            return Vec::new();
        }
        let Some(view) = self.active_view() else {
            return Vec::new();
        };
        if !view.info.loaded || view.closing || view.browsing {
            return Vec::new();
        }
        let epoch = view.session_epoch;
        let keys: Vec<_> = view
            .tool_presentations
            .iter()
            .filter(|(_, facts)| needs_timing(facts))
            .map(|(key, _)| key.clone())
            .collect();
        let mut commands = Vec::new();
        for key in keys {
            if self.deferred_pending() + self.queries.in_flight_len()
                >= super::MAX_DEFERRED_REQUESTS
                || self.queries.in_flight_len() >= QuerySlots::CAPACITY
            {
                break;
            }
            let query = QueryKey::Tool { key: key.clone() };
            // A detail/inline query can supply the same metadata. Do not mark
            // it for an extra refresh just because more process chunks arrive.
            if self.queries.contains_or_queued(&query) {
                continue;
            }
            let id = self.next_request_id();
            if self.queries.request_query(query, id) != QueryAdmission::Admitted {
                continue;
            }
            self.tool_facts_mut(&key, "bash").unwrap().timing_read_epoch = Some(epoch);
            let request = OutgoingRequest::tool_read(id, &(&key).into());
            self.pending_requests
                .insert(id, RequestKind::ToolTiming { key, epoch });
            commands.push(AppCommand::Rpc(request));
        }
        commands
    }

    pub(super) fn on_bash_timing_response(
        &mut self,
        key: ToolKey,
        epoch: u64,
        response: &RpcResponse,
    ) {
        let Some(facts) = self
            .sessions
            .known
            .get(&key.session_id)
            .filter(|view| view.session_epoch == epoch)
            .and_then(|view| view.tool_presentations.get(&key))
        else {
            return;
        };
        if facts.timing_read_epoch != Some(epoch) {
            return;
        }
        let Ok(read) = response.parse_tool_read() else {
            return;
        };
        if ToolKey::from(&read.execution.tool_ref) != key
            || read.execution.name != "bash"
            || read.invocation.as_ref().is_some_and(|invocation| {
                ToolKey::from(&invocation.tool_ref) != key || invocation.name != "bash"
            })
        {
            return;
        }
        let mut execution = if let Some(current) = facts.execution.as_ref() {
            // A live/terminal event may have beaten the read response. Only
            // fill its missing start; never roll its lifecycle facts backward.
            if current
                .started_at
                .as_deref()
                .and_then(crate::state::selection::parse_rfc3339)
                .is_some()
            {
                return;
            }
            let mut current = (**current).clone();
            current.started_at = read.execution.started_at;
            // An earlier policy read can still say Requested/AwaitingPolicy.
            // The real process event plus this authority snapshot may advance
            // that state, but never overwrite a later running/terminal state.
            if matches!(
                current.state,
                ToolExecutionStateWire::Requested | ToolExecutionStateWire::AwaitingPolicy
            ) && matches!(
                read.execution.state,
                ToolExecutionStateWire::Running | ToolExecutionStateWire::Cancelling
            ) {
                current.state = read.execution.state;
            }
            current
        } else {
            read.execution
        };
        // Process events have their own monotonic stream owner and may be newer
        // than this read snapshot. The timing read must not replace that owner.
        execution.command = None;
        self.accept_tool_execution(execution, true);
        if self.sessions.active.as_ref() == Some(&key.session_id) {
            self.prepared_conversation = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AppEvent;
    use crate::protocol::{RequestId, ToolRefWire};
    use crate::state::tool::ToolStatus;
    use crate::ui::testapp::{self, respond, take_requests};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn fixture() -> (App, ToolKey) {
        let mut app = testapp::open_empty(crate::theme::ThemeKind::Dark, "ses_1", None, "high");
        let key = ToolKey::new("ses_1", "loop", 0, "timed");
        let read = read(&key);
        let facts = app.tool_facts_mut(&key, "bash").unwrap();
        facts.accept_started("bash");
        facts.invocation = Some(Arc::new(
            serde_json::from_value(read["invocation"].clone()).unwrap(),
        ));
        (app, key)
    }

    fn read(key: &ToolKey) -> Value {
        let value: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent-v1/tool-read-running.json"
        ))
        .unwrap();
        let mut read = value["result"].clone();
        read["execution"]["tool_ref"] = json!(ToolRefWire::from(key));
        read["invocation"]["tool_ref"] = json!(ToolRefWire::from(key));
        read
    }

    fn running(app: &mut App, key: &ToolKey) {
        app.accept_tool_process(crate::protocol::ToolProcessWire {
            tool_ref: key.into(),
            chunk: None,
            command: Some(
                serde_json::from_value(read(key)["execution"]["command"].clone()).unwrap(),
            ),
        });
    }

    fn begin(app: &mut App, key: &ToolKey) -> OutgoingRequest {
        running(app, key);
        let requests = take_requests(app.poll_bash_timing_reads());
        assert_eq!(requests.len(), 1);
        requests.into_iter().next().unwrap()
    }

    #[test]
    fn bash_timing_read_waits_for_actual_process_and_is_one_metadata_read() {
        let (mut app, key) = fixture();
        assert!(
            app.poll_bash_timing_reads().is_empty(),
            "policy invocation alone is not process start"
        );
        let request = begin(&mut app, &key);
        assert_eq!(request.method, "tool.read");
        assert!(request.params["max_bytes"].as_u64().unwrap() <= 262_144);
        assert!(request.params.get("display").is_none());
        assert_eq!(app.queries.in_flight_len(), 1);
        for _ in 0..20 {
            running(&mut app, &key);
            assert!(app.poll_bash_timing_reads().is_empty());
        }
        assert_eq!(app.queries.waiting_len(), 0);
        assert!(
            !app.queries
                .take_refresh(&QueryKey::Tool { key: key.clone() })
        );
        assert!(take_requests(respond(&mut app, &request, read(&key))).is_empty());
        let facts = &app.active_view().unwrap().tool_presentations[&key];
        assert!(facts.timing.is_some_and(|timing| timing.running));
        assert!(facts.inline.is_none() && facts.result.is_none());
        assert_eq!(app.queries.in_flight_len(), 0);
        assert!(app.poll_bash_timing_reads().is_empty());
    }

    #[test]
    fn bash_timing_read_waits_for_shared_capacity_without_fifo_or_refresh_intents() {
        let (mut app, key) = fixture();
        running(&mut app, &key);
        let same = QueryKey::Tool { key: key.clone() };
        app.queries.request_query(same.clone(), RequestId(8000));
        for _ in 0..4 {
            assert!(app.poll_bash_timing_reads().is_empty());
        }
        assert!(!app.queries.take_refresh(&same));
        assert!(
            app.active_view().unwrap().tool_presentations[&key]
                .timing_read_epoch
                .is_none()
        );
        app.free_query_slot(RequestId(8000));
        for id in [8001, 8002] {
            app.queries.request_query(
                QueryKey::WorkspaceStatus {
                    session_id: format!("s{id}"),
                },
                RequestId(id),
            );
        }
        for _ in 0..4 {
            assert!(app.poll_bash_timing_reads().is_empty());
        }
        assert_eq!(app.queries.waiting_len(), 0);
        assert!(
            app.active_view().unwrap().tool_presentations[&key]
                .timing_read_epoch
                .is_none()
        );
        app.free_query_slot(RequestId(8001));
        assert_eq!(take_requests(app.poll_bash_timing_reads()).len(), 1);
        assert_eq!(app.queries.in_flight_len(), 2);
    }

    #[test]
    fn bash_timing_read_rejects_wrong_identity_and_names_without_repeat_reads() {
        for field in [
            "execution_ref",
            "invocation_ref",
            "execution_name",
            "invocation_name",
            "missing_start",
            "malformed",
        ] {
            let (mut app, key) = fixture();
            let request = begin(&mut app, &key);
            let mut response = read(&key);
            match field {
                "execution_ref" => {
                    response["execution"]["tool_ref"]["tool_call_id"] = json!("wrong")
                }
                "invocation_ref" => {
                    response["invocation"]["tool_ref"]["session_id"] = json!("wrong")
                }
                "execution_name" => response["execution"]["name"] = json!("read"),
                "invocation_name" => response["invocation"]["name"] = json!("read"),
                "missing_start" => response["execution"]["started_at"] = Value::Null,
                _ => response = json!({}),
            }
            respond(&mut app, &request, response);
            assert!(
                app.active_view().unwrap().tool_presentations[&key]
                    .timing
                    .is_none(),
                "{field}"
            );
            assert_eq!(app.queries.in_flight_len(), 0);
            for _ in 0..4 {
                assert!(app.poll_bash_timing_reads().is_empty());
            }
        }
    }

    #[test]
    fn bash_timing_read_cannot_revive_finished_tool_or_regress_process_facts() {
        let (mut app, key) = fixture();
        let request = begin(&mut app, &key);
        let facts = app.tool_facts_mut(&key, "bash").unwrap();
        facts.accept_finished(crate::protocol::ToolOutcomeWire::Cancelled, None, false);
        respond(&mut app, &request, read(&key));
        let facts = &app.active_view().unwrap().tool_presentations[&key];
        assert_eq!(facts.status, ToolStatus::Cancelled);
        assert!(facts.timing.is_none());
        assert_eq!(app.queries.in_flight_len(), 0);

        let (mut app, key) = fixture();
        let request = begin(&mut app, &key);
        let command = Arc::make_mut(
            app.tool_facts_mut(&key, "bash")
                .unwrap()
                .command
                .as_mut()
                .unwrap(),
        );
        command.status = CommandStatusWire::Cancelling;
        command.stdout_observed_end = 999;
        respond(&mut app, &request, read(&key));
        let command = app.active_view().unwrap().tool_presentations[&key]
            .command
            .as_ref()
            .unwrap();
        assert_eq!(command.status, CommandStatusWire::Cancelling);
        assert_eq!(command.stdout_observed_end, 999);
    }

    #[test]
    fn bash_timing_read_advances_earlier_policy_execution_from_authority() {
        let (mut app, key) = fixture();
        let mut pending = read(&key)["execution"].clone();
        pending["state"] = json!("awaiting_policy");
        pending["started_at"] = Value::Null;
        pending["command"] = Value::Null;
        app.accept_tool_execution(serde_json::from_value(pending).unwrap(), true);
        let request = begin(&mut app, &key);
        respond(&mut app, &request, read(&key));
        let facts = &app.active_view().unwrap().tool_presentations[&key];
        assert!(facts.timing.is_some_and(|timing| timing.running));
        assert_eq!(
            facts.execution.as_ref().unwrap().state,
            ToolExecutionStateWire::Running
        );
    }

    #[test]
    fn bash_timing_read_epoch_and_session_retirement_fence_late_responses() {
        for retire in [false, true] {
            let (mut app, key) = fixture();
            let request = begin(&mut app, &key);
            if retire {
                app.retire_session_operations(&key.session_id);
            } else {
                app.active_session_mut().unwrap().session_epoch += 1;
            }
            assert_eq!(
                app.queries.in_flight_len(),
                1,
                "late RPC still owns its physical slot"
            );
            respond(&mut app, &request, read(&key));
            assert!(
                app.active_view().unwrap().tool_presentations[&key]
                    .timing
                    .is_none()
            );
            assert_eq!(app.queries.in_flight_len(), 0);
        }
        let (mut app, key) = fixture();
        let request = begin(&mut app, &key);
        app.sessions.active = None;
        respond(&mut app, &request, read(&key));
        assert!(
            app.sessions.active.is_none(),
            "a background response cannot switch the active session"
        );
        assert!(
            app.sessions.known[&key.session_id].tool_presentations[&key]
                .timing
                .is_some()
        );
        assert_eq!(app.queries.in_flight_len(), 0);
    }

    #[test]
    fn bash_timing_read_transport_failures_release_slots_without_polling_floods() {
        for queue_full in [false, true] {
            let (mut app, key) = fixture();
            let request = begin(&mut app, &key);
            let event = if queue_full {
                AppEvent::RpcQueueFull {
                    request,
                    class: crate::rpc::SendClass::Normal,
                }
            } else {
                AppEvent::RpcSendFailed {
                    id: request.id,
                    error: crate::rpc::RpcError::Closed,
                }
            };
            assert!(take_requests(app.update(event)).is_empty());
            assert_eq!(app.queries.in_flight_len(), 0);
            for _ in 0..4 {
                assert!(app.poll_bash_timing_reads().is_empty());
            }
            assert!(
                app.active_view().unwrap().tool_presentations[&key]
                    .timing
                    .is_none()
            );
        }
    }
}
