//! Tool-data DTOs: the display summary used by cards, the authoritative
//! invocation/execution records, and the paged output reader.

use serde::{Deserialize, Serialize};

use super::event::CommandResult;

/// Full tool identity. A tool name or path never addresses a call: all four
/// fields are required on the wire.
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
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub expanded_input: Option<String>,
    #[serde(default)]
    pub input_line_count: Option<usize>,
    #[serde(default)]
    pub hidden_line_count: Option<usize>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub body_truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolInvocation {
    pub tool_ref: ToolRef,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub subject: ToolSubject,
    #[serde(default)]
    pub input: ToolInputPreview,
}

impl ToolInvocation {
    /// The short right-hand label of a card header.
    pub fn detail(&self) -> String {
        match &self.subject {
            ToolSubject::File { path } => path.clone(),
            ToolSubject::Command { script, .. } => format!("$ {script}"),
            ToolSubject::Other => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSubject {
    File {
        #[serde(default)]
        path: String,
    },
    Command {
        #[serde(default)]
        script: String,
        #[serde(default)]
        cwd: String,
    },
    #[default]
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ToolInputPreview {
    #[serde(default)]
    pub total_bytes: usize,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolExecution {
    pub tool_ref: ToolRef,
    #[serde(default)]
    pub name: String,
    pub state: ToolExecutionState,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub output_line_count: Option<usize>,
    #[serde(default)]
    pub output_availability: Option<String>,
    #[serde(default)]
    pub result_bytes: usize,
    #[serde(default)]
    pub result_truncated: bool,
    #[serde(default)]
    pub command: Option<CommandResult>,
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

/// `tool.read` result. Only the display summary and the structured records are
/// used by v0.4; raw arguments come from `tool.output{stream:"input"}`.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolReadResult {
    #[serde(default)]
    pub display: Option<ToolDisplay>,
    #[serde(default)]
    pub invocation: Option<ToolInvocation>,
    #[serde(default)]
    pub execution: Option<ToolExecution>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolOutputPage {
    pub stream: String,
    #[serde(default)]
    pub encoding: String,
    #[serde(default)]
    pub base_offset: u64,
    #[serde(default)]
    pub next_offset: u64,
    #[serde(default)]
    pub observed_end: u64,
    #[serde(default)]
    pub eof: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
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
    pub tool_ref: ToolRef,
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub name: String,
    pub display: ToolDisplay,
    #[serde(default)]
    pub output_line_count: Option<usize>,
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default)]
    pub state: Option<String>,
}

/// One assistant tool call from a history item.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolCallRecord {
    pub tool_call_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub call_index: u32,
    #[serde(default)]
    pub display: Option<ToolDisplay>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_detail_is_human_readable() {
        let command: ToolSubject = serde_json::from_value(
            serde_json::json!({"kind": "command", "script": "ls", "cwd": "/w"}),
        )
        .unwrap();
        assert_eq!(
            ToolInvocation {
                tool_ref: ToolRef {
                    session_id: "s".into(),
                    loop_id: "l".into(),
                    request_index: 0,
                    tool_call_id: "c".into(),
                },
                name: "bash".into(),
                subject: command,
                input: ToolInputPreview::default(),
            }
            .detail(),
            "$ ls"
        );
    }

    #[test]
    fn process_pages_decode_from_base64() {
        let page: ToolOutputPage = serde_json::from_value(serde_json::json!({
            "stream": "stdout", "encoding": "base64", "base_offset": 0,
            "next_offset": 5, "observed_end": 5, "eof": true, "data": "aGVsbG8="
        }))
        .unwrap();
        assert_eq!(page.bytes(), b"hello");
        assert!(page.eof);
    }

    #[test]
    fn unknown_execution_state_degrades() {
        let execution: ToolExecution = serde_json::from_value(serde_json::json!({
            "tool_ref": {"session_id": "s", "loop_id": "l", "request_index": 0, "tool_call_id": "c"},
            "state": "materializing"
        }))
        .unwrap();
        assert_eq!(execution.state, ToolExecutionState::Unknown);
    }
}
