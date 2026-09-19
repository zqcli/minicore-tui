//! Local conversation export orchestration (spec §17.4).
//!
//! The App never renders or writes the file: a pinned `session.read` chain
//! feeds the existing single decode worker, which renders each item's bounded
//! Markdown, and the App forwards those records into the one owned export job.
//! The hand-off channel is bounded, so when the writer is slower than the read
//! chain the outbox holds at most one record and paging pauses: a huge session
//! can never become an in-memory buffer. The pin captured at the start is held
//! until the export finishes, so a new turn never joins this file.

use super::*;
use crate::app::history::ReadPage;
use crate::jobs::{ExportHeader, ExportInbound, ExportOutcome, ExportRecord};
use crate::state::export::{ExportLimitations, header_notes, unsaved_markdown, validate_target};

/// Export-local read chain. `index` is the next item the writer expects, so an
/// out-of-order or duplicate decode result is dropped rather than written into
/// the wrong position.
#[derive(Debug)]
pub(super) struct ExportScan {
    pub session_id: SessionId,
    pub export_id: u64,
    pub session_epoch: u64,
    pub pin: Option<crate::protocol::SnapshotPin>,
    pub next: Option<crate::protocol::ReadCursor>,
    pub page: Option<ReadPage>,
    pub terminal: bool,
    pub items: usize,
    pub limitations: ExportLimitations,
    pub abort: bool,
}

impl ExportScan {
    pub fn has_pending_decode(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| !page.pending_encoded.is_empty())
    }

    /// A page is loaded and fully forwarded, but its chain has not advanced.
    pub fn page_consumed(&self) -> bool {
        self.page.is_some() && !self.has_pending_decode()
    }
}

