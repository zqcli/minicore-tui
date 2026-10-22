//! The one picker or view that owns input at a time. Its state lives in the
//! variant, so closing it drops everything it held.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use super::diff::DiffState;
use super::draft::NO_SESSION;
use super::session::auto_title;
use super::tool::ToolView;
use super::{App, DiffScope, Msg, NoticeLevel, Session, SessionOp, commands, find, tasks};
use crate::app::picker::fuzzy_match;
use crate::protocol::{self, FilesPage, ModelInfo, RpcError, SessionInfo, ToolRef};
use crate::ui::sheets;

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
        /// The new title being typed for the selected session.
        renaming: Option<String>,
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
    /// `/session`; the item count arrives from `session.read`.
    SessionInfo {
        items: Option<usize>,
    },
    Help {
        scroll: usize,
    },
    Diff(DiffState),
    Tool(ToolView),
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

    /// A full-screen view, which hides the transcript and the keys of the composer.
    pub fn is_view(&self) -> bool {
        matches!(
            self,
            Self::Diff(_) | Self::Tool(_) | Self::Context | Self::Help { .. }
        )
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
    SessionInfo,
    Help,
    Diff(DiffScope),
    Find,
    Tool(ToolRef),
}

impl Panel {
    /// Whether the panel reads a session, so a draft has nothing to show in it.
    fn needs_session(&self) -> bool {
        matches!(
            self,
            Self::Context | Self::SessionInfo | Self::Diff(_) | Self::Tool(_)
        )
    }
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

/// A session's name in the picker: its title, else (for the resident session)
/// its first prompt, else its short id.
pub fn session_label(info: &SessionInfo, resident: Option<&Session>) -> String {
    let title = info
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty());
    title
        .map(str::to_owned)
        .or_else(|| {
            let resident = resident.filter(|session| session.info.session_id == info.session_id)?;
            auto_title(resident.first_prompt()?)
        })
        .unwrap_or_else(|| short_session_id(&info.session_id).to_owned())
}

/// `ses_8ca3c4cf...` → `ses_8ca3c4`, enough to tell sessions apart in a row.
pub fn short_session_id(session_id: &str) -> &str {
    let body = session_id.strip_prefix("ses_").unwrap_or(session_id);
    let cut = body
        .char_indices()
        .nth(6)
        .map_or(body.len(), |(index, _)| index);
    &session_id[..session_id.len() - body.len() + cut]
}

/// The picker rows whose label fuzzy-matches `filter`, in order.
pub fn filtered_sessions<'a>(
    rows: &'a [SessionInfo],
    filter: &str,
    resident: Option<&Session>,
) -> Vec<&'a SessionInfo> {
    let filter = filter.trim();
    rows.iter()
        .filter(|info| filter.is_empty() || fuzzy_match(&session_label(info, resident), filter))
        .collect()
}

impl App {
    /// The reasoning levels the current model supports, in Agent order.
    pub fn reasoning_levels(&self) -> Vec<protocol::Reasoning> {
        let id = self.model_id();
        self.models
            .iter()
            .find(|model| Some(model.id.as_str()) == id)
            .map(|model| model.supported_reasoning.clone())
            .unwrap_or_default()
    }

