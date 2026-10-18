//! Application state and the reducer.
//!
//! `App` is mutated only on the main loop. IO lives in spawned tasks that post
//! [`Msg`] values back; there is no request-kind registry, so each operation's
//! completion message carries what the reducer needs. A result that belongs to
//! a superseded session is dropped by comparing the session id and the reopen
//! epoch.

pub mod actions;
pub mod commands;
pub mod features;
pub mod keys;
#[cfg(test)]
mod lifecycle_tests;
pub mod session;
pub mod tasks;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clipboard;
use crate::protocol::{
    AgentEvent, CompactionPhase, CompactionProgress, ContextBudget, LoopOutcome, ModelInfo,
    ProfileInfo, Reasoning, RpcError, SessionContext, SessionInfo, SessionPresentation,
    SessionState, ToolRef, TurnRef,
};
use crate::rpc::Client;
use crate::theme::ThemeKind;
use crate::transcript::block::{NoticeLevel, ToolState};
use crate::transcript::{Block, ChangeSummary, ToolCall, Transcript};

pub use actions::{Action, Answer};
pub use session::{
    InteractionKind, LiveTurn, PendingInteraction, Phase, Queued, QueuedKind, Session,
};
pub use tasks::Tx;

/// Everything the main loop can hand to the reducer.
pub enum Msg {
    Agent(Box<AgentEvent>),
    AgentClosed(String),
    Op(Box<OpDone>),
    Ready(Box<Ready>),
    Sessions(Vec<SessionInfo>),
    Notice(NoticeLevel, String),
    /// Retry the follow-up queue once post-turn compaction finished.
    RetrySend,
    /// The external editor returned a draft.
    DraftEdited(Option<String>),
    Quit,
}

/// Session settings from the command line: profile and model apply when the
/// session is created, model and reasoning also when an existing one is opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchSettings {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
}

impl LaunchSettings {
    pub fn is_empty(&self) -> bool {
        self.profile.is_none() && self.model.is_none() && self.reasoning.is_none()
    }
}

pub struct Ready {
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    pub sessions: Vec<SessionInfo>,
}

/// One completed async operation.
pub struct OpDone {
    /// Session the operation belongs to; `None` for catalog-level results.
    pub session: Option<String>,
    /// Reopen epoch captured when the operation started.
    pub epoch: u64,
    pub kind: OpKind,
}

pub enum OpKind {
    Opened(Result<SessionInfo, RpcError>),
    Created(Result<SessionInfo, RpcError>),
    Deleted(String, Result<(), RpcError>),
    History(Result<tasks::History, RpcError>),
    Older(Result<tasks::History, RpcError>),
    Canonical(Result<tasks::History, RpcError>),
    TurnItems(Result<Vec<(usize, crate::protocol::read::RawHistoryItem)>, RpcError>),
    State(Result<SessionState, RpcError>),
    Presentation(Result<SessionPresentation, RpcError>),
    Context(Result<SessionContext, RpcError>),
    /// The send response, with the text that was sent so a refusal can hand it
    /// back instead of losing it.
    Sent(String, Result<TurnRef, RpcError>),
    Waited(Box<Result<crate::protocol::TurnWaitResult, RpcError>>),
    Steered(Result<u64, RpcError>),
    Cancelled(Result<(), RpcError>),
    Answered(Result<(), RpcError>),
    Compacted(Result<crate::protocol::CompactionResult, RpcError>),
    Updated(Result<SessionInfo, RpcError>),
    Renamed(Result<SessionInfo, RpcError>),
    Changes(String, Result<ChangeSummary, RpcError>),
    ToolRead(
        ToolRef,
        Result<crate::protocol::tool::ToolReadResult, RpcError>,
    ),
    ToolOutput(ToolRef, String, Result<String, RpcError>),
    Copied(Result<crate::clipboard::Copied, String>),
    Exported(Result<String, String>),
    /// `session.state` poll while waiting for post-turn compaction.
    IdleCheck(Result<bool, RpcError>),
    /// The change records of one diff scope.
    ChangeFiles(
        DiffScope,
        Result<Vec<crate::protocol::ChangeRecord>, RpcError>,
    ),
    /// The hunks of one changed path, read for one scope.
    DiffFile(
        DiffQuery,
        Result<crate::protocol::changes::DiffPage, RpcError>,
    ),
    /// Workspace paths for the `@` picker.
    Files(Result<Vec<String>, RpcError>),
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
    pub at: Instant,
}

/// The one bottom-sheet panel or full-screen view that owns input, if any.
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
        matches: Vec<features::FindMatch>,
        current: usize,
    },
    /// An interaction prompt from the agent.
    Interaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffScope {
    Turn,
    Session,
    Workspace,
}

