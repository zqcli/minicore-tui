//! Application state and the reducer.
//!
//! `App` is mutated only on the main loop. IO runs in spawned tasks that post
//! a [`Msg`]; a result reaches the live session with its epoch, and is dropped
//! when that session is gone.

pub mod actions;
pub mod commands;
pub mod composer;
pub mod diff;
mod draft;
pub mod find;
pub mod keys;
mod live;
mod mouse;
mod notify;
mod ops;
pub mod overlay;
pub mod picker;
mod remote;
pub mod scrollbar;
pub mod selection;
pub mod session;
pub mod tasks;
pub mod templates;
#[cfg(test)]
mod tests;
pub mod tool;

use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::ThemeKind;
use crate::protocol::changes::DiffPage;
use crate::protocol::read::{InterruptedRequest, RawHistoryItem};
use crate::protocol::{
    AgentEvent, ChangeList, CompactionResult, ModelInfo, ProfileInfo, Reasoning, RpcError,
    SessionContext, SessionInfo, SessionPresentation, SessionState, SteerResult, ToolReadResult,
    ToolRef, TurnSendResult, TurnWaitResult,
};
use crate::rpc::Client;
use crate::transcript::layout::Layout;
use crate::transcript::{Block, ChangeSummary};
use crate::ui::theme::{Palette, Styles};
use crate::ui::transcript::RenderState;
use crate::ui::views;

pub use actions::{Action, Answer};
pub use diff::DiffQuery;
pub use overlay::{Overlay, Panel};
pub use session::{
    InteractionKind, LiveStatus, LiveTurn, PendingInteraction, Phase, Queued, QueuedKind, Session,
};
pub use tasks::Tx;

/// Everything the main loop can hand to the reducer.
pub enum Msg {
    Remote(crate::remote::RemoteEvent),
    Agent(Box<AgentEvent>),
    AgentClosed(String),
    Ready(Box<Ready>),
    Opened(String, Result<SessionInfo, RpcError>),
    Created(Result<SessionInfo, RpcError>),
    Deleted(String, Result<(), RpcError>),
    /// A `session.rename` from the sessions picker, which may name a closed session.
    Renamed(String, Result<SessionInfo, RpcError>),
    /// A result for the live session with epoch `.0`.
    Session(u64, Box<SessionOp>),
    Notice(NoticeLevel, String),
    /// The external editor returned a draft.
    DraftEdited(Option<String>),
    /// A moment of feedback over the transcript, such as `Copied!`.
    Flash(String),
}

/// The catalogs read at startup.
pub struct Ready {
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    pub sessions: Vec<SessionInfo>,
}

