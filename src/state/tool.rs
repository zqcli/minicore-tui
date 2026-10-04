//! Live tool call state (spec 12.7). One `LiveTool` per tool_call_id;
//! duplicate started/progress/finished events are idempotent.

use std::sync::Arc;

use crate::protocol::ToolDisplayWire;
use crate::protocol::{
    ToolDataAvailabilityWire as Availability, ToolDataStreamWire as Stream, ToolRefWire,
};

/// Recover only the existing safe presentation whitelist from canonical history.
/// Raw arguments are discarded at the history boundary, never retained or
/// displayed as a generic JSON object. This is presentation, not execution.
pub(crate) fn history_display(
    name: &str,
    arguments: &serde_json::Value,
) -> Option<ToolDisplayWire> {
    const LIMIT: usize = 512;
    let field = match name {
        "bash" => "command",
        "read" | "write" | "edit" | "apply_patch" | "patch" => "path",
        _ => return None,
    };
    let source = arguments.get(field)?.as_str()?;
    if source.is_empty() {
        return None;
    }
    // Bound before sanitation as well as after it: hostile control bytes can
    // expand, and the canonical source can be much larger than the preview.
    let prefix_end = source
        .char_indices()
        .map(|(i, _)| i)
        .find(|i| *i > LIMIT)
        .unwrap_or(source.len())
        .min(source.len());
    let prefix = &source[..prefix_end];
    let safe = crate::safe_text::safe_display(prefix);
    let mut detail = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    if detail.is_empty() {
        return None;
    }
    let truncated = prefix_end < source.len() || detail.len() > LIMIT;
    if truncated {
        let mut end = (LIMIT - '…'.len_utf8()).min(detail.len());
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
        detail.push('…');
    }
    Some(ToolDisplayWire {
        body_truncated: false,
        detail,
        expanded_input: None,
        input_line_count: None,
        hidden_line_count: None,
        truncated,
    })
}

impl From<&ToolRefWire> for ToolKey {
    fn from(key: &ToolRefWire) -> Self {
        Self::new(
            &key.session_id,
            &key.loop_id,
            key.request_index,
            &key.tool_call_id,
        )
    }
}
impl From<&ToolKey> for ToolRefWire {
    fn from(key: &ToolKey) -> Self {
        Self {
            session_id: key.session_id.clone(),
            loop_id: key.loop_id.clone(),
            request_index: key.request_index,
            tool_call_id: key.tool_call_id.clone(),
        }
    }
}

/// Raw bytes have one owner; layout snapshots only clone the small chunk Arcs.
/// Closing the single detail releases all four windows (at most 4 MiB, below
/// the global 16 MiB limit). Offsets always refer to the original raw bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamView {
    pub stream: Stream,
    pub next_offset: u64,
    pub base_offset: u64,
    pub observed_end: u64,
    pub eof: bool,
    pub availability: Availability,
    pub truncated: bool,
    pub gap: bool,
    pub chunks: std::collections::VecDeque<StreamChunk>,
    pub retained_bytes: usize,
    pub revision: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub enum StreamChunk {
    Raw(Arc<Vec<u8>>),
    /// Output already owned by ToolFacts/cards is borrowed, not copied into a
    /// second detail body. A response must match the original byte range.
    Result {
        source: Arc<str>,
        range: std::ops::Range<usize>,
    },
}
impl StreamChunk {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Raw(bytes) => bytes,
            Self::Result { source, range } => &source.as_bytes()[range.clone()],
        }
    }
    fn capacity_bytes(&self) -> usize {
        match self {
            Self::Raw(bytes) => bytes.capacity(),
            Self::Result { range, .. } => range.len(),
        }
    }
}

impl std::fmt::Debug for StreamView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamView")
            .field("stream", &self.stream)
            .field("next_offset", &self.next_offset)
            .field("retained_bytes", &self.retained_bytes)
            .finish()
    }
}