/// Which path's hunks a diff read is for, and which scope asked. An answer is
/// only allowed to paint the pane when it matches the diff that is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffQuery {
    pub scope: DiffScope,
    pub path: String,
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
    Interaction,
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
                if let Some(session) = self.session.as_ref() {
                    tasks::list_files(
                        self.client.clone(),
                        self.tx.clone(),
                        session.info.session_id.clone(),
                        String::new(),
                    );
                }
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
            Panel::Interaction => Overlay::Interaction,
        };
    }

    /// Titles an untitled session from its first prompt, once, after the turn
    /// was accepted. An explicit title always wins.
    fn title_from_first_prompt(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if !features::needs_auto_title(&session.info) {
            return;
        }
        let Some(prompt) = session
            .transcript
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::User {
                    text,
                    kind: crate::protocol::read::UserKind::Prompt,
                } => Some(text.as_str()),
                _ => None,
            })
        else {
            return;
        };
        let Some(title) = features::auto_title(prompt) else {
            return;
        };
        let session_id = session.info.session_id.clone();
        tasks::rename(self.client.clone(), self.tx.clone(), session_id, title);
    }

    /// Sessions picker rows matching `filter`, in display order.
    pub fn filtered_sessions(&self, filter: &str) -> Vec<SessionInfo> {
        let filter = filter.trim();
        self.session_rows
            .iter()
            .filter(|info| {
                filter.is_empty() || crate::ui::picker::fuzzy_match(&session_label(info), filter)
            })
            .cloned()
            .collect()
    }

    /// The transcript blocks, or an empty slice when no session is open.
    pub fn transcript_blocks(&self) -> &[Block] {
        self.session
            .as_ref()
            .map_or(&[], |session| session.transcript.blocks())
    }

    /// Matches of the find bar's current query.
    pub fn find_matches(&self, query: &str) -> Vec<features::FindMatch> {
        self.session
            .as_ref()
            .map(|session| features::find_matches(session.transcript.blocks(), query))
            .unwrap_or_default()
    }

    /// Rebuilds the sessions picker rows: the resident workspace first, then
    /// newest first.
    fn reload_session_rows(&mut self) {
        let workspace = self.workspace.to_string_lossy().into_owned();
        let mut rows = self.sessions.clone();
        rows.sort_by(|left, right| {
            let key = |info: &SessionInfo| (info.workspace != workspace, info.updated_at.clone());
            key(right).cmp(&key(left))
        });
        self.session_rows = rows;
    }

    /// Rebuilds the diff view from the cards this session already holds. The
    /// hunks appear when `changes.diff` answers.
    fn open_diff(&mut self, scope: DiffScope) {
        let Some(session) = self.session.as_ref() else {
            self.diff = crate::ui::views::DiffView::new("Diff");
            return;
        };
        let title = match scope {
            DiffScope::Turn => "Diff · turn",
            DiffScope::Session => "Diff · session",
            DiffScope::Workspace => "Diff · workspace",
        };
        let mut view = crate::ui::views::DiffView::new(title);
        view.scope = scope;
        if let Some(loop_id) = latest_loop(session) {
            let files = collect_changed_files(&session.transcript, &loop_id);
            view.set_files(files);
        }
        if view.files.is_empty() {
            view.status = "No file changes are recorded for this scope.".into();
        }
        self.diff = view;
        tasks::list_changes(
            self.client.clone(),
            self.tx.clone(),
            session.info.session_id.clone(),
            scope,
        );
    }

    /// Fills the tool view from the card, then pages the full output.
    fn open_tool(&mut self, key: ToolRef) {
        let Some(call) = self
            .session
            .as_ref()
            .and_then(|session| session.transcript.find_tool(&key))
        else {
            return;
        };
        let palette = self.palette;
        self.tool_view = crate::ui::views::ToolView {
            title: call.name.clone(),
            detail: call.detail.clone(),
            state: crate::ui::views::tool_state_label(call.state).to_owned(),
            input: call
                .expanded_input
                .as_deref()
                .map(|input| {
                    input
                        .lines()
                        .map(|line| {
                            ratatui::text::Line::styled(
                                line.to_owned(),
                                ratatui::style::Style::default().fg(palette.dim),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            output: call
                .view_body()
                .lines()
                .map(|line| ratatui::text::Line::raw(line.to_owned()))
                .collect(),
            scroll: 0,
            status: String::new(),
        };
        tasks::read_tool(self.client.clone(), self.tx.clone(), key);
    }
}

impl Overlay {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// Whether the overlay replaces the transcript area.
    pub fn is_view(&self) -> bool {
        matches!(
            self,
            Self::Context | Self::Help | Self::Diff { .. } | Self::Tool(_)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    /// Stick to the newest content.
    Follow,
    /// Keep this block at the top of the viewport.
    Anchor { block: usize, offset: u16 },
}

pub struct App {
    pub client: Arc<Client>,
    pub tx: Tx,
    pub workspace: PathBuf,
    pub session: Option<Session>,
    pub sessions: Vec<SessionInfo>,
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    /// Session settings requested on the command line. A new session is created
    /// with exactly these; opening an existing one applies them through
    /// `session.update`, so a flag is never silently dropped.
    pub launch: LaunchSettings,
    pub composer: crate::ui::composer::Composer,
    pub notices: VecDeque<Notice>,
    pub scroll: Scroll,
    pub tools_expanded: bool,
    pub show_reasoning: bool,
    pub quit: bool,
    /// Ctrl+C armed the quit; a second press on an empty composer leaves.
    pub quit_armed: bool,
    pub ready: bool,
    pub fatal: Option<Fatal>,
    pub last_error: Option<String>,
    pub waiting_for_compaction: bool,
    pub started: Instant,
    pub interaction_choice: usize,
    /// Incremented for every compaction operation id, so ids are never reused.
    pub op_seq: u64,
    pub theme: ThemeKind,
    /// The picker/view that owns input, if any.
    pub overlay: Overlay,
    /// Theme state used by the UI layer, kept here so `ui` never mutates App.
    pub palette: crate::theme::Palette,
    /// Mouse selection: the cell the drag started at and where it is now.
    pub selection_start: Option<(u16, u16)>,
    pub selection_end: Option<(u16, u16)>,
    /// The last assistant answer, so `/copy` works after a reopen.
    pub last_answer: Option<String>,
    /// The composed diff view, rebuilt when the overlay opens or its data
    /// changes.
    pub diff: crate::ui::views::DiffView,
    /// The composed context view, refreshed from `session.context`.
    pub context_view: crate::ui::views::ContextView,
    /// The tool-output view, rebuilt from the selected card.
    pub tool_view: crate::ui::views::ToolView,
    /// Scroll offset of the help view.
    pub help_scroll: usize,
    /// Sessions picker rows, in display order, refreshed when it opens.
    pub session_rows: Vec<crate::protocol::SessionInfo>,
    /// Confirm a delete with a second Ctrl+D before it runs.
    pub delete_pending: Option<String>,
    /// Change records of each diff row that still needs its line counts, so a
    /// row is measured once and a revisited row is not re-read.
    pub diff_pending: Vec<(crate::app::DiffQuery, Vec<String>)>,
    /// The one diff read in flight; a row is never read twice at once.
    pub diff_in_flight: Option<crate::app::DiffQuery>,
    /// Where the transcript was drawn last frame, for mouse hit-testing.
    pub transcript_area: Option<ratatui::layout::Rect>,
    /// The rows the last frame drew, so selection copies what is on screen.
    pub rendered: crate::ui::selection::Screen,
    /// The transcript's line cache and viewport. Owned by the renderer; the
    /// reducer only invalidates it through block revisions.
    pub layout: crate::ui::View,
    /// Rows the transcript area had last frame, for paging.
    pub viewport: usize,
    /// A draft waiting for the main loop to run the external editor on it; the
    /// terminal belongs to the loop, not to the reducer.
    pub edit_request: Option<String>,
    /// An open or create request is in flight, so "no session yet" means
    /// "waiting" rather than "start one".
    pub loading_session: bool,
}

#[derive(Clone, Debug)]
pub struct Fatal {
    pub reason: String,
    pub stderr_tail: Vec<String>,
}

pub const NOTICE_TTL: Duration = Duration::from_secs(5);
/// Delay before re-checking whether post-turn compaction finished.
pub const BUSY_RETRY: Duration = Duration::from_millis(700);

impl App {
    pub fn new(client: Arc<Client>, tx: Tx, workspace: PathBuf) -> Self {
        Self {
            client,
            tx,
            workspace,
            session: None,
            sessions: Vec::new(),
            models: Vec::new(),
            profiles: Vec::new(),
            launch: LaunchSettings::default(),
            composer: crate::ui::composer::Composer::new(),
            notices: VecDeque::new(),
            scroll: Scroll::Follow,
            tools_expanded: false,
            show_reasoning: false,
            quit: false,
            quit_armed: false,
            ready: false,
            fatal: None,
            last_error: None,
            waiting_for_compaction: false,
            started: Instant::now(),
            interaction_choice: 0,
            op_seq: 0,
            theme: ThemeKind::Dark,
            overlay: Overlay::None,
            palette: crate::theme::Palette::dark(),
            selection_start: None,
            selection_end: None,
            last_answer: None,
            diff: crate::ui::views::DiffView::new("Diff"),
            context_view: crate::ui::views::ContextView::default(),
            tool_view: crate::ui::views::ToolView {
                title: String::new(),
                detail: String::new(),
                state: String::new(),
                input: Vec::new(),
                output: Vec::new(),
                scroll: 0,
                status: String::new(),
            },
            help_scroll: 0,
            session_rows: Vec::new(),
            delete_pending: None,
            diff_pending: Vec::new(),
            diff_in_flight: None,
            transcript_area: None,
            rendered: crate::ui::selection::Screen::default(),
            edit_request: None,
            loading_session: false,
            layout: crate::ui::View::default(),
            viewport: 1,
        }
    }

    // ----- reducer -------------------------------------------------------

    /// Applies one message; returns whether the frame must be redrawn.
    pub fn on_msg(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Agent(event) => self.on_agent_event(*event),
            Msg::AgentClosed(reason) => {
                if self.fatal.is_none() {
                    self.fatal = Some(Fatal {
                        reason,
                        stderr_tail: self.client.stderr_tail(),
                    });
                }
                true
            }
            Msg::Op(done) => self.on_op(*done),
            Msg::Ready(ready) => {
                let Ready {
                    models,
                    profiles,
                    sessions,
                } = *ready;
                self.models = models;
                self.profiles = profiles;
                self.sessions = sessions;
                self.ready = true;
                true
            }
            Msg::Sessions(sessions) => {
                self.sessions = sessions;
                true
            }
            Msg::Notice(level, text) => {
                self.notice(level, text);
                true
            }
            Msg::RetrySend => {
                self.waiting_for_compaction = false;
                self.flush_queue();
                true
            }
            Msg::DraftEdited(draft) => {
                // An empty file is a deliberate blank draft, not a cancel.
                self.composer.set_text(draft.as_deref().unwrap_or_default());
                true
            }
            Msg::Quit => {
                self.quit = true;
                true
            }
        }
    }

    /// Expires transient notices; returns whether the frame changed.
    pub fn tick(&mut self) -> bool {
        let before = self.notices.len();
        self.notices.retain(|notice| {
            notice.level == NoticeLevel::Error || notice.at.elapsed() < NOTICE_TTL
        });
        // How often this runs is the main loop's business; a busy turn needs a
        // clock tick, an idle app does not.
        self.notices.len() != before || self.is_busy() || self.compacting()
    }

    /// Whether a periodic tick is needed at all.
    pub fn wants_tick(&self) -> bool {
        self.is_busy()
            || self.compacting()
            || !self.notices.is_empty()
            || self.waiting_for_compaction
    }

    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        if level == NoticeLevel::Error {
            self.last_error = Some(text.clone());
        }
        self.notices.push_back(Notice {
            level,
            text,
            at: Instant::now(),
        });
        while self.notices.len() > 2 {
            self.notices.pop_front();
        }
    }

    pub fn error(&mut self, error: impl std::borrow::Borrow<RpcError>) {
        self.notice(NoticeLevel::Error, error.borrow().message.clone());
    }

    pub fn transcript(&self) -> Option<&Transcript> {
        self.session.as_ref().map(|session| &session.transcript)
    }

    pub fn is_busy(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_busy)
    }

    pub fn has_interaction(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.interaction.is_some())
    }

    pub fn compacting(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.compaction.is_some())
    }

    /// Status-line text while something is in flight.
    pub fn status_label(&self) -> Option<&'static str> {
        if let Some(session) = self.session.as_ref() {
            if session.compaction.is_some() {
                return Some("Compacting context");
            }
            if session.interaction.is_some() {
                return Some("Waiting for approval");
            }
            if let Some(label) = session.phase_label() {
                return Some(label);
            }
        }
        self.waiting_for_compaction
            .then_some("Waiting for compaction")
    }

    /// Elapsed time of the running turn, for the status line.
    pub fn busy_elapsed(&self) -> Option<Duration> {
        self.session
            .as_ref()
            .and_then(|session| session.live.as_ref())
            .map(|live| live.started.elapsed())
    }

    // ----- actions -------------------------------------------------------

    /// Executes one action; key bindings and slash commands both land here.
    pub fn on_action(&mut self, action: Action) -> bool {
        match action {
            Action::Submit => self.submit(),
            Action::QueueFollowUp => self.submit_follow_up(),
            Action::Send(text) => {
                self.composer.set_text(&text);
                self.submit()
            }
            Action::Interrupt => self.interrupt(),
            Action::OpenSession(id) => {
                self.loading_session = true;
                tasks::open_session(self.client.clone(), self.tx.clone(), id);
                true
            }
            Action::NewSession => {
                self.create_session();
                true
            }
            Action::ReloadSessions => {
                tasks::reload_sessions(self.client.clone(), self.tx.clone());
                true
            }
            Action::DeleteSession(id) => {
                tasks::delete_session(self.client.clone(), self.tx.clone(), id);
                true
            }
            Action::Rename(title) => {
                self.rename(title);
                true
            }
            Action::SetModel(id) => {
                self.update_session(Some(id), None);
                true
            }
            Action::SetReasoning(level) => {
                self.update_session(None, Some(level));
                true
            }
            Action::Compact => {
                self.compact();
                true
            }
            Action::CopyLastAnswer => {
                self.copy_last_answer();
                true
            }
            Action::Export(path) => {
                if let Some(session) = self.session.as_ref() {
                    let path =
                        path.or_else(|| Some(features::export_path(&session.info.session_id)));
                    tasks::export(
                        self.client.clone(),
                        self.tx.clone(),
                        session.info.session_id.clone(),
                        path,
                    );
                }
                true
            }
            Action::EditDraft => {
                self.edit_draft();
                true
            }
            Action::Answer(answer) => {
                self.answer(answer);
                true
            }
            Action::LoadOlder => {
                self.load_older();
                true
            }
            Action::RecallQueued => {
                self.recall_queued();
                true
            }
            Action::SetTheme(kind) => {
                self.apply_theme(kind);
                true
            }
            Action::ToggleTheme => {
                let next = match self.theme {
                    ThemeKind::Dark => ThemeKind::Light,
                    ThemeKind::Light => ThemeKind::Dark,
                };
                self.apply_theme(next);
                true
            }
            Action::ReloadAgent => {
                let client = self.client.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = client
                        .call::<serde_json::Value, crate::protocol::OkResult>(
                            crate::protocol::METHOD_RELOAD,
                            &serde_json::Value::Null,
                            false,
                        )
                        .await;
                    match result {
                        Ok(_) => {
                            let _ =
                                tx.send(Msg::Notice(NoticeLevel::Info, "Agent reloaded".into()));
                        }
                        Err(error) => {
                            let _ = tx.send(Msg::Notice(NoticeLevel::Error, error.message));
                        }
                    }
                });
                true
            }
            Action::Quit => {
                self.quit = true;
                true
            }
        }
    }

    /// Enter: send when idle, steer when a turn is running.
    fn submit(&mut self) -> bool {
        let text = self.composer.submit().trim_end().to_owned();
        if text.trim().is_empty() {
            return true;
        }
        if self.is_busy() {
            self.steer(text);
        } else {
            self.send(text);
        }
        true
    }

    /// Alt+Enter: queue a follow-up; when idle it just sends.
    fn submit_follow_up(&mut self) -> bool {
        let text = self.composer.submit().trim_end().to_owned();
        if text.trim().is_empty() {
            return true;
        }
        if self.is_busy() {
            if let Some(session) = self.session.as_mut() {
                session.push_follow_up(text);
            }
            true
        } else {
            self.send(text);
            true
        }
    }

    /// Switches the palette and persists the choice, so `/theme` survives a
    /// restart. A failed write is a notice, not a reason to keep the old theme.
    fn apply_theme(&mut self, kind: ThemeKind) {
        let mut config = crate::config::Config::load();
        config.theme = kind;
        if let Err(error) = config.save() {
            self.notice(NoticeLevel::Error, error);
        }
        self.theme = kind;
        self.palette = crate::theme::Palette::for_kind(kind);
        self.context_view = crate::ui::views::context_view(
            "Context",
            None,
            self.session
                .as_ref()
                .and_then(|session| session.presentation.as_ref()),
            &self.palette,
        );
    }

    fn send(&mut self, text: String) {
        let Some(session) = self.session.as_mut() else {
            self.composer.set_text(&text);
            self.notice(NoticeLevel::Error, "No session is open");
            return;
        };
        // The durable history echoes this prompt, so the turn's region starts
        // before it: reconciliation replaces it rather than adding a second.
        session.next_turn_start = Some(session.transcript.len());
        session.transcript.push(Block::User {
            text: text.clone(),
            kind: crate::protocol::read::UserKind::Prompt,
        });
        let session_id = session.info.session_id.clone();
        let epoch = session.epoch;
        self.scroll = Scroll::Follow;
        tasks::send_turn(
            self.client.clone(),
            self.tx.clone(),
            session_id,
            text,
            epoch,
        );
    }

    fn steer(&mut self, text: String) {
        let Some(session) = self.session.as_mut() else {
            self.composer.set_text(&text);
            return;
        };
        let Some(turn) = session.live.as_ref().map(|live| live.turn.clone()) else {
            self.composer.set_text(&text);
            return;
        };
        session.queue.push(Queued {
            text: text.clone(),
            kind: QueuedKind::Steer,
        });
        tasks::steer(self.client.clone(), self.tx.clone(), turn, text);
    }

    fn interrupt(&mut self) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if session.interaction.is_some() {
            // Esc on a prompt denies it: the turn must not stay blocked.
            self.answer(Answer::Deny);
            return true;
        }
        let Some(live) = session.live.as_mut() else {
            return false;
        };
        if live.interrupting {
            return false;
        }
        live.interrupting = true;
        let turn = live.turn.clone();
        tasks::cancel_turn(self.client.clone(), self.tx.clone(), turn);
        true
    }

    /// Esc: close the composer's state, then interrupt. Returns whether the key
    /// was consumed.
    pub fn escape(&mut self) -> bool {
        if self.is_busy() {
            return self.interrupt();
        }
        if !self.composer.is_empty() {
            self.composer.clear();
            return true;
        }
        false
    }

    fn create_session(&mut self) {
        self.loading_session = true;
        // An absent profile means "the Agent's configured default". Choosing
        // one here would override it with whatever sorts first.
        let mut params = serde_json::json!({ "workspace": self.workspace });
        if let Some(profile) = &self.launch.profile {
            params["profile"] = serde_json::json!(profile);
        }
        if let Some(model) = &self.launch.model {
            params["model"] = serde_json::json!(model);
        }
        if let Some(reasoning) = self.launch.reasoning {
            params["reasoning"] = serde_json::json!(reasoning);
        }
        tasks::create_session(self.client.clone(), self.tx.clone(), params);
    }

    fn rename(&mut self, title: String) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        tasks::rename(
            self.client.clone(),
            self.tx.clone(),
            session.info.session_id.clone(),
            title.to_owned(),
        );
    }

    fn update_session(
        &mut self,
        model: Option<String>,
        reasoning: Option<crate::protocol::Reasoning>,
    ) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let session_id = session.info.session_id.clone();
        let mut params = serde_json::json!({ "session_id": session_id });
        if let Some(model) = model {
            params["model"] = serde_json::json!(model);
        }
        if let Some(reasoning) = reasoning {
            params["reasoning"] = serde_json::json!(reasoning);
        }
        tasks::update_session(self.client.clone(), self.tx.clone(), params, session_id);
    }

    fn compact(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if session.is_busy() {
            self.notice(NoticeLevel::Error, "Finish the running turn first");
            return;
        }
        let session_id = session.info.session_id.clone();
        self.op_seq += 1;
        let operation_id = format!("tui-{}", self.op_seq);
        if let Some(session) = self.session.as_mut() {
            session.compaction = Some(CompactionProgress {
                operation_id: operation_id.clone(),
                phase: CompactionPhase::Preparing,
                covered_item_count: 0,
                retained_item_count: 0,
            });
        }
        tasks::compact(
            self.client.clone(),
            self.tx.clone(),
            session_id,
            operation_id,
        );
    }

    fn copy_last_answer(&mut self) {
        let text = self
            .session
            .as_ref()
            .and_then(|session| session.transcript.last_answer())
            .map(str::to_owned)
            .or_else(|| self.last_answer.clone());
        let Some(text) = text else {
            self.notice(NoticeLevel::Error, "No answer to copy yet");
            return;
        };
        self.copy(text);
    }

    /// Ctrl+G: hand the draft to the external editor. The terminal belongs to
    /// the main loop, so this only files the request; the loop suspends the
    /// screen, runs the editor, and reports back through [`Msg::DraftEdited`].
    fn edit_draft(&mut self) {
        let draft = self.composer.submit();
        self.edit_request = Some(draft);
    }

    /// Takes the draft the main loop must edit, if Ctrl+G asked for it.
    pub fn take_edit_request(&mut self) -> Option<String> {
        self.edit_request.take()
    }

    /// The editor command: config first, then `$VISUAL`/`$EDITOR`.
    pub fn editor_command(&self) -> Result<Vec<String>, String> {
        crate::config::Config::load().editor().ok_or_else(|| {
            "No editor configured: set `editor` in the TUI config or $VISUAL/$EDITOR".to_owned()
        })
    }

    fn answer(&mut self, answer: Answer) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(interaction) = session.interaction.take() else {
            return;
        };
        let session_id = session.info.session_id.clone();
        let is_approval = matches!(interaction.kind, InteractionKind::Approval);
        let wire = match (answer, is_approval) {
            (Answer::Allow, _) => serde_json::json!({"type": "approval", "decision": "allow_once"}),
            (Answer::Deny, _) => serde_json::json!({"type": "approval", "decision": "deny"}),
            (Answer::Choice(index), true) => {
                if index == 0 {
                    serde_json::json!({"type": "approval", "decision": "allow_once"})
                } else {
                    serde_json::json!({"type": "approval", "decision": "deny"})
                }
            }
            (Answer::Choice(index), false) => serde_json::json!({"type": "choice", "index": index}),
            (Answer::Text(text), _) => serde_json::json!({"type": "text", "text": text}),
        };
        let params = serde_json::json!({
            "session_id": session_id,
            "loop_id": interaction.turn.loop_id,
            "interaction_id": interaction.interaction_id,
            "answer": wire,
        });
        tasks::answer(self.client.clone(), self.tx.clone(), params, session_id);
    }

    fn load_older(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.loading_older || session.loaded_from == 0 {
            return;
        }
        session.loading_older = true;
        // Below the display floor the transcript continues in the canonical
        // history, behind the compaction summary. That is a different read with
        // its own boundary, so it must not be asked for as a display page.
        let below_floor = session.loaded_from <= session.first_item;
        let before = session.loaded_from;
        let session_id = session.info.session_id.clone();
        if below_floor {
            session.reached_floor = true;
            tasks::load_canonical(self.client.clone(), self.tx.clone(), session_id, before);
        } else {
            tasks::load_older(self.client.clone(), self.tx.clone(), session_id, before);
        }
    }

    /// Puts the oldest queued follow-up back in the composer. A draft already
    /// in the composer is never overwritten: the queue item stays where it is
    /// until there is room for it.
    /// Asks for a fresh context ledger: the view must never show a stale one.
    pub fn refresh_context(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        tasks::context(
            self.client.clone(),
            self.tx.clone(),
            session.info.session_id.clone(),
        );
    }

    fn recall_queued(&mut self) {
        if !self.composer.is_empty() {
            self.notice(
                NoticeLevel::Info,
                "Clear the draft before recalling a queued message",
            );
            return;
        }
        let Some(text) = self
            .session
            .as_mut()
            .and_then(|session| session.take_follow_up())
        else {
            return;
        };
        self.composer.set_text(&text);
    }

    /// Returns submitted text to the composer: appended to a draft the user
    /// typed meanwhile rather than replacing it, and one undo step either way.
    fn restore_draft(&mut self, text: &str) {
        if self.composer.is_empty() {
            self.composer.set_text(text);
        } else {
            self.composer.insert(text);
        }
    }

    /// Sends the next queued follow-up when the Agent is free again.
    pub fn flush_queue(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Some(text) = session.take_follow_up() {
            self.send(text);
        }
    }

    /// Moves the transcript view by `delta` rows, switching to an anchored
    /// position so new output cannot move what is being read. `viewport` is the
    /// transcript height the caller knows about; the layout's own record of the
    /// last frame is used when it is taller.
    pub fn scroll_by(&mut self, delta: isize, viewport: usize) {
        if self
            .session
            .as_ref()
            .is_none_or(|session| session.transcript.is_empty())
        {
            return;
        }
        // The layout owns row addressing; the reducer only decides the anchor.
        let layout = &self.layout.cache;
        let total = layout.total_rows();
        let height = layout.viewport.height.max(viewport).max(1);
        if total <= height {
            // Nothing to scroll, but the head of the loaded window may still
            // be reachable: a short display page is exactly when the older
            // history is worth asking for.
            if delta < 0 {
                self.load_older();
            }
            return;
        }
        let max_top = total - height;
        let top = match self.scroll {
            Scroll::Follow => max_top,
            Scroll::Anchor { block, offset } => layout.anchor_row(block, offset).min(max_top),
        };
        let next = (top as isize + delta).clamp(0, max_top as isize) as usize;
        if next == max_top {
            self.scroll = Scroll::Follow;
            return;
        }
        let (block, offset) = layout.anchor(next);
        self.scroll = Scroll::Anchor { block, offset };
        // Reaching the first block asks for the next page; the request is
        // skipped while one is in flight, so holding the key is harmless.
        if delta < 0 && block <= 1 {
            self.load_older();
        }
    }

    /// The drag as text, sliced from the rows the last frame actually drew.
    /// The renderer stores those rows, so the copy matches what is on screen
    /// even while a turn is streaming.
    pub fn selected_text(&self, drag: crate::ui::selection::Drag) -> Option<String> {
        let area = self.transcript_area?;
        let text = crate::ui::selection::text(&self.rendered, area, drag);
        (!text.is_empty()).then_some(text)
    }

    /// Copies whatever the open view shows: the diff, the tool output, the
    /// conversation text.
    pub fn copy_view(&mut self) -> crate::app::keys::Input {
        let text = match &self.overlay {
            Overlay::Diff { .. } => crate::ui::views::diff_text(&self.diff),
            Overlay::Tool(_) => self.tool_view.text(),
            Overlay::Context => view_text(&self.context_view.lines),
            Overlay::Help => String::new(),
            _ => self
                .session
                .as_ref()
                .map(|session| view_text_blocks(session.transcript.blocks()))
                .unwrap_or_default(),
        };
        if text.trim().is_empty() {
            self.notice(NoticeLevel::Info, "Nothing to copy here");
            return crate::app::keys::Input::Consumed;
        }
        self.copy(text);
        crate::app::keys::Input::Consumed
    }

    /// Copies `text` through the clipboard task.
    pub fn copy(&mut self, text: String) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = clipboard::copy(&text).await;
            let _ = tx.send(Msg::Op(Box::new(OpDone {
                session: None,
                epoch: 0,
                kind: OpKind::Copied(result),
            })));
        });
    }

    /// Tab in the diff view: turn -> session -> workspace -> turn.
    pub fn diff_cycle_scope(&mut self) {
        let Overlay::Diff {
            scope,
            file,
            scroll,
        } = &mut self.overlay
        else {
            return;
        };
        *scope = match scope {
            DiffScope::Turn => DiffScope::Session,
            DiffScope::Session => DiffScope::Workspace,
            DiffScope::Workspace => DiffScope::Turn,
        };
        *file = 0;
        *scroll = 0;
        let scope = *scope;
        self.open_diff(scope);
    }

    /// Rows the transcript occupies, as the last frame saw it.
    pub fn viewport_rows(&self) -> usize {
        self.viewport.max(1)
    }

    // ----- operations ----------------------------------------------------

    fn on_op(&mut self, done: OpDone) -> bool {
        let OpDone {
            session,
            epoch,
            kind,
        } = done;
        // Results that are not tied to the resident session apply directly.
        match kind {
            OpKind::Copied(result) => match result {
                Ok(crate::clipboard::Copied::Native) => self.notice(NoticeLevel::Info, "Copied"),
                Ok(crate::clipboard::Copied::Osc52) => {
                    self.notice(NoticeLevel::Info, "Copied with OSC 52")
                }
                Err(error) => self.notice(NoticeLevel::Error, error),
            },
            OpKind::Created(result) => self.on_created(result),
            OpKind::Deleted(id, result) => match result {
                Ok(()) => {
                    self.sessions.retain(|session| session.session_id != id);
                    self.notice(NoticeLevel::Info, "Session deleted");
                }
                Err(error) => self.error(error),
            },
            OpKind::Exported(result) => match result {
                Ok(path) => self.notice(NoticeLevel::Info, format!("Exported to {path}")),
                Err(error) => self.notice(NoticeLevel::Error, error),
            },
            OpKind::Opened(result) => self.on_opened(result),
            kind => {
                let Some(resident) = self.session.as_ref() else {
                    return false;
                };
                if session.as_deref() != Some(resident.info.session_id.as_str()) {
                    return false;
                }
                // A result produced before this session was reopened is stale.
                if epoch != resident.epoch && matches!(kind, OpKind::Waited(_) | OpKind::Sent(..)) {
                    return false;
                }
                return self.on_session_op(&kind);
            }
        }
        true
    }

    fn on_session_op(&mut self, kind: &OpKind) -> bool {
        match kind {
            OpKind::History(result) => self.on_history(result),
            OpKind::Older(result) => self.on_older(result),
            OpKind::Canonical(result) => self.on_canonical(result),
            OpKind::TurnItems(result) => self.on_turn_items(result),
            OpKind::State(result) => self.on_state(result),
            OpKind::IdleCheck(result) => self.on_idle_check(result),
            OpKind::Presentation(result) => self.on_presentation(result),
            OpKind::Context(result) => self.on_context(result),
            OpKind::Sent(text, result) => self.on_sent(text, result),
            OpKind::Waited(result) => self.on_waited(result),
            OpKind::Steered(result) => self.on_steered(result),
            OpKind::Cancelled(result) => self.on_cancelled(result),
            OpKind::Answered(result) => {
                if let Err(error) = result {
                    self.error(error);
                }
                true
            }
            OpKind::Compacted(result) => self.on_compacted(result),
            OpKind::Updated(result) | OpKind::Renamed(result) => self.on_session_info(result),
            OpKind::Changes(loop_id, result) => self.on_changes(loop_id, result),
            OpKind::ToolRead(key, result) => self.on_tool_read(key, result),
            OpKind::ToolOutput(key, stream, result) => match result {
                Ok(text) => {
                    if let Some(call) = self
                        .session
                        .as_mut()
                        .and_then(|session| session.transcript.find_tool_mut(key))
                    {
                        call.set_full_output(stream, text.clone());
                    }
                    true
                }
                Err(error) => {
                    self.error(error);
                    true
                }
            },
            OpKind::Files(result) => self.on_files(result),
            OpKind::ChangeFiles(scope, result) => self.on_change_files(*scope, result),
            OpKind::DiffFile(path, result) => self.on_diff_file(path, result),
            OpKind::Copied(_)
            | OpKind::Created(_)
            | OpKind::Deleted(..)
            | OpKind::Exported(_)
            | OpKind::Opened(_) => unreachable!("handled before the session dispatch"),
        }
    }

    fn on_opened(&mut self, result: Result<SessionInfo, RpcError>) {
        match result {
            Ok(info) => self.adopt(info, None),
            Err(error) => {
                self.loading_session = false;
                self.error(error);
            }
        }
    }

    fn on_created(&mut self, result: Result<SessionInfo, RpcError>) {
        match result {
            Ok(info) => self.adopt(info, Some(())),
            Err(error) => {
                self.loading_session = false;
                self.error(error);
            }
        }
    }

    /// Whether a session request is still in flight.
    pub fn session_loading(&self) -> bool {
        self.loading_session
    }

    /// Makes `info` the resident session and loads its tail.
    fn adopt(&mut self, info: SessionInfo, created: Option<()>) {
        self.loading_session = false;
        let mut session = Session::new(info);
        session.epoch = self.session.as_ref().map_or(0, |old| old.epoch) + 1;
        let epoch = session.epoch;
        let session_id = session.info.session_id.clone();
        if !self
            .sessions
            .iter()
            .any(|known| known.session_id == session_id)
        {
            self.sessions.push(session.info.clone());
        }
        self.session = Some(session);
        self.scroll = Scroll::Follow;
        self.waiting_for_compaction = false;
        self.merge_session_info_into_list();
        tasks::load_tail(self.client.clone(), self.tx.clone(), session_id.clone());
        tasks::presentation(
            self.client.clone(),
            self.tx.clone(),
            session_id.clone(),
            epoch,
        );
        tasks::context(self.client.clone(), self.tx.clone(), session_id);
        // A new session already carries its settings; an opened one needs them
        // applied, because the flags describe how to run this session now.
        let (model, reasoning) = (self.launch.model.clone(), self.launch.reasoning);
        if created.is_none() && (model.is_some() || reasoning.is_some()) {
            self.update_session(model, reasoning);
        }
        self.launch = LaunchSettings::default();
    }

    fn merge_session_info_into_list(&mut self) {
        if let Some(session) = self.session.as_ref()
            && let Some(known) = self
                .sessions
                .iter_mut()
                .find(|known| known.session_id == session.info.session_id)
        {
            *known = session.info.clone();
        }
    }

    fn on_history(&mut self, result: &Result<tasks::History, RpcError>) -> bool {
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                self.error(error);
                return true;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        session.transcript.clear();
        session.first_item = page.floor;
        session.loaded_from = page.from;
        session.hidden_items = page.from;
        install_items(session, &page.items);
        // `from` is what is actually behind this window. `has_more` only speaks
        // for the display projection, and an automatic compaction can cover
        // every item, leaving a transcript that would otherwise claim there is
        // nothing to read while the history is still there.
        if page.from > 0 {
            session.transcript.insert(
                0,
                Block::EarlierHistory {
                    hidden_items: page.from,
                },
            );
        }
        session.sync_session_id_into_tools();
        self.scroll = Scroll::Follow;
        true
    }

    fn on_older(&mut self, result: &Result<tasks::History, RpcError>) -> bool {
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                if error.code == crate::protocol::INVALID_STATE {
                    // The task already retried once against a fresh pin, so the
                    // prefix is beyond repair right now; keep what is loaded
                    // instead of dropping the reader's view.
                    self.notice(
                        NoticeLevel::Info,
                        "Could not load older history yet; try again in a moment",
                    );
                } else {
                    self.error(error);
                }
                if let Some(session) = self.session.as_mut() {
                    session.loading_older = false;
                }
                return true;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        session.loading_older = false;
        // Older items go in front of the current transcript, after the
        // "earlier history" placeholder that they replace.
        let mut blocks = Vec::new();
        install_items_into(&mut blocks, &page.items);
        let skip = usize::from(matches!(
            session.transcript.blocks().first(),
            Some(Block::EarlierHistory { .. })
        ));
        if blocks.is_empty() && skip == 0 {
            // Nothing came back and nothing was replaced: the reader is at the
            // boundary, so stop asking rather than looping on the same request.
            session.loaded_from = 0;
            return true;
        }
        session.transcript.remove_front(skip);
        let added = blocks.len();
        for block in blocks.into_iter().rev() {
            session.transcript.insert(0, block);
        }
        let mut prefix = added.saturating_sub(skip);
        // Prepending must not move what the user is reading: the anchor block
        // keeps its identity, so its index shifts by what was inserted (minus
        // the placeholder that was replaced).
        if let Scroll::Anchor { block, offset } = self.scroll {
            let block = block + added;
            self.scroll = Scroll::Anchor { block, offset };
        }
        // A compaction can raise the floor above where the reader is. The rows on
        // screen still reach down to what was loaded, so the read position stays
        // there: moving it to the new floor would skip the items in between once
        // the next below-floor read asks the canonical view.
        // Rows on screen reach down to what was already loaded, so the read
        // position never moves up past it.
        session.loaded_from = page.from.min(session.loaded_from);
        session.first_item = page.floor;
        session.hidden_items = session.loaded_from;
        // A page reaching the display floor leaves the reader at the compaction
        // summary with more history behind it; otherwise the label just states
        // how much older history remains unloaded.
        if page.has_more || session.loaded_from <= session.first_item {
            session.transcript.insert(
                0,
                Block::EarlierHistory {
                    hidden_items: page.from,
                },
            );
            prefix += 1;
        }
        session.rebase_after_prepend(prefix);
        // Blocks built from a page carry an empty session id; the tool views
        // read it back when a card is opened.
        session.sync_session_id_into_tools();
        true
    }

    /// Prepends a canonical page read from below the display floor. The page is
    /// raw history the display projection collapsed away, so the summary
    /// placeholder stays and the items go below it.
    fn on_canonical(&mut self, result: &Result<tasks::History, RpcError>) -> bool {
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                if let Some(session) = self.session.as_mut() {
                    session.loading_older = false;
                }
                if error.code == crate::protocol::INVALID_STATE {
                    // Retried already; the prefix is beyond repair for now, so
                    // keep what is loaded instead of dropping the reader's view.
                    self.notice(
                        NoticeLevel::Info,
                        "Could not load older history yet; try again in a moment",
                    );
                } else {
                    self.error(error);
                }
                return true;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        session.loading_older = false;
        let mut blocks = Vec::new();
        install_items_into(&mut blocks, &page.items);
        let added = blocks.len();
        if added == 0 {
            // Nothing older than this page: stop asking.
            session.loaded_from = 0;
            return true;
        }
        for block in blocks.into_iter().rev() {
            session.transcript.insert(0, block);
        }
        if let Scroll::Anchor { block, offset } = self.scroll {
            self.scroll = Scroll::Anchor {
                block: block + added,
                offset,
            };
        }
        session.rebase_after_prepend(added);
        session.loaded_from = page.from;
        session.hidden_items = page.from;
        session.sync_session_id_into_tools();
        true
    }

    fn on_turn_items(
        &mut self,
        result: &Result<Vec<(usize, crate::protocol::read::RawHistoryItem)>, RpcError>,
    ) -> bool {
        let items = match result {
            Ok(items) => items,
            Err(error) => {
                self.error(error);
                return true;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if !session.reconcile_turn(items) {
            return false;
        }
        true
    }

    fn on_state(&mut self, result: &Result<SessionState, RpcError>) -> bool {
        let state = match result {
            Ok(state) => state,
            Err(error) => {
                self.error(error);
                return true;
            }
        };
        let idle = state.accepts_turn();
        let active_loop = state.active_loop.clone();
        if let Some(session) = self.session.as_mut() {
            session.apply_state(state);
        }
        if idle {
            self.waiting_for_compaction = false;
            if self
                .session
                .as_ref()
                .is_some_and(|session| !session.queue.is_empty())
            {
                self.flush_queue();
            }
        } else if self.waiting_for_compaction {
            // Post-turn compaction still owns the session: keep asking until it
            // settles, then the queue flushes. A single poll would latch the UI.
            self.retry_when_idle();
        }
        // A loop this process did not start has no `turn.wait` of its own.
        if let Some(loop_id) = active_loop.map(|state| state.loop_id)
            && let Some(session) = self.session.as_ref()
            && !session
                .live
                .as_ref()
                .is_some_and(|live| live.turn.loop_id == loop_id)
        {
            self.adopt_loop(session.info.session_id.clone(), loop_id);
        }
        true
    }

    /// Attaches to a loop that is already running, so its events and its end
    /// are observed through the same `turn.wait` path as one this app started.
    fn adopt_loop(&mut self, session_id: String, loop_id: String) {
        let epoch = self.session.as_ref().map_or(0, |session| session.epoch);
        let turn = TurnRef {
            session_id,
            loop_id,
        };
        if let Some(session) = self.session.as_mut()
            && session.live.is_none()
        {
            let start = session
                .next_turn_start
                .take()
                .unwrap_or_else(|| session.transcript.len());
            session.live = Some(LiveTurn::new(turn.clone(), start));
        }
        self.scroll = Scroll::Follow;
        tasks::wait_turn(self.client.clone(), self.tx.clone(), turn, epoch);
    }

    fn on_idle_check(&mut self, result: &Result<bool, RpcError>) -> bool {
        match result {
            Ok(true) => {
                self.waiting_for_compaction = false;
                self.flush_queue();
            }
            Ok(false) => {
                // Still compacting: check again shortly.
                let tx = self.tx.clone();
                let client = self.client.clone();
                let Some(session_id) = self
                    .session
                    .as_ref()
                    .map(|session| session.info.session_id.clone())
                else {
                    return false;
                };
                let epoch = self.session.as_ref().map_or(0, |session| session.epoch);
                tokio::spawn(async move {
                    tokio::time::sleep(BUSY_RETRY).await;
                    tasks::session_state(client, tx, session_id, epoch);
                });
            }
            Err(error) => self.error(error),
        }
        true
    }

    fn on_presentation(&mut self, result: &Result<SessionPresentation, RpcError>) -> bool {
        let presentation = match result {
            Ok(presentation) => presentation,
            Err(error) => {
                self.error(error);
                return true;
            }
        };
        if let Some(session) = self.session.as_mut() {
            session.presentation = Some(presentation.clone());
        }
        true
    }

    fn on_context(&mut self, result: &Result<SessionContext, RpcError>) -> bool {
        match result {
            Ok(context) => {
                if let Some(session) = self.session.as_mut() {
                    session.context = context.budget.clone();
                    if let Some(progress) = &context.current_operation {
                        session.compaction = Some(progress.clone());
                    } else if session.compaction.is_some() {
                        session.compaction = None;
                    }
                    // An automatic compaction has no response of its own: the
                    // context view is where it is reported. Recording it here
                    // makes it appear once, whether it was seen live or on open.
                    if let Some(result) = &context.last_result {
                        let operation_id = match &result.origin {
                            Some(origin) if origin == "auto" => {
                                format!("auto-{}", result.operation_id)
                            }
                            _ => result.operation_id.clone(),
                        };
                        session.push_compaction(&operation_id, result);
                    }
                }
                let context = context.clone();
                if let Some(session) = self.session.as_ref() {
                    self.context_view = crate::ui::views::context_view(
                        "Context",
                        Some(&context),
                        session.presentation.as_ref(),
                        &self.palette,
                    );
                }
                true
            }
            Err(error) => {
                self.error(error);
                true
            }
        }
    }

    fn on_sent(&mut self, text: &str, result: &Result<TurnRef, RpcError>) -> bool {
        match result {
            Ok(turn) => {
                if let Some(session) = self.session.as_mut()
                    && session.live.is_none()
                {
                    let start = session
                        .next_turn_start
                        .take()
                        .unwrap_or_else(|| session.transcript.len());
                    session.live = Some(LiveTurn::new(turn.clone(), start));
                }
                self.title_from_first_prompt();
                true
            }
            Err(error) if error.is_busy() => {
                // Post-turn compaction still owns the session, so this turn was
                // refused, not sent. Take back the optimistic prompt and queue
                // the text: it must neither look sent nor be lost. Poll until
                // the Agent is idle; never resend blindly.
                self.waiting_for_compaction = true;
                if let Some(session) = self.session.as_mut() {
                    session.unsend_prompt(text);
                    session.push_follow_up(text.to_owned());
                }
                self.retry_when_idle();
                true
            }
            Err(error) => {
                // Any other refusal never reached the Agent. The text goes back
                // without erasing anything typed while the send was in flight.
                if let Some(session) = self.session.as_mut() {
                    session.unsend_prompt(text);
                }
                self.restore_draft(text);
                self.error(error);
                true
            }
        }
    }

    fn retry_when_idle(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let session_id = session.info.session_id.clone();
        let epoch = session.epoch;
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(BUSY_RETRY).await;
            tasks::session_state(client, tx, session_id, epoch);
        });
    }

    fn on_waited(&mut self, result: &Result<crate::protocol::TurnWaitResult, RpcError>) -> bool {
        let wait = match result {
            Ok(wait) => wait,
            // The deferred-slot and busy refusals are conditions, not verdicts:
            // the turn is still running, so register the wait again shortly
            // instead of leaving a live turn nothing will ever close.
            Err(error) if error.is_capacity() || error.is_busy() => {
                let Some((turn, epoch)) = self.session.as_ref().and_then(|session| {
                    session
                        .live
                        .as_ref()
                        .map(|live| (live.turn.clone(), session.epoch))
                }) else {
                    self.error(error);
                    return true;
                };
                let client = self.client.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(BUSY_RETRY).await;
                    tasks::wait_turn(client, tx, turn, epoch);
                });
                return true;
            }
            Err(error) => {
                self.error(error);
                return true;
            }
        };
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let loop_id = wait.turn.loop_id.clone();
        let Some(live) = session.live.as_ref() else {
            return false;
        };
        let epoch = session.epoch;
        let start = live.start;
        let gap = live.gap;
        let duration = Some(live.started.elapsed());
        let requests = wait.requests.unwrap_or(live.requests);
        let tool_rounds = wait.tool_rounds.unwrap_or(live.tool_rounds);
        let usage = wait.usage.clone().unwrap_or_default();
        let completed = wait.outcome.is_completed() && !gap;
        let session = self.session.as_mut().expect("session was present");
        session.live = None;
        session.abandon_unapplied_steers();
        session.finish_streaming();
        session.finish_turn(
            &loop_id,
            wait.outcome.clone(),
            usage.clone(),
            requests,
            tool_rounds,
            duration,
        );
        if !completed {
            // Events were dropped, so the streamed turn may be incomplete.
            // Its durable items replace that region when they arrive; the
            // region is remembered here because `live` is gone by then.
            session.reconcile_from = Some((loop_id.clone(), start));
            let turn = wait.turn.clone();
            tasks::load_turn_result(self.client.clone(), self.tx.clone(), turn, epoch);
        }
        if let Some(answer) = self
            .session
            .as_ref()
            .and_then(|session| session.transcript.last_answer())
        {
            self.last_answer = Some(answer.to_owned());
        }
        if let Some(session) = self.session.as_ref() {
            tasks::load_changes(
                self.client.clone(),
                self.tx.clone(),
                session.info.session_id.clone(),
                loop_id.clone(),
            );
            tasks::presentation(
                self.client.clone(),
                self.tx.clone(),
                session.info.session_id.clone(),
                epoch,
            );
            // The Agent compacts automatically at the context trigger with no
            // response of its own, and `session.context` is the only place that
            // result is published. Reading it after the turn is what makes an
            // automatic compaction visible as a marker and keeps the footer's
            // context figure honest.
            tasks::context(
                self.client.clone(),
                self.tx.clone(),
                session.info.session_id.clone(),
            );
        }
        self.scroll = Scroll::Follow;
        self.flush_queue();
        true
    }

    fn on_steered(&mut self, result: &Result<u64, RpcError>) -> bool {
        match result {
            Ok(_index) => {}
            Err(error) if error.code == crate::protocol::TURN_NOT_FOUND => {
                // The turn ended before the steer landed: keep the text.
                if let Some(session) = self.session.as_mut() {
                    session.abandon_unapplied_steers();
                }
                self.notice(NoticeLevel::Info, "Turn already ended; queued as next");
            }
            Err(error) if error.code == crate::protocol::STEER_QUEUE_FULL => {
                if let Some(session) = self.session.as_mut()
                    && let Some(index) = session
                        .queue
                        .iter()
                        .rposition(|queued| queued.kind == QueuedKind::Steer)
                {
                    let queued = session.queue.remove(index);
                    session.queue.push(Queued {
                        text: queued.text,
                        kind: QueuedKind::Follow,
                    });
                }
                self.notice(
                    NoticeLevel::Error,
                    "Steer queue is full; queued for the next turn",
                );
            }
            Err(error) => self.error(error),
        }
        true
    }

    fn on_cancelled(&mut self, result: &Result<(), RpcError>) -> bool {
        if let Err(error) = result {
            self.error(error);
        }
        true
    }

    fn on_compacted(
        &mut self,
        result: &Result<crate::protocol::CompactionResult, RpcError>,
    ) -> bool {
        if let Some(session) = self.session.as_mut() {
            session.compaction = None;
        }
        match result {
            Ok(result) => {
                use crate::protocol::CompactStatus;
                match result.status {
                    CompactStatus::Compacted => {
                        // The marker is added to what is already loaded. The
                        // summary is a new snapshot beside the history, so the
                        // blocks on screen stay: a reload would erase them.
                        if let Some(session) = self.session.as_mut() {
                            session.push_compaction(&result.operation_id.clone(), result);
                        }
                        if let Some(session) = self.session.as_ref() {
                            tasks::presentation(
                                self.client.clone(),
                                self.tx.clone(),
                                session.info.session_id.clone(),
                                session.epoch,
                            );
                        }
                    }
                    CompactStatus::Noop => {
                        self.notice(NoticeLevel::Info, "Nothing to compact yet");
                    }
                    CompactStatus::Failed => self.notice(
                        NoticeLevel::Error,
                        format!(
                            "Compaction failed: {}",
                            result.failure_kind.as_deref().unwrap_or("no progress")
                        ),
                    ),
                    CompactStatus::UnknownWrite | CompactStatus::Unknown => self.notice(
                        NoticeLevel::Error,
                        "Compaction result is unverified; reopen the session to reconcile",
                    ),
                }
                true
            }
            Err(error) => {
                self.error(error);
                true
            }
        }
    }

    fn on_session_info(&mut self, result: &Result<SessionInfo, RpcError>) -> bool {
        match result {
            Ok(info) => {
                if let Some(session) = self.session.as_mut() {
                    session.info = info.clone();
                }
                self.merge_session_info_into_list();
                true
            }
            Err(error) => {
                self.error(error);
                true
            }
        }
    }

    /// Installs the `@` picker's candidate paths and keeps the overlay's
    /// selection valid for the new list.
    fn on_files(&mut self, result: &Result<Vec<String>, RpcError>) -> bool {
        match result {
            Ok(paths) => {
                crate::ui::files::set_candidates(paths.clone());
                if let Overlay::Files { filter, selected } = &mut self.overlay {
                    // A stale selection would point past the new list.
                    *selected =
                        (*selected).min(crate::ui::files::filter_count(filter).saturating_sub(1));
                }
                true
            }
            Err(error) => {
                self.error(error);
                true
            }
        }
    }

    /// Fill the diff view's file column, then ask for the selected file's
    /// hunks.
    fn on_change_files(
        &mut self,
        scope: DiffScope,
        result: &Result<Vec<crate::protocol::ChangeRecord>, RpcError>,
    ) -> bool {
        let records = match result {
            Ok(records) => records.clone(),
            Err(error) => {
                self.diff.status = error.message.clone();
                return true;
            }
        };
        // One row per path: a turn can change the same file several times, and
        // repeating the row would misreport how many files changed. A row keeps
        // its measurement only while its records are the same set: a new record
        // on the same path changes the diff, so the old count would be stale.
        let previous = std::mem::take(&mut self.diff.files);
        let mut files: Vec<crate::ui::views::DiffFile> = Vec::new();
        for record in &records {
            match files.iter_mut().find(|file| file.path == record.path) {
                Some(file) => file.change_refs.push(record.change_ref.clone()),
                None => {
                    let measured = previous
                        .iter()
                        .find(|file| file.path == record.path)
                        .filter(|file| file.change_refs == [record.change_ref.clone()]);
                    files.push(crate::ui::views::DiffFile {
                        path: record.path.clone(),
                        kind: record.kind,
                        lines: measured.and_then(|file| file.lines),
                        failed: measured.is_some_and(|file| file.failed),
                        change_refs: vec![record.change_ref.clone()],
                    });
                }
            }
        }
        self.diff.set_files(files);
        if self.diff.files.is_empty() {
            self.diff.status = "No file changes are recorded for this scope.".into();
            return true;
        }
        self.diff.status.clear();
        // A row that has never been read says so in its line column, so the
        // list would carry unknowns until each is visited. Measure them all
        // once; a row that comes back is not read again. The selected row is
        // first in the queue, so its read paints the pane too.
        self.diff_pending.clear();
        self.diff_in_flight = None;
        let needs_counts: Vec<(DiffQuery, Vec<String>)> = self
            .diff
            .files
            .iter()
            .filter(|file| file.lines.is_none() && !file.failed && !file.change_refs.is_empty())
            .map(|file| {
                (
                    DiffQuery {
                        scope,
                        path: file.path.clone(),
                    },
                    file.change_refs.clone(),
                )
            })
            .collect();
        self.diff_pending = needs_counts;
        self.measure_next_diff_row();
        if self.diff_in_flight.is_none() {
            // Every row was already measured: try to paint the selected one.
            self.load_selected_diff();
        }
        true
    }

    /// Starts the next unmeasured row's read, one at a time so the Agent's
    /// query allowance is not spent all at once.
    fn measure_next_diff_row(&mut self) {
        if self.diff_in_flight.is_some() {
            return;
        }
        let Some((query, refs)) = self.diff_pending.first().cloned() else {
            return;
        };
        self.diff_pending.remove(0);
        self.start_diff_read(query, refs);
    }

    /// Loads the selected file's records. Every record of that path is paged
    /// in and merged, so the diff reflects the whole turn or session. When the
    /// row still needs measuring, this read is its measurement too.
    fn load_selected_diff(&mut self) {
        let Some(file) = self.diff.files.get(self.diff.selected) else {
            return;
        };
        if file.change_refs.is_empty() {
            return;
        }
        let query = DiffQuery {
            scope: self.diff.scope,
            path: file.path.clone(),
        };
        if self.diff_in_flight.as_ref() == Some(&query) {
            return;
        }
        let refs = file.change_refs.clone();
        self.diff_pending.retain(|(pending, _)| *pending != query);
        self.start_diff_read(query, refs);
    }

    /// Posts one diff read and remembers it, so nothing reads the same row on
    /// top of itself.
    fn start_diff_read(&mut self, query: DiffQuery, refs: Vec<String>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        self.diff_in_flight = Some(query.clone());
        tasks::load_diff(
            self.client.clone(),
            self.tx.clone(),
            session.info.session_id.clone(),
            query,
            refs,
        );
    }

    fn on_diff_file(
        &mut self,
        query: &DiffQuery,
        result: &Result<crate::protocol::changes::DiffPage, RpcError>,
    ) -> bool {
        let shown = self.diff.scope == query.scope
            && self
                .diff
                .current()
                .is_some_and(|file| file.path == query.path);
        let file = self
            .diff
            .files
            .iter_mut()
            .find(|file| file.path == query.path);
        match result {
            Ok(page) => {
                let lines = crate::ui::views::hunk_lines(page, &self.palette);
                let (added, removed) = count_hunks(&lines);
                if let Some(file) = file {
                    file.lines = Some((added, removed));
                    file.failed = false;
                }
                if self.diff_in_flight.as_ref() == Some(query) {
                    self.diff_in_flight = None;
                }
                self.measure_next_diff_row();
                if shown {
                    self.diff.hunks = lines;
                    self.diff.scroll = 0;
                    // A record whose before and after content match has no
                    // hunks at all: say that instead of implying a diff.
                    self.diff.status = if page.hunks.is_empty() && !page.binary {
                        "No line changes in these records (content unchanged).".into()
                    } else {
                        String::new()
                    };
                }
            }
            Err(error) => {
                if let Some(file) = file {
                    file.failed = true;
                }
                if self.diff_in_flight.as_ref() == Some(query) {
                    self.diff_in_flight = None;
                }
                self.measure_next_diff_row();
                if shown {
                    self.diff.hunks.clear();
                    self.diff.status = error.message.clone();
                }
            }
        }
        true
    }

    fn on_changes(&mut self, loop_id: &str, result: &Result<ChangeSummary, RpcError>) -> bool {
        let summary = match result {
            Ok(summary) => summary,
            Err(_) => return false,
        };
        if let Some(session) = self.session.as_mut() {
            for block in session.transcript.blocks_mut() {
                if let Block::TurnEnd {
                    loop_id: block_loop,
                    changes,
                    ..
                } = block
                    && block_loop == loop_id
                {
                    *changes = Some(*summary);
                }
            }
        }
        true
    }

    fn on_tool_read(
        &mut self,
        key: &ToolRef,
        result: &Result<crate::protocol::tool::ToolReadResult, RpcError>,
    ) -> bool {
        match result {
            Ok(read) => {
                let Some(session) = self.session.as_mut() else {
                    return false;
                };
                if let Some(call) = session.transcript.find_tool_mut(key) {
                    if let Some(display) = &read.display {
                        call.apply_display(display);
                    }
                    if let Some(invocation) = &read.invocation {
                        call.detail = invocation.detail();
                        call.name = invocation.name.clone();
                    }
                    if let Some(execution) = &read.execution {
                        call.set_state(ToolState::from_execution(execution.state));
                        call.truncated = execution.result_truncated;
                        if let Some(command) = &execution.command {
                            call.exit_code = command.exit_code;
                        }
                    }
                }
                true
            }
            Err(error) => {
                self.error(error);
                true
            }
        }
    }

    // ----- agent events --------------------------------------------------

    fn on_agent_event(&mut self, event: AgentEvent) -> bool {
        if matches!(event, AgentEvent::Unknown) {
            return false;
        }
        let meta = event.meta().cloned();
        let Some(meta) = meta else {
            return false;
        };
        let resident = self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == meta.session_id);
        if !resident {
            // Catalog-level facts still apply for other sessions.
            if let AgentEvent::SessionOpened { session, .. } = &event {
                self.merge_catalog_info((**session).clone());
            }
            return false;
        }
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if meta.dropped_before > 0
            && let Some(live) = session.live.as_mut()
        {
            live.gap = true;
        }
        match event {
            AgentEvent::SessionOpened { session: info, .. } => {
                session.info = (*info).clone();
                true
            }
            AgentEvent::SessionClosed { .. } => false,
            AgentEvent::SessionState { state, .. } => {
                session.apply_state(&state);
                true
            }
            AgentEvent::TurnStarted { turn, .. } => {
                if session.live.is_none() {
                    let start = session
                        .next_turn_start
                        .take()
                        .unwrap_or_else(|| session.transcript.len());
                    session.live = Some(LiveTurn::new(turn.clone(), start));
                    // The event may describe a loop this app never sent, for
                    // example after opening a running session. `turn.wait` is
                    // the only thing that ends a turn, so register one.
                    let epoch = session.epoch;
                    tasks::wait_turn(self.client.clone(), self.tx.clone(), turn, epoch);
                }
                true
            }
            AgentEvent::RequestStarted {
                request_index,
                model,
                ..
            } => {
                if let Some(live) = session.live.as_mut() {
                    live.requests = live.requests.max(request_index + 1);
                    live.phase = Phase::Waiting;
                }
                if !model.is_empty() {
                    session.info.model = model;
                }
                true
            }
            AgentEvent::RequestUsage { usage, .. } => {
                if let Some(live) = session.live.as_mut() {
                    live.usage = *usage;
                }
                true
            }
            AgentEvent::OutputDelta {
                turn,
                request_index,
                reasoning,
                delta,
                ..
            } => {
                if !live_matches(session, &turn) {
                    return false;
                }
                let key = Session::request_key(&turn.loop_id, request_index);
                let Block::Assistant {
                    reasoning: thinking,
                    text,
                    streaming,
                    ..
                } = session.transcript.assistant_mut(&key)
                else {
                    unreachable!("assistant_mut yields an assistant block")
                };
                if reasoning {
                    thinking.push_str(&delta);
                } else {
                    text.push_str(&delta);
                }
                *streaming = true;
                if let Some(live) = session.live.as_mut() {
                    live.phase = if reasoning {
                        Phase::Thinking
                    } else {
                        Phase::Writing
                    };
                }
                true
            }
            AgentEvent::SteerProgress { applied_count, .. } => {
                if let Some(loop_id) = session.live.as_ref().map(|live| live.turn.loop_id.clone()) {
                    session.apply_steer_progress(&loop_id, applied_count);
                }
                true
            }
            AgentEvent::ToolStarted {
                turn,
                request_index,
                tool_call_id,
                tool_name,
                ..
            } => {
                if !live_matches(session, &turn) {
                    return false;
                }
                let key = ToolRef {
                    session_id: turn.session_id.clone(),
                    loop_id: turn.loop_id.clone(),
                    request_index,
                    tool_call_id,
                };
                if session.transcript.find_tool(&key).is_none() {
                    let mut call = ToolCall::new(key, tool_name.clone());
                    call.state = ToolState::Running;
                    call.started_at = Some(Instant::now());
                    session.transcript.push(Block::Tool(Box::new(call)));
                }
                if let Some(live) = session.live.as_mut() {
                    live.phase = Phase::Thinking;
                    live.running_tool = Some((tool_name, String::new()));
                }
                true
            }
            AgentEvent::ToolPresentation {
                call: identity,
                tool_name,
                display,
                ..
            } => {
                if !live_matches(session, &identity.turn) {
                    return false;
                }
                let key = identity.key();
                let detail = display.detail.clone();
                match session.transcript.find_tool_mut(&key) {
                    Some(call) => call.apply_display(&display),
                    None => {
                        let name = if tool_name.is_empty() {
                            "tool".to_owned()
                        } else {
                            tool_name.clone()
                        };
                        let mut call = ToolCall::new(key, name);
                        call.apply_display(&display);
                        call.state = ToolState::Running;
                        call.started_at = Some(Instant::now());
                        session.transcript.push(Block::Tool(Box::new(call)));
                    }
                }
                if let Some(live) = session.live.as_mut()
                    && let Some((_, running)) = live.running_tool.as_mut()
                {
                    *running = detail;
                }
                true
            }
            AgentEvent::ToolArgumentsPreview {
                call: identity,
                tool_name,
                generated,
                discarded,
                display,
                ..
            } => {
                if !live_matches(session, &identity.turn) {
                    return false;
                }
                let key = identity.key();
                if let Some(call) = session.transcript.find_tool_mut(&key) {
                    call.apply_display(&display);
                } else if !generated && !discarded {
                    // The preview may arrive before `tool_started`; the card is
                    // created here so streaming arguments show as they generate.
                    let name = if tool_name.is_empty() {
                        "tool".to_owned()
                    } else {
                        tool_name
                    };
                    let mut call = ToolCall::new(key, name);
                    call.apply_display(&display);
                    session.transcript.push(Block::Tool(Box::new(call)));
                }
                true
            }
            AgentEvent::ToolInvocation { invocation, .. } => {
                let key = invocation.tool_ref.clone();
                match session.transcript.find_tool_mut(&key) {
                    Some(call) => {
                        call.detail = invocation.detail();
                        call.name = invocation.name.clone();
                    }
                    None => {
                        let mut call = ToolCall::new(key, invocation.name.clone());
                        call.detail = invocation.detail();
                        session.transcript.push(Block::Tool(Box::new(call)));
                    }
                }
                true
            }
            AgentEvent::ToolExecution { execution, .. } => {
                let key = execution.tool_ref.clone();
                let state = ToolState::from_execution(execution.state);
                let mut terminal = false;
                if let Some(call) = session.transcript.find_tool_mut(&key) {
                    call.set_state(state);
                    if let Some(count) = execution.output_line_count {
                        call.output_lines = Some(count);
                    }
                    call.truncated = execution.result_truncated;
                    if let Some(command) = &execution.command {
                        call.exit_code = command.exit_code;
                    }
                    terminal = state.is_terminal();
                }
                if terminal {
                    session.finish_tool(&key);
                }
                true
            }
            AgentEvent::ToolProcess { data, .. } => {
                let Some(chunk) = &data.chunk else {
                    return false;
                };
                let bytes = if chunk.encoding == "base64" {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD
                        .decode(chunk.data.as_bytes())
                        .unwrap_or_default()
                } else {
                    chunk.data.clone().into_bytes()
                };
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if let Some(call) = session.transcript.find_tool_mut(&data.tool_ref)
                    && let Some(text) = call.accept_chunk(chunk.stream, chunk, text)
                {
                    call.push_output(&text);
                }
                true
            }
            AgentEvent::ToolFinished {
                call: identity,
                outcome,
                content,
                content_truncated,
                ..
            } => {
                use crate::protocol::event::ToolOutcome;
                let key = identity.key();
                session.finish_tool(&key);
                if let Some(call) = session.transcript.find_tool_mut(&key) {
                    let state = match outcome {
                        ToolOutcome::Success => ToolState::Succeeded,
                        ToolOutcome::Failed => ToolState::Failed,
                        ToolOutcome::Denied => ToolState::Denied,
                        ToolOutcome::Cancelled => ToolState::Cancelled,
                        _ => call.state,
                    };
                    call.set_state(state);
                    call.truncated |= content_truncated;
                    if call.output.is_empty()
                        && let Some(content) = content
                    {
                        call.push_output(&content);
                    }
                }
                true
            }
            AgentEvent::InteractionRequested {
                turn, interaction, ..
            } => {
                let detail = session
                    .transcript
                    .find_tool_by_call_id(&interaction.tool_call_id)
                    .map(|call| call.detail.clone());
                session.interaction =
                    Some(PendingInteraction::from_wire(turn, &interaction, detail));
                self.interaction_choice = 0;
                true
            }
            AgentEvent::InteractionResolved { .. } => {
                session.interaction = None;
                true
            }
            AgentEvent::TurnFinished { .. } => {
                // A hint only: `turn.wait` is what ends the turn.
                if let Some(live) = session.live.as_mut() {
                    live.phase = Phase::Waiting;
                }
                true
            }
            AgentEvent::Unknown => false,
        }
    }

    fn merge_catalog_info(&mut self, info: SessionInfo) {
        match self
            .sessions
            .iter_mut()
            .find(|known| known.session_id == info.session_id)
        {
            Some(known) => *known = info,
            None => self.sessions.push(info),
        }
    }
}

