//! Application state and the reducer.
//!
//! `App` is mutated only on the main loop. IO runs in spawned tasks that post
//! a [`Msg`]; a result for a session that is no longer resident is dropped by
//! its epoch.

pub mod actions;
pub mod commands;
pub mod diff;
pub mod find;
pub mod keys;
mod ops;
pub mod overlay;
pub mod session;
pub mod tasks;
#[cfg(test)]
mod tests;
pub mod tool;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::ThemeKind;
use crate::protocol::changes::DiffPage;
use crate::protocol::read::RawHistoryItem;
use crate::protocol::{
    AgentEvent, ChangeRecord, CompactionResult, ModelInfo, ProfileInfo, Reasoning, RpcError,
    SessionContext, SessionInfo, SessionPresentation, SessionState, ToolReadResult, ToolRef,
    TurnRef, TurnWaitResult,
};
use crate::rpc::Client;
use crate::transcript::layout::Layout;
use crate::transcript::{Block, ChangeSummary, NoticeLevel};
use crate::ui::theme::{Palette, Styles};
use crate::ui::transcript::RenderState;
use crate::ui::{sheets, views};

pub use actions::{Action, Answer};
pub use diff::{DiffQuery, DiffScope};
pub use overlay::{Overlay, Panel};
pub use session::{
    InteractionKind, LiveTurn, PendingInteraction, Phase, Queued, QueuedKind, Session,
};
pub use tasks::Tx;

/// Everything the main loop can hand to the reducer.
pub enum Msg {
    Agent(Box<AgentEvent>),
    AgentClosed(String),
    Ready(Box<Ready>),
    Opened(Result<SessionInfo, RpcError>),
    Created(Result<SessionInfo, RpcError>),
    Deleted(String, Result<(), RpcError>),
    /// A result for the session that was resident at epoch `.0`.
    Session(u64, Box<SessionOp>),
    Notice(NoticeLevel, String),
    /// The external editor returned a draft.
    DraftEdited(Option<String>),
}

/// The catalogs read at startup.
pub struct Ready {
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    pub sessions: Vec<SessionInfo>,
}

/// What an async operation on the resident session produced.
pub enum SessionOp {
    Tail(Result<tasks::History, RpcError>),
    Page(Result<tasks::History, RpcError>),
    TurnItems(Result<Vec<RawHistoryItem>, RpcError>),
    State(Result<SessionState, RpcError>),
    Presentation(Result<SessionPresentation, RpcError>),
    Context(Result<SessionContext, RpcError>),
    /// The send answer, with the text so a refusal can hand it back.
    Sent(String, Result<TurnRef, RpcError>),
    Waited(Result<TurnWaitResult, RpcError>),
    Steered(Result<Value, RpcError>),
    /// An answer of which only a failure matters.
    Done(Result<Value, RpcError>),
    Compacted(Result<CompactionResult, RpcError>),
    Info(Result<SessionInfo, RpcError>),
    Changes(String, Result<ChangeSummary, RpcError>),
    ToolRead(ToolRef, Result<ToolReadResult, RpcError>),
    /// The facts of a card loaded without its detail; `None` when the read failed.
    ToolHydrated(ToolRef, Option<ToolReadResult>),
    /// One stream of a tool call's full output, for the tool view.
    ToolOutput(ToolRef, String, Result<tasks::StreamText, RpcError>),
    Files(Result<Vec<String>, RpcError>),
    ChangeFiles(DiffScope, Result<Vec<ChangeRecord>, RpcError>),
    DiffFile(DiffQuery, Result<Vec<DiffPage>, RpcError>),
}

/// Session settings from the command line: the profile applies when a session
/// is created, model and reasoning also when an existing one is opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchSettings {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
}

impl LaunchSettings {
    /// The settings as wire fields; only a new session takes a profile.
    pub fn fields(&self, with_profile: bool) -> Value {
        let mut fields = json!({});
        if let Some(profile) = self.profile.as_ref().filter(|_| with_profile) {
            fields["profile"] = json!(profile);
        }
        if let Some(model) = &self.model {
            fields["model"] = json!(model);
        }
        if let Some(reasoning) = self.reasoning {
            fields["reasoning"] = json!(reasoning);
        }
        fields
    }
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
    pub at: tokio::time::Instant,
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
    /// Settings requested on the command line, used once by the first session.
    pub launch: LaunchSettings,
    pub composer: crate::ui::composer::Composer,
    pub notices: VecDeque<Notice>,
    pub scroll: Scroll,
    pub tools_expanded: bool,
    pub show_reasoning: bool,
    pub quit: bool,
    /// When Ctrl+C last cleared the draft; a second press soon after quits.
    pub quit_armed: Option<tokio::time::Instant>,
    pub ready: bool,
    pub fatal: Option<Fatal>,
    pub waiting_for_compaction: bool,
    pub started: Instant,
    /// Numbers the compaction operations this process starts.
    pub op_seq: u64,
    pub theme: ThemeKind,
    pub overlay: Overlay,
    pub styles: Styles,
    /// Mouse selection: the cell the drag started at and where it is now.
    pub selection_start: Option<(u16, u16)>,
    pub selection_end: Option<(u16, u16)>,
    /// The rows the last frame drew, so selection copies what is on screen.
    pub rendered: crate::ui::selection::Screen,
    /// The transcript's line cache and viewport, owned by the renderer.
    pub layout: Layout<RenderState>,
    /// A draft waiting for the main loop to run the external editor on it.
    pub edit_request: Option<String>,
    /// An open or create request is in flight, so no session yet means
    /// waiting rather than start one.
    pub loading_session: bool,
}

