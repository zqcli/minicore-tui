//! User-visible operations. Key bindings and slash commands both land in
//! [`App::on_action`], which starts the IO the results of which `ops` applies.

use serde_json::{Value, json};

use super::draft::NO_SESSION;
use super::operation::Operation;
use super::{App, InteractionKind, Msg, NoticeLevel, Scroll, SessionOp, commands, read, tasks};
use crate::config::ThemeKind;
use crate::protocol::{self, InputMode, Reasoning};
use crate::title::LONG_TURN;
use crate::ui::theme::{Palette, Styles};

/// How often the context report is asked for while a turn runs.
const CONTEXT_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Enter: steer, which starts a turn when idle; with no draft, resume a paused queue.
    Submit,
    /// Alt+Enter or Ctrl+Enter: queue a follow-up.
    QueueFollowUp,
    OpenSession(String),
    NewSession,
    /// `/new <profile>`.
    NewSessionWith(String),
    DeleteSession(String),
    Rename(String),
    SetModel(String),
    /// `/model <text>`: the text names a model, which may take some resolving.
    SwitchModel(String),
    /// Ctrl+P, or Shift+Ctrl+P backwards.
    CycleModel {
        forward: bool,
    },
    SetReasoning(Reasoning),
    SetTheme(ThemeKind),
    ToggleTheme,
    Compact,
    CopyLastAnswer,
    Export(Option<String>),
    EditDraft,
    ReloadAgent,
    RetryRecovery,
    Answer(Answer),
    LoadOlder,
    RecallQueued,
    Quit,
    Remote(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    /// Allow now and answer this tool's later approvals without asking.
    AllowTool,
    Deny,
    /// The row picked from the prompt's options; for an approval, row 0 allows.
    Choice(usize),
    Text(String),
}

impl Action {
    /// Whether the action works on a session, so a draft has nothing for it.
    fn needs_session(&self) -> bool {
        matches!(
            self,
            Self::Rename(_)
                | Self::Compact
                | Self::CopyLastAnswer
                | Self::Export(_)
                | Self::LoadOlder
        )
    }
}

impl App {
    pub fn on_action(&mut self, action: Action) {
        if !self.local_allowed(&action) {
            self.remote_flash();
            return;
        }
        if self.resident.current().is_none() && action.needs_session() {
            self.notice(NoticeLevel::Info, NO_SESSION);
            return;
        }
        match action {
            Action::Submit => self.submit_composer(false),
            Action::QueueFollowUp => self.submit_composer(true),
            Action::OpenSession(id) => self.open_session(id),
            Action::NewSession => self.new_draft(None),
            Action::NewSessionWith(profile) if self.profiles.iter().any(|p| p.id == profile) => {
                self.new_draft(Some(profile));
            }
            Action::NewSessionWith(profile) => {
                self.notice(NoticeLevel::Error, format!("Unknown profile {profile}"));
            }
            Action::DeleteSession(id) => {
                let client = self.client.clone();
                let load = self.live(&id).map(|session| session.load.clone());
                self.spawn_msg(tasks::delete_session(client, id, load));
            }
            Action::Rename(title) => self.rename(&title),
            Action::SetModel(id) => self.set_model(id),
            Action::SwitchModel(text) => match commands::resolve_model(&self.models, &text) {
                Ok(model) => self.set_model(model.id.clone()),
                Err(message) => self.notice(NoticeLevel::Error, message),
            },
            Action::CycleModel { forward } => self.cycle_model(forward),
            Action::SetReasoning(level) => self.set_reasoning(level),
            Action::Compact => self.compact(),
            Action::CopyLastAnswer => self.copy_last_answer(),
            Action::Export(path) => {
                if let Some(session) = self.resident.current() {
                    let client = self.client.clone();
                    let session_id = session.info.session_id.clone();
                    self.spawn_msg(tasks::export(client, session_id, path));
                }
            }
            Action::EditDraft => self.edit_request = Some(self.composer.take_for_editor()),
            Action::Answer(answer) => self.answer(answer),
            Action::LoadOlder => self.load_older(),
            Action::RecallQueued => self.recall_queued(),
            Action::RetryRecovery => {
                if let Some(session) = self.resident.current_mut() {
                    session.recovery.retry_failed();
                    self.refresh_control();
                    self.recover_completions();
                }
            }
            Action::SetTheme(kind) => self.apply_theme(kind),
            Action::ToggleTheme => self.apply_theme(match self.theme {
                ThemeKind::Dark => ThemeKind::Light,
                ThemeKind::Light => ThemeKind::Dark,
            }),
            Action::ReloadAgent => {
                self.local_files
                    .reload(&self.tx, self.home.clone(), self.composed)
            }
            Action::Quit => self.request_quit(),
            Action::Remote(command) => self.remote_command(&command),
        }
    }