impl StreamView {
    pub fn capacity_bytes(&self) -> usize {
        self.chunks.iter().map(StreamChunk::capacity_bytes).sum()
    }

    fn push_raw(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let page = crate::limits::TOOL_PAGE_BYTES;
            if let Some(StreamChunk::Raw(last)) = self
                .chunks
                .back_mut()
                .filter(|chunk| chunk.bytes().len() < page)
            {
                let last = Arc::make_mut(last);
                // A layout may share this tail; after copy-on-write reserve
                // exactly one page, not Vec's potentially doubled capacity.
                last.reserve_exact(page - last.len());
                let take = (page - last.len()).min(bytes.len());
                last.extend_from_slice(&bytes[..take]);
                bytes = &bytes[take..];
            } else {
                let take = page.min(bytes.len());
                let mut chunk = Vec::with_capacity(page);
                chunk.extend_from_slice(&bytes[..take]);
                self.chunks.push_back(StreamChunk::Raw(Arc::new(chunk)));
                bytes = &bytes[take..];
            }
        }
    }

    pub fn new(stream: Stream) -> Self {
        Self {
            stream,
            next_offset: 0,
            base_offset: 0,
            observed_end: 0,
            eof: false,
            availability: Availability::Pending,
            truncated: false,
            gap: false,
            chunks: Default::default(),
            retained_bytes: 0,
            revision: 0,
        }
    }

    pub fn accept_page(
        &mut self,
        page: &crate::protocol::tool::ToolOutputPage,
    ) -> Result<(), &'static str> {
        self.accept_page_with_result(page, None)
    }

    pub fn accept_page_with_result(
        &mut self,
        page: &crate::protocol::tool::ToolOutputPage,
        result: Option<&Arc<str>>,
    ) -> Result<(), &'static str> {
        if page.stream != self.stream {
            return Err("wrong tool stream");
        }
        let bytes = decode_stream(self.stream, &page.encoding, &page.data)?;
        let stale = page.next_offset < self.next_offset;
        self.append(
            page.base_offset,
            page.next_offset,
            page.observed_end,
            &bytes,
            result.filter(|result| {
                self.stream == Stream::Output && result.len() <= crate::limits::TOOL_STREAM_BYTES
            }),
        )?;
        self.eof = page.eof && !stale;
        if !stale {
            self.availability = page.availability;
        }
        self.truncated |= page.truncated;
        if self.base_offset == 0
            && self.next_offset >= self.observed_end
            && !self.truncated
            && self.availability == Availability::Available
        {
            self.gap = false;
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn accept_event(
        &mut self,
        chunk: &crate::protocol::ToolProcessChunkWire,
    ) -> Result<(), &'static str> {
        if chunk.stream != self.stream {
            return Err("wrong tool stream");
        }
        let bytes = decode_stream(self.stream, &chunk.encoding, &chunk.data)?;
        if chunk.base_offset > chunk.next_offset
            || chunk.next_offset > chunk.observed_end
            || chunk.next_offset - chunk.base_offset != bytes.len() as u64
        {
            return Err("invalid raw tool event range");
        }
        if chunk.base_offset > self.next_offset || chunk.expired {
            // Notifications are hints, not proof that the missing range was
            // evicted. Keep the cursor so tool.output can recover that range.
            self.gap = true;
            self.truncated |= chunk.expired;
            self.observed_end = self.observed_end.max(chunk.observed_end);
            return Ok(());
        }
        self.append(
            chunk.base_offset,
            chunk.next_offset,
            chunk.observed_end,
            &bytes,
            None,
        )?;
        self.truncated |= chunk.dropped || chunk.expired;
        self.availability = if chunk.expired {
            Availability::Expired
        } else {
            Availability::Available
        };
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    fn append(
        &mut self,
        base: u64,
        next: u64,
        end: u64,
        bytes: &[u8],
        result: Option<&Arc<str>>,
    ) -> Result<(), &'static str> {
        if base > next || next > end || next - base != bytes.len() as u64 {
            return Err("invalid raw tool stream range");
        }
        if next < self.next_offset {
            return Ok(());
        }
        if base > self.next_offset {
            // A gap discards any dangling Unicode prefix too. Never join two
            // disjoint byte ranges as though the missing output were present.
            self.chunks.clear();
            self.retained_bytes = 0;
            self.base_offset = base;
            self.gap = true;
        }
        let skip = self
            .next_offset
            .saturating_sub(base)
            .min(bytes.len() as u64) as usize;
        if !bytes[skip..].is_empty() {
            if self.chunks.is_empty() {
                self.base_offset = base + skip as u64;
            }
            let bytes = &bytes[skip..];
            for (index, chunk) in bytes.chunks(crate::limits::TOOL_PAGE_BYTES).enumerate() {
                let start = usize::try_from(base).ok().and_then(|base| {
                    base.checked_add(skip + index * crate::limits::TOOL_PAGE_BYTES)
                });
                let shared = result.zip(start).filter(|(source, start)| {
                    source
                        .as_bytes()
                        .get(*start..start.saturating_add(chunk.len()))
                        == Some(chunk)
                });
                match shared {
                    Some((source, start)) => {
                        if let Some(StreamChunk::Result { source: previous, range }) = self.chunks.back_mut().filter(|chunk| matches!(chunk, StreamChunk::Result { source: previous, range } if Arc::ptr_eq(previous, source) && range.end == start)) {
                            debug_assert!(Arc::ptr_eq(previous, source));
                            range.end += chunk.len();
                        } else {
                            self.chunks.push_back(StreamChunk::Result { source: source.clone(), range: start..start + chunk.len() });
                        }
                    }
                    None => self.push_raw(chunk),
                }
                self.retained_bytes += chunk.len();
            }
        }
        self.next_offset = next;
        self.observed_end = self.observed_end.max(end);
        let mut capacity = self.capacity_bytes();
        while capacity > crate::limits::TOOL_STREAM_BYTES
            || self.chunks.len() > crate::limits::TOOL_STREAM_CHUNKS
        {
            let old = self.chunks.pop_front().expect("charged chunk");
            capacity -= old.capacity_bytes();
            self.retained_bytes -= old.bytes().len();
            self.base_offset += old.bytes().len() as u64;
            self.truncated = true;
            self.gap = true;
        }
        Ok(())
    }

    /// Runs on the serialized layout worker. Partial trailing codepoints are
    /// withheld before EOF (at most three bytes); invalid bytes use U+FFFD.
    /// No sanitized/display length ever feeds the RPC cursor.
    pub fn display_text(&self) -> String {
        let bytes: Vec<u8> = self
            .chunks
            .iter()
            .flat_map(|chunk| chunk.bytes().iter().copied())
            .collect();
        let mut rest = bytes.as_slice();
        let mut text = String::new();
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(std::str::from_utf8(&rest[..valid]).expect("valid prefix"));
                    rest = &rest[valid..];
                    match error.error_len() {
                        Some(len) => {
                            text.push('\u{fffd}');
                            rest = &rest[len..];
                        }
                        None => {
                            if self.eof {
                                text.push('\u{fffd}');
                            }
                            break;
                        }
                    }
                }
            }
        }
        crate::safe_text::safe_display(&text).into_owned()
    }
}

