//! Agent event notifications. Events are hints: every fact here is also
//! available from an authoritative read, so delivery order never decides
//! correctness.

use serde::{Deserialize, Deserializer};

use super::{
    DeliveredInput, PendingInteraction, Queue, SessionInfo, SessionState, ToolDisplay,
    ToolExecution, ToolInvocation, ToolRef, TurnRef, Usage,
};

#[derive(Clone, Debug, Deserialize)]
pub struct EventMeta {
    pub session_id: String,
    pub dropped_before: u64,
}

/// One `agent.event` notification, decoded from its `{type, data}` envelope.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
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
        #[serde(rename = "channel", deserialize_with = "reasoning_channel")]
        reasoning: bool,
        delta: String,
    },
    InputQueue {
        meta: EventMeta,
        queue: Queue,
    },
    InputDelivered {
        meta: EventMeta,
        turn: TurnRef,
        items: Vec<DeliveredInput>,
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
        #[serde(flatten)]
        call: EventToolCall,
        tool_name: String,
        display: ToolDisplay,
    },
    ToolArgumentsPreview {
        meta: EventMeta,
        #[serde(flatten)]
        call: EventToolCall,
        tool_name: String,
        state: PreviewState,
        display: ToolDisplay,
    },
    ToolInvocation {
        meta: EventMeta,
        #[serde(rename = "data")]
        invocation: Box<ToolInvocation>,
    },
    ToolExecution {
        meta: EventMeta,
        #[serde(rename = "data")]
        execution: Box<ToolExecution>,
    },
    ToolProcess {
        meta: EventMeta,
        data: Box<ToolProcess>,
    },
    ToolFinished {
        meta: EventMeta,
        #[serde(flatten)]
        call: EventToolCall,
        result: ToolResult,
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
    },
    /// An event type this client does not model, or a payload it cannot read.
    #[serde(other)]
    Unknown,
}

fn reasoning_channel<'de, D: Deserializer<'de>>(channel: D) -> Result<bool, D::Error> {
    Ok(String::deserialize(channel)? == "reasoning")
}

