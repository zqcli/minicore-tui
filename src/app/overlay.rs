//! The one picker or view that owns input at a time. Its state lives in the
//! variant, so closing it drops everything it held.

use std::borrow::Cow;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::diff::DiffState;
use super::{Action, App, NoticeLevel, commands, find};
use crate::app::picker::fuzzy_match;
use crate::ui::sheets;
use minicore_host_core::{AgentSummary, ModelDescription, SessionSummary};
use minicore_host_protocol::{AgentId, SessionId, SessionRef};

#[derive(Debug)]
pub enum Overlay {
    None,
    /// An irreversible action, stated with what it affects; only `y` runs it.
    Confirm {
        title: String,
        lines: Vec<String>,
        action: super::Action,
    },
    Unconfirmed {
        selected: usize,
        mode: super::unconfirmed::ReviewMode,
    },
    /// The inline command list; its filter is the composer's `/name`.
    Palette {
        selected: usize,
    },
    Sessions {
        filter: String,
        selected: usize,
        /// Display order, fixed when the picker opens.
        rows: Vec<SessionSummary>,
        /// The new title being typed for the selected session.
        renaming: Option<(SessionRef, String)>,
    },
    Agents {
        filter: String,
        selected: usize,
    },
    AgentDelete {
        agent_id: AgentId,
        name: String,
        sessions: Vec<(SessionId, String)>,
        selected: usize,
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
    /// `/session`; facts come from its native info, state and context.
    SessionInfo,
    Help {
        scroll: usize,
    },
    Error {
        text: String,
        scroll: usize,
        visible_rows: usize,
    },
    Diff(DiffState),
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
        matches!(self, Self::Diff(_) | Self::Help { .. } | Self::Error { .. })
    }
}

/// A picker or view a command or key opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Panel {
    Unconfirmed,
    Sessions,
    Agents,
    AgentDelete,
    Models,
    Reasoning,
    Files,
    Context,
    SessionInfo,
    Help,
    Error,
    Diff,
    Find,
}

impl Panel {
    /// Whether the panel reads a session, so a draft has nothing to show in it.
    fn needs_session(&self) -> bool {
        matches!(self, Self::Context | Self::SessionInfo | Self::Diff)
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
pub fn matching_models<'a>(
    models: &'a [ModelDescription],
    filter: &str,
) -> Vec<&'a ModelDescription> {
    models
        .iter()
        .filter(|model| fuzzy_match(model.model.as_str(), filter.trim()))
        .collect()
}

/// The picker names a Session from catalog metadata, not its loaded history.
pub fn session_label(info: &SessionSummary) -> &str {
    info.title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| short_session_id(info.id.as_str()))
}

/// The folder a session's workspace ends in.
pub fn workspace_name(info: &SessionSummary) -> Option<Cow<'_, str>> {
    Path::new(&info.workspace.as_ref()?.path)
        .file_name()
        .map(|name| name.to_string_lossy())
}

/// The first six symbols of a native ID, enough to scan a picker row.
pub fn short_session_id(id: &str) -> &str {
    &id[..id.len().min(6)]
}

pub fn session_target(info: &SessionSummary) -> SessionRef {
    SessionRef {
        agent_id: info.agent_id.clone(),
        session_id: info.id.clone(),
    }
}

pub fn matching_agents<'a>(agents: &'a [AgentSummary], filter: &str) -> Vec<&'a AgentSummary> {
    let query = filter.to_lowercase();
    agents
        .iter()
        .filter(|agent| {
            format!("{} {}", agent.name, agent.id)
                .to_lowercase()
                .contains(&query)
        })
        .collect()
}

pub fn matching_sessions<'a>(rows: &'a [SessionSummary], filter: &str) -> Vec<&'a SessionSummary> {
    let filter = filter.trim();
    rows.iter()
        .filter(|info| {
            filter.is_empty()
                || fuzzy_match(session_label(info), filter)
                || fuzzy_match(info.id.as_str(), filter)
                || fuzzy_match(info.agent_id.as_str(), filter)
                || workspace_name(info).is_some_and(|name| fuzzy_match(&name, filter))
        })
        .collect()
}

