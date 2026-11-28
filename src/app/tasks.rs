//! Native Host calls running outside the terminal reducer.

use minicore_host_core::*;
use minicore_host_protocol::*;
use minicore_host_transport::{ClientSubscription, HostClient, SessionClient};
use tokio::sync::mpsc;

use super::{App, Msg, NoticeLevel, Ready, UsageHistory};
use crate::history::HistoryWindow;

pub type Tx = mpsc::Sender<Msg>;
pub const MESSAGE_CAPACITY: usize = 256;
const PAGE: u64 = 60;

#[derive(Default)]
pub struct Tasks(std::cell::RefCell<Vec<tokio::task::AbortHandle>>);
impl Tasks {
    pub fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.0.borrow_mut();
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(work).abort_handle());
    }
}
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in self.0.get_mut() {
            task.abort();
        }
    }
}

pub struct Opened {
    pub info: SessionInfo,
    pub state: SessionSnapshot,
    pub history: HistoryWindow,
    pub inputs: InputList,
    pub context: SessionContextSnapshot,
    pub subscription: ClientSubscription,
}

pub enum OutputRead {
    Model(ModelTextSnapshot),
    Tool(ToolTextSnapshot),
}

pub enum SessionOp {
    Observed {
        generation: u64,
        event: Box<ObservationEvent>,
    },
    Baseline {
        generation: u64,
        baseline: SubscribeResult<StateEvent>,
    },
    Subscribe(Result<ClientSubscription, String>),
    ObservationEnded {
        generation: u64,
        error: Option<String>,
        resync: bool,
    },
    State(Result<SessionSnapshot, String>),
    Info(Result<SessionInfo, String>),
    Inputs(Result<InputList, String>),
    Context(Result<SessionContextSnapshot, String>),
    Workspace {
        revision: ConfigRevision,
        result: Result<WorkspaceSnapshot<minicore_host_workspace::StatusResult>, String>,
    },
    History(Result<HistoryWindow, String>),
    OlderUsage {
        usage: Vec<(u64, minicore_host_runtime::model::Usage)>,
        complete: bool,
    },
    Older {
        window: HistoryWindow,
        result: Result<Vec<HistoryRecordData>, String>,
    },
    Turn(TurnId, Result<TurnResultSnapshot, String>),
    Tool(ToolCallRef, Result<ToolCallSnapshot, String>),
    Output {
        source: OutputSource,
        channel: String,
        result: Result<OutputRead, String>,
    },
    Mutation {
        label: String,
        result: Result<(), String>,
    },
    Cancelled {
        turn_id: TurnId,
        result: Result<(), String>,
    },
    Answered {
        turn_id: TurnId,
        load_id: LoadId,
        interaction_id: InteractionId,
        result: Result<(), String>,
    },
    Recalled {
        removed: Vec<QueuedInputRecord>,
        error: Option<String>,
    },
    ChangeFiles(Result<Vec<minicore_host_workspace::ChangeRecord>, CallError>),
    DiffFile(
        super::diff::DiffQuery,
        Result<Vec<minicore_host_workspace::DiffResult>, CallError>,
    ),
}

pub fn operation_id() -> Result<OperationId, String> {
    OperationId::new().map_err(|error| error.to_string())
}
pub fn options() -> CallOptions {
    CallOptions::default()
}

pub(super) fn session_error(target: &SessionRef, error: impl std::fmt::Display) -> String {
    format!(
        "Agent: {}\nSession: {}\n\n{error}",
        target.agent_id, target.session_id
    )
}

pub async fn catalogs(client: &HostClient, selected: Option<AgentId>) -> Result<Ready, String> {
    let models = client.models(options()).await.map_err(|e| e.to_string())?;
    let agents = client.agents(options()).await.map_err(|e| e.to_string())?;
    let selected_agent = selected.or_else(|| agents.first().map(|agent| agent.id.clone()));
    Ok(Ready {
        models,
        agents,
        selected_agent,
    })
}

pub async fn bootstrap(client: HostClient, tx: Tx, selected: Option<AgentId>) {
    let result = async {
        let ready = catalogs(&client, selected.clone()).await?;
        if let Some(id) = selected.as_ref()
            && !ready.agents.iter().any(|agent| agent.id == *id)
        {
            return Err(format!("Agent {id} does not exist in this Host"));
        }
        Ok(ready)
    }
    .await;
    let _ = tx.send(Msg::Ready(result)).await;
}

