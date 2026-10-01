//! Local conversation export (spec §17.4). The pure parts live here: the form
//! state, the item → Markdown rendering, and the temp-file writer that the
//! owned export job drives off the main loop. No shell is ever executed and no
//! directory is created implicitly: the target's parent must already exist.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::protocol::read::{
    RawHistoryItem, ReadChunk, RuntimeAssistantPart, RuntimeItem, RuntimeUserKind,
};
use crate::safe_text::safe_display;

/// The raw-item ceiling above which a read yields a placeholder instead of a
/// body (spec §6.2). Such an item is never assembled for export either: the
/// file records an explicit limitation instead of a partial body.
pub const EXPORT_OVERSIZED_NOTE: &str = "oversized history item: not exported";

/// Which optional parts an export includes. Both default to `false`: the
/// default export is the saved conversation the user already sees.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExportSpec {
    pub include_thinking: bool,
    pub include_tool: bool,
    /// Stream items above the automatic decode ceiling as their raw sanitized
    /// Runtime JSON chunks instead of writing a placeholder (spec §17.4). The
    /// 8 MiB automatic decode ceiling itself is never raised: a raw item is
    /// never typed-decoded, only byte-verified and copied.
    pub raw_oversized: bool,
}

/// Everything that keeps an export from claiming to be the complete
/// conversation (spec §17.4). The header states each one that happened.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExportLimitations {
    pub records_truncated: bool,
    pub read_failed: usize,
    pub oversized_items: usize,
    pub opaque_parts: usize,
    pub unsaved_turns: usize,
    /// Items above the auto-decode ceiling that were streamed verbatim as raw
    /// sanitized Runtime JSON (never typed-decoded).
    pub raw_items: usize,
    /// A raw item whose chunk byte count/offset/complete did not agree: its
    /// bytes are not claimed to be a complete item.
    pub raw_mismatched: usize,
    /// The pinned read chain stopped before the captured end because a page
    /// failed validation. Later items may be missing.
    pub read_stopped: bool,
}

impl ExportLimitations {
    pub fn is_partial(&self) -> bool {
        self.records_truncated
            || self.read_failed > 0
            || self.oversized_items > 0
            || self.opaque_parts > 0
            || self.raw_mismatched > 0
            || self.read_stopped
    }

    /// One note per limitation, in a stable order.
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.records_truncated {
            notes.push(
                "partial: the runtime truncated this session's turn records; \
                 tool/usage metadata may be missing"
                    .to_owned(),
            );
        }
        if self.read_failed > 0 {
            notes.push(format!(
                "partial: {} history item(s) could not be read and are not in this file",
                self.read_failed
            ));
        }
        if self.oversized_items > 0 {
            notes.push(format!(
                "partial: {} oversized history item(s) were left as placeholders \
                 (the export never assembles an unbounded item)",
                self.oversized_items
            ));
        }
        if self.opaque_parts > 0 {
            notes.push(format!(
                "partial: {} provider-only part(s) (encrypted reasoning or opaque \
                 provider payloads) were omitted",
                self.opaque_parts
            ));
        }
        if self.raw_items > 0 {
            notes.push(format!(
                "raw: {} item(s) above the {}-byte auto-decode ceiling were written as \
                 raw sanitized Runtime JSON chunks; they were never typed-decoded and \
                 carry no provider-opaque (encrypted/signature) data",
                self.raw_items,
                crate::protocol::read::MAX_AUTO_ITEM_BYTES
            ));
        }
        if self.raw_mismatched > 0 {
            notes.push(format!(
                "partial: {} raw item(s) had a chunk byte/offset/complete mismatch and are \
                 not claimed to be complete",
                self.raw_mismatched
            ));
        }
        if self.read_stopped {
            notes.push(
                "partial: the pinned read chain stopped early because a page failed \
                 validation; later history items may be missing"
                    .to_owned(),
            );
        }
        if self.unsaved_turns > 0 {
            notes.push(format!(
                "unconfirmed: {} live turn(s) that are not in the saved history were \
                 appended at the user's explicit request",
                self.unsaved_turns
            ));
        }
        notes
    }
}

