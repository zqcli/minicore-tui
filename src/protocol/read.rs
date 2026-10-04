//! Authoritative Protocol v1 read DTOs and the chunk decoder (spec §6).
//!
//! These types are the *raw* Runtime item envelopes returned by `session.read`
//! and `turn.result`. They are deliberately **different types** from the
//! legacy `session.history` display DTO ([`crate::protocol::HistoryItemViewWire`])
//! because the two wire shapes differ: a runtime `User` item carries `input`,
//! an `Assistant` item carries `content`, and tool output is a nested object.
//! Stage B is the only main-history migration; the legacy DTO remains only for
//! compatibility fixtures/diagnostics and is not used by the app read path.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};

use super::{LoopOutcomeWire, SessionInfo, UsageWire};

/// Default auto-decode ceiling for a single raw item (spec §6.2). A larger
/// item is surfaced as a [`Assembled::LargeItem`] placeholder rather than being
/// silently truncated and reported complete.
pub const MAX_AUTO_ITEM_BYTES: usize = 8 * 1024 * 1024;

/// A read position: `item` is the session-global item index and `offset` is
/// the UTF-8 byte offset inside that item's canonical JSON encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadCursor {
    pub item: usize,
    #[serde(default)]
    pub offset: usize,
}

impl ReadCursor {
    pub const fn start() -> Self {
        Self { item: 0, offset: 0 }
    }
}

/// The fixed snapshot prefix a read chain is pinned to (spec §6.1). `total` is
/// the number of *readable items* in the prefix; `captured_end` is the backend
/// JSONL byte boundary and is never computed client-side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotPin {
    pub captured_end: u64,
    pub history_revision: String,
    pub total: usize,
    pub projection: Option<DisplayProjection>,
}

impl SnapshotPin {
    pub fn validate(&self) -> Result<(), ReadError> {
        if self.history_revision.len() != 64
            || !self
                .history_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ReadError::InvalidPin {
                detail: "history_revision is not a 64-character hexadecimal digest".to_owned(),
            });
        }
        if let Some(projection) = &self.projection {
            if projection.revision.len() != 64
                || !projection.revision.bytes().all(|b| b.is_ascii_hexdigit())
                || projection.first_item > self.total
                || projection.covered_item_count > self.total
            {
                return Err(ReadError::InvalidPin {
                    detail: "invalid display projection identity".into(),
                });
            }
        }
        Ok(())
    }
}

/// One returned chunk of one item's canonical JSON (spec §6.1).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReadChunk {
    pub index: usize,
    pub offset: usize,
    pub total_bytes: usize,
    pub encoding: String,
    pub data: String,
    pub complete: bool,
}

/// The result envelope of one `session.read` page.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReadSessionResult {
    pub session: SessionInfo,
    pub items: Vec<ReadChunk>,
    #[serde(default)]
    pub next_cursor: Option<ReadCursor>,
    pub total: usize,
    #[serde(default)]
    pub records: Vec<ReadTurnSummary>,
    #[serde(default)]
    pub records_truncated: bool,
    pub history_revision: String,
    pub captured_end: u64,
    #[serde(default)]
    pub trailing_incomplete: bool,
    #[serde(default)]
    pub projection: Option<DisplayProjection>,
}

impl ReadSessionResult {
    pub fn pin(&self) -> SnapshotPin {
        SnapshotPin {
            captured_end: self.captured_end,
            history_revision: self.history_revision.clone(),
            total: self.total,
            projection: self.projection.clone(),
        }
    }
}

/// A sanitized turn summary attached to a read page. `records` only covers the
/// turns intersecting the returned chunks; it never implies all turns were
/// read (spec §6.1).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReadTurnSummary {
    pub loop_id: String,
    pub outcome: LoopOutcomeWire,
    #[serde(default)]
    pub usage: UsageWire,
    pub requests: u32,
    pub tool_rounds: u64,
    pub final_config_revision: u64,
    pub completed_at: String,
}