#[derive(Clone, Debug)]
pub struct Fatal {
    pub reason: String,
    pub stderr_tail: Vec<String>,
}

pub const NOTICE_TTL: Duration = Duration::from_secs(5);
/// Delay before asking again whether post-turn compaction finished.
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
            show_reasoning: true,
            quit: false,
            quit_armed: None,
            ready: false,
            fatal: None,
            waiting_for_compaction: false,
            started: Instant::now(),
            op_seq: 0,
            theme: ThemeKind::Dark,
            overlay: Overlay::None,
            styles: Styles::of(Palette::dark()),
            selection_start: None,
            selection_end: None,
            rendered: crate::ui::selection::Screen::default(),
            layout: Layout::default(),
            edit_request: None,
            loading_session: false,
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Agent(event) => self.on_agent_event(*event),
            Msg::AgentClosed(reason) => {
                if self.fatal.is_none() {
                    self.fatal = Some(Fatal {
                        reason,
                        stderr_tail: self.client.stderr_tail(),
                    });
                }
            }
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
            }
            Msg::Opened(result) => self.adopt(result, false),
            Msg::Created(result) => self.adopt(result, true),
            Msg::Deleted(id, result) => {
                if self.ok(result).is_some() {
                    self.sessions.retain(|session| session.session_id != id);
                    if let Overlay::Sessions {
                        rows,
                        filter,
                        selected,
                        ..
                    } = &mut self.overlay
                    {
                        rows.retain(|session| session.session_id != id);
                        let shown = sheets::filtered_sessions(rows, filter, self.session.as_ref());
                        *selected = (*selected).min(shown.len().saturating_sub(1));
                    }
                    self.notice(NoticeLevel::Info, "Session deleted");
                }
            }
            Msg::Session(epoch, op) => {
                if self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.epoch == epoch)
                {
                    self.on_op(*op);
                }
            }
            Msg::Notice(level, text) => self.notice(level, text),
            // An empty file is a deliberate blank draft, not a cancel.
            Msg::DraftEdited(draft) => self
                .composer
                .restore_text(draft.as_deref().unwrap_or_default()),
        }
    }

    /// Expires info notices; errors stay until the next key press.
    pub fn tick(&mut self) {
        self.notices.retain(|notice| {
            notice.level == NoticeLevel::Error || notice.at.elapsed() < NOTICE_TTL
        });
    }

    /// Whether something on screen moves with the clock.
    pub fn wants_tick(&self) -> bool {
        self.status_label().is_some()
            || self
                .notices
                .iter()
                .any(|notice| notice.level == NoticeLevel::Info)
    }

    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notices.push_back(Notice {
            level,
            text: text.into(),
            at: tokio::time::Instant::now(),
        });
        while self.notices.len() > 2 {
            self.notices.pop_front();
        }
    }

    pub fn error(&mut self, error: RpcError) {
        self.notice(NoticeLevel::Error, error.message);
    }

    /// The value of a result; an error becomes a notice.
    pub(super) fn ok<T>(&mut self, result: Result<T, RpcError>) -> Option<T> {
        result.map_err(|error| self.error(error)).ok()
    }

    /// The resident session, for results `on_msg` has matched to it by epoch.
    pub(super) fn session_mut(&mut self) -> &mut Session {
        self.session
            .as_mut()
            .expect("the result's session is resident")
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

    /// The transcript blocks, or an empty slice when no session is open.
    pub fn transcript_blocks(&self) -> &[Block] {
        self.session
            .as_ref()
            .map_or(&[], |session| session.transcript.blocks())
    }

    /// Rows the transcript showed in the last frame.
    pub fn viewport_rows(&self) -> usize {
        self.layout.viewport.height
    }

    /// Moves the transcript view by `delta` rows, switching to an anchored
    /// position so new output cannot move what is being read.
    pub fn scroll_by(&mut self, delta: isize) {
        let layout = &self.layout;
        let total = layout.total_rows();
        let height = layout.viewport.height;
        if total <= height {
            // Nothing to scroll, but a short page is when older history is
            // worth asking for.
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
        // Reaching the first block asks for the next page; a request in flight
        // makes holding the key harmless.
        if delta < 0 && block <= 1 {
            self.load_older();
        }
    }

    /// The drag as text, from the rows the last frame drew.
    pub fn selected_text(&self, drag: crate::ui::selection::Drag) -> Option<String> {
        let text = crate::ui::selection::text(&self.rendered, self.layout.viewport.area(), drag);
        (!text.is_empty()).then_some(text)
    }

    /// `c` in a view: copies what the view shows.
    pub fn copy_view(&mut self) {
        let text = match &self.overlay {
            Overlay::Diff(diff) => views::diff::text(diff),
            Overlay::Tool(tool) => views::tool::text(tool, self.session.as_ref()),
            _ => views::context::text(self.session.as_ref(), self.styles),
        };
        if text.trim().is_empty() {
            self.notice(NoticeLevel::Info, "Nothing to copy here");
        } else {
            self.copy(text);
        }
    }
}
