//! `session.read` / `turn.result` DTOs and the chunk assembler.

use serde::Deserialize;
use serde_json::Value;

use super::{LoopOutcome, SessionInfo, ToolSummary, TurnPersistence, TurnRef, Usage};

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
    #[serde(default)]
    pub encoding: String,
    pub data: String,
    pub complete: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReadSessionResult {
    pub session: SessionInfo,
    #[serde(default)]
    pub items: Vec<ReadChunk>,
    #[serde(default)]
    pub next_cursor: Option<Value>,
    pub total: usize,
    #[serde(default)]
    pub records: Vec<TurnRecord>,
    #[serde(default)]
    pub history_revision: String,
    #[serde(default)]
    pub captured_end: u64,
    #[serde(default)]
    pub projection: Option<DisplayProjection>,
}

#[derive(Clone, Debug)]
pub struct TurnRecord {
    pub loop_id: String,
    pub outcome: LoopOutcome,
    pub usage: Usage,
    pub requests: u32,
    pub tool_rounds: u64,
    pub completed_at: Option<String>,
}

impl<'de> Deserialize<'de> for TurnRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            loop_id: String,
            outcome: LoopOutcome,
            #[serde(default)]
            usage: Usage,
            #[serde(default)]
            requests: u32,
            #[serde(default)]
            tool_rounds: u64,
            #[serde(default)]
            completed_at: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            loop_id: wire.loop_id,
            outcome: wire.outcome,
            usage: wire.usage,
            requests: wire.requests,
            tool_rounds: wire.tool_rounds,
            completed_at: wire.completed_at,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct DisplayProjection {
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub first_item: usize,
    #[serde(default)]
    pub covered_item_count: usize,
    #[serde(default)]
    pub covered_usage: Option<CoveredUsage>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CoveredUsage {
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub loop_count: usize,
    #[serde(default)]
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
}

/// A cursor is opaque: shape and meaning belong to the Agent.
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

    /// Reads a cursor the Agent handed back. Unreadable cursors mean the
    /// response cannot be continued, which the caller surfaces as an error.
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

/// Reassembles one item at a time from `session.read`/`turn.result` chunks.
/// A single item may span pages, so the buffer outlives a page.
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

    pub fn discard(&mut self) {
        *self = Self::default();
    }

    pub fn push(&mut self, chunk: ReadChunk) -> Result<ChunkOutcome, ReadError> {
        if !chunk.encoding.is_empty() && chunk.encoding != "utf8_json" {
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
#[derive(Clone, Debug, PartialEq)]
pub struct RawHistoryItem {
    pub item: RuntimeItem,
    pub timestamp: Option<String>,
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

impl RuntimeItem {
    pub fn loop_id(&self) -> Option<&str> {
        match self {
            Self::User(item) => Some(&item.loop_id),
            Self::Assistant(item) => Some(&item.loop_id),
            Self::ToolResult(item) => Some(&item.loop_id),
            Self::Summary(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeUserItem {
    #[serde(default)]
    pub loop_id: String,
    #[serde(default)]
    pub kind: UserKind,
    pub input: RuntimeUserInput,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeUserInput {
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    #[default]
    Prompt,
    Steering,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeAssistantItem {
    #[serde(default)]
    pub loop_id: String,
    #[serde(default)]
    pub request_index: u32,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub content: Vec<RuntimeAssistantPart>,
    #[serde(default)]
    pub finish_reason: String,
    #[serde(default)]
    pub usage: Usage,
}

/// A sanitized assistant part. Reasoning is a nested object on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeAssistantPart {
    Text(String),
    Reasoning {
        text: Option<String>,
        summary: Option<String>,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        arguments: Value,
        call_index: u32,
    },
}

impl<'de> Deserialize<'de> for RuntimeAssistantPart {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        enum Wire {
            Text(String),
            Reasoning {
                #[serde(default)]
                text: Option<String>,
                #[serde(default)]
                summary: Option<String>,
            },
            ToolCall {
                tool_call_id: String,
                #[serde(default)]
                name: String,
                #[serde(default)]
                arguments: Value,
                #[serde(default)]
                call_index: u32,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Text(text) => Self::Text(text),
            Wire::Reasoning { text, summary } => Self::Reasoning { text, summary },
            Wire::ToolCall {
                tool_call_id,
                name,
                arguments,
                call_index,
            } => Self::ToolCall {
                tool_call_id,
                name,
                arguments,
                call_index,
            },
        })
    }
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
    #[serde(default)]
    pub loop_id: String,
    #[serde(default)]
    pub request_index: u32,
    #[serde(default)]
    pub call_id: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub output: Option<RuntimeToolOutput>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeToolOutput {
    #[serde(default)]
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeSummary {
    #[serde(default)]
    pub content: String,
}

#[derive(Deserialize)]
struct ItemEnvelope {
    item: RuntimeItem,
    #[serde(default)]
    tool_summaries: Vec<ToolSummary>,
    #[serde(default)]
    timestamp: Option<String>,
}

pub fn decode_item(raw: &str) -> Result<RawHistoryItem, String> {
    let envelope: ItemEnvelope =
        serde_json::from_str(raw).map_err(|error| format!("item JSON: {error}"))?;
    Ok(RawHistoryItem {
        item: envelope.item,
        timestamp: envelope.timestamp,
        tool_summaries: envelope.tool_summaries,
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct TurnResultPage {
    pub turn: TurnRef,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub outcome: Option<LoopOutcome>,
    #[serde(default)]
    pub persistence: Option<TurnPersistence>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub requests: Option<u32>,
    #[serde(default)]
    pub tool_rounds: Option<u64>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub items: Vec<ReadChunk>,
    #[serde(default)]
    pub next_cursor: Option<Value>,
    #[serde(default)]
    pub total: usize,
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
                "display": {"detail": "a.rs", "truncated": false}
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
}
