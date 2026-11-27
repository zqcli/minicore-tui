//! Native accepted records as blocks and streaming Markdown.

use std::collections::HashMap;
use std::io::{self, Write};

use minicore_host_core::{
    HistoryItemData, HistoryRecordData, ModelFinishedData, ModelResponseData,
};
use minicore_host_protocol::{
    MessageMode, MessageSource, ModelOutputIdentity, ToolCallId, ToolCallRef, TurnId,
};
use minicore_host_runtime::model::{AssistantPart, Usage};

use super::{Block, HistorySource, RequestKey, ToolCall, ToolKey, Transcript, UserKind};

/// Provider-reported counts. Missing fields are not zero, even within a
/// Session that switches providers.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokenTotals {
    counts: [Option<u64>; 3],
    missing: [bool; 3],
}

impl TokenTotals {
    fn add(&mut self, usage: Usage) {
        // Host usage separates generated text and reasoning. If only one is
        // known, show its subtotal with the existing incomplete marker.
        let output = usage.output_tokens();
        let reasoning = usage.reasoning_tokens();
        self.missing[1] |= output.is_none() || reasoning.is_none();
        let generated = output
            .or(reasoning)
            .map(|_| output.unwrap_or(0).saturating_add(reasoning.unwrap_or(0)));
        let counts = [usage.input_tokens(), generated, usage.cache_read_tokens()];
        for (index, count) in counts.into_iter().enumerate() {
            if let Some(count) = count {
                self.counts[index] = Some(self.counts[index].unwrap_or(0).saturating_add(count));
            } else {
                self.missing[index] = true;
            }
        }
    }

    pub fn counts(self) -> [(Option<u64>, bool); 3] {
        std::array::from_fn(|index| (self.counts[index], self.missing[index]))
    }
}

impl Transcript {
    #[cfg(test)]
    pub fn from_history(records: &[HistoryRecordData]) -> Self {
        let mut transcript = Self::default();
        transcript.install_history_records(records);
        transcript
    }

    /// Installs accepted records by position without rebuilding streamed bodies.
    pub fn install_history_records(&mut self, records: &[HistoryRecordData]) {
        self.install_records(records, false);
    }

    pub fn install_preview_records(&mut self, records: &[HistoryRecordData]) {
        let start = records
            .len()
            .saturating_sub(super::agent_card::PREVIEW_BLOCKS);
        for record in &records[start..] {
            self.install_records(std::slice::from_ref(record), true);
            self.retain_recent(super::agent_card::PREVIEW_BLOCKS);
        }
    }

    pub fn install_preview_response(
        &mut self,
        source: &ModelOutputIdentity,
        data: &ModelFinishedData,
    ) {
        let record = HistoryRecordData {
            position: data.history_position,
            recorded_at: data.recorded_at,
            turn_id: Some(source.turn.turn_id.clone()),
            response_index: Some(source.response_index),
            delegations: Vec::new(),
            item: HistoryItemData::Assistant {
                response: ModelResponseData {
                    content: Vec::new(),
                    finish_reason: data.result.finish_reason,
                    usage: data.result.usage,
                },
            },
        };
        let mut index = HistoryIndex::new(self);
        if !index.positions.contains_key(&record.position) {
            append_response(self, &mut index, &record, &data.result.content, false, true);
            self.sort_history();
        }
    }

    pub fn token_totals(&self) -> TokenTotals {
        self.tokens
    }

    pub(crate) fn record_usage(&mut self, position: u64, usage: Usage) {
        if self.usage_positions.insert(position) {
            self.tokens.add(usage);
        }
    }

    fn install_records(&mut self, records: &[HistoryRecordData], preview: bool) {
        if records.is_empty() {
            return;
        }
        let mut index = HistoryIndex::new(self);
        let mut added = false;
        for record in records {
            if preview
                && !matches!(
                    record.item,
                    HistoryItemData::Assistant { .. }
                        | HistoryItemData::Partial { .. }
                        | HistoryItemData::ToolResult { .. }
                )
            {
                continue;
            }
            if let Some(blocks) = index.positions.get(&record.position) {
                for &at in blocks {
                    let mut changed = false;
                    for source in &mut self.metadata[at].history {
                        if source.position == record.position
                            && source.response_index.is_none()
                            && record.response_index.is_some()
                        {
                            source.response_index = record.response_index;
                            changed = true;
                        }
                    }
                    if changed {
                        if let Block::Tool(call) = &mut self.blocks[at] {
                            call.response_index = record.response_index;
                        }
                        self.bump(at);
                    }
                }
            } else {
                append_record(self, &mut index, record, preview);
                added = true;
            }
            if let HistoryItemData::ToolResult { .. } = record.item {
                for &at in index.positions.get(&record.position).into_iter().flatten() {
                    let Block::Tool(call) = &mut self.blocks[at] else {
                        continue;
                    };
                    let before = call.delegations.len();
                    for association in &record.delegations {
                        if !call.delegations.iter().any(|known| {
                            known.parent == association.parent
                                && known.origin == association.origin
                                && known.child == association.child
                                && known.task_message_id == association.task_message_id
                        }) {
                            call.delegations.push(association.clone());
                        }
                    }
                    if call.delegations.len() != before {
                        self.bump(at);
                    }
                }
            }
        }
        if added {
            self.sort_history();
            self.link_delegations();
        }
        // A page can add steps to any Turn, including one already folded, and a
        // record replacing a live block brings the time a run is measured by.
        {
            let mut turns: Vec<&TurnId> = records
                .iter()
                .filter_map(|record| record.turn_id.as_ref())
                .collect();
            turns.dedup();
            for turn in turns {
                self.regroup(turn);
            }
        }
    }
}