    fn submit_composer(&mut self, follow: bool) {
        if let super::Overlay::Unconfirmed { mode, .. } = &mut self.overlay {
            if let super::unconfirmed::ReviewMode::Edit(id) = *mode {
                *mode = super::unconfirmed::ReviewMode::ResendEdited(id);
            }
            return;
        }
        if self.resident.current().is_none() && (!self.ready || self.loading_session) {
            return;
        }
        if !self.composer.text().trim_end().is_empty() {
            if !self.local_input_room(
                self.composer.text().len() + std::mem::size_of_val(self.composer.pastes()),
            ) {
                self.notice(
                    NoticeLevel::Error,
                    "Local input confirmation budget is full; F8 to resolve retained inputs",
                );
                return;
            }
            let entry = self.composer.take_submission();
            self.submit_entry(entry, follow, None);
        } else if !follow && self.resident.current().is_some_and(|s| s.queue.paused) {
            self.call(protocol::METHOD_INPUT_RESUME, json!({}), SessionOp::Resumed);
        }
    }

    pub(super) fn submit_text(&mut self, text: String, follow: bool) {
        self.submit_entry(super::composer::Entry::new(text), follow, None);
    }

    pub(super) fn submit_entry(
        &mut self,
        mut entry: super::composer::Entry,
        follow: bool,
        retry_of: Option<u64>,
    ) {
        if self.resident.current().is_none() {
            if !self.local_input_room(entry.retained_bytes() + entry.text().len()) {
                entry.discard_rollback();
            }
            if !self.local_input_room(entry.retained_bytes() + entry.text().len()) {
                self.composer.restore_submission(entry);
                self.notice(
                    NoticeLevel::Error,
                    "Local input confirmation budget is full; F8 to resolve retained inputs",
                );
                return;
            }
            self.first_message = Some(entry.text().to_owned());
            self.first_submission = Some(entry);
            self.create_session();
        } else {
            if !self.is_busy() {
                self.scroll = Scroll::Follow;
            }
            let mode = match follow {
                true => InputMode::FollowUp,
                false => InputMode::Steer,
            };
            self.push_local_input(
                self.resident.current().unwrap().epoch,
                entry,
                mode,
                retry_of,
            );
        }
    }

    /// Esc on a running turn asks the Agent to cancel it; `turn.wait` ends it.
    pub(super) fn interrupt(&mut self) {
        let Some(live) = self
            .resident
            .current_mut()
            .and_then(|session| session.live.as_mut())
            .filter(|live| !live.interrupting)
        else {
            return;
        };
        live.interrupting = true;
        let operation = Operation::CancelTurn(live.turn.loop_id.clone());
        self.operate(self.resident.current().unwrap().epoch, operation);
    }

    /// Esc: interrupt a running turn, else cancel a compaction, else clear the draft.
    pub fn escape(&mut self) {
        if self.is_busy() {
            self.interrupt();
        } else if let Some(progress) = self
            .resident
            .current()
            .and_then(|session| session.compaction.as_ref())
        {
            self.operate(
                self.resident.current().unwrap().epoch,
                Operation::CancelCompaction(progress.operation_id.clone()),
            );
        } else {
            self.composer.clear();
        }
    }

    /// Asks for the context report now and then while a turn runs, because a
    /// turn-start compaction has no event.
    pub fn watch_context(&mut self) {
        let Some(session) = self
            .resident
            .current_mut()
            .filter(|session| session.is_busy())
        else {
            return;
        };
        if session
            .context_asked
            .is_some_and(|asked| asked.elapsed() < CONTEXT_EVERY)
        {
            return;
        }
        session.context_asked = Some(tokio::time::Instant::now());
        self.refresh_context();
    }

    pub(super) fn create_session(&mut self) {
        self.opening_target = None;
        self.loading_session = true;
        // Without a profile the Agent picks its configured default; choosing
        // one here would override that.
        let params = self.create_fields();
        let client = self.client.clone();
        self.spawn_msg(async move {
            Msg::Created(tasks::session(client, protocol::METHOD_SESSION_CREATE, params).await)
        });
    }

    pub(super) fn request_session_open(&mut self, id: String) {
        self.open_generation = self
            .open_generation
            .checked_add(1)
            .expect("open generation exhausted");
        let generation = self.open_generation;
        self.opening_target = Some((generation, id.clone()));
        self.loading_session = true;
        let client = self.client.clone();
        self.spawn_msg(async move {
            let params = json!({"session_id":id});
            Msg::Opened(
                generation,
                id,
                tasks::session(client, protocol::METHOD_SESSION_OPEN, params).await,
            )
        });
    }

