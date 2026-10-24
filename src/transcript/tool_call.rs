//! A tool call card: its state, facts and the bounded tail of its output.

use std::time::{Duration, Instant};

use crate::protocol::event::{CommandResult, ProcessChunk, ProcessStream};
use crate::protocol::{
    ToolDisplay, ToolExecution, ToolExecutionState, ToolInvocation, ToolReadResult, ToolRef,
    ToolSummary,
};

/// One tool call card with a bounded output tail.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub key: ToolRef,
    pub name: String,
    pub detail: String,
    /// The display body: the command for `bash`, the content for `write`, the
    /// diff for `edit` and `apply_patch`.
    pub expanded_input: Option<String>,
    /// Lines in the whole input, which can exceed what `expanded_input` holds.
    pub input_lines: Option<usize>,
    pub state: ToolState,
    pub started_at: Option<Instant>,
    pub duration: Option<Duration>,
    pub exit_code: Option<i64>,
    /// The output tail; the last entry is the line still being written.
    pub output: Vec<String>,
    /// Lines dropped from the front of `output` to keep it bounded.
    pub output_dropped: usize,
    /// Lines of the model-facing result, as the Agent counted them.
    pub output_lines: Option<usize>,
    pub truncated: bool,
    /// The reader's per-card fold; `None` follows the global Ctrl+O.
    pub expanded: Option<bool>,
    /// Next expected byte offset per stream; `None` until the first chunk sets it.
    pub stream_offsets: [Option<u64>; 2],
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

    pub fn from_execution(state: ToolExecutionState) -> Self {
        match state {
            ToolExecutionState::Requested => Self::Pending,
            ToolExecutionState::AwaitingPolicy
            | ToolExecutionState::Running
            | ToolExecutionState::Cancelling => Self::Running,
            ToolExecutionState::Succeeded => Self::Succeeded,
            ToolExecutionState::Failed => Self::Failed,
            ToolExecutionState::Denied => Self::Denied,
            ToolExecutionState::Cancelled => Self::Cancelled,
            // `input_provided` means the call yielded to an interaction; the
            // final tool_finished event decides the card state.
            ToolExecutionState::InputProvided | ToolExecutionState::Unknown => Self::Pending,
        }
    }
}

impl ToolCall {
    pub fn new(key: ToolRef, name: impl Into<String>) -> Self {
        Self {
            key,
            name: name.into(),
            detail: String::new(),
            expanded_input: None,
            input_lines: None,
            state: ToolState::Pending,
            started_at: None,
            duration: None,
            exit_code: None,
            output: Vec::new(),
            output_dropped: 0,
            output_lines: None,
            truncated: false,
            expanded: None,
            stream_offsets: [None, None],
        }
    }

    /// Advances the state; a terminal state is not reopened by a late or replayed update.
    pub fn set_state(&mut self, state: ToolState) {
        if self.state.is_terminal() && !state.is_terminal() {
            return;
        }
        self.state = state;
    }

    pub fn apply_display(&mut self, display: &ToolDisplay) {
        if !display.detail.is_empty() {
            self.detail = display.detail.clone();
        }
        if display.expanded_input.is_some() {
            self.expanded_input = display.expanded_input.clone();
        }
        self.input_lines = display.input_line_count.or(self.input_lines);
    }

    /// Fills only a missing detail: the display detail is the Agent's one-line
    /// summary, the subject can be a whole script.
    pub fn apply_invocation(&mut self, invocation: &ToolInvocation) {
        if !invocation.name.is_empty() {
            self.name = invocation.name.clone();
        }
        if self.detail.is_empty() {
            self.detail = invocation.detail();
        }
    }

    /// A command record; a later one without an exit code keeps the known one.
    pub fn apply_command(&mut self, command: &CommandResult) {
        self.exit_code = command.exit_code.or(self.exit_code);
    }

    /// An execution record: state, duration, counts and the command's exit code.
    pub fn apply_execution(&mut self, execution: &ToolExecution) {
        self.set_state(ToolState::from_execution(execution.state));
        self.duration = execution.duration().or(self.duration);
        self.output_lines = execution.output_line_count.or(self.output_lines);
        self.truncated |= execution.result_truncated;
        if let Some(command) = &execution.command {
            self.apply_command(command);
        }
    }