// Block indices are stable until the batch's final reorder.
#[derive(Default)]
struct HistoryIndex {
    positions: HashMap<u64, Vec<usize>>,
    responses: HashMap<RequestKey, usize>,
    tools: HashMap<ToolKey, usize>,
    seed_tools: HashMap<ToolCallId, usize>,
    turns: HashMap<TurnId, usize>,
    /// The first record seen for each Turn, to time it at its end.
    started: HashMap<TurnId, minicore_host_protocol::Timestamp>,
}

impl HistoryIndex {
    fn new(transcript: &Transcript) -> Self {
        let mut index = Self::default();
        for (at, (block, meta)) in transcript
            .blocks
            .iter()
            .zip(&transcript.metadata)
            .enumerate()
        {
            for source in &meta.history {
                index.positions.entry(source.position).or_default().push(at);
            }
            match block {
                Block::Assistant { key, .. } => {
                    index.responses.insert(key.clone(), at);
                }
                Block::Tool(call) => index.tool(call.key.clone(), at),
                Block::TurnEnd { turn_id, .. } => {
                    index.turns.entry(turn_id.clone()).or_insert(at);
                }
                _ => {}
            }
        }
        index
    }

    fn tool(&mut self, key: ToolKey, at: usize) {
        if key.turn_id().is_none() {
            self.seed_tools.insert(key.tool_call_id().clone(), at);
        }
        self.tools.insert(key, at);
    }

    fn source(&mut self, transcript: &mut Transcript, at: usize, source: HistorySource) {
        self.positions.entry(source.position).or_default().push(at);
        transcript.add_history_source(at, source);
    }
}

fn source(record: &HistoryRecordData, part: usize) -> HistorySource {
    HistorySource {
        position: record.position,
        recorded_at: record.recorded_at,
        turn_id: record.turn_id.clone(),
        response_index: record.response_index,
        part,
    }
}

fn request_key(record: &HistoryRecordData) -> RequestKey {
    match (&record.turn_id, record.response_index) {
        (Some(turn_id), Some(response_index)) => {
            RequestKey::response(turn_id.clone(), response_index)
        }
        _ => RequestKey::History {
            position: record.position,
        },
    }
}

fn tool_key(record: &HistoryRecordData, tool_call_id: ToolCallId) -> ToolKey {
    match &record.turn_id {
        Some(turn_id) => ToolKey::Call(ToolCallRef {
            turn_id: turn_id.clone(),
            tool_call_id,
        }),
        None => ToolKey::History {
            position: record.position,
            tool_call_id,
        },
    }
}

