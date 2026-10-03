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
        let Some(section) = sections.iter().rev().find(|section| {
            // Live sections are provisional deltas, even when they are
            // visually below the last saved reply. Only stored history
            // qualifies for the default completed-reply target.
            section.id.kind == SectionKind::AssistantText && section.id.history_index.is_some()
        }) else {
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
        match extract_code_block(&source, self.selection_code_offset(&prepared, &section)) {
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
        let Some(index) = section.id.history_index else {
            let live = view.live.as_ref()?;
            if live.reference.as_ref()?.loop_id.as_str() != section.id.loop_id.as_deref()? {
                return None;
            }
            let request = live
                .requests
                .iter()
                .find(|request| Some(request.request_index) == section.id.request_index)?;
            return request
                .parts
                .iter()
                .filter_map(|part| match part {
                    crate::state::turn::LivePart::Text(text)
                        if section.id.kind == SectionKind::AssistantText =>
                    {
                        Some(text.as_str())
                    }
                    crate::state::turn::LivePart::Reasoning(text)
                        if section.id.kind == SectionKind::Thinking =>
                    {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .nth(section.id.ordinal as usize)
                .map(str::to_owned);
        };
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
    fn selection_code_offset(
        &self,
        prepared: &PreparedConversation,
        section: &SectionView,
    ) -> Option<usize> {
        let selection = self.selection.as_ref()?;
        let (start, _) = selection.ordered_points();
        if start.section_id.as_ref() != Some(&section.id) {
            return None;
        }
        // Renderer metadata is in source bytes; section_row is only a visual
        // row and changes with width, Markdown framing and multibyte text.
        let copy = prepared.copy_row(start.row)?;
        (!copy.decorative).then_some(copy.source_offset)
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
    // User cards own a timestamp immediately before their closing blank.
    // A live applied-steer card has one additional receipt marker. Explicit
    // selection may copy the timestamp, but message-source copying must not.
    let timestamp_row = (section.id.kind == SectionKind::User).then(|| {
        let live_card = prepared
            .sections
            .live
            .iter()
            .any(|live| live.id == section.id);
        section
            .rows
            .end
            .saturating_sub(if live_card { 3 } else { 2 })
    });
    let mut text = String::new();
    for copy in prepared.copy_ranges.iter() {
        if !section.rows.contains(&copy.row) || copy.decorative || timestamp_row == Some(copy.row) {
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
    use super::{CopyPlan, extract_code_block, section_copy_text};

    #[test]
    fn message_copy_omits_timestamp_but_explicit_selection_keeps_it() {
        use crate::state::view::{
            ConversationSelection, SectionKind, SelectionGranularity, SelectionPoint,
        };
        let app = crate::ui::testapp::chat(crate::theme::ThemeKind::Dark);
        let prepared = crate::ui::transcript::prepare_conversation(&app, 77);
        let user = prepared
            .sections
            .iter()
            .find(|section| section.id.kind == SectionKind::User)
            .unwrap();
        assert_eq!(
            section_copy_text(&prepared, &user),
            "Hello world with code."
        );
        let timestamp = prepared
            .copy_ranges
            .iter()
            .find(|copy| copy.text == "time unavailable")
            .unwrap();
        let point = |column| SelectionPoint {
            row: timestamp.row,
            column,
            section_id: Some(user.id.clone()),
            section_row: timestamp.row - user.rows.start,
        };
        let selection = ConversationSelection {
            session_id: "ses_1".into(),
            anchor: point(timestamp.columns.start),
            focus: point(timestamp.columns.start + "time unavailable".len() - 1),
            granularity: SelectionGranularity::Character,
            dragged: true,
        };
        assert_eq!(
            crate::ui::transcript::selection_text(&prepared, &selection),
            "time unavailable"
        );
    }

    #[test]
    fn applied_steer_message_copy_keeps_body_but_not_timestamp_or_receipt() {
        let mut app = crate::ui::testapp::live_turn(crate::theme::ThemeKind::Dark);
        app.active_session_mut()
            .unwrap()
            .applied_steers
            .push(crate::state::turn::AppliedSteer {
                local_id: 77,
                text: "change direction now".into(),
                accepted_at: Some("2026-01-02T03:04:05.000Z".into()),
                request_index: 0,
            });
        let prepared = crate::ui::transcript::prepare_conversation(&app, 77);
        let steer = prepared
            .sections
            .iter()
            .find(|section| {
                section.id.kind == crate::state::view::SectionKind::User && section.id.ordinal == 77
            })
            .unwrap();
        assert!(
            prepared
                .copy_ranges
                .iter()
                .any(|copy| steer.rows.contains(&copy.row) && copy.text.contains("1/2/2026")),
            "timestamp remains visible/selectable"
        );
        let text = section_copy_text(&prepared, &steer);
        assert!(text.contains("change direction now"));
        assert!(!text.contains("1/2/2026"));
        assert!(!text.contains("applied"));
    }

    #[test]
    fn copy_last_excludes_active_live_reply() {
        use crate::event::{AppEvent, RpcEvent};
        use crate::protocol::{IncomingFrame, RpcNotification};
        use serde_json::json;
        let mut app = crate::ui::testapp::chat_with_reasoning(crate::theme::ThemeKind::Dark);
        app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".into(),
            text: "next turn".into(),
        });
        for event in [
            json!({"type":"turn_started","data":{"turn":{"session_id":"ses_1","loop_id":"live_copy"},"meta":{"session_id":"ses_1","dropped_before":0}}}),
            json!({"type":"request_started","data":{"turn":{"session_id":"ses_1","loop_id":"live_copy"},"request_index":0,"config_revision":0,"model":"deep","reasoning":"high","meta":{"session_id":"ses_1","dropped_before":0}}}),
            json!({"type":"output_delta","data":{"turn":{"session_id":"ses_1","loop_id":"live_copy"},"request_index":0,"channel":"text","delta":"unfinished live output","meta":{"session_id":"ses_1","dropped_before":0}}}),
        ] {
            app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
                RpcNotification::AgentEvent(serde_json::from_value(event).unwrap()),
            ))));
        }
        let CopyPlan::Text(text) = app.plan_last_reply_copy() else {
            panic!("stored reply must remain copyable")
        };
        assert_eq!(text, "answer text");
    }

    #[test]
    fn copy_last_without_stored_reply_reports_limitation() {
        let app = crate::ui::testapp::live_turn(crate::theme::ThemeKind::Dark);
        let CopyPlan::Limitation(detail) = app.plan_last_reply_copy() else {
            panic!("live deltas are not a completed reply")
        };
        assert!(detail.contains("no completed reply"));
    }

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
    fn copy_app(source: &str, thinking: bool, live: bool, width: u16) -> crate::app::App {
        use crate::event::{AppEvent, RpcEvent};
        use crate::protocol::{IncomingFrame, RpcNotification};
        use serde_json::json;
        let mut app = if live {
            let mut app = crate::ui::testapp::open_empty(
                crate::theme::ThemeKind::Dark,
                "ses_1",
                None,
                "high",
            );
            app.update(AppEvent::SubmitTurn {
                session_id: "ses_1".into(),
                text: "prompt".into(),
            });
            for event in [
                json!({"type":"turn_started","data":{"turn":{"session_id":"ses_1","loop_id":"copy_loop"},"meta":{"session_id":"ses_1","dropped_before":0}}}),
                json!({"type":"request_started","data":{"turn":{"session_id":"ses_1","loop_id":"copy_loop"},"request_index":0,"config_revision":0,"model":"deep","reasoning":"high","meta":{"session_id":"ses_1","dropped_before":0}}}),
                json!({"type":"output_delta","data":{"turn":{"session_id":"ses_1","loop_id":"copy_loop"},"request_index":0,"channel":if thinking {"reasoning"} else {"text"},"delta":source,"meta":{"session_id":"ses_1","dropped_before":0}}}),
            ] {
                app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
                    RpcNotification::AgentEvent(serde_json::from_value(event).unwrap()),
                ))));
            }
            app
        } else {
            let part = if thinking {
                json!({"type":"reasoning","data":{"text":source}})
            } else {
                json!({"type":"text","data":source})
            };
            crate::ui::testapp::open_with(
                crate::theme::ThemeKind::Dark,
                "ses_1",
                None,
                "high",
                vec![json!({
                    "index":0,"item":{"type":"assistant","data":{"loop_id":"copy_loop","request_index":0,"model":"deep","reasoning":"high","content":[part],"usage":{},"finish_reason":"stop"}}
                })],
            )
        };
        app.update(AppEvent::TerminalSize {
            width: width + 3,
            height: 24,
        });
        if thinking {
            app.update(AppEvent::ToggleReasoningSection {
                session_id: "ses_1".into(),
                loop_id: "copy_loop".into(),
                request_index: 0,
                ordinal: 0,
            });
        }
        app
    }

    fn select_copy_row(app: &mut crate::app::App, width: u16, needle: &str) {
        use crate::state::view::{ConversationSelection, SelectionGranularity, SelectionPoint};
        let prepared = crate::ui::transcript::prepare_conversation(app, width);
        let copy = prepared
            .copy_ranges
            .iter()
            .find(|copy| copy.text.contains(needle))
            .unwrap();
        let section = prepared
            .sections
            .iter()
            .find(|section| section.rows.contains(&copy.row))
            .unwrap();
        let point = SelectionPoint {
            row: copy.row,
            column: copy.columns.start,
            section_id: Some(section.id.clone()),
            section_row: copy.row - section.rows.start,
        };
        app.selection = Some(ConversationSelection {
            session_id: "ses_1".into(),
            anchor: point.clone(),
            focus: point,
            granularity: SelectionGranularity::Character,
            dragged: false,
        });
    }

    #[test]
    fn selected_fence_uses_source_bytes_across_widths_and_live_paths() {
        // Formatting, CJK and an invisible ANSI sequence deliberately make
        // visual rows/cells differ from the original Markdown byte offsets.
        let first = format!("{}END_FIRST", "变量🙂".repeat(70));
        let source = format!(
            "**前言** {}\u{1b}[31mintro\u{1b}[0m\n\n```rust\n{first}\n```\n\nBetween\n\n~~~sh\nSECOND\n~~~\n\n```\nTHIRD\n```\n",
            "长段落".repeat(35)
        );
        for width in [57, 77, 117] {
            for live in [false, true] {
                for thinking in [false, true] {
                    let mut app = copy_app(&source, thinking, live, width);
                    select_copy_row(&mut app, width, "END_FIRST");
                    let CopyPlan::Text(text) = app.plan_code_copy() else {
                        panic!("selected fence must be copyable")
                    };
                    assert_eq!(text, first, "width={width} live={live} thinking={thinking}");
                    select_copy_row(&mut app, width, "SECOND");
                    let CopyPlan::Text(text) = app.plan_code_copy() else {
                        panic!("second fence must be copyable")
                    };
                    assert_eq!(text, "SECOND");
                    select_copy_row(&mut app, width, "THIRD");
                    let CopyPlan::Text(text) = app.plan_code_copy() else {
                        panic!("third fence must be copyable")
                    };
                    assert_eq!(text, "THIRD");
                }
            }
        }
    }

    #[test]
    fn live_and_durable_message_and_selection_copy_preserve_hard_breaks() {
        use crate::state::view::{
            ConversationSelection, SectionKind, SelectionGranularity, SelectionPoint,
        };
        for width in [57, 77, 117] {
            for thinking in [false, true] {
                let body = format!("{}\n\n{}", "变量🙂X".repeat(70), "NEXT".repeat(45));
                let source = if thinking {
                    format!("```text\n{body}\n```")
                } else {
                    body.clone()
                };
                for live in [false, true] {
                    let app = copy_app(&source, thinking, live, width);
                    let prepared = crate::ui::transcript::prepare_conversation(&app, width);
                    let kind = if thinking {
                        SectionKind::Thinking
                    } else {
                        SectionKind::AssistantText
                    };
                    let section = prepared
                        .sections
                        .iter()
                        .find(|section| section.id.kind == kind)
                        .unwrap();
                    assert_eq!(
                        section_copy_text(&prepared, &section),
                        body,
                        "width={width} live={live} thinking={thinking}"
                    );
                    let copies = prepared
                        .copy_ranges
                        .iter()
                        .filter(|copy| section.rows.contains(&copy.row) && !copy.decorative)
                        .collect::<Vec<_>>();
                    let point = |copy: &crate::state::view::CopyView<'_>, column| SelectionPoint {
                        row: copy.row,
                        column,
                        section_id: Some(section.id.clone()),
                        section_row: copy.row - section.rows.start,
                    };
                    let selection = ConversationSelection {
                        session_id: "ses_1".into(),
                        anchor: point(
                            copies.first().unwrap(),
                            copies.first().unwrap().columns.start,
                        ),
                        focus: point(
                            copies.last().unwrap(),
                            copies.last().unwrap().columns.end.saturating_sub(1),
                        ),
                        granularity: SelectionGranularity::Character,
                        dragged: true,
                    };
                    assert_eq!(
                        crate::ui::transcript::selection_text(&prepared, &selection),
                        body
                    );
                }
            }
        }
    }
    #[test]
    fn xml_message_copy_keeps_literal_source() {
        let source = "<instructions>\nReview release notes.\n</instructions>";
        let mut user = crate::ui::testapp::open_with(
            crate::theme::ThemeKind::Dark,
            "ses_1",
            None,
            "high",
            vec![crate::ui::testapp::user_entry(0, "copy_loop", source)],
        );
        user.update(crate::event::AppEvent::TerminalSize {
            width: 60,
            height: 24,
        });
        select_copy_row(&mut user, 57, "Review release notes.");
        let CopyPlan::Text(text) = user.plan_section_copy(super::SectionPick::ViewportOrSelection)
        else {
            panic!("XML user source must be copyable")
        };
        assert_eq!(text, source);
        for live in [false, true] {
            let mut app = copy_app(source, false, live, 57);
            select_copy_row(&mut app, 57, "Review release notes.");
            let CopyPlan::Text(text) =
                app.plan_section_copy(super::SectionPick::ViewportOrSelection)
            else {
                panic!("XML source must be copyable")
            };
            assert_eq!(text, source);
        }
    }

    #[test]
    fn copying_soft_wrapped_words_keeps_separator_spaces() {
        let source = "alpha beta gamma delta epsilon ".repeat(30);
        for width in [57, 77, 117] {
            for live in [false, true] {
                let app = copy_app(source.trim_end(), false, live, width);
                let prepared = crate::ui::transcript::prepare_conversation(&app, width);
                let section = prepared
                    .sections
                    .iter()
                    .find(|section| {
                        section.id.kind == crate::state::view::SectionKind::AssistantText
                    })
                    .unwrap();
                assert_eq!(
                    section_copy_text(&prepared, &section),
                    source.trim_end(),
                    "width={width} live={live}"
                );
            }
        }
    }
    #[test]
    fn tool_copy_excludes_affordance_but_keeps_identical_result_text() {
        use crate::state::view::{
            ConversationSelection, SectionKind, SelectionGranularity, SelectionPoint,
        };
        use serde_json::json;
        for result in ["ctrl+o collapse\nretained result", ""] {
            for expanded in [false, true] {
                let mut app = crate::ui::testapp::open_with(
                    crate::theme::ThemeKind::Dark,
                    "ses_1",
                    None,
                    "high",
                    vec![json!({"index":0,"item":{"type":"tool_result","data":{
                        "loop_id":"copy_tool","request_index":0,"call_id":"call_copy",
                        "tool_name":"read","outcome":"success","output":{"content":result}
                    }}})],
                );
                app.update(crate::event::AppEvent::TerminalSize {
                    width: 80,
                    height: 24,
                });
                let prepared = crate::ui::transcript::prepare_conversation(&app, 77);
                let section = prepared
                    .sections
                    .iter()
                    .find(|section| section.id.kind == SectionKind::Tool)
                    .unwrap();
                if section.folded == expanded {
                    app.update(crate::event::AppEvent::ToggleTool {
                        session_id: "ses_1".into(),
                        loop_id: "copy_tool".into(),
                        request_index: 0,
                        tool_call_id: "call_copy".into(),
                    });
                }
                let prepared = crate::ui::transcript::prepare_conversation(&app, 77);
                let section = prepared
                    .sections
                    .iter()
                    .find(|section| section.id.kind == SectionKind::Tool)
                    .unwrap();
                assert_eq!(section.folded, !expanded);
                let footer = prepared.copy_row(section.rows.end - 2).unwrap();
                assert_eq!(
                    footer.decorative,
                    expanded || !result.is_empty(),
                    "only an actual tool footer is decoration"
                );
                let text = section_copy_text(&prepared, &section);
                assert_eq!(
                    text.matches("ctrl+o collapse").count(),
                    usize::from(!result.is_empty())
                );
                assert!(!text.contains("ctrl+o expand"));
                assert!(!text.contains("hidden rows"));
                let point = |row, column| SelectionPoint {
                    row,
                    column,
                    section_id: Some(section.id.clone()),
                    section_row: row - section.rows.start,
                };
                let selection = ConversationSelection {
                    session_id: "ses_1".into(),
                    anchor: point(section.rows.start, 0),
                    focus: point(section.rows.end - 1, 78),
                    granularity: SelectionGranularity::Character,
                    dragged: true,
                };
                assert_eq!(
                    crate::ui::transcript::selection_text(&prepared, &selection).trim_matches('\n'),
                    text
                );
                // Empty cards still retain their target row; it must not be
                // mistaken for the absent expand affordance.
                if result.is_empty() && !expanded {
                    assert!(!footer.decorative);
                    assert!(!footer.text.is_empty());
                }
            }
        }
    }
}