impl App {
    /// Opens the export form. `target` may be prefilled by `/export <path>`.
    pub(super) fn open_export_form(&mut self, target: String) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.sessions.active.is_none() {
            self.notice(NoticeLevel::Info, "open a session before exporting");
            return Vec::new();
        }
        if self.export_scan.is_some() {
            self.notice(NoticeLevel::Info, "an export is already running");
            return Vec::new();
        }
        self.dock = Dock::Export(crate::state::export::ExportFormState::new(target));
        self.panel_scroll = 0;
        Vec::new()
    }

    /// Read-only view of the export form, for the renderer and tests.
    pub fn export_form(&self) -> Option<&crate::state::export::ExportFormState> {
        match &self.dock {
            Dock::Export(form) => Some(form),
            _ => None,
        }
    }

    /// Whether the one owned export chain is still running.
    pub fn export_running(&self) -> bool {
        self.export_scan.is_some()
    }

    fn export_form_mut(&mut self) -> Option<&mut crate::state::export::ExportFormState> {
        match &mut self.dock {
            Dock::Export(form) => Some(form),
            _ => None,
        }
    }

    pub(super) fn export_type_char(&mut self, ch: char) {
        let Some(form) = self.export_form_mut() else {
            return;
        };
        if form.running() || ch.is_control() {
            return;
        }
        form.target.push(ch);
    }

    pub(super) fn export_backspace(&mut self) {
        let Some(form) = self.export_form_mut() else {
            return;
        };
        if !form.running() {
            form.target.pop();
        }
    }

    pub(super) fn export_clear(&mut self) {
        let Some(form) = self.export_form_mut() else {
            return;
        };
        if !form.running() {
            form.target.clear();
        }
    }

    pub(super) fn export_toggle_thinking(&mut self) {
        if let Some(form) = self.export_form_mut() {
            if !form.running() {
                form.spec.include_thinking = !form.spec.include_thinking;
            }
        }
    }

    pub(super) fn export_toggle_tool(&mut self) {
        if let Some(form) = self.export_form_mut() {
            if !form.running() {
                form.spec.include_tool = !form.spec.include_tool;
            }
        }
    }

    /// The separate, explicit choice to append live turns that are not part of
    /// the saved history. It is never the default.
    pub(super) fn export_toggle_unsaved(&mut self) {
        if let Some(form) = self.export_form_mut() {
            if !form.running() {
                form.include_unsaved = !form.include_unsaved;
            }
        }
    }

    /// The explicit overwrite confirmation. It only takes effect on the next
    /// submit: the first attempt never replaces an existing file.
    pub(super) fn export_toggle_overwrite(&mut self) {
        if let Some(form) = self.export_form_mut() {
            if !form.running() {
                form.overwrite = !form.overwrite;
                form.notice = None;
            }
        }
    }

    /// Closes the form. A running export is cancelled: the owned job removes
    /// its uncommitted temp file.
    pub(super) fn export_escape(&mut self) -> Vec<AppCommand> {
        if self.export_scan.is_some() {
            self.cancel_export();
        }
        self.dock = Dock::Composer;
        Vec::new()
    }

    /// Aborts the read chain and tells the job to remove its temp file.
    pub(super) fn cancel_export(&mut self) {
        self.export_scan = None;
        self.export_outbox.clear();
        self.export_hold = false;
        if let Some(tx) = self.export_tx.take() {
            let _ = tx.try_send(ExportInbound::Abort);
        }
        if let Some(form) = self.export_form_mut() {
            if form.running() {
                form.phase = crate::state::export::ExportPhase::Failed;
                form.notice = Some("export cancelled; no file was written".to_owned());
            }
        }
    }

    /// Starts the export: validates the target locally, opens the bounded
    /// channel the job drains and begins the pinned read chain.
    pub(super) fn export_submit(&mut self) -> Vec<AppCommand> {
        let Some(form) = self.export_form() else {
            return Vec::new();
        };
        if form.running() {
            return Vec::new();
        }
        let target = match validate_target(&form.target) {
            Ok(path) => path,
            Err(detail) => {
                if let Some(form) = self.export_form_mut() {
                    form.notice = Some(detail);
                }
                return Vec::new();
            }
        };
        let overwrite = form.overwrite;
        let spec = form.spec;
        let include_unsaved = form.include_unsaved;
        let Some((session_id, epoch)) = self
            .sessions
            .active
            .clone()
            .and_then(|id| Some((id.clone(), self.sessions.known.get(&id)?.session_epoch)))
        else {
            if let Some(form) = self.export_form_mut() {
                form.notice = Some("open a session before exporting".to_owned());
            }
            return Vec::new();
        };
        let pin = self
            .sessions
            .known
            .get(&session_id)
            .and_then(|view| view.transcript.window.pin().cloned());
        self.export_id = self.export_id.wrapping_add(1);
        let export_id = self.export_id;
        let (tx, rx) = tokio::sync::mpsc::channel::<ExportInbound>(2);
        self.export_tx = Some(tx);
        self.export_outbox.clear();
        self.export_hold = false;
        self.export_spec = spec;
        self.export_include_unsaved = include_unsaved;
        self.export_scan = Some(ExportScan {
            session_id,
            export_id,
            session_epoch: epoch,
            pin,
            next: None,
            page: None,
            terminal: false,
            items: 0,
            limitations: ExportLimitations::default(),
            abort: false,
        });
        if let Some(form) = self.export_form_mut() {
            form.phase = crate::state::export::ExportPhase::Running;
            form.notice = None;
            form.limitations = ExportLimitations::default();
        }
        let notes = header_notes(
            if include_unsaved {
                "saved history plus explicitly appended live turns"
            } else {
                "saved history (session_read snapshot)"
            },
            &ExportLimitations::default(),
        );
        self.queue_export_message(ExportInbound::Header(Box::new(ExportHeader { notes })));
        let mut commands = vec![AppCommand::StartExport(Box::new(
            crate::command::StartExportRequest {
                target,
                overwrite,
                rx,
                spec,
            },
        ))];
        commands.extend(self.pump_export());
        commands
    }

    /// Hands one message to the owned job, or parks it in the outbox when the
    /// bounded channel is full. The chain pauses until it is delivered.
    fn queue_export_message(&mut self, message: ExportInbound) {
        if self.export_outbox.is_empty() {
            match self.export_tx.as_ref().map(|tx| tx.try_send(message)) {
                Some(Ok(())) => return,
                Some(Err(tokio::sync::mpsc::error::TrySendError::Full(message))) => {
                    self.export_outbox.push_back(message);
                    return;
                }
                Some(Err(tokio::sync::mpsc::error::TrySendError::Closed(_))) | None => {
                    self.fail_export("the export writer stopped before it finished".to_owned());
                    return;
                }
            }
        }
        self.export_outbox.push_back(message);
    }

    /// Drains the outbox and continues the chain once it is empty. Called at
    /// the start of every update so a slow disk only pauses the export.
    pub(super) fn pump_export(&mut self) -> Vec<AppCommand> {
        while let Some(message) = self.export_outbox.pop_front() {
            match self.export_tx.as_ref().map(|tx| tx.try_send(message)) {
                Some(Ok(())) => {}
                Some(Err(tokio::sync::mpsc::error::TrySendError::Full(message))) => {
                    self.export_outbox.push_front(message);
                    self.export_hold = true;
                    return Vec::new();
                }
                Some(Err(tokio::sync::mpsc::error::TrySendError::Closed(_))) | None => {
                    self.fail_export("the export writer stopped before it finished".to_owned());
                    return Vec::new();
                }
            }
        }
        self.export_hold = false;
        self.continue_export_chain()
    }

    /// Continues whichever step the chain is waiting on: a queued decode, a
    /// loaded page, or the next page read.
    fn continue_export_chain(&mut self) -> Vec<AppCommand> {
        let Some(scan) = self.export_scan.as_ref() else {
            return Vec::new();
        };
        if scan.abort {
            return Vec::new();
        }
        if scan.has_pending_decode() {
            self.queue_export_decode();
            return Vec::new();
        }
        if scan.page_consumed() {
            return self.advance_export_scan();
        }
        if scan.page.is_none() {
            return self.request_export_page();
        }
        Vec::new()
    }

    /// Requests the next export page through the two shared read-only slots.
    fn request_export_page(&mut self) -> Vec<AppCommand> {
        let Some(scan) = self.export_scan.as_ref() else {
            return Vec::new();
        };
        if scan.abort || scan.terminal {
            return Vec::new();
        }
        let session_id = scan.session_id.clone();
        let export_id = scan.export_id;
        let cursor = scan.next.unwrap_or_else(crate::protocol::ReadCursor::start);
        let pin = scan.pin.clone();
        let probe = pin.is_none();
        let id = self.next_request_id();
        let key = crate::app::queries::QueryKey::Export {
            session_id: session_id.clone(),
            export_id,
        };
        match self.queries.request_query(key, id) {
            crate::app::queries::QueryAdmission::Admitted => {}
            crate::app::queries::QueryAdmission::Coalesced
            | crate::app::queries::QueryAdmission::Busy => return Vec::new(),
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
            RequestKind::ExportRead {
                session_id,
                export_id,
            },
        );
        vec![AppCommand::Rpc(request)]
    }

    /// The export twin of [`App::resume_idle_search_page`]: a page whose
    /// admission was queued is retried as soon as no export read is in flight.
    pub(super) fn resume_idle_export_page(&mut self) -> Vec<AppCommand> {
        let needs_page = self
            .export_scan
            .as_ref()
            .is_some_and(|scan| scan.page.is_none() && !scan.terminal && !scan.abort);
        if !needs_page || !self.export_outbox.is_empty() {
            return Vec::new();
        }
        let in_flight = self
            .pending_requests
            .values()
            .any(|kind| matches!(kind, RequestKind::ExportRead { .. }));
        if in_flight {
            return Vec::new();
        }
        self.request_export_page()
    }

    /// Re-issues a queued export page (or drains the outbox) after a read slot
    /// became available.
    pub(super) fn resume_export_scan(
        &mut self,
        session_id: &str,
        export_id: u64,
    ) -> Vec<AppCommand> {
        let owned = self
            .export_scan
            .as_ref()
            .is_some_and(|scan| scan.session_id == session_id && scan.export_id == export_id);
        if !owned {
            return Vec::new();
        }
        self.pump_export()
    }

    /// Applies one export page: the pin is captured from the first page and
    /// every item is queued for the decode worker in order.
    pub(super) fn on_export_read_response(
        &mut self,
        session_id: &SessionId,
        export_id: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let owned = self
            .export_scan
            .as_ref()
            .is_some_and(|scan| scan.session_id == *session_id && scan.export_id == export_id);
        if !owned {
            return Vec::new();
        }
        let page = match response.parse_session_read() {
            Ok(page) => page,
            Err(error) => {
                self.fail_export(format!("an export page is not readable: {error}"));
                return Vec::new();
            }
        };
        if page.session.session_id != *session_id {
            self.fail_export("the export page belongs to another session".to_owned());
            return Vec::new();
        }
        let pin = page.pin();
        let next = page.next_cursor;
        let records_truncated = page.records_truncated;
        let next_is_none = next.is_none();
        let previous_cursor = self
            .export_scan
            .as_ref()
            .and_then(|scan| scan.page.as_ref().map(|page| page.cursor))
            .unwrap_or_else(crate::protocol::ReadCursor::start);
        let (cursor, mut page_state) = {
            let Some(scan) = self.export_scan.as_mut() else {
                return Vec::new();
            };
            if scan.pin.is_none() {
                scan.pin = Some(pin.clone());
            }
            scan.next = next;
            scan.terminal = next_is_none;
            if records_truncated {
                scan.limitations.records_truncated = true;
            }
            let page_state = scan
                .page
                .take()
                .unwrap_or_else(|| ReadPage::new(previous_cursor, None, 0));
            let cursor = page_state.cursor;
            (cursor, page_state)
        };
        let want_pin = self.export_scan.as_ref().and_then(|scan| scan.pin.clone());
        page_state.cursor = cursor;
        page_state.want_pin = want_pin;
        page_state.pending_page = None;
        let mut failed = 0usize;
        for chunk in &page.items {
            match page_state.assembler.push(chunk.clone()) {
                Ok(crate::protocol::read::Assembled::Pending) => {}
                Ok(crate::protocol::read::Assembled::EncodedItem { item }) => {
                    page_state.pending_encoded.push_back(item);
                }
                Ok(crate::protocol::read::Assembled::LargeItem { total_bytes, .. })
                | Ok(crate::protocol::read::Assembled::LargeItemPending { total_bytes, .. }) => {
                    // The export never assembles an unbounded item: it writes a
                    // bounded placeholder and records the limitation.
                    self.note_export_oversized(total_bytes);
                }
                Err(_) => failed += 1,
            }
        }
        if failed > 0 {
            if let Some(scan) = self.export_scan.as_mut() {
                scan.limitations.read_failed += failed;
            }
        }
        if let Some(scan) = self.export_scan.as_mut() {
            scan.page = Some(page_state);
        }
        self.pump_export()
    }

    /// One oversized item: a placeholder record plus an explicit limitation.
    fn note_export_oversized(&mut self, total_bytes: usize) {
        if let Some(scan) = self.export_scan.as_mut() {
            scan.limitations.oversized_items += 1;
        }
        self.queue_export_message(ExportInbound::Item(Box::new(ExportRecord {
            markdown: String::new(),
            oversized: Some(total_bytes),
        })));
        if let Some(scan) = self.export_scan.as_mut() {
            scan.items += 1;
        }
    }

    /// Queues the next pending export item on the single decode worker.
    pub(super) fn queue_export_decode(&mut self) {
        if self.pending_decode.is_some() || self.decode_in_flight.is_some() {
            return;
        }
        let Some((session_epoch, export_id, item, spec)) =
            self.export_scan.as_ref().and_then(|scan| {
                scan.page
                    .as_ref()
                    .and_then(|page| page.pending_encoded.front().cloned())
                    .map(|item| (scan.session_epoch, scan.export_id, item, self.export_spec))
            })
        else {
            return;
        };
        let session_id = self
            .export_scan
            .as_ref()
            .map(|scan| scan.session_id.clone())
            .unwrap_or_default();
        let identity = crate::jobs::DecodeIdentity {
            session_epoch,
            read_chain: export_id,
            target: crate::jobs::DecodeTarget::ExportItem {
                session_id,
                export_id,
                index: item.index,
            },
        };
        let request = crate::jobs::DecodeRequest {
            identity: identity.clone(),
            fingerprint: crate::app::history::encoded_item_fingerprint(&item),
            item,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan: None,
            export: Some(Box::new(spec)),
        };
        self.decode_in_flight = Some(identity);
        self.pending_decode = Some(request);
    }

    /// Forwards one rendered item to the writer in exact index order and
    /// continues the chain.
    pub(super) fn finish_export_item_decoded(
        &mut self,
        session_id: &SessionId,
        export_id: u64,
        index: usize,
        fingerprint: u64,
        outcome: &crate::jobs::DecodeOutcome,
    ) -> Vec<AppCommand> {
        let owned = self
            .export_scan
            .as_ref()
            .is_some_and(|scan| scan.session_id == *session_id && scan.export_id == export_id);
        if !owned {
            return Vec::new();
        }
        let expected = self.export_scan.as_ref().and_then(|scan| {
            scan.page
                .as_ref()
                .and_then(|page| page.pending_encoded.front().cloned())
        });
        let Some(expected) = expected else {
            return Vec::new();
        };
        if expected.index != index
            || crate::app::history::encoded_item_fingerprint(&expected) != fingerprint
        {
            return Vec::new();
        }
        let rendered = outcome
            .export
            .as_deref()
            .map(|rendered| (rendered.markdown.clone(), rendered.opaque_parts));
        if rendered.is_none() {
            // A cancelled or failed decode: the item is not in the file, and
            // the header notes record that.
            if let Some(scan) = self.export_scan.as_mut() {
                if let Some(page) = scan.page.as_mut() {
                    page.pending_encoded.pop_front();
                }
                scan.limitations.read_failed += 1;
            }
            return self.pump_export();
        }
        let (markdown, opaque_parts) = rendered.expect("checked above");
        if let Some(scan) = self.export_scan.as_mut() {
            if let Some(page) = scan.page.as_mut() {
                page.pending_encoded.pop_front();
            }
            scan.limitations.opaque_parts += opaque_parts;
            scan.items += 1;
        }
        self.queue_export_message(ExportInbound::Item(Box::new(ExportRecord {
            markdown,
            oversized: None,
        })));
        self.pump_export()
    }

    /// After one page's items are written: finish at the captured end or read
    /// the next page.
    fn advance_export_scan(&mut self) -> Vec<AppCommand> {
        let terminal = self.export_scan.as_ref().is_some_and(|scan| scan.terminal);
        if terminal {
            self.finish_export_scan();
            return Vec::new();
        }
        self.request_export_page()
    }

    /// Ends the read chain: the live turn is appended only when the user chose
    /// it, then the writer receives its trailing limitation notes and commits
    /// the file.
    fn finish_export_scan(&mut self) {
        if self.export_include_unsaved {
            let blocks = self.live_export_blocks();
            if !blocks.is_empty() {
                if let Some(scan) = self.export_scan.as_mut() {
                    scan.limitations.unsaved_turns += 1;
                }
                self.queue_export_message(ExportInbound::Item(Box::new(ExportRecord {
                    markdown: unsaved_markdown(&blocks),
                    oversized: None,
                })));
            }
        }
        let limitations = self
            .export_scan
            .as_ref()
            .map(|scan| scan.limitations)
            .unwrap_or_default();
        self.queue_export_message(ExportInbound::Finish(Box::new(limitations)));
        self.export_scan = None;
        if let Some(form) = self.export_form_mut() {
            form.limitations = limitations;
        }
    }

    /// The live in-memory turn, already visible in the transcript but not yet
    /// part of the saved history.
    fn live_export_blocks(&self) -> Vec<(String, String)> {
        let Some(view) = self
            .sessions
            .active
            .as_ref()
            .and_then(|id| self.sessions.known.get(id))
        else {
            return Vec::new();
        };
        let Some(loop_state) = view.live.as_ref() else {
            return Vec::new();
        };
        let mut blocks = Vec::new();
        if !loop_state.user_text.trim().is_empty() {
            blocks.push(("User (live)".to_owned(), loop_state.user_text.clone()));
        }
        for request in &loop_state.requests {
            let mut text = String::new();
            for part in &request.parts {
                match part {
                    crate::state::turn::LivePart::Text(chunk) => text.push_str(chunk),
                    crate::state::turn::LivePart::Reasoning(chunk) => {
                        if self.export_spec.include_thinking {
                            text.push_str("\n\n[thinking] ");
                            text.push_str(chunk);
                        }
                    }
                    crate::state::turn::LivePart::Tool { tool_call_id } => {
                        if self.export_spec.include_tool {
                            text.push_str(&format!("\n\n[tool call {tool_call_id}]"));
                        }
                    }
                }
            }
            if !text.trim().is_empty() {
                blocks.push((
                    format!("Assistant (live, request {})", request.request_index),
                    text,
                ));
            }
        }
        blocks
    }

    /// Ends the export with a local failure: the job removes the temp file and
    /// the form reports the exact error.
    pub(super) fn fail_export(&mut self, detail: String) {
        self.export_scan = None;
        self.export_outbox.clear();
        self.export_hold = false;
        if let Some(tx) = self.export_tx.take() {
            let _ = tx.try_send(ExportInbound::Abort);
        }
        if let Some(form) = self.export_form_mut() {
            form.phase = crate::state::export::ExportPhase::Failed;
            form.notice = Some(detail);
        }
    }

    /// Applies the owned job's completion.
    pub(super) fn on_export_job_finished(&mut self, outcome: ExportOutcome) -> Vec<AppCommand> {
        let partial = self
            .export_form()
            .map(|form| form.limitations.is_partial())
            .unwrap_or(false);
        // Every completion ends this export: the read chain must not keep
        // paging into a file that will not be committed (the pre-flight
        // overwrite refusal returns before it drains a single record).
        self.export_scan = None;
        self.export_outbox.clear();
        self.export_hold = false;
        self.export_tx = None;
        let Some(form) = self.export_form_mut() else {
            return Vec::new();
        };
        match outcome {
            ExportOutcome::Finished {
                target,
                bytes,
                items,
            } => {
                form.phase = crate::state::export::ExportPhase::Done;
                form.items = items;
                form.bytes = bytes;
                form.notice = Some(format!(
                    "exported {items} item(s), {bytes} bytes to {target}{}",
                    if partial {
                        " (partial: the file records the limitations)"
                    } else {
                        ""
                    }
                ));
                self.notice(NoticeLevel::Info, format!("export finished: {target}"));
            }
            ExportOutcome::WouldOverwrite { target } => {
                // The explicit overwrite confirmation: nothing was written.
                form.phase = crate::state::export::ExportPhase::Editing;
                form.notice = Some(format!(
                    "{target} already exists — Ctrl+Y marks the export as overwriting it"
                ));
            }
            ExportOutcome::Cancelled { target } => {
                form.phase = crate::state::export::ExportPhase::Failed;
                form.notice = Some(format!("export cancelled; nothing was written to {target}"));
            }
            ExportOutcome::Failed {
                target,
                error,
                temp_removed,
            } => {
                form.phase = crate::state::export::ExportPhase::Failed;
                form.notice = Some(match temp_removed {
                    true => format!("export failed: {error} (the temporary file was removed)"),
                    false => format!(
                        "export failed: {error}; the temporary file next to {target} could not be \
                         confirmed removed"
                    ),
                });
                self.notice(
                    NoticeLevel::Warning,
                    "the export failed; see the export panel",
                );
            }
        }
        Vec::new()
    }
}