fn append_record(
    transcript: &mut Transcript,
    index: &mut HistoryIndex,
    record: &HistoryRecordData,
    preview: bool,
) {
    // A Turn's end is not its start: a lone end record gives no duration.
    if let Some(turn) = &record.turn_id
        && !matches!(record.item, HistoryItemData::TurnFinished { .. })
    {
        index
            .started
            .entry(turn.clone())
            .or_insert(record.recorded_at);
    }
    match &record.item {
        HistoryItemData::User {
            received_at,
            text,
            mode,
            message_id,
            source: sender,
            ..
        } => {
            let at = transcript.push(Block::User {
                text: if preview {
                    super::agent_card::tail(text, super::agent_card::PREVIEW_BYTES).to_owned()
                } else {
                    text.clone()
                },
                kind: match mode {
                    _ if matches!(sender, Some(MessageSource::Agent { .. }))
                        && crate::ui::delegated::is_result(text) =>
                    {
                        UserKind::Delegated
                    }
                    Some(MessageMode::Steer) => UserKind::Steering,
                    Some(MessageMode::FollowUp) => UserKind::FollowUp,
                    None => UserKind::Prompt,
                },
                at: Some(received_at.unwrap_or(record.recorded_at)),
                expanded: None,
            });
            transcript.identify_user(at, record.turn_id.clone(), message_id.clone());
            index.source(transcript, at, source(record, 0));
        }
        HistoryItemData::Assistant { response } => {
            append_response(transcript, index, record, &response.content, false, preview);
        }
        HistoryItemData::Partial { partial } => {
            append_response(transcript, index, record, &partial.content, true, preview);
        }
        HistoryItemData::ToolResult {
            tool_call_id,
            name,
            result,
        } => {
            let key = tool_key(record, tool_call_id.clone());
            let known = if record.turn_id.is_none() {
                index.seed_tools.get(tool_call_id)
            } else {
                index.tools.get(&key)
            }
            .copied();
            let at = known.unwrap_or_else(|| {
                let at =
                    transcript.push(Block::Tool(Box::new(ToolCall::with_key(key.clone(), name))));
                index.tool(key, at);
                at
            });
            if let Block::Tool(call) = &mut transcript.blocks[at] {
                call.response_index = record.response_index.or(call.response_index);
                if preview {
                    call.apply_preview_result(result);
                } else {
                    call.apply_result(result);
                }
            }
            index.source(transcript, at, source(record, 0));
            backfill_tool_duration(transcript, at);
        }
        HistoryItemData::Summary { text } => {
            let at = transcript.push(Block::Summary { text: text.clone() });
            index.source(transcript, at, source(record, 0));
        }
        HistoryItemData::Compaction {
            text,
            source_position,
            covered_items,
        } => {
            let at = transcript.push(Block::Compaction {
                source_position: *source_position,
                covered_items: *covered_items,
                summary: text.clone(),
            });
            index.source(transcript, at, source(record, 0));
        }
        HistoryItemData::TurnFinished { terminal } => {
            let Some(turn_id) = &record.turn_id else {
                return;
            };
            let block = Block::TurnEnd {
                turn_id: turn_id.clone(),
                outcome: terminal.outcome,
                reason: terminal.reason.clone(),
                duration: index
                    .started
                    .get(turn_id)
                    .and_then(|start| super::elapsed(*start, terminal.finished_at)),
                requests: terminal.requests,
                tool_rounds: terminal.tool_rounds,
                usage: Usage::default(),
                changes: super::turn_changes(&transcript.blocks, turn_id),
                saving: None,
            };
            let at = match index.turns.get(turn_id).copied() {
                Some(at) => {
                    let mut block = block;
                    if let (
                        Block::TurnEnd {
                            duration,
                            changes,
                            saving,
                            ..
                        },
                        Block::TurnEnd {
                            duration: old_duration,
                            changes: old_changes,
                            saving: old_saving,
                            ..
                        },
                    ) = (&mut block, &transcript.blocks[at])
                    {
                        *duration = old_duration.or(*duration);
                        *changes = old_changes.or(*changes);
                        *saving = *old_saving;
                    }
                    transcript.blocks[at] = block;
                    at
                }
                None => {
                    let at = transcript.push(block);
                    index.turns.insert(turn_id.clone(), at);
                    at
                }
            };
            transcript.regroup(turn_id);
            index.source(transcript, at, source(record, 0));
        }
    }
}

fn append_response(
    transcript: &mut Transcript,
    index: &mut HistoryIndex,
    record: &HistoryRecordData,
    content: &[AssistantPart],
    partial: bool,
    preview: bool,
) {
    if let HistoryItemData::Assistant { response } = &record.item {
        if !preview {
            transcript.record_usage(record.position, response.usage);
        } else if transcript
            .last_preview_usage
            .is_none_or(|position| record.position > position)
        {
            // A released child's preview may miss old events. Keep its
            // observed counters bounded; never label them a full total.
            transcript.last_preview_usage = Some(record.position);
            transcript.tokens.add(response.usage);
        }
    }
    let key = request_key(record);
    let mut text = if preview {
        preview_parts(content, false)
    } else {
        String::new()
    };
    let mut reasoning = if preview {
        preview_parts(content, true)
    } else {
        String::new()
    };
    if !preview {
        for part in content {
            match part {
                AssistantPart::Text(chunk) => text.push_str(chunk),
                AssistantPart::Reasoning(chunk) => {
                    if let Some(chunk) = chunk.text().or(chunk.summary()) {
                        reasoning.push_str(chunk);
                    }
                }
                AssistantPart::ToolCall(_) => {}
            }
        }
    }
    let at = match index.responses.get(&key).copied() {
        Some(at) => {
            if let Block::Assistant {
                reasoning: known_reasoning,
                text: known_text,
                streaming,
                partial: known_partial,
                ..
            } = &mut transcript.blocks[at]
            {
                *known_reasoning = reasoning;
                *known_text = text;
                *streaming = false;
                *known_partial = partial;
            }
            at
        }
        None => {
            let at = transcript.push(Block::Assistant {
                key: key.clone(),
                reasoning,
                text,
                streaming: false,
                partial,
                expanded: None,
            });
            index.responses.insert(key, at);
            at
        }
    };
    index.source(transcript, at, source(record, 0));
    if partial {
        return;
    }
    let first_tool = if preview {
        content
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, part)| matches!(part, AssistantPart::ToolCall(_)))
            .nth(super::agent_card::PREVIEW_BLOCKS - 2)
            .map_or(0, |(index, _)| index)
    } else {
        0
    };
    for (part_index, part) in content.iter().enumerate().skip(first_tool) {
        let AssistantPart::ToolCall(call) = part else {
            continue;
        };
        let key = tool_key(record, ToolCallId(call.tool_call_id().as_str().to_owned()));
        let at = index.tools.get(&key).copied().unwrap_or_else(|| {
            let at = transcript.push(Block::Tool(Box::new(ToolCall::with_key(
                key.clone(),
                call.name().as_str(),
            ))));
            index.tool(key, at);
            at
        });
        if let Block::Tool(card) = &mut transcript.blocks[at] {
            card.response_index = record.response_index.or(card.response_index);
            if preview {
                card.set_preview_arguments(call.arguments());
            } else {
                card.set_arguments(call.arguments());
            }
        }
        index.source(transcript, at, source(record, part_index + 1));
        backfill_tool_duration(transcript, at);
    }
}

