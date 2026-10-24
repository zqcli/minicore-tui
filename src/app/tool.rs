//! Reads that hydrate tool cards.

use serde_json::json;

use super::{App, SessionOp};
use crate::protocol::{self, RpcError, ToolReadResult, ToolRef};

impl App {
    /// `tool.read` with the display facts of the card `key`; `wrap` turns the
    /// answer into the op to post.
    pub(super) fn read_tool(
        &self,
        key: &ToolRef,
        wrap: impl FnOnce(Result<ToolReadResult, RpcError>) -> SessionOp + Send + 'static,
    ) {
        let fields = json!({
            "loop_id": key.loop_id, "request_index": key.request_index,
            "tool_call_id": key.tool_call_id, "display": true,
        });
        self.call(protocol::METHOD_TOOL_READ, fields, wrap);
    }
}
