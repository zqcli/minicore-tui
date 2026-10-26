//! Phone protocol v2 input and reply envelopes.

use serde::Deserialize;
use serde_json::{Value, json};

pub const VERSION: u64 = 2;

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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intent {
    Submit {
        text: String,
    },
    Queue {
        text: String,
    },
    Cancel,
    Compact,
    /// Takes back the queued follow-up at `index` of `state.queue` while it still reads `text`.
    Unqueue {
        index: usize,
        text: String,
    },
    Answer {
        interaction_id: String,
        choice: Choice,
        /// Steered into the same turn after a deny, so the agent hears why.
        message: Option<String>,
    },
    LoadOlder,
    OpenSession {
        session_id: String,
    },
    NewSession {
        workspace: Option<String>,
        profile: Option<String>,
    },
    SetModel {
        model: String,
    },
    SetReasoning {
        level: String,
    },
    RenameSession {
        session_id: String,
        title: String,
    },
    DeleteSession {
        session_id: String,
    },
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
#[serde(untagged)]
pub enum Choice {
    Name(String),
    Index(usize),
}

pub fn error(code: &str, message: &str) -> Value {
    json!({"t": "error", "code": code, "message": message})
}

pub fn ack(id: &str, result: Result<(), String>) -> Value {
    match result {
        Ok(()) => json!({"t": "ack", "id": id, "ok": true}),
        Err(error) => json!({"t": "ack", "id": id, "ok": false, "error": error}),
    }
}

pub fn bad_request(value: &Value) -> Value {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| id.len() <= 64)
        .unwrap_or("");
    let mut reply = ack(id, Err("bad_request".into()));
    if value
        .get("t")
        .and_then(Value::as_str)
        .is_some_and(|t| t.starts_with("fs_"))
    {
        reply["t"] = json!("fs");
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_rename_and_delete_intents_deserialize() {
        let rename: Intent = serde_json::from_value(
            json!({"t":"intent", "id":"c29", "kind":"rename_session", "session_id":"ses_1", "title":"Fix"}),
        )
        .unwrap();
        assert!(
            matches!(rename, Intent::RenameSession { session_id, title } if session_id == "ses_1" && title == "Fix")
        );
        let delete: Intent = serde_json::from_value(
            json!({"t":"intent", "id":"c30", "kind":"delete_session", "session_id":"ses_1"}),
        )
        .unwrap();
        assert!(matches!(delete, Intent::DeleteSession { session_id } if session_id == "ses_1"));
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
}
