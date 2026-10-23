//! `session.read` / `turn.result` DTOs and the chunk assembler.

use serde::Deserialize;
use serde_json::Value;

use super::{LoopOutcome, ToolSummary, Usage};

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
    pub items: Vec<ReadChunk>,
    pub next_cursor: Option<Value>,
    pub total: usize,
    pub history_revision: String,
    pub captured_end: u64,
    pub projection: Option<DisplayProjection>,
    /// Summaries of the turns that have an item in this page.
    #[serde(default)]
    pub records: Vec<TurnRecord>,
}

/// How a stored turn ended.
#[derive(Clone, Debug, Deserialize)]
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
#[derive(Clone, Debug, Deserialize)]
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
    pub covered_usage: Option<CoveredUsage>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
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
                .and_then(|projection| projection.covered_usage.clone()),
        }
    }
}

/// The read prefix a continuation is pinned to. Never cached across
/// operations: every read starts from a fresh probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotPin {
    pub captured_end: u64,
    pub history_revision: String,
    pub projection_revision: Option<String>,
    pub total: usize,
    pub first_item: usize,
    pub covered_usage: Option<CoveredUsage>,
}

/// A read position: an item index and a byte offset inside that item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    /// Reads a cursor the Agent handed back; `None` means the read cannot go on.
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            item: value.get("item")?.as_u64()? as usize,
            offset: value.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
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
    #[error("agent cursor {0} is not a read cursor")]
    Cursor(Value),
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
        } else if chunk.total_bytes != self.total_bytes {
            return Err(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset + delivered,
            });
        }
        if chunk.offset != self.next_offset {
            return Err(ReadError::OffsetMismatch {
                expected: self.next_offset,
                found: chunk.offset,
            });
        }
        self.next_offset += delivered;
        if self.next_offset > self.total_bytes {
            return Err(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset,
            });
        }
        if self.skipping {
            if !chunk.complete {
                return Ok(ChunkOutcome::Pending);
            }
            let index = self.index;
            let total_bytes = self.total_bytes;
            self.discard();
            return Ok(ChunkOutcome::Oversized { index, total_bytes });
        }
        self.buffer.push_str(&chunk.data);
        if !chunk.complete {
            return Ok(ChunkOutcome::Pending);
        }
        if self.next_offset != self.total_bytes {
            return Err(ReadError::ByteCount {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset,
            });
        }
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

#[derive(Clone, Debug, Deserialize)]
pub struct TurnResultPage {
    pub items: Vec<ReadChunk>,
    pub next_cursor: Option<Value>,
    /// Absent while the turn is still running.
    pub outcome: Option<super::LoopOutcome>,
    pub usage: Option<super::Usage>,
    pub requests: Option<u32>,
    pub tool_rounds: Option<u64>,
    pub interrupted: Option<InterruptedRequest>,
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
        let outcome = assembler.push(chunk(2, 0, total, "", false)).unwrap();
        assert_eq!(outcome, ChunkOutcome::Pending);
        let outcome = assembler.push(chunk(2, 0, total, "", true)).unwrap();
        assert!(matches!(
            outcome,
            ChunkOutcome::Oversized {
                index: 2,
                total_bytes
            } if total_bytes == total
        ));
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
