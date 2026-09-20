//! `/copy`: selection, current message, code block and the last completed
//! reply (spec §17.3).
//!
//! Copy always reuses the prepared conversation's copy metadata, so the text
//! has real hard newlines, never a soft-wrap newline, and never Rail or
//! timing decorations. A copy is local: it never issues a remote read to
//! "complete" a message, and content that is not loaded reports its
//! limitation instead of silently copying a placeholder.

use super::*;
use crate::state::view::{SectionKind, SectionView};

/// The concrete text one copy produces, or why it could not.
pub(super) enum CopyPlan {
    Text(String),
    Limitation(String),
}

impl App {
    pub(super) fn copy_command(&mut self, target: crate::command::CopyTarget) -> Vec<AppCommand> {
        use crate::command::CopyTarget;
        let plan = match target {
            CopyTarget::Selection => {
                if self.selection.is_none() {
                    self.notice(NoticeLevel::Info, "no conversation selection to copy");
                    return Vec::new();
                }
                return self.copy_selection_command();
            }
            CopyTarget::LastReply => self.plan_last_reply_copy(),
            CopyTarget::Message => self.plan_section_copy(SectionPick::ViewportOrSelection),
            CopyTarget::Code => self.plan_code_copy(),
        };
        match plan {
            CopyPlan::Text(text) => {
                if text.is_empty() {
                    self.notice(NoticeLevel::Info, "nothing to copy in this view");
                    return Vec::new();
                }
                vec![self.capture_copy(text)]
            }
            CopyPlan::Limitation(detail) => {
                self.notice(NoticeLevel::Info, detail);
                Vec::new()
            }
        }
    }

    /// The last completed Assistant reply's visible text, excluding Thinking.
    fn plan_last_reply_copy(&self) -> CopyPlan {
        let width = self.terminal_content_width();
        let prepared = self.conversation_for_input(width);
        let sections = prepared.sections.iter().collect::<Vec<_>>();
        let Some(section) = sections
            .iter()
            .rev()
            .find(|section| section.id.kind == SectionKind::AssistantText)
        else {
            return CopyPlan::Limitation(
                "no completed reply is loaded; scroll or /export to read more".to_owned(),
            );
        };
        if section.folded {
            return CopyPlan::Limitation(
                "the last reply is folded; expand it before copying".to_owned(),
            );
        }
        CopyPlan::Text(section_copy_text(&prepared, section))
    }

    /// The section the user is looking at: the selection anchor when there is
    /// one, otherwise the first content row of the viewport.
    fn plan_section_copy(&self, pick: SectionPick) -> CopyPlan {
        let width = self.terminal_content_width();
        let prepared = self.conversation_for_input(width);
        let Some(section) = self.pick_section(&prepared, pick) else {
            return CopyPlan::Limitation(
                "no message under the cursor; select text first or scroll the conversation"
                    .to_owned(),
            );
        };
        if self.section_is_placeholder(&prepared, &section) {
            return CopyPlan::Limitation(
                "that message is a large item placeholder; /export it explicitly to read it"
                    .to_owned(),
            );
        }
        if section.id.kind == SectionKind::Summary && section.collapsible {
            return self
                .section_source_text(&section)
                .map(CopyPlan::Text)
                .unwrap_or_else(|| {
                    CopyPlan::Limitation(
                        "summary source is not loaded; /export to read it".to_owned(),
                    )
                });
        }
        CopyPlan::Text(section_copy_text(&prepared, &section))
    }

    /// `/copy code`: the fenced block of the current message. The block source
    /// is the raw message text, so the fences are removed cleanly instead of
    /// guessing from rendered rows.
    fn plan_code_copy(&self) -> CopyPlan {
        let width = self.terminal_content_width();
        let prepared = self.conversation_for_input(width);
        let Some(section) = self.pick_section(&prepared, SectionPick::ViewportOrSelection) else {
            return CopyPlan::Limitation(
                "no message under the cursor to copy code from".to_owned(),
            );
        };
        let Some(source) = self.section_source_text(&section) else {
            return CopyPlan::Limitation(
                "that message's source is not loaded; /export it explicitly to read it".to_owned(),
            );
        };
        match extract_code_block(&source, self.selection_code_offset(&section)) {
            Some(code) if !code.is_empty() => CopyPlan::Text(code),
            Some(_) => CopyPlan::Limitation("that code block is empty".to_owned()),
            None => CopyPlan::Limitation("no fenced code block in this message".to_owned()),
        }
    }