/// The export panel phase. `Editing` owns the target input; `Cancelling` is a
/// real in-flight state: a cancel was requested but the owned job has not yet
/// reported whether it committed, aborted, or failed (spec §17.4). Every other
/// phase is read-only feedback for the one owned job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportPhase {
    Editing,
    Running,
    Cancelling,
    Done,
    Failed,
}

/// The typed, final outcome of the one owned job. The App keeps this so the
/// UI can distinguish a committed file from a cancelled one and from an
/// unknown target state, instead of inferring it from a stale notice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportCompletion {
    /// The file was committed to this exact target.
    Committed {
        target: String,
        bytes: usize,
        items: usize,
    },
    /// The job was cancelled; the temp file was removed and nothing committed.
    Cancelled { target: String },
    /// The target existed at commit time and was not replaced. Nothing was
    /// written; the form stays editable.
    TargetExists { target: String },
    /// The job failed. `temp_removed` is the removal fact; `target_unknown`
    /// means a commit attempt could not prove the target was untouched.
    Failed {
        target: String,
        error: String,
        temp_removed: bool,
        target_unknown: bool,
    },
}

/// The small export form (spec §17.4): a local target plus the optional
/// content choices. `include_unsaved` is an explicit, separate choice; saved
/// history is the default source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportFormState {
    pub target: String,
    /// UTF-8 byte insertion position in the local target field.
    pub target_cursor: usize,
    pub spec: ExportSpec,
    pub include_unsaved: bool,
    pub overwrite: bool,
    pub phase: ExportPhase,
    pub notice: Option<String>,
    /// The typed final outcome of the last owned job, if any. This is the
    /// authority for committed/cancelled/unknown, not the notice string.
    pub completion: Option<ExportCompletion>,
    pub limitations: ExportLimitations,
    pub items: usize,
    pub bytes: usize,
}

impl ExportFormState {
    pub fn new(target: String) -> Self {
        Self {
            target_cursor: target.len(),
            target,
            spec: ExportSpec::default(),
            include_unsaved: false,
            overwrite: false,
            phase: ExportPhase::Editing,
            notice: None,
            completion: None,
            limitations: ExportLimitations::default(),
            items: 0,
            bytes: 0,
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.phase, ExportPhase::Running | ExportPhase::Cancelling)
    }
}

/// Validates the typed target without touching the file system. An empty or
/// directory-only spelling is refused here; a missing parent directory is
/// reported by the job that actually opens the file.
pub fn validate_target(target: &str) -> Result<PathBuf, String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err("enter a file path to export to".to_owned());
    }
    if trimmed.contains('\0') {
        return Err("the target path contains a NUL byte".to_owned());
    }
    let path = PathBuf::from(trimmed);
    if path.file_name().is_none() {
        return Err("the target must name a file, not a directory".to_owned());
    }
    Ok(path)
}

/// One rendered history item. `opaque_parts` counts provider-only parts that
/// were deliberately dropped: they are never written verbatim.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExportItemText {
    pub markdown: String,
    pub opaque_parts: usize,
}

