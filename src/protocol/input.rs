//! The session input queue the Agent owns: its entries and what the `input.*`
//! methods answer.

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Deserialize)]
pub struct InputPushResult {
    pub session_id: String,
    pub load: String,
    pub accepted: bool,
    /// The entry this push accepted; the Agent always states it.
    pub id: String,
    pub accepted_at: Option<String>,
    /// The input whose start failed after acceptance; `accepted` stays true.
    pub start_error: Option<InputStartError>,
}

/// A claimed input that could not start its turn, after it was accepted.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct InputStartError {
    pub input_id: String,
    pub kind: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputRemoveResult {
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputRecallResult {
    pub texts: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputResumeResult {
    pub start_error: Option<InputStartError>,
}

/// An input a running turn took, as `input_delivered` reports it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct DeliveredInput {
    pub id: String,
    pub text: String,
    pub accepted_at: Option<String>,
    /// The preparation candidate this input joined, which may never have been issued.
    pub request_index: u32,
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

    #[test]
    fn a_push_receipt_states_acceptance_and_an_older_heads_start_failure() {
        let push: InputPushResult = serde_json::from_value(json!({
            "session_id": "ses_1", "load": "load_1", "accepted": true,
            "id": "inp_new", "accepted_at": "2026-10-30T10:00:00Z",
            "start_error": {"input_id": "inp_older", "kind": "runtime_start_failed",
                            "message": "The queued turn could not start"}
        }))
        .unwrap();
        assert_eq!(push.id, "inp_new");
        let error = push.start_error.unwrap();
        assert_eq!(error.input_id, "inp_older");
        assert!(push.accepted);
    }

    #[test]
    fn a_push_receipt_requires_identity_and_explicit_acceptance() {
        let receipt = json!({
            "session_id": "ses_1", "load": "load_1", "accepted": true, "id": "inp_1"
        });
        for key in ["session_id", "load", "accepted", "id"] {
            let mut incomplete = receipt.clone();
            incomplete.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<InputPushResult>(incomplete).is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn a_resume_receipt_may_report_a_start_failure() {
        let resume: InputResumeResult = serde_json::from_value(json!({
            "start_error": {"input_id": "inp_1", "kind": "runtime_start_failed",
                            "message": "The queued turn could not start"}
        }))
        .unwrap();
        assert_eq!(resume.start_error.unwrap().input_id, "inp_1");
    }
}
