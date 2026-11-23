//! Real tool invocations and their bounded presentation output.

use std::time::{Duration, Instant};

use minicore_host_core::{
    DelegationAssociation, ToolCallState, ToolCallStateData, ToolInvocationData, ToolResultData,
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
    pub state: ToolState,
    pub delegations: Vec<DelegationAssociation>,
    pub agent_card: Option<Box<super::agent_card::AgentCard>>,
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
    result_seen: bool,
    arguments_display: Option<(String, bool)>,
    edit_input: Option<String>,
    /// How many places an edit actually replaced, from its result.
    replacements: Option<usize>,
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
            state: ToolState::Pending,
            delegations: Vec::new(),
            agent_card: None,
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
            result_seen: false,
            arguments_display: None,
            edit_input: None,
            replacements: None,
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
        self.arguments_display = Some(json_display(arguments));
        let string = |key: &str| arguments.get(key).and_then(Value::as_str);
        self.detail = if self.name == "agent" {
            agent_subject(arguments)
        } else {
            string("path")
                .or_else(|| string("command"))
                .or_else(|| string("query"))
                .or_else(|| string("pattern"))
                .or_else(|| string("description"))
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .to_owned()
        };
        self.edit_input = if self.name == "edit" {
            string("old_text")
                .zip(string("new_text"))
                .map(|(old, new)| {
                    old.lines()
                        .map(|line| format!("-{line}\n"))
                        .chain(new.lines().map(|line| format!("+{line}\n")))
                        .collect()
                })
        } else {
            None
        };
    }

    /// What an `agent` wait returned, in words; other results keep their body.
    fn agent_wait_text(&self, content: &str) -> Option<&'static str> {
        let value: Value = serde_json::from_str(content).ok()?;
        (self.name == "agent" && self.detail.starts_with("wait")).then_some(())?;
        match value["status"].as_str()? {
            "input_available" => Some("A delegated result or new input arrived\n"),
            "timed_out" => Some("Nothing arrived before the wait timed out\n"),
            _ => None,
        }
    }

    pub fn expanded_input(&self) -> Option<&str> {
        let key = match self.name.as_str() {
            "bash" => "command",
            "write" => "content",
            "apply_patch" => "patch",
            "edit" => return self.edit_input.as_deref(),
            _ => return None,
        };
        self.arguments.as_ref()?.get(key)?.as_str()
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

    pub fn set_preview_arguments(&mut self, arguments: &Value) {
        self.detail = ["command", "path", "query", "pattern", "description"]
            .iter()
            .find_map(|key| arguments.get(key).and_then(Value::as_str))
            .map(|text| super::agent_card::head(text.lines().next().unwrap_or(""), 512).to_owned())
            .unwrap_or_default();
    }

    pub fn apply_preview_result(&mut self, result: &ToolResultData) {
        self.output.clear();
        self.output_dropped = 0;
        self.output_lines = None;
        self.output_from_result = false;
        self.truncated = false;
        self.apply_result_inner(result, true);
    }

    pub fn apply_result(&mut self, result: &ToolResultData) {
        self.apply_result_inner(result, false);
    }

    fn apply_result_inner(&mut self, result: &ToolResultData, preview: bool) {
        self.result_seen = true;
        self.set_state(ToolState::from_outcome(result.outcome));
        self.duration = self
            .duration
            .or_else(|| self.started_at.map(|start| start.elapsed()));
        let content = result.output.content().as_str();
        let content = self.agent_wait_text(content).unwrap_or(content);
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
        if self.name == "edit" {
            self.replacements = parsed
                .as_ref()
                .and_then(|value| value["replacements"].as_u64())
                .and_then(|count| usize::try_from(count).ok());
        }
        if matches!(
            self.name.as_str(),
            "read" | "write" | "edit" | "apply_patch" | "bash"
        ) {
            self.result_error = parsed
                .as_ref()
                .filter(|value| value["status"] == "failed")
                .and_then(|value| value["error"]["message"].as_str())
                .map(str::to_owned);
        }
        if self.name == "agent" && result.outcome == ToolResultOutcome::Failed {
            self.result_error = parsed
                .as_ref()
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
                    let limit = if preview {
                        super::agent_card::PREVIEW_BYTES
                    } else {
                        usize::MAX
                    };
                    self.truncated |= stdout.len().saturating_add(stderr.len()) > limit;
                    let stderr = super::agent_card::tail(stderr, limit);
                    let stdout =
                        super::agent_card::tail(stdout, limit.saturating_sub(stderr.len()));
                    self.push_output(stdout);
                    if !stdout.trim().is_empty() && !stderr.trim().is_empty() {
                        // Both streams interleave in one card; mark where stderr starts.
                        self.push_output(if stdout.ends_with('\n') {
                            "── stderr ──\n"
                        } else {
                            "\n── stderr ──\n"
                        });
                    }
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
                    // Failures and directory listings read as text, not JSON.
                    let readable = parsed.as_ref().and_then(|value| {
                        if value["status"] == "failed" {
                            let error = &value["error"];
                            return Some(format!(
                                "{}{}\n",
                                error["message"].as_str().unwrap_or("failed"),
                                error["code"]
                                    .as_str()
                                    .map(|code| format!(" ({code})"))
                                    .unwrap_or_default()
                            ));
                        }
                        let entries = value["entries"]
                            .as_array()
                            .filter(|_| self.name == "read" && value["kind"] == "directory")?;
                        let mut listing = String::new();
                        for entry in entries {
                            listing.push_str(entry["name"].as_str().unwrap_or("?"));
                            if entry["kind"] == "directory" {
                                listing.push('/');
                            }
                            listing.push('\n');
                        }
                        if let Some(next) = value["next_offset"].as_u64() {
                            listing.push_str(&format!("… more from offset {next}\n"));
                        }
                        Some(listing)
                    });
                    let read_text = read_text.or(readable.as_deref());
                    let formatted = parsed.as_ref().filter(|_| read_text.is_none());
                    let display =
                        formatted
                            .filter(|_| self.name != "bash" || preview)
                            .map(|value| {
                                if preview {
                                    json_tail(value)
                                } else {
                                    json_display(value)
                                }
                            });
                    // An unrecognized bash result keeps its existing pretty-JSON tail.
                    let bash_json =
                        formatted
                            .filter(|_| self.name == "bash" && !preview)
                            .map(|value| {
                                serde_json::to_string_pretty(value).expect("JSON values serialize")
                            });
                    self.truncated |= display.as_ref().is_some_and(|(_, truncated)| *truncated);
                    let body = read_text
                        .or(display.as_ref().map(|(text, _)| text.as_str()))
                        .or(bash_json.as_deref())
                        .unwrap_or(content);
                    self.output_lines =
                        Some(formatted.map_or_else(|| body.lines().count(), json_lines));
                    if preview {
                        self.truncated |= body.len() > super::agent_card::PREVIEW_BYTES;
                        self.push_output(super::agent_card::tail(
                            body,
                            super::agent_card::PREVIEW_BYTES,
                        ));
                    } else if self.name == "bash" {
                        self.push_output(body);
                    } else {
                        self.push_head(body);
                    }
                }
            }
            self.output_from_result = true;
        }
        if preview {
            let drop = self.output.len().saturating_sub(EXPANDED_ROWS + 1);
            self.output.drain(..drop);
            self.output_dropped += drop;
            self.output.shrink_to_fit();
        } else {
            self.settle();
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == ToolState::Running
    }

    pub fn has_result(&self) -> bool {
        self.result_seen
    }

    pub fn is_delegation_receipt(&self) -> bool {
        self.agent_card.is_none()
            && !self.delegations.is_empty()
            && self.key.call_ref().is_some_and(|key| {
                self.delegations
                    .iter()
                    .all(|association| &association.origin != key)
            })
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
                .expanded_input()
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
        for line in self.expanded_input().unwrap_or("").lines() {
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
        let input = self.expanded_input()?;
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
                // An edit's text is one replacement; replace_all repeats it.
                let times = if self.name == "edit" {
                    self.replacements.unwrap_or(1).max(1)
                } else {
                    1
                };
                Some((counts.0 * times, counts.1 * times))
            }
            _ => None,
        }
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

