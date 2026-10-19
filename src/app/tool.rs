//! The tool output view: its state and the reads that fill it.

use serde_json::json;

use super::tasks::StreamText;
use super::{App, Overlay, Panel, Scroll, SessionOp, tasks};
use crate::protocol::{self, RpcError, ToolReadResult, ToolRef};
use crate::transcript::{Block, NoticeLevel};
use crate::ui::views::{self, ToolRow};

/// The open tool view. Its header and input are read from the card when drawn;
/// the output starts as the card's tail until the full streams arrive.
#[derive(Debug)]
pub struct ToolState {
    pub key: ToolRef,
    pub rows: Vec<ToolRow>,
    /// Full output per stream, `None` while loading; bash's two streams have no
    /// total order, so they stay separate sections.
    streams: Vec<(String, Option<StreamText>)>,
    pub scroll: usize,
    pub status: String,
}

impl ToolState {
    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.rows.len().saturating_sub(rows);
        self.scroll = (self.scroll as isize)
            .saturating_add(delta)
            .clamp(0, max as isize) as usize;
    }

    /// Records one stream's full text. The output is rebuilt once every stream
    /// has answered, so the tail is not replaced by half of the result.
    pub fn set_stream(&mut self, name: &str, text: StreamText) {
        let Some(slot) = self.streams.iter_mut().find(|(stream, _)| stream == name) else {
            return;
        };
        slot.1 = Some(text);
        if self.streams.iter().all(|(_, text)| text.is_some()) {
            self.rows = views::stream_rows(&self.streams);
            self.status.clear();
            self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
        }
    }
}

impl App {
    /// The view of one tool call, and the reads that fill it: the card refreshed
    /// by `tool.read` and the full output of each stream.
    pub(super) fn open_tool(&self, key: ToolRef) -> Option<ToolState> {
        let call = self.session.as_ref()?.transcript.find_tool(&key)?;
        let streams: &[&str] = if call.name == "bash" {
            &["stdout", "stderr"]
        } else {
            &["output"]
        };
        let read_key = key.clone();
        self.read_tool(&key, move |result| SessionOp::ToolRead(read_key, result));
        for stream in streams {
            let (client, key, stream) = (self.client.clone(), key.clone(), (*stream).to_owned());
            self.spawn(async move {
                let output = tasks::read_output(client, key.clone(), stream.clone()).await;
                SessionOp::ToolOutput(key, stream, output)
            });
        }
        Some(ToolState {
            rows: views::text_rows(&call.output.join("\n")),
            streams: streams
                .iter()
                .map(|stream| ((*stream).to_owned(), None))
                .collect(),
            status: "loading the full output…".into(),
            scroll: 0,
            key,
        })
    }

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

    /// `[` / `]` in the tool view: the previous or next tool card.
    pub fn step_tool(&mut self, delta: isize) {
        let Overlay::Tool(tool) = &self.overlay else {
            return;
        };
        let keys: Vec<&ToolRef> = self
            .session
            .iter()
            .flat_map(|session| session.transcript.tool_calls())
            .map(|call| &call.key)
            .collect();
        let Some(index) = keys.iter().position(|key| **key == tool.key) else {
            return;
        };
        let next = index.saturating_add_signed(delta).min(keys.len() - 1);
        if next != index {
            let key = keys[next].clone();
            self.open_panel(Panel::Tool(key));
        }
    }

    /// `/tool`: the card at the top of the viewport when scrolled, else the
    /// newest one.
    pub(super) fn show_tool(&mut self) {
        let blocks = self.transcript_blocks();
        let is_tool = |block: &Block| matches!(block, Block::Tool(_));
        let index = match self.scroll {
            Scroll::Anchor { block, .. } => blocks
                .iter()
                .skip(block)
                .position(is_tool)
                .map(|offset| block + offset)
                .or_else(|| blocks[..block.min(blocks.len())].iter().rposition(is_tool)),
            Scroll::Follow => blocks.iter().rposition(is_tool),
        };
        let key = index.and_then(|index| match &blocks[index] {
            Block::Tool(call) => Some(call.key.clone()),
            _ => None,
        });
        match key {
            Some(key) => self.open_panel(Panel::Tool(key)),
            None => self.notice(NoticeLevel::Info, "No tool calls to show yet"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(streams: &[&str]) -> ToolState {
        ToolState {
            key: ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            rows: vec![ToolRow::Text("tail".into())],
            streams: streams
                .iter()
                .map(|stream| ((*stream).to_owned(), None))
                .collect(),
            scroll: 0,
            status: "loading the full output…".into(),
        }
    }

    fn stream(text: &str) -> StreamText {
        StreamText {
            text: text.into(),
            ..Default::default()
        }
    }

    fn shown(state: &ToolState) -> String {
        views::tool_text(state, None)
    }

    #[test]
    fn the_view_scrolls_within_the_output() {
        let mut state = state(&[]);
        state.rows = (0..10)
            .map(|index| ToolRow::Text(index.to_string()))
            .collect();
        state.scroll_by(4, 5);
        assert_eq!(state.scroll, 4);
        state.scroll_by(99, 5);
        assert_eq!(state.scroll, 5);
        state.scroll_by(isize::MIN, 5);
        assert_eq!(state.scroll, 0);
        state.scroll_by(isize::MAX, 5);
        assert_eq!(state.scroll, 5);
    }

    /// Bash streams have no total order, so they are separate sections, and the
    /// card's tail stays until both have answered.
    #[test]
    fn bash_output_shows_stdout_and_stderr_sections_once_both_arrive() {
        let mut state = state(&["stdout", "stderr"]);
        state.set_stream("stdout", stream("ok 1\nok 2\n"));
        assert_eq!(shown(&state), "tail\n", "half a result keeps the tail");
        state.set_stream("stderr", stream("warn\u{1b}[31m!\n"));
        assert_eq!(
            shown(&state),
            "── stdout ──\nok 1\nok 2\n\n── stderr ──\nwarn!\n"
        );
        assert!(state.status.is_empty());
    }

    #[test]
    fn an_empty_stream_is_omitted_and_retention_is_explained() {
        let mut state = state(&["stdout", "stderr"]);
        let mut stdout = stream("last line\n");
        stdout.dropped_head = true;
        state.set_stream("stdout", stdout);
        state.set_stream("stderr", stream(""));
        let text = shown(&state);
        assert!(!text.contains("stderr"), "{text}");
        assert!(text.contains("no longer retained\nlast line"), "{text}");

        let mut state = self::state(&["output"]);
        state.set_stream("output", stream("result\n"));
        assert_eq!(
            shown(&state),
            "result\n",
            "one stream needs no section header"
        );
    }
}