/// What an async operation on the foreground session produced.
pub enum SessionOp {
    Tail(Result<tasks::History, RpcError>),
    Page(Result<tasks::History, RpcError>),
    TurnItems(
        String,
        Result<(Vec<RawHistoryItem>, Option<InterruptedRequest>), RpcError>,
    ),
    State(Result<SessionState, RpcError>),
    Presentation(Result<SessionPresentation, RpcError>),
    /// A `workspace.status` answer, wanted only for the branch it caches in the presentation.
    WorkspaceStatus(Result<Value, RpcError>),
    Context(Result<SessionContext, RpcError>),
    /// The send answer, with the queued message so a refusal can hand it back.
    Sent(Queued, Result<TurnSendResult, RpcError>),
    Waited(Result<TurnWaitResult, RpcError>),
    /// The loop and text keep late steer results from changing another turn's queue.
    Steered(String, String, Result<SteerResult, RpcError>),
    /// An answer of which only a failure matters.
    Done(Result<Value, RpcError>),
    Compacted(Result<CompactionResult, RpcError>),
    Info(Result<SessionInfo, RpcError>),
    /// The number of history items, for `/session`.
    ItemCount(Result<usize, RpcError>),
    Changes(String, Result<ChangeSummary, RpcError>),
    /// The facts of a card loaded without its detail; `None` when the read failed.
    ToolHydrated(ToolRef, Option<ToolReadResult>),
    Files(Result<Vec<String>, RpcError>),
    ChangeFiles(Result<ChangeList, RpcError>),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Error,
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

/// The last left-button press, for drags and repeated clicks.
#[derive(Clone, Debug)]
pub struct Press {
    pub cell: (u16, u16),
    pub at: tokio::time::Instant,
    /// 1, 2 or 3: a character drag, a word, a line.
    pub clicks: u8,
    /// The word under the press, which the next click must hit to count.
    pub word: Option<Range<u16>>,
}

pub struct App {
    pub remote: crate::remote::Remote,
    pub client: Arc<Client>,
    pub tx: Tx,
    pub workspace: PathBuf,
    /// The session on screen; `None` is the draft.
    pub session: Option<Session>,
    /// The other live sessions, most recently left first.
    pub background: Vec<Session>,
    /// Reducer code is running for a background session swapped into `session`.
    pub background_op: bool,
    /// The draft's composer text while a session is on screen.
    pub draft_text: String,
    /// A long turn ended or asked for approval, or a background session asked.
    pub ring: bool,
    pub sessions: Vec<SessionInfo>,
    pub catalog_generation: u64,
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    /// Settings for the next session: the command-line flags, then what a
    /// draft changes. The session that adopts them takes them.
    pub launch: LaunchSettings,
    /// The Agent's default profile, which a new session takes without a `--profile`.
    pub default_profile: Option<String>,
    pub composer: composer::Composer,
    /// Editor-only hit area from the last frame (not completion/sheet rows).
    pub composer_area: ratatui::layout::Rect,
    pub composer_max_rows: usize,
    pub composer_dragging: bool,
    pub notices: VecDeque<Notice>,
    pub scroll: Scroll,
    pub tools_expanded: bool,
    pub show_reasoning: bool,
    pub quit: bool,
    /// When Ctrl+C last cleared the draft; a second press soon after quits.
    pub quit_armed: Option<tokio::time::Instant>,
    pub ready: bool,
    pub fatal: Option<Fatal>,
    pub started: Instant,
    /// Numbers the compaction operations this process starts.
    pub op_seq: u64,
    pub theme: ThemeKind,
    pub overlay: Overlay,
    pub styles: Styles,
    /// The highlighted cells, copied when the button comes up.
    pub selection: Option<selection::Drag>,
    pub press: Option<Press>,
    /// The scrollbar thumb is held this many rows below its top.
    pub scrollbar_grab: Option<usize>,
    /// The last scroll by the user, which shows the scrollbar for a moment.
    pub scrolled_at: Option<tokio::time::Instant>,
    pub flash: Option<(String, tokio::time::Instant)>,
    /// The transcript's line cache and viewport, owned by the renderer.
    pub layout: Layout<RenderState>,
    /// A draft waiting for the main loop to run the external editor on it.
    pub edit_request: Option<String>,
    /// An open or create request is in flight, so no session yet means
    /// waiting rather than start one.
    pub loading_session: bool,
    /// The draft's text while its session is being created; sent once it exists.
    pub first_message: Option<String>,
    /// Numbers the live sessions, so a late result never reaches another one.
    pub epoch: u64,
    pub templates: Vec<templates::Template>,
    /// The TUI directory; `None` in tests, which touch no files.
    pub home: Option<crate::config::Home>,
    /// The Agent config is composed from `home`, so `/reload` composes it again.
    pub composed: bool,
    /// `[ui] editor` from config.toml.
    pub editor: Vec<String>,
    /// The `/word` draft an Enter already warned about; a second Enter sends it.
    pub armed_unknown: Option<String>,
    pub notifier: crate::title::Notifier,
}

#[derive(Clone, Debug)]
pub struct Fatal {
    pub reason: String,
    pub stderr_tail: Vec<String>,
}

pub const NOTICE_TTL: Duration = Duration::from_secs(5);
/// How long a flash and the scrollbar stay after the event that showed them.
pub const FLASH_TTL: Duration = Duration::from_secs(1);
/// Presses this close together on one word make a double or triple click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// Delay before asking the Agent again after a busy or full-queue refusal.
pub const BUSY_RETRY: Duration = Duration::from_millis(700);

impl App {
    pub fn new(client: Arc<Client>, tx: Tx, workspace: PathBuf) -> Self {
        Self {
            remote: crate::remote::Remote::default(),
            client,
            tx,
            workspace,
            session: None,
            background: Vec::new(),
            background_op: false,
            draft_text: String::new(),
            ring: false,
            sessions: Vec::new(),
            catalog_generation: 0,
            models: Vec::new(),
            profiles: Vec::new(),
            launch: LaunchSettings::default(),
            default_profile: None,
            composer: composer::Composer::new(),
            composer_area: ratatui::layout::Rect::default(),
            composer_max_rows: crate::ui::composer::MIN_ROWS,
            composer_dragging: false,
            notices: VecDeque::new(),
            scroll: Scroll::Follow,
            tools_expanded: false,
            show_reasoning: true,
            quit: false,
            quit_armed: None,
            ready: false,
            fatal: None,
            started: Instant::now(),
            op_seq: 0,
            theme: ThemeKind::Dark,
            overlay: Overlay::None,
            styles: Styles::of(Palette::dark()),
            selection: None,
            press: None,
            scrollbar_grab: None,
            scrolled_at: None,
            flash: None,
            layout: Layout::default(),
            edit_request: None,
            loading_session: false,
            first_message: None,
            epoch: 0,
            templates: Vec::new(),
            home: None,
            composed: false,
            editor: Vec::new(),
            armed_unknown: None,
            notifier: crate::title::Notifier::default(),
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Remote(event) => self.on_remote(event),
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
                self.catalog_generation += 1;
                self.models = models;
                self.profiles = profiles;
                self.sessions = sessions;
                self.templates = templates::load(self.home.as_ref());
                self.ready = true;
            }
            Msg::Opened(id, result) => self.with_notice_for(&id, |app| {
                // Remote opens start from the catalog; deletion can finish before the open reply.
                if app.remote.opening.is_some()
                    && !app.sessions.iter().any(|info| info.session_id == id)
                {
                    app.loading_session = false;
                    app.remote.opening = None;
                    app.notice(NoticeLevel::Error, "Session deleted while opening");
                } else {
                    app.adopt(result, None);
                }
            }),
            Msg::Created(result) => {
                let first = self.first_message.take();
                self.adopt(result, first);
            }
            Msg::Deleted(id, result) => self.with_notice_for(&id, |app| {
                if app.ok(result).is_some() {
                    app.forget_phone_session(&id);
                    app.sessions.retain(|session| session.session_id != id);
                    app.background
                        .retain(|session| session.info.session_id != id);
                    app.catalog_generation += 1;
                    if let Overlay::Sessions { rows, .. } = &mut app.overlay {
                        rows.retain(|session| session.session_id != id);
                        // A zero step keeps the selection on a row that is still there.
                        app.overlay_move(0);
                    }
                }
            }),
            Msg::Renamed(id, result) => self.with_notice_for(&id, |app| app.on_renamed(result)),
            Msg::Session(epoch, op) => {
                if let Some(session) = self.session.as_ref()
                    && session.epoch == epoch
                {
                    let id = session.info.session_id.clone();
                    self.with_notice_for(&id, |app| app.on_op(*op));
                } else if let Some(index) = self
                    .background
                    .iter()
                    .position(|session| session.epoch == epoch)
                {
                    self.with_background(index, |app| app.on_op(*op));
                }
            }
            Msg::Notice(level, text) => self.notice(level, text),
            // An empty file is a deliberate blank draft, not a cancel.
            Msg::DraftEdited(draft) => self
                .composer
                .restore_text(draft.as_deref().unwrap_or_default()),
            Msg::Flash(text) => self.flash = Some((text, tokio::time::Instant::now())),
        }
    }