fn json_display(value: &Value) -> (String, bool) {
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
                    return Err(std::io::Error::other("JSON display budget reached"));
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
    let formatted = serde_json::to_writer_pretty(&mut writer, value);
    debug_assert!(formatted.is_ok() || writer.truncated);
    if let Err(error) = std::str::from_utf8(&writer.bytes) {
        writer.bytes.truncate(error.valid_up_to());
    }
    (
        String::from_utf8(writer.bytes).expect("JSON display ends at a UTF-8 boundary"),
        writer.truncated,
    )
}

fn json_tail(value: &Value) -> (String, bool) {
    use super::agent_card::PREVIEW_BYTES;
    use std::collections::VecDeque;
    #[derive(Default)]
    struct TailWriter {
        bytes: VecDeque<u8>,
        truncated: bool,
    }
    impl std::io::Write for TailWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let discard = (self.bytes.len() + bytes.len()).saturating_sub(PREVIEW_BYTES);
            self.truncated |= discard > 0;
            let previous = self.bytes.len();
            self.bytes.drain(..discard.min(previous));
            self.bytes
                .extend(&bytes[discard.saturating_sub(previous)..]);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = TailWriter::default();
    serde_json::to_writer_pretty(&mut writer, value).expect("JSON values serialize");
    let bytes = writer
        .bytes
        .into_iter()
        .skip_while(|byte| *byte & 0xc0 == 0x80)
        .collect();
    (
        String::from_utf8(bytes).expect("JSON tail starts at a UTF-8 boundary"),
        writer.truncated,
    )
}

