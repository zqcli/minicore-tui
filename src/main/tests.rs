use super::*;
use minicore_host_core::SessionOwnedWork;
use minicore_host_protocol::{
    AgentLifecycle, ConfigRevision, HostId, ReasoningPreference, Residency, Scheduling,
    SessionLifecycle, StorageMode, Timestamp, WorkspaceRef,
};

fn session(agent_id: &AgentId, workspace: &Path, activity: i64) -> SessionSummary {
    SessionSummary {
        created_at: Timestamp::from_unix_milliseconds(0).unwrap(),
        last_activity_at: Timestamp::from_unix_milliseconds(activity).unwrap(),
        workspace: Some(WorkspaceRef {
            path: workspace.to_string_lossy().into(),
        }),
        model: "fixture/model".parse().unwrap(),
        reasoning: ReasoningPreference::Auto,
        id: SessionId::new().unwrap(),
        agent_id: agent_id.clone(),
        title: None,
        mode: StorageMode::Persistent,
        config_revision: ConfigRevision::default(),
        lifecycle: SessionLifecycle::Open,
        scheduling: Scheduling::Auto,
        pause_reasons: Vec::new(),
        residency: Residency::Unloaded,
        active_turn_id: None,
        next_step: 0,
        next_turn: 0,
        pending_interactions: 0,
        owned_work: Box::new(SessionOwnedWork {
            model: false,
            tools: Vec::new(),
            preparing: false,
            history_save: false,
            metadata_save: false,
            compaction: None,
            tool_cleanup: false,
            scope_cleanup: false,
            result_handoff: false,
        }),
        block_reason: None,
        metadata_fault: None,
        deletion: None,
        end_reason: None,
        delegated_by: None,
        delegation_root: None,
    }
}

fn agent(id: &AgentId, default_session_id: Option<SessionId>) -> AgentSummary {
    AgentSummary {
        id: id.clone(),
        host_id: HostId::new().unwrap(),
        name: "Fixture".into(),
        mode: StorageMode::Persistent,
        lifecycle: AgentLifecycle::Active,
        config_revision: ConfigRevision::default(),
        default_session_id,
        metadata_saving: false,
        metadata_fault: None,
        deletion: None,
        sessions: Vec::new(),
    }
}

#[test]
fn continue_uses_actual_activity_within_the_selected_agent_and_workspace() {
    let selected = AgentId::new().unwrap();
    let other = AgentId::new().unwrap();
    let workspace = Path::new("work");
    let sessions = vec![
        session(&selected, workspace, 200),
        session(&selected, workspace, 100),
        session(&other, workspace, 400),
        session(&selected, Path::new("elsewhere"), 500),
    ];
    assert_eq!(
        pick_recent(&sessions, Some(&selected), workspace)
            .unwrap()
            .id,
        sessions[0].id
    );
}

#[test]
fn explicit_session_keeps_its_real_owner_and_rejects_a_conflicting_agent() {
    let owner = AgentId::new().unwrap();
    let other = AgentId::new().unwrap();
    let workspace = Path::new("work");
    let sessions = vec![session(&owner, workspace, 10)];
    let result = startup_session(
        &sessions,
        &[],
        Some(&other),
        None,
        Some(&sessions[0].id),
        false,
        workspace,
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.agent_id, owner);
    assert!(
        startup_session(
            &sessions,
            &[],
            Some(&other),
            Some(&other),
            Some(&sessions[0].id),
            false,
            workspace
        )
        .is_err()
    );
}

#[test]
fn missing_saved_default_is_visible_and_never_replaced_with_another_session() {
    let selected = AgentId::new().unwrap();
    let workspace = Path::new("work");
    let sessions = vec![session(&selected, workspace, 10)];
    let agents = vec![agent(&selected, Some(SessionId::new().unwrap()))];
    assert!(
        startup_session(
            &sessions,
            &agents,
            Some(&selected),
            None,
            None,
            false,
            workspace
        )
        .is_err()
    );
    let agents = vec![agent(&selected, Some(sessions[0].id.clone()))];
    assert_eq!(
        startup_session(
            &sessions,
            &agents,
            Some(&selected),
            None,
            None,
            false,
            workspace
        )
        .unwrap()
        .unwrap()
        .session_id,
        sessions[0].id
    );
}

#[test]
fn starting_in_another_workspace_never_opens_the_default_sessions_project() {
    let selected = AgentId::new().unwrap();
    let default_dir = Path::new("project-a");
    let here = Path::new("project-b");
    let default = session(&selected, default_dir, 50);
    let agents = vec![agent(&selected, Some(default.id.clone()))];
    let open = |sessions: &[SessionSummary], workspace: &Path| {
        startup_session(
            sessions,
            &agents,
            Some(&selected),
            None,
            None,
            false,
            workspace,
        )
        .unwrap()
        .map(|target| target.session_id)
    };
    // Same directory: the default opens as before.
    assert_eq!(
        open(std::slice::from_ref(&default), default_dir),
        Some(default.id.clone())
    );
    // Another directory: nothing opens, so the first input creates a Session there.
    assert_eq!(open(std::slice::from_ref(&default), here), None);
    // Unless that directory already has recent work, which resumes instead.
    let local = session(&selected, here, 20);
    assert_eq!(open(&[default, local.clone()], here), Some(local.id));
}
