//! Terminal state; Host owners remain authoritative for execution and persistence.

pub mod actions;
mod agent_cards;
pub mod commands;
pub mod composer;
mod control;
pub mod diff;
mod draft;
pub mod find;
pub mod keys;
mod live;
mod local_files;
mod mouse;
mod notify;
mod ops;
pub mod overlay;
pub mod picker;
pub(crate) mod resident;
pub mod scrollbar;
pub mod selection;
pub mod session;
pub(crate) mod tasks;
pub mod templates;
#[cfg(test)]
mod tests;
pub mod unconfirmed;

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use minicore_host_core::{
    AgentSummary, ModelDescription, ObservationEvent, SessionSnapshot, SessionSummary, StateEvent,
};
use minicore_host_protocol::*;
use minicore_host_transport::HostClient;

use crate::config::ThemeKind;
use crate::transcript::Block;
use crate::transcript::layout::Layout;
use crate::ui::theme::{Palette, Styles};
use crate::ui::transcript::RenderState;
use crate::ui::views;

pub use actions::{Action, Answer};
pub use overlay::{Overlay, Panel};
pub use session::{LiveStatus, LiveTurn, PendingInteraction, Session};
pub use tasks::{MESSAGE_CAPACITY, SessionOp, Tx, bootstrap};

pub enum Msg {
    Ready(Result<Ready, String>),
    Catalogs(Result<Ready, String>),
    RefreshCatalogs(Result<(), String>),
    SessionRenamed(SessionRef, Result<(), String>),
    HostState(Box<StateEvent>),
    HostObserved(Box<ObservationEvent>),
    HostBaseline(SubscribeResult<StateEvent>),
    HostObservationError(String),
    AgentCardRead(
        crate::transcript::agent_card::DelegationKey,
        Box<agent_cards::Read>,
    ),
    Opened(u64, SessionRef, Result<Box<tasks::Opened>, String>),
    Created(u64, Result<SessionRef, String>),
    CreationOpened(u64, SessionRef, Result<Box<tasks::Opened>, String>),
    AgentCreated(u64, Result<SessionRef, String>),
    Reloaded(Result<Box<Reloaded>, String>),
    Session(u64, Box<SessionOp>),
    LocalPushed(u64, Result<MessageAccepted, CallError>),
    Failed(String),
    Notice(NoticeLevel, String),
    DraftEdited(String),
    Flash(String),
}

pub struct Ready {
    pub models: Vec<ModelDescription>,
    pub agents: Vec<AgentSummary>,
    pub sessions: Vec<SessionSummary>,
    pub selected_agent: Option<AgentId>,
}