/// Restores a tool call's wall time on history reload from its saved
/// invocation and result timestamps when no live duration was captured.
fn backfill_tool_duration(transcript: &mut Transcript, at: usize) {
    let Block::Tool(call) = &transcript.blocks[at] else {
        return;
    };
    if call.duration.is_some() || !call.has_result() {
        return;
    }
    let sources = transcript.history_sources(at);
    let (Some(first), Some(last)) = (sources.first(), sources.last()) else {
        return;
    };
    if first.position < last.position
        && let Some(duration) = super::elapsed(first.recorded_at, last.recorded_at)
        && let Block::Tool(call) = &mut transcript.blocks[at]
    {
        call.duration = Some(duration);
    }
}

fn preview_parts(content: &[AssistantPart], reasoning: bool) -> String {
    let mut remaining = super::agent_card::PREVIEW_BYTES;
    let mut parts = Vec::new();
    for part in content.iter().rev() {
        let text = match part {
            AssistantPart::Text(text) if !reasoning => Some(text.as_ref()),
            AssistantPart::Reasoning(text) if reasoning => text.text().or(text.summary()),
            _ => None,
        };
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            let cut = text.len() > remaining;
            let text = super::agent_card::tail(text, remaining);
            remaining -= text.len();
            parts.push(text);
            if cut || remaining == 0 {
                break;
            }
        }
    }
    let mut out = String::with_capacity(super::agent_card::PREVIEW_BYTES - remaining);
    for text in parts.into_iter().rev() {
        out.push_str(text);
    }
    out
}

pub fn to_markdown(records: &[HistoryRecordData]) -> String {
    let mut out = Vec::new();
    write_markdown(&mut out, records).expect("writing to a vector succeeds");
    String::from_utf8(out).expect("Markdown contains UTF-8")
}

/// Writes native records directly, without bounded tool-card output or screen state.
pub fn write_markdown(writer: &mut impl Write, records: &[HistoryRecordData]) -> io::Result<()> {
    for record in records {
        let stamp = serde_json::to_value(record.recorded_at).expect("Timestamp serializes");
        write!(
            writer,
            "<!-- record {} · {}",
            record.position,
            stamp.as_str().expect("Timestamp is text")
        )?;
        if let Some(turn) = &record.turn_id {
            write!(writer, " · turn {turn}")?;
        }
        if let Some(response) = record.response_index {
            write!(writer, " · response {response}")?;
        }
        writeln!(writer, " -->\n")?;
        match &record.item {
            HistoryItemData::User { text, mode, .. } => {
                writeln!(
                    writer,
                    "**{}**\n\n{text}\n",
                    if *mode == Some(MessageMode::Steer) {
                        "Steering"
                    } else {
                        "You"
                    }
                )?;
            }
            HistoryItemData::Assistant { response } => {
                write_parts(writer, &response.content, false)?
            }
            HistoryItemData::Partial { partial } => {
                writeln!(writer, "**Incomplete assistant response**\n")?;
                write_parts(writer, &partial.content, true)?;
                for call in &partial.incomplete_tool_calls {
                    writeln!(
                        writer,
                        "*Unfinished tool call `{}` (`{}`); not executed.*\n",
                        call.tool_name, call.tool_call_id
                    )?;
                    write_fenced(writer, &call.arguments_text, "text")?;
                }
            }
            HistoryItemData::ToolResult {
                tool_call_id,
                name,
                result,
            } => {
                writeln!(
                    writer,
                    "**Tool result: {name}** (`{tool_call_id}`, {:?})\n",
                    result.outcome
                )?;
                write_fenced(writer, result.output.content().as_str(), "text")?;
            }
            HistoryItemData::Summary { text } => {
                writeln!(writer, "---\n\n**Context summary**\n\n{text}\n")?
            }
            HistoryItemData::Compaction {
                text,
                source_position,
                covered_items,
            } => {
                writeln!(
                    writer,
                    "---\n\n**Context compacted** · {covered_items} items · source position {source_position}\n\n{text}\n"
                )?;
            }
            HistoryItemData::TurnFinished { terminal } => {
                writeln!(
                    writer,
                    "*Turn {:?} · {} requests · {} tool rounds*\n",
                    terminal.outcome, terminal.requests, terminal.tool_rounds
                )?;
                if let Some(reason) = &terminal.reason {
                    writeln!(writer, "{reason}\n")?;
                }
            }
        }
    }
    Ok(())
}

