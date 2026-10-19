//! The transcript data model: ordered blocks, tool calls, and the operations
//! that apply Agent events and history items to them.

use std::time::{Duration, Instant};

use crate::protocol::event::{CommandResult, ProcessChunk, ProcessStream};
use crate::protocol::{
    LoopOutcome, ToolDisplay, ToolExecutionState, ToolInvocation, ToolRef, ToolSummary, Usage,
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
        /// Items the summary covered and items it kept, each when reported;
        /// shown when there are no token figures (the usual automatic case).
        coverage: Option<(Option<usize>, Option<usize>)>,
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

/// What one turn changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: usize,
    /// Added and removed lines, only when every file's diff was counted: an
    /// unknown count is not shown rather than shown as `+0 −0`.
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
    /// The display body: the command for `bash`, the content for `write`, the
    /// diff for `edit` and `apply_patch`.
    pub expanded_input: Option<String>,
    /// Lines in the whole input, which can exceed what `expanded_input` holds.
    pub input_lines: Option<usize>,
    pub state: ToolState,
    pub started_at: Option<Instant>,
    pub duration: Option<Duration>,
    pub exit_code: Option<i64>,
    /// The output tail. The last entry is the line still being written, so a
    /// chunk that ends mid-line continues it instead of starting a new one.
    pub output: Vec<String>,
    /// Lines dropped from the front of `output` to keep it bounded.
    pub output_dropped: usize,
    /// Lines of the model-facing result, as the Agent counted them.
    pub output_lines: Option<usize>,
    pub truncated: bool,
    /// The reader's per-card fold; `None` follows the global Ctrl+O.
    pub expanded: Option<bool>,
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
            input_lines: None,
            state: ToolState::Pending,
            started_at: None,
            duration: None,
            exit_code: None,
            output: Vec::new(),
            output_dropped: 0,
            output_lines: None,
            truncated: false,
            expanded: None,
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

    pub fn apply_display(&mut self, display: &ToolDisplay) {
        if !display.detail.is_empty() {
            self.detail = display.detail.clone();
        }
        if display.expanded_input.is_some() {
            self.expanded_input = display.expanded_input.clone();
        }
        self.input_lines = display.input_line_count.or(self.input_lines);
    }

    /// The invocation subject only fills a missing detail: the presentation
    /// detail is the Agent's card summary (one line, with a read range), while
    /// the subject is raw, a whole multi-line script for `bash`.
    pub fn apply_invocation(&mut self, invocation: &ToolInvocation) {
        if !invocation.name.is_empty() {
            self.name = invocation.name.clone();
        }
        if self.detail.is_empty() {
            self.detail = invocation.detail();
        }
    }

    /// A command record. Its exit code is set only once really observed, so a
    /// later record without one never erases it.
    pub fn apply_command(&mut self, command: &CommandResult) {
        self.exit_code = command.exit_code.or(self.exit_code);
    }

    /// The model-facing result text, from `tool_finished` or history. It only
    /// fills an empty card: live process output is the better source. A `bash`
    /// result is `exit_code: N`, `stdout:` and `stderr:` sections; the exit
    /// code goes to the header, the labels are dropped and the tail is kept.
    /// Any other result keeps its head, which is what its card shows.
    pub fn apply_result(&mut self, content: &str) {
        let bash = (self.name == "bash")
            .then(|| split_bash_result(content))
            .flatten();
        if let Some((code, _, _)) = bash {
            self.exit_code = self.exit_code.or(code);
        }
        if !self.output.is_empty() {
            return;
        }
        match bash {
            Some((_, stdout, stderr)) => {
                self.push_output(stdout);
                self.push_output(stderr);
            }
            None => self.push_head(content),
        }
    }

    /// Stores the head of a whole result within the card bounds. The Agent's
    /// cut marker is kept as the last line: it is counted in the Agent's line
    /// total, and it says that the result was cut.
    fn push_head(&mut self, content: &str) {
        let body = content.strip_suffix('\n').unwrap_or(content);
        let (body, cut) = match body.strip_suffix(TRUNCATED_MARKER) {
            Some(rest) if rest.is_empty() || rest.ends_with('\n') => (rest, true),
            _ => (body, false),
        };
        let mut bytes = 0;
        for line in body.lines().take(OUTPUT_TAIL_LINES) {
            if bytes > OUTPUT_TAIL_BYTES {
                break;
            }
            let mut kept = String::new();
            self.truncated |= append_capped(&mut kept, line);
            bytes += kept.len() + 1;
            self.output.push(kept);
        }
        if cut {
            self.truncated = true;
            self.output.push(TRUNCATED_MARKER.to_owned());
        }
    }

    /// Keeps what only live events knew when the durable record of the same
    /// call replaces this card: history has no timing, exit code or raw process
    /// output, and the reader's fold must survive.
    pub fn keep_live_facts(&mut self, live: &ToolCall) {
        self.duration = self.duration.or(live.duration);
        self.exit_code = self.exit_code.or(live.exit_code);
        self.expanded = self.expanded.or(live.expanded);
        if live.stream_offsets.iter().any(Option::is_some) {
            self.output.clone_from(&live.output);
            self.output_dropped = live.output_dropped;
            self.truncated |= live.truncated;
        }
    }

    /// Whether the card shows its expanded body under the global fold.
    pub fn is_expanded(&self, all_expanded: bool) -> bool {
        self.expanded.unwrap_or(all_expanded)
    }

    /// Flips this card. A card back in line with the global fold follows it
    /// again, so Ctrl+O keeps working on it.
    pub fn toggle_expanded(&mut self, all_expanded: bool) {
        let expanded = !self.is_expanded(all_expanded);
        self.expanded = (expanded != all_expanded).then_some(expanded);
    }

    /// The header detail. The Agent's detail for an `apply_patch` call is a
    /// placeholder (`tool apply_patch`), so the paths come from the patch.
    pub fn header_detail(&self) -> String {
        if !self.detail.is_empty() && !self.detail.starts_with("tool ") {
            return self.detail.clone();
        }
        let paths = self.patch_paths();
        if paths.is_empty() {
            self.detail.clone()
        } else {
            paths.join(", ")
        }
    }

    fn patch_paths(&self) -> Vec<&str> {
        const PREFIXES: [&str; 6] = [
            "*** Update File: ",
            "*** Add File: ",
            "*** Delete File: ",
            "*** Move to: ",
            "+++ b/",
            "--- a/",
        ];
        let mut paths: Vec<&str> = Vec::new();
        for line in self.expanded_input.as_deref().unwrap_or("").lines() {
            let path = PREFIXES
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
                .map(str::trim);
            if let Some(path) = path
                && !path.is_empty()
                && !paths.contains(&path)
            {
                paths.push(path);
            }
        }
        paths
    }

    /// Added and removed lines of a file-changing call: diff lines for `edit`
    /// and `apply_patch`, every line for `write`.
    pub fn line_changes(&self) -> Option<(usize, usize)> {
        let input = self.expanded_input.as_deref()?;
        match self.name.as_str() {
            "write" => Some((input.lines().count() + self.hidden_input_lines(), 0)),
            "edit" | "apply_patch" | "patch" => {
                let mut counts = (0, 0);
                for line in input.lines() {
                    if line.starts_with("+++ ") || line.starts_with("--- ") {
                        continue;
                    }
                    match line.as_bytes().first() {
                        Some(b'+') => counts.0 += 1,
                        Some(b'-') => counts.1 += 1,
                        _ => {}
                    }
                }
                Some(counts)
            }
            _ => None,
        }
    }

    /// Input lines the Agent left out of `expanded_input` to bound it.
    pub fn hidden_input_lines(&self) -> usize {
        let Some(input) = self.expanded_input.as_deref() else {
            return 0;
        };
        // The Agent counts a trailing newline as starting one more line.
        let sent = if input.is_empty() {
            0
        } else {
            input.matches('\n').count() + 1
        };
        self.input_lines
            .map_or(0, |total| total.saturating_sub(sent))
    }

    /// The output lines worth showing: the open last line when empty and the
    /// Agent's `[truncated]` markers are left out (the header says it).
    pub fn output_content(&self) -> Vec<&str> {
        let mut lines: Vec<&str> = self
            .output
            .iter()
            .map(String::as_str)
            .filter(|line| *line != TRUNCATED_MARKER)
            .collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        lines
    }

    pub fn apply_summary(&mut self, summary: &ToolSummary) {
        self.apply_display(&summary.display);
        self.output_lines = summary.output_line_count.or(self.output_lines);
        self.truncated |= summary.output_truncated;
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
        stream: ProcessStream,
        chunk: &ProcessChunk,
        text: String,
    ) -> Option<String> {
        let slot = match stream {
            ProcessStream::Stdout => 0,
            ProcessStream::Stderr => 1,
            ProcessStream::Other => return Some(text),
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
                self.mark_gap();
                return Some(text);
            }
            None => {}
        }
        self.stream_offsets[slot] = Some(chunk.next_offset);
        Some(text)
    }

    /// Puts a `…` line where output is missing and starts a new line after it.
    fn mark_gap(&mut self) {
        if self.output.last().is_some_and(String::is_empty) {
            self.output.pop();
        }
        self.output.push("…".to_owned());
        self.output.push(String::new());
    }

    /// Appends output text, keeping only the tail. The text continues the open
    /// last line. Lines are the primary bound, but one line can be megabytes
    /// long, so the tail is also capped by bytes: a card is a preview, and the
    /// full text stays available through the tool view.
    pub fn push_output(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut pieces = text.split('\n');
        let first = pieces.next().unwrap_or_default();
        match self.output.last_mut() {
            Some(open) => self.truncated |= append_capped(open, first),
            None => {
                let mut line = String::new();
                self.truncated |= append_capped(&mut line, first);
                self.output.push(line);
            }
        }
        for line in pieces {
            let mut capped = String::new();
            self.truncated |= append_capped(&mut capped, line);
            self.output.push(capped);
        }
        self.bound_output();
    }

    fn bound_output(&mut self) {
        let mut drop = self.output.len().saturating_sub(OUTPUT_TAIL_LINES);
        // Drop from the front until the tail fits the byte budget, always
        // keeping the newest line so a card never goes blank.
        let mut bytes = self.output[drop..]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>();
        while bytes > OUTPUT_TAIL_BYTES && drop + 1 < self.output.len() {
            bytes -= self.output[drop].len() + 1;
            drop += 1;
        }
        if drop > 0 {
            self.output.drain(..drop);
            self.output_dropped += drop;
            self.truncated = true;
        }
    }
}