/// Identity and accounting of one explicitly selected display read chain.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DisplayProjection {
    pub revision: String,
    pub first_item: usize,
    pub covered_item_count: usize,
    pub covered_usage: CoveredUsage,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CoveredUsage {
    pub usage: UsageWire,
    pub loop_count: usize,
    pub last_loop_id: Option<String>,
    pub partial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCountState {
    Exact,
    LowerBound,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ToolSummary {
    pub tool_ref: super::ToolRefWire,
    pub tool_call_id: String,
    pub name: String,
    pub display: super::ToolDisplayWire,
    pub output_line_count: Option<usize>,
    pub output_truncated: bool,
    pub count_state: ToolCountState,
}

/// One complete canonical item body emitted by [`ChunkAssembler`]. The
/// assembler never deserializes item JSON on the App thread; the shared string
/// is submitted to the single bounded decode worker and is dropped after that
/// hand-off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedHistoryItem {
    pub index: usize,
    pub data: Arc<str>,
}

impl EncodedHistoryItem {
    pub fn bytes(&self) -> usize {
        self.data.len()
    }
}

/// One decoded raw Runtime `HistoryItem` plus its optional acceptance time.
#[derive(Clone, Debug, PartialEq)]
pub struct RawHistoryItem {
    pub item: RuntimeItem,
    pub timestamp: Option<String>,
    pub tool_summaries: Vec<ToolSummary>,
}

impl RawHistoryItem {
    pub fn tool_display(&self, call_id: &str) -> Option<&super::ToolDisplayWire> {
        self.tool_summaries
            .iter()
            .find(|summary| summary.tool_call_id == call_id)
            .map(|summary| &summary.display)
    }
}

/// A sanitized Runtime `HistoryItem`, decoded from its canonical JSON.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RuntimeItem {
    User(RuntimeUserItem),
    Assistant(RuntimeAssistantItem),
    ToolResult(RuntimeToolResultItem),
    Summary(RuntimeSummaryItem),
}

impl RuntimeItem {
    /// The loop this item belongs to, when it belongs to one.
    pub fn loop_id(&self) -> Option<&str> {
        match self {
            Self::User(item) => Some(&item.loop_id),
            Self::Assistant(item) => Some(&item.loop_id),
            Self::ToolResult(item) => Some(&item.loop_id),
            Self::Summary(_) => None,
        }
    }
}

/// Runtime `UserHistory`: the input is `input.text`, not the legacy `text`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeUserItem {
    pub loop_id: String,
    pub kind: RuntimeUserKind,
    pub input: RuntimeUserInput,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeUserInput {
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeUserKind {
    Prompt,
    Steering,
}

/// Runtime `AssistantHistory`: ordered `content` parts, not legacy
/// `text`/`reasoning` aggregate strings.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeAssistantItem {
    pub loop_id: String,
    pub request_index: u32,
    pub model: String,
    #[serde(default)]
    pub reasoning: Option<String>,
    pub content: Vec<RuntimeAssistantPart>,
    pub finish_reason: String,
    #[serde(default)]
    pub usage: UsageWire,
}

/// A sanitized Runtime `AssistantPart`. Reasoning text is a nested object;
/// `encrypted`/`signature` opaque fields are retained as opaque strings.
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeAssistantPart {
    Text(String),
    Reasoning {
        text: Option<String>,
        summary: Option<String>,
        encrypted: Option<String>,
        signature: Option<String>,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        arguments: serde_json::Value,
        call_index: u32,
    },
}

impl<'de> Deserialize<'de> for RuntimeAssistantPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "data", rename_all = "snake_case")]
        enum Wire {
            Text(String),
            Reasoning {
                #[serde(default)]
                text: Option<String>,
                #[serde(default)]
                summary: Option<String>,
                #[serde(default)]
                encrypted: Option<String>,
                #[serde(default)]
                signature: Option<String>,
            },
            ToolCall {
                tool_call_id: String,
                name: String,
                #[serde(default)]
                arguments: serde_json::Value,
                #[serde(default)]
                call_index: u32,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Text(text) => Self::Text(text),
            Wire::Reasoning {
                text,
                summary,
                encrypted,
                signature,
            } => Self::Reasoning {
                text,
                summary,
                encrypted,
                signature,
            },
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
    /// Bytes this part contributes to the display/body budget. The visible
    /// text is charged once; opaque fields are not retained.
    pub fn visible_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Reasoning { text, summary, .. } => {
                text.as_ref().map_or(0, String::len) + summary.as_ref().map_or(0, String::len)
            }
            Self::ToolCall { arguments, .. } => arguments.to_string().len(),
        }
    }
}

/// A sanitized Runtime `ToolResultHistory`: the call id field is `call_id` and
/// the text is nested as `output.content`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeToolResultItem {
    pub loop_id: String,
    pub request_index: u32,
    pub call_id: String,
    pub tool_name: String,
    pub outcome: String,
    pub output: Option<RuntimeToolOutput>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeToolOutput {
    pub content: String,
}

/// Runtime `SummaryHistory`: the body is nested at `content.content`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeSummaryItem {
    pub content: String,
}

