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

/// Active keyboard focus inside the `Ctrl+B` Async Tasks Inspector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskFocus {
    List,
    Detail,
    Composer,
}

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
    /// Every async task / delegated child of this conversation, searchable and
    /// inspectable with a real child conversation pane and direct reply box.
    Delegations {
        filter: String,
        selected: usize,
        focus: TaskFocus,
        /// On narrow terminals (`< 96` cols), toggles between list and child conversation.
        detail_open: bool,
    },
    /// The inline Host file browser; its filter never edits the draft.
    Files {
        selected: usize,
        browser: super::files::FileBrowser,
    },
    Context {
        scroll: usize,
    },
    /// `/session`; facts come from its native info, state and context.
    SessionInfo {
        scroll: usize,
    },
    Help {
        scroll: usize,
        /// Typed words narrowing the keys and commands shown.
        filter: String,
    },
    Error {
        text: String,
        scroll: usize,
        visible_rows: usize,
    },
    /// `/remote`: exposes the embedded Host over WSS/WS and displays
    /// live connection details while terminal control is handed off.
    Remote {
        show_key: bool,
        scroll: usize,
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
        matches!(
            self,
            Self::Diff(_)
                | Self::Delegations { .. }
                | Self::Help { .. }
                | Self::Error { .. }
                | Self::Context { .. }
                | Self::SessionInfo { .. }
                | Self::Remote { .. }
        )
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
    Delegations,
    Files,
    Context,
    SessionInfo,
    Help,
    Error,
    Diff,
    Find,
}

/// Generalized async task view item projected from native Host state. Today
/// populated by delegated Agent sessions; future async commands/tools join the
/// same list without changing the inspector UI.
pub enum AsyncTaskItem<'a> {
    Agent(&'a crate::transcript::agent_card::AgentCard),
    StandaloneDelegated {
        session_id: String,
        summary: crate::ui::delegated::Summary,
    },
}

