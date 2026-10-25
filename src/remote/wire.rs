//! Phone protocol v1 input and reply envelopes.

use serde::Deserialize;
use serde_json::{Value, json};

pub const VERSION: u64 = 1;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Handshake {
    Pair {
        v: u64,
        code: String,
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
            Self::Pair { v, .. } | Self::Hello { v, .. } => *v,
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
    Answer {
        interaction_id: String,
        choice: Choice,
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
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
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
