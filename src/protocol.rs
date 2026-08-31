//! Local wire DTOs for the minicore-agent stdio JSON-RPC contract, pinned to
//! the fixed baseline in `docs/rpc-contract.md`. Only the shapes the TUI uses
//! are modeled. DTOs never deny unknown fields: the agent may add read-only
//! fields without breaking this client (development spec 11.1).

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// JSON-RPC version sent on every request and required on every frame.
pub const JSONRPC_VERSION: &str = "2.0";

pub const METHOD_PING: &str = "agent.ping";
pub const METHOD_LIST_MODELS: &str = "model.list";
pub const METHOD_LIST_PROFILES: &str = "profile.list";
pub const METHOD_LIST_SESSIONS: &str = "session.list";

/// Monotonic request id, starting at 1 (spec 10.4). Never persisted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// One outbound NDJSON request line (spec 10.5).
#[derive(Debug, Serialize)]
pub struct OutgoingRequest {
    #[serde(rename = "jsonrpc")]
    jsonrpc: &'static str,
    pub id: RequestId,
    pub method: &'static str,
    pub params: Value,
}

impl OutgoingRequest {
    pub fn new(id: RequestId, method: &'static str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            method,
            params,
        }
    }

    pub fn ping(id: RequestId) -> Self {
        Self::new(id, METHOD_PING, json!({}))
    }

    pub fn list_models(id: RequestId) -> Self {
        Self::new(id, METHOD_LIST_MODELS, json!({}))
    }

    pub fn list_profiles(id: RequestId) -> Self {
        Self::new(id, METHOD_LIST_PROFILES, json!({}))
    }

    pub fn list_sessions(id: RequestId) -> Self {
        Self::new(id, METHOD_LIST_SESSIONS, json!({}))
    }
}

/// A complete incoming frame (spec 10.6).
#[derive(Debug, Clone, PartialEq)]
pub enum IncomingFrame {
    Response(RpcResponse),
    Notification(RpcNotification),
}

/// Notifications are never responses. `Unknown` keeps an ignorable, typed
/// representation so a future agent notification can never be mistaken for
/// the answer to a request.
#[derive(Debug, Clone, PartialEq)]
pub enum RpcNotification {
    AgentEvent(AgentEventEnvelope),
    Unknown { method: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcResponse {
    pub id: RequestId,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// The typed result payload, or the agent error response.
    pub fn result_as<T: DeserializeOwned>(&self) -> Result<T, RpcResponseError> {
        match (&self.result, &self.error) {
            (Some(result), None) => Ok(serde_json::from_value(result.clone())?),
            (None, Some(error)) => Err(RpcResponseError::Agent(error.clone())),
            _ => Err(RpcResponseError::Malformed),
        }
    }

    pub fn parse_ping(&self) -> Result<PingResult, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_models(&self) -> Result<ModelListResult, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_profiles(&self) -> Result<ProfileListResult, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_sessions(&self) -> Result<SessionListResult, RpcResponseError> {
        self.result_as()
    }
}

/// Typed access to an `RpcResponse` payload failed.
#[derive(Debug, thiserror::Error)]
pub enum RpcResponseError {
    #[error("agent error {0}")]
    Agent(RpcError),
    #[error("malformed result payload: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("response has no result or error payload")]
    Malformed,
}

/// The agent's wire error object.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<RpcErrorData>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self
            .data
            .as_ref()
            .map(|data| data.kind.as_str())
            .unwrap_or("unknown");
        write!(f, "{} (code {}, kind {})", self.message, self.code, kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcErrorData {
    pub kind: String,
    pub retryable: bool,
}

/// The `agent.event` notification envelope. Per-event typed payloads arrive
/// with the app state phase (spec 11.7); the raw `data` value is preserved
/// here so the frame layer stays independent of the event kinds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AgentEventEnvelope {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Value,
}

/// Why an incoming line failed as a frame. Every kind is fatal for the
/// connection (spec 10.6): the client never scans ahead to recover later
/// lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameErrorKind {
    InvalidUtf8,
    InvalidJson,
    InvalidEnvelope,
    TooLarge,
    PartialFrame,
    Io,
}

impl fmt::Display for FrameErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => write!(f, "frame is not valid UTF-8"),
            Self::InvalidJson => write!(f, "frame is not valid JSON"),
            Self::InvalidEnvelope => write!(f, "frame is not a valid RPC envelope"),
            Self::TooLarge => write!(f, "frame exceeds the size limit"),
            Self::PartialFrame => write!(f, "stdout closed mid-frame"),
            Self::Io => write!(f, "pipe I/O failure"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError {
    pub kind: FrameErrorKind,
    pub detail: String,
}

impl FrameError {
    pub fn new(kind: FrameErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.detail)
    }
}

/// Parses one complete stdout line. The byte slice must not include the
/// trailing newline.
pub fn parse_frame(line: &[u8]) -> Result<IncomingFrame, FrameError> {
    let text = std::str::from_utf8(line)
        .map_err(|_| FrameError::new(FrameErrorKind::InvalidUtf8, "frame bytes are not UTF-8"))?;
    let value: Value = serde_json::from_str(text)
        .map_err(|_| FrameError::new(FrameErrorKind::InvalidJson, "frame is not valid JSON"))?;
    if !value.is_object() {
        return Err(invalid("frame is not an object"));
    }
    let envelope: Envelope = serde_json::from_value(value)
        .map_err(|_| invalid("frame envelope fields are malformed"))?;
    if envelope.jsonrpc.as_ref().and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(invalid("missing or wrong jsonrpc version"));
    }
    let method = envelope.method.as_ref().and_then(Value::as_str);
    match (envelope.id, method) {
        (Some(id), _) => parse_response(id, envelope.result, envelope.error),
        (None, Some("agent.event")) => {
            let params = envelope
                .params
                .ok_or_else(|| invalid("agent.event notification without params"))?;
            let event = serde_json::from_value::<AgentEventEnvelope>(params)
                .map_err(|_| invalid("malformed agent.event params"))?;
            Ok(IncomingFrame::Notification(RpcNotification::AgentEvent(
                event,
            )))
        }
        (None, Some(method)) => Ok(IncomingFrame::Notification(RpcNotification::Unknown {
            method: method.to_owned(),
        })),
        (None, None) => {
            if envelope.method.is_some() {
                Err(invalid("method is not a string"))
            } else {
                Err(invalid("frame has neither id nor method"))
            }
        }
    }
}