fn live_matches(session: &Session, turn: &TurnRef) -> bool {
    session
        .live
        .as_ref()
        .is_some_and(|live| live.turn.loop_id == turn.loop_id)
}

/// Appends decoded items to the transcript, in source order.
fn install_items(session: &mut Session, items: &[(usize, crate::protocol::read::RawHistoryItem)]) {
    for (_, item) in items {
        crate::transcript::block::append_history_item(
            &mut session.transcript,
            item,
            &session.info.session_id,
        );
    }
}

fn install_items_into(
    blocks: &mut Vec<Block>,
    items: &[(usize, crate::protocol::read::RawHistoryItem)],
) {
    let mut scratch = Transcript::default();
    for (_, item) in items {
        crate::transcript::block::append_history_item(&mut scratch, item, "");
    }
    blocks.extend(scratch.into_blocks());
}

/// Default `/export` path: `./minicore-<session>-<date>.md`.
pub fn default_export_path(session_id: &str) -> String {
    let short = session_id.strip_prefix("ses_").unwrap_or(session_id);
    let short = &short[..short.len().min(8)];
    format!("minicore-{short}-{}.md", crate::app::commands::today())
}

/// A short human duration: `0.4s`, `12s`, `3m07s`.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else if seconds < 60.0 {
        format!("{}s", seconds as u64)
    } else {
        let minutes = (seconds / 60.0) as u64;
        let rest = (seconds % 60.0) as u64;
        format!("{minutes}m{rest:02}s")
    }
}