pub async fn create_agent(
    client: &HostClient,
    name: String,
    config: AgentConfig,
    overrides: SessionOverrides,
) -> Result<SessionRef, String> {
    let receipt = client
        .create_agent(
            AgentCreate {
                operation_id: operation_id()?,
                name,
                mode: StorageMode::Persistent,
                config,
                default_session: Some(SessionOptions {
                    mode: None,
                    overrides: Some(overrides),
                }),
                initial_message: None,
            },
            options(),
        )
        .await
        .map_err(|e| e.to_string())?;
    if let CreateStep::Failed { error } = &receipt.result.steps.agent {
        return Err(error.to_string());
    }
    if let CreateStep::Failed { error } = &receipt.result.steps.session {
        return Err(format!(
            "Agent {} was created, but its default Session failed: {error}",
            receipt.agent_id.as_ref().map_or("unknown", AgentId::as_str)
        ));
    }
    Ok(SessionRef {
        agent_id: receipt
            .agent_id
            .ok_or_else(|| "Agent creation did not publish an Agent".to_owned())?,
        session_id: receipt
            .session_id
            .ok_or_else(|| "Agent creation did not publish its default Session".to_owned())?,
    })
}

pub async fn create_session(
    client: &HostClient,
    agent_id: AgentId,
    overrides: SessionOverrides,
) -> Result<SessionRef, String> {
    let receipt = client
        .create_session(
            SessionCreate {
                operation_id: operation_id()?,
                target: AgentRef {
                    host_id: client.initial_info().host_id.clone(),
                    agent_id,
                },
                mode: None,
                overrides: Some(overrides),
                initial_message: None,
            },
            options(),
        )
        .await
        .map_err(|e| e.to_string())?;
    if let CreateStep::Failed { error } = &receipt.result.steps.session {
        return Err(error.to_string());
    }
    let session_id = receipt
        .session_id
        .ok_or_else(|| "Session creation did not publish a Session".to_owned())?;
    Ok(SessionRef {
        agent_id: receipt.agent_id,
        session_id,
    })
}

pub async fn subscribe_session(
    client: &HostClient,
    target: SessionRef,
) -> Result<ClientSubscription, String> {
    native_session_subscription(client, target)
        .await
        .map_err(|error| error.to_string())
}

pub(super) async fn native_session_subscription(
    client: &HostClient,
    target: SessionRef,
) -> Result<ClientSubscription, CallError> {
    client
        .subscribe(
            SubscribeRequest {
                scope: SubscriptionScope::Session {
                    agent_id: target.agent_id,
                    session_id: target.session_id,
                },
                categories: [
                    EventCategory::State,
                    EventCategory::Output,
                    EventCategory::Completion,
                    EventCategory::Message,
                    EventCategory::Interaction,
                ]
                .into(),
            },
            options(),
        )
        .await
}

pub async fn open(
    client: HostClient,
    target: SessionRef,
    launch: super::LaunchSettings,
) -> Result<Box<Opened>, String> {
    let session = client.session(target.clone());
    let mut info = session.info(options()).await.map_err(|e| e.to_string())?;
    if launch.model.is_some() || launch.reasoning.is_some() {
        update(
            &session,
            info.config_revision,
            SessionUpdateChanges {
                title: NullablePatch::Unchanged,
                config: SessionConfigPatch {
                    model: launch.model,
                    reasoning: launch.reasoning,
                    ..Default::default()
                },
            },
        )
        .await?;
        info = session.info(options()).await.map_err(|e| e.to_string())?;
    }
    let subscription = subscribe_session(&client, target).await?;
    let state = session.state(options()).await.map_err(|e| e.to_string())?;
    let history = HistoryWindow::tail(&session, PAGE)
        .await
        .map_err(|e| e.to_string())?;
    let inputs = session.inputs(options()).await.map_err(|e| e.to_string())?;
    let context = session
        .context(options())
        .await
        .map_err(|e| e.to_string())?;
    Ok(Box::new(Opened {
        info,
        state,
        history,
        inputs,
        context,
        subscription,
    }))
}

