//! Agent event notifications. Events are hints: every field here is also
//! available from an authoritative read, so ordering across the two Agent
//! queues must never decide correctness.

use serde::Deserialize;
use serde_json::Value;

use super::{
    CompactionPhase, LoopOutcome, PendingInteraction, SessionInfo, SessionState, ToolDisplay,
    ToolExecution, ToolInvocation, ToolRef, TurnPersistence, TurnRef, Usage,
};

#[derive(Clone, Debug, Deserialize)]
pub struct EventMeta {
    pub session_id: String,
    #[serde(default)]
    pub loop_id: Option<String>,
    #[serde(default)]
    pub dropped_before: u64,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    SessionOpened {
        meta: EventMeta,
        session: Box<SessionInfo>,
    },
    SessionClosed {
        meta: EventMeta,
    },
    SessionState {
        meta: EventMeta,
        state: Box<SessionState>,
    },
    TurnStarted {
        meta: EventMeta,
        turn: TurnRef,
    },
    RequestStarted {
        meta: EventMeta,
        turn: TurnRef,
        request_index: u32,
        model: String,
    },
    RequestUsage {
        meta: EventMeta,
        turn: TurnRef,
        usage: Box<Usage>,
    },
    OutputDelta {
        meta: EventMeta,
        turn: TurnRef,
        request_index: u32,
        reasoning: bool,
        delta: String,
    },
    SteerProgress {
        meta: EventMeta,
        turn: TurnRef,
        applied_count: u64,
    },
    ToolStarted {
        meta: EventMeta,
        turn: TurnRef,
        request_index: u32,
        tool_call_id: String,
        tool_name: String,
    },
    ToolPresentation {
        meta: EventMeta,
        call: EventToolCall,
        tool_name: String,
        display: ToolDisplay,
    },
    ToolArgumentsPreview {
        meta: EventMeta,
        call: EventToolCall,
        attempt: u64,
        revision: u64,
        tool_name: String,
        generated: bool,
        discarded: bool,
        display: ToolDisplay,
    },
    ToolInvocation {
        meta: EventMeta,
        invocation: Box<ToolInvocation>,
    },
    ToolExecution {
        meta: EventMeta,
        execution: Box<ToolExecution>,
    },
    ToolProcess {
        meta: EventMeta,
        data: Box<ToolProcess>,
    },
    ToolFinished {
        meta: EventMeta,
        call: EventToolCall,
        outcome: ToolOutcome,
        content: Option<String>,
        content_truncated: bool,
    },
    InteractionRequested {
        meta: EventMeta,
        turn: TurnRef,
        interaction: Box<PendingInteraction>,
    },
    InteractionResolved {
        meta: EventMeta,
        turn: TurnRef,
        interaction_id: String,
    },
    TurnFinished {
        meta: EventMeta,
        turn: TurnRef,
        outcome: LoopOutcome,
        persistence: TurnPersistence,
    },
    /// A notification this client does not model.
    Unknown,
}

/// Identity of one tool call inside the running turn.
#[derive(Clone, Debug, Deserialize)]
pub struct EventToolCall {
    pub turn: TurnRef,
    pub request_index: u32,
    pub tool_call_id: String,
}

