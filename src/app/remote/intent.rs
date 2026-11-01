//! Admission and dispatch of phone operations against their named view.

use std::path::PathBuf;

use serde_json::json;

use crate::app::operation::{self, Operation};
use crate::app::{Answer, App, InteractionKind, LaunchSettings, commands};
use crate::protocol::{InputMode, Reasoning};
use crate::remote::{
    DraftSettings, NoticeTarget, PendingKind, PhoneView, fs,
    wire::{self, Choice, Intent},
};

impl App {
    /// Validates an intent against the projection it was built from and runs it.
    ///
    /// `None` means the answer waits on the Agent and is sent from that
    /// result; `Some` is answered at once, before any side effect that could
    /// still fail.
    pub(super) fn phone_intent(
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
        if self.remote.pending(connection, id).is_some() {
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
        let phone = self.admitted(connection);
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

    /// An input operation that the Agent never accepted may be shown as unsent.
    fn refusal(&self, intent: &Intent, error: &str) -> wire::Refusal {
        match intent {
            Intent::Submit { .. } | Intent::Queue { .. } | Intent::Unqueue { .. } => {
                wire::Refusal::rejected(error)
            }
            _ => wire::Refusal::new(error),
        }
    }

    /// Dispatches a checked intent, returning a local reply or waiting for its Agent result.
    fn remote_intent(
        &mut self,
        connection: u64,
        id: &str,
        intent: Intent,
        view_epoch: &str,
        mut session_id: Option<String>,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        let epoch = session_id
            .as_deref()
            .and_then(|id| self.live(id))
            .map(|session| session.epoch);
        let mut retained = None;
        let mut message = None;
        let operation = match intent {
            Intent::Submit { text, .. } => {
                return self.remote_text(
                    connection,
                    id,
                    text,
                    InputMode::Steer,
                    view_epoch,
                    session_id,
                );
            }
            Intent::Queue { text, .. } => {
                return self.remote_text(
                    connection,
                    id,
                    text,
                    InputMode::FollowUp,
                    view_epoch,
                    session_id,
                );
            }
            Intent::Cancel {
                loop_id,
                operation_id,
                ..
            } => {
                // Cancel only the turn or compaction the phone was shown.
                let session = epoch.and_then(|epoch| self.resident.get(epoch));
                let live = session.and_then(|s| s.live.as_ref());
                match (loop_id, live) {
                    (Some(named), Some(current))
                        if named == current.turn.loop_id && operation_id.is_none() =>
                    {
                        Operation::CancelTurn(named)
                    }
                    (None, None) => {
                        match (operation_id, session.and_then(|s| s.compaction.as_ref())) {
                            (Some(named), Some(current)) if named == current.operation_id => {
                                Operation::CancelCompaction(named)
                            }
                            (None, None) => return Some(Ok(None)),
                            _ => return Some(Err(wire::Refusal::new("stale"))),
                        }
                    }
                    _ => return Some(Err(wire::Refusal::new("stale"))),
                }
            }
            Intent::Compact { .. } => {
                let Some(session) = epoch.and_then(|epoch| self.resident.get(epoch)) else {
                    return Some(Err(wire::Refusal::new("No session")));
                };
                if session.is_busy() || session.compacting() {
                    return Some(Err(wire::Refusal::new("busy")));
                }
                self.new_compaction()
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
                self.hold_phone(
                    connection,
                    id,
                    view_epoch,
                    session_id,
                    PendingKind::Unqueue {
                        input_id: input_id.clone(),
                        edit,
                    },
                );
                self.remove_phone_input(connection, id.to_owned(), input_id);
                return None;
            }
            Intent::Answer {
                choice,
                message: reason,
                ..
            } => {
                let interaction = epoch
                    .and_then(|epoch| self.resident.get(epoch))
                    .and_then(|s| s.interaction.as_ref())
                    .expect("the answer target was validated before admission");
                let answer = match (choice, &interaction.kind) {
                    (Choice::Text { text }, InteractionKind::Text) => Answer::Text(text),
                    (Choice::Name(name), InteractionKind::Approval) => match name.as_str() {
                        "allow" => Answer::Allow,
                        "allow_session" => Answer::AllowTool,
                        "deny" => Answer::Deny,
                        _ => return Some(Err(wire::Refusal::new("bad_request"))),
                    },
                    (Choice::Index(index), InteractionKind::Choice(options))
                        if index < options.len() =>
                    {
                        Answer::Choice(index)
                    }
                    _ => return Some(Err(wire::Refusal::new("bad_request"))),
                };
                if reason
                    .as_ref()
                    .is_some_and(|message| answer != Answer::Deny || message.trim().is_empty())
                {
                    return Some(Err(wire::Refusal::new("bad_request")));
                }
                retained = match &answer {
                    Answer::Text(text) => Some(text.clone()),
                    _ => reason.clone(),
                };
                message = reason;
                match operation::answer(interaction, answer) {
                    Ok(Some(operation)) => operation,
                    _ => return Some(Err(wire::Refusal::new("bad_request"))),
                }
            }
            Intent::LoadOlder { .. } => {
                let Some(epoch) = epoch else {
                    return Some(Err(wire::Refusal::new("No session")));
                };
                self.load_older_for(epoch);
                return Some(Ok(None));
            }
            Intent::OpenSession { session_id, .. } => {
                return self.open_remote_session(connection, id, view_epoch, session_id);
            }
            Intent::RenameSession {
                session_id: named,
                title,
                ..
            } => {
                if let Err(refusal) = self.remote_session(&named) {
                    return Some(Err(wire::Refusal::new(refusal)));
                }
                if title.trim().chars().count() > 200 || title.chars().any(char::is_control) {
                    return Some(Err(wire::Refusal::new("bad_request")));
                }
                session_id = Some(named);
                Operation::Rename(title)
            }
            Intent::DeleteSession {
                session_id: named, ..
            } => {
                if let Err(error) = self.check_phone_delete(connection, &named) {
                    return Some(Err(error));
                }
                session_id = Some(named);
                Operation::Delete
            }
            Intent::NewSession {
                workspace, profile, ..
            } => return Some(self.draft_in(workspace, profile)),
            Intent::SetModel { model, .. } => {
                if !self.models.iter().any(|m| m.id == model) {
                    return Some(Err(wire::Refusal::new("Unknown model")));
                }
                let Some(epoch) = epoch else {
                    self.set_model(model);
                    return Some(Ok(None));
                };
                let current = self.resident.get(epoch).unwrap().info.reasoning;
                Operation::Update(self.model_fields(&model, Some(current)))
            }
            Intent::SetReasoning { level, .. } => {
                let Some(level) = Reasoning::parse(&level) else {
                    return Some(Err(wire::Refusal::new("Unknown reasoning level")));
                };
                if epoch.is_none() {
                    self.set_reasoning(level);
                    return Some(Ok(None));
                }
                Operation::Update(json!({"reasoning":level}))
            }
        };
        self.hold_phone(
            connection,
            id,
            view_epoch,
            session_id,
            PendingKind::Command { text: retained },
        );
        self.call_phone_command(connection, id.to_owned(), operation, message);
        None
    }

    fn check_phone_delete(&self, connection: u64, session_id: &str) -> Result<(), wire::Refusal> {
        self.remote_session(session_id)
            .map_err(wire::Refusal::new)?;
        if self.phone_view_session(connection) == Some(session_id) {
            return Err(wire::Refusal::new(
                "Open another session before deleting this one",
            ));
        }
        if let Some(phone) = self
            .remote
            .phones
            .iter()
            .find(|phone| phone.view.session_id() == Some(session_id))
        {
            return Err(wire::Refusal::new(format!(
                "Open on \"{}\"",
                phone.connection.device.name
            )));
        }
        if let Some(refusal) = self.delete_refusal(session_id) {
            return Err(wire::Refusal::new(refusal));
        }
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
    pub(super) fn remote_session(&self, session_id: &str) -> Result<(), String> {
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
}
