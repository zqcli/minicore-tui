//! The transcript data model: ordered blocks, tool calls, and the operations
//! that apply Agent events and history items to them.

use std::time::{Duration, Instant};

use crate::protocol::{
    LoopOutcome, ToolDisplay, ToolExecutionState, ToolRef, ToolSummary, Usage,
    read::RawHistoryItem, read::RuntimeAssistantPart, read::RuntimeItem, read::UserKind,
};

/// One rendered unit of the conversation.
#[derive(Clone, Debug)]
pub enum Block {
    User {
        text: String,
        kind: UserKind,
    },
    Assistant {
        key: RequestKey,
        reasoning: String,
        text: String,
        streaming: bool,
    },
    Tool(Box<ToolCall>),
    TurnEnd {
        loop_id: String,
        outcome: LoopOutcome,
        duration: Option<Duration>,
        requests: u32,
        tool_rounds: u64,
        usage: Usage,
        changes: Option<ChangeSummary>,
    },
    Compaction {
        before: Option<u64>,
        after: Option<u64>,
        summary: Option<String>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    EarlierHistory {
        hidden_items: usize,
    },
}

/// Identity of one assistant response inside a turn.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestKey {
    pub loop_id: String,
    pub request_index: u32,
}

impl Block {
    pub fn is_streaming(&self) -> bool {
        matches!(
            self,
            Block::Assistant {
                streaming: true,
                ..
            }
        )
    }

    pub fn loop_id(&self) -> Option<&str> {
        match self {
            Block::Assistant { key, .. } => Some(&key.loop_id),
            Block::Tool(call) => Some(&call.key.loop_id),
            Block::TurnEnd { loop_id, .. } => Some(loop_id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl ChangeSummary {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

/// One tool call card. Output is a bounded tail: full output lives behind
/// `tool.output` in the tool view.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub key: ToolRef,
    pub name: String,
    pub detail: String,
    pub expanded_input: Option<String>,
    pub state: ToolState,
    pub started_at: Option<Instant>,
    pub duration: Option<Duration>,
    pub exit_code: Option<i64>,
    pub output: Vec<String>,
    pub output_lines: Option<usize>,
    pub truncated: bool,
    /// Full output fetched for the tool view, keyed by stream name.
    pub full_output: Option<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

impl ToolState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::Cancelled
        )
    }

    pub fn from_execution(state: ToolExecutionState) -> Self {
        match state {
            ToolExecutionState::Requested => Self::Pending,
            ToolExecutionState::AwaitingPolicy
            | ToolExecutionState::Running
            | ToolExecutionState::Cancelling => Self::Running,
            ToolExecutionState::Succeeded => Self::Succeeded,
            ToolExecutionState::Failed => Self::Failed,
            ToolExecutionState::Denied => Self::Denied,
            ToolExecutionState::Cancelled => Self::Cancelled,
            // `input_provided` means the call yielded to an interaction; the
            // final tool_finished event decides the card state.
            ToolExecutionState::InputProvided | ToolExecutionState::Unknown => Self::Pending,
        }
    }
}

impl ToolCall {
    pub fn new(key: ToolRef, name: impl Into<String>) -> Self {
        Self {
            key,
            name: name.into(),
            detail: String::new(),
            expanded_input: None,
            state: ToolState::Pending,
            started_at: None,
            duration: None,
            exit_code: None,
            output: Vec::new(),
            output_lines: None,
            truncated: false,
            full_output: None,
        }
    }

    /// Stores the paged full output of one stream for the tool view.
    pub fn set_full_output(&mut self, stream: &str, text: String) {
        self.full_output = Some((stream.to_owned(), text));
    }

    /// The body shown in the tool view: full output when loaded, else the tail.
    pub fn view_body(&self) -> String {
        match &self.full_output {
            Some((_, text)) => text.clone(),
            None => self.output.join("\n"),
        }
    }

    /// For `bash`, the expanded input is the command; otherwise the diff body.
    pub fn body_preview(&self, limit: usize) -> Vec<&str> {
        match &self.expanded_input {
            Some(input) => input.lines().take(limit).collect(),
            None => self
                .output
                .iter()
                .rev()
                .take(limit)
                .rev()
                .map(String::as_str)
                .collect(),
        }
    }

    /// Whether the card's body is a patch, so its lines get diff colors.
    pub fn is_patch(&self) -> bool {
        self.expanded_input.is_some() && call_is_patch(&self.name)
    }

    pub fn apply_display(&mut self, display: &ToolDisplay) {
        if !display.detail.is_empty() {
            self.detail = display.detail.clone();
        }
        if display.expanded_input.is_some() {
            self.expanded_input = display.expanded_input.clone();
        }
        if let Some(lines) = display.input_line_count {
            self.output_lines = self.output_lines.or(Some(lines));
        }
    }

    pub fn apply_summary(&mut self, summary: &ToolSummary) {
        self.apply_display(&summary.display);
        self.output_lines = summary.output_line_count.or(self.output_lines);
        self.truncated = summary.output_truncated;
        self.state = match summary.state.as_deref() {
            Some("failed") => ToolState::Failed,
            Some("denied") => ToolState::Denied,
            Some("cancelled") => ToolState::Cancelled,
            Some("succeeded") => ToolState::Succeeded,
            _ => self.state,
        };
    }

    /// Appends a process-output chunk, keeping only the tail.
    pub fn push_output(&mut self, text: &str) {
        for line in text.split('\n') {
            self.output.push(line.to_owned());
        }
        if self.output.len() > OUTPUT_TAIL_LINES {
            let excess = self.output.len() - OUTPUT_TAIL_LINES;
            self.output.drain(..excess);
            self.truncated = true;
        }
    }
}

impl ToolCall {
    /// Rows of body a folded card shows. Patches get the start of the diff so
    /// the change is recognizable; commands get their output tail.
    pub fn folded_rows(&self) -> usize {
        if self.is_patch() {
            8
        } else if self.state == ToolState::Failed || self.name == "read" {
            3
        } else {
            4
        }
    }
}

fn call_is_patch(name: &str) -> bool {
    matches!(name, "edit" | "write" | "apply_patch" | "patch")
}

/// Lines of process output kept per card. The tool view pages the rest.
pub const OUTPUT_TAIL_LINES: usize = 400;

/// Ordered blocks with a revision per block. The revision is bumped on every
/// mutation and is the layout cache key, so the cache never guesses from
/// content.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    blocks: Vec<Block>,
    revisions: Vec<u64>,
    next_revision: u64,
}

impl Transcript {
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn revisions(&self) -> &[u64] {
        &self.revisions
    }