impl App {
    pub(super) fn spawn_for(
        &self,
        epoch: u64,
        work: impl Future<Output = SessionOp> + Send + 'static,
    ) {
        let tx = self.tx.clone();
        self.resident
            .get(epoch)
            .expect("resident task target")
            .tasks
            .spawn(async move {
                let _ = tx.send(Msg::Session(epoch, Box::new(work.await))).await;
            });
    }
    pub(super) fn spawn_msg(&self, work: impl Future<Output = Msg> + Send + 'static) {
        let tx = self.tx.clone();
        self.tasks.spawn(async move {
            let _ = tx.send(work.await).await;
        });
    }
    pub(super) fn refresh_catalogs(&mut self) {
        if self.catalog_loading {
            self.catalog_due = true;
            return;
        }
        self.catalog_loading = true;
        let client = self.client.clone();
        let selected = self.selected_agent.clone();
        self.spawn_msg(async move { Msg::Catalogs(catalogs(&client, selected).await) });
    }
    pub(super) fn start_host_observation(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.tasks.spawn(async move {
            let mut established = false;
            loop {
                let result = client
                    .subscribe(
                        SubscribeRequest {
                            scope: SubscriptionScope::Host {
                                host_id: client.initial_info().host_id.clone(),
                            },
                            categories: [
                                EventCategory::State,
                                EventCategory::Output,
                                EventCategory::Completion,
                                EventCategory::Message,
                                EventCategory::Interaction,
                            ]
                            .into(),
                        },
                        options(),
                    )
                    .await;
                let mut subscription = match result {
                    Ok(value) => value,
                    Err(error) => {
                        let message = if established {
                            Msg::HostObservationError(error.to_string())
                        } else {
                            Msg::Failed(error.to_string())
                        };
                        let _ = tx.send(message).await;
                        return;
                    }
                };
                if tx
                    .send(Msg::HostBaseline(subscription.result().clone()))
                    .await
                    .is_err()
                {
                    return;
                }
                established = true;
                loop {
                    match subscription.recv().await {
                        Ok(Some(ObservationNotification::Event(push))) => {
                            if tx
                                .send(Msg::HostObserved(Box::new(push.event)))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(Some(ObservationNotification::ResyncRequired(required))) => {
                            let _ = tx
                                .send(Msg::HostObservationError(required.fault.to_string()))
                                .await;
                            break;
                        }
                        Ok(None) => {
                            let _ = tx
                                .send(Msg::HostObservationError("Host observation ended".into()))
                                .await;
                            return;
                        }
                        Err(error) => {
                            let _ = tx.send(Msg::HostObservationError(error.to_string())).await;
                            return;
                        }
                    }
                }
            }
        });
    }
    pub(super) fn pump_subscription(&mut self, epoch: u64, mut subscription: ClientSubscription) {
        let session = self.session_mut(epoch);
        if let Some(task) = session.subscription_task.take() {
            task.abort();
        }
        session.subscription_generation += 1;
        let generation = session.subscription_generation;
        let baseline = subscription.result().clone();
        self.on_op(
            epoch,
            SessionOp::Baseline {
                generation,
                baseline,
            },
        );
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            loop {
                let op = match subscription.recv().await {
                    Ok(Some(ObservationNotification::Event(push))) => SessionOp::Observed {
                        generation,
                        event: Box::new(push.event),
                    },
                    Ok(Some(ObservationNotification::ResyncRequired(required))) => {
                        let _ = tx
                            .send(Msg::Session(
                                epoch,
                                Box::new(SessionOp::ObservationEnded {
                                    generation,
                                    error: Some(required.fault.to_string()),
                                    resync: true,
                                }),
                            ))
                            .await;
                        return;
                    }
                    Ok(None) => SessionOp::ObservationEnded {
                        generation,
                        error: None,
                        resync: false,
                    },
                    Err(error) => SessionOp::ObservationEnded {
                        generation,
                        error: Some(error.to_string()),
                        resync: false,
                    },
                };
                let ended = matches!(op, SessionOp::ObservationEnded { .. });
                if tx.send(Msg::Session(epoch, Box::new(op))).await.is_err() || ended {
                    return;
                }
            }
        })
        .abort_handle();
        self.session_mut(epoch).subscription_task = Some(task);
    }
    pub(super) fn resubscribe(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        if session.subscription_loading {
            return;
        }
        session.subscription_loading = true;
        let target = session.address();
        let client = self.client.clone();
        self.spawn_for(epoch, async move {
            SessionOp::Subscribe(subscribe_session(&client, target).await)
        });
    }
    pub(super) fn refresh_control_for(&self, epoch: u64) {
        let client = self
            .resident
            .get(epoch)
            .expect("resident state")
            .client
            .clone();
        self.spawn_for(epoch, async move {
            SessionOp::State(client.state(options()).await.map_err(|e| e.to_string()))
        });
    }
    pub(super) fn refresh_inputs(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        if session.inputs_loading {
            session.inputs_due = true;
            return;
        }
        session.inputs_loading = true;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            SessionOp::Inputs(client.inputs(options()).await.map_err(|e| e.to_string()))
        });
    }
    pub(super) fn load_inspector_usage(&mut self, target: &SessionRef) {
        let Some(session) = self.resident.find_mut(target) else {
            return;
        };
        if session.usage_history != UsageHistory::Unread {
            return;
        }
        let Some(mut window) = session
            .history
            .as_ref()
            .filter(|window| window.has_more())
            .cloned()
        else {
            return;
        };
        session.usage_history = UsageHistory::Reading;
        let epoch = session.epoch;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let mut usage = Vec::new();
            while window.has_more() {
                let Ok(page) = window.older(&client, PAGE).await else {
                    return SessionOp::OlderUsage {
                        usage,
                        complete: false,
                    };
                };
                usage.extend(page.into_iter().filter_map(|record| {
                    let HistoryItemData::Assistant { response } = record.item else {
                        return None;
                    };
                    Some((record.position, response.usage))
                }));
            }
            SessionOp::OlderUsage {
                usage,
                complete: true,
            }
        });
    }

    pub fn refresh_context(&mut self) {
        if let Some(epoch) = self.resident.current().map(|s| s.epoch) {
            self.refresh_context_for(epoch);
        }
    }
    pub(super) fn refresh_context_for(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        if session.context_loading {
            session.context_due = true;
            return;
        }
        session.context_loading = true;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            SessionOp::Context(client.context(options()).await.map_err(|e| e.to_string()))
        });
    }
    pub(super) fn refresh_session_details_for(&mut self, epoch: u64) {
        self.refresh_session_info_for(epoch);
        self.refresh_context_for(epoch);
    }
    pub(super) fn refresh_session_info_for(&mut self, epoch: u64) {
        let client = self.session_mut(epoch).client.clone();
        self.spawn_for(epoch, async move {
            SessionOp::Info(client.info(options()).await.map_err(|e| e.to_string()))
        });
    }
    pub(super) fn refresh_workspace_for(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        if session.info.config.workspace.is_none() {
            session.branch = None;
            session.workspace_due = false;
            return;
        }
        if session.workspace_loading {
            session.workspace_due = true;
            return;
        }
        session.workspace_due = false;
        session.workspace_loading = true;
        let client = session.client.clone();
        let revision = session.info.config_revision;
        self.spawn_for(epoch, async move {
            SessionOp::Workspace {
                revision,
                result: client
                    .workspace_status(options())
                    .await
                    .map_err(|e| e.to_string()),
            }
        });
    }
    pub(super) fn refresh_history(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        if session.history_refreshing {
            session.history_due = true;
            return;
        }
        session.history_refreshing = true;
        let client = session.client.clone();
        let upper = session.history_upper_bound;
        self.spawn_for(epoch, async move {
            SessionOp::History(catch_up_history(&client, upper).await)
        });
    }
    pub(super) fn read_turn(&mut self, epoch: u64, turn_id: TurnId) {
        let session = self.session_mut(epoch);
        if !session.turn_reads.insert(turn_id.clone()) {
            return;
        }
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let result = client
                .turn_result(turn_id.clone(), options())
                .await
                .map_err(|e| e.to_string());
            SessionOp::Turn(turn_id, result)
        });
    }
    pub(super) fn read_tool(&mut self, epoch: u64, tool: ToolCallRef) {
        let session = self.session_mut(epoch);
        if !session.tool_reads.insert(tool.clone()) {
            return;
        }
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let result = client
                .toolcall(tool.clone(), options())
                .await
                .map_err(|e| e.to_string());
            SessionOp::Tool(tool, result)
        });
    }
    pub(super) fn read_output(&self, epoch: u64, source: OutputSource, request: TextReadRequest) {
        let client = self
            .resident
            .get(epoch)
            .expect("output target")
            .client
            .clone();
        self.spawn_for(epoch, async move {
            let channel = request.channel.clone();
            let result = output(&client, &source, request).await;
            SessionOp::Output {
                source,
                channel,
                result,
            }
        });
    }
}

