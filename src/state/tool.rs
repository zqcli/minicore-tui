//! Live tool call state (spec 12.7). One `LiveTool` per tool_call_id;
//! duplicate started/progress/finished events are idempotent.

use std::sync::Arc;

use crate::protocol::ToolDisplayWire;
use crate::protocol::{
    ToolDataAvailabilityWire as Availability, ToolDataStreamWire as Stream, ToolRefWire,
};

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
    pub status: ToolStatus,
    pub outcome: Option<crate::protocol::ToolOutcomeWire>,
    pub needs_read: bool,
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
                detail: name.to_owned(),
                expanded_input: None,
                input_line_count: None,
                hidden_line_count: None,
                truncated: false,
            }),
            result: None,
            result_truncated: false,
            status: ToolStatus::Pending,
            outcome: None,
            needs_read: false,
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
            + self.display.detail.len()
            + self.display.expanded_input.as_ref().map_or(0, String::len)
            + self.result.as_ref().map_or(0, |result| result.len())
    }

    fn invocation_bytes(&self) -> usize {
        self.invocation.as_ref().map_or(0, |invocation| {
            invocation.name.capacity()
                + invocation.input.preview.capacity()
                + match &invocation.subject {
                    crate::protocol::ToolSubjectWire::Command { script, cwd } => {
                        script.capacity() + cwd.capacity()
                    }
                    crate::protocol::ToolSubjectWire::File { path } => path.capacity(),
                    crate::protocol::ToolSubjectWire::Other => 0,
                }
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

    pub fn truncate_to_bytes(&mut self, budget: usize) {
        let invocation_bytes = self.invocation_bytes();
        let budget = if invocation_bytes > budget {
            self.invocation = None;
            Arc::make_mut(&mut self.display).truncated = true;
            budget
        } else {
            budget - invocation_bytes
        };
        let mut used = 0;
        let display = Arc::make_mut(&mut self.display);
        truncate_string(&mut display.detail, budget, &mut used);
        if let Some(input) = &mut display.expanded_input {
            truncate_string(input, budget, &mut used);
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

fn truncate_string(value: &mut String, budget: usize, used: &mut usize) {
    let available = budget.saturating_sub(*used);
    if value.len() > available {
        let mut end = available;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    *used = (*used).saturating_add(value.len());
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
                detail: "tool".to_owned(),
                expanded_input: None,
                input_line_count: None,
                hidden_line_count: None,
                truncated: false,
            }),
            result: None,
            result_truncated: false,
            status: ToolStatus::Pending,
            outcome: None,
            needs_read: false,
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
}
