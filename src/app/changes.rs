//! Read-only changes/status consumers of the existing query and layout owners.
use super::queries::{QueryAdmission, QueryKey, QueryScope};
use super::*;
use crate::protocol::changes::*;
use crate::state::{
    changes::*,
    panels::{Focus, MainView},
    workspace::FileLayoutIdentity,
};
impl App {
    pub(super) fn status_send_failed(&mut self, session: &str, epoch: u64, generation: u64) {
        if let Some(v) = self
            .sessions
            .known
            .get_mut(session)
            .filter(|v| v.session_epoch == epoch && v.workspace_status.generation == generation)
        {
            v.workspace_status.error = true;
            v.workspace_status.stale = true;
            v.workspace_status.wanted = false;
        }
    }
    pub fn changes(&self) -> Option<&ChangesState> {
        if let MainView::Changes(s) = &self.main_view {
            Some(s)
        } else {
            None
        }
    }
    pub(super) fn arm_workspace_status(&mut self, session: &str, on_open: bool) {
        let Some(v) = self
            .sessions
            .known
            .get_mut(session)
            .filter(|v| v.info.loaded)
        else {
            return;
        };
        if on_open && v.workspace_status.opened_epoch == Some(v.session_epoch) {
            return;
        }
        v.workspace_status.opened_epoch = Some(v.session_epoch);
        v.workspace_status.generation = v.workspace_status.generation.wrapping_add(1);
        v.workspace_status.wanted = true;
        v.workspace_status.stale = v.workspace_status.value.is_some();
    }
    pub(super) fn poll_workspace_status(&mut self) -> Vec<AppCommand> {
        if !self.can_send_requests()
            || self.deferred_pending() + self.queries.in_flight_len() >= MAX_DEFERRED_REQUESTS
        {
            return vec![];
        }
        let targets: Vec<_> = self
            .sessions
            .known
            .iter()
            .filter(|(_, v)| v.info.loaded && v.workspace_status.wanted)
            .map(|(id, v)| (id.clone(), v.session_epoch, v.workspace_status.generation))
            .collect();
        let mut commands = vec![];
        for (session, epoch, generation) in targets {
            let key = QueryKey::WorkspaceStatus {
                session_id: session.clone(),
            };
            if self.queries.contains(&key) {
                continue;
            }
            let id = self.next_request_id();
            if self.queries.request_query(key, id) != QueryAdmission::Admitted {
                continue;
            }
            self.sessions
                .known
                .get_mut(&session)
                .unwrap()
                .workspace_status
                .wanted = false;
            self.pending_requests.insert(
                id,
                RequestKind::WorkspaceStatus {
                    session_id: session.clone(),
                    epoch,
                    generation,
                },
            );
            commands.push(AppCommand::Rpc(OutgoingRequest::workspace_status(
                id, &session,
            )));
        }
        commands
    }
    pub(super) fn on_workspace_status(
        &mut self,
        session: &str,
        epoch: u64,
        generation: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let Some(v) = self.sessions.known.get_mut(session).filter(|v| {
            v.info.loaded && v.session_epoch == epoch && v.workspace_status.generation == generation
        }) else {
            return vec![];
        };
        match response.result_as::<WorkspaceStatus>() {
            Ok(mut status) => {
                // Footer keeps metadata, not a second list of changed paths.
                status.entries = Vec::new();
                if status.complete || v.workspace_status.value.is_none() {
                    v.workspace_status.value = Some(status);
                    v.workspace_status.error = false;
                } else {
                    v.workspace_status.error = true;
                }
                v.workspace_status.stale = v.workspace_status.error
                    || v.workspace_status
                        .value
                        .as_ref()
                        .is_some_and(|s| !s.complete);
            }
            Err(_) => {
                v.workspace_status.error = true;
                v.workspace_status.stale = true;
            }
        }
        vec![]
    }
    pub fn open_changes(&mut self, scope: ChangeScope) -> Vec<AppCommand> {
        let Some(session) = self.sessions.active.clone() else {
            self.notice(NoticeLevel::Info, "先选择 Session");
            return vec![];
        };
        let Some(v) = self.sessions.known.get(&session) else {
            return vec![];
        };
        if scope == ChangeScope::Workspace && !v.info.loaded {
            self.notice(
                NoticeLevel::Info,
                "workspace Changes 需要明确 Continue (Ctrl+G)",
            );
            return vec![];
        }
        let epoch = v.session_epoch;
        self.close_main_detail();
        self.capture_scroll_anchor();
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        let scroll = self.active_view().map(|v| v.scroll.clone());
        self.main_view = MainView::Changes(Box::new(ChangesState {
            session: session.clone(),
            epoch,
            generation: self.workspace_generation,
            conversation_scroll: scroll,
            scope,
            records: vec![],
            list_page: None,
            cursor: None,
            wanted: true,
            selected: 0,
            offset: 0,
            error: None,
            limited: false,
            detail: None,
            in_diff: false,
        }));
        self.focus = Focus::Main;
        self.slash_completion = None;
        self.arm_workspace_status(&session, false);
        let mut commands = self.poll_changes();
        commands.extend(self.poll_workspace_status());
        commands
    }
    pub(super) fn poll_changes(&mut self) -> Vec<AppCommand> {
        let Some(s) = self.changes() else {
            return vec![];
        };
        if self.sessions.active.as_ref() != Some(&s.session)
            || self.sessions.known.get(&s.session).is_none_or(|v| {
                v.session_epoch != s.epoch || s.scope == ChangeScope::Workspace && !v.info.loaded
            })
        {
            self.close_main_detail();
            return vec![];
        }
        let wanted = if s.in_diff {
            s.detail
                .as_ref()
                .is_some_and(|d| d.wanted && d.error.is_none() && !d.stale)
        } else {
            s.wanted && s.error.is_none()
        };
        if !wanted
            || !self.can_send_requests()
            || self.deferred_pending() + self.queries.in_flight_len() >= MAX_DEFERRED_REQUESTS
            || self
                .pending_requests
                .values()
                .any(|r| matches!(r, RequestKind::Changes { .. }))
        {
            return vec![];
        }
        let (session, epoch, generation, diff) =
            (s.session.clone(), s.epoch, s.generation, s.in_diff);
        let id = self.next_request_id();
        if self.queries.request_query(
            QueryKey::Changes {
                session_id: session.clone(),
            },
            id,
        ) != QueryAdmission::Admitted
        {
            return vec![];
        }
        let MainView::Changes(s) = &mut self.main_view else {
            unreachable!()
        };
        let req = if diff {
            let d = s.detail.as_mut().unwrap();
            d.wanted = false;
            OutgoingRequest::changes_diff(
                id,
                &session,
                &d.record.change_ref,
                d.comparison,
                d.cursor.as_ref(),
            )
        } else {
            s.wanted = false;
            OutgoingRequest::changes_list(id, &session, &s.scope, s.cursor.as_ref())
        };
        self.pending_requests.insert(
            id,
            RequestKind::Changes {
                session_id: session,
                epoch,
                generation,
                diff,
            },
        );
        vec![AppCommand::Rpc(req)]
    }
    pub(super) fn on_changes_response(
        &mut self,
        session: &str,
        epoch: u64,
        generation: u64,
        diff: bool,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let MainView::Changes(s) = &mut self.main_view else {
            return vec![];
        };
        if s.session != session
            || s.epoch != epoch
            || s.generation != generation
            || s.in_diff != diff
            || self
                .sessions
                .known
                .get(session)
                .is_none_or(|v| v.session_epoch != epoch)
        {
            return vec![];
        }
        if diff {
            let d = s.detail.as_mut().unwrap();
            let result = response
                .result_as::<DiffPage>()
                .map_err(|_| "diff unavailable/malformed; F5 to refresh")
                .and_then(|page| {
                    if !page.valid_cursor(session) {
                        return Err("invalid diff cursor");
                    }
                    d.accept(page)
                });
            if let Err(e) = result {
                d.error = Some(e.into());
                d.wanted = false;
                d.cursor = None;
            }
        } else {
            let result = response
                .result_as::<ChangesList>()
                .map_err(|_| "changes list unavailable/malformed; F5 to refresh")
                .and_then(|mut p| {
                    if p.session_id != session || p.scope != s.scope || !p.valid_cursor() {
                        return Err("changes list identity/cursor mismatch");
                    }
                    if p.stale {
                        s.error = Some("stale: 保留旧列表，F5 显式刷新".into());
                        s.cursor = None;
                        return Ok(());
                    }
                    for record in std::mem::take(&mut p.records) {
                        let bytes = record_bytes(&record);
                        if s.records.len() >= crate::limits::CHANGE_RECORDS
                            || s.records.iter().map(record_bytes).sum::<usize>() + bytes
                                > crate::limits::CHANGE_RECORD_BYTES
                        {
                            s.limited = true;
                            break;
                        }
                        if !s.records.iter().any(|r| r.change_ref == record.change_ref) {
                            s.records.push(record);
                        }
                    }
                    s.cursor = p.next_cursor.clone();
                    s.list_page = Some(p);
                    if s.records.len() >= crate::limits::CHANGE_RECORDS && s.cursor.is_some() {
                        s.limited = true;
                    }
                    Ok(())
                });
            if let Err(e) = result {
                s.error = Some(e.into());
                s.cursor = None;
            }
        }
        vec![]
    }
    pub(super) fn changes_select(&mut self) -> Vec<AppCommand> {
        let MainView::Changes(s) = &mut self.main_view else {
            return vec![];
        };
        if s.in_diff {
            return vec![];
        }
        let Some(record) = s.records.get(s.selected).cloned() else {
            return vec![];
        };
        if s.detail
            .as_ref()
            .is_none_or(|d| d.record.change_ref != record.change_ref)
        {
            s.detail = Some(DiffState::new(record));
        }
        s.in_diff = true;
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        s.generation = self.workspace_generation;
        if let Some(d) = &mut s.detail {
            d.pending = None;
        }
        self.poll_changes()
    }
    pub(super) fn changes_back(&mut self) -> bool {
        let MainView::Changes(s) = &mut self.main_view else {
            return false;
        };
        if !s.in_diff {
            return false;
        }
        s.in_diff = false;
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        s.generation = self.workspace_generation;
        if let Some(d) = &mut s.detail {
            d.wanted = false;
            d.pending = None;
        }
        self.queries
            .invalidate_scope(&QueryScope::Changes(s.session.clone()));
        true
    }
    pub(super) fn changes_tab(&mut self, step: i32) -> Vec<AppCommand> {
        let Some(s) = self.changes() else {
            return vec![];
        };
        if !s.in_diff {
            let scope = if s.scope == ChangeScope::Workspace {
                ChangeScope::Session
            } else {
                ChangeScope::Workspace
            };
            return self.open_changes(scope);
        }
        let MainView::Changes(s) = &mut self.main_view else {
            unreachable!()
        };
        let d = s.detail.as_mut().unwrap();
        if d.record.origin == ChangeOrigin::Tool {
            return vec![];
        }
        let comparisons = [
            Comparison::HeadToIndex,
            Comparison::IndexToWorktree,
            Comparison::HeadToWorktree,
        ];
        let at = comparisons
            .iter()
            .position(|c| *c == d.comparison)
            .unwrap_or(0);
        let comparison = comparisons[(at as i32 + step).rem_euclid(3) as usize];
        let record = d.record.clone();
        *d = DiffState::new(record);
        d.comparison = comparison;
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        s.generation = self.workspace_generation;
        self.poll_changes()
    }
    pub(super) fn changes_more(&mut self, refresh: bool) -> Vec<AppCommand> {
        let Some(s) = self.changes() else {
            return vec![];
        };
        let session = s.session.clone();
        if refresh && !s.in_diff {
            let scope = s.scope.clone();
            return self.open_changes(scope);
        }
        let MainView::Changes(s) = &mut self.main_view else {
            unreachable!()
        };
        if s.in_diff {
            let d = s.detail.as_mut().unwrap();
            if refresh {
                let comparison = d.comparison;
                let record = d.record.clone();
                *d = DiffState::new(record);
                d.comparison = comparison;
                self.workspace_generation = self.workspace_generation.wrapping_add(1);
                s.generation = self.workspace_generation;
            } else if d.cursor.is_some() && !d.stale && d.error.is_none() {
                d.wanted = true;
            }
        } else if s.cursor.is_some() && !s.limited && s.error.is_none() {
            s.wanted = true;
        }
        if refresh {
            self.arm_workspace_status(&session, false);
        }
        self.poll_changes()
    }
    pub(super) fn scroll_changes(&mut self, delta: i32, end: bool) {
        let height = self.main_body_area().height as usize;
        if let MainView::Changes(s) = &mut self.main_view {
            if s.in_diff {
                let d = s.detail.as_mut().unwrap();
                let max = d
                    .layout
                    .as_ref()
                    .map_or(0, |l| l.rows.len())
                    .saturating_sub(height);
                let at = if d.follow { max } else { d.offset.min(max) };
                d.follow = end;
                if !end {
                    d.offset = at.saturating_add_signed(delta as isize).min(max);
                }
            } else {
                s.selected = if end {
                    s.records.len().saturating_sub(1)
                } else {
                    s.selected
                        .saturating_add_signed(delta as isize)
                        .min(s.records.len().saturating_sub(1))
                };
                s.offset = s.selected.saturating_sub(height.saturating_sub(1));
            }
        }
    }
    pub(super) fn copy_diff(&mut self) -> Vec<AppCommand> {
        let Some(d) = self
            .changes()
            .filter(|s| s.in_diff)
            .and_then(|s| s.detail.as_ref())
        else {
            return vec![];
        };
        let Some(layout) = &d.layout else {
            return vec![];
        };
        let text = layout.copy_text.to_string();
        if d.stale
            || d.error.is_some()
            || d.buffer.partial_line()
            || d.meta.as_ref().is_none_or(|p| !p.complete || p.truncated)
            || layout.identity.revision != d.revision
        {
            self.notice(
                NoticeLevel::Warning,
                "仅复制已显示 diff 行源文本；含旧/部分数据或未完整行，不是完整 patch",
            );
        }
        vec![self.capture_copy(text)]
    }
    pub fn diff_layout_request(&self, width: u16) -> Option<DiffLayoutRequest> {
        let s = self.changes().filter(|s| s.in_diff)?;
        let d = s.detail.as_ref()?;
        if d.pending
            .as_ref()
            .is_some_and(|p| p.generation == s.generation && p.width == width)
        {
            return None;
        }
        let identity = FileLayoutIdentity {
            generation: s.generation,
            revision: d.revision,
            width,
        };
        if d.layout.as_ref().is_some_and(|l| l.identity == identity) {
            return None;
        }
        Some(DiffLayoutRequest {
            identity,
            buffer: d.buffer.clone(),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    pub fn mark_diff_layout_pending(&mut self, identity: FileLayoutIdentity) {
        if let MainView::Changes(s) = &mut self.main_view {
            if let Some(d) = &mut s.detail {
                d.pending = Some(identity);
            }
        }
    }
    pub(super) fn install_diff_layout(&mut self, layout: DiffLayout) {
        if let MainView::Changes(s) = &mut self.main_view {
            if let Some(d) = &mut s.detail {
                if s.in_diff
                    && d.pending.as_ref() == Some(&layout.identity)
                    && s.generation == layout.identity.generation
                {
                    d.pending = None;
                    d.layout = Some(layout);
                }
            }
        }
    }
    pub(super) fn changes_send_failed(&mut self, generation: u64) {
        if let MainView::Changes(s) = &mut self.main_view {
            if s.generation == generation {
                if s.in_diff {
                    let d = s.detail.as_mut().unwrap();
                    d.error = Some("query not sent; F5 to retry".into());
                    d.wanted = false;
                } else {
                    s.error = Some("query not sent; F5 to retry".into());
                    s.wanted = false;
                }
            }
        }
    }
    pub(super) fn review_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
    ) -> Option<Vec<AppCommand>> {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        self.changes()?;
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        if !screen.transcript.contains((mouse.column, mouse.row).into()) {
            if matches!(mouse.kind, K::Down(MouseButton::Left)) {
                self.focus = Focus::Editor;
            }
            return None;
        }
        if !matches!(self.dock, Dock::Composer) {
            return Some(vec![]);
        }
        self.focus = Focus::Main;
        match mouse.kind {
            K::ScrollUp => self.scroll_changes(-3, false),
            K::ScrollDown => self.scroll_changes(3, false),
            K::Down(MouseButton::Left) if mouse.row == screen.transcript.y => {
                if !self.changes_back() {
                    self.close_main_detail();
                }
            }
            K::Down(MouseButton::Left)
                if !self.changes().unwrap().in_diff && mouse.row >= self.main_body_area().y =>
            {
                let y = self.main_body_area().y;
                if let MainView::Changes(s) = &mut self.main_view {
                    s.selected = (s.offset + (mouse.row - y) as usize)
                        .min(s.records.len().saturating_sub(1));
                }
                return Some(self.changes_select());
            }
            _ => {}
        }
        Some(vec![])
    }
}
fn record_bytes(r: &ChangeRecord) -> usize {
    let revision = |v: &ChangeRevision| {
        if let ChangeRevision::Content { sha256, .. } = v {
            sha256.capacity()
        } else {
            0
        }
    };
    std::mem::size_of::<ChangeRecord>()
        + r.change_ref.capacity()
        + r.path.capacity()
        + r.original_path.as_ref().map_or(0, String::capacity)
        + revision(&r.before)
        + revision(&r.after)
        + r.tool_ref.as_ref().map_or(0, |t| {
            t.session_id.capacity() + t.loop_id.capacity() + t.tool_call_id.capacity()
        })
}
