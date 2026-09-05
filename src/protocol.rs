//! Wire DTOs for minicore-agent 0.3.x over stdio JSON-RPC.
//!
//! Responses intentionally ignore unknown fields so patch releases can add
//! read-only data. Outbound request structs are explicit and only serialize
//! fields owned by this client.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const JSONRPC_VERSION: &str = "2.0";

pub const METHOD_PING: &str = "agent.ping";
pub const METHOD_LIST_MODELS: &str = "model.list";
pub const METHOD_LIST_PROFILES: &str = "profile.list";
pub const METHOD_LIST_SESSIONS: &str = "session.list";
pub const METHOD_SESSION_CREATE: &str = "session.create";
pub const METHOD_SESSION_OPEN: &str = "session.open";
pub const METHOD_SESSION_CLOSE: &str = "session.close";
pub const METHOD_SESSION_DELETE: &str = "session.delete";
pub const METHOD_SESSION_STATE: &str = "session.state";
pub const METHOD_SESSION_UPDATE: &str = "session.update";
pub const METHOD_SESSION_HISTORY: &str = "session.history";
pub const METHOD_GET_HISTORY: &str = METHOD_SESSION_HISTORY;
pub const METHOD_TURN_SEND: &str = "turn.send";
pub const METHOD_TURN_CANCEL: &str = "turn.cancel";
pub const METHOD_TURN_WAIT: &str = "turn.wait";
pub const METHOD_TURN_STEER: &str = "turn.steer";
pub const METHOD_SHUTDOWN: &str = "agent.shutdown";

pub const PARSE_ERROR: i64 = -32_700;
pub const INVALID_REQUEST: i64 = -32_600;
pub const METHOD_NOT_FOUND: i64 = -32_601;
pub const INVALID_PARAMS: i64 = -32_602;
pub const INTERNAL_ERROR: i64 = -32_603;
pub const SESSION_NOT_FOUND: i64 = -32_001;
pub const SESSION_NOT_LOADED: i64 = -32_002;
pub const SESSION_BUSY: i64 = -32_003;
pub const SESSION_BLOCKED: i64 = -32_004;
pub const INVALID_STATE: i64 = -32_005;
pub const INTERACTION_NOT_FOUND: i64 = -32_006;
pub const TURN_NOT_FOUND: i64 = -32_007;
pub const PROFILE_NOT_FOUND: i64 = -32_008;
pub const MODEL_NOT_FOUND: i64 = -32_009;
pub const WORKSPACE_ERROR: i64 = -32_010;
pub const STORE_ERROR: i64 = -32_011;
pub const RUNTIME_ERROR: i64 = -32_013;
pub const INVALID_SESSION_SETTINGS: i64 = -32_014;
pub const HISTORY_TOO_LARGE: i64 = -32_015;
pub const STEER_QUEUE_FULL: i64 = -32_016;

