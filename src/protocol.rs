//! Wire DTOs for the fixed minicore-agent 0.5 / Protocol v1 backend over
//! stdio JSON-RPC.
//!
//! Responses intentionally ignore unknown fields so patch releases can add
//! read-only data. Outbound request structs are explicit and only serialize
//! fields owned by this client.
//!
//! The authoritative read DTOs and chunk decoder live in [`read`]; the legacy
//! `session.history` display DTOs remain only for compatibility display and
//! are not a second main-history path.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod changes;
pub mod read;
pub mod tool;
pub mod workspace;

pub use read::{
    Assembled, ChunkAssembler, EncodedHistoryItem, MAX_AUTO_ITEM_BYTES, RawHistoryItem, ReadChunk,
    ReadCursor, ReadError, ReadSessionResult, ReadTurnSummary, RuntimeAssistantItem,
    RuntimeAssistantPart, RuntimeItem, RuntimeSummaryItem, RuntimeToolOutput,
    RuntimeToolResultItem, RuntimeUserInput, RuntimeUserItem, RuntimeUserKind, SnapshotPin,
    TurnAvailability, TurnResultPage,
};

pub const JSONRPC_VERSION: &str = "2.0";

pub const METHOD_PING: &str = "agent.ping";
pub const METHOD_RELOAD: &str = "agent.reload";
pub const METHOD_LIST_MODELS: &str = "model.list";
pub const METHOD_LIST_PROFILES: &str = "profile.list";
pub const METHOD_LIST_SESSIONS: &str = "session.list";
pub const METHOD_SESSION_CREATE: &str = "session.create";
pub const METHOD_SESSION_OPEN: &str = "session.open";
pub const METHOD_SESSION_CLOSE: &str = "session.close";
pub const METHOD_SESSION_DELETE: &str = "session.delete";
pub const METHOD_SESSION_STATE: &str = "session.state";
pub const METHOD_SESSION_UPDATE: &str = "session.update";
pub const METHOD_SESSION_RENAME: &str = "session.rename";
pub const METHOD_SESSION_HISTORY: &str = "session.history";
pub const METHOD_SESSION_PRESENTATION: &str = "session.presentation";
pub const METHOD_GET_HISTORY: &str = METHOD_SESSION_HISTORY;
pub const METHOD_TURN_SEND: &str = "turn.send";
pub const METHOD_TURN_CANCEL: &str = "turn.cancel";
pub const METHOD_TURN_WAIT: &str = "turn.wait";
pub const METHOD_TURN_STEER: &str = "turn.steer";
pub const METHOD_TURN_RESULT: &str = "turn.result";
pub const METHOD_SESSION_READ: &str = "session.read";
pub const METHOD_SESSION_CONTEXT: &str = "session.context";
pub const METHOD_SESSION_COMPACT: &str = "session.compact";
pub const METHOD_SESSION_COMPACT_CANCEL: &str = "session.compact.cancel";
pub const METHOD_TOOL_READ: &str = "tool.read";
pub const METHOD_TOOL_OUTPUT: &str = "tool.output";
pub const METHOD_WORKSPACE_READ: &str = "workspace.read";
pub const METHOD_WORKSPACE_FILES: &str = "workspace.files";
pub const METHOD_WORKSPACE_SEARCH: &str = "workspace.search";
pub const METHOD_WORKSPACE_STATUS: &str = "workspace.status";
pub const METHOD_CHANGES_LIST: &str = "changes.list";
pub const METHOD_CHANGES_DIFF: &str = "changes.diff";
pub const METHOD_SHUTDOWN: &str = "agent.shutdown";

/// Default page for the main history / result reads (spec §6.3).
pub const READ_PAGE_LIMIT: usize = 20;
pub const READ_PAGE_MAX_BYTES: usize = 262_144;
/// The tail window opened by default for a long session (spec §6.3).
pub const READ_TAIL_ITEMS: usize = 200;
/// The one-item probe that establishes a fresh pin/`total` before a windowed
/// read (spec §6.3 step 1). It must start at cursor 0: a non-zero cursor
/// without a pin is an illegal request.
pub const READ_PROBE_LIMIT: usize = 1;
pub const READ_PROBE_MAX_BYTES: usize = 65_536;

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
pub const PROVIDER_ERROR: i64 = -32_012;
pub const RUNTIME_ERROR: i64 = -32_013;
pub const INVALID_SESSION_SETTINGS: i64 = -32_014;
pub const HISTORY_TOO_LARGE: i64 = -32_015;
pub const STEER_QUEUE_FULL: i64 = -32_016;