/// Renders one raw runtime item as Markdown. Only fields the TUI itself
/// displays are written: `encrypted`/`signature` reasoning fields and unknown
/// provider payloads are never exported.
pub fn item_markdown(item: &RawHistoryItem, spec: ExportSpec) -> ExportItemText {
    let mut out = ExportItemText::default();
    match &item.item {
        RuntimeItem::User(user) => {
            let kind = match user.kind {
                RuntimeUserKind::Prompt => "prompt",
                RuntimeUserKind::Steering => "steering",
            };
            push_heading(&mut out.markdown, 2, &format!("User ({kind})"));
            push_body(&mut out.markdown, &user.input.text);
        }
        RuntimeItem::Assistant(assistant) => {
            let heading = format!(
                "Assistant ({}){}",
                assistant.model,
                if assistant.finish_reason.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", assistant.finish_reason)
                }
            );
            push_heading(&mut out.markdown, 2, &heading);
            for part in &assistant.content {
                match part {
                    RuntimeAssistantPart::Text(text) => push_body(&mut out.markdown, text),
                    RuntimeAssistantPart::Reasoning {
                        text,
                        summary,
                        encrypted,
                        signature,
                    } => {
                        if encrypted.is_some() || signature.is_some() {
                            out.opaque_parts += 1;
                        }
                        if !spec.include_thinking {
                            continue;
                        }
                        let body = text.as_deref().or(summary.as_deref());
                        let Some(body) = body else {
                            continue;
                        };
                        push_heading(&mut out.markdown, 3, "Thinking");
                        push_body(&mut out.markdown, body);
                    }
                    RuntimeAssistantPart::ToolCall {
                        name, arguments, ..
                    } => {
                        if !spec.include_tool {
                            continue;
                        }
                        push_heading(&mut out.markdown, 3, &format!("Tool call: {name}"));
                        let rendered = format_json(arguments);
                        push_fence(&mut out.markdown, "json", &rendered);
                    }
                }
            }
        }
        RuntimeItem::ToolResult(result) => {
            if !spec.include_tool {
                return out;
            }
            push_heading(
                &mut out.markdown,
                3,
                &format!("Tool result: {} ({})", result.tool_name, result.outcome),
            );
            let body = tool_result_body(result);
            push_body(&mut out.markdown, &body);
        }
        RuntimeItem::Summary(summary) => {
            push_heading(&mut out.markdown, 2, "Summary");
            push_body(&mut out.markdown, &summary.content);
        }
    }
    out
}

/// The live (unsaved) turn the user explicitly chose to append. It is written
/// with its own `unconfirmed` heading so it can never be mistaken for saved
/// history.
pub fn unsaved_markdown(blocks: &[(String, String)]) -> String {
    let mut text = String::new();
    push_heading(&mut text, 2, "Unconfirmed live turn (not in saved history)");
    for (label, body) in blocks {
        push_heading(&mut text, 3, label);
        push_body(&mut text, body);
    }
    text
}

/// The file header: where the content came from and what is missing. An
/// unsaved live turn changes the file's very first statement, so an
/// `unconfirmed`/`possibly incomplete` marker is visible at the top of the
/// file rather than only in the trailing notes (spec §17.4).
pub fn header_notes(source: &str, limitations: &ExportLimitations) -> Vec<String> {
    let mut notes = vec![format!("source: {source}")];
    let unsaved_requested = source.contains("explicitly appended live turns");
    if limitations.unsaved_turns > 0 {
        notes.push(format!(
            "UNCONFIRMED / POSSIBLY INCOMPLETE: this file appends {} live turn(s) that are \
             not in the saved history; their save status is unconfirmed",
            limitations.unsaved_turns
        ));
    } else if unsaved_requested {
        notes.push(
            "UNCONFIRMED / POSSIBLY INCOMPLETE: explicit live-turn inclusion was requested; \
             any appended turn is not confirmed saved"
                .to_owned(),
        );
    }
    notes.extend(limitations.notes());
    notes
}

/// The trailing notes section appended when any limitation happened. The
/// header is written before the read chain knows what it will find, so the
/// honest limitations are recorded at the end of the file.
pub fn limitations_text(notes: &[String]) -> String {
    let mut text = String::new();
    push_heading(&mut text, 2, "Export notes");
    for note in notes {
        text.push_str("- ");
        text.push_str(&safe_display(note));
        text.push('\n');
    }
    text.push('\n');
    text
}

/// Renders the Markdown file header from already-composed notes.
pub fn header_text(notes: &[String]) -> String {
    let mut text = String::new();
    push_heading(&mut text, 1, "Conversation export");
    for note in notes {
        text.push_str("- ");
        text.push_str(&safe_display(note));
        text.push('\n');
    }
    text.push('\n');
    text
}

fn push_heading(out: &mut String, level: usize, text: &str) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&"#".repeat(level));
    out.push(' ');
    out.push_str(&safe_inline(text));
    out.push_str("\n\n");
}

fn push_body(out: &mut String, text: &str) {
    let text = safe_display(text);
    let trimmed = text.trim_matches('\n');
    if trimmed.is_empty() {
        return;
    }
    out.push_str(trimmed);
    out.push_str("\n\n");
}

fn push_fence(out: &mut String, language: &str, body: &str) {
    out.push_str("```");
    out.push_str(language);
    out.push('\n');
    out.push_str(body.trim_end_matches('\n'));
    out.push_str("\n```\n\n");
}