/// A compact token count: `950`, `12.3k`, `1.8M`.
pub fn format_tokens(tokens: u64) -> String {
    match tokens {
        value if value < 1_000 => format!("{value}"),
        value if value < 1_000_000 => format!("{:.1}k", value as f64 / 1_000.0),
        value => format!("{:.1}M", value as f64 / 1_000_000.0),
    }
}

/// Turn-end summary: outcome, duration, request/tool counts and changes.
pub fn turn_end_line(
    outcome: &LoopOutcome,
    duration: Option<Duration>,
    requests: u32,
    tool_rounds: u64,
    changes: Option<&ChangeSummary>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(outcome.label());
    if let Some(duration) = duration {
        parts.push(format!("in {}", format_duration(duration)));
    }
    if requests > 0 {
        parts.push(format!("{requests} requests"));
    }
    if tool_rounds > 0 {
        parts.push(format!("{tool_rounds} tools"));
    }
    if let Some(changes) = changes
        && !changes.is_empty()
    {
        let files = if changes.files == 1 {
            "1 file changed".to_owned()
        } else {
            format!("{} files changed", changes.files)
        };
        match changes.lines {
            Some((added, removed)) => parts.push(format!("{files} (+{added} −{removed})")),
            None => parts.push(files),
        }
    }
    parts.join(" · ")
}

