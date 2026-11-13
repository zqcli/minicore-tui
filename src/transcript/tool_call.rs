//! Real tool invocations and their bounded presentation output.

use std::time::{Duration, Instant};

use minicore_host_core::{
    DelegationAssociation, ToolCallData, ToolCallState, ToolCallStateData, ToolInvocationData,
    ToolResultData,
};
use minicore_host_local_tools::{CommandResult, OutputData};
use minicore_host_protocol::{ToolCallId, ToolCallRef, TurnId};
use minicore_host_runtime::tools::ToolResultOutcome;
use serde_json::Value;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ToolKey {
    Call(ToolCallRef),
    History {
        position: u64,
        tool_call_id: ToolCallId,
    },
}

impl ToolKey {
    pub fn turn_id(&self) -> Option<&TurnId> {
        match self {
            Self::Call(key) => Some(&key.turn_id),
            Self::History { .. } => None,
        }
    }

    pub fn tool_call_id(&self) -> &ToolCallId {
        match self {
            Self::Call(key) => &key.tool_call_id,
            Self::History { tool_call_id, .. } => tool_call_id,
        }
    }

    pub fn call_ref(&self) -> Option<&ToolCallRef> {
        match self {
            Self::Call(key) => Some(key),
            Self::History { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub key: ToolKey,
    pub response_index: Option<u64>,
    pub name: String,
    pub arguments: Option<Value>,
    pub detail: String,
    pub expanded_input: Option<String>,
    pub input_lines: Option<usize>,
    pub state: ToolState,
    pub delegations: Vec<DelegationAssociation>,
    pub progress: Option<String>,
    /// A native local-tool operation error, distinct from its execution outcome.
    pub result_error: Option<String>,
    pub started_at: Option<Instant>,
    pub duration: Option<Duration>,
    pub exit_code: Option<i64>,
    pub output: Vec<String>,
    pub output_dropped: usize,
    pub output_lines: Option<usize>,
    pub truncated: bool,
    pub expanded: Option<bool>,
    output_from_result: bool,
    arguments_display: Option<(String, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

impl ToolState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::Cancelled
        )
    }

    pub fn from_outcome(outcome: ToolResultOutcome) -> Self {
        match outcome {
            ToolResultOutcome::Success | ToolResultOutcome::InputProvided => Self::Succeeded,
            ToolResultOutcome::Failed => Self::Failed,
            ToolResultOutcome::Denied => Self::Denied,
            ToolResultOutcome::Cancelled => Self::Cancelled,
        }
    }
}

impl ToolCall {
    pub fn new(key: ToolCallRef, name: impl Into<String>) -> Self {
        Self::with_key(ToolKey::Call(key), name)
    }

    pub fn with_key(key: ToolKey, name: impl Into<String>) -> Self {
        Self {
            key,
            response_index: None,
            name: name.into(),
            arguments: None,
            detail: String::new(),
            expanded_input: None,
            input_lines: None,
            state: ToolState::Pending,
            delegations: Vec::new(),
            progress: None,
            result_error: None,
            started_at: None,
            duration: None,
            exit_code: None,
            output: Vec::new(),
            output_dropped: 0,
            output_lines: None,
            truncated: false,
            expanded: None,
            output_from_result: false,
            arguments_display: None,
        }
    }

    pub fn set_state(&mut self, state: ToolState) {
        if !self.state.is_terminal() || state.is_terminal() {
            self.state = state;
        }
    }

    /// Only complete invocation arguments enter the card.
    pub fn set_arguments(&mut self, arguments: &Value) {
        self.arguments = Some(arguments.clone());
        self.arguments_display = Some(argument_display(arguments));
        let string = |key: &str| arguments.get(key).and_then(Value::as_str);
        self.detail = string("path")
            .or_else(|| string("command"))
            .or_else(|| string("query"))
            .or_else(|| string("pattern"))
            .or_else(|| string("description"))
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .to_owned();
        let body = match self.name.as_str() {
            "bash" => string("command").map(str::to_owned),
            "write" => string("content").map(str::to_owned),
            "apply_patch" => string("patch").map(str::to_owned),
            "edit" => string("old_text")
                .zip(string("new_text"))
                .map(|(old, new)| {
                    old.lines()
                        .map(|line| format!("-{line}\n"))
                        .chain(new.lines().map(|line| format!("+{line}\n")))
                        .collect()
                }),
            _ => None,
        };
        self.input_lines = body.as_deref().map(|body| body.lines().count());
        self.expanded_input = body;
    }

    /// A bounded, already formatted view of the complete native arguments.
    pub fn argument_display(&self) -> Option<(&str, bool)> {
        self.arguments_display
            .as_ref()
            .map(|(text, truncated)| (text.as_str(), *truncated))
    }

    pub fn apply_invocation(&mut self, invocation: &ToolInvocationData) {
        self.name.clone_from(&invocation.name);
        self.set_arguments(&invocation.arguments);
        self.set_state(ToolState::Running);
        if !self.state.is_terminal() && self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
    }

    pub fn apply_state(&mut self, state: &ToolCallStateData) {
        self.name.clone_from(&state.name);
        self.response_index = Some(state.response_index);
        self.delegations.clone_from(&state.delegations);
        self.progress = state
            .progress
            .as_ref()
            .and_then(|progress| progress.message.clone());
        if let Some(outcome) = state.outcome {
            self.set_state(ToolState::from_outcome(outcome));
        } else {
            match state.state {
                ToolCallState::Pending => self.set_state(ToolState::Pending),
                ToolCallState::Running => self.set_state(ToolState::Running),
                ToolCallState::Finished => {}
            }
        }
    }

    pub fn apply_read(&mut self, read: &ToolCallData) {
        self.apply_state(&read.state);
        self.set_arguments(&read.arguments);
        if let Some(result) = &read.result {
            self.apply_result(result);
        }
    }

    pub fn apply_result(&mut self, result: &ToolResultData) {
        self.set_state(ToolState::from_outcome(result.outcome));
        self.duration = self
            .duration
            .or_else(|| self.started_at.map(|start| start.elapsed()));
        let content = result.output.content().as_str();
        let command = (self.name == "bash")
            .then(|| serde_json::from_str::<CommandResult>(content).ok())
            .flatten();
        if let Some(command) = &command {
            self.exit_code = command.exit_code.map(i64::from).or(self.exit_code);
        }
        let parsed = command
            .is_none()
            .then(|| serde_json::from_str::<Value>(content).ok())
            .flatten();
        if matches!(
            self.name.as_str(),
            "read" | "write" | "edit" | "apply_patch"
        ) {
            self.result_error = parsed
                .as_ref()
                .filter(|value| value["status"] == "failed")
                .and_then(|value| value["error"]["message"].as_str())
                .map(str::to_owned);
        }
        if self.output.is_empty() {
            match command
                .as_ref()
                .map(|command| (&command.stdout.output, &command.stderr.output))
            {
                Some((OutputData::Utf8(stdout), OutputData::Utf8(stderr))) => {
                    self.output_lines = Some(stdout.lines().count() + stderr.lines().count());
                    self.push_output(stdout);
                    self.push_output(stderr);
                }
                _ => {
                    let read_text = parsed
                        .as_ref()
                        .filter(|value| {
                            self.name == "read"
                                && value["status"] == "ok"
                                && value["kind"] == "file"
                        })
                        .and_then(|value| value["text"].as_str());
                    let formatted = parsed.as_ref().map(|value| {
                        serde_json::to_string_pretty(value).expect("JSON values serialize")
                    });
                    let body = read_text.or(formatted.as_deref()).unwrap_or(content);
                    self.output_lines = Some(body.lines().count());
                    if self.name == "bash" {
                        self.push_output(body);
                    } else {
                        self.push_head(body);
                    }
                }
            }
            self.output_from_result = true;
        }
        self.settle();
    }

    pub fn keep_live_facts(&mut self, live: &Self) {
        self.expanded = live.expanded;
        self.started_at = self.started_at.or(live.started_at);
        self.duration = self.duration.or(live.duration);
        self.exit_code = self.exit_code.or(live.exit_code);
        if self.result_error.is_none() {
            self.result_error.clone_from(&live.result_error);
        }
        if self.arguments.is_none()
            && let Some(arguments) = &live.arguments
        {
            self.set_arguments(arguments);
        }
        if self.delegations.is_empty() {
            self.delegations.clone_from(&live.delegations);
        }
        if !live.output.is_empty() && !live.output_from_result {
            self.output.clone_from(&live.output);
            self.output_dropped = live.output_dropped;
            self.truncated |= live.truncated;
            self.output_from_result = false;
        }
        if !self.state.is_terminal() {
            self.state = live.state;
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == ToolState::Running
    }

    pub fn is_expanded(&self, all_expanded: bool) -> bool {
        self.expanded.unwrap_or(all_expanded)
    }

    pub fn toggle_expanded(&mut self, all_expanded: bool) {
        let expanded = !self.is_expanded(all_expanded);
        self.expanded = (expanded != all_expanded).then_some(expanded);
    }

    pub fn header_detail(&self) -> String {
        if self.name == "bash"
            && let Some(command) = self
                .expanded_input
                .as_deref()
                .and_then(|input| input.lines().find(|line| !line.trim().is_empty()))
        {
            return format!("$ {}", command.trim());
        }
        if !self.detail.is_empty() {
            return self.detail.clone();
        }
        self.patch_paths().join(", ")
    }

    fn patch_paths(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        for line in self.expanded_input.as_deref().unwrap_or("").lines() {
            let path = [
                "*** Update File: ",
                "*** Add File: ",
                "*** Delete File: ",
                "*** Move to: ",
                "+++ b/",
                "--- a/",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(str::trim);
            if let Some(path) = path
                && !path.is_empty()
                && !paths.contains(&path)
            {
                paths.push(path);
            }
        }
        paths
    }

    pub fn line_changes(&self) -> Option<(usize, usize)> {
        let input = self.expanded_input.as_deref()?;
        match self.name.as_str() {
            "write" => Some((input.lines().count(), 0)),
            "edit" | "apply_patch" => {
                let mut counts = (0, 0);
                for line in input.lines() {
                    if line.starts_with("+++ ") || line.starts_with("--- ") {
                        continue;
                    }
                    match line.as_bytes().first() {
                        Some(b'+') => counts.0 += 1,
                        Some(b'-') => counts.1 += 1,
                        _ => {}
                    }
                }
                Some(counts)
            }
            _ => None,
        }
    }

    pub fn hidden_input_lines(&self) -> usize {
        0
    }

    pub fn output_content(&self) -> Vec<&str> {
        let mut lines: Vec<_> = self.output.iter().map(String::as_str).collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        lines
    }

    pub fn settle(&mut self) {
        let content = self.output_content().len();
        if content <= EXPANDED_ROWS {
            return;
        }
        if self.name == "bash" {
            let drop = self.output.len().saturating_sub(EXPANDED_ROWS + 1);
            self.output.drain(..drop);
            self.output_dropped += drop;
        } else {
            self.output_lines = Some(self.output_lines.unwrap_or(0).max(content));
            self.output.truncate(EXPANDED_ROWS);
        }
        self.output.shrink_to_fit();
    }

    fn push_head(&mut self, content: &str) {
        let mut bytes = 0;
        for line in content.lines().take(OUTPUT_TAIL_LINES) {
            let remaining = OUTPUT_TAIL_BYTES.saturating_sub(bytes + 1);
            if remaining < '…'.len_utf8() {
                break;
            }
            let mut kept = String::new();
            self.truncated |= append_capped_to(&mut kept, line, remaining);
            bytes += kept.len() + 1;
            self.output.push(kept);
        }
        self.truncated |= content.lines().count() > self.output.len();
    }

    /// The caller supplies only verified contiguous UTF-8 from the SDK OutputBuffer.
    pub fn push_output(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.output_from_result {
            self.output.clear();
            self.output_dropped = 0;
            self.truncated = false;
            self.output_from_result = false;
        }
        let mut pieces = text.split('\n');
        let first = pieces.next().expect("split has one piece");
        if let Some(line) = self.output.last_mut() {
            self.truncated |= append_capped(line, first);
        } else {
            let mut line = String::new();
            self.truncated |= append_capped(&mut line, first);
            self.output.push(line);
        }
        for piece in pieces {
            let mut line = String::new();
            self.truncated |= append_capped(&mut line, piece);
            self.output.push(line);
        }
        let mut drop = self.output.len().saturating_sub(OUTPUT_TAIL_LINES);
        let mut bytes: usize = self.output[drop..].iter().map(|line| line.len() + 1).sum();
        while bytes > OUTPUT_TAIL_BYTES && drop + 1 < self.output.len() {
            bytes -= self.output[drop].len() + 1;
            drop += 1;
        }
        if drop > 0 {
            self.output.drain(..drop);
            self.output_dropped += drop;
            self.truncated = true;
        }
    }
}

pub const EXPANDED_ROWS: usize = 200;
pub const OUTPUT_TAIL_LINES: usize = 400;
pub const OUTPUT_TAIL_BYTES: usize = 64 * 1024;

fn argument_display(arguments: &Value) -> (String, bool) {
    struct DisplayWriter {
        bytes: Vec<u8>,
        truncated: bool,
    }
    impl std::io::Write for DisplayWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let remaining = OUTPUT_TAIL_BYTES.saturating_sub(self.bytes.len());
            let count = bytes.len().min(remaining);
            self.bytes.extend_from_slice(&bytes[..count]);
            if count < bytes.len() {
                self.truncated = true;
                if count == 0 {
                    return Err(std::io::Error::other("argument display budget reached"));
                }
            }
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DisplayWriter {
        bytes: Vec::new(),
        truncated: false,
    };
    let formatted = serde_json::to_writer_pretty(&mut writer, arguments);
    debug_assert!(formatted.is_ok() || writer.truncated);
    if let Err(error) = std::str::from_utf8(&writer.bytes) {
        writer.bytes.truncate(error.valid_up_to());
    }
    (
        String::from_utf8(writer.bytes).expect("JSON display ends at a UTF-8 boundary"),
        writer.truncated,
    )
}

fn append_capped(line: &mut String, text: &str) -> bool {
    append_capped_to(line, text, OUTPUT_TAIL_BYTES)
}

fn append_capped_to(line: &mut String, text: &str, limit: usize) -> bool {
    if line.len() >= limit && line.ends_with('…') {
        return !text.is_empty();
    }
    line.push_str(text);
    if line.len() <= limit {
        return false;
    }
    let mut end = limit - '…'.len_utf8();
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line.push('…');
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> ToolCall {
        ToolCall::new(
            ToolCallRef {
                turn_id: "00000000000000000001".parse().unwrap(),
                tool_call_id: ToolCallId("call".into()),
            },
            "bash",
        )
    }

    #[test]
    fn complete_arguments_are_not_shortened_to_the_header() {
        let mut call = card();
        let command = "printf '你好'\n".repeat(300);
        let arguments = serde_json::json!({"command":command,"timeout_seconds":5});
        call.set_arguments(&arguments);
        assert_eq!(call.arguments.as_ref(), Some(&arguments));
        assert_eq!(
            call.expanded_input.as_deref(),
            arguments["command"].as_str()
        );
        assert_eq!(call.hidden_input_lines(), 0);
    }

    #[test]
    fn argument_display_is_cached_bounded_utf8_while_native_arguments_stay_complete() {
        let mut call = card();
        let command = "中🙂\n\"quoted\"".repeat(20_000);
        let arguments = serde_json::json!({"command":command,"timeout_seconds":5});
        call.set_arguments(&arguments);
        let (display, truncated) = call.argument_display().unwrap();
        assert!(truncated);
        assert!(display.len() <= OUTPUT_TAIL_BYTES);
        assert!(display.starts_with("{\n  \"command\": \"中🙂\\n\\\"quoted\\\""));
        assert!(!display.contains('\u{fffd}'));
        let address = display.as_ptr();
        let cached = display.to_owned();
        call.push_output("one new output line\n");
        assert_eq!(call.argument_display().unwrap().0.as_ptr(), address);
        assert_eq!(call.argument_display().unwrap().0, cached);
        assert_eq!(call.arguments.as_ref(), Some(&arguments));
        assert_eq!(
            call.expanded_input.as_deref(),
            arguments["command"].as_str()
        );
    }

    #[test]
    fn huge_unicode_output_is_bounded_without_splitting_characters() {
        let mut call = card();
        call.push_output(&"中".repeat(100_000));
        assert!(call.output[0].len() <= OUTPUT_TAIL_BYTES);
        assert!(call.output[0].ends_with('…'));
        call.push_output("\ntail");
        assert_eq!(call.output_content().last(), Some(&"tail"));
    }

    #[test]
    fn terminal_input_result_is_not_reopened_by_a_late_start() {
        let mut call = card();
        call.apply_result(&ToolResultData {
            outcome: ToolResultOutcome::InputProvided,
            output: minicore_host_runtime::tools::ToolOutput::new("accepted").unwrap(),
        });
        call.apply_invocation(&ToolInvocationData {
            name: "question".into(),
            arguments: serde_json::json!({"question":"continue?"}),
        });
        assert_eq!(call.state, ToolState::Succeeded);
        assert!(call.started_at.is_none());
    }

    #[test]
    fn late_verified_stream_output_replaces_the_result_fallback_once() {
        let mut call = card();
        call.apply_result(&ToolResultData {
            outcome: ToolResultOutcome::Success,
            output: minicore_host_runtime::tools::ToolOutput::new("fallback result").unwrap(),
        });
        call.push_output("actual ");
        call.push_output("stream");
        assert_eq!(call.output_content(), ["actual stream"]);
        assert_eq!(call.state, ToolState::Succeeded);
    }

    fn result(value: impl serde::Serialize) -> ToolResultData {
        ToolResultData {
            outcome: ToolResultOutcome::Success,
            output: minicore_host_runtime::tools::ToolOutput::new(
                serde_json::to_string(&value).unwrap(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn native_command_result_supplies_real_streams_and_exit_status() {
        use minicore_host_local_tools::{CommandFileChanges, CommandStatus, CommandStream};
        let mut call = card();
        call.set_arguments(&serde_json::json!({"command":"\n  printf actual","cwd":"."}));
        let command = CommandResult {
            status: CommandStatus::Exited,
            pid: 12,
            exit_code: Some(3),
            signal: None,
            process_waited: true,
            cleanup_confirmed: true,
            stdout: CommandStream {
                output: OutputData::Utf8("actual\n".into()),
                complete: true,
            },
            stderr: CommandStream {
                output: OutputData::Utf8("diagnostic\n".into()),
                complete: true,
            },
            file_changes: CommandFileChanges::Unknown,
            cleanup_errors: Vec::new(),
        };
        call.apply_result(&result(&command));
        assert_eq!(call.header_detail(), "$ printf actual");
        assert_eq!(call.exit_code, Some(3));
        assert_eq!(call.output_content(), ["actual", "diagnostic"]);
        assert_eq!(call.state, ToolState::Succeeded);
        let mut bytes = command;
        bytes.stdout.output = OutputData::Bytes(vec![255, 0]);
        let mut binary = card();
        binary.apply_result(&result(bytes));
        let shown = binary.output.join("\n");
        assert!(shown.contains("\"encoding\":\"bytes\",\"data\":[255,0]"));
        assert!(!shown.contains('\u{fffd}'));
        assert_eq!(binary.exit_code, Some(3));
    }

    #[test]
    fn native_read_result_keeps_its_head_and_arguments_do_not_hide_it() {
        let mut call = card();
        call.name = "read".into();
        let arguments = serde_json::json!({"path":"src/lib.rs","offset":1,"limit":500});
        call.set_arguments(&arguments);
        let text: String = (0..500).map(|line| format!("line {line}\n")).collect();
        call.apply_result(&result(serde_json::json!({
            "status":"ok","kind":"file","path":"src/lib.rs","offset":1,
            "lines":500,"text":text,"next_offset":null,"eof":true
        })));
        assert_eq!(call.arguments.as_ref(), Some(&arguments));
        assert!(call.expanded_input.is_none());
        assert_eq!(call.output.first().map(String::as_str), Some("line 0"));
        assert_eq!(call.output.last().map(String::as_str), Some("line 199"));
        assert_eq!(call.output_lines, Some(500));
    }

    #[test]
    fn native_file_failure_does_not_change_the_execution_outcome() {
        let mut call = card();
        call.name = "write".into();
        call.set_arguments(&serde_json::json!({"path":"file","content":"proposed"}));
        call.apply_result(&result(serde_json::json!({
            "status":"failed","error":{"code":"filesystem","message":"permission denied"}
        })));
        assert_eq!(call.state, ToolState::Succeeded);
        assert_eq!(call.result_error.as_deref(), Some("permission denied"));
        assert!(call.output.join("\n").contains("permission denied"));
        assert_eq!(call.expanded_input.as_deref(), Some("proposed"));
    }

    #[test]
    fn native_edit_arguments_keep_the_complete_requested_replacement() {
        let mut call = card();
        call.name = "edit".into();
        let arguments = serde_json::json!({
            "path":"file","old_text":"one\ntwo\n","new_text":"three\n","replace_all":true
        });
        call.set_arguments(&arguments);
        assert_eq!(call.arguments.as_ref(), Some(&arguments));
        assert_eq!(call.expanded_input.as_deref(), Some("-one\n-two\n+three\n"));
        assert_eq!(call.line_changes(), Some((1, 2)));
    }

    #[test]
    fn the_head_budget_accounts_for_all_retained_lines() {
        let mut call = card();
        call.name = "read".into();
        call.apply_result(&ToolResultData {
            outcome: ToolResultOutcome::Success,
            output: minicore_host_runtime::tools::ToolOutput::new(
                format!("{}\n", "中".repeat(10_000)).repeat(5),
            )
            .unwrap(),
        });
        assert!(call.output.iter().map(|line| line.len() + 1).sum::<usize>() <= OUTPUT_TAIL_BYTES);
        assert!(call.truncated);
    }
}
