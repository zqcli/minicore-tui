//! Local conversation export (spec §17.4). The pure parts live here: the form
//! state, the item → Markdown rendering, and the temp-file writer that the
//! owned export job drives off the main loop. No shell is ever executed and no
//! directory is created implicitly: the target's parent must already exist.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::protocol::read::{RawHistoryItem, RuntimeAssistantPart, RuntimeItem, RuntimeUserKind};
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
}

impl ExportLimitations {
    pub fn is_partial(&self) -> bool {
        self.records_truncated
            || self.read_failed > 0
            || self.oversized_items > 0
            || self.opaque_parts > 0
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

/// The export panel phase. `Editing` owns the target input; every other phase
/// is read-only feedback for the one owned job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportPhase {
    Editing,
    Running,
    Done,
    Failed,
}

/// The small export form (spec §17.4): a local target plus the optional
/// content choices. `include_unsaved` is an explicit, separate choice; saved
/// history is the default source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportFormState {
    pub target: String,
    pub spec: ExportSpec,
    pub include_unsaved: bool,
    pub overwrite: bool,
    pub phase: ExportPhase,
    pub notice: Option<String>,
    pub limitations: ExportLimitations,
    pub items: usize,
    pub bytes: usize,
}

impl ExportFormState {
    pub fn new(target: String) -> Self {
        Self {
            target,
            spec: ExportSpec::default(),
            include_unsaved: false,
            overwrite: false,
            phase: ExportPhase::Editing,
            notice: None,
            limitations: ExportLimitations::default(),
            items: 0,
            bytes: 0,
        }
    }

    pub fn running(&self) -> bool {
        self.phase == ExportPhase::Running
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

/// The file header: where the content came from and what is missing.
pub fn header_notes(source: &str, limitations: &ExportLimitations) -> Vec<String> {
    let mut notes = vec![format!("source: {source}")];
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

/// One owned Markdown writer. All file I/O happens on the export job's thread;
/// a unique temp file is created next to the target and only renamed onto it
/// after a successful flush, so an interrupted export never replaces a
/// complete target file.
pub struct ExportWriter {
    target: PathBuf,
    temp: PathBuf,
    file: Option<std::io::BufWriter<std::fs::File>>,
    bytes: usize,
}

impl std::fmt::Debug for ExportWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExportWriter")
            .field("target", &self.target.display().to_string())
            .field("temp", &self.temp.display().to_string())
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl ExportWriter {
    pub fn create(target: &Path, overwrite: bool) -> Result<Self, ExportStartError> {
        if !overwrite && target.exists() {
            return Err(ExportStartError::TargetExists);
        }
        let name = target
            .file_name()
            .ok_or_else(|| {
                ExportStartError::Io("the target must name a file, not a directory".to_owned())
            })?
            .to_string_lossy()
            .into_owned();
        let temp = target.with_file_name(format!(".{name}.{}.part", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| {
                ExportStartError::Io(format!("cannot create {}: {error}", temp.display()))
            })?;
        Ok(Self {
            target: target.to_path_buf(),
            temp,
            file: Some(std::io::BufWriter::new(file)),
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

    pub fn temp_path(&self) -> &Path {
        &self.temp
    }

    /// Flushes, syncs and atomically renames the temp file onto the target.
    pub fn finish(mut self) -> Result<PathBuf, String> {
        let Some(file) = self.file.take() else {
            return Err("the export writer is closed".to_owned());
        };
        let file = file
            .into_inner()
            .map_err(|error| format!("cannot flush the temporary file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync the temporary file: {error}"))?;
        drop(file);
        std::fs::rename(&self.temp, &self.target).map_err(|error| {
            format!(
                "cannot move {} onto {}: {error}",
                self.temp.display(),
                self.target.display()
            )
        })?;
        Ok(self.target.clone())
    }

    /// Cancels without a rename. `Err` means the temporary file could not be
    /// confirmed removed: an unknown I/O state is never reported as rolled
    /// back.
    pub fn abort(mut self) -> Result<(), String> {
        drop(self.file.take());
        match std::fs::remove_file(&self.temp) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "could not remove the temporary file {}: {error}",
                self.temp.display()
            )),
        }
    }

    /// The target this writer would rename onto.
    pub fn target(&self) -> &Path {
        &self.target
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
        };
        let notes = header_notes("saved history (session_read snapshot)", &limitations);
        let text = header_text(&notes);
        assert!(text.contains("source: saved history"));
        assert!(text.contains("partial: the runtime truncated"));
        assert!(text.contains("2 history item(s)"));
        assert!(text.contains("1 oversized history item(s)"));
        assert!(text.contains("3 provider-only part(s)"));
        assert!(text.contains("unconfirmed: 1 live turn(s)"));
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
        writer.finish().expect("atomic rename");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new body");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn abort_removes_the_uncommitted_temp_and_keeps_the_target() {
        let dir = std::env::temp_dir().join(format!("mctui-export-abort-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("chat.md");
        let _ = std::fs::remove_file(&target);
        let mut writer = ExportWriter::create(&target, false).expect("temp");
        let temp = writer.temp_path().to_path_buf();
        writer.write("half").unwrap();
        writer.abort().expect("temp removed");
        assert!(!temp.exists());
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