    /// Mutable access to every block; each one is marked as changed.
    pub fn blocks_mut(&mut self) -> &mut [Block] {
        for revision in &mut self.revisions {
            self.next_revision += 1;
            *revision = self.next_revision;
        }
        &mut self.blocks
    }

    fn bump(&mut self, index: usize) {
        self.next_revision += 1;
        if let Some(revision) = self.revisions.get_mut(index) {
            *revision = self.next_revision;
        }
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn push(&mut self, block: Block) -> usize {
        self.next_revision += 1;
        self.blocks.push(block);
        self.revisions.push(self.next_revision);
        self.blocks.len() - 1
    }

    pub fn insert(&mut self, index: usize, block: Block) {
        let index = index.min(self.blocks.len());
        self.next_revision += 1;
        self.blocks.insert(index, block);
        self.revisions.insert(index, self.next_revision);
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.revisions.clear();
    }

    pub fn truncate(&mut self, len: usize) {
        self.blocks.truncate(len);
        self.revisions.truncate(len);
    }

    pub fn remove_front(&mut self, count: usize) {
        let count = count.min(self.blocks.len());
        self.blocks.drain(..count);
        self.revisions.drain(..count);
    }

    pub fn into_blocks(self) -> Vec<Block> {
        self.blocks
    }

    pub fn find_tool_by_call_id(&self, tool_call_id: &str) -> Option<&ToolCall> {
        self.blocks.iter().find_map(|block| match block {
            Block::Tool(call) if call.key.tool_call_id == tool_call_id => Some(call.as_ref()),
            _ => None,
        })
    }

    pub fn find_tool_mut(&mut self, key: &ToolRef) -> Option<&mut ToolCall> {
        let index = self
            .blocks
            .iter()
            .position(|block| matches!(block, Block::Tool(call) if &call.key == key))?;
        self.bump(index);
        match &mut self.blocks[index] {
            Block::Tool(call) => Some(call.as_mut()),
            _ => None,
        }
    }

    pub fn find_tool(&self, key: &ToolRef) -> Option<&ToolCall> {
        self.blocks.iter().find_map(|block| match block {
            Block::Tool(call) if &call.key == key => Some(call.as_ref()),
            _ => None,
        })
    }

    /// The assistant block for one request, created if the stream is late.
    pub fn assistant_mut(&mut self, key: &RequestKey) -> &mut Block {
        let index = match self.blocks.iter().position(
            |block| matches!(block, Block::Assistant { key: existing, .. } if existing == key),
        ) {
            Some(index) => index,
            None => {
                self.next_revision += 1;
                self.blocks.push(Block::Assistant {
                    key: key.clone(),
                    reasoning: String::new(),
                    text: String::new(),
                    streaming: true,
                });
                self.revisions.push(self.next_revision);
                self.blocks.len() - 1
            }
        };
        self.bump(index);
        &mut self.blocks[index]
    }

    /// The last assistant answer text, for `/copy`.
    pub fn last_answer(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Assistant { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
    }

    /// The index of the newest live tool card.
    pub fn last_tool_block(&self) -> Option<usize> {
        self.blocks
            .iter()
            .rposition(|block| matches!(block, Block::Tool(_)))
    }

    pub fn live_turn_start(&self, loop_id: &str) -> Option<usize> {
        self.blocks
            .iter()
            .position(|block| block.loop_id() == Some(loop_id))
    }
}

/// Converts one decoded history item into blocks appended to the transcript.
/// Returns how many blocks were appended.
pub fn append_history_item(
    transcript: &mut Transcript,
    item: &RawHistoryItem,
    session_id: &str,
) -> usize {
    let start = transcript.len();
    match &item.item {
        RuntimeItem::User(user) => {
            transcript.push(Block::User {
                text: user.input.text.clone(),
                kind: user.kind,
            });
        }
        RuntimeItem::Assistant(assistant) => {
            let mut reasoning = String::new();
            let mut text = String::new();
            let mut calls = Vec::new();
            for part in &assistant.content {
                match part {
                    RuntimeAssistantPart::Text(chunk) => text.push_str(chunk),
                    RuntimeAssistantPart::Reasoning {
                        text: chunk,
                        summary,
                    } => {
                        if let Some(chunk) = chunk.as_ref().or(summary.as_ref()) {
                            reasoning.push_str(chunk);
                        }
                    }
                    RuntimeAssistantPart::ToolCall {
                        tool_call_id, name, ..
                    } => {
                        let key = ToolRef {
                            session_id: session_id.to_owned(),
                            loop_id: assistant.loop_id.clone(),
                            request_index: assistant.request_index,
                            tool_call_id: tool_call_id.clone(),
                        };
                        let mut call = ToolCall::new(key, name.clone());
                        call.state = ToolState::Succeeded;
                        if let Some(summary) = item.summary(tool_call_id) {
                            call.apply_summary(summary);
                        }
                        calls.push(call);
                    }
                }
            }
            transcript.push(Block::Assistant {
                key: RequestKey {
                    loop_id: assistant.loop_id.clone(),
                    request_index: assistant.request_index,
                },
                reasoning,
                text,
                streaming: false,
            });
            for call in calls {
                transcript.push(Block::Tool(Box::new(call)));
            }
        }
        RuntimeItem::ToolResult(result) => {
            let Some(call) = transcript.blocks.iter_mut().find_map(|block| match block {
                Block::Tool(call)
                    if call.key.loop_id == result.loop_id
                        && call.key.request_index == result.request_index
                        && call.key.tool_call_id == result.call_id =>
                {
                    Some(call.as_mut())
                }
                _ => None,
            }) else {
                return transcript.len() - start;
            };
            call.state = match result.outcome.as_str() {
                "success" => ToolState::Succeeded,
                "denied" => ToolState::Denied,
                "cancelled" => ToolState::Cancelled,
                _ => ToolState::Failed,
            };
            if let Some(output) = &result.output {
                call.push_output(&output.content);
            }
        }
        RuntimeItem::Summary(summary) => {
            transcript.push(Block::Compaction {
                before: None,
                after: None,
                summary: Some(summary.content.clone()),
            });
        }
    }
    transcript.len() - start
}

/// Renders decoded canonical items as a Markdown conversation.
pub fn to_markdown(items: &[RawHistoryItem]) -> String {
    let mut out = String::with_capacity(4096);
    for item in items {
        match &item.item {
            RuntimeItem::User(user) => {
                let label = match user.kind {
                    UserKind::Steering => "**Steering**",
                    _ => "**You**",
                };
                out.push_str(label);
                out.push_str("\n\n");
                out.push_str(user.input.text.trim_end());
                out.push_str("\n\n");
            }
            RuntimeItem::Assistant(assistant) => {
                for part in &assistant.content {
                    match part {
                        RuntimeAssistantPart::Reasoning { text: thinking, .. } => {
                            if let Some(thinking) = thinking {
                                out.push_str("> ");
                                out.push_str(&thinking.replace('\n', "\n> "));
                                out.push_str("\n\n");
                            }
                        }
                        RuntimeAssistantPart::Text(text) => {
                            out.push_str(text.trim_end());
                            out.push_str("\n\n");
                        }
                        RuntimeAssistantPart::ToolCall { name, .. } => {
                            out.push_str(&format!("`{name}`\n\n"));
                        }
                    }
                }
            }
            RuntimeItem::ToolResult(result) => {
                if let Some(output) = &result.output
                    && !output.content.trim().is_empty()
                {
                    out.push_str("```\n");
                    out.push_str(output.content.trim_end());
                    out.push_str("\n```\n\n");
                }
            }
            RuntimeItem::Summary(summary) => {
                out.push_str("---\n\n*Context summary*\n\n");
                out.push_str(summary.content.trim_end());
                out.push_str("\n\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::decode_item;
    use serde_json::json;

    fn key(call: &str) -> ToolRef {
        ToolRef {
            session_id: "s".into(),
            loop_id: "l".into(),
            request_index: 1,
            tool_call_id: call.into(),
        }
    }

    #[test]
    fn tool_output_keeps_a_bounded_tail() {
        let mut call = ToolCall::new(key("c"), "bash");
        for line in 0..OUTPUT_TAIL_LINES + 20 {
            call.push_output(&format!("line {line}\n"));
        }
        assert!(call.truncated);
        assert!(call.output.len() <= OUTPUT_TAIL_LINES + 1);
        assert_eq!(call.output.last().unwrap(), "");
        assert!(call.output.iter().any(|line| line == "line 419"));
    }

    #[test]
    fn assistant_blocks_are_created_once_per_request() {
        let mut transcript = Transcript::default();
        let request = RequestKey {
            loop_id: "l".into(),
            request_index: 2,
        };
        transcript.assistant_mut(&request);
        transcript.assistant_mut(&request);
        assert_eq!(transcript.len(), 1);
    }

    #[test]
    fn history_items_produce_user_assistant_and_tool_blocks() {
        let mut transcript = Transcript::default();
        let user = decode_item(
            &json!({"item": {"type": "user", "data": {
                "loop_id": "l", "kind": "prompt", "input": {"text": "hello"}
            }}})
            .to_string(),
        )
        .unwrap();
        append_history_item(&mut transcript, &user, "s");

        let assistant = decode_item(
            &json!({"item": {"type": "assistant", "data": {
                "loop_id": "l", "request_index": 1, "model": "m",
                "content": [
                    {"type": "text", "data": "running tests"},
                    {"type": "tool_call", "data": {"tool_call_id": "c1", "name": "bash"}}
                ],
                "finish_reason": "tool_calls"
            }}, "tool_summaries": [{
                "tool_ref": {"session_id": "s", "loop_id": "l", "request_index": 1, "tool_call_id": "c1"},
                "tool_call_id": "c1", "name": "bash",
                "display": {"detail": "$ pytest", "truncated": false}
            }]})
            .to_string(),
        )
        .unwrap();
        append_history_item(&mut transcript, &assistant, "s");

        assert_eq!(transcript.len(), 3);
        let Block::Tool(call) = &transcript.blocks()[2] else {
            panic!("expected a tool card");
        };
        assert_eq!(call.detail, "$ pytest");
    }

    #[test]
    fn tool_result_updates_the_card_it_belongs_to() {
        let mut transcript = Transcript::default();
        let mut call = ToolCall::new(key("c1"), "bash");
        call.state = ToolState::Running;
        transcript.push(Block::Tool(Box::new(call)));
        let result = decode_item(
            &json!({"item": {"type": "tool_result", "data": {
                "loop_id": "l", "request_index": 1, "call_id": "c1",
                "tool_name": "bash", "outcome": "success",
                "output": {"content": "ok"}
            }}})
            .to_string(),
        )
        .unwrap();
        append_history_item(&mut transcript, &result, "s");
        let Block::Tool(call) = &transcript.blocks()[0] else {
            panic!("expected a tool card");
        };
        assert_eq!(call.state, ToolState::Succeeded);
        assert_eq!(call.output, vec!["ok".to_owned()]);
    }

    #[test]
    fn last_answer_ignores_reasoning_only_blocks() {
        let mut transcript = Transcript::default();
        transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: "thinking".into(),
            text: String::new(),
            streaming: false,
        });
        transcript.push(Block::User {
            text: "again".into(),
            kind: UserKind::Prompt,
        });
        assert_eq!(transcript.last_answer(), None);
    }
}