    pub(super) fn rename(&self, title: &str) {
        if let Some(session) = self.resident.current() {
            self.rename_for(session.epoch, title);
        }
    }

    pub(super) fn rename_for(&self, epoch: u64, title: &str) {
        if !title.trim().is_empty() {
            self.operate(epoch, Operation::Rename(title.to_owned()));
        }
    }

    pub(super) fn update_session(&self, fields: Value) {
        if let Some(session) = self.resident.current() {
            self.operate(session.epoch, Operation::Update(fields));
        }
    }

    /// Ctrl+P: the next model in the catalog, wrapping at the end.
    fn cycle_model(&mut self, forward: bool) {
        if self.models.len() < 2 {
            self.notice(NoticeLevel::Info, "Only one model is available");
            return;
        }
        let current = self.model_id();
        let at = self
            .models
            .iter()
            .position(|model| Some(model.id.as_str()) == current);
        let delta = if forward { 1 } else { -1 };
        let next = at.map_or(0, |at| {
            crate::app::picker::step(at, delta, self.models.len())
        });
        self.set_model(self.models[next].id.clone());
    }

    pub(super) fn compact(&mut self) {
        let Some(session) = self.resident.current_mut() else {
            return;
        };
        if session.is_busy() {
            self.notice(NoticeLevel::Error, "Finish the running turn first");
            return;
        }
        let epoch = session.epoch;
        let operation = self.new_compaction();
        self.operate(epoch, operation);
    }

    fn copy_last_answer(&mut self) {
        let text = self
            .resident
            .current()
            .and_then(|session| session.transcript.last_answer())
            .map(str::to_owned);
        match text {
            Some(text) => self.copy(text),
            None => self.notice(NoticeLevel::Error, "No answer to copy yet"),
        }
    }

    /// Switches the palette and persists the choice, so `/theme` survives a
    /// restart. A failed write is a notice, not a reason to keep the old theme.
    fn apply_theme(&mut self, kind: ThemeKind) {
        if let Some(home) = &self.home {
            self.local_files.theme(&self.tx, home.config_file(), kind);
        }
        self.theme = kind;
        self.styles = Styles::of(Palette::for_kind(kind));
        self.layout.clear();
    }

    /// The editor command: config first, then `$VISUAL`/`$EDITOR`.
    pub fn editor_command(&self) -> Result<Vec<String>, String> {
        crate::config::editor_command(&self.editor).ok_or_else(|| {
            "No editor configured: set `editor` under [ui] in config.toml or $VISUAL/$EDITOR"
                .to_owned()
        })
    }

    /// Takes the draft Ctrl+G asked the main loop to edit; the terminal
    /// belongs to the loop, which reports back through `Msg::DraftEdited`.
    pub fn take_edit_request(&mut self) -> Option<String> {
        self.edit_request.take()
    }

    pub(super) fn answer(&mut self, answer: Answer) {
        if let Some(epoch) = self.resident.current().map(|session| session.epoch) {
            self.answer_for(epoch, answer);
        }
    }

    pub(super) fn answer_for(&mut self, epoch: u64, answer: Answer) {
        let Some(interaction) = self
            .resident
            .get(epoch)
            .and_then(|session| session.interaction.as_ref())
        else {
            return;
        };
        match super::operation::answer(interaction, answer) {
            Ok(Some(operation)) => {
                self.operate(epoch, operation);
                self.refresh_control_for(epoch);
            }
            Ok(None) => {}
            Err(message) => self.notice(NoticeLevel::Error, message),
        }
    }

    /// A new approval: answered silently for a tool allowed for the session,
    /// else its card is read for the input it would run, which an `edit`
    /// card only has once it has run.
    pub(super) fn on_approval_requested(&mut self, epoch: u64) {
        let Some(session) = self.resident.get(epoch) else {
            return;
        };
        let Some(interaction) = session.interaction.as_ref() else {
            return;
        };
        let approval = matches!(interaction.kind, InteractionKind::Approval);
        if approval && session.allowed_tools.contains(&interaction.tool_name) {
            self.answer_for(epoch, Answer::Allow);
            return;
        }
        let card = session
            .transcript
            .find_tool_by_call_id(&interaction.tool_call_id)
            .filter(|call| call.expanded_input.is_none());
        if let Some(call) = card {
            let key = call.key.clone();
            let hydrated = key.clone();
            self.read_tool(&key, move |read| {
                SessionOp::ToolHydrated(hydrated, read.ok())
            });
        }
        if !self.resident.is_current(epoch) {
            self.background_approval(epoch);
        } else {
            self.ring |= self
                .resident
                .get(epoch)
                .and_then(|session| session.live.as_ref())
                .map(|live| live.started.elapsed())
                >= Some(LONG_TURN);
        }
    }

