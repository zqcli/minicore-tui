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
        /// The Agent's operation id, so a duplicate report is recognised.
        operation_id: String,
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

/// What one turn changed. The line counts are optional because they can be
/// genuinely unknown: a record may carry no diff, or its diff may be
/// unavailable, partial or truncated. Printing `+0 −0` for an unknown count
/// would be a lie, so the counts are kept as an `Option` pair and an absent
/// pair is simply not rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: usize,
    /// Added and removed lines, when every file's diff was counted.
    pub lines: Option<(usize, usize)>,
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
    /// Next expected byte offset per stream. `None` until the first chunk, so
    /// the first chunk establishes the cursor whatever its base offset is.
    pub stream_offsets: [Option<u64>; 2],
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
            stream_offsets: [None, None],
            full_output: None,
        }
    }

    /// Advances the state. A tool that already reached a terminal state stays
    /// there: late or replayed non-terminal updates must not reopen it.
    pub fn set_state(&mut self, state: ToolState) {
        if self.state.is_terminal() && !state.is_terminal() {
            return;
        }
        self.state = state;
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

    /// Accepts one process chunk for `stream`. Returns the text to append, or
    /// `None` when the chunk is a duplicate, out of order, or evicted: a gap is
    /// marked instead of silently splicing unrelated bytes together.
    pub fn accept_chunk(
        &mut self,
        stream: crate::protocol::event::ProcessStream,
        chunk: &crate::protocol::event::ProcessChunk,
        text: String,
    ) -> Option<String> {
        let slot = match stream {
            crate::protocol::event::ProcessStream::Stdout => 0,
            crate::protocol::event::ProcessStream::Stderr => 1,
            crate::protocol::event::ProcessStream::Other => return Some(text),
        };
        if chunk.expired {
            return None;
        }
        match self.stream_offsets[slot] {
            Some(expected) if chunk.base_offset == expected => {}
            Some(expected) if chunk.base_offset < expected => return None,
            Some(_) => {
                // The Agent dropped bytes: say so rather than joining halves.
                self.stream_offsets[slot] = Some(chunk.next_offset);
                return Some(format!("…\n{text}"));
            }
            None => {}
        }
        self.stream_offsets[slot] = Some(chunk.next_offset);
        Some(text)
    }

    /// Appends a process-output chunk, keeping only the tail. Lines are the
    /// primary bound, but one line can be megabytes long, so the tail is also
    /// capped by bytes: a card is a preview, and the full text stays available
    /// through the tool view.
    pub fn push_output(&mut self, text: &str) {
        for line in text.split('\n') {
            let capped = cap_line(line);
            self.truncated |= capped.len() != line.len();
            self.output.push(capped);
        }
        if self.output.len() > OUTPUT_TAIL_LINES {
            let excess = self.output.len() - OUTPUT_TAIL_LINES;
            self.output.drain(..excess);
            self.truncated = true;
        }
        // Drop from the front until the tail fits the byte budget, always
        // keeping the newest line so a card never goes blank.
        let mut bytes = self.output.iter().map(|line| line.len() + 1).sum::<usize>();
        let mut drop = 0;
        while bytes > OUTPUT_TAIL_BYTES && drop + 1 < self.output.len() {
            bytes -= self.output[drop].len() + 1;
            drop += 1;
        }
        if drop > 0 {
            self.output.drain(..drop);
            self.truncated = true;
        }
    }
}

/// Lines of process output kept per card. The tool view pages the rest.
pub const OUTPUT_TAIL_LINES: usize = 400;
/// Byte cap for a card's stored output tail.
pub const OUTPUT_TAIL_BYTES: usize = 64 * 1024;

