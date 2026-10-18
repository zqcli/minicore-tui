//! The transcript: block data model plus its layout cache.

pub mod block;
pub mod layout;

pub use block::{Block, ChangeSummary, NoticeLevel, RequestKey, ToolCall, ToolState, Transcript};