    /// A `tool.read` answer: whatever of display, invocation and execution it holds.
    pub fn apply_read(&mut self, read: &ToolReadResult) {
        if let Some(display) = &read.display {
            self.apply_display(display);
        }
        if let Some(invocation) = &read.invocation {
            self.apply_invocation(invocation);
        }
        if let Some(execution) = &read.execution {
            self.apply_execution(execution);
        }
    }

    /// The model-facing result: a `bash` result gives its exit code and keeps its
    /// tail, any other keeps its head.
    pub fn apply_result(&mut self, content: &str) {
        let bash = (self.name == "bash")
            .then(|| split_bash_result(content))
            .flatten();
        if let Some((code, _, _)) = bash {
            self.exit_code = self.exit_code.or(code);
        }
        // Live process output is the better source.
        if !self.output.is_empty() {
            return;
        }
        match bash {
            Some((_, stdout, stderr)) => {
                self.push_output(stdout);
                self.push_output(stderr);
            }
            None => self.push_head(content),
        }
    }

    /// Stores the head of a result within the card bounds, keeping the Agent's
    /// cut marker as the last line since its line total counts it.
    fn push_head(&mut self, content: &str) {
        let body = content.strip_suffix('\n').unwrap_or(content);
        let (body, cut) = match body.strip_suffix(TRUNCATED_MARKER) {
            Some(rest) if rest.is_empty() || rest.ends_with('\n') => (rest, true),
            _ => (body, false),
        };
        let mut bytes = 0;
        for line in body.lines().take(OUTPUT_TAIL_LINES) {
            if bytes > OUTPUT_TAIL_BYTES {
                break;
            }
            let mut kept = String::new();
            self.truncated |= append_capped(&mut kept, line);
            bytes += kept.len() + 1;
            self.output.push(kept);
        }
        if cut {
            self.truncated = true;
            self.output.push(TRUNCATED_MARKER.to_owned());
        }
    }

    /// Keeps only the expanded rows of a finished card: the tail for `bash`, otherwise the head.
    pub fn settle(&mut self) {
        let content = self.output_content().len();
        if content <= EXPANDED_ROWS {
            return;
        }
        if self.name == "bash" {
            // The open last line stays so the kept lines are the same ones.
            let drop = self.output.len().saturating_sub(EXPANDED_ROWS + 1);
            self.output.drain(..drop);
            self.output_dropped += drop;
        } else {
            // The Agent's total counts the cut marker, which the head loses.
            let markers = self
                .output
                .iter()
                .filter(|line| *line == TRUNCATED_MARKER)
                .count();
            self.output_lines = Some(
                self.output_lines
                    .map_or(0, |total| total.saturating_sub(markers))
                    .max(content),
            );
            self.output.truncate(EXPANDED_ROWS);
        }
        self.output.shrink_to_fit();
    }

    /// Carries over what only live events knew when history replaces this card.
    pub fn keep_live_facts(&mut self, live: &ToolCall) {
        if self.detail.is_empty() {
            self.detail.clone_from(&live.detail);
        }
        if self.expanded_input.is_none() {
            self.expanded_input.clone_from(&live.expanded_input);
            self.input_lines = self.input_lines.or(live.input_lines);
        }
        if self.detail.is_empty()
            && self.name == "bash"
            && let Some(script) = self
                .expanded_input
                .as_deref()
                .and_then(|input| input.lines().next())
        {
            self.detail = format!("$ {script}");
        }
        self.duration = self.duration.or(live.duration);
        self.exit_code = self.exit_code.or(live.exit_code);
        self.expanded = self.expanded.or(live.expanded);
        if live.stream_offsets.iter().any(Option::is_some) {
            self.output.clone_from(&live.output);
            self.output_dropped = live.output_dropped;
            self.truncated |= live.truncated;
        }
    }

    /// A call that has started and not finished; one still waiting on approval
    /// or input has not.
    pub fn is_running(&self) -> bool {
        !self.state.is_terminal() && self.started_at.is_some()
    }

    /// Whether the card shows its expanded body under the global fold.
    pub fn is_expanded(&self, all_expanded: bool) -> bool {
        self.expanded.unwrap_or(all_expanded)
    }

    /// Flips this card; one back in line with the global fold follows it again.
    pub fn toggle_expanded(&mut self, all_expanded: bool) {
        let expanded = !self.is_expanded(all_expanded);
        self.expanded = (expanded != all_expanded).then_some(expanded);
    }

