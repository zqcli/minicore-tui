//! Remote mode commands, local authorization and phone intents.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use super::operation::{Operation, Outcome};
use super::{Action, App, LaunchSettings, Msg, NoticeLevel, Overlay, commands, tasks};
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
                if let Some(phone) = self.phone_mut(connection) {
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
                if self
                    .remote
                    .phones
                    .iter()
                    .any(|phone| phone.connection.id == connection)
                {
                    self.remote.notice_target = NoticeTarget::Phone(connection);
                    let result = self.phone_intent(connection, &id, intent);
                    self.remote.notice_target = NoticeTarget::Local;
                    // An operation waiting on the Agent is answered from its result, not here.
                    if let Some(reply) = result {
                        self.remote.send(Some(connection), wire::ack(&id, reply));
                    }
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

    /// Validates an intent against the projection it was built from and runs it.
    ///
    /// `None` means the answer waits on the Agent and is sent from that
    /// result; `Some` is answered at once, before any side effect that could
    /// still fail.
    fn phone_intent(
        &mut self,
        connection: u64,
        id: &str,
        intent: Intent,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        self.remote.notice_target = NoticeTarget::Phone(connection);
        let outcome = self.route_intent(connection, id, intent);
        self.remote.notice_target = NoticeTarget::Local;
        outcome
    }

    fn route_intent(
        &mut self,
        connection: u64,
        id: &str,
        mut intent: Intent,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        if self.remote.holds_pending(connection, id) {
            self.admitted(connection).connection.bye("duplicate_id");
            self.remote.disconnected(connection);
            return None;
        }
        if let Some(refusal) = self.stale_target(connection, &intent) {
            return Some(Err(self.refusal(&intent, refusal)));
        }
        // A target the phone still holds can still be refused by what the TUI is
        // doing; that is the opening device's answer, not a stale view.
        let blocked = self.intent_blocked(connection, &intent);
        if let Some(refusal) = blocked {
            return Some(Err(self.refusal(&intent, refusal)));
        }
        let holds = match &intent {
            Intent::LoadOlder { .. } | Intent::NewSession { .. } => false,
            Intent::OpenSession { session_id, .. } => self.live(session_id).is_none(),
            Intent::SetModel { .. } | Intent::SetReasoning { .. } => {
                self.phone_view_session(connection).is_some()
            }
            _ => true,
        };
        if let Intent::Submit { text, .. } | Intent::Queue { text, .. } = &mut intent {
            match self.phone_text(text) {
                Ok(expanded) => *text = expanded,
                Err(refusal) => return Some(Err(refusal)),
            }
        }
        let text = match &intent {
            Intent::Submit { text, .. } | Intent::Queue { text, .. } => Some(text.as_str()),
            Intent::Answer {
                choice: Choice::Text { text },
                ..
            } => Some(text.as_str()),
            Intent::Answer { message, .. } => message.as_deref(),
            _ => None,
        };
        if holds && !self.remote.admits_pending(connection, text) {
            return Some(Err(self.refusal(&intent, "busy")));
        }
        let view_epoch = intent.target().view_epoch.clone();
        let session_id = intent.target().session_id.clone();
        let focus = !matches!(intent, Intent::OpenSession { .. });
        if focus {
            self.focus_phone(connection);
        }
        let result = self.remote_intent(connection, id, intent, &view_epoch, session_id);
        if focus && self.resident.current().is_none() {
            let draft = DraftSettings {
                workspace: self.workspace.clone(),
                launch: std::mem::take(&mut self.launch),
            };
            self.remote.draft = Some(draft.clone());
            self.set_phone_view(connection, PhoneView::Draft(draft));
        }
        result
    }

    /// Whether an open or create elsewhere forbids this intent outright.
    fn intent_blocked(&self, connection: u64, intent: &Intent) -> Option<&'static str> {
        let phone = self
            .remote
            .phones
            .iter()
            .find(|phone| phone.connection.id == connection)
            .expect("an admitted phone");
        let blocked = self.loading_session
            && match intent {
                Intent::OpenSession { session_id, .. } => self.live(session_id).is_none(),
                Intent::Submit { .. } | Intent::Queue { .. } => phone.view.session_id().is_none(),
                _ => false,
            };
        blocked.then(|| {
            if self
                .remote
                .opening
                .as_ref()
                .is_some_and(|opening| *opening != connection)
            {
                "Another device is opening a session"
            } else {
                "A session is opening or being created"
            }
        })
    }

    /// Whether the intent addresses a projection the phone no longer holds, or
    /// another resident instance than the one it was built from.
    ///
    /// Every intent must come from the view the phone currently has. An intent
    /// that acts on the viewed session must name that session and its resident
    /// instance; an intent that addresses another session, or that only changes
    /// this phone's view, names whatever it acts on.
    fn stale_target(&self, connection: u64, intent: &Intent) -> Option<&'static str> {
        let phone = self.admitted(connection);
        let target = intent.target();
        if phone.projection.view_epoch() != target.view_epoch {
            return Some("stale");
        }
        if !intent.acts_on_view() {
            return None;
        }
        let bound = self.phone_view_session(connection);
        // A draft has no resident instance, so it names no session and no load.
        let Some(bound) = bound else {
            if matches!(intent, Intent::Answer { .. }) {
                return Some("stale");
            }
            return (target.session_id.is_some() || target.load.is_some()).then_some("stale");
        };
        if target.session_id.as_deref() != Some(bound) {
            return Some("stale");
        }
        let Some(load) = target.load.as_deref() else {
            return Some("missing_load");
        };
        let session = self
            .live(bound)
            .expect("the phone views a resident session");
        if session.load != load {
            return Some("stale_load");
        }
        if let Intent::Answer {
            loop_id,
            interaction_id,
            ..
        } = intent
            && !session
                .interaction
                .as_ref()
                .is_some_and(|i| i.interaction_id == *interaction_id && i.turn.loop_id == *loop_id)
        {
            return Some("stale");
        }
        None
    }

    fn admitted(&self, connection: u64) -> &crate::remote::Phone {
        self.remote
            .phones
            .iter()
            .find(|phone| phone.connection.id == connection)
            .expect("an admitted phone")
    }

    fn phone_view_session(&self, connection: u64) -> Option<&str> {
        self.remote
            .phones
            .iter()
            .find(|phone| phone.connection.id == connection)
            .expect("an admitted phone")
            .view
            .session_id()
    }

    /// An input operation that the Agent never accepted may be shown as unsent.
    fn refusal(&self, intent: &Intent, error: &str) -> wire::Refusal {
        match intent {
            Intent::Submit { .. } | Intent::Queue { .. } | Intent::Unqueue { .. } => {
                wire::Refusal::rejected(error)
            }
            _ => wire::Refusal::new(error),
        }
    }

    fn focus_phone(&mut self, connection: u64) {
        self.remote.focus_generation = self
            .remote
            .focus_generation
            .checked_add(1)
            .expect("phone focus generation exhausted");
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
        let Some(phone) = self
            .remote
            .phones
            .iter_mut()
            .find(|phone| phone.connection.id == connection)
        else {
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

    /// Dispatches a checked intent, returning a local reply or waiting for its Agent result.
    fn remote_intent(
        &mut self,
        connection: u64,
        id: &str,
        intent: Intent,
        view_epoch: &str,
        session_id: Option<String>,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        let epoch = session_id
            .as_deref()
            .and_then(|id| self.live(id))
            .map(|session| session.epoch);
        match intent {
            Intent::Submit { text, .. } => {
                self.remote_text(connection, id, text, false, view_epoch, session_id)
            }
            Intent::Queue { text, .. } => {
                self.remote_text(connection, id, text, true, view_epoch, session_id)
            }
            Intent::Cancel {
                loop_id,
                operation_id,
                ..
            } => {
                // Only the turn the phone was shown may be cancelled. Without a
                // turn on screen the phone cancels the compaction it showed; with
                // neither, there is nothing to stop.
                let live = epoch
                    .and_then(|epoch| self.resident.get(epoch))
                    .and_then(|s| s.live.as_ref())
                    .map(|live| live.turn.loop_id.clone());
                let operation = match (loop_id, live) {
                    (Some(named), Some(current)) if named == current && operation_id.is_none() => {
                        Operation::CancelTurn(named)
                    }
                    (Some(_), Some(_)) => return Some(Err(wire::Refusal::new("stale"))),
                    (Some(_), None) => return Some(Err(wire::Refusal::new("stale"))),
                    (None, Some(_)) => return Some(Err(wire::Refusal::new("stale"))),
                    (None, None) => {
                        let current = epoch
                            .and_then(|epoch| self.resident.get(epoch))
                            .and_then(|s| s.compaction.as_ref());
                        match (operation_id, current) {
                            (Some(named), Some(current)) if named == current.operation_id => {
                                Operation::CancelCompaction(named)
                            }
                            (None, None) => return Some(Ok(None)),
                            _ => return Some(Err(wire::Refusal::new("stale"))),
                        }
                    }
                };
                self.hold_phone_command(connection, id, view_epoch, session_id, None);
                self.call_phone_command(connection, id.to_owned(), operation, None);
                None
            }
            Intent::Compact { .. } => {
                let Some(session) = epoch.and_then(|epoch| self.resident.get(epoch)) else {
                    return Some(Err(wire::Refusal::new("No session")));
                };
                if session.is_busy() || session.compacting() {
                    return Some(Err(wire::Refusal::new("busy")));
                }
                self.op_seq = self
                    .op_seq
                    .checked_add(1)
                    .expect("operation identity exhausted");
                let operation = Operation::Compact(format!("tui-{}", self.op_seq));
                self.hold_phone_command(connection, id, view_epoch, session_id, None);
                self.call_phone_command(connection, id.to_owned(), operation, None);
                None
            }
            Intent::Unqueue { input_id, edit, .. } => {
                let queued = epoch
                    .and_then(|epoch| self.resident.get(epoch))
                    .and_then(|session| {
                        session.queue.items.iter().find(|item| item.id == input_id)
                    });
                if queued.is_none() {
                    return Some(Err(wire::Refusal::rejected("input_not_queued")));
                }
                self.hold_pending(
                    connection,
                    crate::remote::PendingIntent {
                        id: id.to_owned(),
                        view_epoch: view_epoch.to_owned(),
                        session_id,
                        load: epoch
                            .and_then(|epoch| self.resident.get(epoch))
                            .map(|s| s.load.clone()),
                        epoch: epoch
                            .and_then(|epoch| self.resident.get(epoch))
                            .map(|s| s.epoch),
                        focus_generation: self.remote.focus_generation,
                        kind: crate::remote::PendingKind::Unqueue {
                            input_id: input_id.clone(),
                            edit,
                        },
                    },
                );
                self.remove_phone_input(connection, id.to_owned(), input_id);
                None
            }
            Intent::Answer {
                choice, message, ..
            } => {
                let interaction = epoch
                    .and_then(|epoch| self.resident.get(epoch))
                    .and_then(|s| s.interaction.as_ref())
                    .expect("the answer target was validated before admission");
                let answer = match (choice, &interaction.kind) {
                    (Choice::Text { text }, super::InteractionKind::Text) => {
                        super::Answer::Text(text)
                    }
                    (Choice::Name(name), super::InteractionKind::Approval) => match name.as_str() {
                        "allow" => super::Answer::Allow,
                        "allow_session" => super::Answer::AllowTool,
                        "deny" => super::Answer::Deny,
                        _ => return Some(Err(wire::Refusal::new("bad_request"))),
                    },
                    (Choice::Index(index), super::InteractionKind::Choice(options))
                        if index < options.len() =>
                    {
                        super::Answer::Choice(index)
                    }
                    _ => return Some(Err(wire::Refusal::new("bad_request"))),
                };
                if message.as_ref().is_some_and(|message| {
                    answer != super::Answer::Deny || message.trim().is_empty()
                }) {
                    return Some(Err(wire::Refusal::new("bad_request")));
                }
                let retained = match &answer {
                    super::Answer::Text(text) => Some(text.clone()),
                    _ => message.clone(),
                };
                let operation = match super::operation::answer(interaction, answer) {
                    Ok(Some(operation)) => operation,
                    _ => return Some(Err(wire::Refusal::new("bad_request"))),
                };
                self.hold_phone_command(connection, id, view_epoch, session_id, retained);
                self.call_phone_command(connection, id.to_owned(), operation, message);
                None
            }
            Intent::LoadOlder { .. } => {
                if epoch.and_then(|epoch| self.resident.get(epoch)).is_none() {
                    return Some(Err(wire::Refusal::new("No session")));
                }
                self.load_older_for(epoch.unwrap());
                Some(Ok(None))
            }
            Intent::OpenSession { session_id, .. } => {
                self.open_remote_session(connection, id, view_epoch, session_id)
            }
            Intent::RenameSession {
                session_id, title, ..
            } => {
                if let Err(refusal) = self.remote_session(&session_id) {
                    return Some(Err(wire::Refusal::new(refusal)));
                }
                if title.trim().chars().count() > 200 || title.chars().any(char::is_control) {
                    return Some(Err(wire::Refusal::new("bad_request")));
                }
                self.hold_phone_command(connection, id, view_epoch, Some(session_id), None);
                self.call_phone_command(connection, id.to_owned(), Operation::Rename(title), None);
                None
            }
            Intent::DeleteSession { session_id, .. } => {
                match self.delete_remote_session(connection, id, view_epoch, session_id) {
                    Ok(()) => None,
                    Err(error) => Some(Err(error)),
                }
            }
            Intent::NewSession {
                workspace, profile, ..
            } => Some(self.draft_in(workspace, profile)),
            Intent::SetModel { model, .. } => {
                if !self.models.iter().any(|m| m.id == model) {
                    return Some(Err(wire::Refusal::new("Unknown model")));
                }
                if epoch.and_then(|epoch| self.resident.get(epoch)).is_none() {
                    self.set_model(model);
                    Some(Ok(None))
                } else {
                    let mut fields = json!({"model":model});
                    if let Some(level) = self.reasoning_for_level(
                        &model,
                        self.resident
                            .get(epoch.unwrap())
                            .map(|session| session.info.reasoning),
                    ) {
                        fields["reasoning"] = json!(level);
                    }
                    self.hold_phone_command(connection, id, view_epoch, session_id, None);
                    self.call_phone_command(
                        connection,
                        id.to_owned(),
                        Operation::Update(fields),
                        None,
                    );
                    None
                }
            }
            Intent::SetReasoning { level, .. } => {
                let Some(level) = Reasoning::parse(&level) else {
                    return Some(Err(wire::Refusal::new("Unknown reasoning level")));
                };
                if epoch.and_then(|epoch| self.resident.get(epoch)).is_none() {
                    self.set_reasoning(level);
                    Some(Ok(None))
                } else {
                    self.hold_phone_command(connection, id, view_epoch, session_id, None);
                    self.call_phone_command(
                        connection,
                        id.to_owned(),
                        Operation::Update(json!({"reasoning":level})),
                        None,
                    );
                    None
                }
            }
        }
    }

    fn open_remote_session(
        &mut self,
        connection: u64,
        operation: &str,
        view_epoch: &str,
        session_id: String,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        if let Err(error) = self.remote_session(&session_id) {
            return Some(Err(wire::Refusal::new(error)));
        }
        if self.live(&session_id).is_some() {
            self.set_phone_view(connection, PhoneView::Session(session_id));
            self.focus_phone(connection);
            return Some(Ok(None));
        } else {
            if self.resident.current().is_none() {
                self.remote.draft = Some(DraftSettings {
                    workspace: self.workspace.clone(),
                    launch: self.draft_launch().clone(),
                });
            }
            self.remote.opening = Some(connection);
            self.loading_session = true;
            self.remote.focus_generation = self
                .remote
                .focus_generation
                .checked_add(1)
                .expect("phone focus generation exhausted");
            self.remote.hold_pending(
                connection,
                crate::remote::PendingIntent {
                    id: operation.to_owned(),
                    view_epoch: view_epoch.to_owned(),
                    session_id: Some(session_id.clone()),
                    load: None,
                    epoch: None,
                    focus_generation: self.remote.focus_generation,
                    kind: crate::remote::PendingKind::Open,
                },
            );
            let client = self.client.clone();
            let id = operation.to_owned();
            self.spawn_msg(async move {
                let result = tasks::session(
                    client,
                    protocol::METHOD_SESSION_OPEN,
                    json!({"session_id":session_id}),
                )
                .await;
                Msg::Remote(RemoteEvent::Created {
                    connection,
                    id,
                    result: Box::new(result),
                })
            });
        }
        None
    }

    fn delete_remote_session(
        &mut self,
        connection: u64,
        operation: &str,
        view_epoch: &str,
        session_id: String,
    ) -> Result<(), wire::Refusal> {
        self.remote_session(&session_id)
            .map_err(wire::Refusal::new)?;
        if self.phone_view_session(connection) == Some(session_id.as_str()) {
            return Err(wire::Refusal::new(
                "Open another session before deleting this one",
            ));
        }
        if let Some(phone) = self
            .remote
            .phones
            .iter()
            .find(|phone| phone.view.session_id() == Some(&session_id))
        {
            return Err(wire::Refusal::new(format!(
                "Open on \"{}\"",
                phone.connection.device.name
            )));
        }
        if let Some(refusal) = self.delete_refusal(&session_id) {
            return Err(wire::Refusal::new(refusal));
        }
        self.hold_phone_command(connection, operation, view_epoch, Some(session_id), None);
        self.call_phone_command(connection, operation.to_owned(), Operation::Delete, None);
        Ok(())
    }

    /// Points this device's view at a draft; the next submit creates it.
    fn draft_in(
        &mut self,
        workspace: Option<String>,
        profile: Option<String>,
    ) -> Result<Option<serde_json::Value>, wire::Refusal> {
        if profile
            .as_ref()
            .is_some_and(|id| !self.profiles.iter().any(|p| &p.id == id))
        {
            return Err(wire::Refusal::new("Unknown profile"));
        }
        let path = workspace
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace.clone());
        if !path.is_absolute() {
            return Err(wire::Refusal::new("Workspace must be absolute"));
        }
        let path = fs::canonical_within(self.remote.roots(), &path)
            .map_err(|error| wire::Refusal::new(error.code().to_owned()))?;
        if !path.is_dir() {
            return Err(wire::Refusal::new("not_a_directory"));
        }
        self.launch = LaunchSettings::default();
        self.new_draft(profile);
        self.workspace = path;
        Ok(None)
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

    /// Hands phone text to the Agent; its answer is the acknowledgement.
    ///
    /// The phone's own operation makes the call, so the acknowledgement carries
    /// that operation's identity and the acceptance facts the Agent reported,
    /// and no other connection can be answered for it.
    fn remote_text(
        &mut self,
        connection: u64,
        id: &str,
        text: String,
        follow: bool,
        view_epoch: &str,
        session_id: Option<String>,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        if !self.ready {
            return Some(Err(wire::Refusal::rejected(
                "A session is opening or being created",
            )));
        }
        let target = session_id
            .as_deref()
            .and_then(|id| self.live(id))
            .map(|session| (session.epoch, session.load.clone()));
        let mode = match follow {
            true => InputMode::FollowUp,
            false => InputMode::Steer,
        };
        if target.is_none() {
            // A draft creates its session first. The push of its first text is
            // made by this operation, so it answers that operation either way.
            self.hold_pending(
                connection,
                crate::remote::PendingIntent {
                    id: id.to_owned(),
                    view_epoch: view_epoch.to_owned(),
                    session_id,
                    load: None,
                    epoch: None,
                    focus_generation: self.remote.focus_generation,
                    kind: crate::remote::PendingKind::DraftInput {
                        text: text.clone(),
                        mode,
                    },
                },
            );
            self.remote.opening = Some(connection);
            self.create_phone_session(connection, id.to_owned());
            return None;
        }
        self.hold_pending(
            connection,
            crate::remote::PendingIntent {
                id: id.to_owned(),
                view_epoch: view_epoch.to_owned(),
                session_id,
                load: target.as_ref().map(|(_, load)| load.clone()),
                epoch: target.map(|(epoch, _)| epoch),
                focus_generation: self.remote.focus_generation,
                kind: crate::remote::PendingKind::Input { text: text.clone() },
            },
        );
        self.push_phone_input(connection, id.to_owned(), text, mode);
        None
    }

    /// Creates the session a phone's draft needs, then pushes its first text.
    ///
    /// A create that fails is answered to the operation that asked for it: its
    /// text was never sent, so only that phone is offered it again.
    fn create_phone_session(&mut self, connection: u64, id: String) {
        self.loading_session = true;
        let mut params = self.launch.fields(true);
        params["workspace"] = json!(self.workspace);
        let client = self.client.clone();
        self.spawn_msg(async move {
            let result =
                crate::app::tasks::session(client, crate::protocol::METHOD_SESSION_CREATE, params)
                    .await;
            Msg::Remote(RemoteEvent::Created {
                connection,
                id,
                result: Box::new(result),
            })
        });
    }

    /// Adopts a phone's open/create outcome without replacing a newer foreground.
    fn phone_created(
        &mut self,
        connection: u64,
        id: String,
        result: Result<crate::app::tasks::Adopted, crate::protocol::RpcError>,
    ) {
        let Some(mut pending) = self.remote.release_pending(connection, &id) else {
            return;
        };
        self.loading_session = false;
        self.remote.opening = None;
        let created = match result {
            Ok(created) => created,
            Err(error) => {
                let refusal = match pending.kind {
                    crate::remote::PendingKind::Open => wire::Refusal::new(error.message),
                    _ => wire::Refusal::rejected(error.message),
                };
                self.phone_answer(connection, &id, Err(refusal));
                return;
            }
        };
        if matches!(pending.kind, crate::remote::PendingKind::Open)
            && pending.session_id.as_deref() != Some(created.info.session_id.as_str())
        {
            self.phone_answer(
                connection,
                &id,
                Err(wire::Refusal::new("Opened session identity changed")),
            );
            return;
        }
        if matches!(pending.kind, crate::remote::PendingKind::Open)
            && !self
                .sessions
                .iter()
                .any(|s| s.session_id == created.info.session_id)
        {
            self.phone_answer(
                connection,
                &id,
                Err(wire::Refusal::new("Session deleted while opening")),
            );
            return;
        }
        let unchanged = self.remote.phones.iter().any(|phone| {
            phone.connection.id == connection && phone.projection.view_epoch() == pending.view_epoch
        });
        let keep_focus = unchanged && self.remote.focus_generation == pending.focus_generation;
        let typed = (keep_focus
            && self.resident.current().is_none()
            && matches!(pending.kind, crate::remote::PendingKind::DraftInput { .. }))
        .then(|| self.composer.text().to_owned());
        pending.session_id = Some(created.info.session_id.clone());
        pending.load = Some(created.load.clone());
        let epoch = match self.install_session(created, keep_focus, true) {
            Ok(epoch) => epoch,
            Err(error) => {
                self.phone_answer(connection, &id, Err(wire::Refusal::new(error.message)));
                return;
            }
        };
        if keep_focus {
            self.launch = super::LaunchSettings::default();
            self.remote.draft = None;
        }
        if let Some(typed) = typed {
            self.draft_text.clear();
            self.composer.set_text(&typed);
        }
        pending.epoch = Some(epoch);
        if unchanged {
            self.set_phone_view(
                connection,
                PhoneView::Session(pending.session_id.clone().unwrap()),
            );
        }
        let crate::remote::PendingKind::DraftInput { text, mode } = &pending.kind else {
            self.phone_answer(connection, &id, Ok(None));
            return;
        };
        let (text, mode) = (text.clone(), *mode);
        self.remote.hold_pending(connection, pending);
        self.push_phone_input(connection, id, text, mode);
    }

    /// Sends a terminal reply only to its original connection.
    fn phone_answer(
        &mut self,
        connection: u64,
        id: &str,
        reply: Result<Option<serde_json::Value>, wire::Refusal>,
    ) {
        self.remote.send(Some(connection), wire::ack(id, reply));
    }

    /// The phone's `input.push`, answered to the connection that made it.
    fn push_phone_input(&mut self, connection: u64, id: String, text: String, mode: InputMode) {
        let pending = self
            .remote
            .pending(connection, &id)
            .expect("a held phone input");
        let fields = json!({ "mode": mode, "text": text,
            "session_id":pending.session_id, "load":pending.load });
        let client = self.client.clone();
        self.spawn_msg(async move {
            let result = client
                .call::<_, crate::protocol::InputPushResult>(protocol::METHOD_INPUT_PUSH, &fields)
                .await;
            Msg::Remote(RemoteEvent::Pushed {
                connection,
                id,
                result: Box::new(result),
            })
        });
    }

    /// Answers a phone's `input.push` from the Agent's own answer.
    ///
    /// Acceptance is what the Agent says it is: only a refusal it classified as
    /// happening before acceptance leaves the phone free to show the text as
    /// unsent. An accepted input whose turn then failed to start is still
    /// accepted, with the failure the Agent reported.
    fn phone_pushed(
        &mut self,
        connection: u64,
        id: String,
        result: Result<crate::protocol::InputPushResult, crate::protocol::RpcError>,
    ) {
        let Some(pending) = self.remote.release_pending(connection, &id) else {
            return;
        };
        let result = result.and_then(|receipt| {
            tasks::accepted_input(
                receipt,
                pending.session_id.as_deref().expect("input target"),
                pending.load.as_deref().expect("input load"),
            )
        });
        let reply = match &result {
            Ok(pushed) => Ok(Some(self.pushed_fields(&pending, pushed))),
            Err(error) => Err(self.pushed_refusal(error)),
        };
        self.remote.send(Some(connection), wire::ack(&id, reply));
        self.refresh_phone_target(&pending);
    }

    /// The acceptance facts one accepted push reports.
    fn pushed_fields(
        &self,
        pending: &crate::remote::PendingIntent,
        pushed: &crate::protocol::InputPushResult,
    ) -> serde_json::Value {
        let mut fields =
            json!({"accepted": true, "session_id":pending.session_id, "load":pending.load});
        fields["input_id"] = json!(pushed.id);
        if let Some(accepted_at) = pushed.accepted_at.as_ref() {
            fields["accepted_at"] = json!(accepted_at);
        }
        if let Some(error) = pushed.start_error.as_ref() {
            fields["start_error"] = json!({
                "input_id": error.input_id,
                "kind": error.kind,
                "message": error.message,
            });
        }
        fields
    }

    /// How sure the phone may be that its text was not accepted.
    fn pushed_refusal(&self, error: &crate::protocol::RpcError) -> wire::Refusal {
        let acceptance = error
            .acceptance
            .unwrap_or(crate::protocol::Acceptance::Unknown);
        wire::Refusal {
            error: error.message.clone(),
            acceptance: Some(acceptance),
        }
    }

    /// The phone's `input.remove`, answered to the connection that made it.
    fn remove_phone_input(&mut self, connection: u64, operation: String, input_id: String) {
        let pending = self
            .remote
            .pending(connection, &operation)
            .expect("a held phone removal");
        let fields =
            json!({ "id": input_id, "session_id":pending.session_id, "load":pending.load });
        let client = self.client.clone();
        self.spawn_msg(async move {
            let result = client
                .call::<_, crate::protocol::InputRemoveResult>(
                    protocol::METHOD_INPUT_REMOVE,
                    &fields,
                )
                .await;
            Msg::Remote(RemoteEvent::Removed {
                connection,
                id: operation,
                result: Box::new(result),
            })
        });
    }

    /// Answers a phone's `input.remove`; an edit gets the text back.
    fn phone_removed(
        &mut self,
        connection: u64,
        id: String,
        result: Result<crate::protocol::InputRemoveResult, crate::protocol::RpcError>,
    ) {
        let Some(pending) = self.remote.release_pending(connection, &id) else {
            return;
        };
        let reply = match &result {
            Ok(removed) => match &pending.kind {
                crate::remote::PendingKind::Unqueue {
                    input_id,
                    edit: true,
                } => Ok(Some(json!({"input_id":input_id, "text":removed.text}))),
                _ => Ok(None),
            },
            Err(error) => Err(wire::Refusal {
                error: error.message.clone(),
                acceptance: Some(error.acceptance.unwrap_or(match error.kind.as_str() {
                    "input_not_queued" | "missing_load" | "stale_load" => {
                        crate::protocol::Acceptance::Rejected
                    }
                    _ => crate::protocol::Acceptance::Unknown,
                })),
            }),
        };
        self.remote.send(Some(connection), wire::ack(&id, reply));
        self.refresh_phone_target(&pending);
    }

    fn refresh_phone_target(&mut self, pending: &crate::remote::PendingIntent) {
        if let Some(epoch) = pending.epoch
            && self.resident.get(epoch).is_some_and(|session| {
                Some(&session.info.session_id) == pending.session_id.as_ref()
                    && Some(&session.load) == pending.load.as_ref()
            })
        {
            self.refresh_control_for(epoch);
        }
    }

    fn hold_phone_command(
        &mut self,
        connection: u64,
        id: &str,
        view_epoch: &str,
        session_id: Option<String>,
        text: Option<String>,
    ) {
        let session = session_id.as_deref().and_then(|id| self.live(id));
        let load = session.map(|s| s.load.clone());
        let epoch = session.map(|s| s.epoch);
        self.remote.hold_pending(
            connection,
            crate::remote::PendingIntent {
                id: id.to_owned(),
                view_epoch: view_epoch.to_owned(),
                session_id,
                load,
                epoch,
                focus_generation: self.remote.focus_generation,
                kind: crate::remote::PendingKind::Command { text },
            },
        );
    }

    fn call_phone_command(
        &self,
        connection: u64,
        id: String,
        operation: Operation,
        message: Option<String>,
    ) {
        let pending = self
            .remote
            .pending(connection, &id)
            .expect("a held phone command");
        let session_id = pending.session_id.clone().expect("a session command");
        let load = pending.load.clone();
        let client = self.client.clone();
        self.spawn_msg(async move {
            let result = async {
                let outcome = operation.execute(&client, &session_id, load.as_deref()).await?;
                if let Some(text) = message {
                    let fields = json!({"session_id":session_id,"load":load,"mode":"steer","text":text.trim_end()});
                    let receipt = client.call(protocol::METHOD_INPUT_PUSH, &fields).await?;
                    tasks::accepted_input(receipt, &session_id, load.as_deref().expect("an answer targets a loaded session"))?;
                }
                Ok(outcome)
            }.await;
            Msg::Remote(RemoteEvent::Finished { connection, id, result })
        });
    }

    fn phone_finished(
        &mut self,
        connection: u64,
        id: String,
        result: Result<Outcome, crate::protocol::RpcError>,
    ) {
        let Some(pending) = self.remote.release_pending(connection, &id) else {
            return;
        };
        let reply = result
            .map(|outcome| {
                self.with_notice_for(
                    pending.session_id.as_deref().expect("a session command"),
                    |app| {
                        app.apply_operation(pending.epoch, outcome);
                    },
                );
                None
            })
            .map_err(|error| wire::Refusal::new(error.message));
        self.remote.send(Some(connection), wire::ack(&id, reply));
        self.refresh_phone_target(&pending);
    }

    /// Reads phone text as the composer reads Enter, refusing what it refuses.
    fn phone_text(&mut self, text: &str) -> Result<String, wire::Refusal> {
        let command = commands::parse(text).and_then(|(name, arg)| commands::execute(name, arg));
        if command.is_some() {
            return Err(wire::Refusal::rejected("unknown_command"));
        }
        let text = match self
            .prompt_text(text)
            .map_err(|_| wire::Refusal::rejected("unknown_command"))?
        {
            Some(rewritten) => rewritten,
            None => text.to_owned(),
        };
        if text.trim().is_empty() {
            return Err(wire::Refusal::rejected("Remote text is empty"));
        }
        Ok(text.trim_end().to_owned())
    }

    fn hold_pending(&mut self, connection: u64, pending: crate::remote::PendingIntent) {
        self.remote.hold_pending(connection, pending);
    }

    /// Shows session `id`: at once when it is live, else opened in the Agent.
    pub fn open_session(&mut self, id: String) {
        if self
            .resident
            .current()
            .is_some_and(|session| session.info.session_id == id)
        {
            return;
        }
        if let Some(epoch) = self.live(&id).map(|session| session.epoch) {
            // The session being created takes the screen when it arrives.
            if self.first_message.is_none() {
                self.switch_to(epoch);
            }
            return;
        }
        self.request_session_open(id);
    }
}