impl<'a> AsyncTaskItem<'a> {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Agent(_) | Self::StandaloneDelegated { .. } => "agent",
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::Agent(card) => card.key.child.session_id.as_str(),
            Self::StandaloneDelegated { session_id, .. } => session_id.as_str(),
        }
    }

    pub fn short_id(&self) -> &str {
        short_session_id(self.session_id())
    }

    pub fn task_name(&self) -> Option<&str> {
        match self {
            Self::Agent(card) => card
                .association
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty()),
            Self::StandaloneDelegated { .. } => None,
        }
    }

    pub fn parent_session(&self) -> Option<&SessionRef> {
        match self {
            Self::Agent(card) => Some(&card.association.parent),
            Self::StandaloneDelegated { .. } => None,
        }
    }

    pub fn dispatched_by(
        &self,
        root: Option<&SessionRef>,
        names: &std::collections::HashMap<String, String>,
    ) -> Option<String> {
        let parent = self.parent_session()?;
        if root.is_some_and(|r| r == parent) {
            return Some("from main".to_owned());
        }
        let short = short_session_id(parent.session_id.as_str());
        if let Some(name) = names.get(parent.session_id.as_str()) {
            Some(format!("from {name} ({short})"))
        } else {
            Some(format!("from #{short}"))
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::Agent(card) => {
                if let Some(name) = self.task_name() {
                    crate::wrap::sanitize(name).into_owned()
                } else {
                    crate::ui::agent_cards::task(card)
                }
            }
            Self::StandaloneDelegated { summary, .. } => {
                if let Some(answer) = summary
                    .answer
                    .as_deref()
                    .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
                {
                    crate::wrap::sanitize(answer.trim()).into_owned()
                } else {
                    format!("Delegated Session {}", self.short_id())
                }
            }
        }
    }

    pub fn status_label(&self) -> String {
        match self {
            Self::Agent(card) => crate::ui::agent_cards::status(card),
            Self::StandaloneDelegated { summary, .. } => summary.outcome.clone(),
        }
    }

    pub fn subtitle(&self) -> String {
        match self {
            Self::Agent(card) => {
                let mut detail = crate::ui::agent_cards::status(card);
                if let Some((_, turn)) = &card.turn
                    && turn.tool_rounds > 0
                {
                    detail.push_str(&format!(
                        " · {}",
                        crate::ui::blocks::plural(turn.tool_rounds, "tool round")
                    ));
                }
                if let Some(name) = &card.agent_name {
                    detail.push_str(&format!(" · {name}"));
                }
                detail
            }
            Self::StandaloneDelegated { summary, .. } => {
                format!("{} · unloaded origin", summary.outcome)
            }
        }
    }

    pub fn needs_input(&self) -> bool {
        match self {
            Self::Agent(card) => crate::ui::agent_cards::needs_input(card),
            Self::StandaloneDelegated { .. } => false,
        }
    }

    pub fn is_running(&self) -> bool {
        match self {
            Self::Agent(card) => {
                card.outcome().is_none() && !card.historical && !card.history_unavailable
            }
            Self::StandaloneDelegated { .. } => false,
        }
    }

    pub fn is_failed(&self) -> bool {
        match self {
            Self::Agent(card) => {
                card.outcome() == Some(minicore_host_protocol::TurnOutcome::Failed)
            }
            Self::StandaloneDelegated { summary, .. } => summary.outcome == "failed",
        }
    }

    pub fn is_completed(&self) -> bool {
        match self {
            Self::Agent(card) => {
                card.outcome() == Some(minicore_host_protocol::TurnOutcome::Completed)
            }
            Self::StandaloneDelegated { summary, .. } => summary.outcome == "completed",
        }
    }

    fn priority(&self) -> u8 {
        if self.needs_input() {
            0
        } else if self.is_running() {
            1
        } else if self.is_failed() {
            2
        } else {
            3
        }
    }

    pub fn glyph(&self, styles: crate::ui::theme::Styles) -> (&'static str, ratatui::style::Style) {
        match self {
            Self::Agent(card) => crate::ui::agent_cards::glyph(card, styles),
            Self::StandaloneDelegated { summary, .. } => match summary.outcome.as_str() {
                "completed" => ("✓", styles.success()),
                "failed" => ("✗", styles.failure()),
                "cancelled" => ("⊘", styles.meta()),
                _ => ("○", styles.meta()),
            },
        }
    }

    pub fn matches_filter(&self, filter: &str) -> bool {
        let query = filter.trim();
        if query.is_empty() {
            return true;
        }
        let lower_q = query.to_lowercase();
        match self {
            Self::Agent(card) => {
                let text = format!(
                    "{} {} {} {} {}",
                    self.kind_label(),
                    card.association.name.as_deref().unwrap_or(""),
                    card.task,
                    card.agent_name.as_deref().unwrap_or(""),
                    crate::ui::agent_cards::status(card),
                );
                let ids = format!(
                    "{} {} {}",
                    card.key.child.session_id,
                    card.key.child.agent_id,
                    card.association.parent.session_id,
                );
                text.to_lowercase().contains(&lower_q)
                    || ids.to_lowercase().contains(&lower_q)
                    || fuzzy_match(&text, query)
            }
            Self::StandaloneDelegated {
                session_id,
                summary,
            } => {
                let text = format!(
                    "{} {} {}",
                    self.kind_label(),
                    summary.outcome,
                    summary.answer.as_deref().unwrap_or(""),
                );
                text.to_lowercase().contains(&lower_q)
                    || session_id.to_lowercase().contains(&lower_q)
                    || fuzzy_match(&text, query)
            }
        }
    }

    pub fn detail_lines(
        &self,
        styles: crate::ui::theme::Styles,
        width: usize,
        reasoning: bool,
    ) -> Vec<ratatui::text::Line<'static>> {
        match self {
            Self::Agent(card) => {
                crate::ui::agent_cards::detail_lines(card, styles, width, reasoning)
            }
            Self::StandaloneDelegated { summary, .. } => {
                crate::ui::delegated::detail_lines(summary, styles, width)
            }
        }
    }

    pub fn copy_text(&self) -> String {
        match self {
            Self::Agent(card) => crate::ui::agent_cards::copy_text(card),
            Self::StandaloneDelegated { summary, .. } => crate::ui::delegated::copy_text(summary),
        }
    }

    pub fn open_target(&self) -> Option<(SessionRef, bool)> {
        match self {
            Self::Agent(card) => Some((card.association.child.clone(), card.history_unavailable)),
            Self::StandaloneDelegated { .. } => None,
        }
    }

    pub fn origin_target(&self) -> Option<(SessionRef, minicore_host_protocol::ToolCallRef)> {
        match self {
            Self::Agent(card) => Some((
                card.association.parent.clone(),
                card.association.origin.clone(),
            )),
            Self::StandaloneDelegated { .. } => None,
        }
    }
}