pub(super) async fn output(
    client: &SessionClient,
    source: &OutputSource,
    request: TextReadRequest,
) -> Result<OutputRead, String> {
    match source {
        OutputSource::Model { source } => client
            .turn_output(
                source.turn.turn_id.clone(),
                source.response_index,
                request,
                options(),
            )
            .await
            .map(OutputRead::Model),
        OutputSource::Toolcall { source } => client
            .tool_output(
                ToolCallRef {
                    turn_id: source.turn.turn_id.clone(),
                    tool_call_id: source.tool_call_id.clone(),
                },
                request,
                options(),
            )
            .await
            .map(OutputRead::Tool),
    }
    .map_err(|error| error.to_string())
}

pub async fn export(client: SessionClient, path: Option<String>) -> Msg {
    let target = client.target().clone();
    let path = path.unwrap_or_else(|| {
        format!(
            "minicore-{}-{}.md",
            client.target().session_id,
            jiff::Timestamp::now().strftime("%F")
        )
    });
    let file = path.clone();
    let runtime = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let output = std::fs::File::create(&file).map_err(|error| format!("{file}: {error}"))?;
        let mut output = std::io::BufWriter::new(output);
        runtime
            .block_on(crate::history::export_markdown(&client, &mut output, 100))
            .map_err(|e| e.to_string())?;
        output.flush().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|result| result);
    match result {
        Ok(()) => Msg::Notice(NoticeLevel::Info, format!("Exported to {path}")),
        Err(error) => Msg::Notice(NoticeLevel::Error, session_error(&target, error)),
    }
}