fn invalid(detail: &str) -> FrameError {
    FrameError::new(FrameErrorKind::InvalidEnvelope, detail.to_owned())
}

fn parse_response(
    id_value: Value,
    result: Option<Value>,
    error: Option<Value>,
) -> Result<IncomingFrame, FrameError> {
    let id = id_value
        .as_u64()
        .map(RequestId)
        .ok_or_else(|| invalid("response id is not an unsigned integer"))?;
    let error = match error {
        None => None,
        Some(value) => Some(parse_wire_error(value)?),
    };
    let response = match (result, error) {
        (Some(result), None) => RpcResponse {
            id,
            result: Some(result),
            error: None,
        },
        (None, Some(error)) => RpcResponse {
            id,
            result: None,
            error: Some(error),
        },
        _ => return Err(invalid("response must have exactly one of result or error")),
    };
    Ok(IncomingFrame::Response(response))
}

fn parse_wire_error(value: Value) -> Result<RpcError, FrameError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("error is not an object"))?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid("error code is missing or not an integer"))?;
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("error message is missing or not a string"))?
        .to_owned();
    let data = match object.get("data") {
        None => None,
        Some(data) => {
            let object = data
                .as_object()
                .ok_or_else(|| invalid("error data is not an object"))?;
            Some(RpcErrorData {
                kind: object
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("error data kind is missing"))?
                    .to_owned(),
                retryable: object
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("error data retryable is missing or not a bool"))?,
            })
        }
    };
    Ok(RpcError {
        code,
        message,
        data,
    })
}

/// Permissive wire envelope; field-level validation happens in `parse_frame`.
#[derive(Deserialize)]
struct Envelope {
    jsonrpc: Option<Value>,
    id: Option<Value>,
    method: Option<Value>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<Value>,
}

/// The `agent.ping` result: `{"version":"0.2.0"}` on the pinned baseline.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PingResult {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelListResult {
    pub models: Vec<ModelInfo>,
}

/// Wire shape of `model.list` entries.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub model_ref: String,
    pub context_window: u64,
    pub supports_tools: bool,
    pub supported_reasoning: Vec<Reasoning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProfileListResult {
    pub profiles: Vec<ProfileInfo>,
}

/// Wire shape of `profile.list` entries; the agent's read-only `approval`
/// field is intentionally not modeled.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProfileInfo {
    pub id: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
}

/// Wire shape of session entries (`session.list`, `session.create`,
/// `session.open`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub title: Option<String>,
    pub profile: String,
    pub workspace: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub loaded: bool,
    pub instance_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Model reasoning levels the TUI understands. Unknown wire values are a