/// A heading never contains a newline or control character.
fn safe_inline(text: &str) -> String {
    safe_display(text)
        .chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch })
        .collect()
}

/// A tool result body: its text when present, otherwise an explicit marker for
/// a body the runtime did not return. Never the raw provider envelope.
fn tool_result_body(result: &crate::protocol::read::RuntimeToolResultItem) -> String {
    let content = result.output.content.as_str();
    if content.is_empty() {
        "(no textual result was returned)".to_owned()
    } else {
        content.to_owned()
    }
}

/// Pretty-prints a structured tool argument value. The value comes from the
/// sanitized protocol DTO; opaque provider blobs never reach this point.
fn format_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Why the writer could not start. `TargetExists` is the explicit-overwrite
/// gate: the caller re-runs with `overwrite = true` only after the user
/// confirms.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportStartError {
    TargetExists,
    Io(String),
}

/// Why the atomic commit failed. `TargetExists` is the no-clobber race: the
/// target appeared after the export began, so the existing file was not
/// replaced (spec §17.4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportCommitError {
    TargetExists,
    Io {
        error: String,
        temp_removed: bool,
        target_state_unknown: bool,
    },
}

/// The heading written before a raw oversized item's verbatim chunks.
pub fn raw_item_start_text(index: usize, total_bytes: usize) -> String {
    format!(
        "### Raw item {index} — sanitized Runtime JSON, {total_bytes} bytes\n\n\
         The bytes below are the item's canonical JSON as returned by the Agent; they were \
         streamed verbatim and never typed-decoded. Provider-opaque fields (encrypted \
         reasoning / signatures) were already removed by the Agent's sanitizer.\n\n```json\n"
    )
}

/// Closes the raw item block, recording a chunk byte/offset/complete mismatch
/// instead of claiming a complete item.
pub fn raw_item_end_text(index: usize, complete: bool) -> String {
    if complete {
        "\n```\n\n".to_owned()
    } else {
        format!(
            "\n<!-- raw item {index} is NOT complete: its chunk byte/offset/complete data did \
             not agree -->\n```\n\n"
        )
    }
}

/// Verifies one raw oversized item's chunks without ever decoding them. It
/// enforces the same `utf8_json` encoding, monotonic offsets and declared
/// `total_bytes` as the bounded assembler, but keeps no body: the caller
/// forwards each verified chunk straight to the writer (spec §17.4).
#[derive(Debug, Default)]
pub struct RawItemStream {
    pub index: usize,
    pub total_bytes: usize,
    pub next_offset: usize,
    pub complete: bool,
    pub mismatch: bool,
}

impl RawItemStream {
    pub fn start(index: usize, total_bytes: usize) -> Self {
        Self {
            index,
            total_bytes,
            next_offset: 0,
            complete: false,
            mismatch: false,
        }
    }

    /// Accepts one chunk. `Ok(true)` means the item is complete; `Err` records
    /// a real mismatch so the file never claims a complete body it does not
    /// have.
    pub fn push(&mut self, chunk: &ReadChunk) -> Result<bool, String> {
        if chunk.encoding != "utf8_json" {
            self.mismatch = true;
            return Err(format!(
                "raw item {} used encoding '{}', expected utf8_json",
                chunk.index, chunk.encoding
            ));
        }
        if chunk.index != self.index {
            self.mismatch = true;
            return Err(format!(
                "raw item {} received chunk for item {}",
                self.index, chunk.index
            ));
        }
        if chunk.total_bytes != self.total_bytes {
            self.mismatch = true;
            return Err(format!(
                "raw item {} declared {} bytes, chunk says {}",
                self.index, self.total_bytes, chunk.total_bytes
            ));
        }
        if chunk.offset != self.next_offset {
            self.mismatch = true;
            return Err(format!(
                "raw item {} expected offset {}, chunk starts at {}",
                self.index, self.next_offset, chunk.offset
            ));
        }
        // `data` is already the outer JSON-decoded string, so its `len()` is
        // the canonical item's UTF-8 byte length (spec §6.2).
        let delivered = chunk.data.len();
        self.next_offset = self.next_offset.saturating_add(delivered);
        if self.next_offset > self.total_bytes {
            self.mismatch = true;
            return Err(format!(
                "raw item {} delivered {} bytes, declared {}",
                self.index, self.next_offset, self.total_bytes
            ));
        }
        if chunk.complete {
            if self.next_offset != self.total_bytes {
                self.mismatch = true;
                return Err(format!(
                    "raw item {} ended at {} bytes, declared {}",
                    self.index, self.next_offset, self.total_bytes
                ));
            }
            self.complete = true;
            return Ok(true);
        }
        Ok(false)
    }
}

