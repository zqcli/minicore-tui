//! Concrete Context view over the existing B execution/context owners.
use super::*;
use crate::state::panels::{ContextState, Focus, MainView};
impl App {
    pub fn can_manual_compact(&self) -> bool {
        self.compact_supported
            && self.context_supported
            && self.can_send_requests()
            && self.reload.is_none()
            && self.deferred_pending() < MAX_DEFERRED_REQUESTS
            && self.active_view().is_some_and(|v| {
                v.info.loaded
                    && !v.closing
                    && !v.is_blocked()
                    && !v.is_preparing()
                    && v.live.is_none()
                    && v.unsaved_loop.is_none()
                    && !v.event_gap
                    && v.transcript.complete
                    && v.state
                        .as_ref()
                        .is_some_and(|s| s.status == SessionStatusWire::Idle)
                    && v.manual_compact.as_ref().is_none_or(|m| m.result.is_some())
            })
    }
    pub fn context_panel(&self) -> Option<&ContextState> {
        if let MainView::Context(c) = &self.main_view {
            Some(c)
        } else {
            None
        }
    }
    pub fn open_context(&mut self) -> Vec<AppCommand> {
        let Some(session) = self.sessions.active.clone() else {
            self.notice(NoticeLevel::Info, "先选择 Session");
            return vec![];
        };
        let Some(v) = self.sessions.known.get(&session).filter(|v| v.info.loaded) else {
            self.notice(NoticeLevel::Info, "Context 需要明确 Continue (Ctrl+G)");
            return vec![];
        };
        let epoch = v.session_epoch;
        self.close_main_detail();
        self.capture_scroll_anchor();
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        self.main_view = MainView::Context(Box::new(ContextState {
            session,
            epoch,
            generation: self.workspace_generation,
            conversation_scroll: self.active_view().map(|v| v.scroll.clone()),
            offset: 0,
            action: 0,
            scrollbar_grab: None,
        }));
        self.focus = Focus::Main;
        self.slash_completion = None;
        self.refresh_context_panel()
    }
    pub(super) fn refresh_context_panel(&mut self) -> Vec<AppCommand> {
        if !self.context_supported {
            self.notice(NoticeLevel::Warning, "Agent 不兼容 session.context");
            return Vec::new();
        }
        let Some(c) = self.context_panel() else {
            return vec![];
        };
        let (session, generation) = (c.session.clone(), c.generation);
        self.queue_context_read(&session, ContextQueryOwner::Panel(generation))
            .into_iter()
            .collect()
    }
    pub(super) fn context_action(&mut self) -> Vec<AppCommand> {
        match self.context_panel().map(|c| c.action) {
            Some(0) => self.refresh_context_panel(),
            Some(1) => self.start_manual_compact(),
            Some(2) => {
                let Some((session, operation)) = self.context_cancel_target() else {
                    return vec![];
                };
                if let Some(m) = self
                    .sessions
                    .known
                    .get_mut(&session)
                    .and_then(|v| v.manual_compact.as_mut())
                    .filter(|m| m.operation_id == operation)
                {
                    m.cancel_requested = true;
                }
                self.request_compact_cancel(&session, &operation)
                    .into_iter()
                    .collect()
            }
            _ => vec![],
        }
    }
    pub fn context_cancel_target(&self) -> Option<(String, String)> {
        if !self.compact_cancel_supported {
            return None;
        }
        let c = self.context_panel()?;
        let v = self.sessions.known.get(&c.session)?;
        if !v.info.loaded || v.session_epoch != c.epoch {
            return None;
        }
        let id = v
            .manual_compact
            .as_ref()
            .filter(|m| m.result.is_none())
            .map(|m| m.operation_id.clone())
            .or_else(|| {
                v.context
                    .as_ref()?
                    .current_operation
                    .as_ref()
                    .map(|o| o.operation_id.clone())
            })?;
        Some((c.session.clone(), id))
    }
    pub(super) fn context_tab(&mut self, step: i32) {
        if let MainView::Context(c) = &mut self.main_view {
            c.action = (c.action as i32 + step).rem_euclid(3) as usize;
        }
    }
    pub(super) fn scroll_context(&mut self, delta: i32, end: bool) {
        let max = crate::ui::context::rows(self)
            .len()
            .saturating_sub(self.main_body_area().height as usize);
        if let MainView::Context(c) = &mut self.main_view {
            c.offset = if end {
                max
            } else {
                c.offset.saturating_add_signed(delta as isize).min(max)
            };
        }
    }
    pub(super) fn context_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
    ) -> Option<Vec<AppCommand>> {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        self.context_panel()?;
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        let body = crate::ui::workspace::file_body(screen.transcript);
        let scrollbar = screen.scrollbar_for(body);
        let c = self.context_panel().unwrap();
        if matches!(self.dock, Dock::Composer)
            && (c.scrollbar_grab.is_some() || scrollbar.contains((mouse.column, mouse.row).into()))
        {
            if let Some(g) = crate::ui::scrollbar::geometry(
                scrollbar,
                crate::ui::context::rows(self).len(),
                c.offset,
            ) {
                match mouse.kind {
                    K::Down(MouseButton::Left) | K::Drag(MouseButton::Left) => {
                        let grab = c.scrollbar_grab.unwrap_or_else(|| {
                            (mouse.row as usize)
                                .saturating_sub(g.thumb_top)
                                .min(g.thumb_height.saturating_sub(1))
                        });
                        let offset =
                            crate::ui::scrollbar::scroll_top_at(g, mouse.row as usize, grab);
                        if let MainView::Context(c) = &mut self.main_view {
                            c.scrollbar_grab = Some(grab);
                            c.offset = offset;
                        }
                        self.focus = Focus::Main;
                        return Some(vec![]);
                    }
                    K::Up(_) => {
                        if let MainView::Context(c) = &mut self.main_view {
                            c.scrollbar_grab = None;
                        }
                        return Some(vec![]);
                    }
                    _ => {}
                }
            }
        }
        if !screen.transcript.contains((mouse.column, mouse.row).into())
            && !scrollbar.contains((mouse.column, mouse.row).into())
        {
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
            K::ScrollUp => self.scroll_context(-3, false),
            K::ScrollDown => self.scroll_context(3, false),
            K::Down(MouseButton::Left) if mouse.row == screen.transcript.y => {
                self.close_main_detail()
            }
            _ => {}
        }
        Some(vec![])
    }
}