/// All async tasks for the active conversation, sorted by urgency
/// (`needs_input` → `running` → `failed` → settled) and newest-first within each group.
pub fn async_tasks(app: &super::App) -> Vec<AsyncTaskItem<'_>> {
    let Some(session) = app.resident.current() else {
        return Vec::new();
    };
    let root_addr = session.address();
    let mut seen_sessions = std::collections::HashSet::new();
    let mut items = Vec::new();
    for block in session.transcript.blocks().iter().rev() {
        if let crate::transcript::Block::Tool(call) = block
            && let Some(card) = call.agent_card.as_deref()
        {
            seen_sessions.insert(card.key.child.session_id.to_string());
            items.push(AsyncTaskItem::Agent(card));
        }
    }
    for card in app.agent_cards.cards_for_root(&root_addr) {
        if seen_sessions.insert(card.key.child.session_id.to_string()) {
            items.push(AsyncTaskItem::Agent(card));
        }
    }
    for block in session.transcript.blocks().iter().rev() {
        if let crate::transcript::Block::User {
            kind: crate::transcript::UserKind::Delegated,
            text,
            ..
        } = block
            && let Some(summary) = crate::ui::delegated::summary(text)
            && seen_sessions.insert(summary.session_id.clone())
        {
            items.push(AsyncTaskItem::StandaloneDelegated {
                session_id: summary.session_id.clone(),
                summary,
            });
        }
    }
    items.sort_by_key(AsyncTaskItem::priority);
    items
}

pub fn matching_async_tasks<'a>(
    items: &'a [AsyncTaskItem<'a>],
    filter: &str,
) -> Vec<&'a AsyncTaskItem<'a>> {
    items
        .iter()
        .filter(|item| item.matches_filter(filter))
        .collect()
}

/// One delegated child for the overview: its card, newest first.
pub fn delegations(app: &super::App) -> Vec<&crate::transcript::agent_card::AgentCard> {
    let Some(session) = app.resident.current() else {
        return Vec::new();
    };
    session
        .transcript
        .blocks()
        .iter()
        .rev()
        .filter_map(|block| match block {
            crate::transcript::Block::Tool(call) => call.agent_card.as_deref(),
            _ => None,
        })
        .collect()
}

