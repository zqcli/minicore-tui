//! Wire DTOs for minicore-agent 0.7.0 (Protocol v1). Unknown fields are
//! ignored, and enums the Agent documents as non-exhaustive decode future values
//! to `Unknown` instead of failing the frame.

pub mod changes;
pub mod event;
pub mod failure;
pub mod read;
pub mod stamp;
pub mod tool;
pub mod workspace;

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use changes::{ChangeKind, ChangeList};
pub use event::AgentEvent;
pub use failure::Failure;
pub use read::{
    ChunkAssembler, ChunkOutcome, DisplayProjection, RawHistoryItem, ReadChunk, ReadCursor,
    ReadSessionResult, RuntimeAssistantItem, RuntimeAssistantPart, RuntimeItem, RuntimeToolResult,
    RuntimeUserItem, SnapshotPin, TurnResultPage,
};
pub use tool::{
    ToolDisplay, ToolExecution, ToolExecutionState, ToolInvocation, ToolOutputPage, ToolReadResult,
    ToolRef, ToolSubject, ToolSummary,
};
pub use workspace::{FileEntry, FileKind, FilesPage};

pub const JSONRPC_VERSION: &str = "2.0";
pub const PROTOCOL_VERSION: u32 = 1;

pub const METHOD_PING: &str = "agent.ping";
pub const METHOD_RELOAD: &str = "agent.reload";
pub const METHOD_MODELS: &str = "model.list";
pub const METHOD_PROFILES: &str = "profile.list";
pub const METHOD_SESSION_LIST: &str = "session.list";
pub const METHOD_SESSION_CREATE: &str = "session.create";
pub const METHOD_SESSION_OPEN: &str = "session.open";
pub const METHOD_SESSION_CLOSE: &str = "session.close";
pub const METHOD_SESSION_DELETE: &str = "session.delete";
pub const METHOD_SESSION_STATE: &str = "session.state";
pub const METHOD_SESSION_UPDATE: &str = "session.update";
pub const METHOD_SESSION_RENAME: &str = "session.rename";
pub const METHOD_SESSION_READ: &str = "session.read";
pub const METHOD_SESSION_PRESENTATION: &str = "session.presentation";
pub const METHOD_SESSION_CONTEXT: &str = "session.context";
pub const METHOD_SESSION_COMPACT: &str = "session.compact";
pub const METHOD_SESSION_COMPACT_CANCEL: &str = "session.compact.cancel";
pub const METHOD_TURN_SEND: &str = "turn.send";
pub const METHOD_TURN_STEER: &str = "turn.steer";
pub const METHOD_TURN_CANCEL: &str = "turn.cancel";
pub const METHOD_TURN_WAIT: &str = "turn.wait";
pub const METHOD_TURN_RESULT: &str = "turn.result";
pub const METHOD_TOOL_READ: &str = "tool.read";
pub const METHOD_TOOL_OUTPUT: &str = "tool.output";
pub const METHOD_WORKSPACE_FILES: &str = "workspace.files";
pub const METHOD_WORKSPACE_STATUS: &str = "workspace.status";
pub const METHOD_CHANGES_LIST: &str = "changes.list";
pub const METHOD_CHANGES_DIFF: &str = "changes.diff";
pub const METHOD_INTERACTION_ANSWER: &str = "interaction.answer";

pub const INVALID_REQUEST: i64 = -32_600;
pub const INVALID_PARAMS: i64 = -32_602;
pub const INTERNAL_ERROR: i64 = -32_603;
pub const SESSION_NOT_LOADED: i64 = -32_002;
pub const SESSION_BUSY: i64 = -32_003;
pub const INVALID_STATE: i64 = -32_005;
pub const TURN_NOT_FOUND: i64 = -32_007;
pub const STEER_QUEUE_FULL: i64 = -32_016;
pub const RESOURCE_EXHAUSTED: i64 = -32_019;
pub const QUERY_LIMIT: i64 = -32_020;

/// Capabilities this client requires before it sends anything beyond the
/// handshake: exactly the ones it uses.
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    "session.read",
    "session.read.display",
    "session.context",
    "turn.result",
    "tool.read",
    "tool.read.display",
    "workspace.files",
    "changes.list",
    "changes.diff",
];

/// Tail read window: how many source items the first history page targets.
pub const TAIL_ITEMS: usize = 60;
pub const PAGE_LIMIT: usize = 20;
pub const PAGE_MAX_BYTES: usize = 262_144;
pub const PROBE_LIMIT: usize = 1;
pub const PROBE_MAX_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct TurnRef {
    pub session_id: String,
    pub loop_id: String,
}

