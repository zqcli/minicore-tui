//! A terminal projection of one explicitly addressed Host Session.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use minicore_host_core::{
    InteractionRequestData, PendingInteractionData, SessionContextSnapshot, SessionInfo,
    SessionSnapshot, TurnStateData,
};
use minicore_host_protocol::*;
use minicore_host_runtime::tools::ToolInputAnswerKind;
use minicore_host_transport::{OutputBuffer, SessionClient};

use super::{Scroll, composer::Composer, tasks::Tasks};
use crate::history::HistoryWindow;
use crate::transcript::{Block, Transcript};

pub const ARM_AFTER: Duration = Duration::from_millis(250);

pub struct Session {
    pub info: SessionInfo,
    pub epoch: u64,
    pub client: SessionClient,
    pub state: SessionSnapshot,
    pub transcript: Transcript,
    pub history: Option<HistoryWindow>,
    pub history_upper_bound: u64,
    pub loading_older: bool,
    pub history_refreshing: bool,
    pub history_due: bool,
    pub live: Option<LiveTurn>,
    pub queue: Option<InputList>,
    pub inputs_loading: bool,
    pub inputs_due: bool,
    pub context: Option<SessionContextSnapshot>,
    pub context_loading: bool,
    pub(super) context_due: bool,
    pub branch: Option<String>,
    pub interaction: Option<PendingInteraction>,
    pub interaction_editor: Composer,
    pub armed_at: tokio::time::Instant,
    pub draft: Option<String>,
    pub returned: Vec<String>,
    pub unseen: bool,
    pub scroll: Scroll,
    pub observation_error: Option<String>,
    pub subscription_generation: u64,
    pub(super) subscription_loading: bool,
    pub(super) subscription_task: Option<tokio::task::AbortHandle>,
    pub(super) tasks: Tasks,
    pub(super) outputs: Vec<Output>,
    pub(super) closed_outputs: Vec<OutputPosition>,
    pub(super) completed: HashSet<TurnId>,
    pub(super) turn_reads: HashSet<TurnId>,
    pub(super) tool_reads: HashSet<ToolCallRef>,
    pub(super) failed_turn_reads: HashMap<TurnId, String>,
    pub(super) failed_tool_reads: HashMap<ToolCallRef, String>,
    pub(super) turn_revisions: HashMap<TurnId, (Option<LoadId>, u64)>,
    pub(super) tool_revisions: HashMap<ToolCallRef, (Option<LoadId>, u64)>,
}

pub struct Output {
    pub buffer: OutputBuffer,
    pub channel: String,
    pub rendered: usize,
    pub reading: bool,
    pub failed: bool,
    pub fixed_final: bool,
}

pub struct LiveTurn {
    pub turn_id: TurnId,
    pub load_id: Option<LoadId>,
    pub state: TurnStateData,
    pub started: tokio::time::Instant,
    pub running_tool: Option<String>,
    pub start: usize,
    pub interrupting: bool,
}

pub struct PendingInteraction {
    pub data: PendingInteractionData,
    pub turn_id: TurnId,
    pub load_id: LoadId,
    pub selected: usize,
    pub submitted: bool,
}

