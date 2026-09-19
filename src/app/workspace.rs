//! Explicit, loaded-session workspace reads through the existing two query slots.
use super::queries::{QueryAdmission, QueryKey, QueryScope};
use super::*;
use crate::protocol::workspace::*;
use crate::state::panels::{Focus, MainView};
use crate::state::workspace::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceQuery {
    Files,
    Grep,
    File,
}
impl App {
    pub(super) fn workspace_send_failed(&mut self, generation: u64, kind: WorkspaceQuery) {
        if kind == WorkspaceQuery::File {
            if let MainView::FilePreview(file) = &mut self.main_view {
                if file.generation == generation {
                    file.error = Some("workspace.read was not sent; F5 to retry".into());
                    file.wanted = false;
                }
            }
        } else if let Dock::Workspace(browser) = &mut self.dock {
            if browser.generation == generation {
                browser.error = Some("workspace query was not sent; F5 to retry".into());
                browser.due = None;
            }
        }
    }
    pub(super) fn workspace_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
    ) -> Option<Vec<AppCommand>> {
        use crossterm::event::{MouseButton, MouseEventKind as Kind};
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        if let Dock::Workspace(b) = &mut self.dock {
            if let Kind::Down(button) = mouse.kind {
                if screen.panel.contains((mouse.column, mouse.row).into()) {
                    let row = mouse.row.saturating_sub(screen.panel.y);
                    if row == 1 {
                        b.scope_focused = false;
                    } else if row == 2 {
                        b.scope_focused = true;
                    } else if row >= 3 && row < screen.panel.height.saturating_sub(3) {
                        let count = screen.panel.height.saturating_sub(6) as usize;
                        let start = b.selected.saturating_sub(count.saturating_sub(1));
                        b.selected = (start + row as usize - 3).min(b.len().saturating_sub(1));
                        if button == MouseButton::Right {
                            return Some(self.workspace_select(true));
                        }
                    }
                }
            }
            return Some(vec![]);
        }
        let file = self.file_preview()?;
        let body = crate::ui::workspace::file_body(screen.transcript);
        if matches!(self.dock, Dock::Composer)
            && (file.scrollbar_grab.is_some()
                || (mouse.column == body.right().saturating_sub(1)
                    && body.contains((mouse.column, mouse.row).into())))
        {
            let total = file.layout.as_ref().map_or(0, |l| l.rows.len());
            if let Some(geometry) = crate::ui::scrollbar::geometry(
                body,
                total,
                file.scroll_offset(body.height as usize),
            ) {
                match mouse.kind {
                    Kind::Down(MouseButton::Left) | Kind::Drag(MouseButton::Left) => {
                        let grab = file.scrollbar_grab.unwrap_or_else(|| {
                            (mouse.row as usize)
                                .saturating_sub(geometry.thumb_top)
                                .min(geometry.thumb_height.saturating_sub(1))
                        });
                        let offset =
                            crate::ui::scrollbar::scroll_top_at(geometry, mouse.row as usize, grab);
                        if let MainView::FilePreview(file) = &mut self.main_view {
                            file.scrollbar_grab = Some(grab);
                            file.offset = offset;
                            file.follow = false;
                        }
                        self.focus = Focus::Main;
                        return Some(vec![]);
                    }
                    Kind::Up(_) => {
                        if let MainView::FilePreview(file) = &mut self.main_view {
                            file.scrollbar_grab = None;
                        }
                        return Some(vec![]);
                    }
                    _ => {}
                }
            }
        }
        if screen.transcript.contains((mouse.column, mouse.row).into()) {
            if !matches!(self.dock, Dock::Composer) {
                return Some(vec![]);
            }
            self.focus = Focus::Main;
            match mouse.kind {
                Kind::ScrollUp => self.scroll_file(-3, false),
                Kind::ScrollDown => self.scroll_file(3, false),
                Kind::Down(MouseButton::Left) if mouse.row == screen.transcript.y => {
                    let (copy, more, refresh) =
                        crate::ui::workspace::file_actions(screen.transcript);
                    if copy.contains((mouse.column, mouse.row).into()) {
                        return Some(self.copy_file());
                    }
                    if more.contains((mouse.column, mouse.row).into()) {
                        return Some(self.file_more(false));
                    }
                    if refresh.contains((mouse.column, mouse.row).into()) {
                        return Some(self.file_more(true));
                    }
                    if mouse.column < screen.transcript.x + 10 {
                        self.return_main_detail();
                    }
                }
                _ => {}
            }
            Some(vec![])
        } else {
            if matches!(mouse.kind, Kind::Down(MouseButton::Left)) {
                self.focus = Focus::Editor;
            }
            if matches!(mouse.kind, Kind::ScrollUp | Kind::ScrollDown) {
                Some(vec![])
            } else {
                None
            }
        }
    }
    pub fn workspace_browser(&self) -> Option<&WorkspaceBrowser> {
        if let Dock::Workspace(browser) = &self.dock {
            Some(browser)
        } else {
            None
        }
    }
    pub fn file_preview(&self) -> Option<&FilePreviewState> {
        if let MainView::FilePreview(file) = &self.main_view {
            Some(file)
        } else {
            None
        }
    }
    pub fn has_main_detail(&self) -> bool {
        !matches!(self.main_view, MainView::Conversation)
    }
    fn workspace_owner(&mut self) -> Option<(String, u64)> {
        if let Some(view) = self.active_view().filter(|view| view.info.loaded) {
            return Some((view.info.session_id.clone(), view.session_epoch));
        }
        self.notice(
            NoticeLevel::Info,
            "文件功能需要 loaded Session；请明确 Continue (Ctrl+G)，不会自动打开或附加内容",
        );
        None
    }
    pub fn open_workspace_browser(
        &mut self,
        kind: BrowserKind,
        query: String,
        at_sign: bool,
    ) -> Vec<AppCommand> {
        let Some((session, epoch)) = self.workspace_owner() else {
            return vec![];
        };
        self.close_workspace_browser();
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        let mut browser = WorkspaceBrowser::new(
            kind,
            session,
            epoch,
            self.workspace_generation,
            self.instant_now() + Duration::from_millis(150),
        );
        browser.query = query;
        let (line, col) = self.composer.cursor();
        browser.origin = (kind == BrowserKind::Files).then_some(ReferenceInsertion {
            line,
            start: col.saturating_sub(usize::from(at_sign)),
            end: col,
            revision: self.composer.editor_revision(),
        });
        self.slash_completion = None;
        self.dock = Dock::Workspace(Box::new(browser));
        vec![]
    }
    pub(super) fn close_workspace_browser(&mut self) {
        if let Some(browser) = self.workspace_browser() {
            let session = browser.session.clone();
            self.queries.invalidate_scope(&QueryScope::Workspace {
                session_id: session,
                file: false,
            });
            self.dock = Dock::Composer;
        }
    }
    pub(super) fn workspace_edit(
        &mut self,
        text: Option<&str>,
        backspace: bool,
        scope_toggle: bool,
        case_toggle: bool,
    ) -> Vec<AppCommand> {
        let due = self.instant_now() + Duration::from_millis(150);
        if let Dock::Workspace(browser) = &mut self.dock {
            if case_toggle && browser.kind != BrowserKind::Grep {
                return vec![];
            }
            if scope_toggle {
                browser.scope_focused = !browser.scope_focused;
                return vec![];
            }
            let input = if browser.scope_focused {
                &mut browser.scope
            } else {
                &mut browser.query
            };
            if backspace {
                input.pop();
            }
            if let Some(text) = text {
                let limit = if browser.scope_focused { 4096 } else { 1024 };
                if input.len() + text.len() > limit || text.contains(['\n', '\r', '\0']) {
                    return vec![];
                }
                input.push_str(text);
            }
            if case_toggle && browser.kind == BrowserKind::Grep {
                browser.case_sensitive = !browser.case_sensitive;
            }
            self.workspace_generation = self.workspace_generation.wrapping_add(1);
            browser.reset(self.workspace_generation, due);
        }
        vec![]
    }
    pub(super) fn workspace_move(&mut self, delta: i32) -> Vec<AppCommand> {
        if let Dock::Workspace(browser) = &mut self.dock {
            browser.selected = browser
                .selected
                .saturating_add_signed(delta as isize)
                .min(browser.len().saturating_sub(1));
        }
        vec![]
    }
    pub(super) fn workspace_more(&mut self, refresh: bool) -> Vec<AppCommand> {
        let now = self.instant_now();
        if let Dock::Workspace(browser) = &mut self.dock {
            if refresh {
                self.workspace_generation = self.workspace_generation.wrapping_add(1);
                browser.reset(self.workspace_generation, now);
            } else if browser.cursor.is_some() && !browser.limited && browser.error.is_none() {
                browser.due = Some(now);
            }
        }
        self.poll_workspace()
    }
    pub(super) fn workspace_select(&mut self, preview: bool) -> Vec<AppCommand> {
        let Some(browser) = self.workspace_browser() else {
            return vec![];
        };
        if browser.kind == BrowserKind::Files {
            let Some(entry) = browser.files.get(browser.selected) else {
                return vec![];
            };
            if entry.kind == FileKind::Directory {
                let path = entry.path.clone();
                let now = self.instant_now();
                self.workspace_generation = self.workspace_generation.wrapping_add(1);
                if let Dock::Workspace(b) = &mut self.dock {
                    b.scope = path;
                    b.reset(self.workspace_generation, now);
                }
                return self.poll_workspace();
            }
            if entry.kind != FileKind::File {
                self.notice(NoticeLevel::Info, "仅常规文件可预览或引用");
                return vec![];
            }
            let path = entry.path.clone();
            if !preview {
                let origin = browser.origin;
                let token = reference_token(&path);
                let Some(origin) = origin.filter(|o| o.revision == self.composer.editor_revision())
                else {
                    self.notice(NoticeLevel::Warning, "草稿已改变；重新打开候选后插入路径");
                    return vec![];
                };
                if self.composer.cursor() != (origin.line, origin.end) {
                    self.notice(NoticeLevel::Warning, "光标已移动；重新打开候选后插入路径");
                    return vec![];
                }
                // Keep the already typed @. One native insert preserves cursor,
                // paste ranges and undo; no u16 cursor jump or delete/insert pair.
                let insertion = if origin.end > origin.start {
                    &token[1..]
                } else {
                    &token
                };
                if !self.admit_draft_input(insertion.len().saturating_add(path.capacity()))
                    || !self.composer.can_insert_bytes(insertion.len())
                {
                    return vec![];
                }
                if !self.composer_mut().type_text(insertion) {
                    return vec![];
                }
                self.composer_mut().remember_file_reference(
                    origin.line,
                    origin.start,
                    token.chars().count(),
                    path,
                );
                self.close_workspace_browser();
                self.focus = Focus::Editor;
                self.notice(
                    NoticeLevel::Info,
                    "仅引用路径，模型需要时再读；预览内容不会附加到输入或 History",
                );
                return vec![];
            }
            return self.preview_browser_file(path, None);
        }
        let Some(item) = browser.matches.get(browser.selected) else {
            return vec![];
        };
        let target = FileRange {
            start_line: item.line_number,
            line_byte_offset: item
                .line_text_byte_offset
                .saturating_add(item.match_byte_ranges.first().map_or(0, |r| r.start)),
        };
        self.preview_browser_file(item.path.clone(), Some(target))
    }
    fn preview_browser_file(&mut self, path: String, target: Option<FileRange>) -> Vec<AppCommand> {
        let return_target = match std::mem::replace(&mut self.dock, Dock::Composer) {
            Dock::Workspace(mut browser) => {
                browser.due = None;
                self.queries.invalidate_scope(&QueryScope::Workspace {
                    session_id: browser.session.clone(),
                    file: false,
                });
                ReturnTarget::Browser(browser)
            }
            _ => ReturnTarget::Conversation,
        };
        self.open_file_preview(path, target, return_target)
    }
    pub fn open_file_preview(
        &mut self,
        path: String,
        target: Option<FileRange>,
        return_target: ReturnTarget,
    ) -> Vec<AppCommand> {
        let Some((session, epoch)) = self.workspace_owner() else {
            return vec![];
        };
        self.close_main_detail();
        self.capture_scroll_anchor();
        let scroll = self.active_view().map(|v| v.scroll.clone());
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        self.main_view = MainView::FilePreview(Box::new(FilePreviewState {
            session,
            epoch,
            generation: self.workspace_generation,
            path,
            conversation_scroll: scroll,
            return_target,
            requested: FileRange::default(),
            next: Some(FileRange::default()),
            revision: None,
            content: FileBuffer::default(),
            content_revision: 0,
            status: None,
            error: None,
            wanted: true,
            truncated: false,
            line_truncated: false,
            offset: 0,
            follow: false,
            scrollbar_grab: None,
            target,
            layout: None,
            layout_pending: None,
        }));
        self.focus = Focus::Main;
        self.slash_completion = None;
        self.poll_workspace()
    }
    pub(super) fn preview_reference(&mut self) -> Vec<AppCommand> {
        let Some(path) = self.composer.file_reference_at_cursor().map(str::to_owned) else {
            return vec![];
        };
        self.open_file_preview(path, None, ReturnTarget::Conversation)
    }
    pub(super) fn return_main_detail(&mut self) {
        let target = if let MainView::FilePreview(file) = &mut self.main_view {
            std::mem::replace(&mut file.return_target, ReturnTarget::Conversation)
        } else {
            ReturnTarget::Conversation
        };
        self.close_main_detail();
        if let ReturnTarget::Browser(mut browser) = target {
            if self.active_view().is_some_and(|v| {
                v.info.loaded
                    && v.session_epoch == browser.epoch
                    && v.info.session_id == browser.session
            }) {
                self.workspace_generation = self.workspace_generation.wrapping_add(1);
                browser.generation = self.workspace_generation;
                browser.due = None;
                self.dock = Dock::Workspace(browser);
            }
        }
    }
    pub(super) fn poll_workspace(&mut self) -> Vec<AppCommand> {
        let valid = |session: &str, epoch| {
            self.sessions.active.as_deref() == Some(session)
                && self
                    .sessions
                    .known
                    .get(session)
                    .is_some_and(|v| v.session_epoch == epoch && v.info.loaded)
        };
        let file_invalid = self
            .file_preview()
            .is_some_and(|f| !valid(&f.session, f.epoch));
        let browser_invalid = self
            .workspace_browser()
            .is_some_and(|b| !valid(&b.session, b.epoch));
        if file_invalid {
            self.close_main_detail();
        }
        if browser_invalid {
            self.close_workspace_browser();
        }
        if !self.can_send_requests() {
            return vec![];
        }
        let mut commands = vec![];
        for file_query in [true, false] {
            if self.deferred_pending() + self.queries.in_flight_len() >= MAX_DEFERRED_REQUESTS {
                break;
            }
            let candidate = if file_query {
                self.file_preview()
                    .filter(|f| f.wanted && f.error.is_none())
                    .map(|f| {
                        (
                            f.session.clone(),
                            f.epoch,
                            f.generation,
                            WorkspaceQuery::File,
                        )
                    })
            } else {
                self.workspace_browser()
                    .filter(|b| {
                        b.due.is_some_and(|d| d <= self.instant_now())
                            && b.error.is_none()
                            && !(b.kind == BrowserKind::Grep && b.query.is_empty())
                    })
                    .map(|b| {
                        (
                            b.session.clone(),
                            b.epoch,
                            b.generation,
                            if b.kind == BrowserKind::Files {
                                WorkspaceQuery::Files
                            } else {
                                WorkspaceQuery::Grep
                            },
                        )
                    })
            };
            let Some((session_id, epoch, generation, kind)) = candidate else {
                continue;
            };
            let key = QueryKey::Workspace {
                session_id: session_id.clone(),
                file: file_query,
            };
            // One outstanding request per concrete workspace consumer, including old generations.
            if self.pending_requests.values().any(|k| matches!(k, RequestKind::Workspace { kind, .. } if (*kind == WorkspaceQuery::File) == file_query)) { continue; }
            let paths = if kind == WorkspaceQuery::Grep {
                match self.workspace_browser().unwrap().paths() {
                    Ok(paths) => paths,
                    Err(error) => {
                        if let Dock::Workspace(b) = &mut self.dock {
                            b.error = Some(error.into());
                            b.due = None;
                        }
                        continue;
                    }
                }
            } else {
                vec![]
            };
            let id = self.next_request_id();
            if self.queries.request_query(key, id) != QueryAdmission::Admitted {
                continue;
            }
            let request = match kind {
                WorkspaceQuery::File => {
                    let MainView::FilePreview(file) = &mut self.main_view else {
                        unreachable!()
                    };
                    let range = file.next.unwrap_or_default();
                    file.requested = range;
                    file.wanted = false;
                    OutgoingRequest::workspace_read(
                        id,
                        &session_id,
                        &file.path,
                        range,
                        file.revision.as_deref(),
                    )
                }
                WorkspaceQuery::Files | WorkspaceQuery::Grep => {
                    let Dock::Workspace(b) = &mut self.dock else {
                        unreachable!()
                    };
                    b.due = None;
                    if kind == WorkspaceQuery::Files {
                        OutgoingRequest::workspace_files(
                            id,
                            &session_id,
                            &b.scope,
                            &b.query,
                            b.cursor.as_ref(),
                        )
                    } else {
                        OutgoingRequest::workspace_search(
                            id,
                            &session_id,
                            &b.query,
                            &paths,
                            b.case_sensitive,
                            b.cursor.as_ref(),
                        )
                    }
                }
            };
            self.pending_requests.insert(
                id,
                RequestKind::Workspace {
                    session_id,
                    epoch,
                    generation,
                    kind,
                },
            );
            commands.push(AppCommand::Rpc(request));
        }
        commands
    }
    pub(super) fn on_workspace_response(
        &mut self,
        session: String,
        epoch: u64,
        generation: u64,
        kind: WorkspaceQuery,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.active.as_deref() != Some(&session)
            || self
                .sessions
                .known
                .get(&session)
                .is_none_or(|v| !v.info.loaded || v.session_epoch != epoch)
        {
            return vec![];
        }
        if kind == WorkspaceQuery::File {
            if let MainView::FilePreview(file) = &mut self.main_view {
                if file.session != session || file.epoch != epoch || file.generation != generation {
                    return vec![];
                }
                let result = response
                    .result_as::<FilePage>()
                    .map_err(|_| "workspace.read unavailable or malformed; F5 to retry")
                    .and_then(|page| file.accept(page));
                if let Err(error) = result {
                    file.error = Some(error.into());
                    file.wanted = false;
                    file.next = None;
                }
            }
            return vec![];
        }
        let Dock::Workspace(browser) = &mut self.dock else {
            return vec![];
        };
        if browser.session != session || browser.epoch != epoch || browser.generation != generation
        {
            return vec![];
        }
        let result = if kind == WorkspaceQuery::Files {
            response
                .result_as::<FilesPage>()
                .map_err(|_| "workspace.files unavailable or malformed; F5 to retry")
                .and_then(|page| {
                    if !page.validate() {
                        return Err("malformed files cursor");
                    }
                    let selected = browser.files.get(browser.selected).map(|f| f.path.clone());
                    for entry in page.entries {
                        if browser.len() >= crate::limits::WORKSPACE_CANDIDATES
                            || browser.retained_bytes() + entry.path.capacity()
                                > crate::limits::WORKSPACE_CANDIDATE_BYTES
                        {
                            browser.limited = true;
                            break;
                        }
                        if !browser.files.iter().any(|f| f.path == entry.path) {
                            browser.files.push(entry);
                        }
                    }
                    browser.files.sort_by(|a, b| a.path.cmp(&b.path));
                    if let Some(selected) = selected {
                        browser.selected = browser
                            .files
                            .iter()
                            .position(|f| f.path == selected)
                            .unwrap_or(0);
                    }
                    browser.cursor = page.next_cursor;
                    browser.truncated = page.truncated;
                    browser.scan_complete = page.scan_complete;
                    browser.stopped_by = Some(page.stopped_by);
                    browser.skipped = page.skipped_count;
                    Ok(())
                })
        } else {
            response
                .result_as::<SearchPage>()
                .map_err(|_| "workspace.search unavailable or malformed; F5 to retry")
                .and_then(|page| {
                    if !page.validate() {
                        return Err("malformed search cursor or UTF-8 match range");
                    }
                    for item in page.matches {
                        let bytes = item.path.capacity()
                            + item.line_text.capacity()
                            + item.match_byte_ranges.capacity() * std::mem::size_of::<MatchRange>();
                        if browser.len() >= crate::limits::WORKSPACE_CANDIDATES
                            || browser.retained_bytes() + bytes
                                > crate::limits::WORKSPACE_CANDIDATE_BYTES
                        {
                            browser.limited = true;
                            break;
                        }
                        browser.matches.push(item);
                    }
                    browser.cursor = page.next_cursor;
                    browser.truncated = page.truncated;
                    browser.scan_complete = page.scan_complete;
                    browser.stopped_by = Some(page.stopped_by);
                    browser.skipped = page.skipped_files;
                    Ok(())
                })
        };
        if let Err(error) = result {
            browser.error = Some(error.into());
            browser.cursor = None;
        }
        // A deadline is never an implicit rescan, even if a malformed peer supplied a cursor.
        if browser.stopped_by == Some(ScanStop::Deadline) {
            browser.cursor = None;
        }
        if browser.cursor.is_some()
            && (browser.len() >= crate::limits::WORKSPACE_CANDIDATES
                || browser.retained_bytes() >= crate::limits::WORKSPACE_CANDIDATE_BYTES)
        {
            browser.limited = true;
        }
        browser.due = None;
        vec![]
    }
    pub(super) fn file_more(&mut self, refresh: bool) -> Vec<AppCommand> {
        if refresh {
            if let MainView::FilePreview(file) = &mut self.main_view {
                self.workspace_generation = self.workspace_generation.wrapping_add(1);
                file.generation = self.workspace_generation;
                file.content = FileBuffer::default();
                file.content_revision = 0;
                file.revision = None;
                file.status = None;
                file.error = None;
                file.next = Some(FileRange::default());
                file.wanted = true;
                file.layout = None;
                file.layout_pending = None;
            }
        } else if let MainView::FilePreview(file) = &mut self.main_view {
            if file.status == Some(FileStatus::Ok) && file.error.is_none() && file.next.is_some() {
                file.wanted = true;
            }
        }
        self.poll_workspace()
    }
    pub(super) fn copy_file(&mut self) -> Vec<AppCommand> {
        let Some(file) = self.file_preview() else {
            return vec![];
        };
        if file.content.bytes == 0 && file.status != Some(FileStatus::Ok) {
            self.notice(
                NoticeLevel::Info,
                "没有可复制正文：文件尚未可用，不能当作空文件成功",
            );
            return vec![];
        }
        let Some(layout) = &file.layout else {
            self.notice(NoticeLevel::Info, "文件仍在布局或没有可复制正文");
            return vec![];
        };
        let text = layout.copy_text.to_string();
        if file.next.is_some()
            || file.status != Some(FileStatus::Ok)
            || file.error.is_some()
            || layout.identity.revision != file.content_revision
        {
            self.notice(
                NoticeLevel::Warning,
                "仅复制已显示的旧/部分文件；无行号或软换行装饰",
            );
        }
        vec![self.capture_copy(text)]
    }
    pub fn file_layout_request(&self, width: u16) -> Option<FileLayoutRequest> {
        let file = self.file_preview()?;
        file.status?;
        if file
            .layout_pending
            .as_ref()
            .is_some_and(|i| i.generation == file.generation && i.width == width)
        {
            return None;
        }
        let identity = FileLayoutIdentity {
            generation: file.generation,
            revision: file.content_revision,
            width,
        };
        if file.layout.as_ref().is_some_and(|l| l.identity == identity) {
            return None;
        }
        Some(FileLayoutRequest {
            identity,
            content: file.content.clone(),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    pub fn mark_file_layout_pending(&mut self, identity: FileLayoutIdentity) {
        if let MainView::FilePreview(file) = &mut self.main_view {
            file.layout_pending = Some(identity);
        }
    }
    pub(super) fn install_file_layout(&mut self, layout: FileLayout) {
        if let MainView::FilePreview(file) = &mut self.main_view {
            if file.layout_pending.as_ref() != Some(&layout.identity)
                || file.generation != layout.identity.generation
            {
                return;
            }
            if let Some(target) = file.target {
                if let Some(index) = layout.rows.iter().rposition(|r| {
                    r.source.start_line == target.start_line
                        && r.source.line_byte_offset <= target.line_byte_offset
                }) {
                    file.offset = index;
                    file.follow = false;
                    if file.next.is_none_or(|next| {
                        (next.start_line, next.line_byte_offset)
                            > (target.start_line, target.line_byte_offset)
                    }) {
                        file.target = None;
                    }
                }
            }
            file.layout_pending = None;
            file.layout = Some(layout);
        }
    }
    pub(super) fn scroll_file(&mut self, delta: i32, end: bool) {
        let height = self.main_body_area().height as usize;
        if let MainView::FilePreview(file) = &mut self.main_view {
            let offset = file.scroll_offset(height);
            file.follow = end;
            if !end {
                file.offset = offset.saturating_add_signed(delta as isize);
            }
        }
    }
    pub fn main_body_area(&self) -> ratatui::layout::Rect {
        if self.tool_detail().is_some() {
            return self.tool_body_area();
        }
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        crate::ui::workspace::file_body(screen.transcript)
    }
}
