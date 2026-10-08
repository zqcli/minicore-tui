//! Best-effort model-argument snapshots. This owner never reads tool data,
//! validates arguments, or advances the execution lifecycle.
use std::sync::Arc;

use super::{App, SessionView};
use crate::limits::{
    TOOL_ARGUMENT_PREVIEW_BYTES, TOOL_ARGUMENT_PREVIEW_CALLS, TOOL_ARGUMENT_PREVIEW_ID_BYTES,
    TOOL_ARGUMENT_PREVIEW_TOTAL_BYTES,
};
use crate::protocol::{
    Reasoning, ToolArgumentsPreviewDataWire, ToolArgumentsPreviewStateWire as State, TurnRef,
};
use crate::state::tool::{
    ArgumentsPreview, ArgumentsPreviewFence, LiveTool, ToolFacts, ToolKey, ToolStatus,
};
use crate::state::turn::{LivePart, LiveRequest};

fn remove_from_requests(requests: &mut [LiveRequest], key: &ToolKey) {
    if let Some(request) = requests
        .iter_mut()
        .find(|r| r.request_index == key.request_index)
    {
        request
            .tools
            .retain(|tool| tool.tool_call_id != key.tool_call_id);
        request.parts.retain(|part| !matches!(part, LivePart::Tool { tool_call_id } if tool_call_id == &key.tool_call_id));
    }
}

impl App {
    /// Eviction retains the bounded revision tombstone, so old snapshots cannot
    /// bring an evicted/discarded speculative card back to life.
    pub(super) fn remove_arguments_preview(view: &mut SessionView, key: &ToolKey) {
        if !view
            .tool_presentations
            .get(key)
            .is_some_and(|f| f.arguments_preview.is_some())
        {
            return;
        }
        Arc::make_mut(&mut view.tool_presentations).remove(key);
        Arc::make_mut(&mut view.tool_folds).remove(key);
        if let Some(live) = view.live.as_mut().filter(|live| {
            live.reference
                .as_ref()
                .is_some_and(|turn| turn.loop_id == key.loop_id)
        }) {
            remove_from_requests(&mut live.requests, key);
        }
        if let Some(unsaved) = view
            .unsaved_loop
            .as_mut()
            .filter(|live| live.turn.loop_id == key.loop_id)
        {
            remove_from_requests(&mut unsaved.requests, key);
        }
        if let Some(fence) = view
            .arguments_preview_fence
            .as_mut()
            .filter(|f| f.loop_id == key.loop_id && f.request_index == key.request_index)
        {
            if let Some(call) = fence.calls.get_mut(&key.tool_call_id) {
                call.state = State::Discarded;
            }
        }
        view.transcript.invalidate();
    }