/// Pretty JSON escapes string newlines; only nonempty containers add rows.
fn json_lines(value: &Value) -> usize {
    match value {
        Value::Array(values) if !values.is_empty() => {
            2 + values.iter().map(json_lines).sum::<usize>()
        }
        Value::Object(values) if !values.is_empty() => {
            2 + values.values().map(json_lines).sum::<usize>()
        }
        _ => 1,
    }
}

fn append_capped(line: &mut String, text: &str) -> bool {
    append_capped_to(line, text, OUTPUT_TAIL_BYTES)
}

fn append_capped_to(line: &mut String, text: &str, limit: usize) -> bool {
    if line.len() >= limit && line.ends_with('…') {
        return !text.is_empty();
    }
    if text.len() <= limit.saturating_sub(line.len()) {
        line.push_str(text);
        return false;
    }
    line.reserve_exact(limit.saturating_sub(line.len()));
    let mut end = limit - '…'.len_utf8();
    if line.len() > end {
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
    } else {
        end -= line.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        line.push_str(&text[..end]);
    }
    line.push('…');
    true
}

/// `create session · task`, `send · task`, `wait for delegated results`: the
/// action an `agent` call takes, in place of its JSON.
fn agent_subject(arguments: &Value) -> String {
    let field = |key: &str| arguments.get(key).and_then(Value::as_str).unwrap_or("");
    let first_line = |text: &str| {
        text.lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    match field("action") {
        "wait" => "wait for delegated results".into(),
        "create" => {
            let task = first_line(field("text"));
            let what = format!("create {}", field("resource"));
            if task.is_empty() {
                what
            } else {
                format!("{what} · {task}")
            }
        }
        "send" => format!("send · {}", first_line(field("text"))),
        "" => String::new(),
        action => action.to_owned(),
    }
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
    fn complete_input_borrows_the_native_arguments() {
        for (name, key) in [
            ("bash", "command"),
            ("write", "content"),
            ("apply_patch", "patch"),
        ] {
            let mut call = card();
            call.name = name.into();
            let input = "printf '你好'\n".repeat(300);
            let arguments = serde_json::json!({key:input,"timeout_seconds":5});
            call.set_arguments(&arguments);
            let stored = call.arguments.as_ref().unwrap()[key].as_str().unwrap();
            assert_eq!(call.arguments.as_ref(), Some(&arguments));
            assert_eq!(call.expanded_input(), Some(input.as_str()));
            assert_eq!(call.expanded_input().unwrap().as_ptr(), stored.as_ptr());
        }
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
        assert_eq!(call.expanded_input(), arguments["command"].as_str());
    }

    #[test]
    fn huge_output_keeps_bounded_capacity_without_splitting_characters() {
        for size in [1024 * 1024, 8 * 1024 * 1024] {
            for character in ["x", "中🙂"] {
                let mut call = card();
                call.push_output(&character.repeat(size / character.len()));
                call.settle();
                assert!(call.output[0].len() <= OUTPUT_TAIL_BYTES);
                assert!(call.output[0].capacity() <= OUTPUT_TAIL_BYTES);
                assert!(call.output[0].ends_with('…'));
                call.push_output("\ntail");
                assert_eq!(call.output_content().last(), Some(&"tail"));
            }
        }
    }

    #[test]
    fn a_capped_append_keeps_the_utf8_prefix_across_chunks() {
        for chunks in [["ab", "中🙂xyz"], ["ab中🙂", "xyz"]] {
            let mut line = String::new();
            assert!(!append_capped_to(&mut line, chunks[0], 10));
            assert!(append_capped_to(&mut line, chunks[1], 10));
            assert_eq!(line, "ab中…");
        }
        let mut line = String::new();
        assert!(!append_capped_to(&mut line, "0123456789", 10));
        assert!(append_capped_to(&mut line, "x", 10));
        assert_eq!(line, "0123456…");
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
                omitted_bytes: 0,
            },
            stderr: CommandStream {
                output: OutputData::Utf8("diagnostic\n".into()),
                complete: true,
                omitted_bytes: 0,
            },
            file_changes: CommandFileChanges::Unknown,
            cleanup_errors: Vec::new(),
        };
        call.apply_result(&result(&command));
        assert_eq!(call.header_detail(), "$ printf actual");
        assert_eq!(call.exit_code, Some(3));
        assert_eq!(
            call.output_content(),
            ["actual", "── stderr ──", "diagnostic"]
        );
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
    fn large_native_result_previews_keep_decoded_recent_lines() {
        use minicore_host_local_tools::{CommandFileChanges, CommandStatus, CommandStream};
        let text = format!(
            "{}last first\n\nlast second\n",
            "actual output 日本語\n".repeat(600)
        );
        let command = CommandResult {
            status: CommandStatus::Exited,
            pid: 12,
            exit_code: Some(3),
            signal: None,
            process_waited: true,
            cleanup_confirmed: true,
            stdout: CommandStream {
                output: OutputData::Utf8(text.clone()),
                complete: true,
                omitted_bytes: 0,
            },
            stderr: CommandStream {
                output: OutputData::Utf8("diagnostic\n".into()),
                complete: true,
                omitted_bytes: 0,
            },
            file_changes: CommandFileChanges::Unknown,
            cleanup_errors: Vec::new(),
        };
        let mut bash = card();
        bash.push_output("older observed output");
        bash.apply_preview_result(&result(command));
        assert_eq!(bash.exit_code, Some(3));
        assert!(bash.output_content().ends_with(&[
            "last first",
            "",
            "last second",
            "── stderr ──",
            "diagnostic"
        ]));
        assert!(bash.truncated);
        assert!(
            bash.output.iter().map(|line| line.len() + 1).sum::<usize>()
                <= super::super::agent_card::PREVIEW_BYTES + 1
        );

        let mut read = card();
        read.name = "read".into();
        read.apply_preview_result(&result(
            serde_json::json!({"status":"ok","kind":"file","text":text}),
        ));
        assert!(
            read.output_content()
                .ends_with(&["last first", "", "last second"])
        );
        assert!(read.truncated);
        assert!(!read.output.join("\n").contains("\\n"));
        assert!(
            read.output.iter().map(|line| line.len() + 1).sum::<usize>()
                <= super::super::agent_card::PREVIEW_BYTES + 1
        );

        read.apply_preview_result(&result(serde_json::json!({"status":"ok","kind":"file","text":format!("{}final\n", "x\n".repeat(300))})));
        assert_eq!(read.output_content().last(), Some(&"final"));
    }

    #[test]
    fn generic_result_previews_keep_the_actual_json_tail() {
        let value = serde_json::json!({"rows": (0..5000).collect::<Vec<_>>()});
        let mut call = card();
        call.name = "agent".into();
        call.apply_preview_result(&result(&value));
        let shown = call.output.join("\n");
        assert!(shown.contains("4999"));
        assert!(shown.ends_with('}'));
        assert!(call.truncated);
        let unicode = serde_json::json!({"text": format!("{}last", "日本語".repeat(5000))});
        for value in [&value, &unicode] {
            let (tail, truncated) = json_tail(value);
            let full = serde_json::to_string_pretty(value).unwrap();
            assert!(truncated);
            assert_eq!(
                tail,
                super::super::agent_card::tail(&full, super::super::agent_card::PREVIEW_BYTES)
            );
        }
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
        assert!(call.expanded_input().is_none());
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
        assert_eq!(call.expanded_input(), Some("proposed"));
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
        assert_eq!(call.expanded_input(), Some("-one\n-two\n+three\n"));
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

    #[test]
    fn json_results_keep_a_bounded_preview_and_the_complete_line_count() {
        let value = serde_json::json!({"rows":(0..2000).map(|index| serde_json::json!({"index":index,"text":"中🙂".repeat(100)})).collect::<Vec<_>>()});
        let full = serde_json::to_string_pretty(&value).unwrap();
        let mut call = card();
        call.name = "fixture".into();
        call.apply_result(&result(value));
        assert!(call.truncated);
        assert!(call.output.iter().map(|line| line.len() + 1).sum::<usize>() <= OUTPUT_TAIL_BYTES);
        assert!(call.output.len() <= EXPANDED_ROWS);
        assert_eq!(call.output_lines, Some(full.lines().count()));
        assert!(!call.output.join("\n").contains('\u{fffd}'));
        assert_eq!(call.output.first().unwrap(), "{");
    }

    #[test]
    fn json_line_counts_include_empty_containers_and_escaped_newlines() {
        for value in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"a":[{},[],"one\ntwo",{"b":[1,2]}]}),
        ] {
            assert_eq!(
                json_lines(&value),
                serde_json::to_string_pretty(&value)
                    .unwrap()
                    .lines()
                    .count()
            );
        }
    }

    #[test]
    fn unrecognized_bash_json_keeps_the_pretty_result_tail() {
        let value = serde_json::json!({"rows":(0..300).collect::<Vec<_>>()});
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let mut call = card();
        call.apply_result(&result(value));
        let expected = pretty.lines().collect::<Vec<_>>();
        assert_eq!(call.output_lines, Some(expected.len()));
        assert_eq!(
            call.output_content(),
            expected[expected.len() - call.output.len()..]
        );
        assert!(call.output.len() > 1);
    }
}
