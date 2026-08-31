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
pub const METHOD_SESSION_CREATE: &str = "session.create";
pub const METHOD_SESSION_OPEN: &str = "session.open";
pub const METHOD_SESSION_STATE: &str = "session.state";
pub const METHOD_TRANSCRIPT: &str = "session.transcript";
pub const METHOD_TURN_SEND: &str = "turn.send";
pub const METHOD_TURN_WAIT: &str = "turn.wait";
pub const METHOD_TURN_CANCEL: &str = "turn.cancel";

/// Monotonic request id, starting at 1 (spec 10.4). Never persisted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// The exact identity of a running turn, echoed by `turn.send` and accepted
/// by `turn.wait` and `turn.cancel`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct TurnRef {
    pub session_id: String,
    pub instance_id: String,
    pub turn_id: String,
}

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

    /// `session.create` with the given workspace and optional overrides; the
    /// agent fills in defaults for absent fields.
    pub fn session_create(
        id: RequestId,
        workspace: &str,
        profile: Option<&str>,
        model: Option<&str>,
        reasoning: Option<Reasoning>,
        title: Option<&str>,
    ) -> Self {
        let params = SessionCreateParams {
            workspace: workspace.to_owned(),
            profile: profile.map(str::to_owned),
            model: model.map(str::to_owned),
            reasoning,
            title: title.map(str::to_owned),
        };
        let params = serde_json::to_value(params).expect("session create params serialize");
        Self::new(id, METHOD_SESSION_CREATE, params)
    }

    pub fn session_open(id: RequestId, session_id: &str) -> Self {
        let params = SessionIdParams {
            session_id: session_id.to_owned(),
        };
        let params = serde_json::to_value(params).expect("session params serialize");
        Self::new(id, METHOD_SESSION_OPEN, params)
    }

    pub fn session_state(id: RequestId, session_id: &str) -> Self {
        let params = SessionIdParams {
            session_id: session_id.to_owned(),
        };
        let params = serde_json::to_value(params).expect("session params serialize");
        Self::new(id, METHOD_SESSION_STATE, params)
    }

    /// One transcript page: entries strictly after `after`, or the whole
    /// log when `after` is `None`. The agent's default page size is 100;
    /// the app pages one page at a time.
    pub fn transcript(id: RequestId, session_id: &str, after: Option<u64>) -> Self {
        let params = TranscriptParams {
            session_id: session_id.to_owned(),
            after,
            limit: TRANSCRIPT_PAGE_LIMIT,
        };
        let params = serde_json::to_value(params).expect("transcript params serialize");
        Self::new(id, METHOD_TRANSCRIPT, params)
    }

    pub fn send_turn(id: RequestId, session_id: &str, text: &str) -> Self {
        let params = SendTurnParams {
            session_id: session_id.to_owned(),
            text: text.to_owned(),
        };
        let params = serde_json::to_value(params).expect("turn params serialize");
        Self::new(id, METHOD_TURN_SEND, params)
    }

    pub fn wait_turn(id: RequestId, turn: &TurnRef) -> Self {
        let params = serde_json::to_value(turn).expect("turn ref serializes");
        Self::new(id, METHOD_TURN_WAIT, params)
    }

    pub fn cancel_turn(id: RequestId, turn: &TurnRef) -> Self {
        let params = serde_json::to_value(turn).expect("turn ref serializes");
        Self::new(id, METHOD_TURN_CANCEL, params)
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

    pub fn parse_session(&self) -> Result<SessionResult, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_session_state(&self) -> Result<SessionStateWire, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_transcript(&self) -> Result<TranscriptPageWire, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_turn(&self) -> Result<TurnResult, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_turn_wait(&self) -> Result<TurnOutcomeWire, RpcResponseError> {
        self.result_as()
    }

    pub fn parse_cancel(&self) -> Result<CancelledResult, RpcResponseError> {
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

/// Every `agent.event` payload the pinned agent can emit (spec 11.7). The
/// `data` member of each event is modeled by the matching `*DataWire`
/// struct. Unknown future event types parse as `Unknown` and stay
/// ignorable; malformed payloads of known types are a protocol error.
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
        data: TurnEventDataWire,
    },
    OutputDelta {
        data: OutputDeltaDataWire,
    },
    ToolStarted {
        data: ToolEventDataWire,
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
    /// A future event type this client does not model.
    #[serde(other)]
    Unknown,
}

/// Metadata carried by every agent event.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EventMetaWire {
    pub session_id: String,
    pub instance_id: String,
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
pub struct TurnEventDataWire {
    pub turn: TurnRef,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OutputDeltaDataWire {
    pub turn: TurnRef,
    pub channel: OutputChannelWire,
    pub delta: String,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolEventDataWire {
    pub turn: TurnRef,
    pub tool_call_id: String,
    pub tool_name: String,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolProgressDataWire {
    pub turn: TurnRef,
    pub tool_call_id: String,
    pub progress: ToolProgressWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolFinishedDataWire {
    pub turn: TurnRef,
    pub tool_call_id: String,
    pub result: ToolResultWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InteractionRequestedDataWire {
    pub session_id: String,
    pub interaction: PendingInteractionWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InteractionResolvedDataWire {
    pub session_id: String,
    pub interaction_id: String,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TurnFinishedDataWire {
    pub turn: TurnRef,
    pub outcome: TurnOutcomeWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputChannelWire {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolProgressWire {
    pub message: Option<String>,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeWire {
    Success,
    Failed,
    Denied,
    Cancelled,
    InputProvided,
}

/// `tool_finished` result: outcome and byte size only; the durable content
/// comes from the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ToolResultWire {
    pub outcome: ToolOutcomeWire,
    pub content_bytes: u64,
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
            let event = serde_json::from_value::<AgentEventWire>(params)
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

/// Page size for `session.transcript`; the agent's default is 100.
pub const TRANSCRIPT_PAGE_LIMIT: usize = 100;

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
pub struct SessionIdParams {
    pub session_id: String,
}

/// The `session.create` / `session.open` result member.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionResult {
    pub session: SessionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptParams {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendTurnParams {
    pub session_id: String,
    pub text: String,
}

/// The `turn.send` result member: the exact turn identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TurnResult {
    pub turn: TurnRef,
}

/// The `turn.cancel` result member.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CancelledResult {
    pub cancelled: bool,
}

/// The `session.state` return value; also the `session_state` event data.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionStateWire {
    pub session_id: String,
    pub instance_id: String,
    pub status: SessionStatusWire,
    pub health: SessionHealthWire,
    pub active_turn: Option<String>,
    pub pending_interaction: Option<PendingInteractionWire>,
    pub conversation_seq: u64,
    pub last_terminal: Option<TurnOutcomeWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusWire {
    Idle,
    Running,
    WaitingForInput,
    Closing,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionHealthWire {
    Healthy,
    Degraded { diagnostic: DiagnosticWire },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PendingInteractionWire {
    pub interaction_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    /// The tagged interaction kind; this TUI answers none of them.
    pub kind: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DiagnosticWire {
    pub code: String,
    pub category: String,
    pub retryable: bool,
}

/// The `turn.wait` result member.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TurnOutcomeWire {
    pub turn_id: String,
    pub terminal: TurnTerminalWire,
    pub usage: UsageWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnTerminalWire {
    Completed,
    CancelledByUser,
    CancelledByShutdown,
    CancelledByRestart,
    BudgetExceeded,
    Failed { diagnostic: DiagnosticWire },
}

/// Usage counters. The object is always present where specified; every
/// member is optional on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
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

/// One `session.transcript` page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TranscriptPageWire {
    pub entries: Vec<ConversationEntryWire>,
    pub next_after: Option<u64>,
    pub observed_head: u64,
    pub complete: bool,
}

/// Durable conversation entries, externally tagged by the agent.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationEntryWire {
    UserMessage(UserMessageViewWire),
    AssistantMessage(AssistantMessageViewWire),
    ToolResult(ToolResultViewWire),
    Summary(SummaryViewWire),
    TurnTerminal(TurnTerminalEntryViewWire),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserMessageViewWire {
    pub seq: u64,
    pub turn_id: String,
    pub text: String,
    pub execution: TurnExecutionWire,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TurnExecutionWire {
    pub model: String,
    pub reasoning: Reasoning,
    pub max_tool_rounds: u16,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantMessageViewWire {
    pub seq: u64,
    pub turn_id: String,
    pub model: String,
    pub text: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallViewWire>,
    pub usage: UsageWire,
    pub finish_reason: String,
    pub created_at: String,
}

/// Assistant tool calls never carry arguments on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolCallViewWire {
    pub tool_call_id: String,
    pub name: String,
    pub call_index: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolResultViewWire {
    pub seq: u64,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: ToolOutcomeWire,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SummaryViewWire {
    pub seq: u64,
    pub through: u64,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TurnTerminalEntryViewWire {
    pub seq: u64,
    pub turn_id: String,
    pub terminal: TurnTerminalWire,
    pub usage: UsageWire,
    pub created_at: String,
}

/// Model reasoning levels the TUI understands. Unknown wire values are a
/// protocol error (spec 11.5); the agent does not expose other levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
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
    fn agent_event_notification_parses_typed_payloads() {
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
        let AgentEventWire::OutputDelta { data } = event else {
            panic!("expected an output delta event")
        };
        assert_eq!(data.channel, OutputChannelWire::Text);
        assert_eq!(data.delta, "hi");
        assert_eq!(data.turn.turn_id, "trn_1");
        assert_eq!(data.meta.dropped_before, 2);
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

    #[test]
    fn turn_request_builders_match_the_wire_shapes() {
        let frame =
            serde_json::to_value(OutgoingRequest::send_turn(RequestId(5), "ses_1", "Hello"))
                .unwrap();
        assert_eq!(frame["method"], "turn.send");
        assert_eq!(
            frame["params"],
            json!({"session_id": "ses_1", "text": "Hello"})
        );

        let turn = TurnRef {
            session_id: "ses_1".into(),
            instance_id: "ins_1".into(),
            turn_id: "trn_1".into(),
        };
        let frame = serde_json::to_value(OutgoingRequest::wait_turn(RequestId(6), &turn)).unwrap();
        assert_eq!(frame["method"], "turn.wait");
        assert_eq!(
            frame["params"],
            json!({"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_1"})
        );
        let frame =
            serde_json::to_value(OutgoingRequest::cancel_turn(RequestId(7), &turn)).unwrap();
        assert_eq!(frame["method"], "turn.cancel");
        assert_eq!(
            frame["params"],
            json!({"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_1"})
        );
    }

    #[test]
    fn transcript_builder_omits_none_after_and_sends_limit_100() {
        let frame =
            serde_json::to_value(OutgoingRequest::transcript(RequestId(8), "ses_1", None)).unwrap();
        assert_eq!(frame["method"], "session.transcript");
        assert_eq!(
            frame["params"],
            json!({"session_id": "ses_1", "limit": 100})
        );
        assert!(frame["params"].get("after").is_none());
        let frame =
            serde_json::to_value(OutgoingRequest::transcript(RequestId(9), "ses_1", Some(12)))
                .unwrap();
        assert_eq!(frame["params"]["after"], json!(12));
    }

    #[test]
    fn session_create_builder_serializes_only_present_optional_fields() {
        let frame = serde_json::to_value(OutgoingRequest::session_create(
            RequestId(10),
            "/project",
            None,
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(frame["params"], json!({"workspace": "/project"}));
        let frame = serde_json::to_value(OutgoingRequest::session_create(
            RequestId(11),
            "/project",
            Some("coding"),
            Some("deep"),
            Some(Reasoning::High),
            Some("Task"),
        ))
        .unwrap();
        assert_eq!(
            frame["params"],
            json!({
                "workspace": "/project",
                "profile": "coding",
                "model": "deep",
                "reasoning": "high",
                "title": "Task"
            })
        );
    }

    #[test]
    fn session_state_fixture_parses_status_health_and_interaction() {
        let state: SessionStateWire = serde_json::from_value(json!({
            "session_id": "ses_1",
            "instance_id": "ins_1",
            "status": "waiting_for_input",
            "health": {"degraded": {"diagnostic": {
                "code": "provider_unavailable",
                "category": "model",
                "retryable": true
            }}},
            "active_turn": "trn_1",
            "pending_interaction": {
                "interaction_id": "int_1",
                "turn_id": "trn_1",
                "tool_call_id": "call_1",
                "tool_name": "write",
                "kind": {"type": "approval", "data": {"prompt": "Allow?", "risk": "medium"}}
            },
            "conversation_seq": 7,
            "last_terminal": {
                "turn_id": "trn_1",
                "terminal": {"failed": {"diagnostic": {
                    "code": "model_unavailable",
                    "category": "model",
                    "retryable": false
                }}},
                "usage": {"input_tokens": 10, "output_tokens": 4}
            }
        }))
        .unwrap();
        assert_eq!(state.status, SessionStatusWire::WaitingForInput);
        assert!(matches!(state.health, SessionHealthWire::Degraded { .. }));
        assert_eq!(state.conversation_seq, 7);
        assert_eq!(
            state.pending_interaction.as_ref().unwrap().tool_name,
            "write"
        );
        let last = state.last_terminal.unwrap();
        assert!(matches!(last.terminal, TurnTerminalWire::Failed { .. }));
    }

    #[test]
    fn session_status_parses_all_snake_case_values() {
        for (wire, status) in [
            ("idle", SessionStatusWire::Idle),
            ("running", SessionStatusWire::Running),
            ("waiting_for_input", SessionStatusWire::WaitingForInput),
            ("closing", SessionStatusWire::Closing),
        ] {
            assert_eq!(
                serde_json::from_value::<SessionStatusWire>(json!(wire)).unwrap(),
                status
            );
        }
    }

    #[test]
    fn usage_members_default_to_none() {
        let usage: UsageWire = serde_json::from_value(json!({})).unwrap();
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.provider_total_tokens, None);
        assert!(serde_json::from_value::<UsageWire>(json!({"output_tokens": 3})).is_ok());
    }

    #[test]
    fn transcript_page_fixture_parses_all_entry_tags() {
        let page: TranscriptPageWire = serde_json::from_value(json!({
            "entries": [
                {"user_message": {
                    "seq": 1,
                    "turn_id": "trn_1",
                    "text": "user text",
                    "execution": {"model": "deep", "reasoning": "medium", "max_tool_rounds": 8},
                    "created_at": "2026-01-02T03:04:05.006Z"
                }},
                {"assistant_message": {
                    "seq": 2,
                    "turn_id": "trn_1",
                    "model": "deep",
                    "text": "assistant text",
                    "reasoning": "assistant reasoning",
                    "tool_calls": [{"tool_call_id": "call-1", "name": "write", "call_index": 0}],
                    "usage": {},
                    "finish_reason": "tool_calls",
                    "created_at": "2026-01-02T03:04:05.006Z"
                }},
                {"tool_result": {
                    "seq": 3,
                    "turn_id": "trn_1",
                    "tool_call_id": "call-1",
                    "tool_name": "write",
                    "outcome": "denied",
                    "content": "durable tool result",
                    "created_at": "2026-01-02T03:04:05.006Z"
                }},
                {"summary": {
                    "seq": 4,
                    "through": 3,
                    "summary": "durable summary",
                    "created_at": "2026-01-02T03:04:05.006Z"
                }},
                {"turn_terminal": {
                    "seq": 5,
                    "turn_id": "trn_1",
                    "terminal": {"failed": {"diagnostic": {
                        "code": "model_unavailable",
                        "category": "model",
                        "retryable": false
                    }}},
                    "usage": {"input_tokens": 4},
                    "created_at": "2026-01-02T03:04:05.006Z"
                }}
            ],
            "next_after": 5,
            "observed_head": 5,
            "complete": false
        }))
        .unwrap();
        assert_eq!(page.entries.len(), 5);
        match &page.entries[1] {
            ConversationEntryWire::AssistantMessage(assistant) => {
                assert_eq!(assistant.tool_calls[0].name, "write");
                assert!(assistant.text.is_some());
            }
            _ => panic!("expected an assistant entry"),
        }
        assert_eq!(page.next_after, Some(5));
        assert!(!page.complete);
    }

    #[test]
    fn agent_event_wire_parses_every_current_event_type() {
        let fixtures = [
            json!({"type": "session_opened", "data": {"session": {
                "session_id": "ses_1", "title": null, "profile": "coding",
                "workspace": "/p", "model": "deep", "reasoning": "high",
                "loaded": true, "instance_id": "ins_1",
                "created_at": "2026-01-02T03:04:05.006Z",
                "updated_at": "2026-01-02T03:04:05.006Z"
            }, "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "session_closed", "data": {"session_id": "ses_1",
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "session_state", "data": {"state": {
                "session_id": "ses_1", "instance_id": "ins_1", "status": "idle",
                "health": "healthy", "active_turn": null, "pending_interaction": null,
                "conversation_seq": 0, "last_terminal": null
            }, "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "turn_started", "data": {"turn": {"session_id": "ses_1",
                "instance_id": "ins_1", "turn_id": "trn_1"},
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "output_delta", "data": {"turn": {"session_id": "ses_1",
                "instance_id": "ins_1", "turn_id": "trn_1"}, "channel": "reasoning",
                "delta": "hmm", "meta": {"session_id": "ses_1", "instance_id": "ins_1",
                "dropped_before": 0}}}),
            json!({"type": "tool_started", "data": {"turn": {"session_id": "ses_1",
                "instance_id": "ins_1", "turn_id": "trn_1"}, "tool_call_id": "call-1",
                "tool_name": "write", "meta": {"session_id": "ses_1", "instance_id": "ins_1",
                "dropped_before": 0}}}),
            json!({"type": "tool_progress", "data": {"turn": {"session_id": "ses_1",
                "instance_id": "ins_1", "turn_id": "trn_1"}, "tool_call_id": "call-1",
                "progress": {"message": "30%", "completed": 3, "total": 10},
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "tool_finished", "data": {"turn": {"session_id": "ses_1",
                "instance_id": "ins_1", "turn_id": "trn_1"}, "tool_call_id": "call-1",
                "result": {"outcome": "success", "content_bytes": 1024},
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "interaction_requested", "data": {"session_id": "ses_1",
                "interaction": {"interaction_id": "int_1", "turn_id": "trn_1",
                    "tool_call_id": "call-1", "tool_name": "ask", "kind": {"type": "approval"}},
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "interaction_resolved", "data": {"session_id": "ses_1",
                "interaction_id": "int_1", "meta": {"session_id": "ses_1",
                "instance_id": "ins_1", "dropped_before": 0}}}),
            json!({"type": "turn_finished", "data": {"turn": {"session_id": "ses_1",
                "instance_id": "ins_1", "turn_id": "trn_1"},
                "outcome": {"turn_id": "trn_1", "terminal": "completed", "usage": {}},
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
        ];
        for raw in fixtures {
            serde_json::from_value::<AgentEventWire>(raw).expect("event fixture parses");
        }
    }

    #[test]
    fn unknown_agent_event_type_stays_ignorable() {
        let event: AgentEventWire = serde_json::from_value(json!({
            "type": "future_event",
            "data": {"anything": true}
        }))
        .unwrap();
        assert_eq!(event, AgentEventWire::Unknown);
    }
}
