//! Remote mode commands, local authorization and phone intents.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::{Action, Answer, App, Msg, NoticeLevel, Overlay, Session, tasks};
use crate::protocol::{self, Reasoning};
use crate::remote::{
    Bound, RemoteEvent,
    auth::Auth,
    fs, whole_seconds,
    wire::{self, Choice, Intent},
};

pub const REMOTE_FLASH: &str = "Remote control is on · /remote off to take back";

impl App {
    pub fn sync_remote(&mut self) {
        if self.remote.connection.is_none() {
            return;
        }
        let mut view = std::mem::take(&mut self.remote.view);
        for message in view.sync(self) {
            self.remote.send(message);
        }
        self.remote.view = view;
    }

    pub fn local_allowed(&self, action: &Action) -> bool {
        !self.remote.active()
            || matches!(
                action,
                Action::SetTheme(_)
                    | Action::ToggleTheme
                    | Action::CopyLastAnswer
                    | Action::Export(_)
                    | Action::EditDraft
                    | Action::LoadOlder
                    | Action::RecallQueued
                    | Action::Quit
                    | Action::Remote(_)
            )
    }

    pub fn remote_flash(&mut self) {
        self.flash = Some((REMOTE_FLASH.into(), tokio::time::Instant::now()));
    }

    pub fn enter_remote(&mut self, bound: Bound) {
        if let Some(text) = self.session.as_mut().and_then(Session::recall_follow_ups) {
            self.restore_draft(&text);
        }
        self.remote.start(bound, self.tx.clone());
        self.overlay = Overlay::None;
    }