fn decode_stream(stream: Stream, encoding: &str, data: &str) -> Result<Vec<u8>, &'static str> {
    use base64::Engine;
    match (stream, encoding) {
        (Stream::Input, "utf8_json") | (Stream::Output, "utf8") => Ok(data.as_bytes().to_vec()),
        (Stream::Stdout | Stream::Stderr, "base64") => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "invalid tool base64"),
        _ => Err("invalid tool stream encoding"),
    }
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct ToolKey {
    pub session_id: String,
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
}

impl ToolKey {
    pub fn new(session_id: &str, loop_id: &str, request_index: u32, tool_call_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            loop_id: loop_id.to_owned(),
            request_index,
            tool_call_id: tool_call_id.to_owned(),
        }
    }
}

/// Allocated only after an inline card is expanded. Completed text is moved to
/// the existing result owner; the page window then releases its byte chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineToolLoad {
    pub epoch: u64,
    pub generation: u64,
    pub read: bool,
    pub pending: bool,
    pub due: std::time::Instant,
    pub error: Option<String>,
    pub output: StreamView,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamLineCount {
    next: u64,
    newlines: usize,
    seen: bool,
    pub gap: bool,
}

impl InlineToolLoad {
    pub fn retry(&mut self, now: std::time::Instant) {
        if self.error.take().is_none() {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.read = false;
        self.pending = false;
        self.due = now;
        self.output = StreamView::new(Stream::Output);
    }
}

/// The single semantic owner for one tool call. The presentation map owns
/// these facts; live and durable cards retain the shared result `Arc<str>`
/// projected from them instead of copying the body. `status`/`outcome` are
/// monotonic: a late `started` event cannot move a terminal fact back to
/// running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFacts {
    pub display: Arc<ToolDisplayWire>,
    pub result: Option<Arc<str>>,
    pub result_truncated: bool,
    pub output_line_count: Option<usize>,
    pub inline: Option<InlineToolLoad>,
    pub body_deferred: bool,
    pub count_partial: bool,
    pub stream_lines: [StreamLineCount; 2],
    pub status: ToolStatus,
    pub outcome: Option<crate::protocol::ToolOutcomeWire>,
    pub needs_read: bool,
    pub input_available: bool,
    pub conflict: Option<ToolConflict>,
    pub invocation: Option<Arc<crate::protocol::ToolInvocationWire>>,
    pub execution: Option<Arc<crate::protocol::ToolExecutionWire>>,
    pub command: Option<Arc<crate::protocol::CommandResultWire>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolConflict {
    pub retained_outcome: crate::protocol::ToolOutcomeWire,
    pub observed_outcome: crate::protocol::ToolOutcomeWire,
}

/// Compatibility name retained for existing render/source APIs. The map in
/// `SessionView` is now explicitly a `ToolKey -> ToolFacts` owner.
pub type ToolPresentationState = ToolFacts;

impl ToolFacts {
    pub fn new(name: &str) -> Self {
        Self {
            display: Arc::new(ToolDisplayWire {
                body_truncated: false,
                detail: name.to_owned(),
                expanded_input: None,
                input_line_count: None,
                hidden_line_count: None,
                truncated: false,
            }),
            result: None,
            result_truncated: false,
            output_line_count: None,
            inline: None,
            body_deferred: false,
            count_partial: false,
            stream_lines: Default::default(),
            status: ToolStatus::Pending,
            outcome: None,
            needs_read: false,
            input_available: false,
            conflict: None,
            invocation: None,
            execution: None,
            command: None,
        }
    }