impl App {
    /// The row the open sessions picker has selected.
    pub(super) fn selected_session(&self) -> Option<&SessionSummary> {
        let Overlay::Sessions {
            rows,
            filter,
            selected,
            ..
        } = &self.overlay
        else {
            return None;
        };
        matching_sessions(rows, filter).get(*selected).copied()
    }

    /// Opens a picker or view, seeding it from the current state.
    pub fn open_panel(&mut self, panel: Panel) {
        if panel == Panel::AgentDelete {
            if let Some(id) = self.current_agent_id().cloned() {
                self.confirm_agent_delete(id);
            } else {
                self.notice(NoticeLevel::Info, "Select an Agent first");
            }
            return;
        }
        if panel == Panel::Unconfirmed {
            self.open_unconfirmed();
            return;
        }
        if self.resident.current().is_none() {
            // `workspace.files` needs a loaded session.
            let hint = match panel {
                Panel::Files => Some("@ file completion needs a session: send a message first"),
                _ if panel.needs_session() => Some("No session is open"),
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
            Panel::Unconfirmed | Panel::AgentDelete => {
                unreachable!("confirmation panels open before session panels")
            }
            Panel::Sessions => Overlay::Sessions {
                filter: String::new(),
                selected: 0,
                rows: self.session_rows(),
                renaming: None,
            },
            Panel::Agents => Overlay::Agents {
                filter: String::new(),
                selected: self
                    .agents
                    .iter()
                    .position(|agent| Some(&agent.id) == self.current_agent_id())
                    .unwrap_or(0),
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
                self.refresh_context();
                Overlay::SessionInfo
            }
            Panel::Help => Overlay::Help { scroll: 0 },
            Panel::Error => Overlay::Error {
                text: self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "No operation error has been recorded.".into()),
                scroll: 0,
                visible_rows: 0,
            },
            Panel::Diff => Overlay::Diff(self.open_diff()),
            Panel::Find => Overlay::Find {
                query: String::new(),
                matches: Vec::new(),
                current: 0,
            },
        };
    }

