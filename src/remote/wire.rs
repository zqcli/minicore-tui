//! Phone protocol v3 intents and reply envelopes.
//!
//! Every modifying intent names the target it was built from: the view epoch,
//! the resident instance's `load`, and, where they apply, the session, loop,
//! input or interaction identity. A target that no longer exists is refused as
//! stale before anything runs, so an old phone view can never act on whatever
//! happens to be current.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::protocol::Acceptance;

pub const VERSION: u64 = 3;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Handshake {
    Login {
        v: u64,
        password: String,
        device: String,
    },
    Hello {
        v: u64,
        token: String,
    },
}

impl Handshake {
    pub fn version(&self) -> u64 {
        match self {
            Self::Login { v, .. } | Self::Hello { v, .. } => *v,
        }
    }
}

/// What one intent addresses, before its kind-specific fields.
///
/// `view_epoch` is the phone projection the client built the intent from. A
/// draft (`session_id` absent) is bound by the reducer to the session it
/// creates, so the intent never acts on whatever view came later.
#[derive(Debug, Deserialize)]
pub struct Target {
    pub view_epoch: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub load: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intent {
    Submit {
        text: String,
        #[serde(flatten)]
        target: Target,
    },
    Queue {
        text: String,
        #[serde(flatten)]
        target: Target,
    },
    /// Cancels the turn the phone saw, named by its loop, or the compaction it
    /// saw when it had no turn.
    Cancel {
        #[serde(default)]
        loop_id: Option<String>,
        #[serde(default)]
        operation_id: Option<String>,
        #[serde(flatten)]
        target: Target,
    },
    Compact {
        #[serde(flatten)]
        target: Target,
    },
    /// Removes a queued input by its identity, never by position or text.
    Unqueue {
        input_id: String,
        edit: bool,
        #[serde(flatten)]
        target: Target,
    },
    /// Answers the interaction the phone saw, named by loop and interaction.
    Answer {
        loop_id: String,
        interaction_id: String,
        choice: Choice,
        /// Steered into the same turn after a deny, so the agent hears why.
        message: Option<String>,
        #[serde(flatten)]
        target: Target,
    },
    LoadOlder {
        #[serde(flatten)]
        target: Target,
    },
    OpenSession {
        session_id: String,
        #[serde(flatten)]
        target: Target,
    },
    NewSession {
        workspace: Option<String>,
        profile: Option<String>,
        #[serde(flatten)]
        target: Target,
    },
    SetModel {
        model: String,
        #[serde(flatten)]
        target: Target,
    },
    SetReasoning {
        level: String,
        #[serde(flatten)]
        target: Target,
    },
    RenameSession {
        session_id: String,
        title: String,
        #[serde(flatten)]
        target: Target,
    },
    DeleteSession {
        session_id: String,
        #[serde(flatten)]
        target: Target,
    },
}

impl Intent {
    pub fn target(&self) -> &Target {
        match self {
            Self::Submit { target, .. }
            | Self::Queue { target, .. }
            | Self::Cancel { target, .. }
            | Self::Compact { target }
            | Self::Unqueue { target, .. }
            | Self::Answer { target, .. }
            | Self::LoadOlder { target }
            | Self::OpenSession { target, .. }
            | Self::NewSession { target, .. }
            | Self::SetModel { target, .. }
            | Self::SetReasoning { target, .. }
            | Self::RenameSession { target, .. }
            | Self::DeleteSession { target, .. } => target,
        }
    }

    /// Whether the intent acts on the session this phone is viewing.
    ///
    /// The others address another session (open, rename, delete) or only change
    /// this phone's own view, so their target names what they act on.
    pub fn acts_on_view(&self) -> bool {
        matches!(
            self,
            Self::Submit { .. }
                | Self::Queue { .. }
                | Self::Cancel { .. }
                | Self::Compact { .. }
                | Self::Unqueue { .. }
                | Self::Answer { .. }
                | Self::LoadOlder { .. }
                | Self::SetModel { .. }
                | Self::SetReasoning { .. }
        )
    }
}

/// A read relayed to the Agent for the requester's view, answered by `result`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    Context,
    Changes,
    Diff { path: String },
    ToolOutput { call_id: String, offset: u64 },
    Files { query: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t")]
pub enum FsRequest {
    #[serde(rename = "fs_roots")]
    Roots,
    #[serde(rename = "fs_list")]
    List { path: String, hidden: bool },
    #[serde(rename = "fs_mkdir")]
    Mkdir { parent: String, name: String },
}

#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Choice {
    Name(String),
    Index(usize),
    Text { text: String },
}

/// A refusal of an intent, with the acceptance class for input operations.
pub struct Refusal {
    pub error: String,
    pub acceptance: Option<Acceptance>,
}

impl Refusal {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            acceptance: None,
        }
    }

    /// A refusal of an input operation that the Agent never accepted.
    pub fn rejected(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            acceptance: Some(crate::protocol::Acceptance::Rejected),
        }
    }
}