    pub fn accept_execution(
        &mut self,
        mut execution: crate::protocol::ToolExecutionWire,
        authoritative: bool,
    ) {
        if self.is_terminal() && !execution.state.is_terminal() {
            return;
        }
        if let Some(count) = execution.output_line_count {
            self.output_line_count = Some(count);
            self.stream_lines = Default::default();
            self.count_partial = self.display.body_truncated || execution.result_truncated;
        }
        self.input_available |= matches!(
            execution.input_availability,
            Availability::Available | Availability::Partial | Availability::Expired
        );
        if !authoritative && self.is_terminal() && self.outcome != execution.outcome {
            if let Some(outcome) = execution.outcome {
                self.accept_finished(outcome, None, execution.result_truncated);
            }
            self.needs_read = true;
            return;
        }
        if authoritative {
            self.needs_read = false;
            self.conflict = None;
        }
        if let Some(outcome) = execution.outcome {
            if authoritative {
                self.status = ToolStatus::Pending;
            }
            self.accept_finished(outcome, None, execution.result_truncated);
        } else {
            self.status = if matches!(
                execution.state,
                crate::protocol::ToolExecutionStateWire::Running
                    | crate::protocol::ToolExecutionStateWire::Cancelling
            ) {
                ToolStatus::Running
            } else {
                ToolStatus::Pending
            };
        }
        if let Some(command) = execution.command.take() {
            self.accept_command(command);
        }
        self.execution = Some(Arc::new(execution));
    }