    pub(super) fn confirm_agent_delete(&mut self, agent_id: AgentId) {
        let Some(agent) = self.agents.iter().find(|agent| agent.id == agent_id) else {
            self.notice(NoticeLevel::Error, "Agent is no longer in the catalog");
            return;
        };
        self.overlay = Overlay::AgentDelete {
            agent_id,
            name: agent.name.clone(),
            sessions: agent
                .sessions
                .iter()
                .map(|session| {
                    (
                        session.id.clone(),
                        session
                            .title
                            .clone()
                            .unwrap_or_else(|| short_session_id(session.id.as_str()).to_owned()),
                    )
                })
                .collect(),
            selected: 0,
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
        matches!(name, "model" | "reasoning" | "theme" | "approval").then_some((name, rest.trim()))
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
                let mut detail = if command.args.is_empty() {
                    command.description.to_owned()
                } else {
                    format!("{} — {}", command.args, command.description)
                };
                if let Some(current) = self.current_value(command.name) {
                    detail = format!("now {current} · {detail}");
                }
                (command.name.to_owned(), detail)
            })
            .collect()
    }

    /// The value a setting command would change, shown before it runs.
    fn current_value(&self, name: &str) -> Option<String> {
        match name {
            "model" => self.model_id().map(str::to_owned),
            "reasoning" => self
                .reasoning()
                .map(|level| sheets::reasoning_label(level).to_owned()),
            "approval" => self
                .resident
                .current()
                .map(|session| approval_label(session.info.config.execution.approval).to_owned()),
            "theme" => Some(
                match self.theme {
                    crate::config::ThemeKind::Dark => "dark",
                    crate::config::ThemeKind::Light => "light",
                }
                .to_owned(),
            ),
            _ => None,
        }
    }

    fn arg_rows(&self, name: &str, filter: &str) -> Vec<(String, String)> {
        let described = |values: &[(&str, &str)]| -> Vec<(String, String)> {
            values
                .iter()
                .map(|(value, detail)| ((*value).to_owned(), (*detail).to_owned()))
                .collect()
        };
        let mut rows: Vec<(String, String)> = match name {
            "model" => self
                .models
                .iter()
                .map(|model| (model.model.to_string(), sheets::model_details(model)))
                .collect(),
            "reasoning" => self
                .reasoning_levels()
                .iter()
                .map(|level| {
                    (
                        sheets::reasoning_label(*level).to_owned(),
                        sheets::reasoning_description(*level).to_owned(),
                    )
                })
                .collect(),
            "theme" => described(&[
                ("dark", "Light text on a dark terminal"),
                ("light", "Dark text on a light terminal"),
            ]),
            "approval" => described(&[
                ("ask", "Ask before each tool call that needs approval"),
                ("auto", "Run tool calls without asking"),
                ("read_only", "Allow only tools that change nothing"),
            ]),
            _ => unreachable!(),
        };
        // The same current mark as the pickers.
        if let Some(current) = self.current_value(name) {
            for (value, detail) in &mut rows {
                if *value == current {
                    *detail = if detail.is_empty() {
                        "✓ current".to_owned()
                    } else {
                        format!("✓ current · {detail}")
                    };
                }
            }
        }
        rows.retain(|(value, _)| fuzzy_match(value, filter));
        rows.sort_by_key(|(value, _)| (value != filter, !value.starts_with(filter)));
        rows
    }

    /// Copies the foreground session's id.
    pub(super) fn copy_session_id(&mut self) {
        if let Some(session) = self.resident.current() {
            let id = session.info.id.to_string();
            self.copy(id);
        }
    }

    /// Ctrl+N in the sessions picker: edits the selected session's title.
    pub(super) fn start_rename(&mut self) {
        let Some(info) = self.selected_session() else {
            return;
        };
        let title = info.title.clone().unwrap_or_default();
        let target = session_target(info);
        if let Overlay::Sessions { renaming, .. } = &mut self.overlay {
            *renaming = Some((target, title));
        }
    }

    /// Keys while a title is being typed in the sessions picker.
    pub(super) fn on_rename_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Overlay::Sessions {
            renaming: Some((target, title)),
            ..
        } = &mut self.overlay
        else {
            return None;
        };
        match key.code {
            KeyCode::Enter => {
                let title = std::mem::take(title);
                let target = target.clone();
                if let Overlay::Sessions { renaming, .. } = &mut self.overlay {
                    *renaming = None;
                }
                return Some(Action::RenameSession { target, title });
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
        None
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
            .position(|model| Some(model.model.as_str()) == current)
            .unwrap_or(0)
    }

    /// The known sessions: the live ones first (on screen, then the most
    /// recently left), then the current workspace's, then newest first.
    fn session_rows(&self) -> Vec<SessionSummary> {
        let workspace = self.workspace.to_string_lossy();
        let live = |info: &SessionSummary| match self.resident.current() {
            Some(session)
                if session.info.id == info.id && session.info.agent_id == info.agent_id =>
            {
                0
            }
            _ => self
                .resident
                .others()
                .position(|session| {
                    session.info.id == info.id && session.info.agent_id == info.agent_id
                })
                .map_or(usize::MAX, |at| at + 1),
        };
        let rank = |info: &SessionSummary| {
            (
                info.workspace
                    .as_ref()
                    .is_some_and(|value| value.path == workspace),
                info.last_activity_at,
            )
        };
        let mut rows = self.sessions.clone();
        rows.sort_by_cached_key(|info| (live(info), std::cmp::Reverse(rank(info))));
        rows
    }
}

fn approval_label(mode: minicore_host_protocol::ApprovalMode) -> &'static str {
    match mode {
        minicore_host_protocol::ApprovalMode::Ask => "ask",
        minicore_host_protocol::ApprovalMode::Auto => "auto",
        minicore_host_protocol::ApprovalMode::ReadOnly => "read_only",
    }
}