    /// The header detail: the command that ran for `bash`, since the Agent's
    /// summary can name another one; the paths of a patch replace its
    /// `tool apply_patch` placeholder.
    pub fn header_detail(&self) -> String {
        if self.name == "bash"
            && let Some(command) = self
                .expanded_input
                .as_deref()
                .and_then(|input| input.lines().find(|line| !line.trim().is_empty()))
        {
            return format!("$ {}", command.trim());
        }
        if !self.detail.is_empty() && !self.detail.starts_with("tool ") {
            return self.detail.clone();
        }
        let paths = self.patch_paths();
        if paths.is_empty() {
            self.detail.clone()
        } else {
            paths.join(", ")
        }
    }

    fn patch_paths(&self) -> Vec<&str> {
        const PREFIXES: [&str; 6] = [
            "*** Update File: ",
            "*** Add File: ",
            "*** Delete File: ",
            "*** Move to: ",
            "+++ b/",
            "--- a/",
        ];
        let mut paths: Vec<&str> = Vec::new();
        for line in self.expanded_input.as_deref().unwrap_or("").lines() {
            let path = PREFIXES
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

    /// Added and removed lines of a file-changing call: diff lines for `edit`
    /// and `apply_patch`, every line for `write`.
    pub fn line_changes(&self) -> Option<(usize, usize)> {
        let input = self.expanded_input.as_deref()?;
        match self.name.as_str() {
            "write" => Some((input.lines().count() + self.hidden_input_lines(), 0)),
            "edit" | "apply_patch" | "patch" => {
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

    /// Input lines the Agent left out of `expanded_input` to bound it.
    pub fn hidden_input_lines(&self) -> usize {
        let Some(input) = self.expanded_input.as_deref() else {
            return 0;
        };
        // The Agent counts a trailing newline as starting one more line.
        let sent = if input.is_empty() {
            0
        } else {
            input.matches('\n').count() + 1
        };
        self.input_lines
            .map_or(0, |total| total.saturating_sub(sent))
    }

    /// The output lines worth showing, without an empty open line or the `[truncated]` marker.
    pub fn output_content(&self) -> Vec<&str> {
        let mut lines: Vec<&str> = self
            .output
            .iter()
            .map(String::as_str)
            .filter(|line| *line != TRUNCATED_MARKER)
            .collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        lines
    }

    pub fn apply_summary(&mut self, summary: &ToolSummary) {
        self.apply_display(&summary.display);
        self.output_lines = summary.output_line_count.or(self.output_lines);
        self.truncated |= summary.output_truncated;
        self.state = match summary.state.as_deref() {
            Some("failed") => ToolState::Failed,
            Some("denied") => ToolState::Denied,
            Some("cancelled") => ToolState::Cancelled,
            Some("succeeded") => ToolState::Succeeded,
            _ => self.state,
        };
    }

    /// The text of a new chunk for `stream`, or `None` for a duplicate, stale or expired one.
    pub fn accept_chunk(
        &mut self,
        stream: ProcessStream,
        chunk: &ProcessChunk,
        text: String,
    ) -> Option<String> {
        let slot = match stream {
            ProcessStream::Stdout => 0,
            ProcessStream::Stderr => 1,
            ProcessStream::Other => return Some(text),
        };
        if chunk.expired {
            return None;
        }
        match self.stream_offsets[slot] {
            Some(expected) if chunk.base_offset == expected => {}
            Some(expected) if chunk.base_offset < expected => return None,
            Some(_) => {
                // The Agent dropped bytes: say so rather than joining halves.
                self.stream_offsets[slot] = Some(chunk.next_offset);
                self.mark_gap();
                return Some(text);
            }
            None => {}
        }
        self.stream_offsets[slot] = Some(chunk.next_offset);
        Some(text)
    }

    /// Puts a `…` line where output is missing and starts a new line after it.
    fn mark_gap(&mut self) {
        if self.output.last().is_some_and(String::is_empty) {
            self.output.pop();
        }
        self.output.push("…".to_owned());
        self.output.push(String::new());
    }

    /// Appends to the open last line, keeping a tail bounded by lines and bytes.
    pub fn push_output(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut pieces = text.split('\n');
        let first = pieces.next().unwrap_or_default();
        match self.output.last_mut() {
            Some(open) => self.truncated |= append_capped(open, first),
            None => {
                let mut line = String::new();
                self.truncated |= append_capped(&mut line, first);
                self.output.push(line);
            }
        }
        for line in pieces {
            let mut capped = String::new();
            self.truncated |= append_capped(&mut capped, line);
            self.output.push(capped);
        }
        self.bound_output();
    }

    fn bound_output(&mut self) {
        let mut drop = self.output.len().saturating_sub(OUTPUT_TAIL_LINES);
        // One line can be megabytes long, so the byte budget bounds the tail
        // too; the newest line stays so a card never goes blank.
        let mut bytes = self.output[drop..]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>();
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

/// Maximum body rows of an expanded card.
pub const EXPANDED_ROWS: usize = 200;
/// Lines of process output kept per running card.
pub const OUTPUT_TAIL_LINES: usize = 400;
/// Byte cap for a card's stored output tail.
pub const OUTPUT_TAIL_BYTES: usize = 64 * 1024;
/// The line older Agents' `read` and `bash` results end with when cut; stored history keeps it.
pub const TRUNCATED_MARKER: &str = "[truncated]";

/// Appends to one stored line within the byte cap; returns whether anything was cut.
fn append_capped(line: &mut String, text: &str) -> bool {
    if line.len() >= OUTPUT_TAIL_BYTES && line.ends_with('…') {
        // Already cut and marked.
        return !text.is_empty();
    }
    line.push_str(text);
    if line.len() <= OUTPUT_TAIL_BYTES {
        return false;
    }
    let mut end = OUTPUT_TAIL_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line.push('…');
    true
}

/// Splits a `bash` result into its exit code, stdout and stderr.
fn split_bash_result(content: &str) -> Option<(Option<i64>, &str, &str)> {
    let (code, rest) = content.strip_prefix("exit_code: ")?.split_once('\n')?;
    let body = rest.strip_prefix("stdout:\n")?;
    let (stdout, stderr) = match (body.strip_prefix("stderr:\n"), body.rfind("\nstderr:\n")) {
        (Some(stderr), _) => ("", stderr),
        (None, Some(at)) => (&body[..=at], &body[at + "\nstderr:\n".len()..]),
        (None, None) => (body, ""),
    };
    Some((code.trim().parse().ok(), stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(call: &str) -> ToolRef {
        ToolRef {
            session_id: "s".into(),
            loop_id: "l".into(),
            request_index: 1,
            tool_call_id: call.into(),
        }
    }

    #[test]
    fn a_finished_tool_never_goes_back_to_running() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.set_state(ToolState::Succeeded);
        call.set_state(ToolState::Running);
        assert_eq!(call.state, ToolState::Succeeded);
        // A terminal state can still be corrected, e.g. cancelled then failed.
        call.set_state(ToolState::Failed);
        assert_eq!(call.state, ToolState::Failed);
    }

    #[test]
    fn a_pending_tool_still_advances_to_running() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.set_state(ToolState::Pending);
        call.set_state(ToolState::Running);
        assert_eq!(call.state, ToolState::Running);
    }

    #[test]
    fn execution_state_maps_to_card_state() {
        assert_eq!(
            ToolState::from_execution(ToolExecutionState::AwaitingPolicy),
            ToolState::Running
        );
        assert_eq!(
            ToolState::from_execution(ToolExecutionState::Denied),
            ToolState::Denied
        );
    }

    #[test]
    fn a_single_huge_line_cannot_bloat_a_card() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output(&"x".repeat(OUTPUT_TAIL_BYTES * 3));
        assert_eq!(call.output.len(), 1);
        assert!(
            call.output[0].len() <= OUTPUT_TAIL_BYTES + 4,
            "{}",
            call.output[0].len()
        );
        assert!(call.output[0].ends_with('…'));
        assert!(call.truncated);
    }

    #[test]
    fn the_output_tail_is_bounded_by_bytes_as_well_as_lines() {
        let mut call = ToolCall::new(key("c"), "bash");
        let line = format!("{}\n", "y".repeat(1024));
        for _ in 0..200 {
            call.push_output(&line);
        }
        let bytes: usize = call.output.iter().map(|line| line.len() + 1).sum();
        assert!(bytes <= OUTPUT_TAIL_BYTES, "{bytes} bytes kept");
        assert!(call.truncated);
    }

    #[test]
    fn tool_output_keeps_a_bounded_tail() {
        let mut call = ToolCall::new(key("c"), "bash");
        for line in 0..OUTPUT_TAIL_LINES + 20 {
            call.push_output(&format!("line {line}\n"));
        }
        assert!(call.truncated);
        assert!(call.output.len() <= OUTPUT_TAIL_LINES + 1);
        assert_eq!(call.output.last().unwrap(), "");
        assert!(call.output.iter().any(|line| line == "line 419"));
        assert_eq!(
            call.output_dropped + call.output_content().len(),
            OUTPUT_TAIL_LINES + 20,
            "dropped and kept lines add up to what arrived"
        );
    }

    #[test]
    fn settling_a_bash_card_keeps_the_tail_an_expanded_card_shows() {
        let mut call = ToolCall::new(key("c"), "bash");
        for line in 0..400 {
            call.push_output(&format!("line {line}\n"));
        }
        call.settle();
        assert_eq!(call.output.len(), EXPANDED_ROWS + 1);
        assert_eq!(call.output_content().last(), Some(&"line 399"));
        assert_eq!(
            call.output_dropped + call.output_content().len(),
            400,
            "dropped and kept lines still add up"
        );
    }

    #[test]
    fn settling_a_read_card_keeps_the_head_and_the_total() {
        let mut call = ToolCall::new(key("c"), "read");
        let content: String = (0..399).map(|line| format!("line {line}\n")).collect();
        call.apply_result(&format!("{content}{TRUNCATED_MARKER}\n"));
        call.output_lines = Some(400);
        call.settle();
        assert_eq!(call.output.len(), EXPANDED_ROWS);
        assert_eq!(call.output[0], "line 0");
        assert_eq!(
            call.output_lines,
            Some(399),
            "the marker is no longer a line"
        );
    }

    #[test]
    fn a_short_card_is_left_alone() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output("a\nb\n");
        let before = call.output.clone();
        call.settle();
        assert_eq!(call.output, before);
        assert_eq!(call.output_dropped, 0);
    }

    #[test]
    fn chunks_split_mid_line_join_into_one_line() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output("hello wo");
        call.push_output("rld\nnext\n");
        call.push_output("last\n");
        assert_eq!(call.output_content(), ["hello world", "next", "last"]);
    }

    #[test]
    fn a_bash_result_gives_its_exit_code_and_drops_the_labels() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.apply_result("exit_code: 3\nstdout:\nRan 2 tests\nFAILED\nstderr:\nboom\n");
        assert_eq!(call.exit_code, Some(3));
        assert_eq!(call.output_content(), ["Ran 2 tests", "FAILED", "boom"]);

        let mut quiet = ToolCall::new(key("c"), "bash");
        quiet.apply_result("exit_code: 0\nstdout:\nstderr:\n");
        assert_eq!(quiet.exit_code, Some(0));
        assert!(quiet.output_content().is_empty());
    }

    #[test]
    fn a_long_read_result_keeps_its_head_and_the_cut_marker() {
        let mut call = ToolCall::new(key("c"), "read");
        let lines: Vec<String> = (1..=1000).map(|n| format!("{n}: row")).collect();
        call.apply_result(&format!("{}\n[truncated]", lines.join("\n")));
        assert_eq!(call.output[0], "1: row");
        assert_eq!(call.output.last().unwrap(), TRUNCATED_MARKER);
        assert!(call.output.len() <= OUTPUT_TAIL_LINES + 1);
        assert!(call.truncated);
        assert_eq!(call.output_content().len(), OUTPUT_TAIL_LINES);
    }

    #[test]
    fn a_result_does_not_replace_live_output() {
        let mut call = ToolCall::new(key("c"), "bash");
        call.push_output("live\n");
        call.apply_result("exit_code: 1\nstdout:\npreview\nstderr:\n");
        assert_eq!(call.output_content(), ["live"]);
        assert_eq!(call.exit_code, Some(1), "the exit code still lands");
    }

    #[test]
    fn a_command_record_without_an_exit_code_keeps_the_known_one() {
        use crate::protocol::event::{CommandResult, CommandStatus};
        let record = |exit_code| CommandResult {
            status: CommandStatus::Exited,
            exit_code,
        };
        let mut call = ToolCall::new(key("c"), "bash");
        call.apply_command(&record(Some(2)));
        call.apply_command(&record(None));
        assert_eq!(call.exit_code, Some(2));
    }

    #[test]
    fn an_invocation_does_not_override_the_card_detail() {
        use crate::protocol::tool::ToolSubject;
        let invocation = ToolInvocation {
            tool_ref: key("c"),
            name: "bash".into(),
            subject: ToolSubject::Command {
                script: "make\nmake test".into(),
            },
        };
        let mut call = ToolCall::new(key("c"), "bash");
        call.apply_invocation(&invocation);
        assert_eq!(
            call.detail, "$ make\nmake test",
            "it fills a missing detail"
        );
        call.detail = "$ make".into();
        call.apply_invocation(&invocation);
        assert_eq!(call.detail, "$ make");
    }

    #[test]
    fn an_apply_patch_header_names_the_files_it_touches() {
        let mut call = ToolCall::new(key("c"), "apply_patch");
        call.detail = "tool apply_patch".into();
        call.expanded_input = Some(
            "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-old\n+new\n+more\n\
             *** Add File: src/b.rs\n+fresh\n*** End Patch"
                .into(),
        );
        assert_eq!(call.header_detail(), "src/a.rs, src/b.rs");
        assert_eq!(call.line_changes(), Some((3, 1)));

        let mut unified = ToolCall::new(key("c"), "apply_patch");
        unified.expanded_input = Some("--- a/app.py\n+++ b/app.py\n@@ -1 +1 @@\n-a\n+b".into());
        assert_eq!(unified.header_detail(), "app.py");
        assert_eq!(
            unified.line_changes(),
            Some((1, 1)),
            "file headers are not lines"
        );
    }

    #[test]
    fn a_written_file_counts_every_line_as_added() {
        let mut call = ToolCall::new(key("c"), "write");
        call.expanded_input = Some("a\nb".into());
        call.input_lines = Some(40);
        assert_eq!(call.line_changes(), Some((40, 0)));
    }

    #[test]
    fn a_card_toggle_overrides_the_global_fold_until_it_matches_again() {
        let mut call = ToolCall::new(key("c"), "bash");
        assert!(!call.is_expanded(false));
        call.toggle_expanded(false);
        assert!(call.is_expanded(false));
        assert!(call.is_expanded(true));
        call.toggle_expanded(false);
        assert_eq!(call.expanded, None, "back in line, it follows Ctrl+O again");
        assert!(call.is_expanded(true));
    }

    fn chunk(stream: ProcessStream, base: u64, next: u64, expired: bool) -> ProcessChunk {
        ProcessChunk {
            stream,
            encoding: "base64".into(),
            data: String::new(),
            base_offset: base,
            next_offset: next,
            expired,
        }
    }

    #[test]
    fn stdout_and_stderr_keep_independent_cursors() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        let err = ProcessStream::Stderr;
        assert!(
            call.accept_chunk(out, &chunk(out, 0, 10, false), "a".into())
                .is_some()
        );
        // stderr starts at its own zero; the stdout cursor must not apply.
        assert!(
            call.accept_chunk(err, &chunk(err, 0, 4, false), "b".into())
                .is_some()
        );
        assert!(
            call.accept_chunk(out, &chunk(out, 10, 20, false), "c".into())
                .is_some()
        );
        assert_eq!(call.stream_offsets, [Some(20), Some(4)]);
    }

    #[test]
    fn a_replayed_chunk_is_ignored() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        call.accept_chunk(out, &chunk(out, 0, 10, false), "a".into());
        assert!(
            call.accept_chunk(out, &chunk(out, 0, 10, false), "a".into())
                .is_none()
        );
    }

    /// Feeds one chunk exactly as the reducer does.
    fn feed(call: &mut ToolCall, stream: ProcessStream, base: u64, next: u64, text: &str) {
        if let Some(text) =
            call.accept_chunk(stream, &chunk(stream, base, next, false), text.into())
        {
            call.push_output(&text);
        }
    }

    #[test]
    fn a_dropped_range_is_marked_instead_of_spliced() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        feed(&mut call, out, 0, 10, "a");
        // Bytes 10..30 were evicted: the cursor jumps and a gap is shown.
        feed(&mut call, out, 30, 40, "c");
        assert_eq!(call.output, vec!["a", "…", "c"]);
        assert_eq!(call.stream_offsets, [Some(40), None]);
    }

    #[test]
    fn an_expired_chunk_is_dropped() {
        let mut call = ToolCall::new(key("c"), "bash");
        let out = ProcessStream::Stdout;
        assert!(
            call.accept_chunk(out, &chunk(out, 0, 10, true), "a".into())
                .is_none()
        );
        assert!(call.output.is_empty());
        assert_eq!(call.stream_offsets, [None, None]);
    }
}