    pub fn accept_process_count(&mut self, chunk: &crate::protocol::ToolProcessChunkWire) {
        if self.is_terminal() {
            return;
        }
        let index = match chunk.stream {
            Stream::Stdout => 0,
            Stream::Stderr => 1,
            _ => return,
        };
        let Ok(bytes) = decode_stream(chunk.stream, &chunk.encoding, &chunk.data) else {
            return;
        };
        let count = &mut self.stream_lines[index];
        if chunk.next_offset <= count.next {
            return;
        }
        count.gap |= chunk.base_offset > count.next || chunk.expired || chunk.dropped;
        let skip = count.next.saturating_sub(chunk.base_offset) as usize;
        if skip > bytes.len() {
            count.gap = true;
            return;
        }
        let added = &bytes[skip..];
        count.newlines = count
            .newlines
            .saturating_add(added.iter().filter(|byte| **byte == b'\n').count());
        count.seen |= !added.is_empty();
        count.next = chunk.next_offset;
        self.output_line_count = Some(
            self.stream_lines
                .iter()
                .map(|s| s.newlines + usize::from(s.seen))
                .sum(),
        );
    }

    pub fn accept_command(&mut self, command: crate::protocol::CommandResultWire) {
        use crate::protocol::CommandStatusWire::{Cancelling, Running};
        if self
            .command
            .as_ref()
            .is_some_and(|old| !matches!(old.status, Running | Cancelling))
            && matches!(command.status, Running | Cancelling)
        {
            return;
        }
        self.command = Some(Arc::new(command));
    }

    pub fn retained_bytes(&self) -> usize {
        self.invocation_bytes()
            + self.execution_bytes()
            + self.display.detail.capacity()
            + self
                .display
                .expanded_input
                .as_ref()
                .map_or(0, String::capacity)
            + self.result.as_ref().map_or(0, |result| result.len())
            + self.inline.as_ref().map_or(0, |load| {
                load.output.capacity_bytes() + load.error.as_ref().map_or(0, String::capacity)
            })
    }

    fn invocation_bytes(&self) -> usize {
        self.invocation.as_ref().map_or(0, |invocation| {
            tool_ref_bytes(&invocation.tool_ref)
                + invocation.name.capacity()
                + invocation.input.preview.capacity()
                + invocation.input.encoding.capacity()
                + match &invocation.subject {
                    crate::protocol::ToolSubjectWire::Command { script, cwd } => {
                        script.capacity() + cwd.capacity()
                    }
                    crate::protocol::ToolSubjectWire::File { path } => path.capacity(),
                    crate::protocol::ToolSubjectWire::Other => 0,
                }
        })
    }

    fn execution_bytes(&self) -> usize {
        self.execution.as_ref().map_or(0, |execution| {
            tool_ref_bytes(&execution.tool_ref)
                + execution.name.capacity()
                + execution.started_at.as_ref().map_or(0, String::capacity)
                + execution.finished_at.as_ref().map_or(0, String::capacity)
        })
    }

    pub fn accept_started(&mut self, name: &str) {
        if self.is_terminal() {
            return;
        }
        Arc::make_mut(&mut self.display).detail = name.to_owned();
        self.status = ToolStatus::Running;
    }