    pub(super) fn remote_command(&mut self, command: &str) {
        let (name, argument) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(name, argument)| (name, argument.trim()));
        match (name, argument) {
            // The status screen already shows everything; only what covers it closes.
            ("" | "status" | "on", "") if self.remote.active() => self.overlay = Overlay::None,
            ("" | "status", "") => self.notice(
                NoticeLevel::Info,
                "Remote control is off · /remote on starts it",
            ),
            ("off", "") => self.remote.stop("remote_off"),
            ("on", "") if self.remote.starting.is_some() => {}
            ("on", "") => {
                let config = self
                    .remote
                    .config
                    .clone()
                    .expect("remote settings initialized at startup");
                let file = self.remote_file();
                let tx = self.tx.clone();
                self.remote.generation += 1;
                let generation = self.remote.generation;
                self.remote.starting = Some(tokio::spawn(async move {
                    let result = Bound::bind(&config, file).await;
                    let _ = tx.send(Msg::Remote(RemoteEvent::Bound(generation, result)));
                }));
            }
            ("password", password) => {
                self.change_password((!password.is_empty()).then_some(password))
            }
            _ => self.notice(
                NoticeLevel::Error,
                "Use /remote [on|off|status|password [text]]",
            ),
        }
    }

    fn remote_file(&self) -> PathBuf {
        self.home
            .as_ref()
            .expect("TUI directory initialized at startup")
            .dir()
            .join("remote.json")
    }

    /// Sets the password, a generated one for `None`, which signs every phone out.
    fn change_password(&mut self, password: Option<&str>) {
        let result = match &self.remote.auth {
            Some(auth) => auth.lock().unwrap().set_password(password),
            None => Auth::load(self.remote_file()).and_then(|mut auth| auth.set_password(password)),
        };
        if let Err(error) = result {
            self.notice(NoticeLevel::Error, format!("Remote: {error}"));
            return;
        }
        if let Some(phone) = self.remote.connection.take() {
            phone.bye("signed_out");
        }
        self.remote.failed_logins = 0;
        self.remote.paused_until = None;
        self.notice(
            NoticeLevel::Info,
            "New remote password · phones must sign in again",
        );
    }

    pub(super) fn request_quit(&mut self) {
        if self.remote.active() {
            let now = tokio::time::Instant::now();
            if !self
                .remote
                .quit_armed
                .is_some_and(|at| now - at <= Duration::from_secs(3))
            {
                self.remote.quit_armed = Some(now);
                self.flash = Some(("Remote control is on · quit again to end it".into(), now));
                return;
            }
        }
        self.quit = true;
    }

    pub(super) fn on_remote(&mut self, event: RemoteEvent) {
        match event {
            RemoteEvent::Bound(generation, result) => {
                if generation != self.remote.generation {
                    return;
                }
                self.remote.starting = None;
                match result {
                    Ok(bound) => self.enter_remote(bound),
                    Err(error) => self.notice(NoticeLevel::Error, format!("Remote: {error}")),
                }
            }
            RemoteEvent::Connect {
                connection,
                accepted,
            } => {
                if connection.generation != self.remote.generation
                    || !self.remote.active()
                    || self.remote.connection.as_ref().is_some_and(|old| {
                        old.device.token_sha256 != connection.device.token_sha256
                    })
                {
                    let _ = accepted.send(false);
                    return;
                }
                if accepted.send(true).is_err() {
                    return;
                }
                self.remote.connected_at = protocol::stamp::now_millis();
                if let Some(old) = self.remote.connection.replace(connection) {
                    old.bye("replaced");
                }
            }
            RemoteEvent::LoginRefused { locked } => {
                self.remote.failed_logins += 1;
                let text = match locked {
                    Some(pause) => {
                        self.remote.paused_until = Some(tokio::time::Instant::now() + pause);
                        format!("Remote: sign-in paused for {} s", whole_seconds(pause))
                    }
                    None => "Remote: wrong password".to_owned(),
                };
                self.notice(NoticeLevel::Info, text);
            }
            RemoteEvent::Disconnected(id) => {
                if self
                    .remote
                    .connection
                    .as_ref()
                    .is_some_and(|phone| phone.id == id)
                {
                    self.remote.connection = None;
                }
            }
            RemoteEvent::Request {
                connection,
                id,
                intent,
                done,
            } => {
                if self
                    .remote
                    .connection
                    .as_ref()
                    .is_some_and(|phone| phone.id == connection)
                {
                    let result = self.remote_intent(intent);
                    self.remote.send(wire::ack(&id, result));
                }
                let _ = done.send(());
            }
        }
    }

    fn remote_intent(&mut self, intent: Intent) -> Result<(), String> {
        match intent {
            Intent::Submit { text } => self.remote_text(text, false)?,
            Intent::Queue { text } => self.remote_text(text, true)?,
            Intent::Cancel => {
                if self.is_busy() || self.compacting() {
                    self.escape();
                }
            }
            Intent::Answer {
                interaction_id,
                choice,
            } => {
                let interaction = self
                    .session
                    .as_ref()
                    .and_then(|s| s.interaction.as_ref())
                    .filter(|i| i.interaction_id == interaction_id)
                    .ok_or("stale")?;
                let answer = match (choice, &interaction.kind) {
                    (Choice::Name(name), super::InteractionKind::Approval) => match name.as_str() {
                        "allow" => Answer::Allow,
                        "allow_session" => Answer::AllowTool,
                        "deny" => Answer::Deny,
                        _ => return Err("bad_request".into()),
                    },
                    (Choice::Index(index), super::InteractionKind::Choice(options))
                        if index < options.len() =>
                    {
                        Answer::Choice(index)
                    }
                    _ => return Err("bad_request".into()),
                };
                self.answer(answer);
            }
            Intent::LoadOlder => {
                if self.session.is_none() {
                    return Err("No session".into());
                }
                self.load_older();
            }
            Intent::OpenSession { session_id } => {
                if self.loading_session {
                    return Err("A session is opening or being created".into());
                }
                let info = self
                    .sessions
                    .iter()
                    .find(|s| s.session_id == session_id)
                    .ok_or("Unknown session")?;
                if !fs::within(self.remote.roots(), std::path::Path::new(&info.workspace)) {
                    return Err("outside_roots".into());
                }
                self.open_session(session_id);
            }
            Intent::NewSession { workspace, profile } => {
                if self.loading_session {
                    return Err("A session is opening or being created".into());
                }
                if profile
                    .as_ref()
                    .is_some_and(|id| !self.profiles.iter().any(|p| &p.id == id))
                {
                    return Err("Unknown profile".into());
                }
                let path = workspace
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.workspace.clone());
                if !path.is_absolute() {
                    return Err("Workspace must be absolute".into());
                }
                let path = fs::canonical_within(self.remote.roots(), &path)
                    .map_err(|error| error.code().to_owned())?;
                if !path.is_dir() {
                    return Err("not_a_directory".into());
                }
                self.new_draft(profile);
                self.workspace = path;
            }
            Intent::SetModel { model } => {
                if !self.models.iter().any(|m| m.id == model) {
                    return Err("Unknown model".into());
                }
                self.set_model(model);
            }
            Intent::SetReasoning { level } => {
                let level = Reasoning::parse(&level).ok_or("Unknown reasoning level")?;
                self.set_reasoning(level);
            }
        }
        Ok(())
    }

    fn remote_text(&mut self, text: String, follow: bool) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("Remote text is empty".into());
        }
        if !self.ready || self.loading_session {
            return Err("A session is opening or being created".into());
        }
        self.submit_text(text.trim_end().to_owned(), follow);
        Ok(())
    }

    pub fn open_session(&mut self, id: String) {
        self.loading_session = true;
        let client = self.client.clone();
        self.spawn_msg(async move {
            let params = json!({ "session_id": id });
            Msg::Opened(tasks::session(client, protocol::METHOD_SESSION_OPEN, params).await)
        });
    }
}