impl PendingInteraction {
    pub fn request(&self) -> &InteractionRequestData {
        &self.data.request
    }
    pub fn tool_name(&self) -> &str {
        &self.data.name
    }
    pub fn tool_call_id(&self) -> &ToolCallId {
        &self.data.tool_call_id
    }
    pub fn is_text(&self) -> bool {
        matches!(&self.data.request, InteractionRequestData::ToolInput { request } if request.answer_kind == ToolInputAnswerKind::Text)
    }
    pub fn options(&self) -> Vec<&str> {
        match &self.data.request {
            InteractionRequestData::Approval { .. } => vec!["Allow once", "Deny"],
            InteractionRequestData::ToolInput { request } => request
                .choices
                .iter()
                .map(|choice| choice.as_str())
                .collect(),
        }
    }
    pub fn prompt(&self) -> &str {
        match &self.data.request {
            InteractionRequestData::Approval { prompt, .. } => prompt,
            InteractionRequestData::ToolInput { request } => request.prompt.as_str(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    Idle,
    Running,
    Approval,
    Compacting,
}

impl Session {
    pub fn new(
        info: SessionInfo,
        state: SessionSnapshot,
        epoch: u64,
        client: SessionClient,
    ) -> Self {
        let mut session = Self {
            info,
            state,
            epoch,
            client,
            transcript: Transcript::default(),
            history: None,
            history_upper_bound: 0,
            loading_older: false,
            history_refreshing: false,
            history_due: false,
            live: None,
            queue: None,
            inputs_loading: false,
            inputs_due: false,
            context: None,
            context_loading: false,
            context_due: false,
            branch: None,
            interaction: None,
            interaction_editor: Composer::new(),
            armed_at: tokio::time::Instant::now(),
            draft: None,
            returned: Vec::new(),
            unseen: false,
            scroll: Scroll::Follow,
            observation_error: None,
            subscription_generation: 0,
            subscription_loading: false,
            subscription_task: None,
            tasks: Tasks::default(),
            outputs: Vec::new(),
            closed_outputs: Vec::new(),
            completed: HashSet::new(),
            turn_reads: HashSet::new(),
            tool_reads: HashSet::new(),
            failed_turn_reads: HashMap::new(),
            failed_tool_reads: HashMap::new(),
            turn_revisions: HashMap::new(),
            tool_revisions: HashMap::new(),
        };
        session.sync_interaction();
        session
    }
    pub fn address(&self) -> SessionRef {
        SessionRef {
            agent_id: self.info.agent_id.clone(),
            session_id: self.info.id.clone(),
        }
    }
    pub fn paused(&self) -> bool {
        self.state.data.scheduling == Scheduling::Paused
    }
    pub fn is_busy(&self) -> bool {
        self.state.data.active_turn_id.is_some() || self.state.data.preparing
    }
    pub fn compacting(&self) -> bool {
        self.state
            .data
            .compaction
            .as_ref()
            .is_some_and(|activity| !activity.state.finished())
    }
    pub fn live_status(&self) -> LiveStatus {
        if self.interaction.is_some() {
            LiveStatus::Approval
        } else if self.compacting() {
            LiveStatus::Compacting
        } else if self.is_busy() {
            LiveStatus::Running
        } else {
            LiveStatus::Idle
        }
    }
    pub fn status_label(&self) -> Option<&'static str> {
        if self.interaction.is_some() {
            Some("Waiting for input")
        } else if self.compacting() {
            Some("Compacting")
        } else if self.state.data.saving {
            Some("Saving")
        } else if self.state.data.preparing {
            Some("Preparing")
        } else if self.is_busy() {
            Some(match self.live.as_ref().map(|live| live.state.state) {
                Some(TurnState::Cancelling) => "Cancelling",
                Some(TurnState::Finalizing) => "Finishing",
                _ => "Working",
            })
        } else {
            None
        }
    }
    pub fn hidden_items(&self) -> usize {
        self.history
            .as_ref()
            .filter(|history| history.next_cursor.is_some())
            .and_then(|_| {
                (0..self.transcript.len())
                    .filter_map(|index| self.transcript.history_source(index))
                    .map(|source| source.position)
                    .min()
            })
            .map_or(0, |position| position as usize)
    }
    pub fn auto_title(&self) -> Option<String> {
        self.transcript
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::User { text, .. } => Some(crate::wrap::truncate(
                    &crate::wrap::sanitize(
                        text.lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or(""),
                    ),
                    80,
                )),
                _ => None,
            })
    }
    pub fn identity_matches(&self, source: &SessionIdentity) -> bool {
        source.host_id == self.state.host_id
            && source.host_incarnation == self.state.host_incarnation
            && source.agent_id == self.info.agent_id
            && source.session_id == self.info.id
    }

    pub fn turn_matches(&self, source: &TurnIdentity) -> bool {
        self.identity_matches(&source.session)
            && (self.state.data.active_turn_id.as_ref() != Some(&source.turn_id)
                || self.state.data.load_id.is_none()
                || source.load_id.is_none()
                || self.state.data.load_id == source.load_id)
            && self
                .turn_revisions
                .get(&source.turn_id)
                .is_none_or(|(load, _)| {
                    load.is_none() || source.load_id.is_none() || load == &source.load_id
                })
    }

    pub fn apply_state(&mut self, snapshot: SessionSnapshot) -> bool {
        let previous_load = self.state.data.load_id.clone();
        if !merge_snapshot(&mut self.state, snapshot) {
            return false;
        }
        if self.state.data.load_id != previous_load {
            self.context = None;
        }
        if self.live.as_ref().is_some_and(|live| {
            Some(&live.turn_id) != self.state.data.active_turn_id.as_ref()
                || live.load_id != self.state.data.load_id
        }) {
            self.live = None;
        }
        if let Some(live) = &mut self.live {
            live.running_tool = self
                .state
                .data
                .owned_work
                .tools
                .iter()
                .find(|tool| tool.state == minicore_host_core::ToolCallState::Running)
                .map(|tool| tool.name.clone());
        }
        self.sync_interaction();
        true
    }

    pub fn sync_interaction(&mut self) {
        let state = &self.state.data;
        let Some((data, turn_id, load_id)) = state
            .pending_interactions
            .first()
            .zip(state.active_turn_id.as_ref())
            .zip(state.load_id.as_ref())
            .map(|((data, turn), load)| (data, turn, load))
        else {
            self.interaction = None;
            return;
        };
        if self.interaction.as_ref().is_some_and(|pending| {
            pending.data.interaction_id == data.interaction_id
                && pending.turn_id == *turn_id
                && pending.load_id == *load_id
        }) {
            return;
        }
        self.interaction = Some(PendingInteraction {
            data: data.clone(),
            turn_id: turn_id.clone(),
            load_id: load_id.clone(),
            selected: 0,
            submitted: false,
        });
        self.interaction_editor.clear();
        self.armed_at = tokio::time::Instant::now() + ARM_AFTER;
    }
}

