//! Tool-data DTOs: the display summary used by cards, the authoritative
//! invocation/execution records, and the paged output reader.

use serde::{Deserialize, Serialize};

use super::event::CommandResult;
use super::stamp::unix_millis;

/// Full tool identity; a call is never addressed by name or path.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct ToolRef {
    pub session_id: String,
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
}

/// Whitelisted card summary shared by live events, `tool.read{display:true}`
/// and the history display projection.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ToolDisplay {
    pub detail: String,
    pub expanded_input: Option<String>,
    pub input_line_count: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolInvocation {
    pub tool_ref: ToolRef,
    pub name: String,
    pub subject: ToolSubject,
}

impl ToolInvocation {
    /// The short right-hand label of a card header.
    pub fn detail(&self) -> String {
        match &self.subject {
            ToolSubject::File { path } => path.clone(),
            ToolSubject::Command { script } => format!("$ {script}"),
            ToolSubject::Other => String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSubject {
    File {
        path: String,
    },
    Command {
        script: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolExecution {
    pub tool_ref: ToolRef,
    pub state: ToolExecutionState,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub output_line_count: Option<usize>,
    pub result_truncated: bool,
    pub command: Option<CommandResult>,
}

impl ToolExecution {
    /// How long the tool ran, when the Agent recorded both ends.
    pub fn duration(&self) -> Option<std::time::Duration> {
        let start = unix_millis(self.started_at.as_deref()?)?;
        let end = unix_millis(self.finished_at.as_deref()?)?;
        Some(std::time::Duration::from_millis(end.saturating_sub(start)))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionState {
    Requested,
    AwaitingPolicy,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    InputProvided,
    #[serde(other)]
    Unknown,
}

/// `tool.read` result; raw arguments come from `tool.output{stream:"input"}`.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolReadResult {
    pub display: Option<ToolDisplay>,
    pub invocation: Option<ToolInvocation>,
    pub execution: Option<ToolExecution>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolOutputPage {
    pub encoding: String,
    pub base_offset: u64,
    pub next_offset: u64,
    pub eof: bool,
    pub truncated: bool,
    pub availability: Option<String>,
    pub data: String,
}

impl ToolOutputPage {
    /// Decodes one page. Process streams are base64, text streams are UTF-8.
    pub fn bytes(&self) -> Vec<u8> {
        if self.encoding == "base64" {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(self.data.as_bytes())
                .unwrap_or_default()
        } else {
            self.data.clone().into_bytes()
        }
    }
}

/// Summary of one tool call as carried by the history display projection.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ToolSummary {
    pub tool_call_id: String,
    pub display: ToolDisplay,
    pub output_line_count: Option<usize>,
    pub output_truncated: bool,
    pub state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_detail_is_human_readable() {
        let invocation: ToolInvocation = serde_json::from_value(serde_json::json!({
            "tool_ref": {"session_id": "s", "loop_id": "l", "request_index": 0, "tool_call_id": "c"},
            "name": "bash",
            "subject": {"kind": "command", "script": "ls", "cwd": "/w"},
            "subject_truncated": false,
            "input": {"total_bytes": 2, "preview": "{}", "truncated": false, "encoding": "utf8_json"}
        }))
        .unwrap();
        assert_eq!(invocation.detail(), "$ ls");
    }

    #[test]
    fn process_pages_decode_from_base64() {
        let page: ToolOutputPage = serde_json::from_value(serde_json::json!({
            "stream": "stdout", "encoding": "base64", "base_offset": 0,
            "next_offset": 5, "observed_end": 5, "eof": true, "truncated": false,
            "availability": "available", "data": "aGVsbG8="
        }))
        .unwrap();
        assert_eq!(page.bytes(), b"hello");
        assert!(page.eof);
    }

    #[test]
    fn unknown_execution_state_degrades() {
        let execution: ToolExecution = serde_json::from_value(serde_json::json!({
            "tool_ref": {"session_id": "s", "loop_id": "l", "request_index": 0, "tool_call_id": "c"},
            "name": "bash", "state": "materializing", "result_truncated": false
        }))
        .unwrap();
        assert_eq!(execution.state, ToolExecutionState::Unknown);
    }
}