    pub fn accept_finished(
        &mut self,
        outcome: crate::protocol::ToolOutcomeWire,
        result: Option<Arc<str>>,
        truncated: bool,
    ) {
        if self.is_terminal() && self.outcome != Some(outcome) {
            self.needs_read = true;
            self.conflict = Some(ToolConflict {
                retained_outcome: self.outcome.unwrap_or(outcome),
                observed_outcome: outcome,
            });
            self.result_truncated |= truncated;
            Arc::make_mut(&mut self.display).truncated |= truncated;
            return;
        }
        let same_terminal = self.is_terminal() && self.outcome == Some(outcome);
        if !self.is_terminal() {
            self.status = match outcome {
                crate::protocol::ToolOutcomeWire::Success
                | crate::protocol::ToolOutcomeWire::InputProvided => ToolStatus::Succeeded,
                crate::protocol::ToolOutcomeWire::Failed
                | crate::protocol::ToolOutcomeWire::Unknown => ToolStatus::Failed,
                crate::protocol::ToolOutcomeWire::Denied => ToolStatus::Denied,
                crate::protocol::ToolOutcomeWire::Cancelled => ToolStatus::Cancelled,
            };
            self.outcome = Some(outcome);
        }
        if result.is_some() && (self.result.is_none() || same_terminal) {
            self.output_line_count = result.as_deref().map(|text| {
                if text.is_empty() {
                    0
                } else {
                    text.bytes().filter(|b| *b == b'\n').count() + 1
                }
            });
            self.stream_lines = Default::default();
            self.result = result;
        }
        if self.display.hidden_line_count.is_none() {
            Arc::make_mut(&mut self.display).hidden_line_count = self
                .result
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(|text| text.split('\n').count());
        }
        self.result_truncated |= truncated;
        Arc::make_mut(&mut self.display).truncated |= truncated;
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            ToolStatus::Succeeded | ToolStatus::Failed | ToolStatus::Denied | ToolStatus::Cancelled
        )
    }

    pub fn truncate_to_bytes(&mut self, mut budget: usize) {
        if let Some(load) = self.inline.as_mut() {
            self.result_truncated |= !load.output.chunks.is_empty();
            load.output.chunks.clear();
            load.output.retained_bytes = 0;
            load.output.eof = true;
            let error_bytes = load.error.as_ref().map_or(0, String::capacity);
            if error_bytes > budget {
                load.error = None;
            } else {
                budget -= error_bytes;
            }
        }
        let invocation_bytes = self.invocation_bytes();
        let mut budget = if invocation_bytes > budget {
            self.invocation = None;
            Arc::make_mut(&mut self.display).truncated = true;
            budget
        } else {
            budget - invocation_bytes
        };
        let execution_bytes = self.execution_bytes();
        if execution_bytes > budget {
            self.execution = None;
            Arc::make_mut(&mut self.display).truncated = true;
        } else {
            budget -= execution_bytes;
        }
        let mut used = 0;
        let display = Arc::make_mut(&mut self.display);
        truncate_string(&mut display.detail, budget, &mut used);
        if let Some(input) = &mut display.expanded_input {
            let before = input.len();
            truncate_string(input, budget, &mut used);
            display.body_truncated |= input.len() < before;
        }
        if let Some(result) = &mut self.result {
            let available = budget.saturating_sub(used);
            if result.len() > available {
                let mut end = available;
                while end > 0 && !result.is_char_boundary(end) {
                    end -= 1;
                }
                *result = Arc::<str>::from(&result[..end]);
                self.result_truncated = true;
            }
        }
    }
}

fn tool_ref_bytes(tool_ref: &crate::protocol::ToolRefWire) -> usize {
    tool_ref.session_id.capacity() + tool_ref.loop_id.capacity() + tool_ref.tool_call_id.capacity()
}