    pub(super) fn load_older_for(&mut self, epoch: u64) {
        let Some(session) = self.resident.get_mut(epoch) else {
            return;
        };
        if session.loading_older || session.loaded_from == 0 {
            return;
        }
        session.loading_older = true;
        // Below the display floor the transcript continues in the canonical
        // history, behind the compaction summary.
        let before = session.hidden_items();
        let window = if session.loaded_from <= session.first_item {
            read::Window::CanonicalBefore(before)
        } else {
            read::Window::Before(before)
        };
        self.read_history_for(epoch, window);
    }
    pub fn load_older(&mut self) {
        if let Some(epoch) = self.resident.current().map(|session| session.epoch) {
            self.load_older_for(epoch);
        }
    }

    /// Reads a history window for the foreground session.
    pub(super) fn read_history_for(&self, epoch: u64, window: read::Window) {
        let Some(session) = self.resident.get(epoch) else {
            return;
        };
        let (client, session_id) = (self.client.clone(), session.info.session_id.clone());
        self.spawn_for(epoch, async move {
            let tail = matches!(&window, read::Window::Tail);
            let history = read::read_history(client, session_id, window).await;
            if tail {
                SessionOp::Tail(history)
            } else {
                SessionOp::Page(history)
            }
        });
    }

    /// Asks for a fresh context report.
    pub fn refresh_context_for(&self, epoch: u64) {
        self.call_for(
            epoch,
            protocol::METHOD_SESSION_CONTEXT,
            json!({}),
            SessionOp::Context,
        );
    }
    pub fn refresh_context(&self) {
        if let Some(epoch) = self.resident.current().map(|session| session.epoch) {
            self.refresh_context_for(epoch);
        }
    }

    /// Asks for the footer's context usage.
    pub(super) fn refresh_presentation_for(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        session.presentation_asked = Some(tokio::time::Instant::now());
        session.presentation_in_flight = true;
        self.call_for(
            epoch,
            protocol::METHOD_SESSION_PRESENTATION,
            json!({}),
            SessionOp::Presentation,
        );
    }

    /// Refreshes branch and context independently; periodic usage reads never run Git.
    pub(super) fn refresh_session_details_for(&mut self, epoch: u64) {
        self.refresh_presentation_for(epoch);
        self.call_for(
            epoch,
            protocol::METHOD_WORKSPACE_STATUS,
            json!({}),
            SessionOp::WorkspaceStatus,
        );
    }

    /// Alt+↑ takes every queued message back to an empty composer.
    fn recall_queued(&mut self) {
        if !self.composer.is_empty() {
            self.notice(
                NoticeLevel::Info,
                "Clear the draft before recalling a queued message",
            );
        } else if self
            .resident
            .current()
            .is_some_and(|s| !s.queue.items.is_empty())
        {
            self.recall_queue();
        }
    }

    /// Asks the Agent to give back every queued message.
    pub(super) fn recall_queue(&self) {
        self.call(
            protocol::METHOD_INPUT_RECALL,
            json!({}),
            SessionOp::Recalled,
        );
    }

    /// Returns refused text to the operation that sent it, else to the session, else to the composer.
    ///
    /// The only routing identity is the operation: text never follows whichever
    /// device most recently sent something to the session.
    pub(super) fn restore_draft(&mut self, text: &str) {
        if let Some(session) = self.resident.current_mut() {
            if self.remote.active() {
                session.returned.push(text.to_owned());
                return;
            }
            session.draft = Some(text.to_owned());
        }
        self.restore_to_composer(text);
    }

    /// Puts `text` in the composer, behind a draft typed meanwhile.
    pub(super) fn restore_to_composer(&mut self, text: &str) {
        if self.composer.is_empty() {
            self.composer.restore_text(text);
        } else {
            self.composer.insert(text);
        }
    }

    /// Copies `text` through the clipboard; success flashes, failure is a notice.
    pub fn copy(&mut self, text: String) {
        self.spawn_msg(async move {
            match crate::clipboard::copy(&text).await {
                Ok(crate::clipboard::Copied::Native) => Msg::Flash("Copied!".into()),
                Ok(crate::clipboard::Copied::Osc52) => Msg::Flash("Copied via OSC 52".into()),
                Err(error) => Msg::Notice(NoticeLevel::Error, error),
            }
        });
    }
}
