//! Terminal intents expressed through the existing typed Host client.

use minicore_host_core::{ApprovalAnswer, InteractionAnswerData, InteractionRequestData};
use minicore_host_protocol::*;
use minicore_host_runtime::{
    Text,
    tools::{ToolInputAnswer, ToolInputAnswerKind},
};

use super::{App, Msg, NoticeLevel, Scroll, SessionOp, tasks};
use crate::config::ThemeKind;
use crate::ui::theme::{Palette, Styles};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Submit {
        follow: bool,
    },
    NewSession,
    SelectAgent(AgentId),
    CreateAgent(String),
    OpenSession(SessionRef),
    Rename(String),
    RenameSession {
        target: SessionRef,
        title: String,
    },
    DeleteSession(SessionRef),
    SwitchModel(String),
    CycleModel {
        forward: bool,
    },
    SetReasoning(ReasoningPreference),
    Compact,
    CopyLastAnswer,
    Export(Option<String>),
    EditDraft,
    Answer(Answer),
    LoadOlder,
    RecallQueued,
    RetryRecovery,
    SetTheme(ThemeKind),
    ToggleTheme,
    ReloadHost,
    RemoteStart(Option<String>),
    RemoteStop,
    RemoteRotateKey,
    Pause,
    Resume,
    Load,
    Unload,
    CloseSession,
    /// `/close`: states what closing means before `CloseSession` runs.
    ConfirmClose,
    StartAgent,
    StopAgent,
    DeleteAgent {
        agent_id: AgentId,
        session_ids: Vec<SessionId>,
    },
    SetDefaultSession,
    SetApproval(ApprovalMode),
    /// `/child [n]`: the nth latest delegated child of this conversation.
    OpenChild(usize),
    Interrupt,
    Quit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Deny,
    Choice(usize),
    Text(String),
}

