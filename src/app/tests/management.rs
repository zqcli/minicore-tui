use std::sync::Arc;

use crossterm::event::KeyCode;
use minicore_host_core::{Host, HostSettings, ResourceFuture, ResourceResolver};
use minicore_host_protocol::*;
use minicore_host_transport::HostClient;
use tokio::sync::{Notify, Semaphore, mpsc};

use super::harness::Harness;
use crate::app::{Action, App, Msg, SessionOp, tasks};
use crate::testing::real_bounded;

fn assert_info_eq(
    actual: &minicore_host_core::SessionInfo,
    expected: &minicore_host_core::SessionInfo,
) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

async fn rename_refreshes_metadata(navigate: bool) {
    let mut h = Harness::opened().await;
    let original = h.native.target.clone();
    let other = h.another_session().await;
    h.open(&other).await;
    let other_info = h.app.resident.current().unwrap().info.clone();
    h.open(&original).await;
    let old_info = h.app.resident.current().unwrap().info.clone();
    h.app.composer.set_text("/rename Updated title");
    h.press(KeyCode::Enter);
    // Hold the actual completion so navigation deterministically precedes it.
    let completion = real_bounded(async {
        loop {
            let message = h.messages.recv().await.unwrap();
            match message {
                Msg::SessionRenamed(_, Ok(())) | Msg::RefreshCatalogs(Ok(())) => break message,
                Msg::SessionRenamed(_, Err(error)) | Msg::RefreshCatalogs(Err(error)) => {
                    panic!("rename failed: {error}")
                }
                _ => h.app.on_msg(message),
            }
        }
    })
    .await;
    if navigate {
        h.open(&other).await;
    }
    assert_eq!(
        h.app.resident.find(&original).unwrap().info.title,
        old_info.title
    );
    h.app.on_msg(completion);
    // A receipt requests a read; it must not synthesize metadata or control state.
    assert_eq!(
        h.app.resident.find(&original).unwrap().info.title,
        old_info.title
    );
    h.until(|app| {
        app.sessions.iter().any(|session| {
            session.id == original.session_id && session.title.as_deref() == Some("Updated title")
        }) && app.resident.find(&original).unwrap().info.title.as_deref() == Some("Updated title")
    })
    .await;
    let native = h
        .native
        .client
        .session(original.clone())
        .info(tasks::options())
        .await
        .unwrap();
    assert_info_eq(&h.app.resident.find(&original).unwrap().info, &native);
    assert_info_eq(&h.app.resident.find(&other).unwrap().info, &other_info);
    let expected = if navigate { &other } else { &original };
    assert_eq!(&h.app.resident.current().unwrap().address(), expected);
    h.open(&original).await;
    h.app.composer.set_text("/session");
    h.press(KeyCode::Enter);
    assert!(matches!(h.app.overlay, crate::app::Overlay::SessionInfo));
    assert_eq!(
        h.app.resident.current().unwrap().info.title.as_deref(),
        Some("Updated title")
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn rename_refreshes_resident_metadata_and_reselection() {
    rename_refreshes_metadata(false).await;
}

#[tokio::test(start_paused = true)]
async fn rename_refreshes_original_metadata_after_navigation() {
    rename_refreshes_metadata(true).await;
}

#[tokio::test(start_paused = true)]
async fn failed_rename_preserves_resident_metadata() {
    let mut h = Harness::opened().await;
    let before = h.app.resident.current().unwrap().info.clone();
    h.app.on_action(Action::RenameSession {
        target: SessionRef {
            agent_id: before.agent_id.clone(),
            session_id: SessionId::new().unwrap(),
        },
        title: "Must not leak".into(),
    });
    h.until(|app| {
        app.notices
            .iter()
            .any(|notice| notice.level == crate::app::NoticeLevel::Error)
    })
    .await;
    assert_info_eq(&h.app.resident.current().unwrap().info, &before);
    assert_info_eq(
        &h.native
            .client
            .session(h.native.target.clone())
            .info(tasks::options())
            .await
            .unwrap(),
        &before,
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn reasoning_arguments_distinguish_picker_valid_and_invalid_values() {
    let mut h = Harness::opened().await;
    let before = h.app.resident.current().unwrap().info.clone();
    h.app.composer.set_text("/reasoning");
    h.press(KeyCode::Enter);
    assert!(matches!(
        h.app.overlay,
        crate::app::Overlay::Reasoning { .. }
    ));
    h.press(KeyCode::Esc);
    for (command, expected) in [
        (
            "/reasoning invalid-not-a-level",
            "auto|disabled|low|medium|high|xhigh|max|ultra",
        ),
        ("/theme invalid-not-a-theme", "dark|light"),
        ("/approval invalid-not-a-mode", "ask|auto|read_only"),
    ] {
        let theme = h.app.theme;
        h.app.composer.set_text(command);
        h.press(KeyCode::Enter);
        assert!(matches!(h.app.overlay, crate::app::Overlay::None));
        let notice = h
            .app
            .notices
            .back()
            .expect("invalid argument needs an error");
        assert_eq!(notice.level, crate::app::NoticeLevel::Error);
        assert!(notice.text.contains(expected), "{}", notice.text);
        assert_eq!(h.app.theme, theme);
        assert_info_eq(&h.app.resident.current().unwrap().info, &before);
        assert_info_eq(
            &h.native
                .client
                .session(h.native.target.clone())
                .info(tasks::options())
                .await
                .unwrap(),
            &before,
        );
    }
    h.app.composer.set_text("/reasoning auto");
    h.press(KeyCode::Enter);
    h.until(|app| app.resident.current().unwrap().info.config_revision > before.config_revision)
        .await;
    assert_eq!(h.app.reasoning(), Some(ReasoningPreference::Auto));
    assert_info_eq(
        &h.native
            .client
            .session(h.native.target.clone())
            .info(tasks::options())
            .await
            .unwrap(),
        &h.app.resident.current().unwrap().info,
    );
    h.finish().await;
}

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
    let message_id = MessageId::new().unwrap();
    client
        .session(target.clone())
        .send(
            MessageInput {
                message_id: message_id.clone(),
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

    // The preserved input cannot drain while this Agent is stopped.
    client
        .session(target)
        .remove_input(
            InputRemoveRequest {
                operation_id: OperationId::new().unwrap(),
                message_id,
                expected_controller_revision: None,
            },
            tasks::options(),
        )
        .await
        .unwrap();
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
    h.open(&desired).await;
    // Opening can consume older queued catalog reads. Establish the Agent
    // picker's actual selectable target after that navigation has completed.
    h.app.refresh_catalogs();
    h.until(|app| {
        app.agents.iter().any(|agent| {
            agent.id == other.agent_id
                && agent.default_session_id.as_ref() == Some(&other.session_id)
        }) && app
            .resident
            .current()
            .is_some_and(|session| session.address() == desired)
    })
    .await;

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
async fn catalog_mutations_share_one_reader_and_final_metadata_cannot_roll_back() {
    let mut h = Harness::opened().await;
    let target = h.another_session().await;
    h.open(&target).await;
    h.until(|app| !app.catalog_loading).await;
    h.app.refresh_catalogs();
    let old = real_bounded(async {
        loop {
            let message = h.messages.recv().await.unwrap();
            if matches!(message, Msg::Catalogs(_)) {
                break message;
            }
            h.app.on_msg(message);
        }
    })
    .await;
    assert!(h.app.catalog_loading);
    h.app.on_action(Action::RenameSession {
        target: target.clone(),
        title: "Final Session title".into(),
    });
    h.app.on_action(Action::SetDefaultSession);
    real_bounded(async {
        let mut requests = 0;
        while requests < 3 {
            let message = h.messages.recv().await.unwrap();
            match &message {
                Msg::RefreshCatalogs(Ok(())) | Msg::SessionRenamed(_, Ok(())) => requests += 1,
                Msg::RefreshCatalogs(Err(error)) | Msg::SessionRenamed(_, Err(error)) => {
                    panic!("mutation failed: {error}")
                }
                Msg::Catalogs(_) => panic!("mutation bypassed the active catalog reader"),
                _ => {}
            }
            h.app.on_msg(message);
        }
    })
    .await;
    let native = h
        .native
        .client
        .agent(
            agent_ref(&h.native.client, target.agent_id.clone()),
            tasks::options(),
        )
        .await
        .unwrap();
    assert_eq!(
        native.info.default_session_id.as_ref(),
        Some(&target.session_id)
    );
    assert_eq!(
        native
            .sessions
            .iter()
            .find(|row| row.id == target.session_id)
            .unwrap()
            .title
            .as_deref(),
        Some("Final Session title")
    );
    assert!(h.app.catalog_due);
    h.app.on_msg(old);
    assert!(h.app.catalog_loading);
    assert!(!h.app.catalog_due);

    real_bounded(async {
        let mut reads = 0;
        while h.app.catalog_loading {
            let message = h.messages.recv().await.unwrap();
            if let Msg::Catalogs(result) = &message {
                reads += 1;
                let catalog = result.as_ref().unwrap();
                assert_eq!(
                    catalog
                        .agents
                        .iter()
                        .find(|row| row.id == target.agent_id)
                        .unwrap()
                        .default_session_id
                        .as_ref(),
                    Some(&target.session_id)
                );
                assert_eq!(
                    catalog
                        .sessions
                        .iter()
                        .find(|row| row.id == target.session_id && row.agent_id == target.agent_id)
                        .unwrap()
                        .title
                        .as_deref(),
                    Some("Final Session title")
                );
            }
            h.app.on_msg(message);
        }
        assert_eq!(
            reads, 1,
            "requests during one read need only one trailing read"
        );
    })
    .await;
    assert_eq!(
        h.app
            .sessions
            .iter()
            .find(|row| row.id == target.session_id && row.agent_id == target.agent_id)
            .unwrap()
            .title
            .as_deref(),
        Some("Final Session title")
    );
    h.finish().await;
}

async fn take_workspace(h: &mut Harness) -> Msg {
    real_bounded(async {
        loop {
            let message = h.messages.recv().await.unwrap();
            if matches!(&message, Msg::Session(_, op) if matches!(op.as_ref(), SessionOp::Workspace { .. })) {
                return message;
            }
            h.app.on_msg(message);
        }
    })
    .await
}

#[tokio::test]
async fn workspace_reads_coalesce_and_follow_only_the_current_workspace_config() {
    let mut h = Harness::opened().await;
    let epoch = h.app.resident.current().unwrap().epoch;
    assert!(
        h.app
            .resident
            .current()
            .unwrap()
            .info
            .config
            .workspace
            .is_none()
    );
    h.app.refresh_workspace_for(epoch);
    assert!(!h.app.resident.current().unwrap().workspace_loading);

    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for (directory, branch) in [(&first, "first"), (&second, "second")] {
        let output = std::process::Command::new("git")
            .args(["init", "--quiet", "--initial-branch", branch])
            .arg(directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    h.app.update_session(SessionConfigPatch {
        workspace: NullablePatch::Set(WorkspaceRef {
            path: first.path().to_str().unwrap().into(),
        }),
        ..SessionConfigPatch::default()
    });
    let old = take_workspace(&mut h).await;
    assert!(h.app.resident.current().unwrap().workspace_loading);
    h.app.update_session(SessionConfigPatch {
        model: Some("fixture/during-read".parse().unwrap()),
        ..SessionConfigPatch::default()
    });
    h.until(|app| {
        app.resident.current().unwrap().info.config.model.as_str() == "fixture/during-read"
    })
    .await;
    assert!(!h.app.resident.current().unwrap().workspace_due);
    h.app.on_msg(old);
    assert!(h.app.resident.current().unwrap().branch.is_none());
    assert!(h.app.resident.current().unwrap().workspace_due);
    assert!(!h.app.resident.current().unwrap().workspace_loading);
    let refreshed = take_workspace(&mut h).await;
    h.app.on_msg(refreshed);
    assert_eq!(
        h.app.resident.current().unwrap().branch.as_deref(),
        Some("first")
    );

    h.app.refresh_workspace_for(epoch);
    let old = take_workspace(&mut h).await;
    for _ in 0..4 {
        h.app.refresh_workspace_for(epoch);
    }
    assert!(h.app.resident.current().unwrap().workspace_due);

    h.app.update_session(SessionConfigPatch {
        workspace: NullablePatch::Set(WorkspaceRef {
            path: second.path().to_str().unwrap().into(),
        }),
        ..SessionConfigPatch::default()
    });
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .info
            .config
            .workspace
            .as_ref()
            .unwrap()
            .path
            == second.path().to_str().unwrap()
    })
    .await;
    h.app.on_msg(old);
    assert!(h.app.resident.current().unwrap().branch.is_none());
    assert!(!h.app.resident.current().unwrap().workspace_loading);
    assert!(h.app.resident.current().unwrap().workspace_due);
    let current = take_workspace(&mut h).await;
    h.app.on_msg(current);
    assert_eq!(
        h.app.resident.current().unwrap().branch.as_deref(),
        Some("second")
    );
    assert!(!h.app.resident.current().unwrap().workspace_loading);

    h.app.update_session(SessionConfigPatch {
        model: Some("fixture/changed".parse().unwrap()),
        ..SessionConfigPatch::default()
    });
    h.until(|app| app.resident.current().unwrap().info.config.model.as_str() == "fixture/changed")
        .await;
    assert_eq!(
        h.app.resident.current().unwrap().branch.as_deref(),
        Some("second")
    );
    assert!(!h.app.resident.current().unwrap().workspace_loading);

    h.app.update_session(SessionConfigPatch {
        workspace: NullablePatch::Clear,
        ..SessionConfigPatch::default()
    });
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .info
            .config
            .workspace
            .is_none()
    })
    .await;
    h.app.refresh_workspace_for(epoch);
    assert!(h.app.resident.current().unwrap().branch.is_none());
    assert!(!h.app.resident.current().unwrap().workspace_loading);
    assert!(!h.app.resident.current().unwrap().workspace_due);
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
    tasks::bootstrap(client.clone(), app.tx.clone(), None).await;
    until(&mut app, &mut messages, |app| app.ready).await;
    app.on_action(Action::CreateAgent("No source".into()));
    assert!(
        app.notices
            .back()
            .unwrap()
            .text
            .contains("Select an Agent to copy")
    );
    assert!(
        tasks::catalogs(&client, None)
            .await
            .unwrap()
            .agents
            .is_empty()
    );

    let mut source_config = AgentConfig::default();
    source_config.role.instructions = "Source Role instructions".into();
    source_config.instructions = "Source Agent instructions".into();
    let source = tasks::create_agent(
        &client,
        "Source Agent".into(),
        source_config.clone(),
        SessionOverrides(SessionConfigPatch {
            instructions: Some("Session-only instructions".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    app.on_msg(Msg::Catalogs(
        tasks::catalogs(&client, Some(source.agent_id)).await,
    ));

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
    assert_eq!(native.info.config, source_config);
    assert!(native.info.configuration_source.is_none());
    let copied_session = client
        .session(first.clone())
        .info(tasks::options())
        .await
        .unwrap();
    assert_eq!(
        copied_session.config.instructions,
        "Source Agent instructions"
    );

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

#[tokio::test(start_paused = true)]
async fn partial_reload_result_projection_refreshes_native_state_and_preserves_the_session() {
    let mut h = Harness::opened().await;
    let target = h.app.resident.current().unwrap().address();
    h.app.reloading = true;
    h.app
        .on_msg(Msg::Reloaded(Ok(Box::new(crate::app::Reloaded {
            result: HostReloadResult::PartiallyApplied {
                config_revision: ConfigRevision::default(),
                agents: vec![AgentConfigurationResult::Failed {
                    key: "coding".into(),
                    agent_id: Some(target.agent_id.clone()),
                    error: Fault::new(
                        FaultKind::HostStorageUnavailable,
                        "explicit reload-result fixture",
                    ),
                }],
            },
            templates: Some(Vec::new()),
        }))));
    assert!(!h.app.reloading);
    assert!(h.app.catalog_loading);
    assert!(h.app.notices.iter().any(|notice| {
        notice.level == crate::app::NoticeLevel::Error
            && notice.text.contains("partial Agent results")
            && notice.text.contains("coding")
            && notice.text.contains("explicit reload-result fixture")
    }));
    h.until(|app| !app.catalog_loading).await;
    assert_eq!(h.app.resident.current().unwrap().address(), target);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn accepted_prompt_recall_uses_the_overridden_data_root() {
    let mut h = Harness::opened().await;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let data = temp.path().join("runtime");
    let directory = crate::config::ConfigDirectory::resolve(Some(&config), Some(&data)).unwrap();
    h.app.config_directory = Some(directory.clone());
    h.app
        .composer
        .set_text("Remember only after native acceptance");
    h.app.on_action(Action::Submit { follow: false });
    let call = h.next_call().await;
    call.finish();
    h.until(|app| app.local_inputs.is_empty()).await;
    h.app.local_files.finish().await;
    assert_eq!(
        crate::prompt_history::load(&directory.history_file())[0].text,
        "Remember only after native acceptance"
    );
    assert!(data.join("tui").join("history.jsonl").is_file());
    assert!(!config.join("history.jsonl").exists());
    assert!(!config.join("data").exists());
    h.finish().await;
}