impl From<String> for Refusal {
    fn from(error: String) -> Self {
        Self::new(error)
    }
}

impl From<&str> for Refusal {
    fn from(error: &str) -> Self {
        Self::new(error)
    }
}

pub fn error(code: &str, message: &str) -> Value {
    json!({"t": "error", "code": code, "message": message})
}

/// The answer to an intent; `result` is `Err` when it was refused.
pub fn ack(id: &str, result: Result<Option<Value>, Refusal>) -> Value {
    match result {
        Ok(Some(fields)) => {
            let mut reply = json!({"t": "ack", "id": id, "ok": true});
            for (key, value) in fields.as_object().into_iter().flatten() {
                reply[key] = value.clone();
            }
            reply
        }
        Ok(None) => json!({"t": "ack", "id": id, "ok": true}),
        Err(refusal) => {
            let mut reply = json!({"t": "ack", "id": id, "ok": false, "error": refusal.error});
            if let Some(acceptance) = refusal.acceptance {
                reply["acceptance"] = json!(match acceptance {
                    crate::protocol::Acceptance::Rejected => "rejected",
                    crate::protocol::Acceptance::Unknown => "unknown",
                });
            }
            reply
        }
    }
}

pub fn result(id: &str, result: Result<Value, String>) -> Value {
    match result {
        Ok(data) => json!({"t": "result", "id": id, "ok": true, "data": data}),
        Err(error) => json!({"t": "result", "id": id, "ok": false, "error": error}),
    }
}