    /// Expires info notices, the flash and the scrollbar; errors stay until the next key press.
    pub fn tick(&mut self) {
        self.notices.retain(|notice| {
            notice.level == NoticeLevel::Error || notice.at.elapsed() < NOTICE_TTL
        });
        self.flash = self.flash.take().filter(|(_, at)| at.elapsed() < FLASH_TTL);
        self.scrolled_at = self.scrolled_at.filter(|at| at.elapsed() < FLASH_TTL);
        self.watch_context();
    }

    /// Whether the Working line's spinner changes with every frame.
    pub fn animating(&self) -> bool {
        self.status_label().is_some()
    }

    /// When the next flash, scrollbar or info notice goes away, which is the
    /// only frame they need.
    pub fn next_expiry(&self) -> Option<tokio::time::Instant> {
        let notices = self
            .notices
            .iter()
            .filter(|notice| notice.level == NoticeLevel::Info)
            .map(|notice| notice.at + NOTICE_TTL);
        let flash = self.flash.iter().map(|(_, at)| *at + FLASH_TTL);
        let scrollbar = self.scrolled_at.map(|at| at + FLASH_TTL);
        notices.chain(flash).chain(scrollbar).min()
    }

    /// The scrollbar shows while its thumb is held and briefly after a scroll.
    pub fn scrollbar_shown(&self) -> bool {
        self.scrollbar_grab.is_some() || self.scrolled_at.is_some_and(|at| at.elapsed() < FLASH_TTL)
    }

