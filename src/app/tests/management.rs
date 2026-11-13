use std::sync::Arc;

use crossterm::event::KeyCode;
use minicore_host_core::{Host, HostSettings, ResourceFuture, ResourceResolver};
use minicore_host_protocol::*;
use minicore_host_transport::HostClient;
use tokio::sync::{Notify, Semaphore, mpsc};

use super::harness::Harness;
use crate::app::{Action, App, Msg, tasks};
use crate::testing::real_bounded;

fn agent_ref(client: &HostClient, agent_id: AgentId) -> AgentRef {
    AgentRef {
        host_id: client.initial_info().host_id.clone(),
        agent_id,
    }
}

async fn memory_agent(client: &HostClient, name: &str, config: AgentConfig) -> SessionRef {
    let receipt = client
        .create_agent(
            AgentCreate {
                operation_id: OperationId::new().unwrap(),
                name: name.into(),
                mode: StorageMode::Memory,
                config,
                default_session: None,
                initial_message: None,
            },
            tasks::options(),
        )
        .await
        .unwrap();
    SessionRef {
        agent_id: receipt.agent_id.unwrap(),
        session_id: receipt.session_id.unwrap(),
    }
}

struct GatedPreparationDouble {
    entered: Notify,
    release: Semaphore,
}

impl ResourceResolver for GatedPreparationDouble {
    fn validate(&self, _: &SessionConfig) -> Result<(), Fault> {
        Ok(())
    }

    fn resolve<'a>(&'a self, _: &'a SessionConfig) -> ResourceFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            Err(Fault::new(
                FaultKind::SessionResourceMissing,
                "fixture preparation released without a model",
            ))
        })
    }
}

async fn until(app: &mut App, messages: &mut mpsc::Receiver<Msg>, ready: impl Fn(&App) -> bool) {
    real_bounded(async {
        while !ready(app) {
            app.on_msg(messages.recv().await.expect("App message stream ended"));
            assert!(app.fatal.is_none(), "{:?}", app.fatal);
        }
    })
    .await;
}

async fn take_agent_created(
    app: &mut App,
    messages: &mut mpsc::Receiver<Msg>,
) -> (Msg, SessionRef) {
    real_bounded(async {
        loop {
            let message = messages.recv().await.expect("Agent creation result");
            match &message {
                Msg::AgentCreated(_, Ok(target)) => {
                    let target = target.clone();
                    return (message, target);
                }
                Msg::AgentCreated(_, Err(error)) => panic!("Agent creation failed: {error}"),
                _ => app.on_msg(message),
            }
        }
    })
    .await
}

async fn finish_host(
    mut app: App,
    mut messages: mpsc::Receiver<Msg>,
    host: Arc<Host>,
    client: HostClient,
) {
    messages.close();
    app.local_files.finish().await;
    drop(app);
    let control = host.process_control();
    control
        .shutdown(HostShutdownRequest {
            operation_id: OperationId::new().unwrap(),
            strategy: HostShutdownStrategy::Graceful,
        })
        .unwrap();
    real_bounded(async {
        let mut state = control.snapshot().unwrap();
        while state.lifecycle != HostLifecycle::Stopped {
            state = control.changed(&state).await.unwrap();
        }
    })
    .await;
    client.close();
}