/// The outcome of feeding one chunk to the assembler. `EncodedItem` is
/// deliberately separate from a decoded Runtime value: the App loop only
/// validates/chunks and the bounded worker owns JSON deserialization.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Assembled {
    /// The item is still incomplete; feed the next chunk.
    Pending,
    /// One complete, contiguous canonical item is ready for the decode worker.
    EncodedItem { item: EncodedHistoryItem },
    /// The item exceeds [`MAX_AUTO_ITEM_BYTES`]. Its bytes were discarded; the
    /// caller shows a visible placeholder and can re-read it on demand. It is
    /// never reported as a complete item.
    LargeItem { index: usize, total_bytes: usize },
    /// The first chunk of an oversized item. The placeholder is visible now,
    /// while the assembler remains positioned inside the item for an explicit
    /// continuation read.
    LargeItemPending { index: usize, total_bytes: usize },
}

/// `turn.result` availability: a turn may still be running, may only exist as
/// the in-process retained report, or may be stored (spec §7.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnAvailability {
    Pending,
    Live,
    Stored,
}

/// One `turn.result` page (spec §7.2). `index` on items is **turn-local**, not
/// a session-global history index.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TurnResultPage {
    pub turn: super::TurnRef,
    pub availability: TurnAvailability,
    #[serde(default)]
    pub outcome: Option<LoopOutcomeWire>,
    #[serde(default)]
    pub persistence: Option<super::TurnPersistenceWire>,
    #[serde(default)]
    pub usage: Option<UsageWire>,
    #[serde(default)]
    pub requests: Option<u32>,
    #[serde(default)]
    pub tool_rounds: Option<u64>,
    #[serde(default)]
    pub final_config_revision: Option<u64>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub items: Vec<ReadChunk>,
    #[serde(default)]
    pub next_cursor: Option<ReadCursor>,
    #[serde(default)]
    pub total: usize,
}

/// Reasons a chunk stream is not a well-formed canonical item.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReadError {
    #[error("unexpected read encoding '{0}', expected utf8_json")]
    Encoding(String),
    #[error("read chunk index {found} does not follow item {expected}")]
    IndexOutOfOrder { expected: usize, found: usize },
    #[error("read chunk offset {found} does not continue at {expected}")]
    OffsetMismatch { expected: usize, found: usize },
    #[error("read chunk total_bytes {found} disagrees with {expected}")]
    TotalBytesMismatch { expected: usize, found: usize },
    #[error("item {index} declared {declared} bytes but delivered {delivered}")]
    ByteCountMismatch {
        index: usize,
        declared: usize,
        delivered: usize,
    },
    #[error("item {index} JSON is not a Runtime history item: {detail}")]
    MalformedItem { index: usize, detail: String },
    #[error("page is not contiguous at item {expected}, found {found}")]
    NonContiguous { expected: usize, found: usize },
    #[error("page did not advance from cursor item {item}")]
    CursorStalled { item: usize },
    #[error("item {index} changed after it was already loaded")]
    ItemChanged { index: usize },
    #[error("invalid history snapshot pin: {detail}")]
    InvalidPin { detail: String },
    #[error("page pin no longer matches the captured prefix")]
    PinMismatch,
    #[error("history total changed from {expected} to {found} within one pinned chain")]
    TotalChanged { expected: usize, found: usize },
    #[error("turn result total changed from {expected} to {found}")]
    TurnTotalMismatch { expected: usize, found: usize },
    #[error("turn result cursor offset {found} does not continue at {expected}")]
    CursorOffsetMismatch { expected: usize, found: usize },
}

/// Reassembles raw item chunks into one canonical encoded item (spec §6.2).
/// It buffers at most one incomplete item, advances only by real
/// `data.as_bytes().len()`, and never preallocates from the declared
/// `total_bytes`. JSON deserialization happens in the owned decode worker.
#[derive(Debug, Default)]
pub struct ChunkAssembler {
    index: usize,
    next_offset: usize,
    total_bytes: usize,
    buffer: String,
    active: bool,
    skipping_large: bool,
}

