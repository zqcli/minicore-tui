//! User-visible operations. Key bindings and slash commands both land in
//! [`App::on_action`], which starts the IO the results of which `ops` applies.

use serde_json::{Value, json};

use super::draft::NO_SESSION;
use super::{
    App, InteractionKind, Msg, NoticeLevel, Scroll, SessionOp, commands, tasks, templates,
};
use crate::config::ThemeKind;
use crate::protocol::{
    self, CompactionPhase, CompactionProgress, InputMode, Reasoning, SessionResult,
};
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
        if self.session.is_none() && action.needs_session() {
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
                self.spawn_msg(tasks::delete_session(client, id));
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
                if let Some(session) = self.session.as_ref() {
                    let client = self.client.clone();
                    let session_id = session.info.session_id.clone();
                    self.spawn_msg(tasks::export(client, session_id, path));
                }
            }
            Action::EditDraft => self.edit_request = Some(self.composer.submit()),
            Action::Answer(answer) => self.answer(answer),
            Action::LoadOlder => self.load_older(),
            Action::RecallQueued => self.recall_queued(),
            Action::SetTheme(kind) => self.apply_theme(kind),
            Action::ToggleTheme => self.apply_theme(match self.theme {
                ThemeKind::Dark => ThemeKind::Light,
                ThemeKind::Light => ThemeKind::Dark,
            }),
            Action::ReloadAgent => {
                self.templates = templates::load(self.home.as_ref());
                if let Some(home) = self.home.as_ref().filter(|_| self.composed)
                    && let Err(error) =
                        crate::config::load(home).and_then(|loaded| loaded.write(home))
                {
                    self.notice(NoticeLevel::Error, error.to_string());
                    return;
                }
                let client = self.client.clone();
                self.spawn_msg(async move {
                    match client
                        .call::<_, Value>(protocol::METHOD_RELOAD, &Value::Null)
                        .await
                    {
                        Ok(_) => Msg::Notice(
                            NoticeLevel::Info,
                            "Agent reloaded; the new system prompt applies from the next turn"
                                .into(),
                        ),
                        Err(error) => Msg::Notice(NoticeLevel::Error, error.message),
                    }
                });
            }
            Action::Quit => self.request_quit(),
            Action::Remote(command) => self.remote_command(&command),
        }
    }

    fn submit_composer(&mut self, follow: bool) {
        if self.session.is_none() && (!self.ready || self.loading_session) {
            return;
        }
        let text = self.composer.submit().trim_end().to_owned();
        if !text.is_empty() {
            self.submit_text(text, follow);
        } else if !follow && self.session.as_ref().is_some_and(|s| s.queue.paused) {
            self.call(protocol::METHOD_INPUT_RESUME, json!({}), SessionOp::Resumed);
        }
    }

    pub(super) fn submit_text(&mut self, text: String, follow: bool) {
        if self.session.is_none() {
            self.first_message = Some(text);
            self.create_session();
        } else {
            if !self.is_busy() {
                self.scroll = Scroll::Follow;
            }
            let mode = match follow {
                true => InputMode::FollowUp,
                false => InputMode::Steer,
            };
            self.push_input(text, mode);
        }
    }

    /// Hands `text` to the Agent's queue; its answer says whether a turn started.
    pub(super) fn push_input(&self, text: String, mode: InputMode) {
        self.call(
            protocol::METHOD_INPUT_PUSH,
            json!({ "mode": mode, "text": text }),
            move |result| SessionOp::Pushed(text, mode, result),
        );
    }

    /// Esc on a running turn asks the Agent to cancel it; `turn.wait` ends it.
    pub(super) fn interrupt(&mut self) {
        let Some(live) = self
            .session
            .as_mut()
            .and_then(|session| session.live.as_mut())
            .filter(|live| !live.interrupting)
        else {
            return;
        };
        live.interrupting = true;
        let fields = json!({ "loop_id": live.turn.loop_id });
        self.call(protocol::METHOD_TURN_CANCEL, fields, SessionOp::Done);
    }

    /// Esc: interrupt a running turn, else cancel a compaction, else clear the draft.
    pub fn escape(&mut self) {
        if self.is_busy() {
            self.interrupt();
        } else if let Some(progress) = self
            .session
            .as_ref()
            .and_then(|session| session.compaction.as_ref())
        {
            let fields = json!({ "operation_id": progress.operation_id });
            self.call(
                protocol::METHOD_SESSION_COMPACT_CANCEL,
                fields,
                SessionOp::Done,
            );
        } else {
            self.composer.clear();
        }
    }

    /// Asks for the context report now and then while a turn runs, because a
    /// turn-start compaction has no event.
    pub fn watch_context(&mut self) {
        let Some(session) = self.session.as_mut().filter(|session| session.is_busy()) else {
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
        self.loading_session = true;
        // Without a profile the Agent picks its configured default; choosing
        // one here would override that.
        let mut params = self.launch.fields(true);
        params["workspace"] = json!(self.workspace);
        let client = self.client.clone();
        self.spawn_msg(async move {
            Msg::Created(tasks::session(client, protocol::METHOD_SESSION_CREATE, params).await)
        });
    }

    pub(super) fn rename(&self, title: &str) {
        let title = title.trim();
        if !title.is_empty() {
            self.call(
                protocol::METHOD_SESSION_RENAME,
                json!({ "title": title }),
                |result: Result<SessionResult, _>| SessionOp::Info(result.map(|r| r.session)),
            );
        }
    }

    /// `session.update` with the model or reasoning level in `fields`.
    pub(super) fn update_session(&self, fields: Value) {
        self.call(
            protocol::METHOD_SESSION_UPDATE,
            fields,
            |result: Result<SessionResult, _>| SessionOp::Info(result.map(|r| r.session)),
        );
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
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.is_busy() {
            self.notice(NoticeLevel::Error, "Finish the running turn first");
            return;
        }
        self.op_seq += 1;
        let operation_id = format!("tui-{}", self.op_seq);
        session.compaction = Some(CompactionProgress {
            operation_id: operation_id.clone(),
            phase: CompactionPhase::Preparing,
            covered_item_count: 0,
            retained_item_count: 0,
        });
        self.call(
            protocol::METHOD_SESSION_COMPACT,
            json!({ "operation_id": operation_id }),
            SessionOp::Compacted,
        );
    }

    fn copy_last_answer(&mut self) {
        let text = self
            .session
            .as_ref()
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
        if let Some(home) = &self.home
            && let Err(error) = crate::config::save_theme(&home.config_file(), kind)
        {
            self.notice(NoticeLevel::Error, error);
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
        let Some(interaction) = self
            .session
            .as_mut()
            .and_then(|session| session.interaction.take())
        else {
            return;
        };
        let decision = |decision: &str| json!({"type": "approval", "decision": decision});
        let wire = match (answer, &interaction.kind) {
            (Answer::Choice(index), InteractionKind::Choice(_)) => {
                json!({"type": "choice", "index": index})
            }
            (Answer::AllowTool | Answer::Choice(2), InteractionKind::Approval) => {
                if let Some(session) = self.session.as_mut() {
                    session.allowed_tools.insert(interaction.tool_name.clone());
                }
                decision("allow_once")
            }
            (Answer::Allow | Answer::AllowTool | Answer::Choice(0), _) => decision("allow_once"),
            (Answer::Deny | Answer::Choice(_), _) => decision("deny"),
        };
        let fields = json!({
            "loop_id": interaction.turn.loop_id,
            "interaction_id": interaction.interaction_id,
            "answer": wire,
        });
        self.call(protocol::METHOD_INTERACTION_ANSWER, fields, SessionOp::Done);
    }

    /// A new approval: answered silently for a tool allowed for the session,
    /// else its card is read for the input it would run, which an `edit`
    /// card only has once it has run.
    pub(super) fn on_approval_requested(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some(interaction) = session.interaction.as_ref() else {
            return;
        };
        let approval = matches!(interaction.kind, InteractionKind::Approval);
        if approval && session.allowed_tools.contains(&interaction.tool_name) {
            self.answer(Answer::Allow);
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
        if self.background_op {
            self.background_approval();
        } else {
            self.ring |= self.busy_elapsed() >= Some(LONG_TURN);
        }
    }

    pub(super) fn load_older(&mut self) {
        let Some(session) = self.session.as_mut() else {
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
            tasks::Window::CanonicalBefore(before)
        } else {
            tasks::Window::Before(before)
        };
        self.read_history(window);
    }

    /// Reads a history window for the foreground session.
    pub(super) fn read_history(&self, window: tasks::Window) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let (client, session_id) = (self.client.clone(), session.info.session_id.clone());
        self.spawn(async move {
            let history = tasks::read_history(client, session_id, window).await;
            match window {
                tasks::Window::Tail => SessionOp::Tail(history),
                _ => SessionOp::Page(history),
            }
        });
    }

    /// Asks for a fresh context report.
    pub fn refresh_context(&self) {
        self.call(
            protocol::METHOD_SESSION_CONTEXT,
            json!({}),
            SessionOp::Context,
        );
    }

    /// Asks whether the session is mid-turn or compacting.
    pub(super) fn read_state(&self) {
        self.call(protocol::METHOD_SESSION_STATE, json!({}), SessionOp::State);
    }

    /// Asks for the footer's figures: model label, branch, context usage.
    pub(super) fn refresh_presentation(&mut self) {
        let session = self.session_mut();
        session.presentation_asked = Some(tokio::time::Instant::now());
        session.presentation_in_flight = true;
        self.call(
            protocol::METHOD_SESSION_PRESENTATION,
            json!({}),
            SessionOp::Presentation,
        );
    }

    /// The Agent reports a branch only after a `workspace.status`, so one runs
    /// before the presentation is read again.
    pub(super) fn refresh_branch(&mut self) {
        self.call(
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
            .session
            .as_ref()
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

    /// Asks for the queue, which a session opened later may already have.
    pub(super) fn read_queue(&self) {
        self.call(protocol::METHOD_INPUT_LIST, json!({}), SessionOp::Queue);
    }

    /// Returns refused text to its remote sender or session, otherwise appending it to the local composer.
    pub(super) fn restore_draft(&mut self, text: &str) {
        if let Some(session) = &mut self.session {
            if self
                .remote
                .returned(&session.info.session_id, session.last_sender, text)
            {
                return;
            }
            if self.background_op || self.remote.active() {
                session.returned.push(text.to_owned());
                return;
            }
        } else if let crate::remote::NoticeTarget::Phone(connection) = self.remote.notice_target
            && self
                .remote
                .phones
                .iter()
                .any(|phone| phone.connection.id == connection)
        {
            self.remote
                .send(Some(connection), json!({"t":"returned", "text":text}));
            return;
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
