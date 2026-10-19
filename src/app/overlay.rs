//! The one picker or view that owns input at a time. Its state lives in the
//! variant, so closing it drops everything it held.

use serde_json::json;

use super::diff::DiffState;
use super::tool::ToolState;
use super::{App, DiffScope, SessionOp, find};
use crate::protocol::{self, FilesPage, SessionInfo, ToolRef};

#[derive(Debug)]
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
        /// Display order, fixed when the picker opens.
        rows: Vec<SessionInfo>,
        /// The session a first Ctrl+D asked to delete.
        armed: Option<String>,
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
        paths: Vec<String>,
    },
    Context,
    Help {
        scroll: usize,
    },
    Diff(DiffState),
    Tool(ToolState),
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

/// The workspace paths containing the typed filter.
pub fn matching_paths<'a>(paths: &'a [String], filter: &str) -> Vec<&'a String> {
    let filter = filter.trim();
    paths
        .iter()
        .filter(|path| filter.is_empty() || path.contains(filter))
        .collect()
}

impl App {
    /// Opens a picker or view, seeding it from the current state.
    pub fn open_panel(&mut self, panel: Panel) {
        self.overlay = match panel {
            Panel::Palette => Overlay::Palette {
                filter: String::new(),
                selected: 0,
            },
            Panel::Sessions => Overlay::Sessions {
                filter: String::new(),
                selected: 0,
                rows: self.session_rows(),
                armed: None,
            },
            Panel::Models => Overlay::Models { selected: 0 },
            Panel::Reasoning => Overlay::Reasoning { selected: 0 },
            Panel::Files => {
                self.list_files();
                Overlay::Files {
                    filter: String::new(),
                    selected: 0,
                    paths: Vec::new(),
                }
            }
            Panel::Context => {
                self.refresh_context();
                Overlay::Context
            }
            Panel::Help => Overlay::Help { scroll: 0 },
            Panel::Diff(scope) => Overlay::Diff(self.open_diff(scope)),
            Panel::Find => Overlay::Find {
                query: String::new(),
                matches: Vec::new(),
                current: 0,
            },
            Panel::Tool(key) => self.open_tool(key).map_or(Overlay::None, Overlay::Tool),
        };
    }

    /// The known sessions, the resident workspace first, then newest first.
    fn session_rows(&self) -> Vec<SessionInfo> {
        let workspace = self.workspace.to_string_lossy();
        let rank = |info: &SessionInfo| (info.workspace == workspace, info.updated_at.clone());
        let mut rows = self.sessions.clone();
        rows.sort_by_cached_key(|info| std::cmp::Reverse(rank(info)));
        rows
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
}
