//! `session.read` / `turn.result` DTOs and the chunk assembler.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{LoopOutcome, SessionInfo, ToolSummary, TurnRef, Usage};

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// One canonical item body, complete and contiguous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedItem {
    pub index: usize,
    pub data: Box<str>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReadChunk {
    pub index: usize,
    pub offset: usize,
    pub total_bytes: usize,
    pub encoding: String,
    pub data: String,
    pub complete: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReadSessionResult {
    pub session: SessionInfo,
    pub items: Vec<ReadChunk>,
    #[serde(deserialize_with = "required_option")]
    pub next_cursor: Option<ReadCursor>,
    pub total: usize,
    pub history_revision: String,
    pub captured_end: u64,
    pub projection: Option<DisplayProjection>,
    /// Summaries of the turns that have an item in this page.
    pub records: Vec<TurnRecord>,
}

/// How a stored turn ended.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TurnRecord {
    pub loop_id: String,
    pub outcome: LoopOutcome,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub requests: u32,
    pub interrupted: Option<InterruptedRequest>,
}

/// What a cancelled request had streamed; display-only, never a history item.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct InterruptedRequest {
    pub request_index: u32,
    pub reasoning: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DisplayProjection {
    pub revision: String,
    pub first_item: usize,
    pub covered_item_count: usize,
    /// The usage of the turns behind the summary.
    pub covered_usage: CoveredUsage,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CoveredUsage {
    pub usage: Usage,
    pub partial: bool,
}

impl ReadSessionResult {
    pub fn pin(&self) -> SnapshotPin {
        SnapshotPin {
            captured_end: self.captured_end,
            history_revision: self.history_revision.clone(),
            projection_revision: self
                .projection
                .as_ref()
                .map(|projection| projection.revision.clone()),
            total: self.total,
            first_item: self
                .projection
                .as_ref()
                .map_or(0, |projection| projection.first_item),
            covered_usage: self
                .projection
                .as_ref()
                .map(|projection| projection.covered_usage.clone()),
        }
    }
}

/// The read prefix a continuation is pinned to. Never cached across
/// operations: every read starts from a fresh probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotPin {
    pub captured_end: u64,
    pub history_revision: String,
    pub projection_revision: Option<String>,
    pub total: usize,
    pub first_item: usize,
    pub covered_usage: Option<CoveredUsage>,
}

/// A read position: an item index and a byte offset inside that item.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReadCursor {
    pub item: usize,
    pub offset: usize,
}

impl ReadCursor {
    pub fn start() -> Self {
        Self { item: 0, offset: 0 }
    }