impl App {
    pub fn on_action(&mut self, action: Action) {
        match action {
            Action::Submit { follow } => self.submit_composer(follow),
            Action::NewSession => self.create_session(),
            Action::SelectAgent(id) => {
                self.selected_agent = Some(id.clone());
                self.select_session(None);
                if let Some(session_id) = self
                    .agents
                    .iter()
                    .find(|agent| agent.id == id)
                    .and_then(|a| a.default_session_id.clone())
                {
                    self.open_session(SessionRef {
                        agent_id: id,
                        session_id,
                    });
                }
            }
            Action::CreateAgent(name) => {
                if !self.require_control() {
                    return;
                }
                let client = self.client.clone();
                let Some(agent_id) = self.current_agent_id().cloned() else {
                    self.notice(
                        NoticeLevel::Error,
                        "Select an Agent to copy, or add agents/*.json and run /reload first",
                    );
                    return;
                };
                let source = AgentRef {
                    host_id: client.initial_info().host_id.clone(),
                    agent_id,
                };
                let overrides = self.creation_overrides();
                self.launch = super::LaunchSettings::default();
                self.open_generation += 1;
                self.opening_target = None;
                self.loading_session = false;
                let generation = self.open_generation;
                self.spawn_msg(async move {
                    Msg::AgentCreated(
                        generation,
                        async {
                            let config = client
                                .agent(source, tasks::options())
                                .await
                                .map_err(|error| error.to_string())?
                                .info
                                .config;
                            tasks::create_agent(&client, name, config, overrides).await
                        }
                        .await,
                    )
                });
            }
            Action::OpenSession(target) => self.open_session(target),
            Action::Rename(title) => {
                if let Some(target) = self.resident.current().map(|s| s.address()) {
                    self.rename_target(target, title);
                }
            }
            Action::RenameSession { target, title } => self.rename_target(target, title),
            Action::DeleteSession(target) => {
                if !self.require_control() {
                    return;
                }
                let client = self.client.clone();
                self.spawn_msg(async move {
                    let result = async {
                        client
                            .session(target.clone())
                            .delete(
                                SessionDeleteRequest {
                                    operation_id: tasks::operation_id()?,
                                    close: true,
                                },
                                tasks::options(),
                            )
                            .await
                            .map(drop)
                            .map_err(|e| e.to_string())
                    }
                    .await
                    .map_err(|error| tasks::session_error(&target, error));
                    Msg::RefreshCatalogs(result)
                });
            }
            Action::SwitchModel(text) => {
                match super::commands::resolve_model(&self.models, &text) {
                    Ok(model) => self.set_model(model.model.clone()),
                    Err(error) => self.notice(NoticeLevel::Error, error),
                }
            }
            Action::CycleModel { forward } => self.cycle_model(forward),
            Action::SetReasoning(reasoning) => self.set_reasoning(reasoning),
            Action::Compact => self.compact(),
            Action::CopyLastAnswer => self.copy_last_answer(),
            Action::Export(path) => {
                if let Some(session) = self.resident.current() {
                    self.spawn_msg(tasks::export(session.client.clone(), path));
                }
            }
            Action::EditDraft => self.edit_request = Some(self.composer.take_for_editor()),
            Action::Answer(answer) => self.answer(answer),
            Action::LoadOlder => self.load_older(),
            Action::RecallQueued => self.recall_queued(),
            Action::RetryRecovery => {
                if let Some(epoch) = self.resident.current().map(|s| s.epoch) {
                    self.retry_failed_reads(epoch);
                    self.resubscribe(epoch);
                    self.refresh_history(epoch);
                    self.refresh_control_for(epoch);
                }
            }
            Action::SetTheme(theme) => self.apply_theme(theme),
            Action::ToggleTheme => self.apply_theme(match self.theme {
                ThemeKind::Dark => ThemeKind::Light,
                ThemeKind::Light => ThemeKind::Dark,
            }),
            Action::ReloadHost => self.reload_host(),
            Action::RemoteStart(listen_override) => self.start_remote(listen_override),
            Action::RemoteStop => self.stop_remote(),
            Action::RemoteRotateKey => self.rotate_remote_key(),
            Action::Pause => self.session_control("pause"),
            Action::Resume => self.session_control("resume"),
            Action::Load => self.session_control("load"),
            Action::Unload => self.session_control("unload"),
            Action::CloseSession => self.close_session(),
            Action::StartAgent => self.control_agent(true),
            Action::StopAgent => self.control_agent(false),
            Action::DeleteAgent {
                agent_id,
                session_ids,
            } => self.delete_agent(agent_id, session_ids),
            Action::SetDefaultSession => self.set_default_session(),
            Action::SetApproval(approval) => {
                if let Some(session) = self.resident.current() {
                    let mut execution = session.info.config.execution.clone();
                    execution.approval = approval;
                    self.update_session(SessionConfigPatch {
                        execution: Some(execution),
                        ..Default::default()
                    });
                } else {
                    self.notice(
                        NoticeLevel::Info,
                        "Select a Session to change its approval policy",
                    );
                }
            }
            Action::OpenChild(nth) => self.open_nth_child(nth),
            Action::ConfirmClose => {
                if let Some(session) = self.resident.current() {
                    let label = match &session.info.title {
                        Some(title) => format!("{title} · {}", session.info.id),
                        None => session.info.id.to_string(),
                    };
                    self.overlay = super::Overlay::Confirm {
                        title: "Close Session".into(),
                        lines: vec![
                            label,
                            "A closed session cannot run again; its history stays readable.".into(),
                            "/new starts a new session.".into(),
                        ],
                        action: Action::CloseSession,
                    };
                }
            }
            Action::Interrupt => {
                if self.is_busy() {
                    self.interrupt();
                } else {
                    self.notice(NoticeLevel::Info, "Nothing is running");
                }
            }
            Action::Quit => self.request_quit(),
        }
    }
    fn control_agent(&mut self, start: bool) {
        if !self.require_control() {
            return;
        }
        let Some(agent_id) = self.current_agent_id().cloned() else {
            return;
        };
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.spawn_msg(async move {
            let result: Result<_, String> = async {
                let target = AgentRef {
                    host_id: client.initial_info().host_id.clone(),
                    agent_id: agent_id.clone(),
                };
                let operation_id = tasks::operation_id()?;
                if start {
                    client
                        .start_agent(
                            target.clone(),
                            AgentStartRequest { operation_id },
                            tasks::options(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                } else {
                    client
                        .stop_agent(
                            target.clone(),
                            AgentStopRequest { operation_id },
                            tasks::options(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Ok(target)
            }
            .await;
            match result {
                Ok(target) => {
                    tasks::observe_agent(&client, &tx, target, tasks::AgentWait::Lifecycle).await
                }
                Err(error) => Msg::RefreshCatalogs(Err(format!("Agent: {agent_id}\n\n{error}"))),
            }
        });
    }
    fn delete_agent(&mut self, agent_id: AgentId, session_ids: Vec<SessionId>) {
        if !self.require_control() {
            return;
        }
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.spawn_msg(async move {
            let result: Result<_, String> = async {
                let target = AgentRef {
                    host_id: client.initial_info().host_id.clone(),
                    agent_id: agent_id.clone(),
                };
                let scope = if session_ids.is_empty() {
                    AgentDeleteScope::Empty {}
                } else {
                    AgentDeleteScope::Sessions {
                        session_ids,
                        close: true,
                    }
                };
                client
                    .delete_agent(
                        target.clone(),
                        AgentDeleteRequest {
                            operation_id: tasks::operation_id()?,
                            scope,
                        },
                        tasks::options(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(target)
            }
            .await;
            match result {
                Ok(target) => {
                    tasks::observe_agent(&client, &tx, target, tasks::AgentWait::Deletion).await
                }
                Err(error) => Msg::RefreshCatalogs(Err(format!("Agent: {agent_id}\n\n{error}"))),
            }
        });
    }
    fn set_default_session(&mut self) {
        if !self.require_control() {
            return;
        }
        let Some(target) = self.resident.current().map(|s| s.address()) else {
            return;
        };
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.spawn_msg(async move {
            let result = async {
                let agent = AgentRef {
                    host_id: client.initial_info().host_id.clone(),
                    agent_id: target.agent_id.clone(),
                };
                client
                    .set_default_session(
                        agent.clone(),
                        AgentSetDefaultSessionRequest {
                            operation_id: tasks::operation_id()?,
                            session_id: Some(target.session_id.clone()),
                        },
                        tasks::options(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(agent)
            }
            .await;
            let message = match result {
                Ok(agent) => {
                    tasks::observe_agent(&client, &tx, agent, tasks::AgentWait::Metadata).await
                }
                Err(error) => Msg::RefreshCatalogs(Err(error)),
            };
            match message {
                Msg::RefreshCatalogs(Err(error)) => {
                    Msg::RefreshCatalogs(Err(tasks::session_error(&target, error)))
                }
                message => message,
            }
        });
    }
    fn submit_composer(&mut self, follow: bool) {
        if let super::Overlay::Unconfirmed { mode, .. } = &mut self.overlay {
            if let super::unconfirmed::ReviewMode::Edit(id) = *mode {
                *mode = super::unconfirmed::ReviewMode::ResendEdited(id);
            }
            return;
        }
        if !self.ready || self.loading_session || !self.require_control() {
            return;
        }
        if !self.composer.text().trim_end().is_empty() {
            if !self.local_input_room(
                self.composer.text().len() + std::mem::size_of_val(self.composer.pastes()),
            ) {
                self.notice(
                    NoticeLevel::Error,
                    "Local input confirmation budget is full; F8 to review retained inputs",
                );
                return;
            }
            let entry = self.composer.take_submission();
            self.submit_entry(entry, follow, None);
        } else if !follow && self.resident.current().is_some_and(|s| s.paused()) {
            self.session_control("resume");
        }
    }
    pub(super) fn submit_entry(
        &mut self,
        entry: super::composer::Entry,
        follow: bool,
        retry_of: Option<u64>,
    ) {
        if !self.require_control() {
            self.composer.restore_submission(entry);
            return;
        }
        if let Some(session) = self.resident.current() {
            if session.state.data.lifecycle != SessionLifecycle::Open {
                self.composer.restore_submission(entry);
                self.notice(
                    NoticeLevel::Error,
                    "This Session is closed; create a new Session to send input",
                );
                return;
            }
            let epoch = session.epoch;
            if !session.is_busy() {
                self.scroll = Scroll::Follow;
            }
            self.push_local_input(
                epoch,
                entry,
                if follow {
                    MessageMode::FollowUp
                } else {
                    MessageMode::Steer
                },
                retry_of,
            );
        } else {
            if !self.local_input_room(entry.retained_bytes() + entry.text().len()) {
                self.composer.restore_submission(entry);
                self.notice(
                    NoticeLevel::Error,
                    "Local input confirmation budget is full; F8 to review retained inputs",
                );
                return;
            }
            self.start_session_creation(
                Some(entry),
                if follow {
                    MessageMode::FollowUp
                } else {
                    MessageMode::Steer
                },
            );
        }
    }
    pub(super) fn create_session(&mut self) {
        self.start_session_creation(None, MessageMode::Steer);
    }
    fn start_session_creation(&mut self, entry: Option<super::composer::Entry>, mode: MessageMode) {
        if !self.require_control() {
            if let Some(entry) = entry {
                self.restore_creation_entry(entry);
            }
            return;
        }
        let Some(agent) = self.current_agent_id().cloned() else {
            if let Some(entry) = entry {
                self.restore_creation_entry(entry);
            }
            self.notice(
                NoticeLevel::Error,
                "Select an Agent, or add agents/*.json and run /reload first",
            );
            return;
        };
        if self.pending_creations.len() >= super::unconfirmed::MAX_OPERATIONS {
            if let Some(entry) = entry {
                self.restore_creation_entry(entry);
            }
            self.notice(
                NoticeLevel::Error,
                "Wait for pending Session creation to finish",
            );
            return;
        }
        self.open_generation += 1;
        let generation = self.open_generation;
        self.opening_target = None;
        self.loading_session = true;
        self.pending_creations
            .insert(generation, super::PendingCreation { entry, mode });
        let client = self.client.clone();
        let overrides = self.creation_overrides();
        self.launch = super::LaunchSettings::default();
        self.spawn_msg(async move {
            Msg::Created(
                generation,
                tasks::create_session(&client, agent, overrides).await,
            )
        });
    }
    pub(super) fn creation_failed(&mut self, generation: u64, error: String) {
        if generation == self.open_generation {
            self.loading_session = false;
        }
        let creation = self
            .pending_creations
            .remove(&generation)
            .expect("creation owns its failed result");
        if let Some(entry) = creation.entry {
            self.restore_creation_entry(entry);
        }
        self.notice(NoticeLevel::Error, error);
    }
    fn restore_creation_entry(&mut self, entry: super::composer::Entry) {
        if self.resident.current().is_none() {
            if self.composer.is_empty() {
                self.composer.restore_submission(entry);
            } else {
                self.composer.insert(&format!("\n\n{}", entry.text()));
            }
        } else {
            if !self.draft_text.is_empty() {
                self.draft_text.push_str("\n\n");
            }
            self.draft_text.push_str(entry.text());
        }
    }
    pub(super) fn request_session_open(&mut self, target: SessionRef) {
        self.open_generation += 1;
        let generation = self.open_generation;
        self.opening_target = Some((generation, target.clone()));
        self.loading_session = true;
        let client = self.client.clone();
        let launch = std::mem::take(&mut self.launch);
        self.spawn_msg(async move {
            let result = tasks::open(client, target.clone(), launch).await;
            Msg::Opened(generation, target, result)
        });
    }
    pub(super) fn mutate(
        &self,
        epoch: u64,
        label: &str,
        work: impl Future<Output = Result<(), String>> + Send + 'static,
    ) {
        let label = label.to_owned();
        self.spawn_for(epoch, async move {
            SessionOp::Mutation {
                label,
                result: work.await,
            }
        });
    }
    fn session_control(&mut self, kind: &'static str) {
        if !self.require_control() {
            return;
        }
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        self.mutate(epoch, kind, async move {
            let request = SessionControlRequest {
                operation_id: tasks::operation_id()?,
            };
            match kind {
                "pause" => client.pause(request, tasks::options()).await,
                "resume" => client.resume(request, tasks::options()).await,
                "load" => client.load(request, tasks::options()).await,
                "unload" => client.unload(request, tasks::options()).await,
                _ => unreachable!("terminal control action"),
            }
            .map(drop)
            .map_err(|e| e.to_string())
        });
    }
    fn close_session(&mut self) {
        if !self.require_control() {
            return;
        }
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        self.mutate(epoch, "close", async move {
            client
                .close(
                    SessionCloseRequest {
                        operation_id: tasks::operation_id()?,
                    },
                    tasks::options(),
                )
                .await
                .map(drop)
                .map_err(|e| e.to_string())
        });
    }
    pub(super) fn interrupt(&mut self) {
        if !self.require_control() {
            return;
        }
        let Some(session) = self.resident.current_mut() else {
            return;
        };
        let Some(turn_id) = session.state.data.active_turn_id.clone() else {
            return;
        };
        if let Some(live) = &mut session.live {
            if live.interrupting {
                return;
            }
            live.interrupting = true;
        }
        let epoch = session.epoch;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let result = async {
                client
                    .cancel_turn(
                        TurnCancelRequest {
                            operation_id: tasks::operation_id()?,
                            turn_id: turn_id.clone(),
                            keep_inbox: true,
                        },
                        tasks::options(),
                    )
                    .await
                    .map(drop)
                    .map_err(|e| e.to_string())
            }
            .await;
            SessionOp::Cancelled { turn_id, result }
        });
    }
    pub fn escape(&mut self) {
        // From a child opened through its card, Esc goes back rather than
        // interrupting the child's delegated work; /cancel interrupts it.
        if self.composer.is_empty()
            && let Some(parent) = self.return_target().cloned()
        {
            self.open_session(parent);
            return;
        }
        if self.is_busy() {
            self.interrupt();
            return;
        }
        let compact = self.resident.current().and_then(|s| {
            s.state
                .data
                .compaction
                .as_ref()
                .map(|a| (s.epoch, s.client.clone(), a.source.clone()))
        });
        if let Some((
            epoch,
            client,
            minicore_host_core::CompactionSource::Manual {
                operation_id: target_operation_id,
            },
        )) = compact
        {
            if !self.require_control() {
                return;
            }
            self.mutate(epoch, "cancel compaction", async move {
                client
                    .cancel_compact(
                        CompactCancelRequest {
                            operation_id: tasks::operation_id()?,
                            target_operation_id,
                        },
                        tasks::options(),
                    )
                    .await
                    .map(drop)
                    .map_err(|e| e.to_string())
            });
        } else if !self.composer.is_empty() {
            self.composer.clear();
            self.notice(NoticeLevel::Info, "Draft cleared · ctrl+z restores it");
        }
    }
    pub(super) fn compact(&mut self) {
        if !self.require_control() {
            return;
        }
        if self.is_busy() {
            // Host rejects it as SESSION_BUSY; say what to do instead.
            self.notice(
                NoticeLevel::Usage,
                "A turn is running; /compact after it finishes, or /cancel it first",
            );
            return;
        }
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        self.mutate(epoch, "compact", async move {
            client
                .compact(
                    CompactRequest {
                        operation_id: tasks::operation_id()?,
                    },
                    tasks::options(),
                )
                .await
                .map(drop)
                .map_err(|e| e.to_string())
        });
    }
    fn rename_target(&mut self, target: SessionRef, title: String) {
        if !self.require_control() {
            return;
        }
        let client = self.client.clone();
        self.spawn_msg(async move {
            let result = async {
                let session = client.session(target.clone());
                let info = session
                    .info(tasks::options())
                    .await
                    .map_err(|e| e.to_string())?;
                tasks::update(
                    &session,
                    info.config_revision,
                    SessionUpdateChanges {
                        title: NullablePatch::Set(title),
                        config: SessionConfigPatch::default(),
                    },
                )
                .await
            }
            .await;
            Msg::SessionRenamed(target, result)
        });
    }
    pub(super) fn update_session(&mut self, config: SessionConfigPatch) {
        if !self.require_control() {
            return;
        }
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        let revision = session.info.config_revision;
        self.mutate(epoch, "update Session", async move {
            tasks::update(
                &client,
                revision,
                SessionUpdateChanges {
                    title: NullablePatch::Unchanged,
                    config,
                },
            )
            .await
        });
    }
    fn cycle_model(&mut self, forward: bool) {
        if self.models.len() < 2 {
            self.notice(NoticeLevel::Info, "Only one model is available");
            return;
        }
        let at = self
            .models
            .iter()
            .position(|model| Some(model.model.as_str()) == self.model_id());
        let next = at.map_or(0, |at| {
            super::picker::step(at, if forward { 1 } else { -1 }, self.models.len())
        });
        self.set_model(self.models[next].model.clone());
    }
    fn copy_last_answer(&mut self) {
        match self
            .resident
            .current()
            .and_then(|s| s.transcript.last_answer())
            .map(str::to_owned)
        {
            Some(text) => self.copy(text),
            None => self.notice(NoticeLevel::Info, "No answer to copy yet"),
        }
    }
    fn apply_theme(&mut self, kind: ThemeKind) {
        if let Some(home) = &self.config_directory {
            self.local_files.theme(&self.tx, home.ui_file(), kind);
        }
        self.theme = kind;
        self.styles = Styles::of(Palette::for_kind(kind));
        self.layout.clear();
    }
    pub fn editor_command(&self) -> Result<Vec<String>, String> {
        crate::config::editor_command(&self.editor).ok_or_else(|| {
            "No editor configured: set ui.editor in tui.json or $VISUAL/$EDITOR".into()
        })
    }
    pub fn take_edit_request(&mut self) -> Option<String> {
        self.edit_request.take()
    }
    pub(super) fn answer(&mut self, answer: Answer) {
        if !self.require_control() {
            return;
        }
        let Some(session) = self.resident.current_mut() else {
            return;
        };
        let Some(pending) = session.interaction.as_mut() else {
            return;
        };
        if pending.submitted {
            return;
        }
        let response = match (&pending.data.request, answer) {
            (InteractionRequestData::Approval { .. }, Answer::Allow | Answer::Choice(0)) => {
                Some(InteractionAnswerData::Approval {
                    decision: ApprovalAnswer::AllowOnce,
                })
            }
            (InteractionRequestData::Approval { .. }, Answer::Deny | Answer::Choice(_)) => {
                Some(InteractionAnswerData::Approval {
                    decision: ApprovalAnswer::Deny,
                })
            }
            (InteractionRequestData::ToolInput { request }, Answer::Text(text))
                if request.answer_kind == ToolInputAnswerKind::Text =>
            {
                Some(InteractionAnswerData::ToolInput {
                    answer: ToolInputAnswer::Text(Text::new(text)),
                })
            }
            (InteractionRequestData::ToolInput { request }, Answer::Choice(index))
                if request.answer_kind == ToolInputAnswerKind::SingleChoice =>
            {
                Some(InteractionAnswerData::ToolInput {
                    answer: ToolInputAnswer::Choice { index },
                })
            }
            (InteractionRequestData::ToolInput { .. }, Answer::Deny) => None,
            _ => return,
        };
        let operation_id = match tasks::operation_id() {
            Ok(id) => id,
            Err(e) => {
                self.error(e);
                return;
            }
        };
        let turn_id = pending.turn_id.clone();
        let load_id = pending.load_id.clone();
        let interaction_id = pending.data.interaction_id.clone();
        pending.submitted = true;
        let epoch = session.epoch;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let result = match response {
                Some(answer) => client
                    .answer_interaction(
                        InteractionAnswerRequest {
                            operation_id,
                            turn_id: turn_id.clone(),
                            load_id: load_id.clone(),
                            interaction_id: interaction_id.clone(),
                            answer,
                        },
                        tasks::options(),
                    )
                    .await
                    .map(drop),
                None => client
                    .cancel_turn(
                        TurnCancelRequest {
                            operation_id,
                            turn_id: turn_id.clone(),
                            keep_inbox: true,
                        },
                        tasks::options(),
                    )
                    .await
                    .map(drop),
            }
            .map_err(|e| e.to_string());
            SessionOp::Answered {
                turn_id,
                load_id,
                interaction_id,
                result,
            }
        });
    }
    pub fn load_older(&mut self) {
        let Some(session) = self.resident.current_mut() else {
            return;
        };
        if session.loading_older {
            return;
        }
        let Some(mut window) = session.history.take() else {
            return;
        };
        if window.next_cursor.is_none() {
            session.history = Some(window);
            return;
        }
        session.loading_older = true;
        let epoch = session.epoch;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let result = window.older(&client, 60).await.map_err(|e| e.to_string());
            SessionOp::Older { window, result }
        });
    }
    fn recall_queued(&mut self) {
        if !self.require_control() {
            return;
        }
        if !self.composer.is_empty() {
            self.notice(
                NoticeLevel::Info,
                "Clear the draft before recalling queued input",
            );
            return;
        }
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let mut removed = Vec::new();
            let result = async {
                let inputs = client
                    .inputs(tasks::options())
                    .await
                    .map_err(|e| e.to_string())?;
                for input in inputs
                    .next_step
                    .into_iter()
                    .chain(inputs.next_turn)
                    .filter(|input| matches!(input.source, MessageSource::User))
                {
                    client
                        .remove_input(
                            InputRemoveRequest {
                                operation_id: tasks::operation_id()?,
                                message_id: input.input.message_id.clone(),
                                expected_controller_revision: None,
                            },
                            tasks::options(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    removed.push(input);
                }
                Ok::<_, String>(())
            }
            .await;
            SessionOp::Recalled {
                removed,
                error: result.err(),
            }
        });
    }
    pub(super) fn restore_to_composer(&mut self, text: &str) {
        if self.composer.is_empty() {
            self.composer.restore_text(text);
        } else {
            self.composer.insert(text);
        }
    }
    pub fn copy(&mut self, text: String) {
        self.copy_with_label(text, "Copied!");
    }
    pub fn copy_with_label(&mut self, text: String, label: &'static str) {
        self.spawn_msg(async move {
            match crate::clipboard::copy(&text).await {
                Ok(crate::clipboard::Copied::Native) => Msg::Flash(label.into()),
                Ok(crate::clipboard::Copied::Osc52) => Msg::Flash(label.into()),
                Err(error) => Msg::Notice(NoticeLevel::Error, error),
            }
        });
    }
    fn start_remote(&mut self, listen_override: Option<String>) {
        if self.remote.is_some() {
            if listen_override.is_some() {
                self.notice(
                    NoticeLevel::Info,
                    "Remote access is already running; press Esc or run /remote off first",
                );
            }
            self.overlay = super::Overlay::Remote {
                show_key: true,
                scroll: 0,
            };
            return;
        }
        if self.remote_starting {
            return;
        }
        let Some(host) = self.client.host() else {
            self.notice(
                NoticeLevel::Error,
                "Remote mode requires an embedded in-process Host",
            );
            return;
        };
        if !self.require_control() {
            return;
        }
        self.remote_starting = true;
        let config = self.remote_config.clone();
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.spawn_msg(async move {
            let workspace_roots_empty = client
                .workspace_roots(tasks::options())
                .await
                .map(|roots| roots.roots.is_empty())
                .unwrap_or(false);
            let result = crate::host::RemoteServer::start(
                host,
                &config,
                listen_override,
                tx,
                workspace_roots_empty,
            )
            .await
            .map(Box::new);
            Msg::RemoteStarted(result)
        });
    }
    pub(super) fn on_remote_started(
        &mut self,
        result: Result<Box<crate::host::RemoteServer>, String>,
    ) {
        self.remote_starting = false;
        match result {
            Ok(server) => {
                if let Err(error) = self.client.release_operator() {
                    server.stop();
                    self.notice(NoticeLevel::Error, error.to_string());
                    return;
                }
                self.remote = Some(*server);
                self.overlay = super::Overlay::Remote {
                    show_key: true,
                    scroll: 0,
                };
            }
            Err(error) => self.notice(NoticeLevel::Error, error),
        }
    }
    pub(super) fn on_remote_failed(&mut self, error: String) {
        if let Some(remote) = self.remote.take() {
            remote.stop();
        }
        let _ = self.client.reclaim_operator();
        if matches!(self.overlay, super::Overlay::Remote { .. }) {
            self.overlay = super::Overlay::None;
        }
        self.refresh_after_remote();
        self.notice(
            NoticeLevel::Error,
            format!("Remote server stopped: {error}"),
        );
    }
    fn stop_remote(&mut self) {
        let Some(remote) = self.remote.take() else {
            self.notice(NoticeLevel::Info, "Remote access is not running");
            return;
        };
        remote.stop();
        if let Err(error) = self.client.reclaim_operator() {
            self.notice(
                NoticeLevel::Error,
                format!("Cannot reclaim terminal control: {error}"),
            );
        }
        if matches!(self.overlay, super::Overlay::Remote { .. }) {
            self.overlay = super::Overlay::None;
        }
        self.refresh_after_remote();
        self.notice(
            NoticeLevel::Info,
            "Remote access stopped · terminal control restored",
        );
    }
    fn rotate_remote_key(&mut self) {
        let Some(remote) = &mut self.remote else {
            self.notice(NoticeLevel::Info, "Remote access is not running");
            return;
        };
        match remote.rotate_key() {
            Ok(()) => {
                self.flash = Some((
                    "New access key generated".into(),
                    tokio::time::Instant::now(),
                ));
            }
            Err(error) => self.notice(NoticeLevel::Error, error),
        }
    }
    fn refresh_after_remote(&mut self) {
        self.refresh_catalogs();
        if let Some(epoch) = self.resident.current().map(|session| session.epoch) {
            self.refresh_control_for(epoch);
            self.refresh_history(epoch);
            self.refresh_session_info_for(epoch);
        }
    }
    fn reload_host(&mut self) {
        if !self.require_control() || self.reloading {
            return;
        }
        self.reloading = true;
        let client = self.client.clone();
        let tx = self.tx.clone();
        let home = self.config_directory.clone();
        self.spawn_msg(async move {
            let result = async {
                let current = client
                    .config(tasks::options())
                    .await
                    .map_err(|e| e.to_string())?;
                let operation_id = tasks::operation_id()?;
                let mut receipt = client
                    .reload(
                        HostReloadRequest {
                            operation_id: operation_id.clone(),
                            expected_revision: current.config_revision,
                        },
                        tasks::options(),
                    )
                    .await
                    .map_err(|e| format!("Host reload {operation_id}: {e}"))?;
                let mut displayed = None;
                loop {
                    match &receipt.result {
                        HostReloadResult::Applied { .. }
                        | HostReloadResult::PartiallyApplied { .. }
                        | HostReloadResult::RestartRequired { .. } => break,
                        HostReloadResult::Failed { error, .. } => {
                            return Err(format!("Host reload {operation_id}: {error}"));
                        }
                        HostReloadResult::Preparing { .. } | HostReloadResult::Applying { .. } => {
                            if displayed.as_ref() != Some(&receipt.result) {
                                let stage =
                                    if matches!(receipt.result, HostReloadResult::Preparing { .. })
                                    {
                                        "preparing resources"
                                    } else {
                                        "applying configuration"
                                    };
                                let _ = tx
                                    .send(Msg::Notice(
                                        NoticeLevel::Info,
                                        format!("Host reload: {stage}"),
                                    ))
                                    .await;
                                displayed = Some(receipt.result.clone());
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    receipt = match client
                        .receipt(operation_id.clone(), tasks::options())
                        .await
                        .map_err(|e| {
                            format!("Host reload {operation_id} outcome is unconfirmed: {e}")
                        })? {
                        Receipt::HostReload(receipt) => receipt,
                        _ => {
                            return Err(format!(
                                "Host reload {operation_id} returned a different operation kind"
                            ));
                        }
                    };
                }
                let templates = if matches!(
                    receipt.result,
                    HostReloadResult::Applied { .. } | HostReloadResult::PartiallyApplied { .. }
                ) {
                    match tokio::task::spawn_blocking(move || super::templates::load(home.as_ref())).await {
                        Ok(templates) => Some(templates),
                        Err(error) => {
                            let _ = tx.send(Msg::Notice(
                                NoticeLevel::Error,
                                format!("Host applied configuration; local template reload failed: {error}"),
                            )).await;
                            None
                        }
                    }
                } else {
                    None
                };
                Ok(super::Reloaded {
                    result: receipt.result,
                    templates,
                })
            }
            .await;
            Msg::Reloaded(result.map(Box::new))
        });
    }
}
