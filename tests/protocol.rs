//! Wire-contract tests for the Agent v0.3 protocol.

use minicore_tui::protocol::{
    HistoryItemWire, IncomingFrame, METHOD_PING, Reasoning, RpcNotification, SessionStatusWire,
    TurnPersistenceWire, parse_frame,
};

fn fixture(name: &str) -> IncomingFrame {
    let path = format!(
        "{}/tests/fixtures/protocol/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("missing fixture {name}: {error}"));
    parse_frame(&bytes).unwrap_or_else(|error| panic!("fixture {name} is invalid: {error}"))
}

fn response(name: &str) -> minicore_tui::protocol::RpcResponse {
    match fixture(name) {
        IncomingFrame::Response(response) => response,
        other => panic!("fixture {name} is not a response: {other:?}"),
    }
}

#[test]
fn discovery_fixtures_decode_real_agent_shapes() {
    let models = response("model-list.json").parse_models().unwrap().models;
    assert_eq!(models[0].id, "gpt-4o");
    assert_eq!(
        models[0].supported_reasoning,
        vec![
            Reasoning::Auto,
            Reasoning::Disabled,
            Reasoning::Low,
            Reasoning::Medium,
            Reasoning::High,
        ]
    );

    let profiles = response("profile-list.json")
        .parse_profiles()
        .unwrap()
        .profiles;
    assert_eq!(profiles[0].id, "coding");
    assert_eq!(profiles[0].reasoning, Reasoning::High);

    let session = response("session-create.json")
        .parse_session()
        .unwrap()
        .session;
    assert_eq!(session.session_id, "ses_6f3c1a");
    assert_eq!(session.workspace, "/srv/vaults/demo-01");
}

#[test]
fn session_state_uses_an_active_loop_object() {
    let state = response("session-state.json")
        .parse_session_state()
        .unwrap();
    assert_eq!(state.session_id, "ses_6f3c1a");
    assert_eq!(state.status, SessionStatusWire::Running);
    assert_eq!(state.active_loop.unwrap().loop_id, "loop_77aa");
}

#[test]
fn history_fixture_decodes_contiguous_indexed_items() {
    let page = response("history-page.json").parse_history().unwrap();
    assert_eq!(page.next_offset, None);
    assert_eq!(page.total, 4);
    assert_eq!(
        page.items.iter().map(|item| item.index).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert!(matches!(page.items[0].item, HistoryItemWire::User(_)));
    assert!(matches!(page.items[1].item, HistoryItemWire::Assistant(_)));
    assert!(matches!(page.items[2].item, HistoryItemWire::ToolResult(_)));
    assert!(matches!(page.items[3].item, HistoryItemWire::Assistant(_)));
}

#[test]
fn event_fixtures_keep_request_index_and_tool_outcome() {
    for name in [
        "output-delta.json",
        "tool-started.json",
        "tool-finished.json",
    ] {
        assert!(matches!(
            fixture(name),
            IncomingFrame::Notification(RpcNotification::AgentEvent(_))
        ));
    }
    let event = match fixture("tool-finished.json") {
        IncomingFrame::Notification(RpcNotification::AgentEvent(event)) => event,
        _ => unreachable!(),
    };
    let minicore_tui::protocol::AgentEventWire::ToolFinished { data } = event else {
        panic!("expected tool_finished")
    };
    assert_eq!(data.request_index, 0);
    assert_eq!(
        data.result.outcome,
        minicore_tui::protocol::ToolOutcomeWire::Success
    );
}

#[test]
fn turn_wait_is_a_direct_turn_result_view() {
    let result = response("turn-wait.json").parse_turn_wait().unwrap();
    assert_eq!(result.turn.loop_id, "loop_77aa");
    assert_eq!(result.requests, Some(1));
    assert_eq!(result.persistence, Some(TurnPersistenceWire::Persisted));
}

#[test]
fn unknown_reasoning_is_rejected_but_unknown_read_only_fields_are_ignored() {
    let original = std::fs::read_to_string(format!(
        "{}/tests/fixtures/protocol/model-list.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
    value["result"]["models"][0]["new_read_only_field"] = serde_json::json!(true);
    let frame = parse_frame(serde_json::to_string(&value).unwrap().as_bytes()).unwrap();
    let IncomingFrame::Response(response) = frame else {
        panic!("expected response")
    };
    assert_eq!(response.parse_models().unwrap().models.len(), 2);

    value["result"]["models"][0]["supported_reasoning"][0] = serde_json::json!("turbo");
    let frame = parse_frame(serde_json::to_string(&value).unwrap().as_bytes()).unwrap();
    let IncomingFrame::Response(response) = frame else {
        panic!("expected response")
    };
    assert!(response.parse_models().is_err());
}

#[test]
fn ping_builder_matches_json_rpc_shape() {
    let request =
        minicore_tui::protocol::OutgoingRequest::ping(minicore_tui::protocol::RequestId(1));
    assert_eq!(request.method, METHOD_PING);
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"jsonrpc":"2.0","id":1,"method":"agent.ping","params":{}}"#
    );
}

#[test]
fn unknown_fields_and_usage_defaults_and_outcome_tolerance() {
    // 1. ToolOutcomeWire accepts unknown future outcomes as Unknown
    let outcome_json = r#""custom_provider_outcome""#;
    let outcome: minicore_tui::protocol::ToolOutcomeWire =
        serde_json::from_str(outcome_json).unwrap();
    assert_eq!(outcome, minicore_tui::protocol::ToolOutcomeWire::Unknown);

    // 2. CancelReasonWire accepts unknown future reasons as Unknown(String)
    let cancel_json = r#""sandbox_oom_killed""#;
    let cancel: minicore_tui::protocol::CancelReasonWire =
        serde_json::from_str(cancel_json).unwrap();
    assert_eq!(
        cancel,
        minicore_tui::protocol::CancelReasonWire::Unknown("sandbox_oom_killed".to_string())
    );

    // 3. UsageWire handles completely omitted fields with default None
    let usage_json = "{}";
    let usage: minicore_tui::protocol::UsageWire = serde_json::from_str(usage_json).unwrap();
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.reasoning_tokens, None);
    assert_eq!(usage.cache_read_tokens, None);
    assert_eq!(usage.cache_write_tokens, None);
    assert_eq!(usage.provider_total_tokens, None);

    // 4. TurnResultViewWire with extra unknown fields and missing usage fields decodes cleanly
    let result_json = serde_json::json!({
        "turn": { "session_id": "ses_1", "loop_id": "loop_1" },
        "status": "completed",
        "outcome": { "type": "completed" },
        "requests": 1,
        "tool_rounds": 0,
        "final_config_revision": 1,
        "persistence": "persisted",
        "future_field_not_in_spec": { "foo": "bar" },
        "usage": { "input_tokens": 42 }
    });
    let result: minicore_tui::protocol::TurnResultViewWire =
        serde_json::from_value(result_json).unwrap();
    assert_eq!(result.turn.loop_id, "loop_1");
    assert_eq!(result.requests, Some(1));
    assert_eq!(result.usage.as_ref().unwrap().input_tokens, Some(42));
    assert_eq!(result.usage.as_ref().unwrap().output_tokens, None);
}

#[test]
fn local_budget_failure_is_optional_numeric_and_round_trips() {
    use minicore_tui::protocol::ModelErrorWire;
    for field in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!({
            "estimated_tokens": 0, "input_budget_tokens": u64::MAX
        })),
    ] {
        let mut wire = serde_json::json!({"kind":"context_length_exceeded", "delivery":"not_sent", "retryable":false});
        if let Some(field) = field {
            wire["local_context_budget"] = field;
        }
        let parsed: ModelErrorWire = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            parsed.local_context_budget.is_some(),
            wire["local_context_budget"].is_object()
        );
        if let Some(budget) = &parsed.local_context_budget {
            assert_eq!(budget.estimated_tokens, 0);
            assert_eq!(budget.input_budget_tokens, u64::MAX);
        }
        assert_eq!(
            serde_json::from_value::<ModelErrorWire>(serde_json::to_value(&parsed).unwrap())
                .unwrap(),
            parsed
        );
    }
}

