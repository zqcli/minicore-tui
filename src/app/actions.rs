//! User-visible operations. Key bindings and slash commands both land in
//! [`App::on_action`], which starts the IO the results of which `ops` applies.

use serde_json::{Value, json};

use super::{App, InteractionKind, Msg, Queued, QueuedKind, Scroll, Session, SessionOp, tasks};
use crate::config::ThemeKind;
use crate::protocol::{
    self, CompactionPhase, CompactionProgress, Reasoning, SessionResult, TurnSendResult,
};
use crate::transcript::{Block, NoticeLevel};
use crate::ui::theme::{Palette, Styles};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Enter: send when idle, steer when a turn runs.
    Submit,
    /// Alt+Enter: queue a follow-up.
    QueueFollowUp,
    OpenSession(String),
    NewSession,
    DeleteSession(String),
    Rename(String),
    SetModel(String),
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
    /// `/tool`: open the full output of the tool card in view.
    ShowTool,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Deny,
    /// The row picked from the prompt's options; for an approval, row 0 allows.
    Choice(usize),
}

impl App {
    pub fn on_action(&mut self, action: Action) {
        match action {
            Action::Submit => self.submit(),
            Action::QueueFollowUp => self.submit_follow_up(),
            Action::OpenSession(id) => {
                self.loading_session = true;
                let client = self.client.clone();
                self.spawn_msg(async move {
                    let params = json!({ "session_id": id });
                    Msg::Opened(tasks::session(client, protocol::METHOD_SESSION_OPEN, params).await)
                });
            }
            Action::NewSession => self.create_session(),
            Action::DeleteSession(id) => {
                let client = self.client.clone();
                self.spawn_msg(tasks::delete_session(client, id));
            }
            Action::Rename(title) => self.rename(&title),
            Action::SetModel(id) => self.update_session(json!({ "model": id })),
            Action::SetReasoning(level) => self.update_session(json!({ "reasoning": level })),
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
            Action::ShowTool => self.show_tool(),
            Action::RecallQueued => self.recall_queued(),
            Action::SetTheme(kind) => self.apply_theme(kind),
            Action::ToggleTheme => self.apply_theme(match self.theme {
                ThemeKind::Dark => ThemeKind::Light,
                ThemeKind::Light => ThemeKind::Dark,
            }),
            Action::ReloadAgent => {
                let client = self.client.clone();
                self.spawn_msg(async move {
                    match client
                        .call::<_, Value>(protocol::METHOD_RELOAD, &Value::Null)
                        .await
                    {
                        Ok(_) => Msg::Notice(NoticeLevel::Info, "Agent reloaded".into()),
                        Err(error) => Msg::Notice(NoticeLevel::Error, error.message),
                    }
                });
            }
            Action::Quit => self.quit = true,
        }
    }

    /// Enter: send when idle, steer when a turn is running. Without a session
    /// it starts one and keeps the draft.
    fn submit(&mut self) {
        if self.session.is_none() {
            if !self.loading_session {
                self.create_session();
            }
            return;
        }
        let text = self.composer.submit().trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        if self.is_busy() {
            self.steer(text);
        } else {
            self.send(text);
        }
    }

    /// Alt+Enter: queue a follow-up; when idle it just sends.
    fn submit_follow_up(&mut self) {
        let text = self.composer.submit().trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        match self.session.as_mut().filter(|session| session.is_busy()) {
            Some(session) => session.push_follow_up(text),
            None => self.send(text),
        }
    }

    fn send(&mut self, text: String) {
        let Some(session) = self.session.as_mut() else {
            self.composer.restore_text(&text);
            self.notice(NoticeLevel::Error, "No session is open");
            return;
        };
        // The durable history echoes this prompt, so the turn's region starts
        // before it: reconciling replaces the prompt rather than doubling it.
        session.next_turn_start = Some(session.transcript.len());
        session.transcript.push(Block::User {
            text: text.clone(),
            kind: protocol::read::UserKind::Prompt,
        });
        self.scroll = Scroll::Follow;
        self.call(
            protocol::METHOD_TURN_SEND,
            json!({ "text": text }),
            move |result: Result<TurnSendResult, _>| {
                SessionOp::Sent(text, result.map(|sent| sent.turn))
            },
        );
    }

