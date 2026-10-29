//! The session input queue the Agent owns: its entries and what the `input.*`
//! methods answer.

use serde::{Deserialize, Serialize};

use super::TurnRef;

/// How a pushed input reaches the session.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// As soon as possible: the running turn's next request, else a new turn.
    Steer,
    /// After the running turn has ended, as a new turn.
    FollowUp,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct QueueItem {
    pub id: String,
    pub mode: InputMode,
    pub text: String,
    pub accepted_at: Option<String>,
}

/// The entries in the order they run (steers first) and whether nothing starts by itself.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Queue {
    pub items: Vec<QueueItem>,
    pub paused: bool,
}

/// A turn the Agent started from the queue, with the entry that became its first input.
#[derive(Clone, Debug, Deserialize)]
pub struct StartedTurn {
    pub turn: TurnRef,
    pub input: QueueItem,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputPushResult {
    /// The turn this push started, whose input is an older entry when queue order requires.
    pub started: Option<StartedTurn>,
    pub queue: Queue,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputRemoveResult {
    pub text: String,
    pub queue: Queue,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputRecallResult {
    pub texts: Vec<String>,
    pub queue: Queue,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputResumeResult {
    pub started: Option<StartedTurn>,
    pub queue: Queue,
}

/// An input a running turn took, as `input_delivered` reports it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct DeliveredInput {
    pub id: String,
    pub text: String,
    pub accepted_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_queue_decodes_modes_order_and_the_paused_flag() {
        let queue: Queue = serde_json::from_value(json!({
            "items": [
                {"id": "i1", "mode": "steer", "text": "now", "accepted_at": "2026-10-28T10:00:00Z"},
                {"id": "i2", "mode": "follow_up", "text": "later"}
            ],
            "paused": true
        }))
        .unwrap();
        assert!(queue.paused);
        let modes: Vec<_> = queue.items.iter().map(|item| item.mode).collect();
        assert_eq!(modes, [InputMode::Steer, InputMode::FollowUp]);
        assert_eq!(queue.items[1].accepted_at, None);
    }

    #[test]
    fn modes_go_out_in_the_wire_names() {
        assert_eq!(serde_json::to_value(InputMode::Steer).unwrap(), "steer");
        assert_eq!(
            serde_json::to_value(InputMode::FollowUp).unwrap(),
            "follow_up"
        );
    }
}