/// Keeps one line's head; the tool view has the whole text.
fn cap_line(line: &str) -> String {
    if line.len() <= OUTPUT_TAIL_BYTES {
        return line.to_owned();
    }
    let mut end = OUTPUT_TAIL_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

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

    pub fn remove(&mut self, index: usize) {
        if index < self.blocks.len() {
            self.blocks.remove(index);
            self.revisions.remove(index);
        }
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
                operation_id: String::new(),
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
    use crate::protocol::event::{ProcessChunk, ProcessStream};
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
    fn a_finished_tool_never_goes_back_to_running() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.set_state(ToolState::Succeeded);
        call.set_state(ToolState::Running);
        assert_eq!(call.state, ToolState::Succeeded);
        // A later terminal state is still allowed: the outcome is corrected,
        // for example a cancelled run finishing as failed.
        call.set_state(ToolState::Failed);
        assert_eq!(call.state, ToolState::Failed);
    }

    #[test]
    fn a_pending_tool_still_advances_to_running() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.set_state(ToolState::Pending);
        call.set_state(ToolState::Running);
        assert_eq!(call.state, ToolState::Running);
    }

    #[test]
    fn a_single_huge_line_cannot_bloat_a_card() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output(&"x".repeat(OUTPUT_TAIL_BYTES * 3));
        assert_eq!(call.output.len(), 1);
        assert!(
            call.output[0].len() <= OUTPUT_TAIL_BYTES + 4,
            "{}",
            call.output[0].len()
        );
        assert!(call.output[0].ends_with('…'));
        assert!(call.truncated);
    }

    #[test]
    fn the_output_tail_is_bounded_by_bytes_as_well_as_lines() {
        let mut call = ToolCall::new(key("c"), "bash");
        let line = "y".repeat(1024);
        for _ in 0..200 {
            call.push_output(&line);
        }
        let bytes: usize = call.output.iter().map(|line| line.len() + 1).sum();
        assert!(bytes <= OUTPUT_TAIL_BYTES, "{bytes} bytes kept");
        assert!(call.truncated);
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

    fn chunk(stream: ProcessStream, base: u64, next: u64, expired: bool) -> ProcessChunk {
        ProcessChunk {
            stream,
            encoding: "base64".into(),
            data: String::new(),
            base_offset: base,
            next_offset: next,
            dropped: false,
            expired,
        }
    }

    #[test]
    fn stdout_and_stderr_keep_independent_cursors() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        let err = ProcessStream::Stderr;
        assert!(
            call.accept_chunk(out, &chunk(out, 0, 10, false), "a".into())
                .is_some()
        );
        // stderr starts at its own zero; the stdout cursor must not apply.
        assert!(
            call.accept_chunk(err, &chunk(err, 0, 4, false), "b".into())
                .is_some()
        );
        assert!(
            call.accept_chunk(out, &chunk(out, 10, 20, false), "c".into())
                .is_some()
        );
        assert_eq!(call.stream_offsets, [Some(20), Some(4)]);
    }

    #[test]
    fn a_replayed_chunk_is_ignored() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        call.accept_chunk(out, &chunk(out, 0, 10, false), "a".into());
        assert!(
            call.accept_chunk(out, &chunk(out, 0, 10, false), "a".into())
                .is_none()
        );
    }

    /// Feeds one chunk exactly as the reducer does.
    fn feed(call: &mut ToolCall, stream: ProcessStream, base: u64, next: u64, text: &str) {
        if let Some(text) =
            call.accept_chunk(stream, &chunk(stream, base, next, false), text.into())
        {
            call.push_output(&text);
        }
    }

    #[test]
    fn a_dropped_range_is_marked_instead_of_spliced() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        feed(&mut call, out, 0, 10, "a");
        // Bytes 10..30 were evicted: the cursor jumps and a gap is shown.
        feed(&mut call, out, 30, 40, "c");
        assert_eq!(call.output, vec!["a", "…", "c"]);
        assert_eq!(call.stream_offsets, [Some(40), None]);
    }

    #[test]
    fn an_expired_chunk_is_dropped() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        assert!(
            call.accept_chunk(out, &chunk(out, 0, 10, true), "a".into())
                .is_none()
        );
        assert!(call.output.is_empty());
        assert_eq!(call.stream_offsets, [None, None]);
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

#[cfg(test)]
mod append_tests {
    use super::*;

    #[test]
    fn an_assistant_item_with_a_text_part_becomes_an_assistant_block() {
        let raw = r#"{"item":{"type":"assistant","data":{"loop_id":"l0",
            "request_index":0,"model":"mock","content":[
            {"type":"text","data":"earlier answer"}]}}}"#;
        let item = crate::protocol::read::decode_item(raw).unwrap();
        let mut transcript = Transcript::default();
        append_history_item(&mut transcript, &item, "s1");
        assert!(
            matches!(transcript.blocks(), [Block::Assistant { text, .. }] if text == "earlier answer"),
            "{:?}",
            transcript.blocks()
        );
    }
}