impl Panel {
    /// Whether the panel reads a session, so a draft has nothing to show in it.
    fn needs_session(&self) -> bool {
        matches!(self, Self::Context | Self::SessionInfo | Self::Diff)
    }
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

pub fn session_title(info: &SessionSummary) -> Option<&str> {
    info.title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
}

/// A preview is a display fallback, never a persisted or editable title.
pub fn session_label(info: &SessionSummary) -> &str {
    session_title(info)
        .or_else(|| {
            info.first_user_preview
                .as_deref()
                .filter(|text| !text.is_empty())
        })
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
            info.delegated_by.is_none()
                && (filter.is_empty()
                    || fuzzy_match(session_label(info), filter)
                    || info
                        .first_user_preview
                        .as_deref()
                        .is_some_and(|text| fuzzy_match(text, filter))
                    || fuzzy_match(info.id.as_str(), filter)
                    || fuzzy_match(info.agent_id.as_str(), filter)
                    || workspace_name(info).is_some_and(|name| fuzzy_match(&name, filter)))
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
        if panel == Panel::Files {
            self.open_files();
            return;
        }
        if self.resident.current().is_none() {
            let hint = match panel {
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
            Panel::Unconfirmed | Panel::AgentDelete | Panel::Files => {
                unreachable!("confirmation panels open before session panels")
            }
            Panel::Delegations => Overlay::Delegations {
                filter: String::new(),
                selected: 0,
                focus: TaskFocus::List,
                detail_open: false,
            },
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
            Panel::Context => {
                self.refresh_context();
                Overlay::Context { scroll: 0 }
            }
            Panel::SessionInfo => {
                self.refresh_context();
                Overlay::SessionInfo { scroll: 0 }
            }
            Panel::Help => Overlay::Help {
                scroll: 0,
                filter: String::new(),
            },
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
        if matches!(self.overlay, Overlay::Delegations { .. }) {
            self.ensure_inspector_session();
        }
    }

    /// Opens the `Ctrl+B` Async Tasks Inspector focused on the given child Session.
    pub fn open_delegation_for(&mut self, child_session_id: &str) {
        let items = async_tasks(self);
        let selected = items
            .iter()
            .position(|item| item.session_id() == child_session_id)
            .unwrap_or(0);
        self.overlay = Overlay::Delegations {
            filter: String::new(),
            selected,
            focus: TaskFocus::List,
            detail_open: true,
        };
        self.ensure_inspector_session();
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
        if !commands::is_command_line(text) {
            return None;
        }
        let (name, _) = commands::parse(text)?;
        if name == "cmd" && self.template_query().is_none() {
            return None;
        }
        Some(name)
    }

    /// Lookup mode for the canonical prefix and the menu entry. A space
    /// after a template name leaves lookup mode so the draft accepts arguments.
    pub fn template_query(&self) -> Option<&str> {
        let text = self.composer.text();
        if !commands::is_command_line(text) {
            return None;
        }
        let rest = text.strip_prefix('/')?;
        if rest == "cmd" {
            return Some("");
        }
        let (prefix, tail) = rest.split_once(char::is_whitespace)?;
        let query = tail.trim_start_matches([' ', '\t']);
        (matches!(prefix, "cmd" | "command") && !query.contains(char::is_whitespace))
            .then_some(query)
    }

    /// The command and the partial argument while the draft is `/name arg`
    /// for a command whose argument completes.
    pub fn palette_arg(&self) -> Option<(&str, &str)> {
        let text = self.composer.text();
        if !commands::is_command_line(text) {
            return None;
        }
        let (name, rest) = text.strip_prefix('/')?.split_once(char::is_whitespace)?;
        matches!(
            name,
            "model" | "reasoning" | "theme" | "approval" | "remote" | "wss"
        )
        .then_some((name, rest.trim()))
    }

    /// The command list's rows as `(name, description)`: the commands for the
    /// typed name, or the values the command accepts for its argument.
    pub fn palette_rows(&self) -> Vec<(String, String)> {
        if let Some(query) = self.template_query() {
            return commands::template_filter(&self.templates, query)
                .into_iter()
                .map(|entry| {
                    (
                        entry.name.to_owned(),
                        format!("{} {}", entry.args, entry.description)
                            .trim()
                            .to_owned(),
                    )
                })
                .collect();
        }
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
    pub(crate) fn current_value(&self, name: &str) -> Option<String> {
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
                ("ask", "Ask before mutating or outside-workspace tool calls"),
                (
                    "auto",
                    "Run inside- and outside-workspace tool calls without asking",
                ),
            ]),
            "remote" | "wss" => described(&[
                (
                    "on",
                    "Start remote WSS/WS server and show connection overlay",
                ),
                (
                    "off",
                    "Cancel startup or stop remote access and restore terminal control",
                ),
                ("key", "Generate a fresh one-time access key"),
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
        let mut rows: Vec<SessionSummary> = self
            .sessions
            .iter()
            .filter(|info| info.delegated_by.is_none())
            .cloned()
            .collect();
        rows.sort_by_cached_key(|info| (live(info), std::cmp::Reverse(rank(info))));
        rows
    }
}

pub(crate) fn approval_label(mode: minicore_host_protocol::ApprovalMode) -> &'static str {
    match mode {
        minicore_host_protocol::ApprovalMode::Ask => "ask",
        minicore_host_protocol::ApprovalMode::Auto => "auto",
    }
}
