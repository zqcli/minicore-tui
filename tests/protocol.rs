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
    assert_eq!(result.requests, 1);
    assert_eq!(result.persistence, TurnPersistenceWire::Persisted);
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

    value["result"]["models"][0]["supported_reasoning"][0] = serde_json::json!("ultra");
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
    assert_eq!(result.requests, 1);
    assert_eq!(result.usage.input_tokens, Some(42));
    assert_eq!(result.usage.output_tokens, None);
}