    /// Shows `text`; one about a background session names it.
    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        let label = match &self.remote.notice_target {
            crate::remote::NoticeTarget::Session(id) => {
                let session = self.live(id);
                let info = session
                    .map(|session| &session.info)
                    .or_else(|| self.sessions.iter().find(|info| &info.session_id == id));
                Some(info.map_or_else(
                    || overlay::short_session_id(id).to_owned(),
                    |info| overlay::session_label(info, session),
                ))
            }
            _ => None,
        };
        self.remote.notice(level, &text, label.as_deref());
        let text = match self.background_op {
            true => format!("\"{}\": {text}", self.session_label()),
            false => text,
        };
        self.push_notice(level, text);
    }

    pub(super) fn with_notice_for(&mut self, id: &str, op: impl FnOnce(&mut Self)) {
        let previous = std::mem::replace(
            &mut self.remote.notice_target,
            crate::remote::NoticeTarget::Session(id.to_owned()),
        );
        op(self);
        self.remote.notice_target = previous;
    }

    /// Shows `text` locally as it is.
    pub(super) fn push_notice(&mut self, level: NoticeLevel, text: String) {
        self.notices.push_back(Notice {
            level,
            text,
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

    /// The foreground session, for results `on_msg` has matched to it by epoch.
    pub(super) fn session_mut(&mut self) -> &mut Session {
        self.session
            .as_mut()
            .expect("the result's session is in the foreground")
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
        self.session.as_ref().is_some_and(Session::compacting)
    }

    /// Status-line text while something is in flight.
    pub fn status_label(&self) -> Option<&'static str> {
        self.session.as_ref().and_then(Session::status_label)
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
        self.scroll_to(top.saturating_add_signed(delta));
    }

    /// Holds the top of the view where it is, so a block that folds or unfolds
    /// grows or shrinks downward instead of pushing the rows above it up.
    pub fn pin_view(&mut self) {
        if self.scroll == Scroll::Follow {
            let (block, offset) = self.layout.anchor(self.layout.viewport.top);
            self.scroll = Scroll::Anchor { block, offset };
        }
    }

    /// Puts transcript row `top` at the top of the view; the bottom follows.
    pub fn scroll_to(&mut self, top: usize) {
        let layout = &self.layout;
        let max_top = layout.total_rows().saturating_sub(layout.viewport.height);
        self.scrolled_at = Some(tokio::time::Instant::now());
        if top >= max_top {
            self.scroll = Scroll::Follow;
            return;
        }
        let upwards = top <= layout.viewport.top;
        let (block, offset) = layout.anchor(top);
        self.scroll = Scroll::Anchor { block, offset };
        // Reaching the first block asks for the next page; a request in flight
        // makes holding the key harmless.
        if upwards && block <= 1 {
            self.load_older();
        }
    }

    /// The drag as text, from the rows the last frame drew.
    pub fn selected_text(&self, drag: selection::Drag) -> Option<String> {
        let text = selection::text(
            |row| {
                let line = self.layout.row_line(row)?;
                Some((line.to_string(), crate::ui::rails::content_cells(line)))
            },
            self.layout.viewport.area(),
            drag,
        );
        (!text.is_empty()).then_some(text)
    }

    /// Copies the diff or context report for `c`.
    pub fn copy_view(&mut self) {
        let text = match &self.overlay {
            Overlay::Diff(diff) => diff::text(diff),
            _ => views::context::text(self.session.as_ref(), self.styles),
        };
        if text.trim().is_empty() {
            self.notice(NoticeLevel::Info, "Nothing to copy here");
        } else {
            self.copy(text);
        }
    }
}
