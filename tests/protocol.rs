//! Protocol fixtures against the production DTOs (`minicore_tui::protocol`).
//! Every fixture is one complete NDJSON frame, desensitized and pinned to
//! the RPC contract in `docs/rpc-contract.md` (baseline commit
//! `6d5e9630…`, agent RPC 0.2.0). The fixtures also pin the tolerance
//! contract: unknown fields parse, unknown reasoning values are rejected,
//! and no credential or real workspace path is embedded.

use minicore_tui::protocol::{
    ConversationEntryWire, IncomingFrame, METHOD_PING, Reasoning, RpcNotification, parse_frame,
};

fn fixture(name: &str) -> IncomingFrame {
    let path = format!(
        "{}/tests/fixtures/protocol/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    parse_frame(&bytes).unwrap_or_else(|e| panic!("fixture {name} is not a valid frame: {e}"))
}

fn fixture_response(name: &str) -> minicore_tui::protocol::RpcResponse {
    match fixture(name) {
        IncomingFrame::Response(response) => response,
        other => panic!("fixture {name} must be a response, got {other:?}"),
    }
}

#[test]
fn model_list_fixture_parses_with_unknown_extra_fields() {
    let response = fixture_response("model-list.json");
    let models = response.parse_models().unwrap().models;
    assert_eq!(models.len(), 2);
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
    assert_eq!(models[1].id, "fast");
    // The extra "vendor" field on the wire must not break the DTO.
}

#[test]
fn model_list_rejects_an_unknown_reasoning_value() {
    let original = std::fs::read_to_string(format!(
        "{}/tests/fixtures/protocol/model-list.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
    value["result"]["models"][0]["supported_reasoning"][1] = serde_json::json!("ultra");
    let frame = parse_frame(serde_json::to_string(&value).unwrap().as_bytes()).unwrap();
    let IncomingFrame::Response(response) = frame else {
        panic!("expected a response")
    };
    assert!(
        response.parse_models().is_err(),
        "an unknown reasoning level is a protocol error (spec 11.5)"
    );
}

#[test]
fn profile_list_fixture_parses_and_ignores_extra_result_members() {
    let response = fixture_response("profile-list.json");
    let profiles = response.parse_profiles().unwrap().profiles;
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].id, "coding");
    assert_eq!(profiles[0].reasoning, Reasoning::High);
    // The extra "default" result member is read-only and tolerated.
}

#[test]
fn session_create_and_open_fixture_parses() {
    let response = fixture_response("session-create.json");
    let session = response.parse_session().unwrap().session;
    assert_eq!(session.session_id, "ses_6f3c1a");
    assert_eq!(session.workspace, "/srv/vaults/demo-01");
    assert_eq!(session.model, "gpt-4o");
    assert!(session.loaded);
    assert_eq!(session.instance_id.as_deref(), Some("ins_9fe2"));
}

#[test]
fn session_list_fixture_parses_both_kinds_of_sessions() {
    let response = fixture_response("session-list.json");
    let sessions = response.parse_sessions().unwrap().sessions;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].title.as_deref(), Some("Refactor CLI"));
    assert_eq!(sessions[1].title, None);
    assert_eq!(sessions[1].instance_id, None);
}

#[test]
fn session_state_fixture_parses() {
    let response = fixture_response("session-state.json");
    let state = response.parse_session_state().unwrap();
    assert_eq!(state.session_id, "ses_6f3c1a");
    assert!(matches!(
        state.status,
        minicore_tui::protocol::SessionStatusWire::Running
    ));
    assert_eq!(state.active_turn.as_deref(), Some("trn_77aa"));
}

#[test]
fn transcript_page_fixture_covers_every_entry_kind() {
    let response = fixture_response("transcript-page.json");
    let page = response.parse_transcript().unwrap();
    assert!(page.complete);
    assert_eq!(page.next_after, Some(5));
    assert_eq!(page.entries.len(), 5);
    let kinds: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| match entry {
            ConversationEntryWire::UserMessage(_) => "user",
            ConversationEntryWire::AssistantMessage(_) => "assistant",
            ConversationEntryWire::ToolResult(_) => "tool",
            ConversationEntryWire::Summary(_) => "summary",
            ConversationEntryWire::TurnTerminal(_) => "terminal",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "tool", "assistant", "terminal"]
    );
}

#[test]
fn agent_event_fixtures_parse_as_notifications() {
    for name in [
        "output-delta.json",
        "tool-started.json",
        "tool-finished.json",
    ] {
        match fixture(name) {
            IncomingFrame::Notification(RpcNotification::AgentEvent(_)) => {}
            other => panic!("fixture {name} must be an agent event, got {other:?}"),
        }
    }
}

#[test]
fn turn_wait_fixture_parses_the_durable_outcome() {
    let response = fixture_response("turn-wait.json");
    let outcome = response.parse_turn_wait().unwrap();
    assert_eq!(outcome.turn_id, "trn_77aa");
    assert!(matches!(
        outcome.terminal,
        minicore_tui::protocol::TurnTerminalWire::Completed
    ));
}

#[test]
fn rpc_error_fixture_reports_a_typed_agent_error() {
    match fixture("rpc-error.json") {
        IncomingFrame::Response(response) => {
            let error = response.error.clone().expect("error present");
            assert_eq!(error.code, -32000);
            assert_eq!(error.message, "turn not found");
            let data = error.data.expect("error data present");
            assert_eq!(data.kind, "turn_not_found");
            assert!(data.retryable);
            assert!(response.result.is_none());
        }
        other => panic!("fixture rpc-error.json must be a response, got {other:?}"),
    }
}

#[test]
fn fixtures_never_embed_credentials_or_real_workspace_paths() {
    for name in [
        "model-list.json",
        "profile-list.json",
        "session-create.json",
        "session-list.json",
        "session-state.json",
        "transcript-page.json",
        "output-delta.json",
        "tool-started.json",
        "tool-finished.json",
        "turn-wait.json",
        "rpc-error.json",
    ] {
        let content = std::fs::read_to_string(format!(
            "{}/tests/fixtures/protocol/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        for secret in ["sk-", "api_key", "apikey", "bearer", "password", "secret"] {
            assert!(
                !content.to_lowercase().contains(secret),
                "fixture {name} leaks a credential pattern: {secret}"
            );
        }
        for real_path in ["/Users/", "/home/", "/root/", "C:\\"] {
            assert!(
                !content.contains(real_path),
                "fixture {name} leaks a real path"
            );
        }
    }
}

#[test]
fn ping_builder_matches_the_baseline_frame() {
    let request =
        minicore_tui::protocol::OutgoingRequest::ping(minicore_tui::protocol::RequestId(1));
    assert_eq!(request.method, METHOD_PING);
    assert_eq!(request.id, minicore_tui::protocol::RequestId(1));
    let line = serde_json::to_string(&request).unwrap();
    let expect = r#"{"jsonrpc":"2.0","id":1,"method":"agent.ping","params":{}}"#;
    assert_eq!(line, expect);
}
