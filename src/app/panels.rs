//! Tool detail navigation and queries use the App's existing RPC/query owners.
use super::queries::{QueryAdmission, QueryKey, QueryScope};
use super::*;
use crate::protocol::{
    ToolDataStreamWire as Stream, ToolExecutionWire, ToolInvocationWire, ToolProcessWire,
};
use crate::state::panels::{
    Focus, MainView, ToolDetailState, ToolLayoutIdentity, ToolLayoutRequest, ToolTextLayout,
};
use crate::state::tool::ToolFacts;

impl App {
    pub(crate) fn has_text_selection(&self) -> bool {
        self.selection.is_some() || self.editor_selection.is_some()
    }
    pub(super) fn copy_tool_detail(&mut self) -> Vec<AppCommand> {
        let Some(detail) = self.tool_detail() else {
            return Vec::new();
        };
        // Copy the same immutable snapshot the user can see. A newer stream
        // revision waiting on the worker must not starve copy during output.
        let width = self.tool_body_area().width.saturating_sub(1).max(1);
        let Some(layout) = detail
            .layout
            .as_ref()
            .filter(|layout| layout.identity.width == width)
        else {
            self.notice(NoticeLevel::Info, "工具输出仍在布局，请稍后复制");
            return Vec::new();
        };
        let text = layout.text.to_string();
        let partial = detail.stream().gap
            || detail.stream().truncated
            || !detail.stream().eof
            || layout.identity.revision != detail.stream().revision;
        if partial {
            self.notice(NoticeLevel::Warning, "仅复制当前已保留窗口，输出可能不完整");
        }
        vec![self.capture_copy(text)]
    }
    pub(super) fn handle_tool_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
    ) -> Option<Vec<AppCommand>> {
        use crossterm::event::{MouseButton, MouseEventKind as Kind};
        self.tool_detail()?;
        if !matches!(self.dock, Dock::Composer) {
            return None;
        }
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        let body = crate::ui::tool_detail::body_area(screen.transcript);
        let detail = self.tool_detail().unwrap();
        let dragging = detail.scrollbar_grab.is_some();
        if dragging
            || (mouse.column == body.right().saturating_sub(1)
                && body.contains((mouse.column, mouse.row).into()))
        {
            let total = detail.layout.as_ref().map_or(0, |layout| layout.rows.len());
            if let Some(geometry) =
                crate::ui::scrollbar::geometry(body, total, detail.offset(body.height as usize))
            {
                match mouse.kind {
                    Kind::Down(MouseButton::Left) | Kind::Drag(MouseButton::Left) => {
                        let grab = detail.scrollbar_grab.unwrap_or_else(|| {
                            (mouse.row as usize)
                                .saturating_sub(geometry.thumb_top)
                                .min(geometry.thumb_height.saturating_sub(1))
                        });
                        let offset =
                            crate::ui::scrollbar::scroll_top_at(geometry, mouse.row as usize, grab);
                        if let MainView::ToolDetail(detail) = &mut self.main_view {
                            detail.scrollbar_grab = Some(grab);
                            detail.scroll[detail.tab.index()] = offset;
                            detail.follow[detail.tab.index()] = false;
                        }
                        return Some(Vec::new());
                    }
                    Kind::Up(_) => {
                        if let MainView::ToolDetail(detail) = &mut self.main_view {
                            detail.scrollbar_grab = None;
                        }
                        return Some(Vec::new());
                    }
                    _ => {}
                }
            }
        }
        if !screen.transcript.contains((mouse.column, mouse.row).into()) {
            if matches!(mouse.kind, Kind::Down(MouseButton::Left)) {
                self.focus = Focus::Editor;
            }
            if matches!(mouse.kind, Kind::ScrollUp | Kind::ScrollDown) {
                return Some(Vec::new());
            }
            return None;
        }
        self.focus = Focus::Main;
        match mouse.kind {
            Kind::ScrollUp => self.scroll_tool(-3, false),
            Kind::ScrollDown => self.scroll_tool(3, false),
            Kind::Down(MouseButton::Left) if mouse.row == screen.transcript.y => {
                let (copy, refresh) = crate::ui::tool_detail::action_areas(screen.transcript);
                if copy.contains((mouse.column, mouse.row).into()) {
                    return Some(self.copy_tool_detail());
                }
                if refresh.contains((mouse.column, mouse.row).into()) {
                    return Some(self.refresh_tool_detail());
                }
                if mouse.column < screen.transcript.x + 10 {
                    self.close_tool_detail();
                }
            }
            Kind::Down(MouseButton::Left)
                if mouse.row == self.tool_body_area().y.saturating_sub(2) =>
            {
                let tabs = self.tool_tabs();
                let selected = self.tool_detail().unwrap().tab;
                let mut column = screen.transcript.x + 1;
                for (index, tab) in tabs.iter().enumerate() {
                    let width = crate::markdown::column_width(tab.label()) as u16
                        + if *tab == selected { 4 } else { 2 };
                    if (column..column + width).contains(&mouse.column) {
                        let current = tabs.iter().position(|tab| *tab == selected).unwrap_or(0);
                        return Some(self.detail_tab(index as i32 - current as i32));
                    }
                    column += width;
                }
            }
            _ => {}
        }
        Some(Vec::new())
    }
    pub fn tool_detail(&self) -> Option<&ToolDetailState> {
        match &self.main_view {
            MainView::ToolDetail(detail) => Some(detail),
            _ => None,
        }
    }
    pub fn focused_region(&self) -> Focus {
        match &self.dock {
            Dock::Composer => self.focus,
            Dock::Search(_) => Focus::Search,
            Dock::Workspace(browser)
                if browser.kind == crate::state::workspace::BrowserKind::Grep =>
            {
                Focus::Search
            }
            Dock::SessionSelector(state) if !matches!(state.mode, SessionPanelMode::Browse) => {
                Focus::Confirmation
            }
            Dock::Export(form) if form.overwrite => Focus::Confirmation,
            _ => Focus::Dock,
        }
    }
    pub fn open_tool_detail(&mut self, key: ToolKey) -> Vec<AppCommand> {
        if self.sessions.active.as_ref() != Some(&key.session_id) {
            return Vec::new();
        }
        let Some(view) = self.sessions.known.get(&key.session_id) else {
            return Vec::new();
        };
        let epoch = view.session_epoch;
        self.close_main_detail();
        self.capture_scroll_anchor();
        let saved_scroll = self.active_view().map(|view| view.scroll.clone());
        self.tool_generation = self.tool_generation.wrapping_add(1);
        self.main_view = MainView::ToolDetail(Box::new(ToolDetailState::new(
            key,
            epoch,
            self.tool_generation,
            self.instant_now(),
        )));
        self.focus = Focus::Main;
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            detail.conversation_scroll = saved_scroll;
        }
        self.slash_completion = None;
        self.poll_tool_detail()
    }
    pub(super) fn tool_command(&mut self, key: ToolKey) -> Vec<AppCommand> {
        if self.sessions.active.as_ref() != Some(&key.session_id) {
            self.notice(
                NoticeLevel::Warning,
                "/tool requires the active session's complete ToolRef",
            );
            return Vec::new();
        }
        self.open_tool_detail(key)
    }
    pub(super) fn close_tool_detail(&mut self) {
        if self.tool_detail().is_some() {
            self.close_main_detail();
        }
    }
    /// Exhaustive finite routing: adding a new main view must add its close
    /// ownership here rather than silently dropping it through an `if let`.
    pub(super) fn close_main_detail(&mut self) {
        self.focus = Focus::Editor;
        let (session, epoch, scroll, scope) = match std::mem::take(&mut self.main_view) {
            MainView::Conversation => return,
            MainView::ToolDetail(detail) => (
                detail.key.session_id.clone(),
                detail.epoch,
                detail.conversation_scroll,
                QueryScope::Tool(detail.key),
            ),
            MainView::FilePreview(file) => (
                file.session.clone(),
                file.epoch,
                file.conversation_scroll,
                QueryScope::Workspace {
                    session_id: file.session,
                    file: true,
                },
            ),
        };
        self.queries.invalidate_scope(&scope);
        if let Some(view) = self
            .sessions
            .known
            .get_mut(&session)
            .filter(|view| view.session_epoch == epoch)
        {
            if let Some(scroll) = scroll {
                view.scroll = scroll;
            }
        }
    }
    pub(super) fn detail_escape(&mut self) -> Vec<AppCommand> {
        if self.selection.is_some() || self.editor_selection.is_some() {
            self.clear_selection();
        } else {
            self.return_main_detail();
        }
        Vec::new()
    }
    pub(super) fn detail_tab(&mut self, step: i32) -> Vec<AppCommand> {
        if self.tool_detail().is_none() {
            return Vec::new();
        }
        let tabs = self.tool_tabs();
        let now = self.instant_now();
        self.tool_generation = self.tool_generation.wrapping_add(1);
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            let at = tabs.iter().position(|tab| *tab == detail.tab).unwrap_or(0);
            detail.tab = tabs[(at as i32 + step).rem_euclid(tabs.len() as i32) as usize];
            detail.generation = self.tool_generation;
            detail.layout = None;
            detail.layout_pending = None;
            detail.due = Some(now);
            detail.error = None;
        }
        self.poll_tool_detail()
    }
    pub fn tool_facts(&self) -> Option<&ToolFacts> {
        let detail = self.tool_detail()?;
        self.sessions
            .known
            .get(&detail.key.session_id)?
            .tool_presentations
            .get(&detail.key)
            .map(Arc::as_ref)
    }
    pub fn tool_tabs(&self) -> Vec<Stream> {
        let Some(facts) = self.tool_facts() else {
            return vec![Stream::Output];
        };
        let mut tabs = Vec::new();
        if facts.command.is_some() {
            tabs.extend([Stream::Stdout, Stream::Stderr]);
        }
        if facts.execution.as_ref().is_some_and(|execution| {
            execution.output_availability != crate::protocol::ToolDataAvailabilityWire::Unavailable
        }) {
            tabs.push(Stream::Output);
        }
        if facts.invocation.is_some()
            || facts.execution.as_ref().is_some_and(|execution| {
                matches!(
                    execution.input_availability,
                    crate::protocol::ToolDataAvailabilityWire::Available
                        | crate::protocol::ToolDataAvailabilityWire::Partial
                        | crate::protocol::ToolDataAvailabilityWire::Expired
                )
            })
        {
            tabs.push(Stream::Input);
        }
        if tabs.is_empty() {
            tabs.push(Stream::Output);
        }
        tabs
    }
    pub(super) fn refresh_tool_detail(&mut self) -> Vec<AppCommand> {
        let now = self.instant_now();
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            detail.read = false;
            detail.error = None;
            detail.due = Some(now);
            // Explicit reread can recover an evicted prefix without silently
            // pretending the UI's retained tail was the complete output.
            detail.streams[detail.tab.index()] = crate::state::tool::StreamView::new(detail.tab);
            detail.layout = None;
            detail.layout_pending = None;
            self.tool_generation = self.tool_generation.wrapping_add(1);
            detail.generation = self.tool_generation;
        }
        self.poll_tool_detail()
    }
    pub(super) fn poll_tool_detail(&mut self) -> Vec<AppCommand> {
        if self.tool_facts().is_some_and(|facts| facts.needs_read) {
            let now = self.instant_now();
            if let MainView::ToolDetail(detail) = &mut self.main_view {
                if detail.error.is_none() {
                    detail.read = false;
                    detail.due = Some(now);
                }
            }
        }
        let Some(detail) = self.tool_detail() else {
            return Vec::new();
        };
        if self.sessions.active.as_ref() != Some(&detail.key.session_id)
            || self
                .sessions
                .known
                .get(&detail.key.session_id)
                .is_none_or(|view| view.session_epoch != detail.epoch)
        {
            self.close_tool_detail();
            return Vec::new();
        }
        if !self.can_send_requests()
            || detail.error.is_some()
            || detail.due.is_none_or(|due| due > self.instant_now())
        {
            return Vec::new();
        }
        let key = detail.key.clone();
        let query_key = QueryKey::Tool { key: key.clone() };
        if self.queries.contains(&query_key) {
            return Vec::new();
        }
        if self.deferred_pending() + self.queries.in_flight_len() >= MAX_DEFERRED_REQUESTS {
            return Vec::new();
        }
        let epoch = detail.epoch;
        let generation = detail.generation;
        let stream = detail.read.then_some(detail.tab);
        let offset = detail.stream().next_offset;
        let id = self.next_request_id();
        if self.queries.request_query(query_key, id) != QueryAdmission::Admitted {
            return Vec::new();
        }
        let tool_ref = (&key).into();
        let request = match stream {
            None => OutgoingRequest::tool_read(id, &tool_ref),
            Some(stream) => OutgoingRequest::tool_output(id, &tool_ref, stream, offset),
        };
        self.pending_requests.insert(
            id,
            RequestKind::ToolDetail {
                key,
                epoch,
                generation,
                stream,
            },
        );
        let now = self.instant_now();
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            detail.last_query = Some(now);
            detail.due = None;
        }
        vec![AppCommand::Rpc(request)]
    }
    pub(super) fn on_tool_detail_response(
        &mut self,
        key: ToolKey,
        epoch: u64,
        generation: u64,
        stream: Option<Stream>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if !self.tool_detail().is_some_and(|detail| {
            detail.key == key && detail.epoch == epoch && detail.generation == generation
        }) || self
            .sessions
            .known
            .get(&key.session_id)
            .is_none_or(|view| view.session_epoch != epoch)
        {
            return Vec::new();
        }
        let now = self.instant_now();
        let result: Result<(), String> = if let Some(stream) = stream {
            response
                .parse_tool_output()
                .map_err(|error| error.to_string())
                .and_then(|page| {
                    if ToolKey::from(&page.tool_ref) != key || page.stream != stream {
                        return Err("tool output identity mismatch".into());
                    }
                    let terminal = self.tool_facts().is_some_and(ToolFacts::is_terminal);
                    let shared_result = self.tool_facts().and_then(|facts| facts.result.clone());
                    if let MainView::ToolDetail(detail) = &mut self.main_view {
                        let old_offset = detail.streams[stream.index()].next_offset;
                        detail.streams[stream.index()]
                            .accept_page_with_result(&page, shared_result.as_ref())
                            .map_err(str::to_owned)?;
                        if detail.stream().eof && (terminal || matches!(stream, Stream::Input)) {
                            detail.due = None;
                        } else if !page.eof && page.next_offset > old_offset {
                            detail.due = Some(now);
                        } else {
                            detail.read = false;
                            detail.due = Some(now + Duration::from_millis(500));
                        }
                    }
                    Ok(())
                })
        } else {
            response
                .parse_tool_read()
                .map_err(|error| error.to_string())
                .and_then(|read| {
                    if ToolKey::from(&read.execution.tool_ref) != key
                        || read
                            .invocation
                            .as_ref()
                            .is_some_and(|inv| ToolKey::from(&inv.tool_ref) != key)
                    {
                        return Err("tool read identity mismatch".into());
                    }
                    let first = self.tool_detail().is_some_and(|detail| !detail.initialized);
                    let command = read.execution.command.is_some();
                    if let Some(invocation) = read.invocation {
                        self.accept_tool_invocation(invocation);
                    }
                    self.accept_tool_execution(read.execution, true);
                    if self.tool_facts().is_some_and(|facts| facts.needs_read) {
                        return Err(
                            "tool facts remain conflicting; explicit retry required".to_owned()
                        );
                    }
                    if let MainView::ToolDetail(detail) = &mut self.main_view {
                        detail.read = true;
                        detail.initialized = true;
                        detail.due = Some(now);
                        if first && command {
                            detail.tab = Stream::Stdout;
                        }
                    }
                    Ok(())
                })
        };
        if let Err(error) = result {
            if let MainView::ToolDetail(detail) = &mut self.main_view {
                detail.error = Some(format!(
                    "{} — F5 / /refresh 重试",
                    crate::safe_text::safe_display(&error)
                ));
                detail.due = None;
            }
        }
        // Subsequent pages rejoin the common FIFO after this response releases
        // its slot. Never recursively retry a failed/resource-exhausted read.
        Vec::new()
    }
    fn tool_facts_mut(&mut self, key: &ToolKey, name: &str) -> Option<&mut ToolFacts> {
        let view = self.sessions.known.get_mut(&key.session_id)?;
        let facts = Arc::make_mut(&mut view.tool_presentations)
            .entry(key.clone())
            .or_insert_with(|| Arc::new(ToolFacts::new(name)));
        Some(Arc::make_mut(facts))
    }
    pub(super) fn accept_tool_invocation(&mut self, invocation: ToolInvocationWire) {
        let key = ToolKey::from(&invocation.tool_ref);
        let name = invocation.name.clone();
        if let Some(facts) = self.tool_facts_mut(&key, &name) {
            facts.invocation = Some(Arc::new(invocation));
        }
    }
    pub(super) fn accept_tool_execution(
        &mut self,
        execution: ToolExecutionWire,
        authoritative: bool,
    ) {
        let key = ToolKey::from(&execution.tool_ref);
        let name = execution.name.clone();
        if let Some(facts) = self.tool_facts_mut(&key, &name) {
            facts.accept_execution(execution, authoritative);
        }
        let needs_read = self
            .sessions
            .known
            .get(&key.session_id)
            .and_then(|view| view.tool_presentations.get(&key))
            .is_some_and(|facts| facts.needs_read);
        let now = self.instant_now();
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            if detail.key == key && detail.error.is_none() {
                if needs_read {
                    detail.read = false;
                }
                detail.due = Some(now);
            }
        }
    }
    pub(super) fn accept_tool_process(&mut self, process: ToolProcessWire) {
        let key = ToolKey::from(&process.tool_ref);
        if let Some(command) = process.command {
            if let Some(facts) = self.tool_facts_mut(&key, "tool") {
                facts.accept_command(command);
            }
        }
        let now = self.instant_now();
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            if detail.key != key || detail.error.is_some() {
                return;
            }
            if let Some(chunk) = process.chunk {
                if chunk.stream == detail.tab {
                    if let Err(error) = detail.streams[chunk.stream.index()].accept_event(&chunk) {
                        detail.error = Some(error.to_owned());
                        detail.due = None;
                        return;
                    }
                }
            }
            detail.due = Some(
                detail
                    .last_query
                    .map_or(now, |last| (last + Duration::from_millis(250)).max(now)),
            );
        }
    }
    pub fn tool_layout_request(&self, width: u16) -> Option<ToolLayoutRequest> {
        let detail = self.tool_detail()?;
        // Do not continually cancel a large in-flight layout on every chunk:
        // install that bounded snapshot, then catch up to the latest revision.
        if detail.layout_pending.as_ref().is_some_and(|pending| {
            pending.generation == detail.generation && pending.width == width
        }) {
            return None;
        }
        let identity = ToolLayoutIdentity {
            generation: detail.generation,
            stream: detail.tab,
            revision: detail.stream().revision,
            width,
        };
        if detail.layout_pending.as_ref() == Some(&identity)
            || detail
                .layout
                .as_ref()
                .is_some_and(|layout| layout.identity == identity)
        {
            return None;
        }
        Some(ToolLayoutRequest {
            identity,
            stream: detail.stream().clone(),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    pub fn mark_tool_layout_pending(&mut self, identity: ToolLayoutIdentity) {
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            detail.layout_pending = Some(identity);
        }
    }
    pub(super) fn install_tool_layout(&mut self, layout: ToolTextLayout) {
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            if detail.layout_pending.as_ref() == Some(&layout.identity)
                && detail.generation == layout.identity.generation
                && detail.tab == layout.identity.stream
            {
                detail.layout_pending = None;
                detail.layout = Some(layout);
            }
        }
    }
    pub(super) fn scroll_tool(&mut self, delta: i32, end: bool) {
        let height = self.tool_body_area().height as usize;
        if let MainView::ToolDetail(detail) = &mut self.main_view {
            let index = detail.tab.index();
            let offset = detail.offset(height);
            detail.follow[index] = end;
            if !end {
                detail.scroll[index] = offset.saturating_add_signed(delta as isize);
            }
        }
    }
    pub fn tool_body_area(&self) -> ratatui::layout::Rect {
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        crate::ui::tool_detail::body_area(screen.transcript)
    }
}