    /// The block-level source text behind one rendered section, if it is
    /// loaded. This is the raw markdown for assistant/user/summary sections
    /// and the tool result for a tool section.
    fn section_source_text(&self, section: &SectionView) -> Option<String> {
        use crate::state::transcript::{AssistantPart, TranscriptBlock};
        let view = self.active_view()?;
        let index = section.id.history_index?;
        let block = view
            .transcript
            .blocks
            .iter()
            .find(|block| match block.as_ref() {
                TranscriptBlock::User(user) => user.index == Some(index),
                TranscriptBlock::Assistant(assistant) => assistant.index == index,
                TranscriptBlock::Tool(tool) => tool.index == Some(index),
                TranscriptBlock::Summary(summary) => summary.index == index,
                TranscriptBlock::HistoryPlaceholder(_) => false,
            })?;
        match block.as_ref() {
            TranscriptBlock::User(user) => Some(user.text.clone()),
            TranscriptBlock::Assistant(assistant) => assistant
                .parts
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(text) if section.id.kind == SectionKind::AssistantText => {
                        Some(text.as_str())
                    }
                    AssistantPart::Reasoning(text) if section.id.kind == SectionKind::Thinking => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .nth(section.id.ordinal as usize)
                .map(str::to_owned),
            TranscriptBlock::Tool(tool) => tool.result.as_deref().map(str::to_owned),
            TranscriptBlock::Summary(summary) => Some(summary.content.clone()),
            TranscriptBlock::HistoryPlaceholder(_) => None,
        }
    }

    /// The byte offset of the selection inside the picked section's source.
    fn selection_code_offset(&self, section: &SectionView) -> Option<usize> {
        let selection = self.selection.as_ref()?;
        let (start, _) = selection.ordered_points();
        if start.section_id.as_ref() != Some(&section.id) {
            return None;
        }
        Some(start.section_row)
    }

    fn pick_section(
        &self,
        prepared: &PreparedConversation,
        pick: SectionPick,
    ) -> Option<SectionView> {
        if matches!(pick, SectionPick::ViewportOrSelection) {
            if let Some(selection) = self.selection.as_ref() {
                let (start, _) = selection.ordered_points();
                if let Some(section) = prepared.section_at(start.row, start.column) {
                    return Some(section);
                }
            }
        }
        let height = self.viewport.1.max(1);
        let position = crate::ui::transcript::scroll_position(self, prepared.total_rows(), height);
        (position.offset..position.offset.saturating_add(position.visible_rows).max(1)).find_map(
            |row| {
                // A collapsed summary has only a decorative label, but still
                // owns a complete logical source for message/code copying.
                if let Some(section) = prepared.sections.iter().find(|section| {
                    section.id.kind == SectionKind::Summary
                        && section.collapsible
                        && row == section.rows.start + 1
                }) {
                    return Some(section);
                }
                let copy = prepared.copy_row(row)?;
                if copy.decorative {
                    return None;
                }
                prepared.sections.iter().find(|section| {
                    section.rows.contains(&row) && !matches!(section.id.kind, SectionKind::Notice)
                })
            },
        )
    }

