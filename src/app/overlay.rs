//! The one picker or view that owns input at a time. Its state lives in the
//! variant, so closing it drops everything it held.

use serde_json::json;

use super::diff::DiffState;
use super::tool::ToolState;
use super::{App, DiffScope, SessionOp, commands, find};
use crate::protocol::{self, FilesPage, ModelInfo, SessionInfo, ToolRef};
use crate::ui::picker::fuzzy_match;

#[derive(Debug)]
pub enum Overlay {
    None,
    /// The inline command list; its filter is the composer's `/name`.
    Palette {
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
        filter: String,
        selected: usize,
    },
    Reasoning {
        selected: usize,
    },
    /// The inline `@` file list; its filter is the composer's `@token`.
    Files {
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

/// The workspace paths containing the typed filter, case-insensitively; a
/// match in the file name ranks before one only in the directories.
pub fn matching_paths<'a>(paths: &'a [String], filter: &str) -> Vec<&'a String> {
    let filter = filter.to_lowercase();
    let mut matches: Vec<&String> = paths
        .iter()
        .filter(|path| path.to_lowercase().contains(&filter))
        .collect();
    matches.sort_by_cached_key(|path| {
        let name = file_name(path).to_lowercase();
        (!name.starts_with(&filter), !name.contains(&filter))
    });
    matches
}

/// The last component of a workspace path.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The models whose id fuzzy-matches `filter`, in catalog order.
pub fn matching_models<'a>(models: &'a [ModelInfo], filter: &str) -> Vec<&'a ModelInfo> {
    models
        .iter()
        .filter(|model| fuzzy_match(&model.id, filter.trim()))
        .collect()
}

impl App {
    /// Opens a picker or view, seeding it from the current state.
    pub fn open_panel(&mut self, panel: Panel) {
        self.overlay = match panel {
            Panel::Sessions => Overlay::Sessions {
                filter: String::new(),
                selected: 0,
                rows: self.session_rows(),
                armed: None,
            },
            Panel::Models => Overlay::Models {
                filter: String::new(),
                selected: self.current_model_row(),
            },
            Panel::Reasoning => {
                let current = self.session.as_ref().map(|session| session.info.reasoning);
                let levels = self.reasoning_levels();
                Overlay::Reasoning {
                    selected: levels
                        .iter()
                        .position(|level| Some(*level) == current)
                        .unwrap_or(0),
                }
            }
            Panel::Files => {
                self.list_files();
                Overlay::Files {
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

    /// The command name typed after `/` while the draft is a one-line command.
    pub fn palette_query(&self) -> Option<&str> {
        let text = self.composer.text();
        commands::is_command_line(text)
            .then(|| commands::parse(text).map(|(name, _)| name))
            .flatten()
    }

    /// The `@token` that ends at the cursor, without its `@`.
    pub fn mention_query(&self) -> Option<&str> {
        let text = self.composer.text();
        text[self.word_start()..self.composer.cursor()].strip_prefix('@')
    }

    /// Where the whitespace-delimited word before the cursor begins.
    pub(super) fn word_start(&self) -> usize {
        self.composer.text()[..self.composer.cursor()]
            .char_indices()
            .rfind(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(at, ch)| at + ch.len_utf8())
    }

    /// The model picker opens on the session's model.
    fn current_model_row(&self) -> usize {
        let current = self.session.as_ref().map(|session| &session.info.model);
        self.models
            .iter()
            .position(|model| Some(&model.id) == current)
            .unwrap_or(0)
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