/// protocol error (spec 11.5); the agent does not expose other levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    Auto,
    Disabled,
    Low,
    Medium,
    High,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PING_FRAME: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"agent.ping","params":{}}"#;

    fn response_with_result(id: u64, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    fn parse_value(frame: Value) -> Result<IncomingFrame, FrameError> {
        parse_frame(serde_json::to_string(&frame).unwrap().as_bytes())
    }

    #[test]
    fn ping_frame_matches_the_documented_wire_shape() {
        let bytes = serde_json::to_vec(&OutgoingRequest::ping(RequestId(1))).unwrap();
        assert_eq!(bytes, PING_FRAME);
    }

    #[test]
    fn catalog_requests_use_the_contract_method_names() {
        let frame = serde_json::to_value(OutgoingRequest::list_models(RequestId(2))).unwrap();
        assert_eq!(frame["method"], "model.list");
        assert_eq!(frame["params"], json!({}));
        let frame = serde_json::to_value(OutgoingRequest::list_profiles(RequestId(3))).unwrap();
        assert_eq!(frame["method"], "profile.list");
        let frame = serde_json::to_value(OutgoingRequest::list_sessions(RequestId(4))).unwrap();
        assert_eq!(frame["method"], "session.list");
    }

    #[test]
    fn request_ids_serialize_as_integers() {
        let frame =
            serde_json::to_value(OutgoingRequest::new(RequestId(42), METHOD_PING, json!({})))
                .unwrap();
        assert_eq!(frame["id"], json!(42));
        assert_eq!(RequestId(1), RequestId(1));
        assert!(RequestId(1) < RequestId(2));
    }

    #[test]
    fn response_with_id_is_recognized() {
        let frame = parse_value(response_with_result(7, json!({"version": "0.2.0"}))).unwrap();
        let response = match frame {
            IncomingFrame::Response(response) => response,
            _ => panic!("expected response"),
        };
        assert_eq!(response.id, RequestId(7));
        assert_eq!(response.result, Some(json!({"version": "0.2.0"})));
        assert_eq!(response.error, None);
    }

    #[test]
    fn error_response_parses_code_message_and_data() {
        let frame = parse_value(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {
                "code": -32014,
                "message": "invalid session settings",
                "data": {"kind": "invalid_session_settings", "retryable": false}
            }
        }))
        .unwrap();
        let response = match frame {
            IncomingFrame::Response(response) => response,
            _ => panic!("expected response"),
        };
        assert_eq!(response.id, RequestId(3));
        let error = response.error.as_ref().unwrap();
        assert_eq!(error.code, -32014);
        assert_eq!(error.message, "invalid session settings");
        let data = error.data.as_ref().unwrap();
        assert_eq!(data.kind, "invalid_session_settings");
        assert!(!data.retryable);
        assert!(matches!(
            response.result_as::<PingResult>(),
            Err(RpcResponseError::Agent(_))
        ));
    }

    #[test]
    fn agent_event_notification_parses_the_envelope() {
        let frame = parse_value(json!({
            "jsonrpc": "2.0",
            "method": "agent.event",
            "params": {
                "type": "output_delta",
                "data": {
                    "turn": {"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_1"},
                    "channel": "text",
                    "delta": "hi",
                    "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 2}
                }
            }
        }))
        .unwrap();
        let event = match frame {
            IncomingFrame::Notification(RpcNotification::AgentEvent(event)) => event,
            _ => panic!("expected agent event notification"),
        };
        assert_eq!(event.event_type, "output_delta");
        assert_eq!(event.data["meta"]["dropped_before"], json!(2));
    }

    #[test]
    fn unknown_notification_is_never_a_response() {
        let frame = parse_value(json!({
            "jsonrpc": "2.0",
            "method": "future.method",
            "params": {"anything": true}
        }))
        .unwrap();
        let method = match frame {
            IncomingFrame::Notification(RpcNotification::Unknown { method }) => method,
            _ => panic!("expected unknown notification"),
        };
        assert_eq!(method, "future.method");
    }

    #[test]
    fn frames_without_id_or_method_are_invalid() {
        assert!(matches!(
            parse_value(json!({"jsonrpc": "2.0"})),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
        assert!(matches!(
            parse_value(json!({"jsonrpc": "2.0", "params": {}})),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
    }

    #[test]
    fn responses_require_a_numeric_id_and_exactly_one_payload() {
        for id in [
            Value::Null,
            Value::String("abc".into()),
            json!(-1),
            json!({"x": 1}),
        ] {
            assert!(matches!(
                parse_value(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
                Err(FrameError {
                    kind: FrameErrorKind::InvalidEnvelope,
                    ..
                })
            ));
        }
        assert!(matches!(
            parse_value(json!({"jsonrpc": "2.0", "id": 1})),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
        assert!(matches!(
            parse_value(
                json!({"jsonrpc": "2.0", "id": 1, "result": {}, "error": {"code": 1, "message": "x"}})
            ),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
    }

    #[test]
    fn wrong_jsonrpc_version_and_non_object_frames_are_invalid() {
        assert!(matches!(
            parse_value(json!({"jsonrpc": "1.0", "id": 1, "result": {}})),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
        assert!(matches!(
            parse_frame(b"[]"),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
        assert!(matches!(
            parse_frame(br#""just a string""#),
            Err(FrameError {
                kind: FrameErrorKind::InvalidEnvelope,
                ..
            })
        ));
    }

    #[test]
    fn invalid_json_and_invalid_utf8_are_distinguished() {
        assert!(matches!(
            parse_frame(b"not json at all"),
            Err(FrameError {
                kind: FrameErrorKind::InvalidJson,
                ..
            })
        ));
        assert!(matches!(
            parse_frame(b"\xff\xfe{\"jsonrpc\":\"2.0\""),
            Err(FrameError {
                kind: FrameErrorKind::InvalidUtf8,
                ..
            })
        ));
    }

    #[test]
    fn profile_list_ignores_unknown_wire_fields() {
        let result: ProfileListResult = serde_json::from_value(json!({
            "profiles": [{
                "id": "coding",
                "model": "deep",
                "reasoning": "high",
                "tools": ["read", "write"],
                "approval": "auto"
            }]
        }))
        .unwrap();
        assert_eq!(result.profiles.len(), 1);
        let profile = &result.profiles[0];
        assert_eq!(profile.id, "coding");
        assert_eq!(profile.model, "deep");
        assert_eq!(profile.reasoning, Reasoning::High);
        assert_eq!(profile.tools, vec!["read".to_owned(), "write".to_owned()]);
    }

    #[test]
    fn model_list_fixture_parses_all_reasoning_levels() {
        let result: ModelListResult = serde_json::from_value(json!({
            "models": [{
                "id": "deep",
                "model_ref": "deep",
                "context_window": 128000,
                "supports_tools": true,
                "supported_reasoning": ["auto", "disabled", "low", "medium", "high"]
            }]
        }))
        .unwrap();
        let model = &result.models[0];
        assert_eq!(model.context_window, 128000);
        assert!(model.supports_tools);
        assert_eq!(
            model.supported_reasoning,
            vec![
                Reasoning::Auto,
                Reasoning::Disabled,
                Reasoning::Low,
                Reasoning::Medium,
                Reasoning::High
            ]
        );
    }

    #[test]
    fn session_list_fixture_parses_nullable_fields() {
        let result: SessionListResult = serde_json::from_value(json!({
            "sessions": [
                {
                    "session_id": "ses_1",
                    "title": "Task",
                    "profile": "coding",
                    "workspace": "/project",
                    "model": "deep",
                    "reasoning": "high",
                    "loaded": true,
                    "instance_id": "ins_1",
                    "created_at": "2026-01-02T03:04:05.006Z",
                    "updated_at": "2026-01-02T03:04:05.006Z"
                },
                {
                    "session_id": "ses_2",
                    "title": null,
                    "profile": "coding",
                    "workspace": "/project",
                    "model": "fast",
                    "reasoning": "disabled",
                    "loaded": false,
                    "instance_id": null,
                    "created_at": "2026-01-02T03:04:05.006Z",
                    "updated_at": "2026-01-02T03:04:05.006Z"
                }
            ]
        }))
        .unwrap();
        assert_eq!(result.sessions.len(), 2);
        assert_eq!(result.sessions[0].title.as_deref(), Some("Task"));
        assert!(result.sessions[0].loaded);
        assert_eq!(result.sessions[1].title, None);
        assert_eq!(result.sessions[1].instance_id, None);
        assert_eq!(result.sessions[1].reasoning, Reasoning::Disabled);
    }

    #[test]
    fn unknown_reasoning_value_is_a_protocol_error() {
        let result: Result<ModelInfo, _> = serde_json::from_value(json!({
            "id": "deep",
            "model_ref": "deep",
            "context_window": 128000,
            "supports_tools": true,
            "supported_reasoning": ["max"]
        }));
        assert!(result.is_err());
    }

    #[test]
    fn result_as_returns_typed_results() {
        let frame = parse_value(response_with_result(1, json!({"version": "0.2.0"}))).unwrap();
        let IncomingFrame::Response(response) = frame else {
            panic!("expected response")
        };
        assert_eq!(response.parse_ping().unwrap().version, "0.2.0");
    }
}