impl TurnRef {
    pub fn new(session_id: impl Into<String>, loop_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            loop_id: loop_id.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
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
}

#[derive(Debug, Clone)]
pub enum IncomingFrame {
    Response(RpcResponse),
    Event(AgentEvent),
    /// A notification method this client does not know about.
    UnknownNotification(String),
}

#[derive(Debug, Clone)]
pub struct RpcResponse {
    pub id: RequestId,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, RpcError> {
        match (&self.result, &self.error) {
            (Some(value), None) => {
                serde_json::from_value(value.clone()).map_err(|error| RpcError {
                    code: INVALID_PARAMS,
                    kind: "malformed_result".into(),
                    message: error.to_string(),
                    retryable: false,
                })
            }
            (None, Some(error)) => Err(error.clone()),
            _ => Err(RpcError {
                code: INVALID_REQUEST,
                kind: "malformed_result".into(),
                message: "response has neither result nor error".into(),
                retryable: false,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub kind: String,
    pub message: String,
    pub retryable: bool,
}

impl RpcError {
    pub fn new(code: i64, kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            kind: kind.into(),
            message: message.into(),
            retryable: false,
        }
    }

    /// Transport-level failure: the agent is gone or the frame stream broke.
    pub fn closed(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, "closed", message)
    }

    pub fn is_busy(&self) -> bool {
        self.code == SESSION_BUSY
    }

    pub fn is_capacity(&self) -> bool {
        matches!(self.code, RESOURCE_EXHAUSTED | QUERY_LIMIT)
    }

    /// A stale history pin or a superseded operation: the caller restarts the
    /// read instead of surfacing a failure.
    pub fn is_stale_pin(&self) -> bool {
        self.code == INVALID_STATE
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameError {
    pub detail: String,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for FrameError {}

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
    let text = std::str::from_utf8(line).map_err(|_| FrameError {
        detail: "frame is not valid UTF-8".into(),
    })?;
    let value: Value = serde_json::from_str(text).map_err(|error| FrameError {
        detail: format!("frame is not valid JSON: {error}"),
    })?;
    let envelope: Envelope = serde_json::from_value(value).map_err(|error| FrameError {
        detail: format!("frame envelope is malformed: {error}"),
    })?;
    if envelope.jsonrpc.as_ref().and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return Err(FrameError {
            detail: "frame has no jsonrpc 2.0 marker".into(),
        });
    }
    match (
        envelope.id,
        envelope.method.as_ref().and_then(Value::as_str),
    ) {
        (Some(id), _) => parse_response(id, envelope.result, envelope.error),
        (None, Some("agent.event")) => {
            let params = envelope.params.ok_or_else(|| FrameError {
                detail: "agent.event without params".into(),
            })?;
            // Events are hints the turn reconciles anyway, so an unreadable one
            // must not stop the stream.
            let event = serde_json::from_value(params).unwrap_or(AgentEvent::Unknown);
            Ok(IncomingFrame::Event(event))
        }
        (None, Some(method)) => Ok(IncomingFrame::UnknownNotification(method.to_owned())),
        (None, None) => Err(FrameError {
            detail: "frame has neither id nor method".into(),
        }),
    }
}

fn parse_response(
    id: Value,
    result: Option<Value>,
    error: Option<Value>,
) -> Result<IncomingFrame, FrameError> {
    let id = id.as_u64().map(RequestId).ok_or_else(|| FrameError {
        detail: "response id is not an unsigned integer".into(),
    })?;
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
        _ => {
            return Err(FrameError {
                detail: "response needs exactly one of result or error".into(),
            });
        }
    };
    Ok(IncomingFrame::Response(response))
}

fn parse_wire_error(value: Value) -> Result<RpcError, FrameError> {
    let object = value.as_object().ok_or_else(|| FrameError {
        detail: "error is not an object".into(),
    })?;
    let code = object
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| FrameError {
            detail: "error code is missing".into(),
        })?;
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (kind, retryable) = match object.get("data") {
        Some(data) => (
            data.get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            data.get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        None => ("unknown", false),
    };
    Ok(RpcError {
        code,
        kind: kind.to_owned(),
        message: message.to_owned(),
        retryable,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct PingResult {
    pub version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<String>,
}

/// Why the connected backend cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("agent speaks protocol {found}; this build requires {required}")]
    ProtocolVersion { found: u32, required: u32 },
    #[error("agent {version} lacks capabilities: {missing}")]
    MissingCapabilities { version: String, missing: String },
}

pub fn validate_backend(ping: &PingResult) -> Result<(), BackendError> {
    if ping.protocol_version != PROTOCOL_VERSION {
        return Err(BackendError::ProtocolVersion {
            found: ping.protocol_version,
            required: PROTOCOL_VERSION,
        });
    }
    let missing: Vec<&str> = REQUIRED_CAPABILITIES
        .iter()
        .copied()
        .filter(|required| !ping.capabilities.iter().any(|have| have == required))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(BackendError::MissingCapabilities {
            version: ping.version.clone(),
            missing: missing.join(", "),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    #[default]
    Auto,
    Disabled,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
    Ultra,
    #[serde(other)]
    Unknown,
}

impl Reasoning {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Disabled => "disabled",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "auto" => Self::Auto,
            "disabled" => Self::Disabled,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::XHigh,
            "max" => Self::Max,
            "ultra" => Self::Ultra,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelListResult {
    pub models: Vec<ModelInfo>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// `provider/model`.
    pub model_ref: Option<String>,
    pub context_window: Option<u64>,
    pub supported_reasoning: Vec<Reasoning>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileListResult {
    pub profiles: Vec<ProfileInfo>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileInfo {
    pub id: String,
    pub model: String,
    pub reasoning: Reasoning,
    /// `auto`, `ask` or `read_only`.
    #[serde(default)]
    pub approval: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: String,
    pub title: Option<String>,
    pub profile: String,
    pub workspace: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub loaded: bool,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionResult {
    pub session: SessionInfo,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    WaitingForInput,
    Finishing,
    Blocked,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionState {
    pub session_id: String,
    pub status: SessionStatus,
    pub active_loop: Option<LoopState>,
    pub compaction: Option<CompactionProgress>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoopState {
    pub loop_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingInteraction {
    pub interaction_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub kind: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompactionProgress {
    pub operation_id: String,
    pub phase: CompactionPhase,
    pub covered_item_count: usize,
    pub retained_item_count: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhase {
    Preparing,
    Summarizing,
    Merging,
    Committing,
    #[serde(other)]
    Unknown,
}

impl CompactionPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Summarizing => "summarizing",
            Self::Merging => "merging",
            Self::Committing => "committing",
            Self::Unknown => "working",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionPresentation {
    pub session_id: String,
    pub git_branch: Option<String>,
    pub context: Option<ContextUsage>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
    pub percent: Option<f64>,
    pub kind: ContextKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Reported,
    Estimated,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionContext {
    pub session_id: String,
    pub current_operation: Option<CompactionProgress>,
    pub coverage: Option<ContextCoverage>,
    pub last_result: Option<CompactionResult>,
    pub budget: Option<ContextBudget>,
    pub automatic: Option<AutomaticView>,
    pub last_prepare_failure: Option<String>,
}

/// The latest compactions at the start of a turn, which have no event.
#[derive(Clone, Debug, Deserialize)]
pub struct AutomaticView {
    pub current: Option<AutomaticObservation>,
    pub last: Option<AutomaticObservation>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AutomaticObservation {
    pub operation_id: String,
    pub before_tokens: Option<u64>,
    pub after_tokens: Option<u64>,
    pub outcome: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContextCoverage {
    pub covered_loop_count: usize,
    pub covered_item_count: usize,
    pub retained_item_count: usize,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContextBudget {
    pub estimated_history_tokens: Option<u64>,
    pub input_budget_tokens: Option<u64>,
    pub trigger_tokens: Option<u64>,
    pub target_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompactStatus {
    Compacted,
    Noop,
    Failed,
    UnknownWrite,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CompactionResult {
    pub operation_id: String,
    pub status: CompactStatus,
    pub before_tokens: Option<u64>,
    pub after_tokens: Option<u64>,
    pub covered_loop_count: Option<usize>,
    pub covered_item_count: Option<usize>,
    pub retained_item_count: Option<usize>,
    pub failure_kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TurnSendResult {
    pub turn: TurnRef,
    pub accepted_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SteerResult {
    pub accepted_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub provider_total_tokens: Option<u64>,
}

impl Usage {
    pub fn add(&mut self, other: &Self) {
        fn add_one(target: &mut Option<u64>, value: Option<u64>) {
            if let Some(value) = value {
                *target = Some(target.unwrap_or(0) + value);
            }
        }
        add_one(&mut self.input_tokens, other.input_tokens);
        add_one(&mut self.output_tokens, other.output_tokens);
        add_one(&mut self.reasoning_tokens, other.reasoning_tokens);
        add_one(&mut self.cache_read_tokens, other.cache_read_tokens);
        add_one(&mut self.cache_write_tokens, other.cache_write_tokens);
        add_one(&mut self.provider_total_tokens, other.provider_total_tokens);
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

impl UsageTotals {
    pub fn add(&mut self, usage: &Usage) {
        self.input += usage.input_tokens.unwrap_or(0);
        self.output += usage.output_tokens.unwrap_or(0);
        self.cached += usage.cache_read_tokens.unwrap_or(0);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoopOutcome {
    Completed,
    Cancelled {
        reason: CancelReason,
    },
    Failed {
        kind: String,
        model_error: Option<ModelError>,
    },
    #[serde(other)]
    Unknown,
}

impl LoopOutcome {
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed)
    }

    /// How the turn ended, in words: `interrupted`, `failed · provider
    /// unavailable`.
    pub fn label(&self) -> String {
        match self {
            Self::Completed => "completed".into(),
            Self::Cancelled { reason } => match reason {
                CancelReason::User => "interrupted".into(),
                CancelReason::Shutdown => "cancelled · session closed".into(),
                CancelReason::OwnerDropped => "cancelled · owner went away".into(),
                CancelReason::Deadline => "cancelled · deadline reached".into(),
                CancelReason::Unknown => "cancelled".into(),
            },
            Self::Failed { kind, model_error } => {
                format!(
                    "failed · {}",
                    Failure::new(kind, model_error.as_ref()).title
                )
            }
            Self::Unknown => "unknown outcome".into(),
        }
    }

    /// The friendly account of a failed turn.
    pub fn failure(&self) -> Option<Failure> {
        match self {
            Self::Failed { kind, model_error } => Some(Failure::new(kind, model_error.as_ref())),
            _ => None,
        }
    }

    /// What the provider reported for a failed model call.
    pub fn provider(&self) -> Option<&ProviderError> {
        match self {
            Self::Failed {
                model_error: Some(error),
                ..
            } => error.provider.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    User,
    OwnerDropped,
    Shutdown,
    Deadline,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelError {
    pub kind: String,
    pub delivery: Option<Delivery>,
    pub retry_after_millis: Option<u64>,
    // Boxed so a turn-end block stays small.
    pub provider: Option<Box<ProviderError>>,
}

/// How far the request got before the call failed.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    NotStarted,
    Started,
    #[serde(other)]
    Unknown,
}

/// The provider's own account of a failure; the field names are also the
/// phone's `failure` keys.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ProviderError {
    /// `429 rate_limit_exceeded/requests`: the status, the code, and the type
    /// when it adds to the code.
    pub fn identifiers(&self) -> Option<String> {
        let name = match (&self.code, &self.kind) {
            (Some(code), Some(kind)) if code != kind => Some(format!("{code}/{kind}")),
            (Some(name), _) | (None, Some(name)) => Some(name.clone()),
            (None, None) => None,
        };
        let parts: Vec<String> = self
            .status
            .map(|s| s.to_string())
            .into_iter()
            .chain(name)
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// The authoritative `turn.wait` result.
#[derive(Clone, Debug, Deserialize)]
pub struct TurnWaitResult {
    pub turn: TurnRef,
    pub outcome: LoopOutcome,
    pub usage: Option<Usage>,
    pub requests: Option<u32>,
    pub tool_rounds: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_roundtrips_every_agent_level() {
        for (level, wire) in [
            (Reasoning::Auto, "auto"),
            (Reasoning::Disabled, "disabled"),
            (Reasoning::Low, "low"),
            (Reasoning::Medium, "medium"),
            (Reasoning::High, "high"),
            (Reasoning::XHigh, "xhigh"),
            (Reasoning::Max, "max"),
            (Reasoning::Ultra, "ultra"),
        ] {
            assert_eq!(serde_json::to_value(level).unwrap(), json!(wire));
            assert_eq!(Reasoning::parse(wire), Some(level));
        }
    }

    #[test]
    fn unknown_session_status_degrades_instead_of_failing() {
        let state: SessionState = serde_json::from_value(json!({
            "session_id": "s", "status": "teleporting"
        }))
        .unwrap();
        assert_eq!(state.status, SessionStatus::Unknown);
    }

    #[test]
    fn unknown_outcome_and_cancel_reason_are_accepted() {
        let wait: TurnWaitResult = serde_json::from_value(json!({
            "turn": {"session_id": "s", "loop_id": "l"},
            "outcome": {"type": "cancelled", "reason": "quantum"}
        }))
        .unwrap();
        assert!(matches!(
            wait.outcome,
            LoopOutcome::Cancelled {
                reason: CancelReason::Unknown
            }
        ));
        let future: TurnWaitResult = serde_json::from_value(json!({
            "turn": {"session_id": "s", "loop_id": "l"},
            "outcome": {"type": "reticulating"}
        }))
        .unwrap();
        assert_eq!(future.outcome, LoopOutcome::Unknown);
        assert_eq!(future.tool_rounds, None);
    }

    fn failed(model_error: Value) -> LoopOutcome {
        serde_json::from_value(
            json!({"type": "failed", "kind": "model", "model_error": model_error}),
        )
        .unwrap()
    }

    #[test]
    fn a_model_error_carries_the_provider_account_when_the_agent_sends_it() {
        let outcome = failed(json!({"kind": "rate_limited", "delivery": "not_started",
            "retryable": true, "retry_after_millis": 12000,
            "provider": {"status": 429, "code": "rate_limit_exceeded", "type": "requests",
                "message": "Rate limit reached\nfor gpt"}}));
        let LoopOutcome::Failed {
            model_error: Some(error),
            ..
        } = &outcome
        else {
            panic!("expected a model error");
        };
        assert_eq!(error.delivery, Some(Delivery::NotStarted));
        assert_eq!(error.retry_after_millis, Some(12000));
        let provider = outcome.provider().unwrap();
        assert_eq!(
            provider.message.as_deref(),
            Some("Rate limit reached\nfor gpt")
        );
        assert_eq!(
            provider.identifiers().as_deref(),
            Some("429 rate_limit_exceeded/requests")
        );
    }

    #[test]
    fn a_model_error_without_provider_fields_still_parses() {
        // Records written before the Agent reported providers carry only a kind.
        let old = failed(json!({"kind": "timeout"}));
        assert_eq!(old.provider(), None);
        assert_eq!(old.label(), "failed · timed out");
        let sparse = failed(json!({"kind": "timeout", "retry_after_millis": null,
            "delivery": "someday", "provider": {"status": 504}}));
        assert_eq!(
            sparse.provider().unwrap().identifiers().as_deref(),
            Some("504")
        );
        let LoopOutcome::Failed {
            model_error: Some(error),
            ..
        } = sparse
        else {
            panic!("expected a model error");
        };
        assert_eq!(error.delivery, Some(Delivery::Unknown));
        assert_eq!(error.retry_after_millis, None);
    }

    #[test]
    fn identifiers_show_the_type_only_when_it_adds_to_the_code() {
        let ids = |status, code: Option<&str>, kind: Option<&str>| {
            ProviderError {
                status,
                code: code.map(Into::into),
                kind: kind.map(Into::into),
                message: None,
            }
            .identifiers()
        };
        assert_eq!(
            ids(Some(429), Some("limit"), Some("limit")).as_deref(),
            Some("429 limit")
        );
        assert_eq!(ids(None, Some("limit"), None).as_deref(), Some("limit"));
        assert_eq!(
            ids(Some(400), None, Some("invalid")).as_deref(),
            Some("400 invalid")
        );
        assert_eq!(ids(None, None, None), None);
    }

    #[test]
    fn handshake_requires_protocol_one_and_used_capabilities() {
        let ping = |capabilities: &[&str]| PingResult {
            version: "0.7.0".into(),
            protocol_version: 1,
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
        };
        assert_eq!(validate_backend(&ping(REQUIRED_CAPABILITIES)), Ok(()));
        assert!(matches!(
            validate_backend(&ping(&[REQUIRED_CAPABILITIES[0]])),
            Err(BackendError::MissingCapabilities { .. })
        ));
        let mut wrong = ping(REQUIRED_CAPABILITIES);
        wrong.protocol_version = 2;
        assert!(matches!(
            validate_backend(&wrong),
            Err(BackendError::ProtocolVersion { found: 2, .. })
        ));
    }

    #[test]
    fn response_error_carries_kind_and_retryability() {
        let frame = br#"{"jsonrpc":"2.0","id":7,"error":{"code":-32003,"message":"busy","data":{"kind":"session_busy","retryable":true}}}"#;
        let IncomingFrame::Response(response) = parse_frame(frame).unwrap() else {
            panic!("expected a response");
        };
        let error = response.error.unwrap();
        assert_eq!(response.id, RequestId(7));
        assert_eq!(error.code, SESSION_BUSY);
        assert_eq!(error.kind, "session_busy");
        assert!(error.retryable && error.is_busy());
    }

    #[test]
    fn usage_totals_ignore_unknown_fields_instead_of_zero_filling() {
        let usage: Usage = serde_json::from_value(json!({"input_tokens": 10})).unwrap();
        let mut totals = UsageTotals::default();
        totals.add(&usage);
        assert_eq!(totals.input, 10);
        assert_eq!(totals.output, 0);
    }
}