#[test]
fn compaction_origin_is_optional_and_explicit_origin_wins_over_id() {
    use minicore_tui::protocol::CompactResultWire;
    for (id, origin, expected) in [
        ("auto-old", None, "automatic"),
        ("old-manual", None, "unknown"),
        ("auto-explicit", Some("manual"), "manual"),
        ("new-id", Some("automatic"), "automatic"),
        ("auto-new", Some("future_origin"), "unknown"),
    ] {
        let result: CompactResultWire = serde_json::from_value(serde_json::json!({
            "operation_id":id, "status":"compacted", "origin":origin,
            "summary":"must not be retained", "encrypted_content":"must not be retained"
        }))
        .unwrap();
        assert_eq!(result.origin_label(), expected);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("must not be retained")
        );
    }
}

#[test]
fn argument_preview_wire_is_additive_and_debug_redacts_content() {
    use minicore_tui::protocol::{
        AgentEventWire, REQUIRED_CAPABILITIES, ToolArgumentsPreviewStateWire,
    };
    assert!(!REQUIRED_CAPABILITIES.contains(&"tool.arguments.preview"));
    let frame = serde_json::json!({"jsonrpc":"2.0","method":"agent.event","params":{"type":"tool_arguments_preview","data":{
        "turn":{"session_id":"session","loop_id":"loop"},"request_index":2,"tool_call_id":"call","tool_name":"write",
        "attempt":18446744073709551615u64,"revision":18446744073709551615u64,"state":"generated","partial":true,
        "display":{"detail":"PRIVATE_PATH","expanded_input":"PRIVATE_BODY","body_truncated":true},
        "meta":{"session_id":"session","loop_id":"loop","dropped_before":3}}}});
    let decoded = parse_frame(&serde_json::to_vec(&frame).unwrap()).unwrap();
    let IncomingFrame::Notification(RpcNotification::AgentEvent(
        AgentEventWire::ToolArgumentsPreview { data },
    )) = decoded
    else {
        panic!("expected preview");
    };
    assert_eq!(data.attempt, u64::MAX);
    assert_eq!(data.revision, u64::MAX);
    assert_eq!(data.state, ToolArgumentsPreviewStateWire::Generated);
    assert_eq!(data.request_index, 2);
    assert!(data.partial && data.display.body_truncated);
    let debug = format!("{data:?}");
    assert!(!debug.contains("PRIVATE_PATH"));
    assert!(!debug.contains("PRIVATE_BODY"));
    let mut malformed = frame;
    malformed["params"]["data"]["state"] = serde_json::json!("validated");
    assert!(parse_frame(&serde_json::to_vec(&malformed).unwrap()).is_err());
}