fn write_parts(writer: &mut impl Write, parts: &[AssistantPart], partial: bool) -> io::Result<()> {
    for part in parts {
        match part {
            AssistantPart::Text(text) => writeln!(writer, "{text}\n")?,
            AssistantPart::Reasoning(reasoning) => {
                if let Some(text) = reasoning.text().or(reasoning.summary()) {
                    writeln!(writer, "> {}\n", text.replace('\n', "\n> "))?;
                }
            }
            AssistantPart::ToolCall(call) => {
                writeln!(
                    writer,
                    "**{}: {}** (`{}`)\n",
                    if partial {
                        "Unexecuted tool call in partial response"
                    } else {
                        "Tool call"
                    },
                    call.name(),
                    call.tool_call_id()
                )?;
                write_fenced(
                    writer,
                    &serde_json::to_string_pretty(call.arguments()).expect("JSON serializes"),
                    "json",
                )?;
            }
        }
    }
    Ok(())
}

fn write_fenced(writer: &mut impl Write, text: &str, language: &str) -> io::Result<()> {
    let fence = "`".repeat(
        text.lines()
            .map(|line| line.chars().take_while(|ch| *ch == '`').count())
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    writeln!(writer, "{fence}{language}\n{text}\n{fence}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn record(position: u64, item: Value) -> HistoryRecordData {
        let mut value = json!({"position":position,"recorded_at":"2026-11-13T10:00:00Z",
            "turn_id":"00000000000000000001","response_index":0});
        value
            .as_object_mut()
            .unwrap()
            .extend(item.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn assistant(position: u64) -> HistoryRecordData {
        record(
            position,
            json!({"kind":"assistant","response":{"content":[
            {"type":"text","data":"working"},
            {"type":"tool_call","data":{"tool_call_id":"call","name":"bash","arguments":{"command":"printf '你好'\necho done"},"call_index":0}}
        ],"finish_reason":"tool_calls","usage":{}}}),
        )
    }

    fn result(position: u64) -> HistoryRecordData {
        record(
            position,
            json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
                "result":{"outcome":"success","output":{"content":"done"}}}),
        )
    }

    #[test]
    fn task_tokens_count_reasoning_without_inventing_unreported_counts() {
        let mut totals = super::TokenTotals::default();
        totals.add(Usage::from_optional(None, None, Some(30)).with_cache_write_tokens(Some(100)));
        assert_eq!(
            totals.counts(),
            [(None, true), (Some(30), true), (None, true)]
        );
        totals.add(Usage::new(50, 70, 30).with_cache_read_tokens(Some(10)));
        assert_eq!(
            totals.counts(),
            [(Some(50), true), (Some(130), true), (Some(10), true)]
        );
    }

    #[test]
    fn saved_followup_keeps_its_origin_link_without_the_original_history_page() {
        let followup = record(
            9,
            json!({"kind":"tool_result","tool_call_id":"supplement","name":"agent",
            "result":{"outcome":"success","output":{"content":"accepted"}},
            "delegations":[{
                "parent":{"agent_id":"00000000000000000002","session_id":"00000000000000000003"},
                "origin":{"turn_id":"00000000000000000001","tool_call_id":"original"},
                "child":{"agent_id":"00000000000000000002","session_id":"00000000000000000004"},
                "task_message_id":"00000000000000000005",
                "accepted_message_ids":["00000000000000000005"],
                "inputs":[],"completion":null
            }]}),
        );
        for live_first in [false, true] {
            let mut transcript = Transcript::default();
            if live_first {
                let mut live = followup.clone();
                live.delegations.clear();
                transcript.install_history_records(&[live]);
            }
            transcript.install_history_records(std::slice::from_ref(&followup));
            let call = transcript.tool_calls().next().unwrap();
            assert!(call.is_delegation_receipt());
            assert_eq!(call.delegations, followup.delegations);
            assert!(
                transcript
                    .find_tool(&followup.delegations[0].origin)
                    .is_none()
            );
            let revisions: Vec<_> = transcript.revisions().collect();
            transcript.install_history_records(std::slice::from_ref(&followup));
            assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
            assert_eq!(transcript.tool_calls().next().unwrap().delegations.len(), 1);
        }
    }

    #[test]
    fn preview_text_keeps_utf8_tail_and_native_part_boundaries() {
        use minicore_host_runtime::model::ReasoningContent;
        let large = "界".repeat(super::super::agent_card::PREVIEW_BYTES);
        let parts = [
            AssistantPart::Text(large.clone().into()),
            AssistantPart::Reasoning(
                ReasoningContent::new(Some("first\nsecond".into()), None, None, None).unwrap(),
            ),
            AssistantPart::Text("\nlast real line".into()),
        ];
        let text = preview_parts(&parts, false);
        assert!(text.len() <= super::super::agent_card::PREVIEW_BYTES);
        assert!(format!("{large}\nlast real line").ends_with(&text));
        assert_eq!(preview_parts(&parts, true), "first\nsecond");
    }

    #[test]
    fn repeated_message_ids_and_equal_text_keep_distinct_native_positions() {
        let user =
            json!({"kind":"user","text":"same","message_id":"00000000000000000002","mode":"steer"});
        let records = [record(4, user.clone()), record(5, user)];
        let mut transcript = Transcript::from_history(&records);
        let revisions: Vec<_> = transcript.revisions().collect();
        transcript.install_history_records(&records);
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript.history_source(0).unwrap().position, 4);
        assert_eq!(transcript.history_source(1).unwrap().position, 5);
        assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
    }

    #[test]
    fn a_result_cut_by_the_tail_page_joins_its_later_loaded_invocation() {
        let result = record(
            2,
            json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
            "result":{"outcome":"success","output":{"content":"你好\ndone"}}}),
        );
        let mut transcript = Transcript::from_history(&[result]);
        assert_eq!(transcript.tool_calls().count(), 1);
        transcript.install_history_records(&[assistant(1)]);
        let call = transcript.tool_calls().next().unwrap();
        assert_eq!(call.state, super::super::ToolState::Succeeded);
        assert_eq!(
            call.arguments.as_ref().unwrap()["command"],
            "printf '你好'\necho done"
        );
        assert_eq!(call.output_content(), ["你好", "done"]);
        assert_eq!(transcript.tool_calls().count(), 1);
        let tool = transcript
            .blocks
            .iter()
            .position(|block| matches!(block, Block::Tool(_)))
            .unwrap();
        assert_eq!(
            transcript
                .history_sources(tool)
                .iter()
                .map(|source| source.position)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn a_partial_is_neither_a_complete_response_nor_a_tool_execution() {
        let partial = record(
            3,
            json!({"kind":"partial","partial":{"content":[{"type":"text","data":"unfinished α"}],
            "incomplete_tool_calls":[{"part_index":1,"call_index":0,"tool_call_id":"fragment","tool_name":"bash","arguments_text":"{\"command\":"}],"usage":null}}),
        );
        let transcript = Transcript::from_history(std::slice::from_ref(&partial));
        assert!(
            matches!(&transcript.blocks()[0], Block::Assistant { partial: true, streaming: false, text, .. } if text == "unfinished α")
        );
        assert_eq!(transcript.tool_calls().count(), 0);
        assert!(to_markdown(&[partial]).contains("not executed"));
    }

    #[test]
    fn summaries_and_compactions_keep_all_prior_transcript_content() {
        let records = [
            record(0, json!({"kind":"user","text":"keep me"})),
            record(1, json!({"kind":"summary","text":"literal summary"})),
            record(
                2,
                json!({"kind":"compaction","text":"replacement context","source_position":2,"covered_items":2}),
            ),
        ];
        let transcript = Transcript::from_history(&records);
        assert_eq!(transcript.len(), 3);
        assert!(matches!(transcript.blocks()[0], Block::User { .. }));
        assert!(matches!(transcript.blocks()[1], Block::Summary { .. }));
        assert!(matches!(
            transcript.blocks()[2],
            Block::Compaction {
                source_position: 2,
                covered_items: 2,
                ..
            }
        ));
    }

    #[test]
    fn seed_responses_keep_position_identity_without_inventing_a_turn() {
        let mut record = assistant(8);
        record.turn_id = None;
        record.response_index = None;
        let transcript = Transcript::from_history(&[record]);
        assert!(matches!(
            &transcript.blocks()[0],
            Block::Assistant {
                key: RequestKey::History { position: 8 },
                ..
            }
        ));
        assert!(
            transcript
                .tool_calls()
                .next()
                .unwrap()
                .key
                .call_ref()
                .is_none()
        );
    }

    #[test]
    fn export_uses_complete_native_results_even_when_the_card_has_a_bounded_tail() {
        let output = (0..900)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let records = [
            assistant(1),
            record(
                2,
                json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
            "result":{"outcome":"success","output":{"content":output}}}),
            ),
        ];
        let transcript = Transcript::from_history(&records);
        let call = transcript.tool_calls().next().unwrap();
        assert!(!call.output_content().contains(&"line 0"));
        let markdown = to_markdown(&records);
        assert!(markdown.contains("line 0\n"));
        assert!(markdown.contains("line 899\n"));
        assert!(markdown.contains("printf '你好'\\necho done"));
        assert!(markdown.contains("record 2 · 2026-11-13T10:00:00Z"));
    }

    #[test]
    fn history_completes_the_response_identity_missing_from_a_tool_finish_event() {
        let mut event = record(
            2,
            json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
            "result":{"outcome":"success","output":{"content":"done"}}}),
        );
        event.response_index = None;
        let mut transcript = Transcript::from_history(&[event.clone()]);
        let anchor = transcript.history_source(0).unwrap().clone();
        let mut canonical = event;
        canonical.response_index = Some(7);
        transcript.install_history_records(&[canonical]);
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript.find_history_source(&anchor), Some(0));
        assert_eq!(
            transcript.history_source(0).unwrap().response_index,
            Some(7)
        );
        assert_eq!(
            transcript.tool_calls().next().unwrap().response_index,
            Some(7)
        );
    }

    #[test]
    fn overlapping_pages_preserve_anchors_revisions_and_canonical_bodies() {
        let tail = [
            record(4, json!({"kind":"user","text":"same"})),
            record(5, json!({"kind":"summary","text":"keep me"})),
        ];
        let mut transcript = Transcript::from_history(&tail);
        let anchors: Vec<_> = (0..2)
            .map(|at| transcript.history_source(at).unwrap().clone())
            .collect();
        let revisions: Vec<_> = transcript.revisions().collect();
        let response = assistant(2);
        let key = request_key(&response);
        if let Block::Assistant { text, expanded, .. } = transcript.assistant_mut(&key) {
            *text = "streamed draft".into();
            *expanded = Some(true);
        }
        assert!(!transcript.has_canonical_response(&key));
        let page = [
            result(3),
            response,
            record(1, json!({"kind":"user","text":"same"})),
            tail[0].clone(),
        ];
        transcript.install_history_records(&page);
        assert_eq!(transcript.len(), 5);
        assert!(transcript.has_canonical_response(&key));
        assert!(matches!(&transcript.blocks()[1], Block::Assistant {
            text, streaming: false, expanded: Some(true), ..
        } if text == "working"));
        assert_eq!(
            transcript.tool_calls().next().unwrap().output_content(),
            ["done"]
        );
        for (anchor, revision) in anchors.iter().zip(revisions) {
            let at = transcript.find_history_source(anchor).unwrap();
            assert_eq!(transcript.revisions().nth(at), Some(revision));
        }
        let revisions: Vec<_> = transcript.revisions().collect();
        transcript.install_history_records(&page);
        assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
        assert_eq!(transcript.len(), 5);
    }

    #[test]
    fn tool_response_identity_fill_preserves_every_source_and_canonical_body() {
        let mut records = [assistant(1), result(2)];
        records[0].response_index = Some(7);
        records[1].response_index = None;
        let mut transcript = Transcript::from_history(&records);
        let anchor = transcript.history_source(1).unwrap().clone();
        let response_revision = transcript.revisions().next();
        records[1].response_index = Some(7);
        transcript.install_history_records(&records);
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript.find_history_source(&anchor), Some(1));
        assert!(transcript.has_canonical_response(&request_key(&records[0])));
        assert_eq!(transcript.revisions().next(), response_revision);
        assert!(
            transcript
                .metadata
                .iter()
                .flat_map(|meta| &meta.history)
                .all(|source| source.response_index == Some(7))
        );
        assert_eq!(
            transcript.tool_calls().next().unwrap().response_index,
            Some(7)
        );
        let revisions: Vec<_> = transcript.revisions().collect();
        transcript.install_history_records(&records);
        assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
    }

    #[test]
    fn a_tool_finish_without_response_identity_keeps_its_invocation_identity() {
        let mut finished = result(2);
        finished.response_index = None;
        let transcript = Transcript::from_history(&[assistant(1), finished]);
        assert_eq!(
            transcript.tool_calls().next().unwrap().response_index,
            Some(0)
        );
        assert_eq!(transcript.history_sources(1).len(), 2);
    }

    #[test]
    fn a_streamed_response_and_seed_with_equal_text_keep_distinct_identities() {
        let mut seed = assistant(1);
        seed.turn_id = None;
        seed.response_index = None;
        let response = assistant(2);
        let key = request_key(&response);
        let mut transcript = Transcript::from_history(&[seed]);
        if let Block::Assistant { text, .. } = transcript.assistant_mut(&key) {
            *text = "working".into();
        }
        assert!(!transcript.has_canonical_response(&key));
        assert!(transcript.has_canonical_response(&RequestKey::History { position: 1 }));
        transcript.install_history_records(std::slice::from_ref(&response));
        assert!(transcript.has_canonical_response(&key));
        assert!(transcript.has_canonical_response(&RequestKey::History { position: 1 }));
        assert_eq!(
            transcript
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Assistant { .. }))
                .count(),
            2
        );
        let revisions: Vec<_> = transcript.revisions().collect();
        transcript.install_history_records(&[response]);
        assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
    }

    #[test]
    #[ignore = "native history installation timing"]
    fn native_history_performance() {
        use std::time::Instant;

        for count in [1_000, 5_000, 10_000, 20_000] {
            let records: Vec<_> = (0..count)
                .map(|position| {
                    let mut record = match position % 3 {
                        0 => record(position, json!({"kind":"user","text":"repeat"})),
                        1 => assistant(position),
                        _ => result(position),
                    };
                    record.turn_id = Some(format!("{:020}", position / 3 + 1).parse().unwrap());
                    record
                })
                .collect();
            let mut transcript = Transcript::default();
            let start = Instant::now();
            transcript.install_history_records(&records);
            let build = start.elapsed();
            let revisions: Vec<_> = transcript.revisions().collect();
            let start = Instant::now();
            transcript.install_history_records(&records);
            let replay = start.elapsed();
            let start = Instant::now();
            transcript.install_history_records(&records[records.len() - 60..]);
            let tail = start.elapsed();
            assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
            let active = RequestKey::response(format!("{:020}", count + 1).parse().unwrap(), 0);
            transcript.assistant_mut(&active);
            let start = Instant::now();
            for _ in 0..100_000 {
                assert!(!std::hint::black_box(
                    transcript.has_canonical_response(std::hint::black_box(&active))
                ));
            }
            let active_checks = start.elapsed();
            eprintln!(
                "native history {count}: build {build:?}, replay {replay:?}, tail 60 {tail:?}, 100k active checks {active_checks:?}"
            );
        }
    }

    #[test]
    fn a_loaded_finished_turn_folds_its_steps() {
        let call = |position, response: u64, id: &str| {
            record(
                position,
                json!({"response_index":response,"kind":"assistant","response":{"content":[
                    {"type":"tool_call","data":{"tool_call_id":id,"name":"read","arguments":{"path":"a"},"call_index":0}}
                ],"finish_reason":"tool_calls","usage":{}}}),
            )
        };
        let done = |position, id: &str| {
            record(
                position,
                json!({"kind":"tool_result","tool_call_id":id,"name":"read",
                    "result":{"outcome":"success","output":{"content":"text"}}}),
            )
        };
        let records = [
            record(0, json!({"kind":"user","text":"go"})),
            call(1, 0, "one"),
            done(2, "one"),
            call(3, 1, "two"),
            done(4, "two"),
            record(
                5,
                json!({"response_index":2,"kind":"assistant","response":{"content":[
                    {"type":"text","data":"done"}],"finish_reason":"stop","usage":{}}}),
            ),
            record(
                6,
                json!({"kind":"turn_finished","finished_at":"2026-11-13T10:00:05Z",
                    "requests":3,"tool_rounds":2,"model_attempts":[1,1,1],
                    "turn_id":"00000000000000000001","outcome":"completed","reason":null}),
            ),
            // A later Turn with one tool must not unfold the first.
            record(
                7,
                json!({"turn_id":"00000000000000000002","kind":"user","text":"again"}),
            ),
            record(
                8,
                json!({"turn_id":"00000000000000000002","kind":"turn_finished",
                    "finished_at":"2026-11-13T10:00:09Z","requests":1,"tool_rounds":0,
                    "model_attempts":[1],"turn_id":"00000000000000000002",
                    "outcome":"completed","reason":null}),
            ),
        ];
        let transcript = Transcript::from_history(&records);
        let groupings: Vec<_> = (0..transcript.blocks.len())
            .map(|index| match transcript.grouping(index) {
                super::super::Grouping::None => "none",
                super::super::Grouping::Head { .. } => "head",
                super::super::Grouping::Member { .. } => "member",
                super::super::Grouping::Tail { .. } => "tail",
            })
            .collect();
        assert!(groupings.contains(&"head"), "{groupings:?}");
        assert!(groupings.contains(&"member"), "{groupings:?}");
    }

    #[test]
    fn reopened_tool_calls_restore_duration_and_backfill_agent_card_metadata() {
        use crate::transcript::agent_card::AgentCard;
        use minicore_host_protocol::{DelegationAssociation, SessionRef, ToolCallRef};

        let result = serde_json::from_value::<HistoryRecordData>(json!({
            "position": 5,
            "recorded_at": "2026-11-13T10:00:12Z",
            "turn_id": "00000000000000000001",
            "response_index": 0,
            "kind": "tool_result",
            "tool_call_id": "call_agent",
            "name": "agent",
            "result": {"outcome": "success", "output": {"content": "accepted"}}
        }))
        .unwrap();
        let mut transcript = Transcript::from_history(&[result]);
        let origin = ToolCallRef {
            turn_id: "00000000000000000001".parse().unwrap(),
            tool_call_id: "call_agent".to_owned().into(),
        };
        let parent = SessionRef {
            agent_id: "00000000000000000002".parse().unwrap(),
            session_id: "00000000000000000003".parse().unwrap(),
        };
        let child = SessionRef {
            agent_id: "00000000000000000099".parse().unwrap(),
            session_id: "00000000000000000004".parse().unwrap(),
        };
        let task_message_id: minicore_host_protocol::MessageId =
            "00000000000000000005".parse().unwrap();
        let card = AgentCard::new(
            "00000000000000000006".parse().unwrap(),
            DelegationAssociation {
                parent,
                root: None,
                origin: origin.clone(),
                child,
                name: None,
                accepted_message_ids: vec![task_message_id.clone()],
                task_message_id,
                inputs: Vec::new(),
                completion: None,
            },
        );
        transcript.find_tool_mut(&origin).unwrap().agent_card = Some(Box::new(card));
        let invocation = serde_json::from_value::<HistoryRecordData>(json!({
            "position": 2,
            "recorded_at": "2026-11-13T10:00:05Z",
            "turn_id": "00000000000000000001",
            "response_index": 0,
            "kind": "assistant",
            "response": {
                "content": [{
                    "type": "tool_call",
                    "data": {
                        "tool_call_id": "call_agent",
                        "name": "agent",
                        "arguments": {
                            "action": "create",
                            "resource": "agent",
                            "name": "tokyo-agent",
                            "text": "Check Tokyo weather"
                        },
                        "call_index": 0
                    }
                }],
                "finish_reason": "tool_calls",
                "usage": {}
            }
        }))
        .unwrap();
        transcript.install_history_records(&[invocation]);
        let call = transcript.find_tool(&origin).unwrap();
        assert_eq!(call.duration, Some(std::time::Duration::from_secs(7)));
        let card = call.agent_card.as_deref().unwrap();
        assert_eq!(card.task, "Check Tokyo weather");
        assert_eq!(card.agent_name.as_deref(), Some("tokyo-agent"));
    }
}