    pub fn value(self) -> Value {
        serde_json::json!({"item": self.item, "offset": self.offset})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChunkOutcome {
    Pending,
    Item(EncodedItem),
    /// The item is too large to auto-decode; the caller shows a placeholder.
    Oversized {
        index: usize,
        total_bytes: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("unexpected chunk encoding {0}")]
    Encoding(String),
    #[error("chunk for item {found} while assembling {expected}")]
    OutOfOrder { expected: usize, found: usize },
    #[error("chunk offset {found} where {expected} was expected")]
    OffsetMismatch { expected: usize, found: usize },
    #[error("item {index} declares {declared} bytes but delivered {delivered}")]
    ByteCount {
        index: usize,
        declared: usize,
        delivered: usize,
    },
    #[error("an incomplete chunk made no progress")]
    EmptyChunk,
    #[error("an incomplete chunk reached its declared byte count")]
    IncompleteAtEnd,
    #[error("could not reserve the item buffer")]
    Allocation,
}

/// Reassembles items from read chunks; one item may span pages, so the buffer
/// outlives a page.
#[derive(Debug, Default)]
pub struct ChunkAssembler {
    index: usize,
    next_offset: usize,
    total_bytes: usize,
    buffer: String,
    active: bool,
    skipping: bool,
}

/// Items above this size are shown as a placeholder instead of being decoded.
pub const MAX_AUTO_ITEM_BYTES: usize = 8 * 1024 * 1024;

impl ChunkAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cursor to continue a partially assembled item from.
    pub fn next_cursor(&self) -> Option<ReadCursor> {
        self.active.then_some(ReadCursor {
            item: self.index,
            offset: self.next_offset,
        })
    }

    fn discard(&mut self) {
        *self = Self::default();
    }

    pub fn push(&mut self, chunk: ReadChunk) -> Result<ChunkOutcome, ReadError> {
        if chunk.encoding != "utf8_json" {
            return Err(ReadError::Encoding(chunk.encoding));
        }
        let delivered = chunk.data.len();
        if delivered == 0 && !chunk.complete {
            return Err(ReadError::EmptyChunk);
        }
        if self.active && chunk.index != self.index {
            return Err(ReadError::OutOfOrder {
                expected: self.index,
                found: chunk.index,
            });
        }
        if !self.active {
            if chunk.offset != 0 {
                return Err(ReadError::OffsetMismatch {
                    expected: 0,
                    found: chunk.offset,
                });
            }
            self.index = chunk.index;
            self.total_bytes = chunk.total_bytes;
            self.buffer.clear();
            self.active = true;
            self.skipping = chunk.total_bytes > MAX_AUTO_ITEM_BYTES;
            if !self.skipping {
                self.buffer
                    .try_reserve_exact(chunk.total_bytes)
                    .map_err(|_| ReadError::Allocation)?;
            }
        } else if chunk.total_bytes != self.total_bytes {
            return Err(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset.saturating_add(delivered),
            });
        }
        if chunk.offset != self.next_offset {
            return Err(ReadError::OffsetMismatch {
                expected: self.next_offset,
                found: chunk.offset,
            });
        }
        self.next_offset = self
            .next_offset
            .checked_add(delivered)
            .ok_or(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: usize::MAX,
            })?;
        if self.next_offset > self.total_bytes {
            return Err(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset,
            });
        }
        if !chunk.complete {
            if self.next_offset == self.total_bytes {
                return Err(ReadError::IncompleteAtEnd);
            }
            if !self.skipping {
                self.buffer.push_str(&chunk.data);
            }
            return Ok(ChunkOutcome::Pending);
        }
        if self.next_offset != self.total_bytes {
            return Err(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset,
            });
        }
        if self.skipping {
            let index = self.index;
            let total_bytes = self.total_bytes;
            self.discard();
            return Ok(ChunkOutcome::Oversized { index, total_bytes });
        }
        self.buffer.push_str(&chunk.data);
        let item = EncodedItem {
            index: self.index,
            data: self.buffer.as_str().into(),
        };
        self.discard();
        Ok(ChunkOutcome::Item(item))
    }
}

/// One decoded Runtime history item, plus what the display projection added.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RawHistoryItem {
    pub item: RuntimeItem,
    /// RFC3339; only user items have one, and old history none.
    pub timestamp: Option<String>,
    /// Only display reads carry summaries.
    #[serde(default)]
    pub tool_summaries: Vec<ToolSummary>,
}