/// One owned Markdown writer. All file I/O happens on the export job's thread;
/// a uniquely named temp file is created in the target's parent directory and
/// only moved onto the target after a successful flush and sync. The default
/// commit uses an atomic **no-clobber** move, so a target created while the
/// export was running is never replaced without the explicit confirmation
/// (spec §17.4).
pub struct ExportWriter {
    target: PathBuf,
    temp: Option<tempfile::NamedTempFile>,
    file: Option<std::io::BufWriter<std::fs::File>>,
    overwrite: bool,
    bytes: usize,
}

impl std::fmt::Debug for ExportWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExportWriter")
            .field("target", &self.target.display().to_string())
            .field(
                "temp",
                &self
                    .temp
                    .as_ref()
                    .map(|temp| temp.path().display().to_string()),
            )
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl ExportWriter {
    pub fn create(target: &Path, overwrite: bool) -> Result<Self, ExportStartError> {
        // A definite pre-flight refusal: no temp file is even created. The
        // commit enforces the same rule atomically, so this is only a fast
        // path and the race is closed later.
        if !overwrite {
            match std::fs::symlink_metadata(target) {
                Ok(_) => return Err(ExportStartError::TargetExists),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ExportStartError::Io(format!(
                        "cannot inspect {}: {error}",
                        target.display()
                    )));
                }
            }
        }
        let name = target
            .file_name()
            .ok_or_else(|| {
                ExportStartError::Io("the target must name a file, not a directory".to_owned())
            })?
            .to_string_lossy()
            .into_owned();
        // The target's own parent keeps the move atomic (same filesystem) and
        // never creates a directory implicitly. A unique random name avoids the
        // stale/fixed-pid collision the previous spelling allowed.
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temp = tempfile::Builder::new()
            .prefix(&format!(".{name}."))
            .suffix(".part")
            .tempfile_in(parent)
            .map_err(|error| {
                ExportStartError::Io(format!(
                    "cannot create a temporary file next to {}: {error}",
                    target.display()
                ))
            })?;
        let file = temp.reopen().map_err(|error| {
            ExportStartError::Io(format!("cannot open the temporary file: {error}"))
        })?;
        Ok(Self {
            target: target.to_path_buf(),
            temp: Some(temp),
            file: Some(std::io::BufWriter::new(file)),
            overwrite,
            bytes: 0,
        })
    }

    pub fn write(&mut self, text: &str) -> std::io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Err(std::io::Error::other("the export writer is closed"));
        };
        file.write_all(text.as_bytes())?;
        self.bytes += text.len();
        Ok(())
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn temp_path(&self) -> Option<&Path> {
        self.temp.as_ref().map(|temp| temp.path())
    }

    /// Flushes, syncs and atomically moves the temp file onto the target. The
    /// default path refuses to replace a file that appeared after the export
    /// started; the explicit overwrite path replaces exactly this target.
    pub fn finish(mut self) -> Result<PathBuf, ExportCommitError> {
        let Some(writer) = self.file.take() else {
            return Err(ExportCommitError::Io {
                error: "the export writer is closed".to_owned(),
                temp_removed: self.remove_temp(),
                target_state_unknown: false,
            });
        };
        let file = match writer.into_inner() {
            Ok(file) => file,
            Err(error) => {
                return Err(ExportCommitError::Io {
                    error: format!("cannot flush: {error}"),
                    temp_removed: self.remove_temp(),
                    target_state_unknown: false,
                });
            }
        };
        if let Err(error) = file.sync_all() {
            drop(file);
            return Err(ExportCommitError::Io {
                error: format!("cannot sync: {error}"),
                temp_removed: self.remove_temp(),
                target_state_unknown: false,
            });
        }
        drop(file);
        let Some(temp) = self.temp.take() else {
            return Err(ExportCommitError::Io {
                error: "the temporary file is already gone".to_owned(),
                temp_removed: true,
                target_state_unknown: false,
            });
        };
        let target = self.target.clone();
        let result = if self.overwrite {
            temp.persist(&target)
        } else {
            temp.persist_noclobber(&target)
        };
        match result {
            Ok(_) => Ok(target),
            Err(error) => {
                let kind = error.error.kind();
                let detail = error.error.to_string();
                let temp_removed = error.file.close().is_ok();
                if !self.overwrite && kind == std::io::ErrorKind::AlreadyExists {
                    if temp_removed {
                        Err(ExportCommitError::TargetExists)
                    } else {
                        Err(ExportCommitError::Io {
                            error: format!(
                                "target {} already exists; could not confirm temporary cleanup: {detail}",
                                target.display()
                            ),
                            temp_removed: false,
                            target_state_unknown: false,
                        })
                    }
                } else {
                    Err(ExportCommitError::Io {
                        error: format!(
                            "cannot move the temporary file onto {}: {detail}",
                            target.display()
                        ),
                        temp_removed,
                        target_state_unknown: true,
                    })
                }
            }
        }
    }

    fn remove_temp(&mut self) -> bool {
        let Some(temp) = self.temp.take() else {
            return true;
        };
        temp.close().is_ok()
    }

    /// Removes the uncommitted temp file. `Err` means removal could not be
    /// confirmed: an unknown I/O state is never reported as rolled back.
    pub fn abort(mut self) -> Result<(), String> {
        self.file.take();
        let Some(temp) = self.temp.take() else {
            return Ok(());
        };
        let path = temp.path().to_path_buf();
        temp.close()
            .map_err(|error| format!("could not remove {}: {error}", path.display()))
    }

    /// The target this writer would move onto.
    pub fn target(&self) -> &Path {
        &self.target
    }
}