pub const DEFAULT_HISTORY_LIMIT: usize = 20;
pub const MAX_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct TurnRef {
    pub session_id: String,
    pub loop_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingRequest {
    pub jsonrpc: &'static str,
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

    pub fn session_create(
        id: RequestId,
        workspace: &str,
        profile: Option<&str>,
        model: Option<&str>,
        reasoning: Option<Reasoning>,
        title: Option<&str>,
    ) -> Self {
        Self::new(
            id,
            METHOD_SESSION_CREATE,
            serde_json::to_value(SessionCreateParams {
                workspace: workspace.to_owned(),
                profile: profile.map(str::to_owned),
                model: model.map(str::to_owned),
                reasoning,
                title: title.map(str::to_owned),
            })
            .expect("session.create params serialize"),
        )
    }

    pub fn session_open(id: RequestId, session_id: &str) -> Self {
        Self::new(id, METHOD_SESSION_OPEN, json!({ "session_id": session_id }))
    }

    pub fn session_close(id: RequestId, session_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_CLOSE,
            json!({ "session_id": session_id }),
        )
    }

    pub fn session_delete(id: RequestId, session_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_DELETE,
            json!({ "session_id": session_id }),
        )
    }

    pub fn session_state(id: RequestId, session_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_STATE,
            json!({ "session_id": session_id }),
        )
    }

    pub fn session_update(
        id: RequestId,
        session_id: &str,
        model: Option<String>,
        reasoning: Option<Reasoning>,
    ) -> Self {
        Self::new(
            id,
            METHOD_SESSION_UPDATE,
            serde_json::to_value(SessionUpdateParams {
                session_id: session_id.to_owned(),
                model,
                reasoning,
            })
            .expect("session.update params serialize"),
        )
    }

    pub fn get_history(id: RequestId, session_id: &str, offset: usize, limit: usize) -> Self {
        Self::new(
            id,
            METHOD_SESSION_HISTORY,
            json!({
                "session_id": session_id, "offset": offset, "limit": limit,
            }),
        )
    }

    pub fn session_history(
        id: RequestId,
        session_id: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Self {
        Self::get_history(
            id,
            session_id,
            offset.unwrap_or(0),
            limit.unwrap_or(DEFAULT_HISTORY_LIMIT),
        )
    }

    pub fn send_turn(id: RequestId, session_id: &str, text: &str) -> Self {
        Self::new(
            id,
            METHOD_TURN_SEND,
            json!({ "session_id": session_id, "text": text }),
        )
    }

    pub fn steer_turn(id: RequestId, turn: &TurnRef, text: &str) -> Self {
        Self::new(
            id,
            METHOD_TURN_STEER,
            json!({
                "session_id": turn.session_id, "loop_id": turn.loop_id, "text": text,
            }),
        )
    }

    pub fn wait_turn(id: RequestId, turn: &TurnRef) -> Self {
        Self::new(
            id,
            METHOD_TURN_WAIT,
            json!({
                "session_id": turn.session_id, "loop_id": turn.loop_id,
            }),
        )
    }

    pub fn cancel_turn(id: RequestId, turn: &TurnRef) -> Self {
        Self::new(
            id,
            METHOD_TURN_CANCEL,
            json!({
                "session_id": turn.session_id, "loop_id": turn.loop_id,
            }),
        )
    }

    pub fn shutdown(id: RequestId) -> Self {
        Self::new(id, METHOD_SHUTDOWN, json!({}))
    }

    // Descriptive aliases used by callers that name the RPC operation first.
    pub fn create_session(
        id: RequestId,
        workspace: &str,
        profile: Option<&str>,
        model: Option<&str>,
        reasoning: Option<Reasoning>,
        title: Option<&str>,
    ) -> Self {
        Self::session_create(id, workspace, profile, model, reasoning, title)
    }
    pub fn open_session(id: RequestId, session_id: &str) -> Self {
        Self::session_open(id, session_id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingFrame {
    Response(RpcResponse),
    Notification(RpcNotification),
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum RpcNotification {
    AgentEvent(AgentEventWire),
    Unknown { method: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcResponse {
    pub id: RequestId,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn result_as<T: DeserializeOwned>(&self) -> Result<T, RpcResponseError> {
        match (&self.result, &self.error) {
            (Some(value), None) => Ok(serde_json::from_value(value.clone())?),
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
    pub fn parse_session(&self) -> Result<SessionResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_session_state(&self) -> Result<SessionStateWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_session_update(&self) -> Result<SessionUpdateResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_close(&self) -> Result<OkResultWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_delete(&self) -> Result<OkResultWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_history(&self) -> Result<HistoryPageWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_turn_send(&self) -> Result<TurnResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_turn_wait(&self) -> Result<TurnResultViewWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_steer(&self) -> Result<OkResultWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_cancel(&self) -> Result<CancelledResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_shutdown(&self) -> Result<ShutdownResult, RpcResponseError> {
        self.result_as()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcResponseError {
    #[error("agent error {0}")]
    Agent(RpcError),
    #[error("malformed result payload: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("response has no result or error payload")]
    Malformed,
}

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
            .map_or("unknown", |data| data.kind.as_str());
        write!(f, "{} (code {}, kind {})", self.message, self.code, kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcErrorData {
    pub kind: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventWire {
    SessionOpened {
        data: SessionOpenedDataWire,
    },
    SessionClosed {
        data: SessionClosedDataWire,
    },
    SessionState {
        data: SessionStateDataWire,
    },
    TurnStarted {
        data: TurnStartedDataWire,
    },
    RequestStarted {
        data: RequestStartedDataWire,
    },
    OutputDelta {
        data: OutputDeltaDataWire,
    },
    ToolStarted {
        data: ToolStartedDataWire,
    },
    ToolProgress {
        data: ToolProgressDataWire,
    },
    ToolFinished {
        data: ToolFinishedDataWire,
    },
    InteractionRequested {
        data: InteractionRequestedDataWire,
    },
    InteractionResolved {
        data: InteractionResolvedDataWire,
    },
    TurnFinished {
        data: TurnFinishedDataWire,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EventMetaWire {
    pub session_id: String,
    #[serde(default)]
    pub loop_id: Option<String>,
    pub dropped_before: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionOpenedDataWire {
    pub session: SessionInfo,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionClosedDataWire {
    pub session_id: String,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionStateDataWire {
    pub state: SessionStateWire,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TurnStartedDataWire {
    pub turn: TurnRef,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RequestStartedDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub config_revision: u64,
    pub model: String,
    pub reasoning: Reasoning,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OutputDeltaDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub channel: OutputChannelWire,
    pub delta: String,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolStartedDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolProgressDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub tool_call_id: String,
    pub progress: ToolProgressWire,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolFinishedDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub tool_call_id: String,
    pub result: ToolResultWire,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InteractionRequestedDataWire {
    pub turn: TurnRef,
    pub interaction: PendingInteractionWire,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InteractionResolvedDataWire {
    pub turn: TurnRef,
    pub interaction_id: String,
    pub meta: EventMetaWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TurnFinishedDataWire {
    pub turn: TurnRef,
    pub outcome: LoopOutcomeWire,
    pub persistence: TurnPersistenceWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputChannelWire {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolProgressWire {
    pub message: Option<String>,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeWire {
    Success,
    Failed,
    Denied,
    Cancelled,
    InputProvided,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolResultWire {
    pub outcome: ToolOutcomeWire,
    pub content_bytes: usize,
}

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
        f.write_str(match self {
            Self::InvalidUtf8 => "frame is not valid UTF-8",
            Self::InvalidJson => "frame is not valid JSON",
            Self::InvalidEnvelope => "frame is not a valid RPC envelope",
            Self::TooLarge => "frame exceeds the size limit",
            Self::PartialFrame => "stdout closed mid-frame",
            Self::Io => "pipe I/O failure",
        })
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

#[derive(Deserialize)]
struct Envelope {
    jsonrpc: Option<Value>,
    id: Option<Value>,
    method: Option<Value>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<Value>,
}

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
    match (
        envelope.id,
        envelope.method.as_ref().and_then(Value::as_str),
    ) {
        (Some(id), _) => parse_response(id, envelope.result, envelope.error),
        (None, Some("agent.event")) => {
            let params = envelope
                .params
                .ok_or_else(|| invalid("agent.event notification without params"))?;
            let event = serde_json::from_value(params)
                .map_err(|_| invalid("malformed agent.event params"))?;
            Ok(IncomingFrame::Notification(RpcNotification::AgentEvent(
                event,
            )))
        }
        (None, Some(method)) => Ok(IncomingFrame::Notification(RpcNotification::Unknown {
            method: method.to_owned(),
        })),
        (None, None) => Err(invalid("frame has neither id nor method")),
    }
}
fn invalid(detail: &str) -> FrameError {
    FrameError::new(FrameErrorKind::InvalidEnvelope, detail)
}
fn parse_response(
    id: Value,
    result: Option<Value>,
    error: Option<Value>,
) -> Result<IncomingFrame, FrameError> {
    let id = id
        .as_u64()
        .map(RequestId)
        .ok_or_else(|| invalid("response id is not an unsigned integer"))?;
    let response = match (result, error) {
        (Some(result), None) => RpcResponse {
            id,
            result: Some(result),
            error: None,
        },
        (None, Some(error)) => RpcResponse {
            id,
            result: None,
            error: Some(parse_wire_error(error)?),
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
        Some(value) => {
            let data = value
                .as_object()
                .ok_or_else(|| invalid("error data is not an object"))?;
            Some(RpcErrorData {
                kind: data
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("error data kind is missing"))?
                    .to_owned(),
                retryable: data
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PingResult {
    pub version: String,
}

pub fn is_supported_agent_version(version: &str) -> bool {
    let version = version.trim();
    #[cfg(not(debug_assertions))]
    {
        if version.contains('-') || version.contains('+') {
            return false;
        }
    }
    let core = version
        .split_once('-')
        .or_else(|| version.split_once('+'))
        .map_or(version, |(core, _)| core);
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    if major != "0" || minor != "3" {
        return false;
    }
    !patch.is_empty() && patch.chars().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelListResult {
    pub models: Vec<ModelInfo>,
}
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub title: Option<String>,
    pub profile: String,
    pub workspace: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub loaded: bool,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionCreateParams {
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionUpdateParams {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionResult {
    pub session: SessionInfo,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionUpdateResult {
    pub session: SessionInfo,
    pub active_revision: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TurnResult {
    pub turn: TurnRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OkResultWire {
    pub ok: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CancelledResult {
    pub cancelled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ShutdownResult {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SessionStateWire {
    pub session_id: String,
    pub status: SessionStatusWire,
    pub active_loop: Option<LoopStateWire>,
    pub block_reason: Option<SessionBlockReasonWire>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusWire {
    Idle,
    Running,
    WaitingForInput,
    Finishing,
    Blocked,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionBlockReasonWire {
    Persistence,
    Internal,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatusWire {
    Starting,
    RunningModel,
    RunningTools,
    WaitingForInput,
    Finishing,
    Finished,
}
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct LoopStateWire {
    pub loop_id: String,
    pub status: LoopStatusWire,
    pub request_index: u32,
    pub config_revision: u64,
    pub model: Option<String>,
    pub pending_interaction: Option<PendingInteractionWire>,
}
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PendingInteractionWire {
    pub interaction_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub kind: Value,
}
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoopOutcomeWire {
    Completed,
    Cancelled {
        reason: CancelReasonWire,
    },
    Failed {
        kind: String,
        model_error: Option<ModelErrorWire>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReasonWire {
    User,
    OwnerDropped,
    Shutdown,
    Deadline,
    #[serde(untagged)]
    Unknown(String),
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ModelErrorWire {
    pub kind: String,
    pub delivery: String,
    pub retryable: bool,
    pub retry_after_millis: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPersistenceWire {
    Persisted,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TurnResultViewWire {
    pub turn: TurnRef,
    pub outcome: LoopOutcomeWire,
    pub usage: UsageWire,
    pub requests: u32,
    pub tool_rounds: u16,
    pub final_config_revision: u64,
    pub persistence: TurnPersistenceWire,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct UsageWire {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub provider_total_tokens: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HistoryPageWire {
    pub items: Vec<IndexedHistoryItemWire>,
    pub next_offset: Option<usize>,
    pub total: usize,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IndexedHistoryItemWire {
    pub index: usize,
    pub item: HistoryItemViewWire,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HistoryItemViewWire {
    User(UserHistoryViewWire),
    Assistant(AssistantHistoryViewWire),
    ToolResult(ToolResultHistoryViewWire),
    Summary(SummaryHistoryViewWire),
}
impl HistoryItemViewWire {
    pub fn loop_id(&self) -> Option<&str> {
        match self {
            Self::User(u) => Some(&u.loop_id),
            Self::Assistant(a) => Some(&a.loop_id),
            Self::ToolResult(t) => Some(&t.loop_id),
            Self::Summary(_) => None,
        }
    }
}
pub type HistoryItemWire = HistoryItemViewWire;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageKindWire {
    Prompt,
    Steering,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserHistoryViewWire {
    pub loop_id: String,
    pub kind: UserMessageKindWire,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantHistoryViewWire {
    pub loop_id: String,
    pub request_index: u32,
    pub model: String,
    pub reasoning_level: Reasoning,
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCallViewWire>,
    pub usage: UsageWire,
    pub finish_reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolCallViewWire {
    pub tool_call_id: String,
    pub name: String,
    #[serde(default)]
    pub call_index: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolResultHistoryViewWire {
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: ToolOutcomeWire,
    pub content: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SummaryHistoryViewWire {
    pub content: String,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    #[default]
    Auto,
    Disabled,
    Low,
    Medium,
    High,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_gate_is_exactly_0_3_major_minor() {
        assert!(is_supported_agent_version("0.3.0"));
        assert_eq!(
            is_supported_agent_version("0.3.1-rc.1"),
            cfg!(debug_assertions)
        );
        assert!(!is_supported_agent_version("0.2.9"));
        assert!(!is_supported_agent_version("0.4.0"));
        assert!(!is_supported_agent_version("0.3.x"));
    }
    #[test]
    fn version_gate_prerelease_policy() {
        if cfg!(debug_assertions) {
            assert!(is_supported_agent_version("0.3.0-alpha.1"));
            assert!(is_supported_agent_version("0.3.1-rc.2+build.42"));
        } else {
            assert!(!is_supported_agent_version("0.3.0-alpha.1"));
            assert!(!is_supported_agent_version("0.3.1-rc.2+build.42"));
        }
    }
    #[test]
    fn ping_request_has_the_documented_shape() {
        let value = serde_json::to_value(OutgoingRequest::ping(RequestId(1))).unwrap();
        assert_eq!(
            value,
            json!({"jsonrpc":"2.0","id":1,"method":"agent.ping","params":{}})
        );
    }
    #[test]
    fn wait_result_is_not_wrapped_in_ok_result() {
        let value = json!({"turn":{"session_id":"ses_1","loop_id":"loop_1"},"outcome":{"type":"completed"},"usage":{},"requests":2,"tool_rounds":1,"final_config_revision":3,"persistence":"persisted"});
        let result: TurnResultViewWire = serde_json::from_value(value).unwrap();
        assert_eq!(result.requests, 2);
        assert_eq!(result.persistence, TurnPersistenceWire::Persisted);
    }
}