    /// Opens a picker or view, seeding it from the current state.
    pub fn open_panel(&mut self, panel: Panel) {
        if self.session.is_none() {
            // `workspace.files` needs a loaded session.
            let hint = match panel {
                Panel::Files => Some("@ file completion needs a session: send a message first"),
                _ if panel.needs_session() => Some(NO_SESSION),
                _ => None,
            };
            if let Some(hint) = hint {
                // The command list the panel was picked from closes too.
                self.overlay = Overlay::None;
                self.notice(NoticeLevel::Info, hint);
                return;
            }
        }
        self.overlay = match panel {
            Panel::Sessions => Overlay::Sessions {
                filter: String::new(),
                selected: 0,
                rows: self.session_rows(),
                armed: None,
                renaming: None,
            },
            Panel::Models => Overlay::Models {
                filter: String::new(),
                selected: self.current_model_row(),
            },
            Panel::Reasoning => {
                let current = self.reasoning();
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
            Panel::SessionInfo => {
                self.count_items();
                Overlay::SessionInfo { items: None }
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

    /// The command and the partial argument while the draft is `/name arg`
    /// for a command whose argument completes.
    pub fn palette_arg(&self) -> Option<(&str, &str)> {
        let text = self.composer.text();
        if !commands::is_command_line(text) {
            return None;
        }
        let (name, rest) = text.strip_prefix('/')?.split_once(char::is_whitespace)?;
        matches!(name, "model" | "new" | "reasoning" | "theme" | "diff")
            .then_some((name, rest.trim()))
    }

    /// The command list's rows as `(name, description)`: the commands for the
    /// typed name, or the values the command accepts for its argument.
    pub fn palette_rows(&self) -> Vec<(String, String)> {
        if let Some((name, filter)) = self.palette_arg() {
            return self.arg_rows(name, filter);
        }
        let Some(query) = self.palette_query() else {
            return Vec::new();
        };
        commands::filter(query, &self.templates)
            .into_iter()
            .map(|command| {
                let detail = if command.args.is_empty() {
                    command.description.to_owned()
                } else {
                    format!("{} — {}", command.args, command.description)
                };
                (command.name.to_owned(), detail)
            })
            .collect()
    }

    fn arg_rows(&self, name: &str, filter: &str) -> Vec<(String, String)> {
        let plain = |values: &[&str]| -> Vec<(String, String)> {
            values
                .iter()
                .map(|value| ((*value).to_owned(), String::new()))
                .collect()
        };
        let mut rows = match name {
            "model" => self
                .models
                .iter()
                .map(|model| (model.id.clone(), sheets::model_details(model)))
                .collect(),
            "new" => self
                .profiles
                .iter()
                .map(|profile| {
                    let approval = profile.approval.as_deref().unwrap_or_default();
                    (profile.id.clone(), format!("{} {approval}", profile.model))
                })
                .collect(),
            "reasoning" => self
                .reasoning_levels()
                .iter()
                .map(|level| (level.label().to_owned(), String::new()))
                .collect(),
            "theme" => plain(&["dark", "light"]),
            _ => plain(&["session", "workspace"]),
        };
        rows.retain(|(value, _)| fuzzy_match(value, filter));
        rows.sort_by_key(|(value, _)| (value != filter, !value.starts_with(filter)));
        rows
    }

    /// Asks how many history items the session holds.
    fn count_items(&self) {
        let fields =
            json!({"limit": protocol::PROBE_LIMIT, "max_bytes": protocol::PROBE_MAX_BYTES});
        self.call(
            protocol::METHOD_SESSION_READ,
            fields,
            |result: Result<protocol::ReadSessionResult, _>| {
                SessionOp::ItemCount(result.map(|page| page.total))
            },
        );
    }

    /// Copies the resident session's id.
    pub(super) fn copy_session_id(&mut self) {
        if let Some(session) = &self.session {
            let id = session.info.session_id.clone();
            self.copy(id);
        }
    }

    /// Ctrl+N in the sessions picker: edits the selected session's title.
    pub(super) fn start_rename(&mut self) {
        let Overlay::Sessions {
            rows,
            filter,
            selected,
            renaming,
            ..
        } = &mut self.overlay
        else {
            return;
        };
        let shown = filtered_sessions(rows, filter, self.session.as_ref());
        if let Some(info) = shown.get(*selected) {
            *renaming = Some(info.title.clone().unwrap_or_default());
        }
    }

    /// Keys while a title is being typed in the sessions picker.
    pub(super) fn on_rename_key(&mut self, key: KeyEvent) {
        let Overlay::Sessions {
            rows,
            filter,
            selected,
            renaming: Some(title),
            ..
        } = &mut self.overlay
        else {
            return;
        };
        match key.code {
            KeyCode::Enter => {
                let title = std::mem::take(title);
                let shown = filtered_sessions(rows, filter, self.session.as_ref());
                let id = shown.get(*selected).map(|info| info.session_id.clone());
                if let Overlay::Sessions { renaming, .. } = &mut self.overlay {
                    *renaming = None;
                }
                if let Some(id) = id {
                    self.rename_session(id, title);
                }
            }
            KeyCode::Backspace => {
                title.pop();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                title.push(character);
            }
            KeyCode::Esc => {
                if let Overlay::Sessions { renaming, .. } = &mut self.overlay {
                    *renaming = None;
                }
            }
            _ => {}
        }
    }

    fn rename_session(&self, session_id: String, title: String) {
        let client = self.client.clone();
        self.spawn_msg(async move {
            let params = json!({ "session_id": session_id, "title": title.trim() });
            Msg::Renamed(tasks::session(client, protocol::METHOD_SESSION_RENAME, params).await)
        });
    }

    /// A renamed session replaces its entry in the picker, the catalog and the footer.
    pub(super) fn on_renamed(&mut self, result: Result<SessionInfo, RpcError>) {
        let Some(info) = self.ok(result) else {
            return;
        };
        if let Overlay::Sessions { rows, .. } = &mut self.overlay
            && let Some(row) = rows
                .iter_mut()
                .find(|row| row.session_id == info.session_id)
        {
            *row = info.clone();
        }
        if let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.info.session_id == info.session_id)
        {
            session.info = info.clone();
        }
        self.merge_catalog_info(info);
        self.notice(NoticeLevel::Info, "Session renamed");
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

    /// The model picker opens on the current model.
    fn current_model_row(&self) -> usize {
        let current = self.model_id();
        self.models
            .iter()
            .position(|model| Some(model.id.as_str()) == current)
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