pub(super) fn merge_snapshot(current: &mut SessionSnapshot, incoming: SessionSnapshot) -> bool {
    if incoming.host_id != current.host_id
        || incoming.host_incarnation != current.host_incarnation
        || incoming.agent_id != current.agent_id
        || incoming.session_id != current.session_id
    {
        return false;
    }
    let old = &current.data;
    let next = &incoming.data;
    let controller = next.controller_revision > old.controller_revision;
    let same_load = next.load_id == old.load_id;
    let runner =
        (same_load && next.runner_revision > old.runner_revision) || (!same_load && controller);
    if !controller && !runner {
        return false;
    }
    if controller {
        let mut next = incoming.data;
        if same_load && !runner {
            next.runner_revision = old.runner_revision;
            next.active_turn_id.clone_from(&old.active_turn_id);
            next.pending_interactions
                .clone_from(&old.pending_interactions);
            next.owned_work.model = old.owned_work.model;
            next.owned_work.tools.clone_from(&old.owned_work.tools);
            next.owned_work.tool_cleanup = old.owned_work.tool_cleanup;
            next.owned_work.scope_cleanup = old.owned_work.scope_cleanup;
            next.owned_work.result_handoff = old.owned_work.result_handoff;
        }
        current.data = next;
    } else {
        current.data.runner_revision = next.runner_revision;
        current.data.active_turn_id.clone_from(&next.active_turn_id);
        current
            .data
            .pending_interactions
            .clone_from(&next.pending_interactions);
        current.data.owned_work.model = next.owned_work.model;
        current
            .data
            .owned_work
            .tools
            .clone_from(&next.owned_work.tools);
        current.data.owned_work.tool_cleanup = next.owned_work.tool_cleanup;
        current.data.owned_work.scope_cleanup = next.owned_work.scope_cleanup;
        current.data.owned_work.result_handoff = next.owned_work.result_handoff;
    }
    true
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.subscription_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        controller: u64,
        runner: u64,
        load: &str,
        turn: &str,
        scheduling: Scheduling,
    ) -> SessionSnapshot {
        serde_json::from_value(serde_json::json!({
            "host_id":"00000000000000000001", "host_incarnation":"00000000000000000002",
            "agent_id":"00000000000000000003", "session_id":"00000000000000000004",
            "data": {
                "owned_work":{"model":true,"tools":[],"preparing":false,"history_save":false,"metadata_save":false,"compaction":null,"tool_cleanup":false,"scope_cleanup":false,"result_handoff":false},
                "end_reason":null,"mode":"memory","controller_revision":controller,"lifecycle":"open","scheduling":scheduling,"pause_reasons":[],
                "residency":"loaded","load_id":load,"runner_revision":runner,"active_turn_id":turn,"pending_interactions":[],"next_step":[],"next_turn":[],
                "preparing":false,"compaction":null,"saving":false,"metadata_saving":false,"metadata_fault":null,"block_reason":null,"last_result":null
            }
        })).unwrap()
    }

    #[test]
    fn controller_and_runner_versions_merge_independently_in_one_load() {
        let load = "00000000000000000005";
        let first = "00000000000000000006";
        let second = "00000000000000000007";
        let mut current = snapshot(10, 20, load, first, Scheduling::Auto);
        assert!(merge_snapshot(
            &mut current,
            snapshot(11, 19, load, second, Scheduling::Paused)
        ));
        assert_eq!(current.data.scheduling, Scheduling::Paused);
        assert_eq!(current.data.runner_revision, 20);
        assert_eq!(
            current.data.active_turn_id.as_ref().unwrap().as_str(),
            first
        );
        assert!(merge_snapshot(
            &mut current,
            snapshot(9, 21, load, second, Scheduling::Auto)
        ));
        assert_eq!(current.data.controller_revision, 11);
        assert_eq!(current.data.scheduling, Scheduling::Paused);
        assert_eq!(current.data.runner_revision, 21);
        assert_eq!(
            current.data.active_turn_id.as_ref().unwrap().as_str(),
            second
        );
    }

    #[test]
    fn a_retired_load_cannot_win_with_a_larger_runner_revision() {
        let old = "00000000000000000005";
        let new = "00000000000000000008";
        let turn = "00000000000000000006";
        let mut current = snapshot(10, 20, old, turn, Scheduling::Auto);
        assert!(merge_snapshot(
            &mut current,
            snapshot(11, 1, new, turn, Scheduling::Paused)
        ));
        assert!(!merge_snapshot(
            &mut current,
            snapshot(10, 999, old, turn, Scheduling::Auto)
        ));
        assert_eq!(current.data.load_id.as_ref().unwrap().as_str(), new);
        assert_eq!(current.data.runner_revision, 1);
        assert_eq!(current.data.scheduling, Scheduling::Paused);
    }
}