impl ChunkAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// The index of the item currently being assembled, if any.
    pub fn current_index(&self) -> Option<usize> {
        self.active.then_some(self.index)
    }

    /// The next cursor accepted by this assembler for a partial item.
    pub fn next_cursor(&self) -> Option<ReadCursor> {
        self.active.then_some(ReadCursor {
            item: self.index,
            offset: self.next_offset,
        })
    }

    /// Drops the buffered partial item. Used when a page lands outside the
    /// requested window, so its partial bytes are never mistaken for loaded
    /// content (spec §6.3).
    pub fn discard(&mut self) {
        self.index = 0;
        self.next_offset = 0;
        self.total_bytes = 0;
        self.buffer.clear();
        self.active = false;
        self.skipping_large = false;
    }

    pub fn push(&mut self, chunk: ReadChunk) -> Result<Assembled, ReadError> {
        if chunk.encoding != "utf8_json" {
            return Err(ReadError::Encoding(chunk.encoding));
        }
        // `String::len` is the UTF-8 byte length, matching the backend's
        // cursor arithmetic (spec §6.2); never the char count.
        let delivered = chunk.data.len();

        if self.active && chunk.index != self.index {
            // `discard` runs as soon as a complete item is emitted. An active
            // assembler therefore always owns an incomplete item; accepting a
            // new index here would silently drop its prefix.
            return Err(ReadError::IndexOutOfOrder {
                expected: self.index,
                found: chunk.index,
            });
        } else if !self.active {
            if chunk.offset != 0 {
                return Err(ReadError::OffsetMismatch {
                    expected: 0,
                    found: chunk.offset,
                });
            }
            self.begin(chunk.index, chunk.total_bytes);
        } else if chunk.total_bytes != self.total_bytes {
            return Err(ReadError::TotalBytesMismatch {
                expected: self.total_bytes,
                found: chunk.total_bytes,
            });
        }

        if chunk.offset != self.next_offset {
            return Err(ReadError::OffsetMismatch {
                expected: self.next_offset,
                found: chunk.offset,
            });
        }

        if self.skipping_large {
            self.next_offset = self.next_offset.saturating_add(delivered);
            if self.next_offset > self.total_bytes {
                return Err(ReadError::ByteCountMismatch {
                    index: self.index,
                    declared: self.total_bytes,
                    delivered: self.next_offset,
                });
            }
            if chunk.complete {
                if self.next_offset != self.total_bytes {
                    return Err(ReadError::ByteCountMismatch {
                        index: self.index,
                        declared: self.total_bytes,
                        delivered: self.next_offset,
                    });
                }
                let total = self.total_bytes;
                let index = self.index;
                self.discard();
                return Ok(Assembled::LargeItem {
                    index,
                    total_bytes: total,
                });
            }
            return Ok(Assembled::Pending);
        }

        if self.total_bytes > MAX_AUTO_ITEM_BYTES {
            self.skipping_large = true;
            self.next_offset = self.next_offset.saturating_add(delivered);
            if !chunk.complete {
                return Ok(Assembled::LargeItemPending {
                    index: self.index,
                    total_bytes: self.total_bytes,
                });
            }
            if self.next_offset > self.total_bytes {
                return Err(ReadError::ByteCountMismatch {
                    index: self.index,
                    declared: self.total_bytes,
                    delivered: self.next_offset,
                });
            }
            if chunk.complete {
                if self.next_offset != self.total_bytes {
                    return Err(ReadError::ByteCountMismatch {
                        index: self.index,
                        declared: self.total_bytes,
                        delivered: self.next_offset,
                    });
                }
                let total = self.total_bytes;
                let index = self.index;
                self.discard();
                return Ok(Assembled::LargeItem {
                    index,
                    total_bytes: total,
                });
            }
            return Ok(Assembled::Pending);
        }

        self.buffer.push_str(&chunk.data);
        self.next_offset = self.next_offset.saturating_add(delivered);
        if self.next_offset > self.total_bytes {
            return Err(ReadError::ByteCountMismatch {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset,
            });
        }

        if !chunk.complete {
            return Ok(Assembled::Pending);
        }
        if self.next_offset != self.total_bytes {
            return Err(ReadError::ByteCountMismatch {
                index: self.index,
                declared: self.total_bytes,
                delivered: self.next_offset,
            });
        }
        let item = EncodedHistoryItem {
            index: self.index,
            data: Arc::<str>::from(self.buffer.as_str()),
        };
        self.discard();
        Ok(Assembled::EncodedItem { item })
    }

    fn begin(&mut self, index: usize, total_bytes: usize) {
        self.index = index;
        self.next_offset = 0;
        self.total_bytes = total_bytes;
        self.buffer.clear();
        self.active = true;
        self.skipping_large = false;
    }
}

/// A decoded raw item is exactly `{ "item": <RuntimeItem>, "timestamp": ? }`.
/// The `data` field already went through outer JSON decoding, so it must not
/// be unescaped again.
#[derive(Deserialize)]
struct Envelope {
    item: RuntimeItem,
    #[serde(default)]
    display: bool,
    #[serde(default)]
    tool_summaries: Vec<ToolSummary>,
    #[serde(default)]
    timestamp: Option<String>,
}

