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
pub mod session;
pub mod tasks;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clipboard;
use crate::editor;
use crate::protocol::{
    AgentEvent, CompactionPhase, CompactionProgress, ContextBudget, LoopOutcome, ModelInfo,
    ProfileInfo, RpcError, SessionContext, SessionInfo, SessionPresentation, SessionState, ToolRef,
    TurnRef,
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
    TurnItems(Result<Vec<(usize, crate::protocol::read::RawHistoryItem)>, RpcError>),
    State(Result<SessionState, RpcError>),
    Presentation(Result<SessionPresentation, RpcError>),
    Context(Result<SessionContext, RpcError>),
    Sent(Result<TurnRef, RpcError>),
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
    Editor(Result<Option<String>, String>),
    /// `session.state` poll while waiting for post-turn compaction.
    IdleCheck(Result<bool, RpcError>),
    /// The change records of one diff scope.
    ChangeFiles(DiffScope, Result<Vec<crate::protocol::ChangeRecord>, RpcError>),
    /// The hunks of one change record.
    DiffFile(String, Result<crate::protocol::changes::DiffPage, RpcError>),
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
        confirm_delete: bool,
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
                    confirm_delete: false,
                }
            }
            Panel::Models => Overlay::Models { selected: 0 },
            Panel::Reasoning => Overlay::Reasoning { selected: 0 },
            Panel::Files => Overlay::Files {
                filter: String::new(),
                selected: 0,
            },
            Panel::Context => {
                if let Some(session) = self.session.as_ref() {
                    // A fresh report: the view must not show a stale ledger.
                    tasks::context(
                        self.client.clone(),
                        self.tx.clone(),
                        session.info.session_id.clone(),
                    );
                }
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
        let Some(prompt) = session.transcript.blocks().iter().find_map(|block| match block {
            Block::User {
                text,
                kind: crate::protocol::read::UserKind::Prompt,
            } => Some(text.as_str()),
            _ => None,
        }) else {
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
                filter.is_empty()
                    || crate::ui::picker::fuzzy_match(&session_label(info), filter)
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
    pub composer: String,
    pub draft_history: VecDeque<String>,
    pub draft_cursor: Option<usize>,
    pub notices: VecDeque<Notice>,
    pub scroll: Scroll,
    pub tools_expanded: bool,
    pub show_reasoning: bool,
    pub quit: bool,
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
    /// The change record whose hunks the diff view is waiting for.
    pub diff_pending: Option<crate::protocol::ChangeRecord>,
    /// The tool-output view, rebuilt from the selected card.
    pub tool_view: crate::ui::views::ToolView,
    /// Scroll offset of the help view.
    pub help_scroll: usize,
    /// Sessions picker rows, in display order, refreshed when it opens.
    pub session_rows: Vec<crate::protocol::SessionInfo>,
    /// Confirm a delete with a second Ctrl+D before it runs.
    pub delete_pending: Option<String>,
    /// Where the transcript was drawn last frame, for mouse hit-testing.
    pub transcript_area: Option<ratatui::layout::Rect>,
    /// The rows the last frame drew, so selection copies what is on screen.
    pub rendered: crate::ui::selection::Screen,
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
            composer: String::new(),
            draft_history: VecDeque::new(),
            draft_cursor: None,
            notices: VecDeque::new(),
            scroll: Scroll::Follow,
            tools_expanded: false,
            show_reasoning: false,
            quit: false,
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
            diff_pending: None,
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
            transcript_area: None,
            rendered: crate::ui::selection::Screen::default(),
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
                if let Some(draft) = draft {
                    self.composer = draft;
                }
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
                self.composer = text;
                self.submit()
            }
            Action::Interrupt => self.interrupt(),
            Action::OpenSession(id) => {
                let epoch = self.next_epoch();
                tasks::open_session(self.client.clone(), self.tx.clone(), id);
                let _ = epoch;
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
                    let path = path
                        .or_else(|| Some(features::export_path(&session.info.session_id)));
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
                let mut config = crate::config::Config::load();
                config.theme = kind;
                if let Err(error) = config.save() {
                    self.notice(NoticeLevel::Error, error);
                }
                self.theme = kind;
                self.palette = crate::theme::Palette::for_kind(kind);
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
        let text = std::mem::take(&mut self.composer).trim_end().to_owned();
        if text.trim().is_empty() {
            return true;
        }
        self.remember_draft(&text);
        if self.is_busy() {
            self.steer(text);
        } else {
            self.send(text);
        }
        true
    }

    /// Alt+Enter: queue a follow-up; when idle it just sends.
    fn submit_follow_up(&mut self) -> bool {
        let text = std::mem::take(&mut self.composer).trim_end().to_owned();
        if text.trim().is_empty() {
            return true;
        }
        self.remember_draft(&text);
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

    fn remember_draft(&mut self, text: &str) {
        if self.draft_history.back().map(String::as_str) != Some(text) {
            self.draft_history.push_back(text.to_owned());
            while self.draft_history.len() > 100 {
                self.draft_history.pop_front();
            }
        }
        self.draft_cursor = None;
    }

    fn send(&mut self, text: String) {
        let Some(session) = self.session.as_mut() else {
            self.composer = text;
            self.notice(NoticeLevel::Error, "No session is open");
            return;
        };
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
            self.composer = text;
            return;
        };
        let Some(turn) = session.live.as_ref().map(|live| live.turn.clone()) else {
            self.composer = text;
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
        let profile = self.profiles.first().map(|profile| profile.id.clone());
        let mut params = serde_json::json!({ "workspace": self.workspace });
        if let Some(profile) = profile {
            params["profile"] = serde_json::json!(profile);
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

    fn edit_draft(&mut self) {
        let tx = self.tx.clone();
        let draft = std::mem::take(&mut self.composer);
        let config = crate::config::Config::load();
        tokio::spawn(async move {
            let command = config.editor();
            let result = match command {
                Some(command) => editor::edit(&command, &draft).await,
                None => Err(
                    "No editor configured: set `editor` in the TUI config or $VISUAL/$EDITOR"
                        .to_owned(),
                ),
            };
            let _ = tx.send(Msg::Op(Box::new(OpDone {
                session: None,
                epoch: 0,
                kind: OpKind::Editor(result),
            })));
        });
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
        tasks::load_older(
            self.client.clone(),
            self.tx.clone(),
            session.info.session_id.clone(),
            session.loaded_from,
        );
    }

    fn recall_queued(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Some(text) = session.take_follow_up() {
            self.composer = text;
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
    /// position so new output cannot move what is being read.
    pub fn scroll_by(&mut self, delta: isize) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let total = session.transcript.len();
        if total == 0 {
            return;
        }
        // Row addressing is the UI layer's job; a signed block step keeps the
        // reducer dependency-free and is exact enough for wheel and PgUp.
        let (block, offset) = match self.scroll {
            Scroll::Follow => (total.saturating_sub(1), 0),
            Scroll::Anchor { block, offset } => (block, offset as isize),
        };
        let next = block as isize + delta;
        if next >= total as isize - 1 && delta > 0 {
            self.scroll = Scroll::Follow;
            return;
        }
        let block = next.clamp(0, total as isize - 1) as usize;
        let offset = if next <= 0 { 0 } else { offset.max(0) as u16 };
        self.scroll = Scroll::Anchor { block, offset };
    }

    /// The drag as text, sliced from the rows the last frame actually drew.
    /// The renderer stores those rows, so the copy matches what is on screen
    /// even while a turn is streaming.
    pub fn selected_text(&self, drag: crate::ui::selection::Drag) -> Option<String> {
        let area = self.transcript_area?;
        let text = crate::ui::selection::text(&self.rendered, area, drag);
        (!text.is_empty()).then_some(text)
    }

    /// Rows available to the transcript, so paging moves a real page.
    pub fn transcript_rows(&self) -> usize {
        self.transcript_area.map_or(20, |area| area.height as usize)
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
        let Overlay::Diff { scope, file, scroll } = &mut self.overlay else {
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

    fn next_epoch(&mut self) -> u64 {
        match self.session.as_mut() {
            Some(session) => {
                session.epoch += 1;
                session.epoch
            }
            None => 0,
        }
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
                Ok(crate::clipboard::Copied::Native) => {
                    self.notice(NoticeLevel::Info, "Copied")
                }
                Ok(crate::clipboard::Copied::Osc52) => {
                    self.notice(NoticeLevel::Info, "Copied with OSC 52")
                }
                Err(error) => self.notice(NoticeLevel::Error, error),
            },
            OpKind::Editor(result) => match result {
                Ok(Some(draft)) => self.composer = draft,
                Ok(None) => {}
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
                if epoch != resident.epoch && matches!(kind, OpKind::Waited(_) | OpKind::Sent(_)) {
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
            OpKind::TurnItems(result) => self.on_turn_items(result),
            OpKind::State(result) => self.on_state(result),
            OpKind::IdleCheck(result) => self.on_idle_check(result),
            OpKind::Presentation(result) => self.on_presentation(result),
            OpKind::Context(result) => self.on_context(result),
            OpKind::Sent(result) => self.on_sent(result),
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
            OpKind::ChangeFiles(_, result) => self.on_change_files(result),
            OpKind::DiffFile(path, result) => self.on_diff_file(path, result),
            OpKind::Copied(_)
            | OpKind::Editor(_)
            | OpKind::Created(_)
            | OpKind::Deleted(..)
            | OpKind::Exported(_)
            | OpKind::Opened(_) => unreachable!("handled before the session dispatch"),
        }
    }

    fn on_opened(&mut self, result: Result<SessionInfo, RpcError>) {
        match result {
            Ok(info) => self.adopt(info),
            Err(error) => self.error(error),
        }
    }

    fn on_created(&mut self, result: Result<SessionInfo, RpcError>) {
        match result {
            Ok(info) => self.adopt(info),
            Err(error) => self.error(error),
        }
    }

    /// Makes `info` the resident session and loads its tail.
    fn adopt(&mut self, info: SessionInfo) {
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
        tasks::presentation(self.client.clone(), self.tx.clone(), session_id, epoch);
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
        session.first_item = page.from;
        session.loaded_from = page.from;
        session.hidden_items = page.from;
        install_items(session, &page.items);
        if page.has_more {
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
                    // The prefix changed (a new compaction); reload the tail.
                    if let Some(session) = self.session.as_ref() {
                        tasks::load_tail(
                            self.client.clone(),
                            self.tx.clone(),
                            session.info.session_id.clone(),
                        );
                    }
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
        session.transcript.remove_front(skip);
        for block in blocks.into_iter().rev() {
            session.transcript.insert(0, block);
        }
        session.loaded_from = page.from;
        session.hidden_items = page.from;
        if page.has_more {
            session.transcript.insert(
                0,
                Block::EarlierHistory {
                    hidden_items: page.from,
                },
            );
        }
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
        let start = session
            .live
            .as_ref()
            .map(|live| live.start)
            .unwrap_or_default();
        session.drop_live_region(start);
        install_items(session, items);
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
        if let Some(session) = self.session.as_mut() {
            session.apply_state(state);
        }
        if idle {
            self.flush_queue();
        }
        true
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

    fn on_sent(&mut self, result: &Result<TurnRef, RpcError>) -> bool {
        match result {
            Ok(turn) => {
                if let Some(session) = self.session.as_mut()
                    && session.live.is_none()
                {
                    let start = session.transcript.len();
                    session.live = Some(LiveTurn::new(turn.clone(), start));
                }
                self.title_from_first_prompt();
                true
            }
            Err(error) if error.is_busy() => {
                // Post-turn compaction still owns the session. Keep the draft
                // and poll until the Agent is idle again; never resend blindly.
                self.waiting_for_compaction = true;
                self.retry_when_idle();
                true
            }
            Err(error) => {
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
        if completed {
            session.finish_turn(
                &loop_id,
                wait.outcome.clone(),
                usage.clone(),
                requests,
                tool_rounds,
                duration,
            );
        } else {
            // Replace the streamed (possibly partial) turn with its durable
            // items; the turn-end line is appended when they arrive.
            session.drop_live_region(start);
            session.finish_turn(
                &loop_id,
                wait.outcome.clone(),
                usage.clone(),
                requests,
                tool_rounds,
                duration,
            );
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
                        if let Some(session) = self.session.as_mut() {
                            session.push_compaction_result(result);
                        }
                        // The projection changed: reload the tail so the summary
                        // and the retained window are consistent.
                        if let Some(session) = self.session.as_ref() {
                            tasks::load_tail(
                                self.client.clone(),
                                self.tx.clone(),
                                session.info.session_id.clone(),
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

    /// Fill the diff view's file column, then ask for the selected file's
    /// hunks.
    fn on_change_files(
        &mut self,
        result: &Result<Vec<crate::protocol::ChangeRecord>, RpcError>,
    ) -> bool {
        let records = match result {
            Ok(records) => records.clone(),
            Err(error) => {
                self.diff.status = error.message.clone();
                return true;
            }
        };
        let files: Vec<crate::ui::views::DiffFile> = records
            .iter()
            .map(|record| crate::ui::views::DiffFile {
                path: record.path.clone(),
                kind: record.kind,
                added: 0,
                removed: 0,
            })
            .collect();
        self.diff.set_files(files);
        if self.diff.files.is_empty() {
            self.diff.status = "No file changes are recorded for this scope.".into();
            self.diff_pending = None;
            return true;
        }
        self.diff_pending = records.first().cloned();
        if let (Some(record), Some(session)) = (records.first(), self.session.as_ref()) {
            tasks::load_diff(
                self.client.clone(),
                self.tx.clone(),
                session.info.session_id.clone(),
                record.change_ref.clone(),
                record.path.clone(),
            );
        }
        true
    }

    fn on_diff_file(
        &mut self,
        _path: &str,
        result: &Result<crate::protocol::changes::DiffPage, RpcError>,
    ) -> bool {
        match result {
            Ok(page) => {
                let lines = crate::ui::views::hunk_lines(page, &self.palette);
                let (added, removed) = count_hunks(&lines);
                self.diff.hunks = lines;
                self.diff.scroll = 0;
                self.diff.status.clear();
                if let Some(file) = self.diff.files.get_mut(self.diff.selected) {
                    file.added = added;
                    file.removed = removed;
                }
            }
            Err(error) => {
                self.diff.hunks.clear();
                self.diff.status = error.message.clone();
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
                        call.state = ToolState::from_execution(execution.state);
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
                    let start = session.transcript.len();
                    session.live = Some(LiveTurn::new(turn, start));
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
                    let mut call = ToolCall::new(key, "tool");
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
                    call.state = state;
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
                if let Some(call) = session.transcript.find_tool_mut(&data.tool_ref) {
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
                    call.state = match outcome {
                        ToolOutcome::Success => ToolState::Succeeded,
                        ToolOutcome::Failed => ToolState::Failed,
                        ToolOutcome::Denied => ToolState::Denied,
                        ToolOutcome::Cancelled => ToolState::Cancelled,
                        _ => call.state,
                    };
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
        parts.push(format!("{} files changed", changes.files));
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

    pub fn context_budget(&self) -> Option<&ContextBudget> {
        self.session.as_ref().and_then(|s| s.context.as_ref())
    }
}

/// The loop id of the newest finished turn, for a turn-scoped diff.
fn latest_loop(session: &Session) -> Option<String> {
    session.transcript.blocks().iter().rev().find_map(|block| match block {
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
            added: 0,
            removed: 0,
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
        _ => info.session_id.clone(),
    }
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
