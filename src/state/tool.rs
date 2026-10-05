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
/// Detail tabs and provisional inline previews use independently bounded
/// windows. Offsets always refer to the original raw bytes.
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
    /// Provisional stdout/stderr windows; never promoted to the final result.
    pub process_output: Option<[StreamView; 2]>,
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
            process_output: None,
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

    pub fn accept_process_chunk(&mut self, chunk: &crate::protocol::ToolProcessChunkWire) {
        if self.is_terminal() || self.process_finished() {
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
        if chunk.base_offset > chunk.next_offset
            || chunk.next_offset > chunk.observed_end
            || chunk.next_offset - chunk.base_offset != bytes.len() as u64
        {
            return;
        }
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
        let output = self.process_output.get_or_insert_with(|| {
            [
                StreamView::new(Stream::Stdout),
                StreamView::new(Stream::Stderr),
            ]
        });
        let preview = &mut output[index];
        // Unlike detail recovery, inline previews keep the latest contiguous
        // segment. This never changes the detail panel's recovery cursor.
        preview
            .append(
                chunk.base_offset,
                chunk.next_offset,
                chunk.observed_end,
                &bytes,
                None,
            )
            .expect("validated process range");
        preview.truncated |= chunk.dropped || chunk.expired;
        preview.availability = if chunk.expired {
            Availability::Expired
        } else {
            Availability::Available
        };
        preview.revision = preview.revision.wrapping_add(1);
    }

    fn process_finished(&self) -> bool {
        self.command.as_ref().is_some_and(|command| {
            !matches!(
                command.status,
                crate::protocol::CommandStatusWire::Running
                    | crate::protocol::CommandStatusWire::Cancelling
            )
        })
    }

    pub fn process_count_partial(&self) -> bool {
        if self.result.is_some()
            || self
                .execution
                .as_ref()
                .is_some_and(|execution| execution.output_line_count.is_some())
        {
            return false;
        }
        self.stream_lines.iter().any(|count| count.gap)
            || self.process_output.as_ref().is_some_and(|streams| {
                streams
                    .iter()
                    .zip(&self.stream_lines)
                    .any(|(stream, count)| stream.observed_end > count.next)
            })
            || self.command.as_ref().is_some_and(|command| {
                command.stdout_observed_end > self.stream_lines[0].next
                    || command.stderr_observed_end > self.stream_lines[1].next
            })
    }

    pub fn process_output_partial(&self) -> bool {
        if self.result.is_some() {
            return false;
        }
        self.process_output.as_ref().is_some_and(|streams| {
            streams.iter().any(|stream| {
                stream.gap || stream.truncated || stream.next_offset < stream.observed_end
            })
        }) || self.command.as_ref().is_some_and(|command| {
            command.output_truncated
                || command.stdout_observed_end
                    > self
                        .process_output
                        .as_ref()
                        .map_or(0, |streams| streams[0].next_offset)
                || command.stderr_observed_end
                    > self
                        .process_output
                        .as_ref()
                        .map_or(0, |streams| streams[1].next_offset)
        })
    }

    /// Only safe display input and the explicit command subject may become a body.
    pub fn input_text(&self) -> Option<(&str, bool)> {
        self.display
            .expanded_input
            .as_deref()
            .map(|text| (text, self.display.body_truncated))
            .or_else(|| {
                self.invocation
                    .as_ref()
                    .and_then(|invocation| match &invocation.subject {
                        crate::protocol::ToolSubjectWire::Command { script, .. } => {
                            Some((script.as_str(), invocation.subject_truncated))
                        }
                        _ => None,
                    })
            })
    }

    pub fn input_line_count(&self) -> Option<(usize, bool)> {
        self.display
            .input_line_count
            .map(|count| (count, self.display.body_truncated))
            .or_else(|| {
                self.input_text().map(|(text, partial)| {
                    (
                        if text.is_empty() {
                            0
                        } else {
                            text.bytes().filter(|b| *b == b'\n').count() + 1
                        },
                        partial,
                    )
                })
            })
    }

    pub fn accept_command(&mut self, command: crate::protocol::CommandResultWire) {
        use crate::protocol::CommandStatusWire::{Cancelling, Running};
        if self.is_terminal() && matches!(command.status, Running | Cancelling) {
            return;
        }
        if self
            .command
            .as_ref()
            .is_some_and(|old| !matches!(old.status, Running | Cancelling))
            && matches!(command.status, Running | Cancelling)
        {
            return;
        }
        if let Some(streams) = &mut self.process_output {
            for (stream, end) in streams
                .iter_mut()
                .zip([command.stdout_observed_end, command.stderr_observed_end])
            {
                let observed_end = stream.observed_end.max(end);
                let eof = self.status != ToolStatus::Pending && self.status != ToolStatus::Running
                    || !matches!(command.status, Running | Cancelling);
                if stream.observed_end != observed_end || stream.eof != eof {
                    stream.observed_end = observed_end;
                    stream.eof = eof;
                    stream.revision = stream.revision.wrapping_add(1);
                }
            }
        }
        self.command = Some(Arc::new(command));
    }

    pub fn retained_bytes(&self) -> usize {
        self.invocation_bytes()
            + self.execution_bytes()
            + self.process_output.as_ref().map_or(0, |streams| {
                streams
                    .iter()
                    .map(StreamView::capacity_bytes)
                    .sum::<usize>()
            })
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
            self.process_output = None;
        } else if let Some(streams) = &mut self.process_output {
            for stream in streams {
                if !stream.eof {
                    stream.eof = true;
                    stream.revision = stream.revision.wrapping_add(1);
                }
            }
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
            used = used.saturating_add(result.len());
        }
        if let Some(streams) = &mut self.process_output {
            let nonempty = streams
                .iter()
                .filter(|stream| !stream.chunks.is_empty())
                .count()
                .max(1);
            let share = budget.saturating_sub(used) / nonempty;
            for stream in streams {
                let mut capacity = stream.capacity_bytes();
                while capacity > share {
                    let old = stream.chunks.pop_front().expect("charged process chunk");
                    capacity -= old.capacity_bytes();
                    stream.retained_bytes -= old.bytes().len();
                    stream.base_offset += old.bytes().len() as u64;
                    stream.truncated = true;
                    stream.gap = true;
                    stream.revision = stream.revision.wrapping_add(1);
                }
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
            process_output: None,
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
    fn streamed_lines_count_new_suffix_only_and_keep_a_bounded_preview() {
        let mut facts = ToolFacts::new("bash");
        facts.accept_started("bash");
        let first = chunk(Stream::Stdout, 0, b"a\n");
        facts.accept_process_chunk(&first);
        assert_eq!(facts.output_line_count, Some(2));
        facts.accept_process_chunk(&first);
        assert_eq!(facts.output_line_count, Some(2));
        facts.accept_process_chunk(&chunk(Stream::Stdout, 1, b"\nb\n"));
        assert_eq!(facts.output_line_count, Some(3));
        facts.accept_process_chunk(&chunk(Stream::Stderr, 0, b"err"));
        assert_eq!(facts.output_line_count, Some(4));
        facts.accept_process_chunk(&chunk(Stream::Stdout, 10, b"lost\n"));
        assert!(facts.stream_lines[0].gap);
        assert_eq!(facts.output_line_count, Some(5));
        assert!(facts.result.is_none());
        assert!(facts.display.expanded_input.is_none());
    }

    fn command(
        status: crate::protocol::CommandStatusWire,
        stdout: u64,
        stderr: u64,
    ) -> crate::protocol::CommandResultWire {
        crate::protocol::CommandResultWire {
            status,
            exit_code: None,
            signal: None,
            termination_confirmed: false,
            stdout_base_offset: 0,
            stdout_observed_end: stdout,
            stderr_base_offset: 0,
            stderr_observed_end: stderr,
            output_complete: false,
            output_truncated: false,
        }
    }

    fn preview(facts: &ToolFacts, index: usize) -> String {
        facts.process_output.as_ref().unwrap()[index].display_text()
    }

    #[test]
    fn preview_is_incremental_deduplicated_and_keeps_split_utf8() {
        let mut facts = ToolFacts::new("bash");
        let text = "中🙂".as_bytes();
        facts.accept_process_chunk(&chunk(Stream::Stdout, 0, &text[..2]));
        assert_eq!(preview(&facts, 0), "");
        facts.accept_process_chunk(&chunk(Stream::Stdout, 2, &text[2..5]));
        assert_eq!(preview(&facts, 0), "中");
        facts.accept_process_chunk(&chunk(Stream::Stdout, 4, &text[4..]));
        facts.accept_process_chunk(&chunk(Stream::Stdout, 0, text));
        assert_eq!(preview(&facts, 0), "中🙂");
        facts.accept_process_chunk(&chunk(Stream::Stderr, 0, b"warning\x1b[31m"));
        assert_eq!(preview(&facts, 0), "中🙂");
        assert!(preview(&facts, 1).starts_with("warning"));
        assert!(!preview(&facts, 1).contains('\x1b'));
        assert_eq!(facts.output_line_count, Some(2));
    }

    #[test]
    fn invalid_ranges_cannot_change_preview_or_count_and_gaps_keep_latest_segment() {
        let mut facts = ToolFacts::new("bash");
        facts.accept_process_chunk(&chunk(Stream::Stdout, 0, b"prefix\xe4"));
        let before = facts.clone();
        let valid = chunk(Stream::Stdout, 7, b"tail");
        for bad in [
            crate::protocol::ToolProcessChunkWire {
                next_offset: 6,
                ..valid.clone()
            },
            crate::protocol::ToolProcessChunkWire {
                observed_end: 8,
                ..valid.clone()
            },
            crate::protocol::ToolProcessChunkWire {
                next_offset: 10,
                ..valid.clone()
            },
            crate::protocol::ToolProcessChunkWire {
                data: "not base64!".into(),
                ..valid.clone()
            },
            crate::protocol::ToolProcessChunkWire {
                encoding: "utf8".into(),
                ..valid.clone()
            },
        ] {
            facts.accept_process_chunk(&bad);
            assert_eq!(facts, before);
        }
        facts.accept_process_chunk(&chunk(Stream::Stdout, 20, b"latest"));
        assert_eq!(preview(&facts, 0), "latest");
        assert!(facts.process_output_partial());
        assert!(facts.process_count_partial());
        let after = facts.clone();
        facts.accept_process_chunk(&chunk(Stream::Stdout, 7, b"stale"));
        assert_eq!(facts, after);
        assert!(facts.result.is_none());
    }

    #[test]
    fn command_metadata_exposes_a_missing_tail_then_final_result_replaces_preview() {
        use crate::protocol::{CommandStatusWire as S, ToolOutcomeWire};
        let mut facts = ToolFacts::new("bash");
        facts.accept_process_chunk(&chunk(Stream::Stdout, 0, b"a"));
        let revision = crate::ui::tool::facts_revision(&facts);
        facts.accept_command(command(S::Running, 2, 0));
        assert_ne!(crate::ui::tool::facts_revision(&facts), revision);
        assert!(facts.process_count_partial());
        assert!(facts.process_output_partial());
        facts.accept_process_chunk(&chunk(Stream::Stdout, 1, b"b"));
        assert!(!facts.process_count_partial());
        assert!(!facts.process_output_partial());
        facts.accept_command(command(S::Exited, 2, 0));
        let completed = facts.clone();
        facts.accept_process_chunk(&chunk(Stream::Stdout, 2, b"late"));
        facts.accept_command(command(S::Running, 6, 0));
        assert_eq!(facts, completed);
        facts.accept_finished(ToolOutcomeWire::Success, Some(Arc::from("final")), false);
        assert!(facts.process_output.is_none());
        assert_eq!(facts.result.as_deref(), Some("final"));
        assert!(!facts.process_output_partial());
        assert!(!facts.process_count_partial());
    }

    #[test]
    fn cancellation_freezes_preview_without_promoting_it_to_result() {
        use crate::protocol::{CommandStatusWire as S, ToolOutcomeWire};
        let mut facts = ToolFacts::new("bash");
        facts.accept_process_chunk(&chunk(Stream::Stdout, 0, b"ok\xe4"));
        facts.accept_finished(ToolOutcomeWire::Cancelled, None, false);
        assert_eq!(preview(&facts, 0), "ok�");
        assert!(facts.result.is_none());
        let cancelled = facts.clone();
        facts.accept_command(command(S::Running, 10, 0));
        facts.accept_process_chunk(&chunk(Stream::Stdout, 3, b"late"));
        assert_eq!(facts, cancelled);
    }

    #[test]
    fn terminal_without_result_does_not_claim_missing_tail_or_whole_stream_is_complete() {
        use crate::protocol::{CommandStatusWire as S, ToolOutcomeWire};
        for received in [false, true] {
            let mut facts = ToolFacts::new("bash");
            if received {
                facts.accept_process_chunk(&chunk(Stream::Stdout, 0, b"prefix"));
            }
            facts.accept_command(command(S::Exited, 30, 10));
            facts.accept_finished(ToolOutcomeWire::Success, None, false);
            assert!(facts.process_count_partial());
            assert!(facts.process_output_partial());
            assert!(facts.result.is_none());
        }
    }

    #[test]
    fn command_subject_supplies_multiline_input_and_marks_only_fallback_counts_partial() {
        let mut facts = ToolFacts::new("bash");
        facts.invocation = Some(Arc::new(crate::protocol::ToolInvocationWire {
            tool_ref: (&ToolKey::new("s", "l", 0, "c")).into(),
            name: "bash".into(),
            subject: crate::protocol::ToolSubjectWire::Command {
                script: "build\nlink".into(),
                cwd: ".".into(),
            },
            subject_truncated: false,
            input: crate::protocol::ToolInputSummaryWire {
                total_bytes: 0,
                preview: "PRIVATE JSON".into(),
                truncated: false,
                encoding: "utf8_json".into(),
            },
        }));
        assert_eq!(facts.input_text(), Some(("build\nlink", false)));
        assert_eq!(facts.input_line_count(), Some((2, false)));
        let revision = crate::ui::tool::facts_revision(&facts);
        Arc::make_mut(facts.invocation.as_mut().unwrap()).subject_truncated = true;
        assert_eq!(facts.input_line_count(), Some((2, true)));
        assert_ne!(crate::ui::tool::facts_revision(&facts), revision);
        Arc::make_mut(&mut facts.display).input_line_count = Some(7);
        assert_eq!(facts.input_line_count(), Some((7, false)));
        Arc::make_mut(&mut facts.display).input_line_count = None;
        Arc::make_mut(&mut facts.display).expanded_input = Some(String::new());
        assert_eq!(facts.input_text(), Some(("", false)));
        assert_eq!(facts.input_line_count(), Some((0, false)));
    }

    #[test]
    fn two_stream_tail_windows_obey_whole_tool_budget_without_losing_exact_count() {
        let mut facts = ToolFacts::new("bash");
        let page = b"log\n".repeat(crate::limits::TOOL_PAGE_BYTES / 4);
        for index in 0..100 {
            for stream in [Stream::Stdout, Stream::Stderr] {
                facts.accept_process_chunk(&chunk(stream, (index * page.len()) as u64, &page));
            }
            facts.truncate_to_bytes(crate::limits::TOOL_STREAM_BYTES);
            assert!(facts.retained_bytes() <= crate::limits::TOOL_STREAM_BYTES);
            assert_eq!(
                facts.output_line_count,
                Some(2 * ((index + 1) * (page.len() / 4) + 1))
            );
        }
        assert!(facts.process_output_partial());
        assert!(!facts.process_count_partial());
        let count = facts.output_line_count;
        facts.truncate_to_bytes(0);
        assert_eq!(facts.retained_bytes(), 0);
        assert_eq!(facts.output_line_count, count);
    }
    #[test]
    fn chunk_observed_tail_remains_a_lower_bound_without_command_metadata() {
        let mut facts = ToolFacts::new("bash");
        let mut event = chunk(Stream::Stdout, 0, b"a\n");
        event.observed_end = 20;
        facts.accept_process_chunk(&event);
        assert!(facts.process_count_partial());
        assert!(facts.process_output_partial());
        facts.accept_finished(crate::protocol::ToolOutcomeWire::Success, None, false);
        assert!(facts.command.is_none());
        assert!(facts.process_count_partial());
        assert!(facts.process_output_partial());
    }
}