/// Decodes one complete item body. This function is called by the owned decode
/// worker in production; synchronous callers are limited to explicit fixture /
/// compatibility paths.
pub fn decode_item(raw: &str) -> Result<RawHistoryItem, String> {
    let envelope: Envelope = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    for summary in &envelope.tool_summaries {
        let matches = match &envelope.item {
            RuntimeItem::Assistant(assistant) => {
                summary.tool_ref.loop_id == assistant.loop_id
                    && summary.tool_ref.request_index == assistant.request_index
                    && assistant.content.iter().any(|part| {
                        matches!(part, RuntimeAssistantPart::ToolCall { tool_call_id, name, .. }
                    if tool_call_id == &summary.tool_call_id && name == &summary.name)
                    })
            }
            RuntimeItem::ToolResult(result) => {
                summary.tool_ref.loop_id == result.loop_id
                    && summary.tool_ref.request_index == result.request_index
                    && summary.tool_call_id == result.call_id
                    && summary.name == result.tool_name
            }
            _ => false,
        };
        if !matches
            || summary.tool_ref.tool_call_id != summary.tool_call_id
            || summary.display.expanded_input.is_some()
        {
            return Err("display tool summary identity or body mismatch".into());
        }
    }
    if let RuntimeItem::ToolResult(result) = &envelope.item {
        if result.output.is_none()
            && (!envelope.display
                || !envelope.tool_summaries.iter().any(|s| {
                    s.tool_ref.loop_id == result.loop_id
                        && s.tool_ref.request_index == result.request_index
                        && s.tool_ref.tool_call_id == result.call_id
                        && s.name == result.tool_name
                }))
        {
            return Err("tool result body missing outside display projection".into());
        }
    }
    if !envelope.display && !envelope.tool_summaries.is_empty() {
        return Err("tool summaries require display projection".into());
    }
    Ok(RawHistoryItem {
        item: envelope.item,
        timestamp: envelope.timestamp,
        tool_summaries: envelope.tool_summaries,
    })
}

#[cfg(test)]
mod display_projection_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_result_body_requires_matching_explicit_display_summary() {
        let mut envelope = json!({"item":{"type":"tool_result","data":{
            "loop_id":"l", "request_index":0, "call_id":"c", "tool_name":"write", "outcome":"success"
        }}});
        assert!(decode_item(&envelope.to_string()).is_err());
        envelope["display"] = json!(true);
        assert!(decode_item(&envelope.to_string()).is_err());
        envelope["tool_summaries"] = json!([{
            "tool_ref":{"session_id":"s","loop_id":"l","request_index":0,"tool_call_id":"c"},
            "tool_call_id":"c", "name":"write", "display":{"detail":"a.rs","input_line_count":57},
            "output_line_count":1,"output_truncated":false,"count_state":"exact"
        }]);
        let decoded = decode_item(&envelope.to_string()).unwrap();
        let RuntimeItem::ToolResult(result) = decoded.item else {
            panic!("result")
        };
        assert!(
            result.output.is_none(),
            "summary is never an empty/full result owner"
        );
        envelope["tool_summaries"][0]["tool_ref"]["loop_id"] = json!("another");
        assert!(decode_item(&envelope.to_string()).is_err());
    }

    #[test]
    fn display_request_binds_projection_while_raw_export_remains_raw() {
        let pin = SnapshotPin {
            captured_end: 100,
            history_revision: "a".repeat(64),
            total: 4,
            projection: Some(DisplayProjection {
                revision: "b".repeat(64),
                first_item: 1,
                covered_item_count: 2,
                covered_usage: CoveredUsage {
                    usage: UsageWire::default(),
                    loop_count: 1,
                    last_loop_id: Some("l".into()),
                    partial: true,
                },
            }),
        };
        let display = crate::protocol::OutgoingRequest::session_display_read(
            crate::protocol::RequestId(1),
            "s",
            Some(ReadCursor { item: 2, offset: 0 }),
            10,
            4096,
            Some(&pin),
        );
        assert_eq!(display.params["view"], "display");
        assert_eq!(display.params["projection_revision"], "b".repeat(64));
        let raw = crate::protocol::OutgoingRequest::session_read(
            crate::protocol::RequestId(2),
            "s",
            None,
            10,
            4096,
            None,
        );
        assert!(raw.params.get("view").is_none());
        assert!(raw.params.get("projection_revision").is_none());
    }
}