#[test]
fn preview_capability_is_optional_for_old_agent_handshake() {
    use minicore_tui::protocol::{PingResult, REQUIRED_CAPABILITIES, validate_backend};
    let mut ping: PingResult = serde_json::from_value(serde_json::json!({"version":"0.6.2","protocol_version":1,"capabilities":REQUIRED_CAPABILITIES})).unwrap();
    assert_eq!(validate_backend(&ping), Ok(()));
    ping.capabilities.push("tool.arguments.preview".into());
    assert_eq!(validate_backend(&ping), Ok(()));
}

#[test]
fn every_known_event_exposes_its_own_meta_and_unknown_has_none() {
    use minicore_tui::protocol::{AgentEventWire, EventMetaWire};
    use serde_json::json;

    let session = response("session-create.json")
        .parse_session()
        .unwrap()
        .session;
    // Shared surplus fields are ignored by the DTOs. Envelope identifiers
    // deliberately differ from the turn/session payload to pin the accessor.
    let data = json!({
        "session": session, "session_id": "payload-session",
        "state": {"session_id":"payload-session", "status":"idle", "active_loop":null, "block_reason":null},
        "turn": {"session_id":"payload-session", "loop_id":"payload-loop"},
        "request_index":2, "config_revision":3, "model":"fixture", "reasoning":"auto",
        "usage":{}, "applied_count":4, "channel":"reasoning", "delta":"delta",
        "tool_call_id":"call", "tool_name":"write", "attempt":5, "revision":6, "partial":false,
        "display":{"detail":"file.rs"}, "progress":{"message":null,"completed":null,"total":null},
        "result":{"outcome":"success","content":"ok","content_bytes":2,"content_truncated":false},
        "interaction":{"interaction_id":"interaction", "tool_call_id":"call", "tool_name":"write", "kind":{}},
        "interaction_id":"interaction", "outcome":{"type":"completed"}, "persistence":"persisted"
    });
    for (index, kind) in [
        "session_opened",
        "session_closed",
        "session_state",
        "turn_started",
        "request_started",
        "request_usage",
        "steer_progress",
        "output_delta",
        "tool_arguments_preview",
        "tool_started",
        "tool_presentation",
        "tool_progress",
        "tool_invocation",
        "tool_execution",
        "tool_process",
        "tool_finished",
        "interaction_requested",
        "interaction_resolved",
        "turn_finished",
    ]
    .into_iter()
    .enumerate()
    {
        let mut data = data.clone();
        if kind == "tool_arguments_preview" {
            data["state"] = json!("generating");
        }
        if matches!(kind, "tool_invocation" | "tool_execution" | "tool_process") {
            let path = format!(
                "{}/tests/fixtures/agent-v1/event-{}.json",
                env!("CARGO_MANIFEST_DIR"),
                kind.replace('_', "-")
            );
            let fact: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            data["data"] = fact["notification"]["data"]["data"].clone();
        }
        let expected = EventMetaWire {
            session_id: format!("meta-session-{index}"),
            loop_id: (index % 2 == 0).then(|| format!("meta-loop-{index}")),
            dropped_before: index as u64 + 1,
        };
        data["meta"] = serde_json::to_value(&expected).unwrap();
        let frame =
            json!({"jsonrpc":"2.0","method":"agent.event","params":{"type":kind,"data":data}});
        let IncomingFrame::Notification(RpcNotification::AgentEvent(event)) =
            parse_frame(&serde_json::to_vec(&frame).unwrap()).unwrap_or_else(|_| {
                panic!(
                    "invalid {kind} fixture: {:?}",
                    serde_json::from_value::<AgentEventWire>(frame["params"].clone()).unwrap_err()
                )
            })
        else {
            panic!("not an agent event: {kind}");
        };
        assert_eq!(event.meta(), Some(&expected), "{kind}");
        assert!(
            std::ptr::eq(event.meta().unwrap(), event.meta().unwrap()),
            "borrowed metadata"
        );
    }
    let unknown: AgentEventWire = serde_json::from_value(json!({
        "type":"future_event", "data":{"meta":{"session_id":"ignored", "dropped_before":99}}
    }))
    .unwrap();
    assert_eq!(unknown.meta(), None);
}