impl EventToolCall {
    pub fn key(&self) -> ToolRef {
        ToolRef {
            session_id: self.turn.session_id.clone(),
            loop_id: self.turn.loop_id.clone(),
            request_index: self.request_index,
            tool_call_id: self.tool_call_id.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Success,
    Failed,
    Denied,
    Cancelled,
    InputProvided,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolProcess {
    pub tool_ref: ToolRef,
    #[serde(default)]
    pub chunk: Option<ProcessChunk>,
    #[serde(default)]
    pub command: Option<CommandResult>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProcessChunk {
    pub stream: ProcessStream,
    pub encoding: String,
    pub data: String,
    pub base_offset: u64,
    pub next_offset: u64,
    #[serde(default)]
    pub dropped: bool,
    #[serde(default)]
    pub expired: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStream {
    Stdout,
    Stderr,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CommandResult {
    pub status: CommandStatus,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub termination_confirmed: bool,
    #[serde(default)]
    pub output_complete: bool,
    #[serde(default)]
    pub output_truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Running,
    Cancelling,
    Exited,
    Cancelled,
    TimedOut,
    SpawnFailed,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    SessionOpened {
        data: SessionOpenedData,
    },
    SessionClosed {
        data: MetaOnly,
    },
    SessionState {
        data: SessionStateData,
    },
    TurnStarted {
        data: TurnStartedData,
    },
    RequestStarted {
        data: RequestStartedData,
    },
    RequestUsage {
        data: RequestUsageData,
    },
    OutputDelta {
        data: OutputDeltaData,
    },
    SteerProgress {
        data: SteerProgressData,
    },
    ToolStarted {
        data: ToolStartedData,
    },
    ToolPresentation {
        data: ToolPresentationData,
    },
    ToolArgumentsPreview {
        data: ToolPreviewData,
    },
    ToolInvocation {
        data: ToolInvocationData,
    },
    ToolExecution {
        data: ToolExecutionData,
    },
    ToolProcess {
        data: ToolProcessData,
    },
    ToolFinished {
        data: ToolFinishedData,
    },
    InteractionRequested {
        data: InteractionRequestedData,
    },
    InteractionResolved {
        data: InteractionResolvedData,
    },
    TurnFinished {
        data: TurnFinishedData,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct MetaOnly {
    meta: EventMeta,
}
#[derive(Deserialize)]
struct SessionOpenedData {
    meta: EventMeta,
    session: SessionInfo,
}
#[derive(Deserialize)]
struct SessionStateData {
    meta: EventMeta,
    state: SessionState,
}
#[derive(Deserialize)]
struct TurnStartedData {
    meta: EventMeta,
    turn: TurnRef,
}
#[derive(Deserialize)]
struct RequestStartedData {
    meta: EventMeta,
    turn: TurnRef,
    #[serde(default)]
    request_index: u32,
    #[serde(default)]
    model: String,
}
#[derive(Deserialize)]
struct RequestUsageData {
    meta: EventMeta,
    turn: TurnRef,
    #[serde(default)]
    usage: Usage,
}
#[derive(Deserialize)]
struct OutputDeltaData {
    meta: EventMeta,
    turn: TurnRef,
    #[serde(default)]
    request_index: u32,
    channel: String,
    delta: String,
}
#[derive(Deserialize)]
struct SteerProgressData {
    meta: EventMeta,
    turn: TurnRef,
    #[serde(default)]
    applied_count: u64,
}
#[derive(Deserialize)]
struct ToolStartedData {
    meta: EventMeta,
    turn: TurnRef,
    #[serde(default)]
    request_index: u32,
    tool_call_id: String,
    tool_name: String,
}
#[derive(Deserialize)]
struct ToolPresentationData {
    meta: EventMeta,
    turn: TurnRef,
    request_index: u32,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    display: ToolDisplay,
}
#[derive(Deserialize)]
struct ToolPreviewData {
    meta: EventMeta,
    turn: TurnRef,
    request_index: u32,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    attempt: u64,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    state: String,
    display: ToolDisplay,
}
/// `{turn, data: <invocation record>, meta}`: the record is the payload.
#[derive(Deserialize)]
struct ToolInvocationData {
    meta: EventMeta,
    #[serde(flatten)]
    invocation: ToolInvocation,
}
/// `{turn, data: <execution record>, meta}`: the record is the payload itself.
#[derive(Deserialize)]
struct ToolExecutionData {
    meta: EventMeta,
    #[serde(flatten)]
    execution: ToolExecution,
}
#[derive(Deserialize)]
struct ToolProcessData {
    meta: EventMeta,
    data: ToolProcess,
}
#[derive(Deserialize)]
struct ToolFinishedData {
    meta: EventMeta,
    turn: TurnRef,
    #[serde(default)]
    request_index: u32,
    tool_call_id: String,
    result: ToolFinishedResult,
}
#[derive(Deserialize)]
struct ToolFinishedResult {
    outcome: ToolOutcome,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    content_truncated: bool,
}
#[derive(Deserialize)]
struct InteractionRequestedData {
    meta: EventMeta,
    turn: TurnRef,
    interaction: PendingInteraction,
}
#[derive(Deserialize)]
struct InteractionResolvedData {
    meta: EventMeta,
    turn: TurnRef,
    interaction_id: String,
}
#[derive(Deserialize)]
struct TurnFinishedData {
    meta: EventMeta,
    turn: TurnRef,
    outcome: LoopOutcome,
    #[serde(default)]
    persistence: Option<TurnPersistence>,
}

impl AgentEvent {
    /// Decodes one event. A known type whose payload does not match this
    /// build degrades to [`AgentEvent::Unknown`]: the live turn reconciles from
    /// `turn.wait`/`turn.result` anyway, so one unreadable event must never stop
    /// the stream.
    pub fn decode(params: Value) -> Result<Self, serde_json::Error> {
        let Ok(wire) = serde_json::from_value::<Wire>(params) else {
            return Ok(Self::Unknown);
        };
        Ok(match wire {
            Wire::SessionOpened { data } => Self::SessionOpened {
                meta: data.meta,
                session: Box::new(data.session),
            },
            Wire::SessionClosed { data } => Self::SessionClosed { meta: data.meta },
            Wire::SessionState { data } => Self::SessionState {
                meta: data.meta,
                state: Box::new(data.state),
            },
            Wire::TurnStarted { data } => Self::TurnStarted {
                meta: data.meta,
                turn: data.turn,
            },
            Wire::RequestStarted { data } => Self::RequestStarted {
                meta: data.meta,
                turn: data.turn,
                request_index: data.request_index,
                model: data.model,
            },
            Wire::RequestUsage { data } => Self::RequestUsage {
                meta: data.meta,
                turn: data.turn,
                usage: Box::new(data.usage),
            },
            Wire::OutputDelta { data } => Self::OutputDelta {
                meta: data.meta,
                turn: data.turn,
                request_index: data.request_index,
                reasoning: data.channel != "text",
                delta: data.delta,
            },
            Wire::SteerProgress { data } => Self::SteerProgress {
                meta: data.meta,
                turn: data.turn,
                applied_count: data.applied_count,
            },
            Wire::ToolStarted { data } => Self::ToolStarted {
                meta: data.meta,
                turn: data.turn,
                request_index: data.request_index,
                tool_call_id: data.tool_call_id,
                tool_name: data.tool_name,
            },
            Wire::ToolPresentation { data } => Self::ToolPresentation {
                meta: data.meta,
                call: EventToolCall {
                    turn: data.turn,
                    request_index: data.request_index,
                    tool_call_id: data.tool_call_id,
                },
                tool_name: data.tool_name,
                display: data.display,
            },
            Wire::ToolArgumentsPreview { data } => Self::ToolArgumentsPreview {
                tool_name: data.tool_name,
                meta: data.meta,
                call: EventToolCall {
                    turn: data.turn,
                    request_index: data.request_index,
                    tool_call_id: data.tool_call_id,
                },
                attempt: data.attempt,
                revision: data.revision,
                generated: data.state == "generated",
                discarded: data.state == "discarded",
                display: data.display,
            },
            Wire::ToolInvocation { data } => Self::ToolInvocation {
                meta: data.meta,
                invocation: Box::new(data.invocation),
            },
            Wire::ToolExecution { data } => Self::ToolExecution {
                meta: data.meta,
                execution: Box::new(data.execution),
            },
            Wire::ToolProcess { data } => Self::ToolProcess {
                meta: data.meta,
                data: Box::new(data.data),
            },
            Wire::ToolFinished { data } => Self::ToolFinished {
                meta: data.meta,
                call: EventToolCall {
                    turn: data.turn,
                    request_index: data.request_index,
                    tool_call_id: data.tool_call_id,
                },
                outcome: data.result.outcome,
                content: data.result.content,
                content_truncated: data.result.content_truncated,
            },
            Wire::InteractionRequested { data } => Self::InteractionRequested {
                meta: data.meta,
                turn: data.turn,
                interaction: Box::new(data.interaction),
            },
            Wire::InteractionResolved { data } => Self::InteractionResolved {
                meta: data.meta,
                turn: data.turn,
                interaction_id: data.interaction_id,
            },
            Wire::TurnFinished { data } => Self::TurnFinished {
                meta: data.meta,
                turn: data.turn,
                outcome: data.outcome,
                persistence: data.persistence.unwrap_or(TurnPersistence::Unknown),
            },
            Wire::Unknown => Self::Unknown,
        })
    }

    pub fn meta(&self) -> Option<&EventMeta> {
        match self {
            Self::SessionOpened { meta, .. }
            | Self::SessionClosed { meta }
            | Self::SessionState { meta, .. }
            | Self::TurnStarted { meta, .. }
            | Self::RequestStarted { meta, .. }
            | Self::RequestUsage { meta, .. }
            | Self::OutputDelta { meta, .. }
            | Self::SteerProgress { meta, .. }
            | Self::ToolStarted { meta, .. }
            | Self::ToolPresentation { meta, .. }
            | Self::ToolArgumentsPreview { meta, .. }
            | Self::ToolInvocation { meta, .. }
            | Self::ToolExecution { meta, .. }
            | Self::ToolProcess { meta, .. }
            | Self::ToolFinished { meta, .. }
            | Self::InteractionRequested { meta, .. }
            | Self::InteractionResolved { meta, .. }
            | Self::TurnFinished { meta, .. } => Some(meta),
            Self::Unknown => None,
        }
    }
}

/// Convenience for the compaction phase carried inside session state events.
pub fn compaction_phase_label(phase: CompactionPhase) -> &'static str {
    phase.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(value: Value) -> AgentEvent {
        AgentEvent::decode(json!({"type": value["type"], "data": value["data"]})).unwrap()
    }

    #[test]
    fn output_delta_maps_channel_to_reasoning_flag() {
        let text = event(json!({"type": "output_delta", "data": {
            "turn": {"session_id": "s", "loop_id": "l"}, "request_index": 1,
            "channel": "text", "delta": "hi", "meta": {"session_id": "s", "dropped_before": 0}
        }}));
        let AgentEvent::OutputDelta {
            reasoning, delta, ..
        } = text
        else {
            panic!("expected delta")
        };
        assert!(!reasoning);
        assert_eq!(delta, "hi");

        let thinking = event(json!({"type": "output_delta", "data": {
            "turn": {"session_id": "s", "loop_id": "l"}, "request_index": 1,
            "channel": "reasoning", "delta": "hmm", "meta": {"session_id": "s", "dropped_before": 3}
        }}));
        let AgentEvent::OutputDelta {
            reasoning, meta, ..
        } = thinking
        else {
            panic!("expected delta")
        };
        assert!(reasoning);
        assert_eq!(meta.dropped_before, 3);
    }

    #[test]
    fn unknown_event_type_decodes_to_unknown() {
        assert!(matches!(
            event(json!({"type": "hologram", "data": {}})),
            AgentEvent::Unknown
        ));
    }

    #[test]
    fn tool_preview_state_is_reduced_to_flags() {
        let preview = event(json!({"type": "tool_arguments_preview", "data": {
            "turn": {"session_id": "s", "loop_id": "l"}, "request_index": 0,
            "tool_call_id": "c", "tool_name": "write", "attempt": 1, "revision": 4,
            "state": "generating", "partial": true,
            "display": {"detail": "a.rs", "truncated": false},
            "meta": {"session_id": "s", "dropped_before": 0}
        }}));
        let AgentEvent::ToolArgumentsPreview {
            generated,
            discarded,
            revision,
            ..
        } = preview
        else {
            panic!("expected preview")
        };
        assert!(!generated && !discarded);
        assert_eq!(revision, 4);
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tool_execution_event_decodes_its_terminal_facts() {
        let event = AgentEvent::decode(json!({
            "type": "tool_execution",
            "data": {
                "tool_ref": {"session_id": "s", "loop_id": "l",
                             "request_index": 0, "tool_call_id": "c"},
                "name": "bash",
                "state": "succeeded",
                "result_truncated": false,
                "meta": {"session_id": "s", "dropped_before": 0}
            }
        }))
        .unwrap();
        match event {
            AgentEvent::ToolExecution { execution, .. } => {
                assert_eq!(execution.tool_ref.tool_call_id, "c");
            }
            other => panic!("expected tool_execution, got {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_known_event_degrades_to_unknown() {
        // A payload shape this build does not know must not stop the stream.
        let event = AgentEvent::decode(json!({"type": "turn_started", "data": 7})).unwrap();
        assert!(matches!(event, AgentEvent::Unknown));
    }
}