#[tokio::test(start_paused = true)]
async fn stopping_agent_stays_stopping_until_its_owned_preparation_settles() {
    let resources = Arc::new(GatedPreparationDouble {
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let host = Host::memory(resources.clone()).unwrap();
    let client = HostClient::in_process(host.clone()).unwrap();
    let target = memory_agent(&client, "Preparing Agent", AgentConfig::default()).await;
    let owner = agent_ref(&client, target.agent_id.clone());
    let (tx, mut messages) = mpsc::channel(tasks::MESSAGE_CAPACITY);
    let mut app = App::new(client.clone(), tx, std::env::temp_dir());
    app.on_msg(Msg::Ready(
        tasks::catalogs(&client, Some(target.agent_id.clone())).await,
    ));
    client
        .session(target.clone())
        .send(
            MessageInput {
                message_id: MessageId::new().unwrap(),
                mode: MessageMode::FollowUp,
                text: "wait for the selected resources".into(),
                in_reply_to: None,
            },
            tasks::options(),
        )
        .await
        .unwrap();
    real_bounded(resources.entered.notified()).await;

    app.on_action(Action::StopAgent);
    until(&mut app, &mut messages, |app| {
        app.agents
            .iter()
            .any(|agent| agent.id == target.agent_id && agent.lifecycle == AgentLifecycle::Stopping)
    })
    .await;
    let native = client.agent(owner.clone(), tasks::options()).await.unwrap();
    assert_eq!(native.info.lifecycle, AgentLifecycle::Stopping);
    assert!(native.sessions[0].owned_work.preparing);
    assert_eq!(native.sessions[0].next_turn, 1);
    assert!(
        app.agents
            .iter()
            .all(|agent| agent.lifecycle != AgentLifecycle::Stopped)
    );

    resources.release.add_permits(1);
    until(&mut app, &mut messages, |app| {
        app.agents
            .iter()
            .any(|agent| agent.id == target.agent_id && agent.lifecycle == AgentLifecycle::Stopped)
    })
    .await;
    let native = client.agent(owner, tasks::options()).await.unwrap();
    assert_eq!(native.info.lifecycle, AgentLifecycle::Stopped);
    assert!(!native.sessions[0].owned_work.preparing);
    assert_eq!(native.sessions[0].next_turn, 1);

    finish_host(app, messages, host, client).await;
}

#[tokio::test(start_paused = true)]
async fn default_session_change_keeps_the_original_agent_after_navigation() {
    let mut h = Harness::opened().await;
    let original = h.native.target.clone();
    let desired = h.another_session().await;
    let original_owner = agent_ref(&h.native.client, original.agent_id.clone());
    let config = h
        .native
        .client
        .agent(original_owner.clone(), tasks::options())
        .await
        .unwrap()
        .info
        .config;
    let other = memory_agent(&h.native.client, "Other Agent", config).await;
    let other_owner = agent_ref(&h.native.client, other.agent_id.clone());
    h.app.on_msg(Msg::Catalogs(
        tasks::catalogs(&h.native.client, Some(original.agent_id.clone())).await,
    ));
    h.open(&desired).await;

    h.app.on_action(Action::SetDefaultSession);
    h.app.on_action(Action::SelectAgent(other.agent_id.clone()));
    h.until(|app| {
        app.agents.iter().any(|agent| {
            agent.id == original.agent_id
                && agent.default_session_id.as_ref() == Some(&desired.session_id)
        }) && app
            .resident
            .current()
            .is_some_and(|session| session.address() == other)
    })
    .await;
    assert_eq!(
        h.native
            .client
            .agent(original_owner, tasks::options())
            .await
            .unwrap()
            .info
            .default_session_id,
        Some(desired.session_id)
    );
    assert_eq!(
        h.native
            .client
            .agent(other_owner, tasks::options())
            .await
            .unwrap()
            .info
            .default_session_id,
        Some(other.session_id)
    );
    assert_eq!(h.app.current_agent_id(), Some(&other.agent_id));
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn deletion_refuses_a_changed_child_scope_without_deleting_new_sessions() {
    let mut h = Harness::opened().await;
    let original = h.native.target.clone();
    h.app.confirm_agent_delete(original.agent_id.clone());
    let added = h.another_session().await;
    h.press(KeyCode::Char('y'));
    h.until(|app| {
        app.notices
            .iter()
            .any(|notice| notice.text.contains("exactly the retained direct Sessions"))
    })
    .await;
    let native = h
        .native
        .client
        .agent(
            agent_ref(&h.native.client, original.agent_id),
            tasks::options(),
        )
        .await
        .unwrap();
    assert_eq!(native.info.lifecycle, AgentLifecycle::Active);
    assert!(native.info.deletion.is_none());
    let mut actual: Vec<_> = native
        .sessions
        .into_iter()
        .map(|session| session.id)
        .collect();
    actual.sort();
    let mut expected = vec![original.session_id, added.session_id];
    expected.sort();
    assert_eq!(actual, expected);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn accepted_agent_deletion_reaches_actual_removal_without_manual_refresh() {
    let mut h = Harness::opened().await;
    let target = h.native.target.clone();
    h.app
        .composer
        .set_text("keep a model request active during deletion");
    h.app.on_action(Action::Submit { follow: false });
    let call = h.next_call().await;
    h.app.confirm_agent_delete(target.agent_id.clone());
    h.press(KeyCode::Char('y'));
    h.until(|app| !app.agents.iter().any(|agent| agent.id == target.agent_id))
        .await;
    assert!(matches!(
        h.native.client.agent(agent_ref(&h.native.client, target.agent_id), tasks::options()).await,
        Err(CallError::Business(fault)) if fault.kind() == Some(FaultKind::HostAgentNotFound)
    ));
    assert!(call.context.cancellation.is_cancelled());
    assert!(h.app.resident.current().is_none());
    drop(call);
    h.finish().await;
}

#[tokio::test]
async fn agent_creation_opens_its_actual_default_and_late_results_preserve_navigation() {
    let directory = tempfile::tempdir().unwrap();
    let resources = Arc::new(GatedPreparationDouble {
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let host = Host::open_persistent(
        directory.path(),
        HostSettings {
            name: "Agent creation fixture".into(),
        },
        resources,
    )
    .await
    .unwrap();
    let client = HostClient::in_process(host.clone()).unwrap();
    let (tx, mut messages) = mpsc::channel(tasks::MESSAGE_CAPACITY);
    let mut app = App::new(client.clone(), tx, directory.path().to_path_buf());
    app.on_msg(Msg::Ready(tasks::catalogs(&client, None).await));

    app.on_action(Action::CreateAgent("First Agent".into()));
    let (created, first) = take_agent_created(&mut app, &mut messages).await;
    app.on_msg(created);
    until(&mut app, &mut messages, |app| {
        !app.loading_session
            && app
                .resident
                .current()
                .is_some_and(|session| session.address() == first)
    })
    .await;
    let native = client
        .agent(agent_ref(&client, first.agent_id.clone()), tasks::options())
        .await
        .unwrap();
    assert_eq!(
        native.info.default_session_id.as_ref(),
        Some(&first.session_id)
    );
    assert_eq!(native.sessions.len(), 1);
    assert_eq!(native.sessions[0].id, first.session_id);

    app.on_action(Action::CreateAgent("Late Agent".into()));
    let (created, late) = take_agent_created(&mut app, &mut messages).await;
    app.open_session(first.clone());
    app.composer
        .set_text("keep this draft with the first Agent");
    app.on_msg(created);
    until(&mut app, &mut messages, |app| {
        app.agents.iter().any(|agent| agent.id == late.agent_id)
    })
    .await;
    assert_eq!(app.resident.current().unwrap().address(), first);
    assert_eq!(app.composer.text(), "keep this draft with the first Agent");
    assert!(!app.loading_session);
    assert!(app.opening_target.is_none());
    let native = client
        .agent(agent_ref(&client, late.agent_id.clone()), tasks::options())
        .await
        .unwrap();
    assert_eq!(native.info.default_session_id, Some(late.session_id));
    assert_eq!(native.sessions.len(), 1);
    finish_host(app, messages, host, client).await;
}