/// Context usage as displayed in the footer and the context view.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub percent: Option<f64>,
    pub budget: Option<u64>,
}

impl App {
    /// Context facts, preferring the Agent's presentation report and falling
    /// back to the history-budget estimate.
    pub fn context_usage(&self) -> ContextUsage {
        let Some(session) = self.session.as_ref() else {
            return ContextUsage::default();
        };
        let budget = session
            .context
            .as_ref()
            .and_then(|budget| budget.input_budget_tokens);
        if let Some(usage) = session
            .presentation
            .as_ref()
            .and_then(|p| p.context.as_ref())
            && usage.tokens.is_some()
        {
            return ContextUsage {
                tokens: usage.tokens,
                percent: usage
                    .percent
                    .or_else(|| match (usage.tokens, usage.window) {
                        (Some(tokens), Some(window)) if window > 0 => {
                            Some(tokens as f64 / window as f64 * 100.0)
                        }
                        _ => None,
                    }),
                budget: usage.window.or(budget),
            };
        }
        let tokens = session.context.as_ref().and_then(|budget| {
            budget
                .estimated_request_context_tokens
                .or(budget.estimated_history_tokens)
        });
        ContextUsage {
            tokens,
            percent: match (tokens, budget) {
                (Some(tokens), Some(budget)) if budget > 0 => {
                    Some(tokens as f64 / budget as f64 * 100.0)
                }
                _ => None,
            },
            budget,
        }
    }