pub const DEFAULT_HISTORY_LIMIT: usize = 20;
pub const MAX_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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
    pub fn reload(id: RequestId) -> Self {
        Self::new(id, METHOD_RELOAD, json!({}))
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

    pub fn session_rename(id: RequestId, session_id: &str, title: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_RENAME,
            serde_json::to_value(SessionRenameParams {
                session_id: session_id.to_owned(),
                title: title.to_owned(),
            })
            .expect("session.rename params serialize"),
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

    pub fn session_presentation(id: RequestId, session_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_PRESENTATION,
            json!({ "session_id": session_id }),
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

    /// `session.read` (spec §6.3). A non-zero cursor requires the pin fields,
    /// so callers that start mid-history must pass the pin they are pinned to.
    pub fn session_read(
        id: RequestId,
        session_id: &str,
        cursor: Option<ReadCursor>,
        limit: usize,
        max_bytes: usize,
        pin: Option<&SnapshotPin>,
    ) -> Self {
        let mut params = serde_json::Map::new();
        params.insert("session_id".into(), json!(session_id));
        if let Some(cursor) = cursor {
            params.insert("cursor".into(), json!(cursor));
        }
        params.insert("limit".into(), json!(limit));
        params.insert("max_bytes".into(), json!(max_bytes));
        if let Some(pin) = pin {
            params.insert("captured_end".into(), json!(pin.captured_end));
            params.insert("history_revision".into(), json!(pin.history_revision));
        }
        Self::new(id, METHOD_SESSION_READ, Value::Object(params))
    }

    pub fn session_display_read(
        id: RequestId,
        session_id: &str,
        cursor: Option<ReadCursor>,
        limit: usize,
        max_bytes: usize,
        pin: Option<&SnapshotPin>,
    ) -> Self {
        let mut request = Self::session_read(id, session_id, cursor, limit, max_bytes, pin);
        request.params["view"] = json!("display");
        if let Some(projection) = pin.and_then(|pin| pin.projection.as_ref()) {
            request.params["projection_revision"] = json!(projection.revision);
        }
        request
    }

    pub fn turn_result(
        id: RequestId,
        turn: &TurnRef,
        cursor: Option<ReadCursor>,
        limit: usize,
        max_bytes: usize,
    ) -> Self {
        let mut params = serde_json::Map::new();
        params.insert("turn".into(), json!(turn));
        if let Some(cursor) = cursor {
            params.insert("cursor".into(), json!(cursor));
        }
        params.insert("limit".into(), json!(limit));
        params.insert("max_bytes".into(), json!(max_bytes));
        Self::new(id, METHOD_TURN_RESULT, Value::Object(params))
    }

    pub fn session_context(id: RequestId, session_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_CONTEXT,
            json!({"session_id": session_id}),
        )
    }

    pub fn session_compact(id: RequestId, session_id: &str, operation_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_COMPACT,
            json!({"session_id": session_id, "operation_id": operation_id}),
        )
    }

    pub fn session_compact_cancel(id: RequestId, session_id: &str, operation_id: &str) -> Self {
        Self::new(
            id,
            METHOD_SESSION_COMPACT_CANCEL,
            json!({"session_id": session_id, "operation_id": operation_id}),
        )
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
    pub fn parse_reload(&self) -> Result<ReloadResult, RpcResponseError> {
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
    pub fn parse_session_context(&self) -> Result<SessionContextWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_session_compact(&self) -> Result<CompactResultWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_session_update(&self) -> Result<SessionUpdateResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_session_rename(&self) -> Result<SessionResult, RpcResponseError> {
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
    pub fn parse_session_read(&self) -> Result<ReadSessionResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_turn_result_page(&self) -> Result<TurnResultPage, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_session_presentation(&self) -> Result<SessionPresentationWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_turn_send(&self) -> Result<TurnResult, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_turn_wait(&self) -> Result<TurnResultViewWire, RpcResponseError> {
        self.result_as()
    }
    pub fn parse_steer(&self) -> Result<SteerResult, RpcResponseError> {
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

/// One decoded `agent.event`. The tool-fact variants box their payloads: they
/// are the largest on the wire and are decoded once per frame.
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
    RequestUsage {
        data: RequestUsageDataWire,
    },
    SteerProgress {
        data: SteerProgressDataWire,
    },
    OutputDelta {
        data: OutputDeltaDataWire,
    },
    ToolArgumentsPreview {
        data: Box<ToolArgumentsPreviewDataWire>,
    },
    ToolStarted {
        data: ToolStartedDataWire,
    },
    ToolPresentation {
        data: ToolPresentationDataWire,
    },
    ToolProgress {
        data: ToolProgressDataWire,
    },
    ToolInvocation {
        data: Box<ToolInvocationDataWire>,
    },
    ToolExecution {
        data: Box<ToolExecutionDataWire>,
    },
    ToolProcess {
        data: Box<ToolProcessDataWire>,
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
/// Real per-request usage reported by the Agent while its loop is running
/// (read-only; the TUI never treats it as a promise of the final total).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RequestUsageDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub usage: UsageWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SteerProgressDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    /// Number of Steering User items present in the prepared prompt history
    /// at this request boundary (authoritative; may lag the ACK counter).
    pub applied_count: u64,
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
/// Optional, replace-only presentation snapshot. Never a validated invocation.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolArgumentsPreviewDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub attempt: u64,
    pub revision: u64,
    pub state: ToolArgumentsPreviewStateWire,
    pub partial: bool,
    pub display: ToolDisplayWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolArgumentsPreviewStateWire {
    Generating,
    Generated,
    Discarded,
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
pub struct ToolPresentationDataWire {
    pub turn: TurnRef,
    pub request_index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub display: ToolDisplayWire,
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

/// `tool_invocation` (spec §7): the sanitized call fact. Raw input is a bounded
/// summary, never the full arguments again.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolInvocationDataWire {
    pub turn: TurnRef,
    pub data: ToolInvocationWire,
    pub meta: EventMetaWire,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ToolInvocationWire {
    pub tool_ref: ToolRefWire,
    pub name: String,
    pub subject: ToolSubjectWire,
    pub subject_truncated: bool,
    pub input: ToolInputSummaryWire,
}

/// `tool_execution` (spec §7): lifecycle state and per-stream availability.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolExecutionDataWire {
    pub turn: TurnRef,
    pub data: ToolExecutionWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ToolExecutionWire {
    pub tool_ref: ToolRefWire,
    pub name: String,
    pub state: ToolExecutionStateWire,
    #[serde(default)]
    pub phase: Option<ToolPhaseWire>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub outcome: Option<ToolOutcomeWire>,
    pub input_availability: ToolDataAvailabilityWire,
    pub output_availability: ToolDataAvailabilityWire,
    #[serde(default)]
    pub output_line_count: Option<usize>,
    pub input_bytes: usize,
    pub result_bytes: usize,
    pub input_truncated: bool,
    pub result_truncated: bool,
    #[serde(default)]
    pub command: Option<CommandResultWire>,
    pub recording: ToolRecordingStateWire,
}

/// `tool_process` (spec §7): a raw stdout/stderr chunk notice. Offsets count
/// raw bytes, never base64 positions.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolProcessDataWire {
    pub turn: TurnRef,
    pub data: ToolProcessWire,
    pub meta: EventMetaWire,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolProcessWire {
    pub tool_ref: ToolRefWire,
    #[serde(default)]
    pub chunk: Option<ToolProcessChunkWire>,
    #[serde(default)]
    pub command: Option<CommandResultWire>,
}

#[derive(Clone, PartialEq, Deserialize)]
pub struct ToolProcessChunkWire {
    pub stream: ToolDataStreamWire,
    pub encoding: String,
    pub data: String,
    pub base_offset: u64,
    pub next_offset: u64,
    pub observed_end: u64,
    pub dropped: bool,
    pub expired: bool,
}

/// The complete tool identity: session, loop, request, and call id. A tool name
/// or path is never a key because all of those repeat (spec §7).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolRefWire {
    pub session_id: String,
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSubjectWire {
    File { path: String },
    Command { script: String, cwd: String },
    Other,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ToolInputSummaryWire {
    pub total_bytes: usize,
    pub preview: String,
    pub truncated: bool,
    pub encoding: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStateWire {
    Requested,
    AwaitingPolicy,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    InputProvided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhaseWire {
    Reading,
    Writing,
    Matching,
    Committing,
    Running,
}

/// Per-stream availability. `unavailable` (never observed) is distinct from a
/// real empty result, and `expired`/`partial` must not be reported as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDataAvailabilityWire {
    Pending,
    Unavailable,
    Available,
    Partial,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDataStreamWire {
    Input,
    Output,
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRecordingStateWire {
    MemoryOnly,
    Saved,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CommandResultWire {
    pub status: CommandStatusWire,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    pub termination_confirmed: bool,
    pub stdout_base_offset: u64,
    pub stdout_observed_end: u64,
    pub stderr_base_offset: u64,
    pub stderr_observed_end: u64,
    /// True only when both streams reached a real end of output.
    pub output_complete: bool,
    /// True when bytes were dropped or observation was cut short.
    pub output_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatusWire {
    Running,
    Cancelling,
    Exited,
    Cancelled,
    TimedOut,
    SpawnFailed,
    Failed,
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

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolProgressWire {
    pub message: Option<String>,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

impl fmt::Debug for ToolProgressWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolProgressWire")
            .field("message_bytes", &self.message.as_ref().map(String::len))
            .field("completed", &self.completed)
            .field("total", &self.total)
            .finish()
    }
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

#[derive(Clone, PartialEq, Deserialize, Serialize)]
pub struct ToolResultWire {
    pub outcome: ToolOutcomeWire,
    pub content_bytes: usize,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub content_truncated: bool,
}

impl fmt::Debug for ToolResultWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolResultWire")
            .field("outcome", &self.outcome)
            .field("content_bytes", &self.content_bytes)
            .field("content_len", &self.content.as_ref().map(String::len))
            .field("content_truncated", &self.content_truncated)
            .finish()
    }
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
    /// Required by the pinned backend; a response without it is a protocol
    /// error, not a fallback to the legacy Agent 0.3 behavior (spec §4.1).
    pub protocol_version: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// The protocol version this TUI speaks (spec §4.1).
pub const PROTOCOL_VERSION: u32 = 1;

/// Capabilities the TUI requires before it will execute anything. The list is
/// fixed by the pinned Agent 0.5 baseline; `session.compact` is deliberately
/// absent because the backend does not advertise it as a capability and this
/// TUI must not invent one (spec §4.1).
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    "session.read",
    "session.read.display",
    "tool.read.display",
    "turn.result",
    "session.context",
    "tool.read",
    "tool.output",
    "workspace.read",
    "workspace.files",
    "workspace.search",
    "workspace.status",
    "changes.list",
    "changes.diff",
    "deferred.waiter_limit",
];

/// Why the connected backend cannot be used. Every variant is a definite
/// incompatibility: the TUI reports it and stops rather than falling back to a
/// different protocol generation (spec §4.1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error(
        "unsupported agent protocol version {found}: minicore-tui requires protocol_version {required}"
    )]
    ProtocolVersion { found: u32, required: u32 },
    #[error("agent {version} is missing required capabilities: {missing}")]
    MissingCapabilities { version: String, missing: String },
}

/// Validates the `agent.ping` handshake against the fixed baseline. The
/// package `version` is advisory; compatibility is decided by
/// `protocol_version` plus the required capability set (spec §4.1).
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
    if !missing.is_empty() {
        return Err(BackendError::MissingCapabilities {
            version: ping.version.clone(),
            missing: missing.join(", "),
        });
    }
    Ok(())
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRenameParams {
    pub session_id: String,
    pub title: String,
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
    #[serde(default)]
    pub accepted_at: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OkResultWire {
    pub ok: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReloadResult {
    pub ok: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SteerResult {
    pub ok: bool,
    #[serde(default)]
    pub accepted_at: Option<String>,
    /// 1-based FIFO acceptance index within the loop; absent on older Agents
    /// that predate steer receipts. Progress receipts compare against it.
    #[serde(default)]
    pub steer_index: Option<u64>,
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
    /// Agent-owned manual/post-turn compaction. It keeps an otherwise idle
    /// Session busy; active-turn emergency recovery uses `SessionContext.recovery`.
    #[serde(default)]
    pub compaction: Option<CompactionProgressWire>,
}

/// The Agent's Session operation progress (spec §7.3), not a Runtime stage.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CompactionProgressWire {
    pub operation_id: String,
    pub phase: CompactionPhaseWire,
    pub covered_item_count: usize,
    pub retained_item_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ContextCoverageWire {
    pub covered_loop_count: usize,
    pub covered_item_count: usize,
    pub retained_item_count: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ContextBudgetWire {
    #[serde(default)]
    pub estimated_history_items: Option<u64>,
    #[serde(default)]
    pub estimated_history_bytes: Option<u64>,
    #[serde(default)]
    pub estimated_history_tokens: Option<u64>,
    #[serde(default)]
    pub estimated_request_context_tokens: Option<u64>,
    #[serde(default)]
    pub input_budget_tokens: Option<u64>,
    #[serde(default)]
    pub trigger_tokens: Option<u64>,
    #[serde(default)]
    pub target_tokens: Option<u64>,
    #[serde(default)]
    pub max_history_items: Option<u64>,
    #[serde(default)]
    pub max_history_bytes: Option<u64>,
    #[serde(default)]
    pub within_runtime_limits: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AutomaticContextOperationWire {
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub loop_id: Option<String>,
    #[serde(default)]
    pub request_index: Option<u32>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub before_tokens: Option<u64>,
    #[serde(default)]
    pub after_tokens: Option<u64>,
    #[serde(default)]
    pub hard_tokens: Option<u64>,
    #[serde(default)]
    pub trigger_tokens: Option<u64>,
    #[serde(default)]
    pub target_tokens: Option<u64>,
    #[serde(default)]
    pub utility_before_tokens: Option<u64>,
    #[serde(default)]
    pub utility_after_tokens: Option<u64>,
    #[serde(default)]
    pub utility_usage: Option<CompactUtilityUsageWire>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AutomaticContextWire {
    #[serde(default)]
    pub current: Option<AutomaticContextOperationWire>,
    #[serde(default)]
    pub last: Option<AutomaticContextOperationWire>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ContextRecoveryWire {
    pub loop_id: String,
    pub request_index: u32,
    #[serde(default)]
    pub before_tokens: Option<u64>,
    #[serde(default)]
    pub after_tokens: Option<u64>,
    #[serde(default)]
    pub utility_usage: Option<CompactUtilityUsageWire>,
    pub outcome: String,
    #[serde(default)]
    pub failure_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SessionContextWire {
    pub session_id: String,
    #[serde(default)]
    pub current_operation: Option<CompactionProgressWire>,
    pub coverage: ContextCoverageWire,
    #[serde(default)]
    pub last_result: Option<CompactResultWire>,
    pub budget: ContextBudgetWire,
    pub automatic: AutomaticContextWire,
    #[serde(default)]
    pub last_prepare_failure: Option<String>,
    #[serde(default)]
    pub recovery: Option<ContextRecoveryWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactStatusWire {
    Compacted,
    Noop,
    Failed,
    UnknownWrite,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CompactUtilityUsageWire {
    pub call_count: u32,
    pub complete: bool,
    #[serde(default)]
    pub usage: Option<UsageWire>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CompactResultWire {
    pub operation_id: String,
    #[serde(default)]
    pub origin: Option<CompactOriginWire>,
    pub status: CompactStatusWire,
    #[serde(default)]
    pub before_tokens: Option<u64>,
    #[serde(default)]
    pub after_tokens: Option<u64>,
    #[serde(default)]
    pub covered_loop_count: Option<usize>,
    #[serde(default)]
    pub covered_item_count: Option<usize>,
    #[serde(default)]
    pub retained_item_count: Option<usize>,
    #[serde(default)]
    pub failure_kind: Option<String>,
    #[serde(default)]
    pub utility_usage: Option<CompactUtilityUsageWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactOriginWire {
    Manual,
    Automatic,
    #[serde(other)]
    Unknown,
}

impl CompactResultWire {
    /// Older Agents reserved `auto-` for automatic compaction operations.
    /// Other IDs alone cannot establish an operation's origin.
    pub fn origin_label(&self) -> &'static str {
        match self.origin {
            Some(CompactOriginWire::Manual) => "manual",
            Some(CompactOriginWire::Automatic) => "automatic",
            Some(CompactOriginWire::Unknown) => "unknown",
            None if self.operation_id.starts_with("auto-") => "automatic",
            None => "unknown",
        }
    }
}

/// `preparing` means compaction has NOT started model work yet: the session is
/// not idle and must not be treated as available for a new turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPhaseWire {
    Preparing,
    Summarizing,
    Merging,
    Committing,
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
pub struct LocalContextBudgetFailureWire {
    pub estimated_tokens: u64,
    pub input_budget_tokens: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ModelErrorWire {
    pub kind: String,
    pub delivery: String,
    pub retryable: bool,
    pub retry_after_millis: Option<u64>,
    #[serde(default)]
    pub local_context_budget: Option<LocalContextBudgetFailureWire>,
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
    #[serde(default)]
    pub usage: Option<UsageWire>,
    #[serde(default)]
    pub requests: Option<u32>,
    #[serde(default)]
    pub tool_rounds: Option<u64>,
    #[serde(default)]
    pub final_config_revision: Option<u64>,
    #[serde(default)]
    pub persistence: Option<TurnPersistenceWire>,
    #[serde(default)]
    pub accepted_at: Option<String>,
    /// Completion time is distinct from prompt/steer acceptance time. The
    /// wait DTO normally does not carry it; `turn.result` may.
    #[serde(default)]
    pub completed_at: Option<String>,
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
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct UserHistoryViewWire {
    pub loop_id: String,
    pub kind: UserMessageKindWire,
    pub text: String,
    #[serde(default)]
    pub timestamp: Option<String>,
}
impl fmt::Debug for UserHistoryViewWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserHistoryViewWire")
            .field("loop_id", &self.loop_id)
            .field("kind", &self.kind)
            .field("text_bytes", &self.text.len())
            .field("timestamp", &self.timestamp)
            .finish()
    }
}
#[derive(Clone, PartialEq, Deserialize)]
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
    #[serde(default)]
    pub parts: Option<Vec<AssistantDisplayPartWire>>,
}
impl fmt::Debug for AssistantHistoryViewWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssistantHistoryViewWire")
            .field("loop_id", &self.loop_id)
            .field("request_index", &self.request_index)
            .field("model", &self.model)
            .field("reasoning_level", &self.reasoning_level)
            .field("text_bytes", &self.text.len())
            .field("reasoning_bytes", &self.reasoning.len())
            .field("tool_call_count", &self.tool_calls.len())
            .field("usage", &self.usage)
            .field("finish_reason", &self.finish_reason)
            .field("part_count", &self.parts.as_ref().map(Vec::len))
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolCallViewWire {
    pub tool_call_id: String,
    pub name: String,
    #[serde(default)]
    pub call_index: u32,
    #[serde(default)]
    pub display: Option<ToolDisplayWire>,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ToolResultHistoryViewWire {
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: ToolOutcomeWire,
    pub content: String,
    #[serde(default)]
    pub content_truncated: bool,
}
impl fmt::Debug for ToolResultHistoryViewWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolResultHistoryViewWire")
            .field("loop_id", &self.loop_id)
            .field("request_index", &self.request_index)
            .field("tool_call_id", &self.tool_call_id)
            .field("tool_name", &self.tool_name)
            .field("outcome", &self.outcome)
            .field("content_len", &self.content.len())
            .field("content_truncated", &self.content_truncated)
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SummaryHistoryViewWire {
    pub content: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolDisplayWire {
    #[serde(default)]
    pub body_truncated: bool,
    pub detail: String,
    #[serde(default)]
    pub expanded_input: Option<String>,
    #[serde(default)]
    pub input_line_count: Option<usize>,
    #[serde(default)]
    pub hidden_line_count: Option<usize>,
    #[serde(default)]
    pub truncated: bool,
}
impl fmt::Debug for ToolDisplayWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolDisplayWire")
            .field("detail_bytes", &self.detail.len())
            .field(
                "expanded_input_bytes",
                &self.expanded_input.as_ref().map(String::len),
            )
            .field("input_line_count", &self.input_line_count)
            .field("hidden_line_count", &self.hidden_line_count)
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AssistantDisplayPartWire {
    Text { text: String },
    Reasoning { text: String },
    ToolCall { tool_call_id: String, name: String },
}
impl fmt::Debug for AssistantDisplayPartWire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { text } => formatter
                .debug_struct("AssistantDisplayPartWire::Text")
                .field("text_bytes", &text.len())
                .finish(),
            Self::Reasoning { text } => formatter
                .debug_struct("AssistantDisplayPartWire::Reasoning")
                .field("text_bytes", &text.len())
                .finish(),
            Self::ToolCall { tool_call_id, name } => formatter
                .debug_struct("AssistantDisplayPartWire::ToolCall")
                .field("tool_call_id", tool_call_id)
                .field("name", name)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKindWire {
    Reported,
    Estimated,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ContextUsageWire {
    #[serde(default)]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub window: Option<u64>,
    #[serde(default)]
    pub percent: Option<f64>,
    pub kind: ContextKindWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LastLoopWire {
    pub loop_id: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionPresentationWire {
    pub session_id: String,
    #[serde(default)]
    pub model_label: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    pub context: ContextUsageWire,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub using_subscription: Option<bool>,
    #[serde(default)]
    pub last_loop: Option<LastLoopWire>,
    /// Latest steering receipt committed at a real Model.start; used for
    /// lost-event reconciliation after a dropped steer_progress event.
    #[serde(default)]
    pub steer_progress: Option<SteerProgressViewWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SteerProgressViewWire {
    pub loop_id: String,
    pub request_index: u32,
    pub applied_count: u64,
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
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
    Ultra,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_usage_decodes_reported_and_preserves_older_kinds() {
        for (kind, expected) in [
            ("reported", ContextKindWire::Reported),
            ("estimated", ContextKindWire::Estimated),
            ("unknown", ContextKindWire::Unknown),
            ("future_kind", ContextKindWire::Unknown),
        ] {
            let context: ContextUsageWire = serde_json::from_value(json!({
                "kind": kind, "tokens": 5_000, "window": 100_000, "percent": 5.0
            }))
            .unwrap();
            assert_eq!(context.kind, expected);
            assert_eq!(context.tokens, Some(5_000));
            assert_eq!(context.window, Some(100_000));
            assert_eq!(context.percent, Some(5.0));
        }
    }

    #[test]
    fn reasoning_wire_roundtrips_all_agent_levels() {
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
            assert_eq!(
                serde_json::to_value(level).unwrap(),
                serde_json::json!(wire),
                "serialize {level:?}"
            );
            assert_eq!(
                serde_json::from_value::<Reasoning>(serde_json::json!(wire)).unwrap(),
                level,
                "deserialize {wire}"
            );
        }
    }

    #[test]
    fn backend_validation_requires_protocol_v1_and_required_capabilities() {
        let full = |capabilities: Vec<&str>| PingResult {
            version: "0.5.0".into(),
            protocol_version: 1,
            capabilities: capabilities.into_iter().map(str::to_owned).collect(),
        };
        let all: Vec<&str> = REQUIRED_CAPABILITIES.to_vec();
        assert_eq!(validate_backend(&full(all.clone())), Ok(()));

        // Package versions are advisory; the complete capability contract is required.
        let ping: PingResult = serde_json::from_value(json!({
            "version": "0.5.0",
            "protocol_version": 1,
            "capabilities": REQUIRED_CAPABILITIES,
        }))
        .unwrap();
        assert_eq!(validate_backend(&ping), Ok(()));

        let wrong_protocol = PingResult {
            version: "0.5.0".into(),
            protocol_version: 2,
            capabilities: all.clone().into_iter().map(str::to_owned).collect(),
        };
        assert!(matches!(
            validate_backend(&wrong_protocol),
            Err(BackendError::ProtocolVersion {
                found: 2,
                required: 1
            })
        ));

        for capability in ["session.read.display", "tool.read.display"] {
            let mut missing = all.clone();
            missing.retain(|value| *value != capability);
            let BackendError::MissingCapabilities { missing, .. } =
                validate_backend(&full(missing)).unwrap_err()
            else {
                panic!("missing display capability")
            };
            assert_eq!(missing, capability);
        }
        let mut missing = all.clone();
        missing.retain(|capability| *capability != "turn.result");
        assert!(matches!(
            validate_backend(&full(missing)),
            Err(BackendError::MissingCapabilities { .. })
        ));
    }

    #[test]
    fn ping_requires_protocol_version_in_the_wire_result() {
        let bare: Result<PingResult, _> = serde_json::from_value(json!({"version": "0.5.0"}));
        assert!(
            bare.is_err(),
            "a ping without protocol_version must be a protocol error"
        );
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
    fn reload_request_and_result_are_strict() {
        let value = serde_json::to_value(OutgoingRequest::reload(RequestId(2))).unwrap();
        assert_eq!(
            value,
            json!({"jsonrpc":"2.0","id":2,"method":"agent.reload","params":{}})
        );
        let response = RpcResponse {
            id: RequestId(2),
            result: Some(json!({"ok": true})),
            error: None,
        };
        assert_eq!(response.parse_reload().unwrap(), ReloadResult { ok: true });
        let extra = RpcResponse {
            id: RequestId(2),
            result: Some(json!({"ok": true, "extra": 1})),
            error: None,
        };
        assert!(matches!(
            extra.parse_reload(),
            Err(RpcResponseError::Parse(_))
        ));
        let false_result = RpcResponse {
            id: RequestId(2),
            result: Some(json!({"ok": false})),
            error: None,
        };
        assert_eq!(
            false_result.parse_reload().unwrap(),
            ReloadResult { ok: false }
        );
        let missing = RpcResponse {
            id: RequestId(2),
            result: Some(json!({})),
            error: None,
        };
        assert!(matches!(
            missing.parse_reload(),
            Err(RpcResponseError::Parse(_))
        ));
    }

    #[test]
    fn rename_request_has_the_documented_shape() {
        let value = serde_json::to_value(OutgoingRequest::session_rename(
            RequestId(2),
            "ses_1",
            "新标题",
        ))
        .unwrap();
        assert_eq!(
            value,
            json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"session.rename",
                "params":{"session_id":"ses_1","title":"新标题"}
            })
        );
    }
    #[test]
    fn wait_result_is_not_wrapped_in_ok_result() {
        let value = json!({"turn":{"session_id":"ses_1","loop_id":"loop_1"},"outcome":{"type":"completed"},"usage":{},"requests":2,"tool_rounds":1,"final_config_revision":3,"persistence":"persisted"});
        let result: TurnResultViewWire = serde_json::from_value(value).unwrap();
        assert_eq!(result.requests, Some(2));
        assert_eq!(result.persistence, Some(TurnPersistenceWire::Persisted));
    }

    #[test]
    fn presentation_wire_debug_redacts_content_bodies() {
        let display = ToolDisplayWire {
            body_truncated: false,
            detail: "$ cat secret".to_owned(),
            expanded_input: Some("private body".to_owned()),
            input_line_count: Some(1),
            hidden_line_count: Some(2),
            truncated: false,
        };
        let result = ToolResultWire {
            outcome: ToolOutcomeWire::Success,
            content_bytes: 13,
            content: Some("private result".to_owned()),
            content_truncated: false,
        };
        let debug = format!("{display:?} {result:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("private body"));
        assert!(!debug.contains("private result"));
        assert!(debug.contains("detail_bytes"));
        assert!(debug.contains("content_len"));
    }
}