    fn steer(&mut self, text: String) {
        let Some(session) = self.session.as_mut() else {
            self.composer.restore_text(&text);
            return;
        };
        let Some(loop_id) = session.live.as_ref().map(|live| live.turn.loop_id.clone()) else {
            self.composer.restore_text(&text);
            return;
        };
        session.queue.push(Queued {
            text: text.clone(),
            kind: QueuedKind::Steer,
        });
        let fields = json!({ "loop_id": loop_id, "text": text });
        self.call(protocol::METHOD_TURN_STEER, fields, SessionOp::Steered);
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

    /// Esc: interrupt a running turn, else clear the draft.
    pub fn escape(&mut self) {
        if self.is_busy() {
            self.interrupt();
        } else {
            self.composer.clear();
        }
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

    fn compact(&mut self) {
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
        let mut config = crate::config::Config::load();
        config.theme = kind;
        if let Err(error) = config.save() {
            self.notice(NoticeLevel::Error, error);
        }
        self.theme = kind;
        self.styles = Styles::of(Palette::for_kind(kind));
    }

    /// The editor command: config first, then `$VISUAL`/`$EDITOR`.
    pub fn editor_command(&self) -> Result<Vec<String>, String> {
        crate::config::Config::load().editor().ok_or_else(|| {
            "No editor configured: set `editor` in the TUI config or $VISUAL/$EDITOR".to_owned()
        })
    }

    /// Takes the draft Ctrl+G asked the main loop to edit; the terminal
    /// belongs to the loop, which reports back through `Msg::DraftEdited`.
    pub fn take_edit_request(&mut self) -> Option<String> {
        self.edit_request.take()
    }

    fn answer(&mut self, answer: Answer) {
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
            (Answer::Allow | Answer::Choice(0), _) => decision("allow_once"),
            (Answer::Deny | Answer::Choice(_), _) => decision("deny"),
        };
        let fields = json!({
            "loop_id": interaction.turn.loop_id,
            "interaction_id": interaction.interaction_id,
            "answer": wire,
        });
        self.call(protocol::METHOD_INTERACTION_ANSWER, fields, SessionOp::Done);
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

    /// Reads a history window for the resident session.
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

    /// Asks for a fresh context report; the view never shows a stale one.
    pub fn refresh_context(&self) {
        self.call(
            protocol::METHOD_SESSION_CONTEXT,
            json!({}),
            SessionOp::Context,
        );
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

    /// Alt+↑: the newest queued follow-up goes back to an empty composer.
    fn recall_queued(&mut self) {
        if !self.composer.is_empty() {
            self.notice(
                NoticeLevel::Info,
                "Clear the draft before recalling a queued message",
            );
        } else if let Some(text) = self.session.as_mut().and_then(|s| s.recall_follow_up()) {
            self.composer.set_text(&text);
        }
    }

    /// Returns submitted text to the composer: appended to a draft typed
    /// meanwhile rather than replacing it, and one undo step either way.
    pub(super) fn restore_draft(&mut self, text: &str) {
        if self.composer.is_empty() {
            self.composer.restore_text(text);
        } else {
            self.composer.insert(text);
        }
    }

    /// Sends the next queued follow-up once the Agent is free.
    pub(super) fn flush_queue(&mut self) {
        if self.is_busy() {
            return;
        }
        if let Some(text) = self.session.as_mut().and_then(Session::take_follow_up) {
            self.send(text);
        }
    }

    /// Copies `text` through the clipboard.
    pub fn copy(&mut self, text: String) {
        self.spawn_msg(async move {
            match crate::clipboard::copy(&text).await {
                Ok(crate::clipboard::Copied::Native) => {
                    Msg::Notice(NoticeLevel::Info, "Copied".into())
                }
                Ok(crate::clipboard::Copied::Osc52) => {
                    Msg::Notice(NoticeLevel::Info, "Copied with OSC 52".into())
                }
                Err(error) => Msg::Notice(NoticeLevel::Error, error),
            }
        });
    }
}