/// Lines of process output kept per card. The tool view pages the rest.
pub const OUTPUT_TAIL_LINES: usize = 400;
/// Byte cap for a card's stored output tail.
pub const OUTPUT_TAIL_BYTES: usize = 64 * 1024;
/// The line the Agent's `read` and `bash` results end with when cut.
pub const TRUNCATED_MARKER: &str = "[truncated]";

/// Appends to one stored line, keeping its head within the byte cap; the tool
/// view has the whole text. Returns whether anything was cut.
fn append_capped(line: &mut String, text: &str) -> bool {
    if line.len() >= OUTPUT_TAIL_BYTES && line.ends_with('…') {
        // Already cut and marked.
        return !text.is_empty();
    }
    line.push_str(text);
    if line.len() <= OUTPUT_TAIL_BYTES {
        return false;
    }
    let mut end = OUTPUT_TAIL_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line.push('…');
    true
}

/// Splits a `bash` result into its exit code, stdout and stderr.
fn split_bash_result(content: &str) -> Option<(Option<i64>, &str, &str)> {
    let (code, rest) = content.strip_prefix("exit_code: ")?.split_once('\n')?;
    let body = rest.strip_prefix("stdout:\n")?;
    let (stdout, stderr) = match (body.strip_prefix("stderr:\n"), body.rfind("\nstderr:\n")) {
        (Some(stderr), _) => ("", stderr),
        (None, Some(at)) => (&body[..=at], &body[at + "\nstderr:\n".len()..]),
        (None, None) => (body, ""),
    };
    Some((code.trim().parse().ok(), stdout, stderr))
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

    /// One block for an in-place change; it is marked as changed.
    pub fn block_mut(&mut self, index: usize) -> Option<&mut Block> {
        self.bump(index);
        self.blocks.get_mut(index)
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
                call.apply_result(&output.content);
            }
        }
        RuntimeItem::Summary(summary) => {
            transcript.push(Block::Compaction {
                operation_id: String::new(),
                before: None,
                after: None,
                coverage: None,
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
        let line = format!("{}\n", "y".repeat(1024));
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
        assert_eq!(
            call.output_dropped + call.output_content().len(),
            OUTPUT_TAIL_LINES + 20,
            "dropped and kept lines add up to what arrived"
        );
    }

    #[test]
    fn chunks_split_mid_line_join_into_one_line() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output("hello wo");
        call.push_output("rld\nnext\n");
        call.push_output("last\n");
        assert_eq!(call.output_content(), ["hello world", "next", "last"]);
    }

    #[test]
    fn a_bash_result_gives_its_exit_code_and_drops_the_labels() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.apply_result("exit_code: 3\nstdout:\nRan 2 tests\nFAILED\nstderr:\nboom\n");
        assert_eq!(call.exit_code, Some(3));
        assert_eq!(call.output_content(), ["Ran 2 tests", "FAILED", "boom"]);

        let mut quiet = ToolCall::new(key("c"), "bash");
        quiet.apply_result("exit_code: 0\nstdout:\nstderr:\n");
        assert_eq!(quiet.exit_code, Some(0));
        assert!(quiet.output_content().is_empty());
    }

    #[test]
    fn a_long_read_result_keeps_its_head_and_the_cut_marker() {
        let mut call = ToolCall::new(key("c"), "read");
        let lines: Vec<String> = (1..=1000).map(|n| format!("{n}: row")).collect();
        call.apply_result(&format!("{}\n[truncated]", lines.join("\n")));
        assert_eq!(call.output[0], "1: row");
        assert_eq!(call.output.last().unwrap(), TRUNCATED_MARKER);
        assert!(call.output.len() <= OUTPUT_TAIL_LINES + 1);
        assert!(call.truncated);
        assert_eq!(call.output_content().len(), OUTPUT_TAIL_LINES);
    }

    #[test]
    fn a_result_does_not_replace_live_output() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output("live\n");
        call.apply_result("exit_code: 1\nstdout:\npreview\nstderr:\n");
        assert_eq!(call.output_content(), ["live"]);
        assert_eq!(call.exit_code, Some(1), "the exit code still lands");
    }

    #[test]
    fn a_command_record_without_an_exit_code_keeps_the_known_one() {
        use crate::protocol::event::{CommandResult, CommandStatus};
        let record = |exit_code| CommandResult {
            status: CommandStatus::Exited,
            exit_code,
            termination_confirmed: true,
            output_complete: true,
            output_truncated: false,
        };
        let mut call = ToolCall::new(key("c"), "bash");
        call.apply_command(&record(Some(2)));
        call.apply_command(&record(None));
        assert_eq!(call.exit_code, Some(2));
    }

    #[test]
    fn an_invocation_does_not_override_the_card_detail() {
        use crate::protocol::tool::{ToolInputPreview, ToolSubject};
        let invocation = ToolInvocation {
            tool_ref: key("c"),
            name: "bash".into(),
            subject: ToolSubject::Command {
                script: "make\nmake test".into(),
                cwd: "/w".into(),
            },
            input: ToolInputPreview::default(),
        };
        let mut call = ToolCall::new(key("c"), "bash");
        call.apply_invocation(&invocation);
        assert_eq!(
            call.detail, "$ make\nmake test",
            "it fills a missing detail"
        );
        call.detail = "$ make".into();
        call.apply_invocation(&invocation);
        assert_eq!(call.detail, "$ make");
    }

    #[test]
    fn an_apply_patch_header_names_the_files_it_touches() {
        let mut call = ToolCall::new(key("c"), "apply_patch");
        call.detail = "tool apply_patch".into();
        call.expanded_input = Some(
            "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-old\n+new\n+more\n\
             *** Add File: src/b.rs\n+fresh\n*** End Patch"
                .into(),
        );
        assert_eq!(call.header_detail(), "src/a.rs, src/b.rs");
        assert_eq!(call.line_changes(), Some((3, 1)));

        let mut unified = ToolCall::new(key("c"), "apply_patch");
        unified.expanded_input = Some("--- a/app.py\n+++ b/app.py\n@@ -1 +1 @@\n-a\n+b".into());
        assert_eq!(unified.header_detail(), "app.py");
        assert_eq!(
            unified.line_changes(),
            Some((1, 1)),
            "file headers are not lines"
        );
    }

    #[test]
    fn a_written_file_counts_every_line_as_added() {
        let mut call = ToolCall::new(key("c"), "write");
        call.expanded_input = Some("a\nb".into());
        call.input_lines = Some(40);
        assert_eq!(call.line_changes(), Some((40, 0)));
    }

    #[test]
    fn a_card_toggle_overrides_the_global_fold_until_it_matches_again() {
        let mut call = ToolCall::new(key("c"), "bash");
        assert!(!call.is_expanded(false));
        call.toggle_expanded(false);
        assert!(call.is_expanded(false));
        assert!(call.is_expanded(true));
        call.toggle_expanded(false);
        assert_eq!(call.expanded, None, "back in line, it follows Ctrl+O again");
        assert!(call.is_expanded(true));
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