impl AgentEvent {
    pub fn meta(&self) -> Option<&EventMeta> {
        match self {
            Self::SessionOpened { meta, .. }
            | Self::SessionClosed { meta }
            | Self::SessionState { meta, .. }
            | Self::TurnStarted { meta, .. }
            | Self::RequestStarted { meta, .. }
            | Self::RequestUsage { meta, .. }
            | Self::OutputDelta { meta, .. }
            | Self::InputQueue { meta, .. }
            | Self::InputDelivered { meta, .. }
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
pub enum PreviewState {
    Generating,
    Generated,
    Discarded,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolResult {
    pub outcome: ToolOutcome,
    pub content: Option<String>,
    /// The Agent omits it when false.
    #[serde(default)]
    pub content_truncated: bool,
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
    pub chunk: Option<ProcessChunk>,
    pub command: Option<CommandResult>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProcessChunk {
    pub stream: ProcessStream,
    pub encoding: String,
    pub data: String,
    pub base_offset: u64,
    pub next_offset: u64,
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
    pub exit_code: Option<i64>,
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

#[cfg(test)]
mod tests {
    use super::super::InputMode;
    use super::*;
    use serde_json::{Value, json};

    /// Every event type the Agent emits, verbatim from its
    /// `src/event/events.jsonl` serialization fixture.
    const AGENT_EVENTS: &str = include_str!("../../tests/fixtures/agent-events.jsonl");

    fn decode(value: Value) -> AgentEvent {
        let frame = json!({"jsonrpc": "2.0", "method": "agent.event", "params": value});
        match super::super::parse_frame(frame.to_string().as_bytes()).unwrap() {
            super::super::IncomingFrame::Event(event) => event,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn every_agent_event_decodes() {
        let events: Vec<AgentEvent> = AGENT_EVENTS
            .lines()
            .map(|line| decode(serde_json::from_str(line).unwrap()))
            .collect();
        assert_eq!(events.len(), 20);
        let unknown: Vec<&str> = AGENT_EVENTS
            .lines()
            .zip(&events)
            .filter(|(_, event)| matches!(event, AgentEvent::Unknown))
            .map(|(line, _)| line.split('"').nth(3).unwrap())
            .collect();
        assert_eq!(unknown, ["tool_progress"], "only unmodeled types degrade");
        for meta in events.iter().filter_map(AgentEvent::meta) {
            assert_eq!(meta.dropped_before, 7);
        }
    }

    #[test]
    fn agent_events_carry_the_fields_the_reducer_reads() {
        let events: Vec<AgentEvent> = AGENT_EVENTS
            .lines()
            .map(|line| decode(serde_json::from_str(line).unwrap()))
            .collect();
        let find = |wanted: fn(&AgentEvent) -> bool| events.iter().find(|event| wanted(event));
        let Some(AgentEvent::OutputDelta {
            reasoning, delta, ..
        }) = find(|event| matches!(event, AgentEvent::OutputDelta { .. }))
        else {
            panic!("no output delta");
        };
        assert!(!reasoning);
        assert_eq!(delta, "hello\n\"world\"");
        let Some(AgentEvent::ToolFinished { call, result, .. }) =
            find(|event| matches!(event, AgentEvent::ToolFinished { .. }))
        else {
            panic!("no tool_finished");
        };
        assert_eq!(call.key().tool_call_id, "call_1");
        assert_eq!(result.outcome, ToolOutcome::Success);
        assert_eq!(result.content.as_deref(), Some("hello\n"));
        assert!(!result.content_truncated);
        let Some(AgentEvent::ToolArgumentsPreview { state, call, .. }) =
            find(|event| matches!(event, AgentEvent::ToolArgumentsPreview { .. }))
        else {
            panic!("no preview");
        };
        assert_eq!(*state, PreviewState::Generated);
        assert_eq!(call.request_index, 1);
        let Some(AgentEvent::ToolProcess { data, .. }) =
            find(|event| matches!(event, AgentEvent::ToolProcess { .. }))
        else {
            panic!("no tool_process");
        };
        let chunk = data.chunk.as_ref().unwrap();
        assert_eq!(
            (chunk.stream, chunk.next_offset),
            (ProcessStream::Stdout, 2)
        );
    }

    #[test]
    fn input_events_carry_the_queue_and_the_delivered_texts() {
        let events: Vec<AgentEvent> = AGENT_EVENTS
            .lines()
            .map(|line| decode(serde_json::from_str(line).unwrap()))
            .collect();
        let Some(AgentEvent::InputQueue { queue, .. }) = events
            .iter()
            .find(|event| matches!(event, AgentEvent::InputQueue { .. }))
        else {
            panic!("no input_queue");
        };
        let modes: Vec<_> = queue.items.iter().map(|item| item.mode).collect();
        assert_eq!(modes, [InputMode::Steer, InputMode::FollowUp]);
        assert!(!queue.paused);
        let Some(AgentEvent::InputDelivered { turn, items, .. }) = events
            .iter()
            .find(|event| matches!(event, AgentEvent::InputDelivered { .. }))
        else {
            panic!("no input_delivered");
        };
        assert_eq!(turn.loop_id, "lup_00000000000000000000000000000002");
        assert_eq!(items[0].text, "also the tests");
        assert_eq!(items[0].id, queue.items[0].id);
    }

    #[test]
    fn a_reasoning_delta_is_flagged() {
        let event = decode(json!({"type": "output_delta", "data": {
            "turn": {"session_id": "s", "loop_id": "l"}, "request_index": 1,
            "channel": "reasoning", "delta": "hmm",
            "meta": {"session_id": "s", "loop_id": "l", "dropped_before": 3}
        }}));
        let AgentEvent::OutputDelta {
            reasoning, meta, ..
        } = event
        else {
            panic!("expected a delta, got {event:?}");
        };
        assert!(reasoning);
        assert_eq!(meta.dropped_before, 3);
    }

    #[test]
    fn a_tool_execution_event_decodes_its_terminal_facts() {
        let event = decode(json!({"type": "tool_execution", "data": {
            "turn": {"session_id": "s", "loop_id": "l"},
            "data": {
                "tool_ref": {"session_id": "s", "loop_id": "l",
                             "request_index": 0, "tool_call_id": "c"},
                "name": "bash", "state": "succeeded", "result_truncated": false,
                "command": {"status": "exited", "exit_code": 2, "termination_confirmed": true}
            },
            "meta": {"session_id": "s", "loop_id": "l", "dropped_before": 0}
        }}));
        let AgentEvent::ToolExecution { execution, .. } = event else {
            panic!("expected tool_execution, got {event:?}");
        };
        assert_eq!(execution.tool_ref.tool_call_id, "c");
        assert_eq!(execution.command.and_then(|c| c.exit_code), Some(2));
    }

    #[test]
    fn unknown_or_unreadable_events_degrade_instead_of_stopping_the_stream() {
        assert!(matches!(
            decode(json!({"type": "hologram", "data": {}})),
            AgentEvent::Unknown
        ));
        assert!(matches!(
            decode(json!({"type": "turn_started", "data": 7})),
            AgentEvent::Unknown
        ));
    }
}
