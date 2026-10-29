//! Remote mode commands, local authorization and phone intents.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::{
    Action, Answer, App, LaunchSettings, Msg, NoticeLevel, Overlay, SessionOp, commands, tasks,
};
use crate::protocol::{self, InputMode, Reasoning};
use crate::remote::{
    Bound, DraftSettings, NoticeTarget, Phone, PhoneView, RemoteEvent,
    auth::Auth,
    fs, query, view, whole_seconds,
    wire::{self, Choice, Intent, Query},
};

pub const REMOTE_FLASH: &str = "Remote control is on · /remote off to take back";

impl App {
    pub fn sync_remote(&mut self) {
        if self.remote.phones.is_empty() {
            return;
        }
        let shared = view::Shared::build(self);
        let mut phones = std::mem::take(&mut self.remote.phones);
        phones.retain_mut(|phone| {
            let shared = shared
                .iter()
                .find(|shared| shared.connections.contains(&phone.connection.id))
                .unwrap();
            let session = phone
                .view
                .session_id()
                .map(|id| self.live(id).expect("a phone views a live session"));
            let messages = phone.projection.sync(session, shared);
            messages
                .into_iter()
                .all(|message| phone.connection.send(message))
        });
        self.remote.phones = phones;
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
        if self
            .session
            .as_ref()
            .is_some_and(|s| !s.queue.items.is_empty())
        {
            self.recall_queue();
        }
        for (address, reason) in &bound.skipped {
            let text = format!("Remote: not listening on {address} ({reason})");
            self.notice(NoticeLevel::Info, text);
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
            ("off", "") => {
                if let Some(draft) = self.remote.draft.take() {
                    self.launch = draft.launch;
                }
                self.remote.stop("remote_off");
                if let Some(session) = &mut self.session {
                    let returned = std::mem::take(&mut session.returned);
                    for text in returned {
                        self.restore_draft(&text);
                    }
                }
            }
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
        self.remote.bye("signed_out");
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
                if connection.generation != self.remote.generation || !self.remote.active() {
                    let _ = accepted.send(false);
                    return;
                }
                if accepted.send(true).is_err() {
                    return;
                }
                // Another tab or a reconnect of the same device takes its place.
                let device = &connection.device.token_sha256;
                if let Some(old) = self
                    .remote
                    .phones
                    .iter()
                    .position(|phone| phone.connection.device.token_sha256 == *device)
                {
                    self.remote.phones.remove(old).connection.bye("replaced");
                }
                let view = PhoneView::foreground(self);
                self.remote.phones.push(Phone {
                    connection,
                    view,
                    projection: view::View::default(),
                    connected_at: jiff::Timestamp::now(),
                    queries: 0,
                });
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
                // A sign-in attempt concerns every connected phone.
                self.remote.notice_target = NoticeTarget::All;
                self.notice(NoticeLevel::Info, text);
                self.remote.notice_target = NoticeTarget::Local;
            }
            RemoteEvent::Query {
                connection,
                id,
                query,
            } => self.phone_query(connection, id, query),
            RemoteEvent::Answered {
                connection,
                id,
                result,
            } => {
                if let Some(phone) = self.phone_mut(connection) {
                    phone.queries -= 1;
                    self.remote
                        .send(Some(connection), wire::result(&id, result));
                }
            }
            RemoteEvent::Disconnected(id) => {
                self.remote.phones.retain(|phone| phone.connection.id != id);
            }
            RemoteEvent::Request {
                connection,
                id,
                intent,
                done,
            } => {
                if self
                    .remote
                    .phones
                    .iter()
                    .any(|phone| phone.connection.id == connection)
                {
                    self.remote.notice_target = NoticeTarget::Phone(connection);
                    let result = self.phone_intent(connection, intent);
                    self.remote.notice_target = NoticeTarget::Local;
                    self.remote.send(Some(connection), wire::ack(&id, result));
                }
                let _ = done.send(());
            }
        }
    }

    fn phone_mut(&mut self, connection: u64) -> Option<&mut Phone> {
        self.remote
            .phones
            .iter_mut()
            .find(|phone| phone.connection.id == connection)
    }

    /// Starts a query for the phone's own view without focusing it: reads move no foreground.
    fn phone_query(&mut self, connection: u64, id: String, query: Query) {
        let Some(phone) = self
            .remote
            .phones
            .iter()
            .find(|phone| phone.connection.id == connection)
        else {
            return;
        };
        let started = if phone.queries == query::IN_FLIGHT {
            Err("busy".to_owned())
        } else {
            let session = phone
                .view
                .session_id()
                .map(|id| self.live(id).expect("a phone views a live session"));
            query::start(self.client.clone(), session, query)
        };
        match started {
            Ok(work) => {
                self.phone_mut(connection).unwrap().queries += 1;
                self.spawn_msg(async move {
                    let result = work.await;
                    Msg::Remote(RemoteEvent::Answered {
                        connection,
                        id,
                        result,
                    })
                });
            }
            Err(error) => self
                .remote
                .send(Some(connection), wire::result(&id, Err(error))),
        }
    }

    fn phone_intent(&mut self, connection: u64, intent: Intent) -> Result<(), String> {
        let phone = self
            .remote
            .phones
            .iter()
            .find(|phone| phone.connection.id == connection)
            .unwrap();
        let blocked = self.loading_session
            && (self.first_message.is_some()
                || match &intent {
                    Intent::OpenSession { session_id } => self.live(session_id).is_none(),
                    Intent::NewSession { .. } => true,
                    Intent::Submit { .. } | Intent::Queue { .. } => {
                        phone.view.session_id().is_none()
                    }
                    _ => false,
                });
        if blocked {
            return Err(if self
                .remote
                .opening
                .as_ref()
                .is_some_and(|opening| *opening != connection)
            {
                "Another device is opening a session"
            } else {
                "A session is opening or being created"
            }
            .into());
        }
        let focus = !matches!(intent, Intent::OpenSession { .. });
        if focus {
            self.focus_phone(connection);
        }
        let result = self.remote_intent(connection, intent);
        if focus && self.session.is_none() {
            let draft = DraftSettings {
                workspace: self.workspace.clone(),
                launch: std::mem::take(&mut self.launch),
            };
            self.remote.draft = Some(draft.clone());
            self.set_phone_view(connection, PhoneView::Draft(draft));
        }
        result
    }

    fn focus_phone(&mut self, connection: u64) {
        let view = self
            .remote
            .phones
            .iter()
            .find(|phone| phone.connection.id == connection)
            .unwrap()
            .view
            .clone();
        self.remote.draft = None;
        self.launch = LaunchSettings::default();
        match view {
            PhoneView::Session(id) => {
                if let Some(index) = self.live_index(&id) {
                    self.switch_to(index);
                }
            }
            PhoneView::Draft(draft) => {
                if self.session.is_some() {
                    self.show_session(None);
                }
                self.workspace = draft.workspace;
                self.launch = draft.launch;
            }
        }
    }

    pub(super) fn set_phone_view(&mut self, connection: u64, view: PhoneView) {
        let Some(phone) = self
            .remote
            .phones
            .iter_mut()
            .find(|phone| phone.connection.id == connection)
        else {
            return;
        };
        if let Some(id) = view.session_id() {
            self.session
                .iter_mut()
                .chain(&mut self.background)
                .find(|session| session.info.session_id == id)
                .expect("a phone views a live session")
                .unseen = false;
        }
        phone.set_view(view);
    }

    pub(super) fn forget_phone_session(&mut self, id: &str) {
        let foreground = self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == id);
        if foreground {
            self.show_session(None);
        }
        let draft = DraftSettings {
            workspace: self.workspace.clone(),
            launch: LaunchSettings::default(),
        };
        for phone in &mut self.remote.phones {
            if phone.view.session_id() == Some(id) {
                phone.set_view(PhoneView::Draft(draft.clone()));
            }
        }
        if foreground {
            self.launch = LaunchSettings::default();
            self.remote.draft = self.remote.active().then_some(draft);
        }
    }

    fn remote_intent(&mut self, connection: u64, intent: Intent) -> Result<(), String> {
        match intent {
            Intent::Submit { text } => self.remote_text(connection, text, false)?,
            Intent::Queue { text } => self.remote_text(connection, text, true)?,
            Intent::Cancel => {
                if self.is_busy() || self.compacting() {
                    self.escape();
                }
            }
            Intent::Compact => {
                let session = self.session.as_ref().ok_or("No session")?;
                if session.is_busy() || session.compacting() {
                    return Err("busy".into());
                }
                self.compact();
            }
            Intent::Unqueue { index, text, edit } => {
                let queued = self
                    .session
                    .as_ref()
                    .and_then(|session| session.queue.items.get(index))
                    .filter(|queued| queued.text == text)
                    .ok_or("stale")?;
                let requester = edit.then_some(connection);
                self.call(
                    protocol::METHOD_INPUT_REMOVE,
                    json!({ "id": queued.id }),
                    move |result| SessionOp::Removed(requester, result),
                );
            }
            Intent::Answer {
                interaction_id,
                choice,
                message,
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
                if message
                    .as_ref()
                    .is_some_and(|message| answer != Answer::Deny || message.trim().is_empty())
                {
                    return Err("bad_request".into());
                }
                self.answer(answer);
                if let Some(message) = message {
                    self.push_input(message.trim_end().to_owned(), InputMode::Steer);
                }
            }
            Intent::LoadOlder => {
                if self.session.is_none() {
                    return Err("No session".into());
                }
                self.load_older();
            }
            Intent::OpenSession { session_id } => {
                self.remote_session(&session_id)?;
                if self.live(&session_id).is_some() {
                    self.set_phone_view(connection, PhoneView::Session(session_id));
                    self.focus_phone(connection);
                } else {
                    if self.session.is_none() {
                        self.remote.draft = Some(DraftSettings {
                            workspace: self.workspace.clone(),
                            launch: self.draft_launch().clone(),
                        });
                    }
                    self.launch = LaunchSettings::default();
                    self.remote.opening = Some(connection);
                    self.open_session(session_id);
                }
            }
            Intent::RenameSession { session_id, title } => {
                self.remote_session(&session_id)?;
                if title.trim().chars().count() > 200 || title.chars().any(char::is_control) {
                    return Err("bad_request".into());
                }
                self.rename_session(session_id, title);
            }
            Intent::DeleteSession { session_id } => {
                self.remote_session(&session_id)?;
                let own = self
                    .remote
                    .phones
                    .iter()
                    .find(|phone| phone.connection.id == connection)
                    .unwrap();
                if own.view.session_id() == Some(&session_id) {
                    return Err("Open another session before deleting this one".into());
                }
                if let Some(phone) = self
                    .remote
                    .phones
                    .iter()
                    .find(|phone| phone.view.session_id() == Some(&session_id))
                {
                    return Err(format!("Open on \"{}\"", phone.connection.device.name));
                }
                if let Some(refusal) = self.delete_refusal(&session_id) {
                    return Err(refusal);
                }
                // Not `Action::DeleteSession`: local gating denies it in remote mode.
                let client = self.client.clone();
                self.spawn_msg(tasks::delete_session(client, session_id));
            }
            Intent::NewSession { workspace, profile } => {
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
                self.launch = LaunchSettings::default();
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

    /// A catalog session the phone may act on: known, and under the roots.
    fn remote_session(&self, session_id: &str) -> Result<(), String> {
        let info = self
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .ok_or("Unknown session")?;
        if !fs::within(self.remote.roots(), std::path::Path::new(&info.workspace)) {
            return Err("outside_roots".into());
        }
        Ok(())
    }

    /// Phone text read as the composer reads Enter; the client runs built-in commands itself.
    fn remote_text(&mut self, connection: u64, text: String, follow: bool) -> Result<(), String> {
        let command = commands::parse(&text).and_then(|(name, arg)| commands::execute(name, arg));
        if command.is_some() {
            return Err("unknown_command".into());
        }
        let text = match self.prompt_text(&text).map_err(|_| "unknown_command")? {
            Some(rewritten) => rewritten,
            None => text,
        };
        if text.trim().is_empty() {
            return Err("Remote text is empty".into());
        }
        if !self.ready {
            return Err("A session is opening or being created".into());
        }
        if let Some(session) = &mut self.session {
            session.last_sender = Some(connection);
        } else {
            self.remote.opening = Some(connection);
        }
        self.submit_text(text.trim_end().to_owned(), follow);
        Ok(())
    }

    /// Shows session `id`: at once when it is live, else opened in the Agent.
    pub fn open_session(&mut self, id: String) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == id)
        {
            return;
        }
        if let Some(index) = self.live_index(&id) {
            // The session being created takes the screen when it arrives.
            if self.first_message.is_none() {
                self.switch_to(index);
            }
            return;
        }
        self.loading_session = true;
        let client = self.client.clone();
        self.spawn_msg(async move {
            let params = json!({ "session_id": id });
            Msg::Opened(
                id,
                tasks::session(client, protocol::METHOD_SESSION_OPEN, params).await,
            )
        });
    }
}