impl RawHistoryItem {
    pub fn summary(&self, call_id: &str) -> Option<&ToolSummary> {
        self.tool_summaries
            .iter()
            .find(|summary| summary.tool_call_id == call_id)
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        let optional = |value: &Option<String>| value.as_ref().map_or(0, String::capacity);
        let mut bytes = optional(&self.timestamp).saturating_add(
            self.tool_summaries
                .capacity()
                .saturating_mul(std::mem::size_of::<ToolSummary>()),
        );
        for summary in &self.tool_summaries {
            bytes = bytes
                .saturating_add(summary.tool_call_id.capacity())
                .saturating_add(summary.display.detail.capacity())
                .saturating_add(optional(&summary.display.expanded_input))
                .saturating_add(optional(&summary.state));
        }
        match &self.item {
            RuntimeItem::User(item) => bytes
                .saturating_add(item.loop_id.capacity())
                .saturating_add(item.input.text.capacity()),
            RuntimeItem::Assistant(item) => {
                bytes = bytes
                    .saturating_add(item.loop_id.capacity())
                    .saturating_add(
                        item.content
                            .capacity()
                            .saturating_mul(std::mem::size_of::<RuntimeAssistantPart>()),
                    );
                for part in &item.content {
                    bytes = bytes.saturating_add(match part {
                        RuntimeAssistantPart::Text(text) => text.capacity(),
                        RuntimeAssistantPart::Reasoning { text, summary } => {
                            optional(text).saturating_add(optional(summary))
                        }
                        RuntimeAssistantPart::ToolCall { tool_call_id, name } => {
                            tool_call_id.capacity().saturating_add(name.capacity())
                        }
                    });
                }
                bytes
            }
            RuntimeItem::ToolResult(item) => bytes
                .saturating_add(item.loop_id.capacity())
                .saturating_add(item.call_id.capacity())
                .saturating_add(item.outcome.capacity())
                .saturating_add(
                    item.output
                        .as_ref()
                        .map_or(0, |output| output.content.capacity()),
                ),
            RuntimeItem::Summary(item) => bytes.saturating_add(item.content.capacity()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RuntimeItem {
    User(RuntimeUserItem),
    Assistant(RuntimeAssistantItem),
    ToolResult(RuntimeToolResult),
    Summary(RuntimeSummary),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeUserItem {
    pub loop_id: String,
    pub kind: UserKind,
    pub input: RuntimeUserInput,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeUserInput {
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    Prompt,
    Steering,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeAssistantItem {
    pub loop_id: String,
    pub request_index: u32,
    pub content: Vec<RuntimeAssistantPart>,
}

/// A sanitized assistant part; display reads carry no tool-call arguments.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RuntimeAssistantPart {
    Text(String),
    Reasoning {
        text: Option<String>,
        summary: Option<String>,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
    },
}

impl RuntimeAssistantPart {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Reasoning { text, .. } => text.as_deref(),
            Self::ToolCall { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeToolResult {
    pub loop_id: String,
    pub request_index: u32,
    pub call_id: String,
    pub outcome: String,
    /// Absent in display reads, which never carry raw tool bodies.
    pub output: Option<RuntimeToolOutput>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeToolOutput {
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeSummary {
    pub content: String,
}

pub fn decode_item(raw: &str) -> Result<RawHistoryItem, String> {
    serde_json::from_str(raw).map_err(|error| format!("item JSON: {error}"))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnAvailability {
    Pending,
    Live,
    Stored,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnPersistence {
    Persisted,
    Failed,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TurnResultPage {
    pub turn: TurnRef,
    /// The resident instance this read observed; null for a durable read.
    #[serde(default)]
    pub load: Option<String>,
    /// The immutable identity of the result body, matching every page.
    #[serde(default)]
    pub result_id: Option<String>,
    #[serde(default)]
    pub persistence_uncertain: bool,
    #[serde(default)]
    pub unavailable_reason: Option<super::UnavailableReason>,
    pub availability: TurnAvailability,
    pub items: Vec<ReadChunk>,
    #[serde(deserialize_with = "required_option")]
    pub next_cursor: Option<ReadCursor>,
    pub total: usize,
    /// Absent while the turn is still running, or when it cannot be read.
    pub outcome: Option<super::LoopOutcome>,
    pub persistence: Option<TurnPersistence>,
    pub usage: Option<super::Usage>,
    pub requests: Option<u32>,
    pub tool_rounds: Option<u64>,
    pub interrupted: Option<InterruptedRequest>,
}

impl TurnResultPage {
    /// Whether the page states a readable, complete result body.
    pub fn is_readable(&self) -> bool {
        matches!(
            self.availability,
            TurnAvailability::Live | TurnAvailability::Stored
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(index: usize, offset: usize, total: usize, data: &str, complete: bool) -> ReadChunk {
        ReadChunk {
            index,
            offset,
            total_bytes: total,
            encoding: "utf8_json".into(),
            data: data.into(),
            complete,
        }
    }

    #[test]
    fn assembler_joins_chunks_across_pages() {
        let mut assembler = ChunkAssembler::new();
        assert_eq!(
            assembler
                .push(chunk(4, 0, 11, "{\"item\":", false))
                .unwrap(),
            ChunkOutcome::Pending
        );
        assert_eq!(
            assembler.next_cursor(),
            Some(ReadCursor { item: 4, offset: 8 })
        );
        let ChunkOutcome::Item(item) = assembler.push(chunk(4, 8, 11, "{\"}", true)).unwrap()
        else {
            panic!("expected the assembled item");
        };
        assert_eq!(item.index, 4);
        assert_eq!(&*item.data, "{\"item\":{\"}");
        assert_eq!(assembler.next_cursor(), None);
    }

    #[test]
    fn assembler_rejects_a_discontinuity_without_losing_its_buffer() {
        let mut assembler = ChunkAssembler::new();
        assembler.push(chunk(1, 0, 6, "abc", false)).unwrap();
        assert_eq!(
            assembler.push(chunk(1, 4, 6, "def", true)).unwrap_err(),
            ReadError::OffsetMismatch {
                expected: 3,
                found: 4
            }
        );
        // The buffer is intact, so the correct continuation still completes.
        let ChunkOutcome::Item(item) = assembler.push(chunk(1, 3, 6, "def", true)).unwrap() else {
            panic!("expected item");
        };
        assert_eq!(&*item.data, "abcdef");
    }

    #[test]
    fn oversized_items_are_reported_rather_than_buffered() {
        let mut assembler = ChunkAssembler::new();
        let total = MAX_AUTO_ITEM_BYTES + 1;
        let first = "x".repeat(MAX_AUTO_ITEM_BYTES);
        let outcome = assembler.push(chunk(2, 0, total, &first, false)).unwrap();
        assert_eq!(outcome, ChunkOutcome::Pending);
        let outcome = assembler
            .push(chunk(2, MAX_AUTO_ITEM_BYTES, total, "x", true))
            .unwrap();
        assert!(matches!(
            outcome,
            ChunkOutcome::Oversized {
                index: 2,
                total_bytes
            } if total_bytes == total
        ));
    }

    #[test]
    fn an_oversized_item_cannot_finish_before_its_declared_bytes() {
        let mut assembler = ChunkAssembler::new();
        let total = MAX_AUTO_ITEM_BYTES + 1;
        assert_eq!(
            assembler.push(chunk(2, 0, total, "x", true)).unwrap_err(),
            ReadError::ByteCount {
                index: 2,
                declared: total,
                delivered: 1
            }
        );
    }

    #[test]
    fn display_items_decode_parts_and_tool_summaries() {
        let raw = json!({
            "item": {"type": "assistant", "data": {
                "loop_id": "l", "request_index": 1, "model": "m",
                "content": [
                    {"type": "reasoning", "data": {"text": "thinking"}},
                    {"type": "text", "data": "answer"},
                    {"type": "tool_call", "data": {"tool_call_id": "c", "name": "read",
                                                   "arguments": {"path": "a.rs"}, "call_index": 0}}
                ],
                "finish_reason": "stop"
            }},
            "display": true,
            "tool_summaries": [{
                "tool_ref": {"session_id": "s", "loop_id": "l", "request_index": 1, "tool_call_id": "c"},
                "tool_call_id": "c", "name": "read",
                "display": {"detail": "a.rs", "truncated": false, "body_truncated": false},
                "output_line_count": 3, "output_truncated": false, "count_state": "exact",
                "state": "succeeded", "input_availability": null, "output_availability": null
            }]
        })
        .to_string();
        let decoded = decode_item(&raw).unwrap();
        let RuntimeItem::Assistant(assistant) = &decoded.item else {
            panic!("expected assistant");
        };
        assert_eq!(assistant.content.len(), 3);
        assert_eq!(assistant.content[1].text(), Some("answer"));
        assert_eq!(decoded.summary("c").unwrap().display.detail, "a.rs");
    }

    #[test]
    fn user_items_read_input_text_and_kind() {
        let raw = json!({"item": {"type": "user", "data": {
            "loop_id": "l", "kind": "steering", "input": {"text": "focus on tests"}
        }}})
        .to_string();
        let RuntimeItem::User(user) = decode_item(&raw).unwrap().item else {
            panic!("expected user");
        };
        assert_eq!(user.input.text, "focus on tests");
        assert_eq!(user.kind, UserKind::Steering);
    }

    #[test]
    fn the_envelope_timestamp_is_read_when_present() {
        let item = json!({"type": "user", "data": {
            "loop_id": "l", "kind": "prompt", "input": {"text": "hi"}
        }});
        let stamped = json!({"item": item, "timestamp": "2026-10-20T21:38:00Z"}).to_string();
        let plain = json!({"item": item, "timestamp": null}).to_string();
        assert_eq!(
            decode_item(&stamped).unwrap().timestamp.as_deref(),
            Some("2026-10-20T21:38:00Z")
        );
        assert_eq!(decode_item(&plain).unwrap().timestamp, None);
    }
}