fn truncate_string(value: &mut String, budget: usize, used: &mut usize) {
    let available = budget.saturating_sub(*used);
    if value.len() > available {
        let mut end = available;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    if value.capacity() > available {
        value.shrink_to_fit();
    }
    *used = (*used).saturating_add(value.capacity());
}

/// Tool call lifecycle as shown in the live, provisional view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveTool {
    pub tool_call_id: String,
    pub name: String,
    pub status: ToolStatus,
    pub progress: Option<String>,
    /// Agent-owned bounded display data, merged by full loop/request/call
    /// identity and never used to execute a tool.
    pub display: Option<Arc<ToolDisplayWire>>,
    pub result: Option<Arc<str>>,
    pub result_truncated: bool,
    pub expanded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolOutcomeWire;

    fn facts() -> ToolFacts {
        ToolFacts {
            display: Arc::new(ToolDisplayWire {
                body_truncated: false,
                detail: "tool".to_owned(),
                expanded_input: None,
                input_line_count: None,
                hidden_line_count: None,
                truncated: false,
            }),
            result: None,
            result_truncated: false,
            output_line_count: None,
            inline: None,
            body_deferred: false,
            count_partial: false,
            stream_lines: Default::default(),
            status: ToolStatus::Pending,
            outcome: None,
            needs_read: false,
            input_available: false,
            conflict: None,
            invocation: None,
            execution: None,
            command: None,
        }
    }

    #[test]
    fn terminal_facts_ignore_late_started_and_conflicting_finished_events() {
        let mut facts = facts();
        let first: Arc<str> = Arc::from("first");
        let conflicting: Arc<str> = Arc::from("conflicting");
        facts.accept_finished(ToolOutcomeWire::Success, Some(first.clone()), false);
        facts.accept_started("late-name");
        facts.accept_finished(ToolOutcomeWire::Failed, Some(conflicting), true);

        assert_eq!(facts.status, ToolStatus::Succeeded);
        assert_eq!(facts.outcome, Some(ToolOutcomeWire::Success));
        assert!(Arc::ptr_eq(facts.result.as_ref().unwrap(), &first));
        assert!(facts.result_truncated);
        assert!(facts.display.truncated);
        assert_eq!(facts.display.detail, "tool");
        assert!(facts.needs_read);
        assert_eq!(
            facts.conflict,
            Some(ToolConflict {
                retained_outcome: ToolOutcomeWire::Success,
                observed_outcome: ToolOutcomeWire::Failed,
            })
        );
    }

    #[test]
    fn finished_facts_reuse_an_existing_result_owner() {
        let mut facts = facts();
        let result: Arc<str> = Arc::from("shared");
        facts.result = Some(result.clone());
        facts.accept_finished(
            ToolOutcomeWire::Success,
            Some(Arc::from("duplicate")),
            false,
        );

        assert!(Arc::ptr_eq(facts.result.as_ref().unwrap(), &result));
    }

    #[test]
    fn retained_bytes_counts_display_capacity_after_truncation() {
        let mut detail = String::with_capacity(1024);
        detail.push_str("tool");
        let mut facts = facts();
        facts.display = Arc::new(ToolDisplayWire {
            body_truncated: false,
            detail,
            expanded_input: None,
            input_line_count: None,
            hidden_line_count: None,
            truncated: false,
        });
        facts.truncate_to_bytes(2);
        assert_eq!(facts.display.detail, "to");
        assert!(facts.retained_bytes() <= 2);
    }

    #[test]
    fn retained_bytes_accounts_for_owned_tool_metadata() {
        let mut facts = facts();
        facts.invocation = Some(Arc::new(crate::protocol::ToolInvocationWire {
            tool_ref: crate::protocol::ToolRefWire {
                session_id: "session".repeat(64),
                loop_id: "loop".repeat(64),
                request_index: 0,
                tool_call_id: "call".repeat(64),
            },
            name: "name".repeat(64),
            subject: crate::protocol::ToolSubjectWire::File {
                path: "path".repeat(64),
            },
            subject_truncated: false,
            input: crate::protocol::ToolInputSummaryWire {
                total_bytes: 0,
                preview: "preview".repeat(64),
                truncated: false,
                encoding: "utf8".to_owned(),
            },
        }));
        facts.execution = Some(Arc::new(crate::protocol::ToolExecutionWire {
            tool_ref: facts.invocation.as_ref().unwrap().tool_ref.clone(),
            name: "execution".repeat(64),
            state: crate::protocol::ToolExecutionStateWire::Running,
            phase: None,
            started_at: Some("started".repeat(64)),
            finished_at: None,
            outcome: None,
            input_availability: Availability::Available,
            output_availability: Availability::Pending,
            output_line_count: None,
            input_bytes: 0,
            result_bytes: 0,
            input_truncated: false,
            result_truncated: false,
            command: None,
            recording: crate::protocol::ToolRecordingStateWire::MemoryOnly,
        }));

        let retained = facts.retained_bytes();
        assert!(retained > facts.display.detail.len());
        facts.truncate_to_bytes(1);
        assert!(facts.invocation.is_none());
        assert!(facts.execution.is_none());
        assert!(facts.retained_bytes() <= 1);
    }
    #[test]
    fn historical_projection_only_exposes_whitelisted_bounded_targets() {
        use serde_json::json;
        for name in ["read", "write", "edit", "apply_patch", "patch"] {
            let display = history_display(name, &json!({"path": "notes.txt", "content": "PRIVATE BODY", "patch": "PRIVATE PATCH", "other": "PRIVATE"})).unwrap();
            assert_eq!(display.detail, "notes.txt");
            assert!(display.expanded_input.is_none());
            assert!(!display.truncated);
        }
        let command = "printf 'visible' >&2; exit 7";
        assert_eq!(
            history_display(
                "bash",
                &json!({"command": command, "env": {"SECRET": "PRIVATE"}})
            )
            .unwrap()
            .detail,
            command
        );
        for args in [
            json!(null),
            json!([]),
            json!({}),
            json!({"command": 4}),
            json!({"command": ""}),
            json!({"command": " \n "}),
        ] {
            assert!(history_display("bash", &args).is_none());
        }
        assert!(
            history_display("unknown", &json!({"path": "PRIVATE", "command": "PRIVATE"})).is_none()
        );
        let display = history_display(
            "bash",
            &json!({"command": format!("{}{}", "\u{1b}\u{202e}中文".repeat(300), "DO_NOT_RETAIN")}),
        )
        .unwrap();
        assert!(display.truncated);
        assert!(display.detail.ends_with('…'));
        assert!(display.detail.len() <= 512);
        assert!(!display.detail.contains('\u{1b}'));
        assert!(!display.detail.contains('\u{202e}'));
        assert!(!display.detail.contains("DO_NOT_RETAIN"));
        assert!(display.expanded_input.is_none());
    }
}