impl Drop for ExportWriter {
    fn drop(&mut self) {
        // A writer dropped without `finish`/`abort` (e.g. a panic) must not
        // leave its temp file behind. A committed writer already took the temp.
        self.file.take();
        self.temp.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::{RuntimeAssistantItem, RuntimeSummaryItem, RuntimeUserInput};

    fn user(text: &str) -> RawHistoryItem {
        RawHistoryItem {
            item: RuntimeItem::User(crate::protocol::read::RuntimeUserItem {
                loop_id: "loop".to_owned(),
                kind: RuntimeUserKind::Prompt,
                input: RuntimeUserInput {
                    text: text.to_owned(),
                },
            }),
            timestamp: None,
        }
    }

    fn assistant(parts: Vec<RuntimeAssistantPart>) -> RawHistoryItem {
        RawHistoryItem {
            item: RuntimeItem::Assistant(RuntimeAssistantItem {
                loop_id: "loop".to_owned(),
                request_index: 0,
                model: "model-x".to_owned(),
                reasoning: None,
                content: parts,
                finish_reason: "stop".to_owned(),
                usage: Default::default(),
            }),
            timestamp: None,
        }
    }

    #[test]
    fn default_items_export_text_without_thinking_or_tools() {
        let item = assistant(vec![
            RuntimeAssistantPart::Reasoning {
                text: Some("private thoughts".to_owned()),
                summary: None,
                encrypted: Some("opaque-blob".to_owned()),
                signature: None,
            },
            RuntimeAssistantPart::Text("visible answer".to_owned()),
            RuntimeAssistantPart::ToolCall {
                tool_call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
                call_index: 0,
            },
        ]);
        let text = item_markdown(&item, ExportSpec::default());
        assert!(text.markdown.contains("visible answer"));
        assert!(!text.markdown.contains("private thoughts"));
        assert!(!text.markdown.contains("read_file"));
        // The encrypted reasoning part is provider-only data: it is counted but
        // never written.
        assert!(!text.markdown.contains("opaque-blob"));
        assert_eq!(text.opaque_parts, 1);
    }

    #[test]
    fn optional_parts_are_included_only_when_requested() {
        let item = assistant(vec![
            RuntimeAssistantPart::Reasoning {
                text: Some("reasoned".to_owned()),
                summary: None,
                encrypted: None,
                signature: None,
            },
            RuntimeAssistantPart::ToolCall {
                tool_call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
                call_index: 0,
            },
        ]);
        let text = item_markdown(
            &item,
            ExportSpec {
                include_thinking: true,
                include_tool: true,
                raw_oversized: false,
            },
        );
        assert!(text.markdown.contains("### Thinking"));
        assert!(text.markdown.contains("reasoned"));
        assert!(text.markdown.contains("### Tool call: read_file"));
        assert!(text.markdown.contains("\"path\": \"src/main.rs\""));
        assert_eq!(text.opaque_parts, 0);
    }

    #[test]
    fn headers_name_the_source_and_every_limitation() {
        let limitations = ExportLimitations {
            records_truncated: true,
            read_failed: 2,
            oversized_items: 1,
            opaque_parts: 3,
            unsaved_turns: 1,
            raw_items: 1,
            raw_mismatched: 0,
            read_stopped: false,
        };
        let notes = header_notes("saved history (session_read snapshot)", &limitations);
        let text = header_text(&notes);
        assert!(text.contains("source: saved history"));
        assert!(text.contains("partial: the runtime truncated"));
        assert!(text.contains("2 history item(s)"));
        assert!(text.contains("1 oversized history item(s)"));
        assert!(text.contains("3 provider-only part(s)"));
        assert!(text.contains("raw: 1 item(s)"));
        assert!(text.contains("unconfirmed: 1 live turn(s)"));
    }

    /// An unsaved live turn marks the file unconfirmed at the very top, not
    /// only in the trailing notes (spec §17.4).
    #[test]
    fn an_unsaved_turn_marks_the_header_unconfirmed() {
        let limitations = ExportLimitations {
            unsaved_turns: 1,
            ..ExportLimitations::default()
        };
        let notes = header_notes(
            "saved history plus explicitly appended live turns",
            &limitations,
        );
        let text = header_text(&notes);
        let unconfirmed = text
            .find("UNCONFIRMED / POSSIBLY INCOMPLETE")
            .expect("the header names the unsaved turn");
        assert!(unconfirmed < 200, "the marker is in the file header");
    }

    #[test]
    fn a_complete_export_claims_no_limitation() {
        assert!(!ExportLimitations::default().is_partial());
        assert!(ExportLimitations::default().notes().is_empty());
    }

    #[test]
    fn target_validation_refuses_empty_and_directory_spellings() {
        assert!(validate_target("   ").is_err());
        assert!(validate_target("/").is_err());
        assert_eq!(
            validate_target(" out/chat.md ").unwrap(),
            PathBuf::from("out/chat.md")
        );
    }

    #[test]
    fn the_writer_refuses_an_existing_target_until_overwrite_is_confirmed() {
        let dir = std::env::temp_dir().join(format!("mctui-export-unit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("chat.md");
        let _ = std::fs::remove_file(&target);
        std::fs::write(&target, "old").unwrap();
        assert_eq!(
            ExportWriter::create(&target, false).err(),
            Some(ExportStartError::TargetExists)
        );
        let mut writer = ExportWriter::create(&target, true).expect("overwrite admitted");
        writer.write("new body").unwrap();
        writer.finish().expect("explicit replace");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new body");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The parent-review race: the target is created after the writer's
    /// pre-flight check. The default commit must refuse to replace it and must
    /// leave the pre-existing file untouched (spec §17.4).
    #[test]
    fn a_target_created_after_create_is_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("mctui-export-race-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("chat.md");
        let _ = std::fs::remove_file(&target);
        let mut writer = ExportWriter::create(&target, false).expect("temp only");
        writer.write("export body").unwrap();
        // The race: another writer creates the target while this export runs.
        std::fs::write(&target, "created during export").unwrap();
        assert_eq!(writer.finish().err(), Some(ExportCommitError::TargetExists));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "created during export",
            "the default commit is a no-clobber commit"
        );
        // The refused temp file was removed.
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "chat.md")
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A temp name is unique per writer: two exports targeting the same file
    /// never collide on a stale pid-derived name.
    #[test]
    fn two_writers_for_one_target_use_distinct_temp_files() {
        let dir = std::env::temp_dir().join(format!("mctui-export-temp-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("chat.md");
        let first = ExportWriter::create(&target, false).expect("first temp");
        let second = ExportWriter::create(&target, false).expect("second temp");
        assert_ne!(first.temp_path(), second.temp_path());
        assert!(first.temp_path().is_some_and(|path| path.exists()));
        assert!(second.temp_path().is_some_and(|path| path.exists()));
        let _ = first.abort();
        let _ = second.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abort_removes_the_uncommitted_temp_and_keeps_the_target() {
        let dir = std::env::temp_dir().join(format!("mctui-export-abort-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("chat.md");
        let _ = std::fs::remove_file(&target);
        let mut writer = ExportWriter::create(&target, false).expect("temp");
        let temp = writer.temp_path().expect("temp path").to_path_buf();
        writer.write("half").unwrap();
        writer.abort().expect("temp removed");
        assert!(!temp.exists());
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A writer dropped without `finish`/`abort` (a panic path) still removes
    /// its uncommitted temp file.
    #[test]
    fn a_dropped_writer_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("mctui-export-drop-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("chat.md");
        let temp = {
            let mut writer = ExportWriter::create(&target, false).expect("temp");
            writer.write("half").unwrap();
            writer.temp_path().expect("temp path").to_path_buf()
        };
        assert!(!temp.exists(), "Drop removes the uncommitted temp");
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_parent_directory_is_reported_without_creating_it() {
        let dir = std::env::temp_dir().join(format!("mctui-export-missing-{}", std::process::id()));
        let target = dir.join("nested").join("chat.md");
        let error = ExportWriter::create(&target, false).expect_err("no implicit mkdir");
        assert!(matches!(error, ExportStartError::Io(_)));
        assert!(!dir.exists());
    }

    #[test]
    fn raw_item_stream_verifies_bytes_without_decoding() {
        let json = "{\"item\":{\"type\":\"summary\",\"data\":{\"content\":\"x\"}}}";
        let total = json.len();
        let mut stream = RawItemStream::start(4, total);
        let head = &json[..10];
        let head_chunk = ReadChunk {
            index: 4,
            offset: 0,
            total_bytes: total,
            encoding: "utf8_json".to_owned(),
            data: head.to_owned(),
            complete: false,
        };
        assert_eq!(stream.push(&head_chunk), Ok(false));
        let tail = ReadChunk {
            index: 4,
            offset: 10,
            total_bytes: total,
            encoding: "utf8_json".to_owned(),
            data: json[10..].to_owned(),
            complete: true,
        };
        assert_eq!(stream.push(&tail), Ok(true));
        assert!(stream.complete && !stream.mismatch);
        assert_eq!(stream.next_offset, total);
    }

    #[test]
    fn raw_item_stream_records_a_real_mismatch() {
        let mut stream = RawItemStream::start(0, 100);
        let wrong_offset = ReadChunk {
            index: 0,
            offset: 5,
            total_bytes: 100,
            encoding: "utf8_json".to_owned(),
            data: "x".to_owned(),
            complete: false,
        };
        assert!(stream.push(&wrong_offset).is_err());
        assert!(stream.mismatch);
        // A short complete chunk that does not reach total_bytes also mismatches.
        let mut short = RawItemStream::start(0, 100);
        let complete = ReadChunk {
            index: 0,
            offset: 0,
            total_bytes: 100,
            encoding: "utf8_json".to_owned(),
            data: "x".to_owned(),
            complete: true,
        };
        assert!(short.push(&complete).is_err());
        assert!(short.mismatch);
    }

    #[test]
    fn summary_items_export_their_text() {
        let item = RawHistoryItem {
            item: RuntimeItem::Summary(RuntimeSummaryItem {
                content: "condensed".to_owned(),
            }),
            timestamp: None,
        };
        assert!(
            item_markdown(&item, ExportSpec::default())
                .markdown
                .contains("condensed")
        );
    }

    #[test]
    fn a_user_prompt_renders_its_text_under_a_user_heading() {
        let text = item_markdown(&user("hello there"), ExportSpec::default());
        assert!(text.markdown.contains("## User (prompt)"));
        assert!(text.markdown.contains("hello there"));
    }
}