    /// Queue rows shown above the composer, at most three.
    pub fn queue_lines(&self) -> Vec<String> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let mut lines: Vec<String> = session
            .queue
            .iter()
            .take(3)
            .map(|queued| {
                format!(
                    "{} {} · {}",
                    if queued.kind == QueuedKind::Steer {
                        "↳"
                    } else {
                        "⏵"
                    },
                    queued.label(),
                    crate::ui::panel::truncate(&queued.text.replace('\n', " "), 60)
                )
            })
            .collect();
        if session.queue.len() > 3 {
            lines.push(format!("+{} more", session.queue.len() - 3));
        }
        lines
    }

    pub fn queue_len(&self) -> usize {
        self.session
            .as_ref()
            .map_or(0, |session| session.queue.len())
    }

    /// Reasoning levels the current model supports.
    pub fn session_levels(&self) -> Vec<crate::protocol::Reasoning> {
        self.session
            .as_ref()
            .and_then(|session| session.model(&self.models))
            .map(|model| model.supported_reasoning.clone())
            .filter(|levels| !levels.is_empty())
            .unwrap_or_else(|| {
                vec![
                    crate::protocol::Reasoning::Auto,
                    crate::protocol::Reasoning::Low,
                    crate::protocol::Reasoning::Medium,
                    crate::protocol::Reasoning::High,
                ]
            })
    }

    /// Rows of the interaction prompt.
    pub fn interaction_lines(&self) -> Vec<String> {
        let Some(interaction) = self
            .session
            .as_ref()
            .and_then(|session| session.interaction.as_ref())
        else {
            return Vec::new();
        };
        let mut lines = vec![format!(
            "{} wants to run {}",
            interaction.tool_name, interaction.prompt
        )];
        if let Some(detail) = &interaction.detail {
            lines.push(detail.clone());
        }
        for (index, option) in interaction.options().iter().enumerate() {
            let marker = if index == self.interaction_choice {
                "▶"
            } else {
                " "
            };
            lines.push(format!("{marker} {option}"));
        }
        lines
    }

    pub fn context_budget(&self) -> Option<&ContextBudget> {
        self.session.as_ref().and_then(|s| s.context.as_ref())
    }
}