#[cfg(test)]
mod live_line_count_tests {
    use super::*;
    use base64::Engine;

    fn chunk(stream: Stream, base: u64, bytes: &[u8]) -> crate::protocol::ToolProcessChunkWire {
        crate::protocol::ToolProcessChunkWire {
            stream,
            encoding: "base64".into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            base_offset: base,
            next_offset: base + bytes.len() as u64,
            observed_end: base + bytes.len() as u64,
            dropped: false,
            expired: false,
        }
    }

    #[test]
    fn streamed_lines_count_new_suffix_only_and_flag_gaps_without_retaining_body() {
        let mut facts = ToolFacts::new("bash");
        facts.accept_started("bash");
        let first = chunk(Stream::Stdout, 0, b"a\n");
        facts.accept_process_count(&first);
        assert_eq!(facts.output_line_count, Some(2));
        facts.accept_process_count(&first);
        assert_eq!(facts.output_line_count, Some(2));
        facts.accept_process_count(&chunk(Stream::Stdout, 1, b"\nb\n"));
        assert_eq!(facts.output_line_count, Some(3));
        facts.accept_process_count(&chunk(Stream::Stderr, 0, b"err"));
        assert_eq!(facts.output_line_count, Some(4));
        facts.accept_process_count(&chunk(Stream::Stdout, 10, b"lost\n"));
        assert!(facts.stream_lines[0].gap);
        assert_eq!(facts.output_line_count, Some(5));
        assert!(facts.result.is_none());
        assert!(facts.display.expanded_input.is_none());
    }
}
