//! Phone operations retain their target until the Agent decides their acknowledgement.

use serde_json::json;

use crate::app::operation::{Operation, Outcome};
use crate::app::{App, LaunchSettings, Msg, tasks};
use crate::protocol::{self, InputMode};
use crate::remote::{
    DraftSettings, PendingIntent, PendingKind, PendingTarget, PhoneView, RemoteEvent, wire,
};

impl App {
    pub(super) fn open_remote_session(
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
            self.hold_phone(
                connection,
                operation,
                view_epoch,
                Some(session_id.clone()),
                PendingKind::Open,
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

    /// Hands phone text to the Agent; its answer is the acknowledgement.
    ///
    /// The phone's own operation makes the call, so the acknowledgement carries
    /// that operation's identity and the acceptance facts the Agent reported,
    /// and no other connection can be answered for it.
    pub(super) fn remote_text(
        &mut self,
        connection: u64,
        id: &str,
        text: String,
        mode: InputMode,
        view_epoch: &str,
        session_id: Option<String>,
    ) -> Option<Result<Option<serde_json::Value>, wire::Refusal>> {
        if !self.ready {
            return Some(Err(wire::Refusal::rejected(
                "A session is opening or being created",
            )));
        }
        let resident = session_id.as_deref().and_then(|id| self.live(id));
        if resident.is_none() {
            // A draft creates its session first. The push of its first text is
            // made by this operation, so it answers that operation either way.
            self.hold_phone(
                connection,
                id,
                view_epoch,
                session_id,
                PendingKind::DraftInput { text, mode },
            );
            self.remote.opening = Some(connection);
            self.create_phone_session(connection, id.to_owned());
            return None;
        }
        self.hold_phone(
            connection,
            id,
            view_epoch,
            session_id,
            PendingKind::Input { text: text.clone() },
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
        let params = self.create_fields();
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
    pub(super) fn phone_created(
        &mut self,
        connection: u64,
        id: String,
        result: Result<crate::protocol::OpenedSession, crate::protocol::RpcError>,
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
                    PendingKind::Open => wire::Refusal::new(error.message),
                    _ => wire::Refusal::rejected(error.message),
                };
                self.phone_answer(connection, &id, Err(refusal));
                return;
            }
        };
        if matches!(pending.kind, PendingKind::Open)
            && pending.target.session_id() != Some(created.session.session_id.as_str())
        {
            self.phone_answer(
                connection,
                &id,
                Err(wire::Refusal::new("Opened session identity changed")),
            );
            return;
        }
        if matches!(pending.kind, PendingKind::Open)
            && !self
                .sessions
                .iter()
                .any(|s| s.session_id == created.session.session_id)
        {
            self.phone_answer(
                connection,
                &id,
                Err(wire::Refusal::new("Session deleted while opening")),
            );
            return;
        }
        let unchanged = self
            .remote
            .phone(connection)
            .is_some_and(|phone| phone.projection.view_epoch() == pending.view_epoch);
        let keep_focus = unchanged && self.remote.focus_generation == pending.focus_generation;
        let typed = (keep_focus
            && self.resident.current().is_none()
            && matches!(pending.kind, PendingKind::DraftInput { .. }))
        .then(|| self.composer.text().to_owned());
        let epoch = match self.install_session(created, keep_focus, true) {
            Ok(epoch) => epoch,
            Err(error) => {
                self.phone_answer(connection, &id, Err(wire::Refusal::new(error.message)));
                return;
            }
        };
        if keep_focus {
            self.launch = LaunchSettings::default();
            self.remote.draft = None;
        }
        if let Some(typed) = typed {
            self.draft_text.clear();
            self.composer.set_text(&typed);
        }
        let target = self.resident.get(epoch).unwrap().target();
        if unchanged {
            self.set_phone_view(connection, PhoneView::Session(target.session_id.clone()));
        }
        pending.target = PendingTarget::Resident(target);
        let PendingKind::DraftInput { text, mode } = &pending.kind else {
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
        let target = pending.target.resident().expect("a resident input");
        let fields = json!({ "mode": mode, "text": text,
            "session_id":target.session_id, "load":target.load });
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
    pub(super) fn phone_pushed(
        &mut self,
        connection: u64,
        id: String,
        result: Result<crate::protocol::InputPushResult, crate::protocol::RpcError>,
    ) {
        let Some(pending) = self.remote.release_pending(connection, &id) else {
            return;
        };
        let target = pending.target.resident().expect("a resident input");
        let result = result
            .and_then(|receipt| tasks::accepted_input(receipt, &target.session_id, &target.load));
        let reply = match &result {
            Ok(pushed) => Ok(Some(self.pushed_fields(pushed))),
            Err(error) => Err(self.pushed_refusal(error)),
        };
        self.phone_answer(connection, &id, reply);
        self.refresh_phone_target(&pending);
    }

    /// The acceptance facts one accepted push reports.
    fn pushed_fields(&self, pushed: &crate::protocol::InputPushResult) -> serde_json::Value {
        let mut fields =
            json!({"accepted": true, "session_id":pushed.session_id, "load":pushed.load});
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
    pub(super) fn remove_phone_input(
        &mut self,
        connection: u64,
        operation: String,
        input_id: String,
    ) {
        let pending = self
            .remote
            .pending(connection, &operation)
            .expect("a held phone removal");
        let target = pending.target.resident().expect("a resident removal");
        let fields = json!({ "id": input_id, "session_id":target.session_id, "load":target.load });
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
    pub(super) fn phone_removed(
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
                PendingKind::Unqueue {
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
        self.phone_answer(connection, &id, reply);
        self.refresh_phone_target(&pending);
    }

    fn refresh_phone_target(&mut self, pending: &PendingIntent) {
        if let Some(target) = pending.target.resident()
            && self.resident.get(target.epoch).is_some()
        {
            self.refresh_control_for(target.epoch);
        }
    }

    pub(super) fn call_phone_command(
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
        let session_id = pending
            .target
            .session_id()
            .expect("a session command")
            .to_owned();
        let load = pending.target.resident().map(|target| target.load.clone());
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

    pub(super) fn phone_finished(
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
                    pending.target.session_id().expect("a session command"),
                    |app| {
                        app.apply_operation(
                            pending.target.resident().map(|target| target.epoch),
                            outcome,
                        );
                    },
                );
                None
            })
            .map_err(|error| wire::Refusal::new(error.message));
        self.phone_answer(connection, &id, reply);
        self.refresh_phone_target(&pending);
    }

    pub(super) fn hold_phone(
        &mut self,
        connection: u64,
        id: &str,
        view_epoch: &str,
        session_id: Option<String>,
        kind: PendingKind,
    ) {
        let target = match session_id {
            None => PendingTarget::Draft,
            Some(id) => match self.live(&id) {
                Some(session) => PendingTarget::Resident(session.target()),
                None => PendingTarget::Stored(id),
            },
        };
        self.remote.hold_pending(
            connection,
            PendingIntent {
                id: id.to_owned(),
                view_epoch: view_epoch.to_owned(),
                target,
                focus_generation: self.remote.focus_generation,
                kind,
            },
        );
    }
}