/// The loop id of the newest finished turn, for a turn-scoped diff.
fn latest_loop(session: &Session) -> Option<String> {
    session
        .transcript
        .blocks()
        .iter()
        .rev()
        .find_map(|block| match block {
            Block::TurnEnd { loop_id, .. } => Some(loop_id.clone()),
            _ => None,
        })
}

/// Files a turn touched, from the write/edit cards it holds, so the diff view
/// can list something before `changes.list` answers.
fn collect_changed_files(
    transcript: &Transcript,
    loop_id: &str,
) -> Vec<crate::ui::views::DiffFile> {
    let mut files: Vec<crate::ui::views::DiffFile> = Vec::new();
    for block in transcript.blocks() {
        let Block::Tool(call) = block else {
            continue;
        };
        if call.key.loop_id != loop_id || !writes_files(&call.name) {
            continue;
        }
        let path = call.detail.trim();
        if path.is_empty() || files.iter().any(|file| file.path == path) {
            continue;
        }
        files.push(crate::ui::views::DiffFile {
            path: path.to_owned(),
            kind: crate::protocol::ChangeKind::Modified,
            lines: None,
            failed: false,
            change_refs: Vec::new(),
        });
    }
    files
}

fn writes_files(name: &str) -> bool {
    matches!(name, "write" | "edit" | "apply_patch" | "patch")
}

