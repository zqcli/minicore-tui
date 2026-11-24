//! A bounded view of actual delegated Session activity.

use minicore_host_core::{DelegationAssociation, SessionSnapshot, TurnStateData};
use minicore_host_protocol::{
    HostIncarnation, MessageId, SessionRef, ToolCallRef, TurnIdentity, TurnOutcome,
};

use super::Transcript;

pub const PREVIEW_BYTES: usize = 8 * 1024;
pub const PREVIEW_BLOCKS: usize = 24;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DelegationKey {
    pub incarnation: HostIncarnation,
    pub child: SessionRef,
    pub task_message_id: MessageId,
}

#[derive(Clone, Debug)]
pub struct AgentCard {
    pub key: DelegationKey,
    pub association: DelegationAssociation,
    pub agent_name: Option<String>,
    pub task: String,
    pub state: Option<SessionSnapshot>,
    pub turn: Option<(TurnIdentity, TurnStateData)>,
    pub transcript: Transcript,
    pub history_unavailable: bool,
    pub observation_error: Option<String>,
    pub truncated: bool,
    pub historical: bool,
    /// The settled result notification delivered to the parent, once it is
    /// in the parent's transcript; the card shows it instead of a second card.
    pub result: Option<String>,
    /// Later inputs sent into this child from the parent.
    pub sends: usize,
}

impl AgentCard {
    pub fn new(incarnation: HostIncarnation, association: DelegationAssociation) -> Self {
        Self {
            key: DelegationKey {
                incarnation,
                child: association.child.clone(),
                task_message_id: association.task_message_id.clone(),
            },
            association,
            agent_name: None,
            task: String::new(),
            state: None,
            turn: None,
            transcript: Transcript::default(),
            history_unavailable: false,
            observation_error: None,
            truncated: false,
            historical: false,
            result: None,
            sends: 0,
        }
    }

    pub fn origin(&self) -> &ToolCallRef {
        &self.association.origin
    }

    pub fn merge(&mut self, newer: &DelegationAssociation) {
        for id in &newer.accepted_message_ids {
            if !self.association.accepted_message_ids.contains(id) {
                self.association.accepted_message_ids.push(id.clone());
            }
        }
        for range in &newer.inputs {
            match self
                .association
                .inputs
                .iter_mut()
                .find(|known| known.turn == range.turn)
            {
                Some(known) => {
                    for id in &range.message_ids {
                        if !known.message_ids.contains(id) {
                            known.message_ids.push(id.clone());
                        }
                    }
                    for id in &range.delegated_message_ids {
                        if !known.delegated_message_ids.contains(id) {
                            known.delegated_message_ids.push(id.clone());
                        }
                    }
                }
                None => self.association.inputs.push(range.clone()),
            }
        }
        if self.association.completion.is_none() {
            self.association.completion.clone_from(&newer.completion);
        }
    }

    pub fn contains_turn(&self, source: &TurnIdentity) -> bool {
        source.session.host_incarnation == self.key.incarnation
            && source.session.agent_id == self.key.child.agent_id
            && source.session.session_id == self.key.child.session_id
            && self
                .association
                .inputs
                .iter()
                .any(|range| range.turn.turn_id == source.turn_id)
            && self.state.as_ref().is_none_or(|state| {
                state.data.active_turn_id.as_ref() != Some(&source.turn_id)
                    || source.load_id.is_none()
                    || state.data.load_id == source.load_id
            })
    }

    pub fn outcome(&self) -> Option<TurnOutcome> {
        let completion = self.association.completion.as_ref()?;
        if completion
            .turns
            .iter()
            .any(|turn| turn.outcome == TurnOutcome::Failed)
        {
            Some(TurnOutcome::Failed)
        } else if completion
            .turns
            .iter()
            .any(|turn| turn.outcome == TurnOutcome::Cancelled)
            || !completion.discarded_inputs.is_empty()
        {
            Some(TurnOutcome::Cancelled)
        } else if !completion.turns.is_empty() {
            Some(TurnOutcome::Completed)
        } else {
            None
        }
    }

    pub fn fault(&self) -> Option<&str> {
        self.association
            .completion
            .as_ref()
            .and_then(|completion| completion.turns.iter().find_map(|turn| turn.fault.as_ref()))
            .or_else(|| self.turn.as_ref().and_then(|(_, turn)| turn.fault.as_ref()))
            .map(|fault| fault.message())
    }

    pub fn trim(&mut self) {
        self.truncated |= self.transcript.retain_recent(PREVIEW_BLOCKS);
    }
}

pub fn head(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub fn append(target: &mut String, delta: &str) -> bool {
    let dropped = target.len() + delta.len() > PREVIEW_BYTES;
    if delta.len() >= PREVIEW_BYTES {
        target.clear();
        target.push_str(tail(delta, PREVIEW_BYTES));
    } else {
        let keep = PREVIEW_BYTES - delta.len();
        let retained = tail(target, keep).len();
        target.drain(..target.len() - retained);
        target.push_str(delta);
    }
    dropped
}