pub async fn update(
    client: &SessionClient,
    revision: ConfigRevision,
    changes: SessionUpdateChanges,
) -> Result<(), String> {
    let receipt = client
        .update(
            SessionUpdateRequest {
                operation_id: operation_id()?,
                expected_config_revision: revision,
                changes,
            },
            options(),
        )
        .await
        .map_err(|e| e.to_string())?;
    if matches!(receipt.result, MetadataUpdateResult::Committed { .. }) {
        return Ok(());
    }
    loop {
        let state = client.state(options()).await.map_err(|e| e.to_string())?;
        if let Some(fault) = state.data.metadata_fault {
            return Err(fault.to_string());
        }
        if !state.data.metadata_saving {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn catch_up_history(
    client: &SessionClient,
    previous_upper: u64,
) -> Result<HistoryWindow, String> {
    let mut window = HistoryWindow::tail(client, PAGE)
        .await
        .map_err(|e| e.to_string())?;
    let mut records = std::mem::take(&mut window.items);
    while window.lower_bound > previous_upper && window.next_cursor.is_some() {
        records.extend(
            window
                .older(client, PAGE)
                .await
                .map_err(|e| e.to_string())?,
        );
    }
    records.retain(|record| record.position >= previous_upper);
    records.sort_by_key(|record| record.position);
    window.items = records;
    Ok(window)
}

#[derive(Clone, Copy)]
pub enum AgentWait {
    Lifecycle,
    Metadata,
    Deletion,
}

/// Agent progress has no Session observation event. Follow an accepted operation
/// until its native owner settles; dropping App aborts this task with the others.
pub async fn observe_agent(client: &HostClient, tx: &Tx, target: AgentRef, wait: AgentWait) -> Msg {
    if tx.send(Msg::RefreshCatalogs(Ok(()))).await.is_err() {
        return Msg::RefreshCatalogs(Err("Terminal closed during Agent observation".into()));
    }
    Msg::RefreshCatalogs(
        wait_agent(client, &target, wait)
            .await
            .map_err(|error| format!("Agent {}: {error}", target.agent_id)),
    )
}

async fn wait_agent(client: &HostClient, target: &AgentRef, wait: AgentWait) -> Result<(), String> {
    loop {
        let info = match client.agent(target.clone(), options()).await {
            Ok(agent) => agent.info,
            Err(CallError::Business(fault))
                if matches!(wait, AgentWait::Deletion)
                    && fault.kind() == Some(FaultKind::HostAgentNotFound) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        let pending = match wait {
            AgentWait::Lifecycle => matches!(
                info.lifecycle,
                AgentLifecycle::Creating | AgentLifecycle::Stopping
            ),
            AgentWait::Metadata => {
                if let Some(error) = info.metadata_fault {
                    return Err(error.to_string());
                }
                info.metadata_saving
            }
            AgentWait::Deletion => {
                if let Some(deletion) = info.deletion {
                    if let DataDeletion::Failed { error } = deletion.data {
                        return Err(format!("deletion failed: {error}"));
                    }
                    for session in deletion.sessions {
                        if let DataDeletion::Failed { error } = session.result.data {
                            return Err(format!(
                                "Session {} deletion failed: {error}",
                                session.session_id
                            ));
                        }
                    }
                }
                true
            }
        };
        if !pending {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}