/// Added and removed line counts of a rendered diff.
fn count_hunks(lines: &[ratatui::text::Line<'static>]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in lines {
        let Some(span) = line.spans.first() else {
            continue;
        };
        let text = span.content.as_ref();
        if text.starts_with('+') {
            added += 1;
        } else if text.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// What the sessions picker filters on: the title, or the id while untitled.
fn session_label(info: &SessionInfo) -> String {
    match info.title.as_deref() {
        Some(title) if !title.trim().is_empty() => title.to_owned(),
        // Untitled sessions get their short id, never the 32-hex one: a closed
        // session cannot be read, so there is no first prompt to borrow.
        _ => short_session_id(&info.session_id).to_owned(),
    }
}

/// `ses_8ca3c4cf...` → `ses_8ca3c4`, enough to tell sessions apart in a row.
pub fn short_session_id(session_id: &str) -> &str {
    let body = session_id.strip_prefix("ses_").unwrap_or(session_id);
    let cut = body
        .char_indices()
        .nth(6)
        .map_or(body.len(), |(index, _)| index);
    let start = session_id.len() - body.len();
    &session_id[..start + cut]
}

/// Rendered lines back to text, for `/copy` in a view.
fn view_text(lines: &[ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The readable text of a transcript, used by `/copy` when no view is open.
fn view_text_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        let text = match block {
            Block::User { text, .. } => text,
            Block::Assistant { text, .. } => text,
            Block::Notice { text, .. } => text,
            Block::Compaction { summary, .. } => summary.as_deref().unwrap_or_default(),
            Block::Tool(call) => call.detail.as_str(),
            Block::TurnEnd { .. } | Block::EarlierHistory { .. } => "",
        };
        if text.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(text);
    }
    out
}