pub struct Reloaded {
    pub result: HostReloadResult,
    pub templates: Option<Vec<templates::Template>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchSettings {
    pub model: Option<ModelRef>,
    pub reasoning: Option<ReasoningPreference>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    /// How to use a command; not an operation error, so not kept for /error.
    Usage,
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
    Follow,
    Anchor { block: usize, offset: u16 },
}
#[derive(Clone, Debug)]
pub struct Press {
    pub cell: (u16, u16),
    pub at: tokio::time::Instant,
    pub clicks: u8,
    pub word: Option<Range<u16>>,
}
#[derive(Clone, Debug)]
pub struct Fatal {
    pub reason: String,
}

pub(super) struct PendingCreation {
    pub entry: Option<composer::Entry>,
    pub mode: MessageMode,
}

pub struct App {
    pub client: HostClient,
    pub host_state: Option<minicore_host_core::HostStateData>,
    pub tx: Tx,
    pub(super) tasks: tasks::Tasks,
    pub local_files: local_files::LocalFiles,
    pub workspace: PathBuf,
    /// `--workspace` or the startup directory. New Sessions use it; opening
    /// another Session only changes `workspace`.
    pub launch_workspace: PathBuf,
    /// A child Session opened from its delegation card, and the parent that
    /// Esc returns to while that child is current.
    pub return_to: Option<(SessionRef, SessionRef)>,
    pub resident: resident::Sessions,
    pub draft_text: String,
    pub ring: bool,
    pub sessions: Vec<SessionSummary>,
    pub agents: Vec<AgentSummary>,
    pub selected_agent: Option<AgentId>,
    pub(super) catalog_loading: bool,
    pub(super) catalog_due: bool,
    pub models: Vec<ModelDescription>,
    pub launch: LaunchSettings,
    pub composer: composer::Composer,
    pub local_inputs: VecDeque<unconfirmed::LocalInput>,
    pub unconfirmed_editor: composer::Composer,
    pub(super) pending_creations: HashMap<u64, PendingCreation>,
    pub composer_area: ratatui::layout::Rect,
    pub composer_max_rows: usize,
    pub composer_dragging: bool,
    pub notices: VecDeque<Notice>,
    pub last_error: Option<String>,
    pub scroll: Scroll,
    /// A fold pinned the view: its anchor holds even past the end, so a
    /// collapse closes toward the row clicked instead of pulling the view down.
    /// Scrolling releases it.
    pub held: bool,
    pub tools_expanded: bool,
    pub show_reasoning: bool,
    pub quit: bool,
    pub quit_armed: Option<tokio::time::Instant>,
    pub ready: bool,
    pub fatal: Option<Fatal>,
    pub started: Instant,
    pub op_seq: u64,
    pub theme: ThemeKind,
    pub overlay: Overlay,
    pub styles: Styles,
    pub selection: Option<selection::Drag>,
    pub press: Option<Press>,
    pub scrollbar_grab: Option<usize>,
    pub scrolled_at: Option<tokio::time::Instant>,
    pub flash: Option<(String, tokio::time::Instant)>,
    pub layout: Layout<RenderState>,
    pub edit_request: Option<String>,
    pub loading_session: bool,
    pub(super) open_generation: u64,
    pub(super) opening_target: Option<(u64, SessionRef)>,
    pub epoch: u64,
    pub templates: Vec<templates::Template>,
    pub config_directory: Option<crate::config::ConfigDirectory>,
    pub(super) reloading: bool,
    pub editor: Vec<String>,
    pub armed_unknown: Option<String>,
    pub notifier: crate::title::Notifier,
    pub(super) notice_target: Option<SessionRef>,
    pub(super) pending_states: HashMap<SessionRef, SessionSnapshot>,
    agent_cards: agent_cards::Cards,
    pub(super) agent_location: Option<(SessionRef, ToolCallRef)>,
}

pub const NOTICE_TTL: Duration = Duration::from_secs(5);
pub const FLASH_TTL: Duration = Duration::from_secs(1);
pub const DOUBLE_CLICK: Duration = Duration::from_millis(500);

impl App {
    pub fn new(client: HostClient, tx: Tx, workspace: PathBuf) -> Self {
        Self {
            client,
            host_state: None,
            tx,
            tasks: tasks::Tasks::default(),
            local_files: local_files::LocalFiles::default(),
            launch_workspace: workspace.clone(),
            return_to: None,
            workspace,
            resident: resident::Sessions::default(),
            draft_text: String::new(),
            ring: false,
            sessions: Vec::new(),
            agents: Vec::new(),
            selected_agent: None,
            catalog_loading: false,
            catalog_due: false,
            models: Vec::new(),
            launch: LaunchSettings::default(),
            composer: composer::Composer::new(),
            local_inputs: VecDeque::new(),
            unconfirmed_editor: composer::Composer::new(),
            pending_creations: HashMap::new(),
            composer_area: ratatui::layout::Rect::default(),
            composer_max_rows: crate::ui::composer::MIN_ROWS,
            composer_dragging: false,
            notices: VecDeque::new(),
            last_error: None,
            scroll: Scroll::Follow,
            held: false,
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
            open_generation: 0,
            opening_target: None,
            epoch: 0,
            templates: Vec::new(),
            config_directory: None,
            reloading: false,
            editor: Vec::new(),
            armed_unknown: None,
            notifier: crate::title::Notifier::default(),
            notice_target: None,
            pending_states: HashMap::new(),
            agent_cards: agent_cards::Cards::default(),
            agent_location: None,
        }
    }
    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::LocalPushed(id, result) => self.on_local_pushed(id, result),
            Msg::Failed(reason) => self.fatal = Some(Fatal { reason }),
            Msg::Ready(result) => match result {
                Ok(ready) => {
                    self.install_catalogs(ready);
                    self.templates = templates::load(self.config_directory.as_ref());
                    self.start_host_observation();
                }
                Err(reason) => self.fatal = Some(Fatal { reason }),
            },
            Msg::Catalogs(result) => {
                self.catalog_loading = false;
                match result {
                    Ok(ready) => self.install_catalogs(ready),
                    Err(error) => self.notice(NoticeLevel::Error, error),
                }
                if std::mem::take(&mut self.catalog_due) {
                    self.refresh_catalogs();
                }
            }
            Msg::SessionRenamed(target, result) => {
                if let Some(epoch) = self.resident.find(&target).map(|session| session.epoch) {
                    self.refresh_session_info_for(epoch);
                }
                self.with_notice_for(&target, |app| app.on_msg(Msg::RefreshCatalogs(result)));
            }
            Msg::RefreshCatalogs(result) => {
                if let Err(error) = result {
                    self.error(error);
                }
                self.refresh_catalogs();
            }
            Msg::HostState(state) => self.on_host_state(*state),
            Msg::HostObserved(event) => self.host_observed(*event),
            Msg::HostBaseline(baseline) => self.host_baseline(baseline),
            Msg::HostObservationError(error) => self.agent_observation_error(error),
            Msg::AgentCardRead(key, read) => self.agent_card_read(key, *read),
            Msg::Opened(generation, target, result) => {
                if self.opening_target.as_ref() != Some(&(generation, target.clone())) {
                    return;
                }
                self.opening_target = None;
                self.loading_session = false;
                match result {
                    Ok(opened) => self.adopt(*opened),
                    Err(error) => {
                        self.with_notice_for(&target, |app| app.notice(NoticeLevel::Error, error))
                    }
                }
            }
            Msg::Created(generation, result) => match result {
                Ok(target) => {
                    self.refresh_catalogs();
                    let client = self.client.clone();
                    self.spawn_msg(async move {
                        let result =
                            tasks::open(client, target.clone(), LaunchSettings::default()).await;
                        Msg::CreationOpened(generation, target, result)
                    });
                }
                Err(error) => self.creation_failed(generation, error),
            },
            Msg::CreationOpened(generation, target, result) => match result {
                Ok(opened) => {
                    let creation = self
                        .pending_creations
                        .remove(&generation)
                        .expect("creation owns its opened result");
                    let select = generation == self.open_generation;
                    if select {
                        self.loading_session = false;
                    }
                    let epoch = self.install_opened(*opened, select);
                    debug_assert_eq!(self.resident.get(epoch).unwrap().address(), target);
                    if let Some(entry) = creation.entry {
                        self.push_local_input(epoch, entry, creation.mode, None);
                    }
                }
                Err(error) => {
                    self.with_notice_for(&target, |app| app.creation_failed(generation, error))
                }
            },
            Msg::AgentCreated(generation, result) => match result {
                Ok(target) => {
                    if generation == self.open_generation {
                        self.selected_agent = Some(target.agent_id.clone());
                        self.select_session(None);
                        self.open_session(target);
                    }
                    self.refresh_catalogs();
                }
                Err(error) => self.notice(NoticeLevel::Error, error),
            },
            Msg::Reloaded(result) => {
                self.reloading = false;
                match result {
                    Ok(reloaded) => match reloaded.result {
                        HostReloadResult::Applied {
                            config_revision, ..
                        } => {
                            if let Some(templates) = reloaded.templates {
                                self.templates = templates;
                            }
                            self.refresh_catalogs();
                            self.notice(NoticeLevel::Info, format!("Host configuration {config_revision} and declared Agents applied"));
                        }
                        HostReloadResult::PartiallyApplied {
                            config_revision,
                            agents,
                        } => {
                            if let Some(templates) = reloaded.templates {
                                self.templates = templates;
                            }
                            self.refresh_catalogs();
                            let failed = agents
                                .iter()
                                .filter_map(|result| match result {
                                    AgentConfigurationResult::Failed { key, error, .. } => {
                                        Some(format!("{key}: {error}"))
                                    }
                                    AgentConfigurationResult::Created { key, receipt }
                                        if !result.succeeded() =>
                                    {
                                        let cause = [
                                            ("Agent", &receipt.result.steps.agent),
                                            ("default Session", &receipt.result.steps.session),
                                        ]
                                        .into_iter()
                                        .find_map(|(step, result)| match result {
                                            CreateStep::Failed { error } => {
                                                Some(format!("{step}: {error}"))
                                            }
                                            _ => None,
                                        })
                                        .unwrap_or_else(|| {
                                            "Agent or default Session creation is incomplete".into()
                                        });
                                        Some(format!("{key}: {cause}"))
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            self.notice(NoticeLevel::Error, format!("Host configuration {config_revision} applied with partial Agent results; inspect the Agent catalog. Failed declarations: {failed}"));
                        }
                        HostReloadResult::RestartRequired { fields, .. } => {
                            let fields = fields
                                .iter()
                                .map(|field| match field {
                                    RestartField::NetworkListen => "network listener",
                                    RestartField::Logging => "logging",
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            self.notice(NoticeLevel::Info, format!("Host configuration requires a restart for: {fields}. The current configuration remains active"));
                        }
                        _ => unreachable!("reload worker returns a completed native result"),
                    },
                    Err(error) => self.notice(NoticeLevel::Error, error),
                }
            }
            Msg::Session(epoch, op) => {
                if let Some(target) = self.resident.get(epoch).map(Session::address) {
                    self.with_notice_for(&target, |app| app.on_op(epoch, *op));
                }
            }
            Msg::Notice(level, text) => self.notice(level, text),
            Msg::DraftEdited(draft) => self.composer.restore_text(&draft),
            Msg::Flash(text) => self.flash = Some((text, tokio::time::Instant::now())),
        }
        self.retire_removed_sessions();
        self.retire_agent_observers();
        self.resolve_agent_location();
    }
    fn install_catalogs(&mut self, ready: Ready) {
        self.models = ready.models;
        self.agents = ready.agents;
        self.sessions = ready.sessions;
        if self
            .selected_agent
            .as_ref()
            .is_none_or(|id| !self.agents.iter().any(|agent| &agent.id == id))
        {
            self.selected_agent = ready
                .selected_agent
                .filter(|id| self.agents.iter().any(|agent| &agent.id == id))
                .or_else(|| self.agents.first().map(|agent| agent.id.clone()));
        }
        let observed: Vec<_> = self
            .pending_states
            .values()
            .cloned()
            .chain(self.resident.iter().map(|session| session.state.clone()))
            .collect();
        for snapshot in observed {
            self.project_catalog_state(&snapshot);
        }
        if let Overlay::Sessions { rows, .. } = &mut self.overlay {
            *rows = self.sessions.clone();
        }
        self.refresh_agent_names();
    }
    pub fn tick(&mut self) {
        self.notices.retain(|notice| {
            notice.level == NoticeLevel::Error || notice.at.elapsed() < NOTICE_TTL
        });
        self.flash = self.flash.take().filter(|(_, at)| at.elapsed() < FLASH_TTL);
        self.scrolled_at = self.scrolled_at.filter(|at| at.elapsed() < FLASH_TTL);
    }
    pub fn animating(&self) -> bool {
        self.status_label().is_some()
    }
    pub fn next_expiry(&self) -> Option<tokio::time::Instant> {
        self.notices
            .iter()
            .filter(|n| n.level == NoticeLevel::Info)
            .map(|n| n.at + NOTICE_TTL)
            .chain(self.flash.iter().map(|(_, at)| *at + FLASH_TTL))
            .chain(self.scrolled_at.map(|at| at + FLASH_TTL))
            .min()
    }
    pub fn scrollbar_shown(&self) -> bool {
        self.scrollbar_grab.is_some() || self.scrolled_at.is_some_and(|at| at.elapsed() < FLASH_TTL)
    }
    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        let background = self
            .notice_target
            .as_ref()
            .and_then(|target| self.resident.find(target))
            .filter(|s| !self.resident.is_current(s.epoch));
        let text = background.map_or_else(
            || text.clone(),
            |s| {
                format!(
                    "\"{}\": {text}",
                    s.info.title.as_deref().unwrap_or(s.info.id.as_str())
                )
            },
        );
        self.push_notice(level, text);
    }
    pub(super) fn with_notice_for(&mut self, target: &SessionRef, op: impl FnOnce(&mut Self)) {
        let previous = self.notice_target.replace(target.clone());
        op(self);
        self.notice_target = previous;
    }
    pub(super) fn push_notice(&mut self, level: NoticeLevel, text: String) {
        if level == NoticeLevel::Error {
            self.last_error = Some(match &self.notice_target {
                Some(target) => tasks::session_error(target, &text),
                None => text.clone(),
            });
        }
        self.notices.push_back(Notice {
            level,
            text,
            at: tokio::time::Instant::now(),
        });
        while self.notices.len() > 2 {
            self.notices.pop_front();
        }
    }
    pub fn error(&mut self, error: impl std::fmt::Display) {
        self.notice(NoticeLevel::Error, error.to_string());
    }
    pub(super) fn session_mut(&mut self, epoch: u64) -> &mut Session {
        self.resident
            .get_mut(epoch)
            .expect("matched resident Session")
    }
    pub fn can_operate(&self) -> bool {
        self.client.initial_info().can_operate
    }
    pub(super) fn require_control(&mut self) -> bool {
        if self.can_operate() {
            true
        } else {
            self.notice(NoticeLevel::Error, "This Host connection is read-only");
            false
        }
    }
    pub fn current_agent_id(&self) -> Option<&AgentId> {
        self.resident
            .current()
            .map(|s| &s.info.agent_id)
            .or(self.selected_agent.as_ref())
    }
    pub(super) fn next_operation(&mut self) -> u64 {
        self.op_seq = self
            .op_seq
            .checked_add(1)
            .expect("local operation sequence");
        self.op_seq
    }
    pub(super) fn request_quit(&mut self) {
        self.quit = true;
    }
    pub fn is_busy(&self) -> bool {
        self.resident.current().is_some_and(Session::is_busy)
    }

    pub fn creating_session(&self) -> bool {
        self.pending_creations.contains_key(&self.open_generation)
    }

    pub(crate) fn active_editor(&self) -> &composer::Composer {
        if matches!(
            self.overlay,
            Overlay::Unconfirmed {
                mode: unconfirmed::ReviewMode::Edit(_),
                ..
            }
        ) {
            return &self.unconfirmed_editor;
        }
        if self.overlay.is_none()
            && let Some(session) = self.resident.current()
            && session
                .interaction
                .as_ref()
                .is_some_and(|pending| pending.is_text())
        {
            return &session.interaction_editor;
        }
        &self.composer
    }

    pub(crate) fn active_editor_mut(&mut self) -> &mut composer::Composer {
        if matches!(
            self.overlay,
            Overlay::Unconfirmed {
                mode: unconfirmed::ReviewMode::Edit(_),
                ..
            }
        ) {
            return &mut self.unconfirmed_editor;
        }
        if self.overlay.is_none()
            && let Some(session) = self.resident.current_mut()
            && session
                .interaction
                .as_ref()
                .is_some_and(|pending| pending.is_text())
        {
            return &mut session.interaction_editor;
        }
        &mut self.composer
    }

    pub fn has_interaction(&self) -> bool {
        self.resident
            .current()
            .is_some_and(|session| session.interaction.is_some())
    }

    pub fn compacting(&self) -> bool {
        self.resident.current().is_some_and(Session::compacting)
    }

    /// Status-line text while something is in flight.
    pub fn status_label(&self) -> Option<&'static str> {
        self.resident.current().and_then(Session::status_label)
    }

    /// Elapsed time of the running turn, for the status line.
    pub fn busy_elapsed(&self) -> Option<Duration> {
        self.resident
            .current()
            .and_then(|session| session.live.as_ref())
            .map(|live| live.started.elapsed())
    }

    /// The transcript blocks, or an empty slice when no session is open.
    pub fn transcript_blocks(&self) -> &[Block] {
        self.resident
            .current()
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
        self.held = true;
    }

    /// Puts transcript row `top` at the top of the view; the bottom follows.
    pub fn scroll_to(&mut self, top: usize) {
        self.held = false;
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

    /// Copies the complete content of the current detail view for `c`.
    pub fn copy_view(&mut self) {
        let text = self.view_text();
        if text.trim().is_empty() {
            self.notice(NoticeLevel::Info, "Nothing to copy here");
        } else {
            self.copy(text);
        }
    }

    fn view_text(&self) -> String {
        match &self.overlay {
            Overlay::Diff(diff) => diff::text(diff),
            Overlay::Error { text, .. } => crate::wrap::sanitize(text).into_owned(),
            _ => views::context::text(self.resident.current(), self.styles),
        }
    }
}