    pub(super) fn clear_arguments_previews(view: &mut SessionView) {
        let keys: Vec<_> = view
            .tool_presentations
            .iter()
            .filter(|(_, facts)| facts.arguments_preview.is_some())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            Self::remove_arguments_preview(view, &key);
        }
    }

    pub(super) fn close_arguments_previews(view: &mut SessionView, loop_id: Option<&str>) {
        if let Some(loop_id) = loop_id {
            if view
                .arguments_preview_fence
                .as_ref()
                .is_none_or(|f| f.loop_id != loop_id)
            {
                let Some(live) = view.live.as_ref().filter(|live| {
                    live.reference
                        .as_ref()
                        .is_some_and(|turn| turn.loop_id == loop_id)
                }) else {
                    return;
                };
                let request_index = live
                    .requests
                    .iter()
                    .map(|request| request.request_index)
                    .max()
                    .unwrap_or(0);
                view.arguments_preview_fence = Some(ArgumentsPreviewFence {
                    loop_id: loop_id.to_owned(),
                    request_index,
                    attempt: 0,
                    closed: true,
                    calls: Default::default(),
                });
            }
        }
        Self::clear_arguments_previews(view);
        if let Some(fence) = view.arguments_preview_fence.as_mut() {
            fence.closed = true;
        }
    }

    pub(super) fn advance_arguments_preview_request(
        view: &mut SessionView,
        turn: &TurnRef,
        request_index: u32,
    ) {
        if view.arguments_preview_fence.as_ref().is_some_and(|f| {
            f.loop_id == turn.loop_id && (f.closed || f.request_index >= request_index)
        }) {
            return;
        }
        Self::clear_arguments_previews(view);
        view.arguments_preview_fence = Some(ArgumentsPreviewFence {
            loop_id: turn.loop_id.clone(),
            request_index,
            attempt: 0,
            closed: false,
            calls: Default::default(),
        });
    }

    /// Called after every reducer pass, including cancel, transport errors,
    /// wait failures, durable replacement, session switch, and loop change.
    pub(super) fn reconcile_arguments_previews(&mut self) {
        for (session_id, view) in &mut self.sessions.known {
            let Some(fence) = view.arguments_preview_fence.as_ref() else {
                continue;
            };
            let live = view.live.as_ref();
            let current = live
                .and_then(|live| live.reference.as_ref())
                .is_some_and(|turn| turn.loop_id == fence.loop_id);
            let next_request = live.and_then(|live| {
                live.requests
                    .iter()
                    .map(|request| request.request_index)
                    .max()
            });
            let closed = self.sessions.active.as_ref() != Some(session_id)
                || !view.info.loaded
                || view.closing
                || live.is_none_or(|live| {
                    live.waiting || live.cancel_requested || live.last_result.is_some()
                })
                || !current;
            if closed {
                Self::close_arguments_previews(view, None);
            } else if let Some(request_index) =
                next_request.filter(|index| *index > fence.request_index)
            {
                let turn = view
                    .live
                    .as_ref()
                    .unwrap()
                    .reference
                    .as_ref()
                    .unwrap()
                    .clone();
                Self::advance_arguments_preview_request(view, &turn, request_index);
            }
            // No historical collection of loop/request watermarks is kept.
            if !current {
                view.arguments_preview_fence = None;
            }
        }
    }

    /// Keep the shared live mirror on the same authoritative owner when an
    /// invocation/execution arrives without a companion started event.
    pub(super) fn sync_live_arguments_upgrade(view: &mut SessionView, key: &ToolKey) {
        let Some(facts) = view.tool_presentations.get(key).cloned() else {
            return;
        };
        if let Some(live) = view.live.as_mut().filter(|live| {
            live.reference
                .as_ref()
                .is_some_and(|turn| turn.loop_id == key.loop_id)
        }) {
            if let Some(tool) = live
                .requests
                .iter_mut()
                .find(|r| r.request_index == key.request_index)
                .and_then(|r| {
                    r.tools
                        .iter_mut()
                        .find(|tool| tool.tool_call_id == key.tool_call_id)
                })
            {
                tool.display = Some(Arc::clone(&facts.display));
                tool.status = facts.status;
                tool.result = facts.result.clone();
                tool.result_truncated = facts.result_truncated;
            }
        }
    }

    pub(super) fn settle_arguments_previews(
        view: &mut SessionView,
        loop_id: &str,
        request_index: u32,
        calls: &[(&str, &str)],
    ) {
        let keys: Vec<_> = view
            .tool_presentations
            .iter()
            .filter(|(key, facts)| {
                key.loop_id == loop_id
                    && key.request_index == request_index
                    && facts.arguments_preview.is_some()
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some((_, name)) = calls.iter().find(|(id, _)| *id == key.tool_call_id) {
                if let Some(facts) = Arc::make_mut(&mut view.tool_presentations).get_mut(&key) {
                    Arc::make_mut(facts).promote_arguments_preview(name);
                }
                Self::sync_live_arguments_upgrade(view, &key);
            } else {
                Self::remove_arguments_preview(view, &key);
            }
        }
    }

    pub(super) fn on_tool_arguments_preview(&mut self, data: ToolArgumentsPreviewDataWire) {
        if self.sessions.active.as_ref() != Some(&data.turn.session_id)
            || data.meta.session_id != data.turn.session_id
            || data
                .meta
                .loop_id
                .as_ref()
                .is_some_and(|id| id != &data.turn.loop_id)
            || !matches!(data.tool_name.as_str(), "read" | "edit" | "write")
            || [
                &data.turn.session_id,
                &data.turn.loop_id,
                &data.tool_call_id,
            ]
            .iter()
            .any(|id| id.is_empty() || id.len() > TOOL_ARGUMENT_PREVIEW_ID_BYTES)
        {
            return;
        }
        let Some(view) = self.sessions.known.get_mut(&data.turn.session_id) else {
            return;
        };
        if !view.info.loaded || view.closing || !Self::bind_live_turn(view, &data.turn) {
            return;
        }
        let live = view.live.as_ref().expect("bound live turn");
        if live.waiting
            || live.cancel_requested
            || live.last_result.is_some()
            || live
                .requests
                .iter()
                .any(|r| r.request_index > data.request_index)
        {
            return;
        }
        if view
            .transcript
            .blocks
            .iter()
            .any(|block| match block.as_ref() {
                crate::state::transcript::TranscriptBlock::Assistant(assistant) => {
                    assistant.loop_id == data.turn.loop_id
                        && assistant.request_index == data.request_index
                }
                crate::state::transcript::TranscriptBlock::Tool(tool) => {
                    tool.loop_id == data.turn.loop_id && tool.request_index == data.request_index
                }
                _ => false,
            })
        {
            return;
        }
        Self::advance_arguments_preview_request(view, &data.turn, data.request_index);
        let fence = view
            .arguments_preview_fence
            .as_ref()
            .expect("current request fence");
        if fence.closed || data.attempt < fence.attempt {
            return;
        }
        if data.attempt > fence.attempt {
            Self::clear_arguments_previews(view);
            let fence = view.arguments_preview_fence.as_mut().unwrap();
            fence.attempt = data.attempt;
            fence.calls.clear();
        }
        let fence = view.arguments_preview_fence.as_mut().unwrap();
        if let Some(previous) = fence.calls.get(&data.tool_call_id) {
            if data.revision <= previous.revision
                || previous.state == State::Discarded
                || (previous.state == State::Generated && data.state == State::Generating)
            {
                return;
            }
        } else if fence.calls.len() >= TOOL_ARGUMENT_PREVIEW_CALLS {
            return;
        }
        let marker = ArgumentsPreview {
            attempt: data.attempt,
            revision: data.revision,
            state: data.state,
            partial: data.partial,
        };
        fence.calls.insert(data.tool_call_id.clone(), marker);
        let key = ToolKey::new(
            &data.turn.session_id,
            &data.turn.loop_id,
            data.request_index,
            &data.tool_call_id,
        );
        // Any real fact, including presentation/started alone, wins forever.
        if view
            .tool_presentations
            .get(&key)
            .is_some_and(|f| f.arguments_preview.is_none())
        {
            return;
        }
        if data.state == State::Discarded {
            Self::remove_arguments_preview(view, &key);
            self.prepared_conversation = None;
            return;
        }
        let used: usize = view
            .tool_presentations
            .iter()
            .filter(|(other, f)| *other != &key && f.arguments_preview.is_some())
            .map(|(_, f)| f.retained_bytes())
            .sum();
        let budget =
            TOOL_ARGUMENT_PREVIEW_BYTES.min(TOOL_ARGUMENT_PREVIEW_TOTAL_BYTES.saturating_sub(used));
        if budget == 0 {
            Self::remove_arguments_preview(view, &key);
            view.arguments_preview_fence
                .as_mut()
                .unwrap()
                .calls
                .get_mut(&key.tool_call_id)
                .unwrap()
                .state = State::Discarded;
            return;
        }
        let mut facts = ToolFacts::new(&data.tool_name);
        facts.arguments_preview = Some(marker);
        let mut display = data.display;
        // Only write content is a supported argument body. An edit preview
        // must never display a speculative diff or a half JSON object.
        if data.tool_name != "write" {
            display.expanded_input = None;
            display.input_line_count = None;
        }
        display.hidden_line_count = None;
        // Bound path independently before sharing the owned payload.
        let mut end = display.detail.len().min(4096).min(budget);
        while !display.detail.is_char_boundary(end) {
            end -= 1;
        }
        display.truncated |= end < display.detail.len();
        display.detail.truncate(end);
        display.detail.shrink_to_fit();
        if let Some(body) = display.expanded_input.as_mut() {
            let mut end = body
                .len()
                .min(budget.saturating_sub(display.detail.capacity()));
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            display.body_truncated |= end < body.len();
            display.truncated |= display.body_truncated;
            body.truncate(end);
            body.shrink_to_fit();
            // Count only retained real content; never imply hidden bytes are
            // available on expand or issue a tool.read for them.
            display.input_line_count = Some(if body.is_empty() {
                0
            } else {
                body.split('\n').count()
            });
        }
        if display.truncated || display.body_truncated {
            facts.arguments_preview.as_mut().unwrap().partial = true;
        }
        facts.display = Arc::new(display);
        let display = Arc::clone(&facts.display);
        Arc::make_mut(&mut view.tool_presentations).insert(key, Arc::new(facts));
        let request = view.live.as_mut().unwrap().ensure_request_mut(
            data.request_index,
            0,
            String::new(),
            Reasoning::Auto,
        );
        if !request.parts.iter().any(|part| matches!(part, LivePart::Tool { tool_call_id } if tool_call_id == &data.tool_call_id)) {
            request.parts.push(LivePart::Tool { tool_call_id: data.tool_call_id.clone() });
        }
        if let Some(tool) = request
            .tools
            .iter_mut()
            .find(|tool| tool.tool_call_id == data.tool_call_id)
        {
            tool.name = data.tool_name;
            tool.display = Some(display);
            tool.status = ToolStatus::Pending;
        } else {
            request.tools.push(LiveTool {
                tool_call_id: data.tool_call_id,
                name: data.tool_name,
                status: ToolStatus::Pending,
                progress: None,
                display: Some(display),
                result: None,
                result_truncated: false,
                expanded: false,
            });
        }
        self.prepared_conversation = None;
    }
}
