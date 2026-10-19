//! The one picker or view that owns input at a time, and how each opens.

use serde_json::json;

use super::{App, DiffScope, Scroll, SessionOp, find, tasks};
use crate::protocol::{self, FilesPage, SessionInfo, ToolReadResult, ToolRef};
use crate::transcript::{Block, NoticeLevel};
use crate::ui::views::{ToolView, tool_state_label};
use crate::ui::{picker, sheets};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// The command palette, with its filter text.
    Palette {
        filter: String,
        selected: usize,
    },
    Sessions {
        filter: String,
        selected: usize,
    },
    Models {
        selected: usize,
    },
    Reasoning {
        selected: usize,
    },
    /// `@`-mention file picker seeded from the composer.
    Files {
        filter: String,
        selected: usize,
    },
    Context,
    Help,
    Diff {
        scope: DiffScope,
        file: usize,
        scroll: u16,
    },
    /// Full output of one tool call.
    Tool(ToolRef),
    /// Find in the transcript.
    Find {
        query: String,
        matches: Vec<find::FindMatch>,
        current: usize,
    },
}

impl Overlay {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// A picker or view a command or key opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Panel {
    Palette,
    Sessions,
    Models,
    Reasoning,
    Files,
    Context,
    Help,
    Diff(DiffScope),
    Find,
    Tool(ToolRef),
}

impl App {
    /// Opens a picker or view, seeding it from the current state.
    pub fn open_panel(&mut self, panel: Panel) {
        self.delete_pending = None;
        self.overlay = match panel {
            Panel::Palette => Overlay::Palette {
                filter: String::new(),
                selected: 0,
            },
            Panel::Sessions => {
                self.reload_session_rows();
                Overlay::Sessions {
                    filter: String::new(),
                    selected: 0,
                }
            }
            Panel::Models => Overlay::Models { selected: 0 },
            Panel::Reasoning => Overlay::Reasoning { selected: 0 },
            Panel::Files => {
                self.list_files();
                Overlay::Files {
                    filter: String::new(),
                    selected: 0,
                }
            }
            Panel::Context => {
                self.refresh_context();
                Overlay::Context
            }
            Panel::Help => {
                self.help_scroll = 0;
                Overlay::Help
            }
            Panel::Diff(scope) => {
                self.open_diff(scope);
                Overlay::Diff {
                    scope,
                    file: 0,
                    scroll: 0,
                }
            }
            Panel::Find => Overlay::Find {
                query: String::new(),
                matches: Vec::new(),
                current: 0,
            },
            Panel::Tool(key) => {
                self.open_tool(key.clone());
                Overlay::Tool(key)
            }
        };
    }

    /// Sessions picker rows matching `filter`, in display order.
    pub fn filtered_sessions(&self, filter: &str) -> Vec<SessionInfo> {
        let filter = filter.trim();
        self.session_rows
            .iter()
            .filter(|info| {
                let label = sheets::session_label(info, self.session.as_ref());
                filter.is_empty() || picker::fuzzy_match(&label, filter)
            })
            .cloned()
            .collect()
    }

    /// The resident workspace first, then newest first.
    fn reload_session_rows(&mut self) {
        let workspace = self.workspace.to_string_lossy().into_owned();
        let mut rows = self.sessions.clone();
        rows.sort_by(|left, right| {
            let key = |info: &SessionInfo| (info.workspace != workspace, info.updated_at.clone());
            key(right).cmp(&key(left))
        });
        self.session_rows = rows;
    }

    /// Asks for the workspace files the `@` picker offers.
    fn list_files(&self) {
        let fields = json!({"recursive": true, "query": "", "limit": 500, "max_bytes": 262_144});
        self.call(
            protocol::METHOD_WORKSPACE_FILES,
            fields,
            |result: Result<FilesPage, _>| {
                SessionOp::Files(result.map(|page| {
                    page.entries
                        .into_iter()
                        .filter(|entry| entry.kind == protocol::FileKind::File)
                        .map(|entry| entry.path)
                        .collect()
                }))
            },
        );
    }

    /// Fills the tool view from the card, then pages the full output of every
    /// stream: stdout and stderr for bash, the result text otherwise.
    fn open_tool(&mut self, key: ToolRef) {
        let Some(call) = self
            .session
            .as_ref()
            .and_then(|session| session.transcript.find_tool(&key))
        else {
            return;
        };
        let streams: &[&str] = if call.name == "bash" {
            &["stdout", "stderr"]
        } else {
            &["output"]
        };
        self.tool_view = ToolView {
            key: Some(key.clone()),
            output: ToolView::text_lines(&call.output.join("\n")),
            streams: streams
                .iter()
                .map(|stream| ((*stream).to_owned(), None))
                .collect(),
            status: "loading the full output…".into(),
            ..Default::default()
        };
        self.refresh_tool_header();
        let fields = json!({
            "loop_id": key.loop_id, "request_index": key.request_index,
            "tool_call_id": key.tool_call_id, "display": true,
        });
        let read_key = key.clone();
        self.call(
            protocol::METHOD_TOOL_READ,
            fields,
            move |result: Result<ToolReadResult, _>| SessionOp::ToolRead(read_key, result),
        );
        for stream in streams {
            let (client, key, stream) = (self.client.clone(), key.clone(), (*stream).to_owned());
            self.spawn(async move {
                let output = tasks::read_output(client, key.clone(), stream.clone()).await;
                SessionOp::ToolOutput(key, stream, output)
            });
        }
    }

    /// Copies the shown card's name, state, detail and input into the tool
    /// view, which `tool.read` may have just refreshed.
    pub(super) fn refresh_tool_header(&mut self) {
        let Some(key) = self.tool_view.key.clone() else {
            return;
        };
        let keys = self.tool_keys();
        let Some(call) = self
            .session
            .as_ref()
            .and_then(|session| session.transcript.find_tool(&key))
        else {
            return;
        };
        let view = &mut self.tool_view;
        view.title = call.name.clone();
        view.detail = call.detail.clone();
        view.state = tool_state_label(call.state).to_owned();
        view.position = keys
            .iter()
            .position(|known| *known == key)
            .map(|index| (index, keys.len()));
        let dim = ratatui::style::Style::default().fg(self.palette.dim);
        // A one-line input that is the detail (a bash command) is not repeated.
        let input = call
            .expanded_input
            .as_deref()
            .filter(|input| input.trim() != call.detail.trim())
            .unwrap_or_default();
        view.input = ToolView::text_lines(input)
            .into_iter()
            .map(|line| line.style(dim))
            .collect();
    }

    /// Every tool card of the transcript, in order.
    fn tool_keys(&self) -> Vec<ToolRef> {
        self.transcript_blocks()
            .iter()
            .filter_map(|block| match block {
                Block::Tool(call) => Some(call.key.clone()),
                _ => None,
            })
            .collect()
    }

    /// `[` / `]` in the tool view: the previous or next tool card.
    pub fn step_tool(&mut self, delta: isize) {
        let Overlay::Tool(current) = &self.overlay else {
            return;
        };
        let keys = self.tool_keys();
        let Some(index) = keys.iter().position(|key| key == current) else {
            return;
        };
        let next = index.saturating_add_signed(delta).min(keys.len() - 1);
        if next != index {
            self.open_panel(Panel::Tool(keys[next].clone()));
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
