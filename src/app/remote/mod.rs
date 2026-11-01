//! Remote mode lifecycle, connection routing and device views.

mod intent;
mod pending;

use std::path::PathBuf;
use std::time::Duration;

use crate::app::{Action, App, LaunchSettings, Msg, NoticeLevel, Overlay};
use crate::remote::{
    Bound, DraftSettings, NoticeTarget, Phone, PhoneView, RemoteEvent,
    auth::Auth,
    query, view, whole_seconds,
    wire::{self, Query},
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
            let messages = phone.projection.sync(phone.connection.id, session, shared);
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
            .resident
            .current()
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
                if let Some(session) = self.resident.current_mut() {
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
                self.remote.generation = self
                    .remote
                    .generation
                    .checked_add(1)
                    .expect("remote generation exhausted");
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
            #[cfg(feature = "e2e-observe")]
            RemoteEvent::PhoneGate(observation) => self.observe_phone_gate(&observation),
            #[cfg(feature = "e2e-observe")]
            RemoteEvent::PhoneGateFailed(error) => panic!("acceptance phone gate failed: {error}"),
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
                let mut projection = view::View::default();
                projection.invalidate(connection.id);
                self.remote.phones.push(Phone {
                    connection,
                    view,
                    projection,
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
                if let Some(phone) = self.remote.phone_mut(connection) {
                    phone.queries -= 1;
                    self.remote
                        .send(Some(connection), wire::result(&id, result));
                }
            }
            RemoteEvent::Pushed {
                connection,
                id,
                result,
            } => self.phone_pushed(connection, id, *result),
            RemoteEvent::Removed {
                connection,
                id,
                result,
            } => self.phone_removed(connection, id, *result),
            RemoteEvent::Created {
                connection,
                id,
                result,
            } => self.phone_created(connection, id, *result),
            RemoteEvent::Finished {
                connection,
                id,
                result,
            } => {
                self.phone_finished(connection, id, result);
            }
            RemoteEvent::Disconnected(id) => {
                self.remote.disconnected(id);
            }
            RemoteEvent::Request {
                connection,
                id,
                intent,
                done,
            } => {
                if self.remote.phone(connection).is_some() {
                    let result = self.phone_intent(connection, &id, intent);
                    // An operation waiting on the Agent is answered from its result, not here.
                    if let Some(reply) = result {
                        self.remote.send(Some(connection), wire::ack(&id, reply));
                    }
                }
                let _ = done.send(());
            }
        }
    }

    /// Starts a query for the phone's own view without focusing it: reads move no foreground.
    fn phone_query(&mut self, connection: u64, id: String, query: Query) {
        let Some(phone) = self.remote.phone(connection) else {
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
                self.remote.phone_mut(connection).unwrap().queries += 1;
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

    fn admitted(&self, connection: u64) -> &crate::remote::Phone {
        self.remote.phone(connection).expect("an admitted phone")
    }

    fn phone_view_session(&self, connection: u64) -> Option<&str> {
        self.admitted(connection).view.session_id()
    }

    fn focus_phone(&mut self, connection: u64) {
        self.remote.focus_generation = self
            .remote
            .focus_generation
            .checked_add(1)
            .expect("phone focus generation exhausted");
        let view = self.admitted(connection).view.clone();
        self.remote.draft = None;
        self.launch = LaunchSettings::default();
        match view {
            PhoneView::Session(id) => {
                if let Some(epoch) = self.live(&id).map(|session| session.epoch) {
                    self.switch_to(epoch);
                }
            }
            PhoneView::Draft(draft) => {
                if self.resident.current().is_some() {
                    self.show_session(None);
                }
                self.workspace = draft.workspace;
                self.launch = draft.launch;
            }
        }
    }

    pub(super) fn set_phone_view(&mut self, connection: u64, view: PhoneView) {
        let Some(phone) = self.remote.phone_mut(connection) else {
            return;
        };
        if let Some(id) = view.session_id() {
            self.resident
                .iter_mut()
                .find(|session| session.info.session_id == id)
                .expect("a phone views a live session")
                .unseen = false;
        }
        phone.set_view(view);
    }

    pub(super) fn forget_phone_session(&mut self, id: &str) {
        let foreground = self
            .resident
            .current()
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
}
