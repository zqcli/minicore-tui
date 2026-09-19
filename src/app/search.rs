//! Conversation search, prompt navigation and the jump machinery
//! (spec §17.1/§17.2).
//!
//! The search panel never scans a large body on the App thread: the loaded
//! scope runs in the owned scan worker, and the explicit full-session scope
//! runs a pinned `session.read` chain whose items are decoded and scanned by
//! the existing single decode worker. Only bounded match summaries are kept.
//!
//! Jumps reuse the prepared conversation's content-based `ScrollAnchor`: a
//! match that is not resident first asks for the exact item window, then the
//! anchor lands on the nearest retained copy row. Fold expansion performed by
//! a jump is a temporary override that is restored when the search closes.

use super::*;
use crate::app::history::ReadPage;
use crate::state::search::{
    SearchCoverage, SearchMatch, SearchPanelMode, SearchPanelState, SearchScope, SearchSource,
    SearchStatus,
};

/// One jump that could not be performed yet because its target item is not
/// resident. The windowed read installs the page and then resumes the jump.
#[derive(Debug, Clone)]
pub(super) enum PendingSearchJump {
    /// A concrete match, anchored once its item window is loaded.
    Match(Box<SearchMatch>),
    /// Navigate to the previous/next user prompt (`-1`/`1`). `before` is the
    /// item index the search may not cross.
    Prompt {
        direction: i32,
        before: Option<usize>,
    },
}

/// A fold override a jump installed temporarily. Closing the search restores
/// the exact previous user choice (including "no override").
#[derive(Debug, Clone)]
pub(super) enum FoldRestore {
    Tool {
        key: ToolKey,
        previous: Option<FoldOverride>,
    },
    Reasoning {
        key: ReasoningKey,
        previous: Option<FoldOverride>,
    },
}

impl App {
    /// The open search panel, if the dock currently owns it.
    pub(crate) fn search_panel(&self) -> Option<&SearchPanelState> {
        match &self.dock {
            Dock::Search(state) => Some(state),
            _ => None,
        }
    }

    pub(crate) fn search_panel_mut(&mut self) -> Option<&mut SearchPanelState> {
        match &mut self.dock {
            Dock::Search(state) => Some(state),
            _ => None,
        }
    }