    fn section_is_placeholder(
        &self,
        prepared: &PreparedConversation,
        section: &SectionView,
    ) -> bool {
        let Some(index) = section.id.history_index else {
            return false;
        };
        if self
            .active_view()
            .is_some_and(|view| view.transcript.window.large_item(index).is_some())
        {
            return true;
        }
        if section.id.kind != SectionKind::Summary {
            return false;
        }
        prepared.copy_ranges.iter().any(|copy| {
            section.rows.contains(&copy.row) && copy.text.contains("[large history item")
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SectionPick {
    /// The selection anchor first, then the viewport's first content row.
    ViewportOrSelection,
}

/// Joins one section's rendered copy rows with real hard newlines. Soft-wrapped
/// rows join directly, so a wrapped line never gains a fake newline
/// (spec §17.3).
fn section_copy_text(prepared: &PreparedConversation, section: &SectionView) -> String {
    let mut text = String::new();
    for copy in prepared.copy_ranges.iter() {
        if !section.rows.contains(&copy.row) || copy.decorative {
            continue;
        }
        text.push_str(copy.text);
        // A soft-wrapped row joins the next row directly; a logical source-line
        // end keeps its newline. An empty row is a visible blank line, so it
        // also ends a line.
        if copy.hard_break_after || copy.text.is_empty() {
            text.push('\n');
        }
    }
    text.trim_matches('\n').to_owned()
}

/// Extracts one fenced code block from raw markdown. `near` (when given)
/// selects the block containing that source offset; otherwise the last block
/// wins. Indented code blocks are not guessed from rendered rows.
fn extract_code_block(source: &str, near: Option<usize>) -> Option<String> {
    let mut blocks: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut fence: Option<(char, usize, usize, String)> = None;
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next();
        let fenced = marker
            .filter(|marker| *marker == '`' || *marker == '~')
            .map(|marker| {
                (
                    marker,
                    trimmed.chars().take_while(|ch| *ch == marker).count(),
                )
            })
            .filter(|(_, count)| *count >= 3);
        match (&mut fence, fenced) {
            (None, Some((marker, count))) => {
                let language = trimmed[count..].trim().to_owned();
                fence = Some((marker, count, start, language));
            }
            (Some((marker, count, _, _)), Some((closing, close_count)))
                if closing == *marker && close_count >= *count =>
            {
                let (_, _, start_at, _) = fence.take().expect("a fence is open");
                let body_start = source[start_at..]
                    .find('\n')
                    .map_or(offset, |index| start_at + index + 1);
                let end = start.saturating_sub(1).max(body_start);
                blocks.push((start_at..end, source[body_start..end].to_owned()));
            }
            _ => {}
        }
    }
    let selected = match near {
        Some(offset) => blocks
            .iter()
            .find(|(range, _)| range.contains(&offset))
            .or_else(|| blocks.last()),
        None => blocks.last(),
    }?;
    let text = selected.1.trim_end_matches('\n').to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::extract_code_block;

    #[test]
    fn fenced_blocks_are_extracted_without_their_fences() {
        let source = "intro\n```rust\nlet a = 1;\nlet b = 2;\n```\noutro\n";
        assert_eq!(
            extract_code_block(source, None).as_deref(),
            Some("let a = 1;\nlet b = 2;")
        );
        let source = "a\n~~~\nplain\n~~~\n```py\nprint(1)\n```\n";
        assert_eq!(
            extract_code_block(source, None).as_deref(),
            Some("print(1)")
        );
        assert_eq!(extract_code_block("no code here", None), None);
        assert_eq!(extract_code_block("```\n```", None), None);
    }

    #[test]
    fn a_source_offset_selects_the_block_that_contains_it() {
        let source = "```\nfirst\n```\nmiddle\n```\nsecond\n```\n";
        let second_offset = source.find("second").unwrap();
        assert_eq!(
            extract_code_block(source, Some(second_offset)).as_deref(),
            Some("second")
        );
        let first_offset = source.find("first").unwrap();
        assert_eq!(
            extract_code_block(source, Some(first_offset)).as_deref(),
            Some("first")
        );
        // An offset outside every block (the prose between two fences) falls
        // back to the last block.
        let middle_offset = source.find("middle").unwrap();
        assert_eq!(
            extract_code_block(source, Some(middle_offset)).as_deref(),
            Some("second")
        );
    }
}