pub fn bad_request(value: &Value) -> Value {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| id.len() <= 64)
        .unwrap_or("");
    let mut reply = ack(id, Err(Refusal::new("bad_request")));
    match value.get("t").and_then(Value::as_str) {
        Some(t) if t.starts_with("fs_") => reply["t"] = json!("fs"),
        Some("query") => reply["t"] = json!("result"),
        _ => {}
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Value {
        json!({"view_epoch": "view_7", "session_id": "ses_1", "load": "load_1"})
    }

    fn intent(extra: Value) -> Value {
        let mut value = json!({"t": "intent", "id": "c1"});
        for (key, part) in extra.as_object().unwrap() {
            value[key] = part.clone();
        }
        for (key, part) in target().as_object().unwrap() {
            value[key] = part.clone();
        }
        value
    }

    #[test]
    fn every_intent_carries_the_view_load_and_session_it_was_built_from() {
        let samples = [
            json!({"kind": "submit", "text": "go"}),
            json!({"kind": "queue", "text": "go"}),
            json!({"kind": "cancel", "loop_id": "lup_1"}),
            json!({"kind": "compact"}),
            json!({"kind": "unqueue", "input_id": "inp_1", "edit": true}),
            json!({"kind": "load_older"}),
            json!({"kind": "open_session", "session_id": "ses_1"}),
            json!({"kind": "new_session", "workspace": "/work"}),
            json!({"kind": "set_model", "model": "m"}),
            json!({"kind": "set_reasoning", "level": "high"}),
            json!({"kind": "rename_session", "session_id": "ses_1", "title": "T"}),
            json!({"kind": "delete_session", "session_id": "ses_1"}),
        ];
        for sample in samples {
            let intent: Intent = serde_json::from_value(intent(sample.clone())).unwrap();
            let target = intent.target();
            assert_eq!(target.view_epoch, "view_7", "{sample}");
            assert_eq!(target.load.as_deref(), Some("load_1"), "{sample}");
        }
    }

    #[test]
    fn an_intent_without_its_view_epoch_is_not_accepted() {
        for sample in [
            json!({"kind": "submit", "text": "go"}),
            json!({"kind": "unqueue", "input_id": "i", "edit": true}),
        ] {
            let mut value = intent(sample.clone());
            value.as_object_mut().unwrap().remove("view_epoch");
            assert!(Intent::deserialize(&value).is_err(), "{sample}");
        }
    }

    #[test]
    fn an_unqueue_names_an_input_and_has_no_position_or_text_fallback() {
        let stale = intent(json!({"kind": "unqueue", "index": 0, "text": "same", "edit": true}));
        assert!(Intent::deserialize(&stale).is_err());
        let unqueue: Intent = serde_json::from_value(intent(json!({
            "kind": "unqueue", "input_id": "inp_1", "edit": true
        })))
        .unwrap();
        assert!(
            matches!(&unqueue, Intent::Unqueue { input_id, edit: true, .. } if input_id == "inp_1")
        );
    }

    #[test]
    fn a_cancelled_interaction_answer_names_its_loop_and_interaction() {
        let answer: Intent = serde_json::from_value(intent(json!({
            "kind": "answer", "loop_id": "lup_3", "interaction_id": "int_2", "choice": "allow"
        })))
        .unwrap();
        assert!(matches!(
            answer,
            Intent::Answer { loop_id, interaction_id, .. } if loop_id == "lup_3" && interaction_id == "int_2"
        ));
        let incomplete =
            intent(json!({"kind": "answer", "interaction_id": "int_2", "choice": "allow"}));
        assert!(Intent::deserialize(&incomplete).is_err());
    }

    #[test]
    fn text_answers_are_objects_not_approval_names_or_choice_indices() {
        let answer: Intent = serde_json::from_value(intent(json!({
            "kind":"answer", "loop_id":"loop", "interaction_id":"question", "choice":{"text":"allow"}
        }))).unwrap();
        assert!(
            matches!(answer, Intent::Answer { choice: Choice::Text { text }, .. } if text == "allow")
        );
        for choice in [
            json!({"text":1}),
            json!({"text":"ok", "index":0}),
            json!({"text":"ok", "extra":true}),
        ] {
            assert!(Intent::deserialize(&intent(json!({"kind":"answer", "loop_id":"loop", "interaction_id":"question", "choice":choice}))).is_err());
        }
    }

    #[test]
    fn an_ack_states_the_successful_result_and_the_acceptance_of_a_refusal() {
        assert_eq!(
            ack("c1", Ok(None)),
            json!({"t": "ack", "id": "c1", "ok": true})
        );
        assert_eq!(
            ack(
                "c2",
                Ok(Some(json!({"accepted": true, "input_id": "inp_2"})))
            ),
            json!({"t": "ack", "id": "c2", "ok": true, "accepted": true, "input_id": "inp_2"})
        );
        assert_eq!(
            ack("c3", Err(Refusal::rejected("stale"))),
            json!({"t": "ack", "id": "c3", "ok": false, "error": "stale", "acceptance": "rejected"})
        );
        assert_eq!(
            ack("c4", Err(Refusal::new("busy"))),
            json!({"t": "ack", "id": "c4", "ok": false, "error": "busy"})
        );
    }

    #[test]
    fn bad_requests_never_echo_an_oversized_or_non_string_id() {
        for id in [
            json!("x".repeat(65)),
            json!("é".repeat(33)),
            json!(1),
            Value::Null,
        ] {
            assert_eq!(bad_request(&json!({"t":"fs_list", "id":id}))["id"], "");
        }
        let id = "x".repeat(64);
        assert_eq!(bad_request(&json!({"t":"intent", "id":id}))["id"], id);
    }

    #[test]
    fn a_bad_query_is_answered_as_a_failed_result() {
        for query in [
            json!({"t":"query", "id":"q1", "kind":"unknown"}),
            json!({"t":"query", "id":"q1", "kind":"diff"}),
            json!({"t":"query", "id":"q1", "kind":"tool_output", "call_id":"c"}),
        ] {
            assert!(Query::deserialize(&query).is_err(), "{query}");
            assert_eq!(
                bad_request(&query),
                json!({"t":"result", "id":"q1", "ok":false, "error":"bad_request"})
            );
        }
        let query =
            json!({"t":"query", "id":"q2", "kind":"tool_output", "call_id":"c", "offset":7});
        assert!(matches!(
            Query::deserialize(&query).unwrap(),
            Query::ToolOutput { call_id, offset: 7 } if call_id == "c"
        ));
    }
}