    /// Opens the search panel for the active session and starts a scan
    /// immediately when a query was supplied.
    pub(super) fn open_search(&mut self, query: String, scope: SearchScope) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        let Some((session_id, epoch)) = self
            .sessions
            .active
            .clone()
            .and_then(|id| Some((id.clone(), self.sessions.known.get(&id)?.session_epoch)))
        else {
            self.notice(NoticeLevel::Info, "open a session before searching");
            return Vec::new();
        };
        if self.search_panel().is_some() {
            self.close_search();
        }
        let panel = SearchPanelState::new(session_id, epoch, query, scope);
        self.dock = Dock::Search(panel);
        self.panel_scroll = 0;
        if self.search_panel().is_some_and(SearchPanelState::has_query) {
            self.start_search_scan()
        } else {
            Vec::new()
        }
    }

    /// Closes the search panel and restores every temporary fold override.
    pub(super) fn close_search(&mut self) {
        self.restore_search_folds();
        self.pending_search_jump = None;
        self.search_scan = None;
        if matches!(self.dock, Dock::Search(_)) {
            self.dock = Dock::Composer;
        }
    }

    /// Esc inside the panel: leave the result list for the query input first,
    /// then close the search. It never cancels a running turn (spec §17).
    pub(super) fn search_escape(&mut self) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        match panel.mode {
            SearchPanelMode::Results if panel.has_query() => {
                if let Some(panel) = self.search_panel_mut() {
                    panel.mode = SearchPanelMode::Input;
                }
                Vec::new()
            }
            _ => {
                self.close_search();
                Vec::new()
            }
        }
    }

    pub(super) fn search_type_char(&mut self, ch: char) {
        let Some(panel) = self.search_panel_mut() else {
            return;
        };
        panel.mode = SearchPanelMode::Input;
        let cursor = panel.query_cursor.min(panel.query.len());
        let cursor = floor_char_boundary(&panel.query, cursor);
        panel.query.insert(cursor, ch);
        panel.query_cursor = cursor + ch.len_utf8();
    }

    pub(super) fn search_backspace(&mut self) {
        let Some(panel) = self.search_panel_mut() else {
            return;
        };
        panel.mode = SearchPanelMode::Input;
        let cursor = floor_char_boundary(&panel.query, panel.query_cursor.min(panel.query.len()));
        let Some(previous) = panel.query[..cursor].chars().next_back() else {
            return;
        };
        let start = cursor - previous.len_utf8();
        panel.query.replace_range(start..cursor, "");
        panel.query_cursor = start;
    }

    pub(super) fn search_clear(&mut self) {
        if let Some(panel) = self.search_panel_mut() {
            panel.query.clear();
            panel.query_cursor = 0;
            panel.mode = SearchPanelMode::Input;
        }
    }

    pub(super) fn search_move(&mut self, delta: i32) {
        if let Some(panel) = self.search_panel_mut() {
            if panel.mode == SearchPanelMode::Results {
                panel.move_cursor(delta);
            }
        }
    }

    /// Enter: in the input line it starts a new generation; in the result list
    /// it jumps to the highlighted match.
    pub(super) fn search_confirm(&mut self) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        match panel.mode {
            SearchPanelMode::Input => {
                if !panel.has_query() {
                    return Vec::new();
                }
                if let Some(panel) = self.search_panel_mut() {
                    panel.mode = SearchPanelMode::Results;
                }
                self.start_search_scan()
            }
            SearchPanelMode::Results => self.jump_to_selected_match(),
        }
    }

    /// `n`/`p`: move to the next/previous match and jump immediately.
    pub(super) fn search_step(&mut self, delta: i32) -> Vec<AppCommand> {
        if let Some(panel) = self.search_panel_mut() {
            if panel.mode != SearchPanelMode::Results {
                panel.mode = SearchPanelMode::Results;
            }
            panel.step_cursor(delta);
        }
        self.jump_to_selected_match()
    }

    pub(super) fn search_toggle_scope(&mut self) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        if panel.scanning() {
            self.notice(
                NoticeLevel::Info,
                "stop the running search before changing its scope",
            );
            return Vec::new();
        }
        let scope = panel.scope.toggled();
        if let Some(panel) = self.search_panel_mut() {
            panel.scope = scope;
        }
        self.notice(
            NoticeLevel::Info,
            format!("search scope: {}", scope.label()),
        );
        if self.search_panel().is_some_and(SearchPanelState::has_query) {
            self.start_search_scan()
        } else {
            Vec::new()
        }
    }

    /// Stops the running scan. Coverage stays honest: the matches already
    /// found are kept and the panel reports the scan as incomplete.
    pub(super) fn search_stop(&mut self) {
        let Some(panel) = self.search_panel_mut() else {
            return;
        };
        if !panel.scanning() {
            return;
        }
        panel.status = SearchStatus::Stopped;
        panel.coverage.stopped = true;
        panel.coverage.complete = false;
        self.search_scan = None;
        self.notice(
            NoticeLevel::Info,
            "search stopped; the matches found so far are kept",
        );
    }

    /// Starts one generation for the current query and scope.
    pub(super) fn start_search_scan(&mut self) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        let query = panel.query.trim().to_owned();
        if query.is_empty() {
            return Vec::new();
        }
        let scope = panel.scope;
        let session_id = panel.session_id.clone();
        let session_epoch = panel.session_epoch;
        let generation = self.search_generation.wrapping_add(1);
        self.search_generation = generation;
        // A new generation invalidates the previous in-flight work; its
        // result is dropped by the identity check below.
        self.search_scan = None;
        let Some(panel) = self.search_panel_mut() else {
            return Vec::new();
        };
        panel.generation = generation;
        panel.error = None;
        panel.matches.clear();
        panel.cursor = 0;
        panel.coverage = SearchCoverage::default();
        panel.status = match scope {
            SearchScope::Loaded => SearchStatus::ScanningLoaded,
            SearchScope::FullSession => SearchStatus::ScanningFull,
        };
        match scope {
            SearchScope::Loaded => {
                let snapshot = self.loaded_scan_snapshot(&session_id);
                let Some(snapshot) = snapshot else {
                    if let Some(panel) = self.search_panel_mut() {
                        panel.status = SearchStatus::Ready;
                        panel.coverage.complete = true;
                        panel.coverage.loaded_items = 0;
                        panel.coverage.total_items = 0;
                    }
                    return Vec::new();
                };
                if let Some(panel) = self.search_panel_mut() {
                    panel.coverage.loaded_items = snapshot.known_items;
                    panel.coverage.total_items = snapshot.total_items;
                    panel.coverage.scanned_items = snapshot.known_items;
                    panel.coverage.complete = true;
                }
                vec![AppCommand::LocalScan(Box::new(LocalScanRequest {
                    identity: LocalScanIdentity {
                        session_id,
                        session_epoch,
                        generation,
                    },
                    needle: query,
                    include_thinking: self.reasoning_visible,
                    blocks: snapshot.blocks,
                    live: snapshot.live,
                }))]
            }
            SearchScope::FullSession => self.start_full_search_scan(),
        }
    }

    /// The loaded-content snapshot: durable blocks are shared by `Arc`; live
    /// loop text is copied (bounded by the running turn).
    fn loaded_scan_snapshot(
        &mut self,
        session_id: &SessionId,
    ) -> Option<crate::state::search::LoadedScanSnapshot> {
        let view = self.sessions.known.get(session_id)?;
        let blocks = Arc::clone(&view.transcript.blocks);
        let known_items = view.transcript.window.len();
        let total_items = view.transcript.window.total();
        let mut live = Vec::new();
        if let Some(loop_state) = view.live.as_ref() {
            live.push(crate::jobs::LiveScanText {
                source: SearchSource::Prompt,
                index: None,
                loop_id: loop_state.reference.as_ref().map(|r| r.loop_id.clone()),
                request_index: None,
                ordinal: 0,
                tool_call_id: None,
                text: loop_state.user_text.clone(),
            });
            for request in &loop_state.requests {
                let loop_id = loop_state.reference.as_ref().map(|r| r.loop_id.clone());
                let mut text_ordinal = 0u32;
                let mut reasoning_ordinal = 0u32;
                for part in &request.parts {
                    match part {
                        crate::state::turn::LivePart::Text(text) => {
                            live.push(crate::jobs::LiveScanText {
                                source: SearchSource::AssistantText,
                                index: None,
                                loop_id: loop_id.clone(),
                                request_index: Some(request.request_index),
                                ordinal: text_ordinal,
                                tool_call_id: None,
                                text: text.clone(),
                            });
                            text_ordinal += 1;
                        }
                        crate::state::turn::LivePart::Reasoning(text) => {
                            if self.reasoning_visible {
                                live.push(crate::jobs::LiveScanText {
                                    source: SearchSource::Thinking,
                                    index: None,
                                    loop_id: loop_id.clone(),
                                    request_index: Some(request.request_index),
                                    ordinal: reasoning_ordinal,
                                    tool_call_id: None,
                                    text: text.clone(),
                                });
                            }
                            reasoning_ordinal += 1;
                        }
                        crate::state::turn::LivePart::Tool { .. } => {}
                    }
                }
                for tool in &request.tools {
                    live.push(crate::jobs::LiveScanText {
                        source: SearchSource::ToolName,
                        index: None,
                        loop_id: loop_id.clone(),
                        request_index: Some(request.request_index),
                        ordinal: 0,
                        tool_call_id: Some(tool.tool_call_id.clone()),
                        text: tool.name.clone(),
                    });
                    if let Some(result) = tool.result.as_deref() {
                        live.push(crate::jobs::LiveScanText {
                            source: SearchSource::ToolResult,
                            index: None,
                            loop_id: loop_id.clone(),
                            request_index: Some(request.request_index),
                            ordinal: 0,
                            tool_call_id: Some(tool.tool_call_id.clone()),
                            text: result.to_owned(),
                        });
                    }
                }
            }
        }
        Some(crate::state::search::LoadedScanSnapshot {
            blocks,
            live,
            known_items,
            total_items,
        })
    }

    /// Installs one loaded-content scan result when it still belongs to the
    /// open panel and its generation.
    pub(super) fn on_local_scan_finished(
        &mut self,
        outcome: crate::jobs::LocalScanOutcome,
    ) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        if panel.generation != outcome.identity.generation
            || panel.session_id != outcome.identity.session_id
            || panel.session_epoch != outcome.identity.session_epoch
        {
            return Vec::new();
        }
        let Some(panel) = self.search_panel_mut() else {
            return Vec::new();
        };
        panel.matches = outcome.matches;
        panel.cursor = 0;
        panel.coverage.truncated = outcome.truncated;
        panel.coverage.complete = !outcome.truncated;
        panel.status = SearchStatus::Ready;
        panel.mode = if panel.matches.is_empty() {
            SearchPanelMode::Input
        } else {
            SearchPanelMode::Results
        };
        Vec::new()
    }

    /// Jump to the highlighted match (spec §17.2).
    pub(super) fn jump_to_selected_match(&mut self) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        let Some(target) = panel.selected().cloned() else {
            return Vec::new();
        };
        if self.active_session_id().as_deref() != Some(panel.session_id.as_str()) {
            self.notice(
                NoticeLevel::Info,
                "the search belongs to another session; open it to jump",
            );
            return Vec::new();
        }
        self.jump_to_match(&target)
    }

    /// Anchors the conversation on one match, reading its item window first
    /// when it is not resident.
    pub(super) fn jump_to_match(&mut self, target: &SearchMatch) -> Vec<AppCommand> {
        let Some(session_id) = self.active_session_id() else {
            return Vec::new();
        };
        self.install_search_folds(target);
        let resident = target
            .index
            .is_some_and(|index| self.history_item_resident(&session_id, index));
        if let Some(index) = target.index {
            if !resident {
                self.pending_search_jump = Some((
                    session_id.clone(),
                    PendingSearchJump::Match(Box::new(target.clone())),
                ));
                return self.request_history_window_at(&session_id, index);
            }
        }
        self.anchor_on_match(&session_id, target);
        Vec::new()
    }

    /// Previous/next user prompt and latest (spec §17.2). Steering rows are
    /// searchable but never prompt jump targets.
    pub(super) fn prompt_jump(&mut self, direction: i32) -> Vec<AppCommand> {
        let Some(session_id) = self.active_session_id() else {
            self.notice(NoticeLevel::Info, "open a session first");
            return Vec::new();
        };
        let prompts = self.loaded_prompts(&session_id);
        let current = self.current_prompt_position(&session_id);
        let target = match (direction, current) {
            (d, Some(current)) if d < 0 => {
                prompts.iter().filter(|entry| entry.0 < current).next_back()
            }
            (d, Some(current)) if d > 0 => prompts.iter().find(|entry| entry.0 > current),
            // No known viewport position: `/prev` starts at the newest loaded
            // prompt, `/next` at the oldest one.
            (d, None) if d < 0 => prompts.last(),
            (_, None) => prompts.first(),
            _ => None,
        };
        if let Some((index, loop_id)) = target.cloned() {
            let target = SearchMatch {
                index: Some(index),
                source: SearchSource::Prompt,
                loop_id: Some(loop_id),
                request_index: None,
                ordinal: 0,
                tool_call_id: None,
                preview: String::new(),
                source_offset: 0,
                byte_range: 0..0,
            };
            return self.jump_to_match(&target);
        }

        // Nothing loaded in that direction: read the window that must contain
        // the older/newer prompt instead of guessing (spec §17.2).
        let before = current.or_else(|| prompts.first().map(|entry| entry.0));
        if direction < 0 {
            let Some(before) = before else {
                self.notice(NoticeLevel::Info, "no earlier prompt is loaded");
                return Vec::new();
            };
            if before == 0 {
                self.notice(NoticeLevel::Info, "no earlier prompt in this session");
                return Vec::new();
            }
            self.pending_search_jump = Some((
                session_id.clone(),
                PendingSearchJump::Prompt {
                    direction,
                    before: Some(before),
                },
            ));
            self.search_jump_attempts = 0;
            return self.request_history_window_at(&session_id, before.saturating_sub(1));
        }
        // Forward: only the not-yet-loaded tail can hold a newer prompt.
        if let Some((index, loop_id)) = prompts.last().cloned() {
            if current.is_none_or(|current| index > current) {
                let target = SearchMatch {
                    index: Some(index),
                    source: SearchSource::Prompt,
                    loop_id: Some(loop_id),
                    request_index: None,
                    ordinal: 0,
                    tool_call_id: None,
                    preview: String::new(),
                    source_offset: 0,
                    byte_range: 0..0,
                };
                return self.jump_to_match(&target);
            }
        }
        if self
            .sessions
            .known
            .get(&session_id)
            .is_some_and(|view| view.transcript.next_cursor.is_some())
        {
            self.pending_search_jump = Some((
                session_id.clone(),
                PendingSearchJump::Prompt {
                    direction,
                    before: before.or(current),
                },
            ));
            self.search_jump_attempts = 0;
            return self.request_history(&session_id).into_iter().collect();
        }
        self.notice(NoticeLevel::Info, "no later prompt in the loaded history");
        Vec::new()
    }

    /// `/latest`: jump to the newest loaded prompt and follow the tail.
    pub(super) fn jump_latest(&mut self) -> Vec<AppCommand> {
        let Some(session_id) = self.active_session_id() else {
            self.notice(NoticeLevel::Info, "open a session first");
            return Vec::new();
        };
        if let Some((index, loop_id)) = self.loaded_prompts(&session_id).last().cloned() {
            let target = SearchMatch {
                index: Some(index),
                source: SearchSource::Prompt,
                loop_id: Some(loop_id),
                request_index: None,
                ordinal: 0,
                tool_call_id: None,
                preview: String::new(),
                source_offset: 0,
                byte_range: 0..0,
            };
            return self.jump_to_match(&target);
        }
        if let Some(view) = self.active_session_mut() {
            view.scroll.follow_tail = true;
            view.scroll.offset = 0;
            view.scroll.new_content = false;
        }
        Vec::new()
    }

    /// Resident durable transcript indices for one session, oldest first.
    fn loaded_prompts(&self, session_id: &SessionId) -> Vec<(usize, String)> {
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        let mut prompts = view
            .transcript
            .blocks
            .iter()
            .filter_map(|block| match block.as_ref() {
                crate::state::transcript::TranscriptBlock::User(user)
                    if user.kind == crate::protocol::UserMessageKindWire::Prompt =>
                {
                    Some((user.index?, user.loop_id.clone().unwrap_or_default()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        prompts.sort_by_key(|entry| entry.0);
        prompts
    }

    /// The item index the viewport currently sits on, used as the relative
    /// origin for `/prev` and `/next`.
    fn current_prompt_position(&self, session_id: &SessionId) -> Option<usize> {
        // The scroll anchor is the durable position; the visible window is
        // only a refinement of it.
        let anchor = self
            .sessions
            .known
            .get(session_id)
            .and_then(|view| view.scroll.anchor.as_ref())
            .and_then(|anchor| anchor.section_id.history_index);
        let Some(prepared) = self.prepared_conversation.as_ref() else {
            return anchor;
        };
        if prepared.session_id.as_deref() != Some(session_id.as_str()) {
            return anchor;
        }
        let _view = self.sessions.known.get(session_id)?;
        let height = self.viewport.1.max(1);
        let position = crate::ui::transcript::scroll_position(self, prepared.total_rows(), height);
        let start = position.offset;
        let end = start
            .saturating_add(position.visible_rows)
            .min(prepared.total_rows());
        (start..end)
            .find_map(|row| {
                let section = prepared
                    .sections
                    .iter()
                    .find(|section| section.rows.contains(&row))?;
                section.id.history_index
            })
            .or(anchor)
    }

    fn anchor_on_match(&mut self, session_id: &SessionId, target: &SearchMatch) {
        let section_id = self.search_section_id(session_id, target);
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return;
        };
        view.scroll.follow_tail = false;
        view.scroll.new_content = false;
        view.scroll.anchor = Some(ScrollAnchor {
            section_id,
            source_offset: target.source_offset,
            screen_row: 3,
        });
    }

    fn search_section_id(&self, session_id: &SessionId, target: &SearchMatch) -> SectionId {
        SectionId {
            session_id: session_id.as_str().into(),
            loop_id: target.loop_id.as_deref().map(Arc::from),
            request_index: target.request_index,
            kind: target.source.section_kind(),
            ordinal: target.ordinal,
            tool_call_id: target.tool_call_id.as_deref().map(Arc::from),
            history_index: target.index,
        }
    }

    /// Records and installs the temporary fold expansion a jump needs, so a
    /// match inside a collapsed reasoning run or tool card is visible.
    fn install_search_folds(&mut self, target: &SearchMatch) {
        let session_id = self.active_session_id();
        let Some(session_id) = session_id else {
            return;
        };
        match target.source {
            SearchSource::Thinking => {
                let (Some(loop_id), Some(request_index)) =
                    (target.loop_id.clone(), target.request_index)
                else {
                    return;
                };
                let key = ReasoningKey::new(&loop_id, request_index, target.ordinal);
                let Some(view) = self.sessions.known.get_mut(&session_id) else {
                    return;
                };
                let previous = view.reasoning_folds.get(&key).copied();
                if previous == Some(FoldOverride::Expanded) {
                    return;
                }
                view.reasoning_folds
                    .insert(key.clone(), FoldOverride::Expanded);
                self.search_fold_restores
                    .push(FoldRestore::Reasoning { key, previous });
                view.transcript.invalidate();
            }
            SearchSource::ToolName | SearchSource::ToolResult => {
                let (Some(loop_id), Some(request_index), Some(tool_call_id)) = (
                    target.loop_id.clone(),
                    target.request_index,
                    target.tool_call_id.clone(),
                ) else {
                    return;
                };
                let key = ToolKey::new(&session_id, &loop_id, request_index, &tool_call_id);
                let Some(view) = self.sessions.known.get_mut(&session_id) else {
                    return;
                };
                let previous = view.tool_folds.get(&key).copied();
                if previous == Some(FoldOverride::Expanded) {
                    return;
                }
                view.tool_folds.insert(key.clone(), FoldOverride::Expanded);
                self.search_fold_restores
                    .push(FoldRestore::Tool { key, previous });
                view.transcript.invalidate();
            }
            _ => {}
        }
    }

    /// Restores every fold value a jump changed, including removing overrides
    /// the user never set.
    fn restore_search_folds(&mut self) {
        let restores = std::mem::take(&mut self.search_fold_restores);
        for restore in restores.into_iter().rev() {
            match restore {
                FoldRestore::Tool { key, previous } => {
                    let Some(view) = self
                        .sessions
                        .active
                        .clone()
                        .and_then(|id| self.sessions.known.get_mut(&id))
                    else {
                        continue;
                    };
                    match previous {
                        Some(value) => {
                            view.tool_folds.insert(key, value);
                        }
                        None => {
                            view.tool_folds.remove(&key);
                        }
                    }
                    view.transcript.invalidate();
                }
                FoldRestore::Reasoning { key, previous } => {
                    let Some(view) = self
                        .sessions
                        .active
                        .clone()
                        .and_then(|id| self.sessions.known.get_mut(&id))
                    else {
                        continue;
                    };
                    match previous {
                        Some(value) => {
                            view.reasoning_folds.insert(key, value);
                        }
                        None => {
                            view.reasoning_folds.remove(&key);
                        }
                    }
                    view.transcript.invalidate();
                }
            }
        }
    }

    fn history_item_resident(&self, session_id: &SessionId, index: usize) -> bool {
        self.sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.transcript.window.item(index).is_some())
    }

    fn active_session_id(&self) -> Option<SessionId> {
        self.sessions.active.clone()
    }

    /// Called after a history page installs items for `session_id`: a pending
    /// jump whose target is now resident completes.
    pub(super) fn on_search_history_progress(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        let Some((pending_session, pending)) = self.pending_search_jump.clone() else {
            return Vec::new();
        };
        if &pending_session != session_id {
            return Vec::new();
        }
        match pending {
            PendingSearchJump::Match(target) => {
                let resident = target
                    .index
                    .is_some_and(|index| self.history_item_resident(session_id, index));
                if resident {
                    self.pending_search_jump = None;
                    let target = *target;
                    self.anchor_on_match(session_id, &target);
                    return Vec::new();
                }
            }
            PendingSearchJump::Prompt { direction, before } => {
                let prompts = self.loaded_prompts(session_id);
                let found = match direction {
                    d if d < 0 => before
                        .and_then(|before| {
                            prompts.iter().filter(|entry| entry.0 < before).next_back()
                        })
                        .or_else(|| before.is_none().then(|| prompts.last()).flatten()),
                    _ => before.and_then(|before| prompts.iter().find(|entry| entry.0 > before)),
                };
                if let Some((index, loop_id)) = found.cloned() {
                    self.pending_search_jump = None;
                    let target = SearchMatch {
                        index: Some(index),
                        source: SearchSource::Prompt,
                        loop_id: Some(loop_id),
                        request_index: None,
                        ordinal: 0,
                        tool_call_id: None,
                        preview: String::new(),
                        source_offset: 0,
                        byte_range: 0..0,
                    };
                    self.anchor_on_match(session_id, &target);
                    return Vec::new();
                }
                // Still nothing in this window: read the next earlier range
                // while the session's first item is not loaded yet.
                if direction < 0 {
                    let oldest = self.loaded_prompts(session_id).first().map(|entry| entry.0);
                    let earliest_loaded = self
                        .sessions
                        .known
                        .get(session_id)
                        .and_then(|view| view.transcript.window.loaded_ranges().first())
                        .map(|range| range.start)
                        .unwrap_or(0);
                    let loaded_from_start = self.history_item_resident(session_id, 0);
                    if !loaded_from_start && earliest_loaded > 0 {
                        let next = earliest_loaded.saturating_sub(1);
                        if let Some((_, PendingSearchJump::Prompt { before, .. })) =
                            self.pending_search_jump.as_mut()
                        {
                            *before = Some(earliest_loaded);
                        }
                        let _ = oldest;
                        return self.request_history_window_at(session_id, next);
                    }
                    self.pending_search_jump = None;
                    self.notice(NoticeLevel::Info, "no earlier prompt in this session");
                }
            }
        }
        Vec::new()
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// One explicit full-session search scan (spec §17.1). It owns a pinned
/// `session.read` chain and its own assembler; decoded items are scanned by
/// the owned decode worker and only summaries come back.
#[derive(Debug)]
pub(super) struct SearchScan {
    pub session_id: String,
    pub generation: u64,
    pub session_epoch: u64,
    pub pin: Option<crate::protocol::read::SnapshotPin>,
    pub next: Option<crate::protocol::ReadCursor>,
    /// The persistent assembler for the scan chain; an item may span pages.
    pub page: Option<ReadPage>,
    pub stop: bool,
    pub terminal: bool,
    /// Scan-local counters so coverage stays correct even when the panel is
    /// replaced mid-scan.
    pub scanned: usize,
    pub large: usize,
}

impl App {
    /// Starts the explicit full-session scan: a pinned `session.read` chain
    /// from item 0, stopping at the captured prefix so a new turn never joins
    /// this search (spec §17.1, §17.4).
    pub(super) fn start_full_search_scan(&mut self) -> Vec<AppCommand> {
        let Some(panel) = self.search_panel() else {
            return Vec::new();
        };
        let session_id = panel.session_id.clone();
        let generation = panel.generation;
        let Some(epoch) = self
            .sessions
            .known
            .get(&session_id)
            .map(|view| view.session_epoch)
        else {
            return Vec::new();
        };
        let pin = self
            .sessions
            .known
            .get(&session_id)
            .and_then(|view| view.transcript.window.pin().cloned());
        if let Some(panel) = self.search_panel_mut() {
            panel.coverage.total_items = pin.as_ref().map_or(0, |pin| pin.total);
        }
        self.search_scan = Some(SearchScan {
            session_id,
            generation,
            session_epoch: epoch,
            pin,
            next: None,
            page: None,
            stop: false,
            terminal: false,
            scanned: 0,
            large: 0,
        });
        self.request_search_page()
    }

    /// Requests the next scan page through the two shared read-only slots.
    fn request_search_page(&mut self) -> Vec<AppCommand> {
        let Some(scan) = self.search_scan.as_ref() else {
            return Vec::new();
        };
        if scan.stop || scan.terminal {
            return Vec::new();
        }
        let session_id = scan.session_id.clone();
        let generation = scan.generation;
        let cursor = scan.next.unwrap_or_else(crate::protocol::ReadCursor::start);
        let pin = scan.pin.clone();
        let probe = pin.is_none();
        let id = self.next_request_id();
        let key = crate::app::queries::QueryKey::Search {
            session_id: session_id.clone(),
            generation,
        };
        match self.queries.request_query(key, id) {
            crate::app::queries::QueryAdmission::Admitted => {}
            crate::app::queries::QueryAdmission::Coalesced
            | crate::app::queries::QueryAdmission::Busy => {
                // The key is queued by QuerySlots and re-issued by
                // `resume_search_scan` once a slot frees; no request is built.
                return Vec::new();
            }
        }
        let (limit, max_bytes) = if probe {
            (
                crate::protocol::READ_PROBE_LIMIT,
                crate::protocol::READ_PROBE_MAX_BYTES,
            )
        } else {
            (READ_PAGE_LIMIT, READ_PAGE_MAX_BYTES)
        };
        let request = OutgoingRequest::session_read(
            id,
            &session_id,
            Some(cursor),
            limit,
            max_bytes,
            pin.as_ref(),
        );
        self.pending_requests.insert(
            id,
            RequestKind::SearchRead {
                session_id,
                generation,
            },
        );
        vec![AppCommand::Rpc(request)]
    }

    /// Re-issues a queued scan page after a read slot became available.
    pub(super) fn resume_search_scan(
        &mut self,
        session_id: &str,
        generation: u64,
    ) -> Vec<AppCommand> {
        let owned = self
            .search_scan
            .as_ref()
            .is_some_and(|scan| scan.session_id == session_id && scan.generation == generation);
        if !owned {
            return Vec::new();
        }
        self.request_search_page()
    }

    /// Applies one scan page and queues its items for the decode worker.
    pub(super) fn on_search_read_response(
        &mut self,
        session_id: &SessionId,
        generation: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let owned = self
            .search_scan
            .as_ref()
            .is_some_and(|scan| scan.session_id == *session_id && scan.generation == generation);
        if !owned {
            return Vec::new();
        }
        let page = match response.parse_session_read() {
            Ok(page) => page,
            Err(error) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("search page for {session_id} is not readable: {error}"),
                );
                self.finish_search_scan(false, 1);
                return Vec::new();
            }
        };
        if page.session.session_id != *session_id {
            self.finish_search_scan(false, 1);
            return Vec::new();
        }
        let pin = page.pin();
        let next = page.next_cursor;
        let records_truncated = page.records_truncated;
        let total = page.total;
        let next_is_none = next.is_none();
        let previous_cursor = self
            .search_scan
            .as_ref()
            .and_then(|scan| scan.page.as_ref().map(|page| page.cursor))
            .unwrap_or_else(crate::protocol::ReadCursor::start);
        let (cursor, mut page_state) = {
            let Some(scan) = self.search_scan.as_mut() else {
                return Vec::new();
            };
            if scan.pin.is_none() {
                scan.pin = Some(pin.clone());
            }
            scan.next = next;
            scan.terminal = next_is_none;
            let page_state = scan
                .page
                .take()
                .unwrap_or_else(|| ReadPage::new(previous_cursor, None, 0));
            let cursor = page_state.cursor;
            (cursor, page_state)
        };
        let want_pin = self.search_scan.as_ref().and_then(|scan| scan.pin.clone());
        page_state.cursor = cursor;
        page_state.want_pin = want_pin;
        page_state.pending_page = None;
        if records_truncated {
            if let Some(panel) = self.search_panel_mut() {
                panel.coverage.records_truncated = true;
            }
        }
        if let Some(panel) = self.search_panel_mut() {
            panel.coverage.total_items = total;
        }
        let mut failure = None;
        for chunk in &page.items {
            match page_state.assembler.push(chunk.clone()) {
                Ok(crate::protocol::read::Assembled::Pending) => {}
                Ok(crate::protocol::read::Assembled::EncodedItem { item }) => {
                    page_state.pending_encoded.push_back(item);
                }
                Ok(crate::protocol::read::Assembled::LargeItem { .. }) => {
                    if let Some(scan) = self.search_scan.as_mut() {
                        scan.large += 1;
                    }
                }
                Ok(crate::protocol::read::Assembled::LargeItemPending { index, .. }) => {
                    if let Some(scan) = self.search_scan.as_mut() {
                        scan.large += 1;
                    }
                    failure = Some(format!(
                        "large item {index} cannot be searched automatically; coverage is incomplete"
                    ));
                    break;
                }
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        let Some(scan) = self.search_scan.as_mut() else {
            return Vec::new();
        };
        scan.page = Some(page_state);
        if let Some(detail) = failure {
            self.notice(NoticeLevel::Warning, format!("search scan: {detail}"));
            self.finish_search_scan(false, 0);
            return Vec::new();
        }
        if !self
            .search_scan
            .as_ref()
            .is_some_and(SearchScan::has_pending_decode)
        {
            return self.advance_search_scan();
        }
        self.queue_search_decode();
        Vec::new()
    }

    pub(super) fn queue_search_decode(&mut self) {
        if self.pending_decode.is_some() || self.decode_in_flight.is_some() {
            return;
        }
        let Some((session_epoch, generation, item, needle, include_thinking)) =
            self.search_scan.as_ref().and_then(|scan| {
                scan.page
                    .as_ref()
                    .and_then(|page| page.pending_encoded.front().cloned())
                    .map(|item| {
                        let needle = self
                            .search_panel()
                            .map(|panel| panel.query.trim().to_owned())
                            .unwrap_or_default();
                        (
                            scan.session_epoch,
                            scan.generation,
                            item,
                            needle,
                            self.reasoning_visible,
                        )
                    })
            })
        else {
            return;
        };
        let session_id = self
            .search_scan
            .as_ref()
            .map(|scan| scan.session_id.clone())
            .unwrap_or_default();
        let identity = crate::jobs::DecodeIdentity {
            session_epoch,
            read_chain: generation,
            target: crate::jobs::DecodeTarget::SearchScan {
                session_id,
                generation,
                index: item.index,
            },
        };
        let request = crate::jobs::DecodeRequest {
            identity: identity.clone(),
            fingerprint: crate::app::history::encoded_item_fingerprint(&item),
            item,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan: Some(Box::new(crate::state::search::ScanSpec {
                needle,
                include_thinking,
            })),
        };
        self.decode_in_flight = Some(identity);
        self.pending_decode = Some(request);
    }

    /// Installs one scanned item's summaries and continues the chain.
    pub(super) fn finish_search_item_decoded(
        &mut self,
        session_id: &SessionId,
        generation: u64,
        index: usize,
        fingerprint: u64,
        outcome: &crate::jobs::DecodeOutcome,
    ) -> Vec<AppCommand> {
        let owned = self
            .search_scan
            .as_ref()
            .is_some_and(|scan| scan.session_id == *session_id && scan.generation == generation);
        if !owned {
            return Vec::new();
        }
        let Some(scan) = self.search_scan.as_mut() else {
            return Vec::new();
        };
        let expected = scan
            .page
            .as_ref()
            .and_then(|page| page.pending_encoded.front().cloned());
        let Some(expected) = expected else {
            return Vec::new();
        };
        if expected.index != index
            || crate::app::history::encoded_item_fingerprint(&expected) != fingerprint
        {
            return Vec::new();
        }
        if let Some(page) = scan.page.as_mut() {
            page.pending_encoded.pop_front();
        }
        scan.scanned = scan.scanned.saturating_add(1);
        if let Some(scan_outcome) = outcome.scan.as_deref() {
            self.install_search_matches(&scan_outcome.matches);
        } else if let Err(detail) = &outcome.result {
            if let Some(panel) = self.search_panel_mut() {
                panel.coverage.failed_items = panel.coverage.failed_items.saturating_add(1);
            }
            self.notice(
                NoticeLevel::Warning,
                format!("search item {index} is not readable: {detail}"),
            );
        }
        if self
            .search_scan
            .as_ref()
            .is_some_and(SearchScan::has_pending_decode)
        {
            self.queue_search_decode();
            return Vec::new();
        }
        self.advance_search_scan()
    }

    /// After one page's items are all scanned: stop on the cap, finish at the
    /// captured end, or request the next page.
    fn advance_search_scan(&mut self) -> Vec<AppCommand> {
        let room = self
            .search_panel()
            .is_some_and(crate::state::search::SearchPanelState::has_match_room);
        let stop_early = !room;
        let terminal = self.search_scan.as_ref().is_some_and(|scan| scan.terminal);
        if stop_early {
            if let Some(panel) = self.search_panel_mut() {
                panel.coverage.truncated = true;
            }
            self.finish_search_scan(true, 0);
            return Vec::new();
        }
        if terminal {
            self.finish_search_scan(true, 0);
            return Vec::new();
        }
        self.request_search_page()
    }

    /// Publishes the scan's final coverage. `stopped_complete` distinguishes a
    /// finished captured prefix from an incomplete run.
    fn finish_search_scan(&mut self, reached_end: bool, failed: usize) {
        let scanned = self.search_scan.as_ref().map_or(0, |scan| scan.scanned);
        let large = self.search_scan.as_ref().map_or(0, |scan| scan.large);
        let stopped = self.search_scan.as_ref().is_some_and(|scan| scan.stop);
        self.search_scan = None;
        let Some(panel) = self.search_panel_mut() else {
            return;
        };
        panel.coverage.scanned_items = scanned;
        panel.coverage.large_items = large;
        panel.coverage.failed_items = panel.coverage.failed_items.saturating_add(failed);
        panel.coverage.stopped = stopped;
        panel.coverage.complete = reached_end
            && !stopped
            && large == 0
            && panel.coverage.failed_items == 0
            && !panel.coverage.truncated;
        panel.status = if stopped {
            SearchStatus::Stopped
        } else {
            SearchStatus::Ready
        };
    }

    fn install_search_matches(&mut self, matches: &[SearchMatch]) {
        let Some(panel) = self.search_panel_mut() else {
            return;
        };
        for target in matches {
            if !panel.has_match_room() {
                panel.coverage.truncated = true;
                break;
            }
            panel.matches.push(target.clone());
        }
        if !panel.matches.is_empty() && panel.mode == SearchPanelMode::Input {
            panel.mode = SearchPanelMode::Results;
        }
    }

    /// Reads the exact item window a jump needs. The pin is captured before
    /// the read, so a first probe establishes it and a second request reads
    /// the target range (spec §6.3 step 5).
    pub(super) fn request_history_window_at(
        &mut self,
        session_id: &SessionId,
        index: usize,
    ) -> Vec<AppCommand> {
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.read_page.is_some() || self.history_decode_pending(session_id) {
            // The running chain will call back through
            // `on_search_history_progress`; the pending jump is kept.
            return Vec::new();
        }
        if view.transcript.window.large_item(index).is_some() {
            self.notice(
                NoticeLevel::Info,
                "that match is inside a large item; export it explicitly to read it",
            );
            self.pending_search_jump = None;
            return Vec::new();
        }
        let attempts = self.search_jump_attempts.saturating_add(1);
        self.search_jump_attempts = attempts;
        if attempts > 4 {
            self.pending_search_jump = None;
            self.search_jump_attempts = 0;
            self.notice(
                NoticeLevel::Warning,
                "the jump target could not be loaded; use /export to read it explicitly",
            );
            return Vec::new();
        }
        let pin = view.transcript.window.pin().cloned();
        let probe = pin.is_none();
        let cursor = if probe {
            crate::protocol::ReadCursor::start()
        } else {
            crate::protocol::ReadCursor {
                item: index,
                offset: 0,
            }
        };
        let gap_revision = view.gap_revision;
        let read = ReadRequest {
            cursor,
            pin,
            window_start: index,
            replacement: false,
            reconcile: false,
            probe,
            gap_revision,
        };
        self.request_read(session_id, read).into_iter().collect()
    }
}

impl SearchScan {
    pub(super) fn has_pending_decode(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| !page.pending_encoded.is_empty())
    }
}
