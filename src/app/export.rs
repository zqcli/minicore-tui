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
/// the wrong position. `raw` streams an item above the auto-decode ceiling as
/// verbatim chunks instead of typed-decoding it (spec §17.4).
#[derive(Debug)]
pub(super) struct ExportScan {
    pub session_id: SessionId,
    pub export_id: u64,
    pub session_epoch: u64,
    pub pin: Option<crate::protocol::SnapshotPin>,
    /// The captured `total`, fixed for the whole chain: a later page may not
    /// change it (spec §17.1/§17.4).
    pub total: Option<usize>,
    pub next: Option<crate::protocol::ReadCursor>,
    pub page: Option<ReadPage>,
    pub terminal: bool,
    /// The current raw item being streamed, if any.
    pub raw: Option<crate::state::export::RawItemStream>,
    pub items: usize,
    pub limitations: ExportLimitations,
    /// A real failure was recorded: the chain stops paging and waits for the
    /// owned job's typed outcome instead of claiming a result locally.
    pub stop: bool,
}

impl ExportScan {
    pub fn has_pending_decode(&self) -> bool {
        self.raw.is_none()
            && self
                .page
                .as_ref()
                .is_some_and(|page| !page.pending_encoded.is_empty())
    }

    /// A page is loaded and fully forwarded, but its chain has not advanced.
    pub fn page_consumed(&self) -> bool {
        self.raw.is_none() && self.page.is_some() && !self.has_pending_decode()
    }

    /// The next read cursor, including the exact offset inside an unfinished
    /// raw item (spec §17.4: a raw continuation is a real item offset).
    pub fn next_cursor(&self) -> crate::protocol::ReadCursor {
        if let Some(stream) = self.raw.as_ref() {
            return crate::protocol::ReadCursor {
                item: stream.index,
                offset: stream.next_offset,
            };
        }
        self.next.unwrap_or_else(crate::protocol::ReadCursor::start)
    }
}

impl App {
    /// Opens the export form. `target` may be prefilled by `/export <path>`;
    /// `raw_oversized` mirrors `/export raw <path>`.
    pub(super) fn open_export_form(
        &mut self,
        target: String,
        raw_oversized: bool,
    ) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.sessions.active.is_none() {
            self.notice(NoticeLevel::Info, "open a session before exporting");
            return Vec::new();
        }
        if self.export_running() {
            self.notice(NoticeLevel::Info, "an export is already running");
            return Vec::new();
        }
        let mut form = crate::state::export::ExportFormState::new(target);
        form.spec.raw_oversized = raw_oversized;
        self.dock = Dock::Export(form);
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

    /// Whether the one owned export chain or its job is still in flight. A
    /// completion clears both, so a busy admission never overwrites an owner
    /// that has not reported yet (spec §17.4).
    pub fn export_running(&self) -> bool {
        self.export_scan.is_some() || self.export_tx.is_some()
    }

    /// Whether the one owned writer slot is still owned by a previous export
    /// that has not reported its typed outcome yet.
    pub fn export_owner_busy(&self) -> bool {
        self.export_tx.is_some()
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

    /// The separate, explicit choice to stream items above the automatic
    /// decode ceiling as raw sanitized Runtime JSON instead of placeholders
    /// (spec §17.4). It never raises the 8 MiB automatic decode ceiling.
    pub(super) fn export_toggle_raw(&mut self) {
        if let Some(form) = self.export_form_mut() {
            if !form.running() {
                form.spec.raw_oversized = !form.spec.raw_oversized;
                form.notice = None;
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
    /// its uncommitted temp file, but the phase becomes `Cancelling` until the
    /// job reports whether it committed first (spec §17.4).
    pub(super) fn export_escape(&mut self) -> Vec<AppCommand> {
        if self.export_running() {
            self.cancel_export();
        }
        self.dock = Dock::Composer;
        Vec::new()
    }

    /// Requests a cancel. This never claims "no file was written": the phase
    /// moves to `Cancelling` and the typed job outcome decides committed vs
    /// cancelled vs unknown. The owned job (and its bounded channel) is kept
    /// alive until that outcome arrives.
    pub fn cancel_export(&mut self) {
        if let Some(scan) = self.export_scan.as_mut() {
            scan.stop = true;
        }
        // The shared cancel token is observed by the writer's own wait loop,
        // so a full channel can never trap the abort. The owned job still owns
        // the channel until it reports its typed outcome.
        if let Some(cancel) = self.export_cancel.as_ref() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.export_outbox.clear();
        self.export_hold = false;
        if let Some(form) = self.export_form_mut() {
            if form.phase == crate::state::export::ExportPhase::Running {
                form.phase = crate::state::export::ExportPhase::Cancelling;
                form.notice = Some(
                    "cancelling: waiting for the writer to confirm whether it committed".to_owned(),
                );
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
        // The one owned writer slot has not reported its previous outcome yet:
        // starting another job would orphan that owner (spec §17.4). The user
        // waits for the typed completion instead.
        if self.export_owner_busy() {
            if let Some(form) = self.export_form_mut() {
                form.notice = Some(
                    "the previous export is still being reported; try again shortly".to_owned(),
                );
            }
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
        let export_session_id = session_id.clone();
        let export_epoch = epoch;
        // The export opens its own pinned chain from item 0 (spec §6.4): the
        // first page establishes the captured prefix, which then stays fixed
        // for the whole export. It deliberately does not inherit the loaded
        // window's pin, so an old view revision can never bound the file.
        self.export_id = self.export_id.wrapping_add(1);
        let export_id = self.export_id;
        let (tx, rx) = tokio::sync::mpsc::channel::<ExportInbound>(2);
        self.export_tx = Some(tx);
        self.export_cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        self.export_outbox.clear();
        self.export_hold = false;
        self.export_spec = spec;
        self.export_include_unsaved = include_unsaved;
        self.export_capture = Some(crate::jobs::ExportCapture {
            export_id,
            session_id: export_session_id.clone(),
            session_epoch: export_epoch,
        });
        self.export_scan = Some(ExportScan {
            session_id,
            export_id,
            session_epoch: epoch,
            pin: None,
            total: None,
            next: None,
            page: None,
            terminal: false,
            raw: None,
            items: 0,
            limitations: ExportLimitations::default(),
            stop: false,
        });
        if let Some(form) = self.export_form_mut() {
            form.phase = crate::state::export::ExportPhase::Running;
            form.notice = None;
            form.completion = None;
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
                capture: crate::jobs::ExportCapture {
                    export_id,
                    session_id: export_session_id.clone(),
                    session_epoch: export_epoch,
                },
                target,
                overwrite,
                cancel: self
                    .export_cancel
                    .as_ref()
                    .map(Arc::clone)
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false))),
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
                    // The App still owns the writer slot: the typed outcome is
                    // what tells committed/cancelled/unknown (spec §17.4).
                    self.note_export_writer_gone();
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
                    self.note_export_writer_gone();
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
        if scan.stop {
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
            // A raw item in progress still needs its continuation chunks.
            if scan.terminal {
                return self.finish_export_scan();
            }
            return self.request_export_page();
        }
        Vec::new()
    }

    /// Requests the next export page through the two shared read-only slots.
    fn request_export_page(&mut self) -> Vec<AppCommand> {
        let Some(scan) = self.export_scan.as_ref() else {
            return Vec::new();
        };
        if scan.stop || scan.terminal {
            return Vec::new();
        }
        let session_id = scan.session_id.clone();
        let export_id = scan.export_id;
        let cursor = scan.next_cursor();
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
            .is_some_and(|scan| scan.page.is_none() && !scan.terminal && !scan.stop);
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

    /// Applies one export page through the shared pinned-chain validator. The
    /// pin/total are fixed for the chain; oversized items are either streamed
    /// verbatim as raw chunks or recorded as an explicit placeholder, and a
    /// real validation failure stops the chain honestly (spec §17.4).
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
                self.note_export_stale(format!("an export page is not readable: {error}"));
                if let Some(scan) = self.export_scan.as_mut() {
                    scan.stop = true;
                    scan.limitations.read_stopped = true;
                }
                return self.finish_export_scan();
            }
        };
        if page.session.session_id != *session_id {
            self.note_export_stale("the export page belongs to another session".to_owned());
            if let Some(scan) = self.export_scan.as_mut() {
                scan.stop = true;
                scan.limitations.read_stopped = true;
            }
            return self.finish_export_scan();
        }
        // The fixed generation is checked with the same rule the main history
        // window applies; a mismatch never splices two prefixes (spec §17.4).
        let (pin, total) = {
            let Some(scan) = self.export_scan.as_ref() else {
                return Vec::new();
            };
            if scan.stop {
                return Vec::new();
            }
            (scan.pin.clone(), scan.total)
        };
        if let Err(stale) = crate::app::history::validate_chain_pin(&pin, &total, &page) {
            if let Some(scan) = self.export_scan.as_mut() {
                scan.stop = true;
                scan.limitations.read_stopped = true;
            }
            self.note_export_stale(stale.to_string());
            return self.finish_export_scan();
        }
        if let Some(scan) = self.export_scan.as_mut() {
            if scan.pin.is_none() {
                scan.pin = Some(page.pin());
            }
            if scan.total.is_none() {
                scan.total = Some(page.total);
            }
        }
        // A raw item in progress consumes its chunks verbatim (no typed decode)
        // until it completes (spec §17.4).
        let raw = self.export_scan.as_mut().and_then(|scan| scan.raw.take());
        if let Some(mut stream) = raw {
            let mut done = false;
            let mut mismatch = false;
            let mut saw_chunk = false;
            let raw_index = stream.index;
            for chunk in page.items.iter().filter(|chunk| chunk.index == raw_index) {
                saw_chunk = true;
                match stream.push(chunk) {
                    Ok(true) => {
                        self.queue_export_message(ExportInbound::RawChunk(Box::new(chunk.clone())));
                        done = true;
                        break;
                    }
                    Ok(false) => {
                        self.queue_export_message(ExportInbound::RawChunk(Box::new(chunk.clone())));
                    }
                    Err(_) => {
                        mismatch = true;
                        break;
                    }
                }
            }
            let index = stream.index;
            let expected_next = if done {
                self.export_scan
                    .as_ref()
                    .and_then(|scan| scan.total)
                    .and_then(|total| {
                        (index.saturating_add(1) < total).then_some(crate::protocol::ReadCursor {
                            item: index.saturating_add(1),
                            offset: 0,
                        })
                    })
            } else {
                Some(crate::protocol::ReadCursor {
                    item: index,
                    offset: stream.next_offset,
                })
            };
            // A missing chunk or a cursor that does not describe the verified
            // prefix is an EOF/gap, not an empty continuation. Stop instead of
            // retrying the same cursor forever or skipping unverified bytes.
            if !saw_chunk || page.next_cursor != expected_next {
                mismatch = true;
            }
            if let Some(scan) = self.export_scan.as_mut() {
                if mismatch {
                    // The bytes were not verified as a complete item: mark the
                    // limitation and stop, never resume on unverified data.
                    scan.limitations.raw_mismatched += 1;
                    scan.limitations.read_stopped = true;
                    scan.stop = true;
                    scan.raw = None;
                } else if done {
                    scan.limitations.raw_items += 1;
                    scan.items += 1;
                    scan.raw = None;
                    scan.next = expected_next;
                    scan.terminal = scan.next.is_none();
                } else {
                    // The item continues: request its next chunks from the
                    // offset the verified prefix reached (spec §17.4).
                    scan.next = expected_next;
                    scan.raw = Some(stream);
                }
            }
            if mismatch {
                return self.finish_export_scan();
            }
            return self.pump_export();
        }
        let Some((mut page_state, requested_cursor)) = self.export_scan.as_mut().map(|scan| {
            let requested = scan.next_cursor();
            let page_state = scan.page.take().unwrap_or_else(|| {
                // A fresh page continues from the cursor this response was
                // requested with, so the validator's contiguity check starts at
                // the right item (a raw item leaves `scan.page` empty).
                ReadPage::new(requested, None, 0)
            });
            (page_state, requested)
        }) else {
            return Vec::new();
        };
        let _ = requested_cursor;
        let mut pin = pin;
        let mut total = total;
        let outcome =
            crate::app::history::advance_chain_page(&mut page_state, &mut pin, &mut total, &page);
        let Some(scan) = self.export_scan.as_mut() else {
            return Vec::new();
        };
        scan.pin = pin;
        scan.total = total;
        scan.next = outcome.next;
        scan.terminal = outcome.terminal;
        if outcome.records_truncated {
            scan.limitations.records_truncated = true;
        }
        if let Some(error) = outcome.stale {
            scan.stop = true;
            scan.limitations.read_stopped = true;
            self.note_export_stale(error.to_string());
            return self.finish_export_scan();
        }
        if let Some(error) = outcome.failed {
            scan.stop = true;
            scan.limitations.read_stopped = true;
            scan.limitations.read_failed += 1;
            self.note_export_stale(format!("an export page failed validation: {error}"));
            return self.finish_export_scan();
        }
        // Stage the page's items in exact order for the decode path.
        page_state.pending_encoded.clear();
        page_state.pending_page = None;
        for crate::app::history::ChainItem::Encoded(item) in outcome.items {
            page_state.pending_encoded.push_back(item);
        }
        let large = outcome.large;
        if large.is_none() {
            scan.page = Some(page_state);
        } else {
            scan.page = None;
        }
        if let Some(large) = large {
            self.handle_export_oversized(large.index, large.total_bytes);
        }
        self.pump_export()
    }

    /// One oversized item: the explicit raw-export entry streams its verified
    /// sanitized JSON chunks verbatim; the default path writes a bounded
    /// placeholder and records the limitation. Neither path typed-decodes it
    /// and the 8 MiB automatic ceiling is never raised (spec §6.2/§17.4).
    fn handle_export_oversized(&mut self, index: usize, total_bytes: usize) {
        // Either path restarts reading this item from its own start: the page
        // that surfaced it discarded its bytes.
        if self.export_spec.raw_oversized {
            self.queue_export_message(ExportInbound::RawStart { index, total_bytes });
            if let Some(scan) = self.export_scan.as_mut() {
                scan.raw = Some(crate::state::export::RawItemStream::start(
                    index,
                    total_bytes,
                ));
                scan.page = None;
                scan.next = Some(crate::protocol::ReadCursor {
                    item: index,
                    offset: 0,
                });
                scan.terminal = false;
            }
            return;
        }
        if let Some(scan) = self.export_scan.as_mut() {
            scan.limitations.oversized_items += 1;
        }
        self.queue_export_message(ExportInbound::Item(Box::new(ExportRecord {
            markdown: String::new(),
            oversized: Some(total_bytes),
        })));
        if let Some(scan) = self.export_scan.as_mut() {
            scan.items += 1;
            scan.next = Some(crate::protocol::ReadCursor {
                item: index.saturating_add(1),
                offset: 0,
            });
            scan.page = None;
            // The captured total is fixed: skipping an oversized item reaches
            // the end of this chain when it was the last captured item.
            scan.terminal = scan.total == Some(index.saturating_add(1));
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
        session_epoch: u64,
        export_id: u64,
        index: usize,
        fingerprint: u64,
        outcome: &crate::jobs::DecodeOutcome,
    ) -> Vec<AppCommand> {
        let owned = self.export_scan.as_ref().is_some_and(|scan| {
            scan.session_id == *session_id
                && scan.session_epoch == session_epoch
                && scan.export_id == export_id
        });
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
            self.finish_export_scan()
        } else {
            self.request_export_page()
        }
    }

    /// Ends the read chain: the live turn is appended only when the user chose
    /// it, then the writer receives its trailing limitation notes and commits
    /// the file.
    fn finish_export_scan(&mut self) -> Vec<AppCommand> {
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
        self.pump_export()
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

    /// The owned job's channel closed without a completion. The App still owns
    /// the writer slot, so this is reported as an unknown state, never as a
    /// local rollback (spec §17.4).
    fn note_export_writer_gone(&mut self) {
        self.export_outbox.clear();
        self.export_hold = false;
        self.export_scan = None;
        if let Some(form) = self.export_form_mut() {
            form.phase = crate::state::export::ExportPhase::Failed;
            form.notice = Some(
                "the export writer stopped without reporting an outcome; the target state is \
                 unknown"
                    .to_owned(),
            );
            form.completion = Some(crate::state::export::ExportCompletion::Failed {
                target: form.target.clone(),
                error: "the export writer stopped without reporting an outcome".to_owned(),
                temp_removed: false,
                target_unknown: true,
            });
        }
    }

    /// A read-chain validation or pin failure. The chain stops, but the typed
    /// job outcome is still what reports committed/cancelled/unknown.
    fn note_export_stale(&mut self, detail: String) {
        self.notice(
            NoticeLevel::Warning,
            format!("export read chain stopped: {detail}"),
        );
    }

    /// Applies the owned job's typed completion. `capture` is the identity the
    /// App recorded at start; a completion for another export is dropped so a
    /// stale result can never decorate a newer form (spec §17.4).
    pub(super) fn on_export_job_finished(
        &mut self,
        capture: crate::jobs::ExportCapture,
        outcome: ExportOutcome,
    ) -> Vec<AppCommand> {
        let owns_completion = self.export_capture.as_ref() == Some(&capture);
        if !owns_completion {
            return Vec::new();
        }
        let view_is_current = self
            .sessions
            .active
            .as_deref()
            .is_some_and(|active| active == capture.session_id)
            && self
                .sessions
                .known
                .get(&capture.session_id)
                .is_some_and(|view| view.session_epoch == capture.session_epoch);
        // Every completion ends this export, even when the session was
        // reopened while the writer was running. A stale completion releases
        // the owner but never decorates the reopened session's form.
        self.export_scan = None;
        self.export_outbox.clear();
        self.export_hold = false;
        self.export_tx = None;
        self.export_cancel = None;
        self.export_capture = None;
        if !view_is_current {
            return Vec::new();
        }
        let partial = self
            .export_form()
            .map(|form| form.limitations.is_partial())
            .unwrap_or(false);
        // The typed outcome is the only authority for committed vs cancelled
        // vs unknown. It is reported on the form when it is open and through a
        // notice otherwise.
        let summary = export_outcome_summary(&outcome, partial);
        if let Some(form) = self.export_form_mut() {
            match &outcome {
                ExportOutcome::Finished {
                    target,
                    bytes,
                    items,
                } => {
                    form.phase = crate::state::export::ExportPhase::Done;
                    form.items = *items;
                    form.bytes = *bytes;
                    form.completion = Some(crate::state::export::ExportCompletion::Committed {
                        target: target.clone(),
                        bytes: *bytes,
                        items: *items,
                    });
                }
                ExportOutcome::WouldOverwrite { target } => {
                    form.phase = crate::state::export::ExportPhase::Editing;
                    form.completion = Some(crate::state::export::ExportCompletion::TargetExists {
                        target: target.clone(),
                    });
                }
                ExportOutcome::Cancelled { target } => {
                    form.phase = crate::state::export::ExportPhase::Failed;
                    form.completion = Some(crate::state::export::ExportCompletion::Cancelled {
                        target: target.clone(),
                    });
                }
                ExportOutcome::Failed {
                    target,
                    error,
                    temp_removed,
                    target_state_unknown,
                } => {
                    form.phase = crate::state::export::ExportPhase::Failed;
                    form.completion = Some(crate::state::export::ExportCompletion::Failed {
                        target: target.clone(),
                        error: error.clone(),
                        temp_removed: *temp_removed,
                        target_unknown: *target_state_unknown,
                    });
                }
            }
            form.notice = Some(summary.clone());
        }
        let level = match &outcome {
            ExportOutcome::Finished { .. } => NoticeLevel::Info,
            _ => NoticeLevel::Warning,
        };
        self.notice(level, summary);
        Vec::new()
    }
}

/// One human-readable line for a typed export outcome. The committed and
/// cancelled cases are never conflated; an unknown target state says so.
fn export_outcome_summary(outcome: &ExportOutcome, partial: bool) -> String {
    match outcome {
        ExportOutcome::Finished {
            target,
            bytes,
            items,
        } => format!(
            "exported {items} item(s), {bytes} bytes to {target}{}",
            if partial {
                " (partial: the file records the limitations)"
            } else {
                ""
            }
        ),
        ExportOutcome::WouldOverwrite { target } => {
            format!("{target} already exists — Ctrl+Y marks the export as overwriting it")
        }
        ExportOutcome::Cancelled { target } => {
            format!("export cancelled; nothing was written to {target}")
        }
        ExportOutcome::Failed {
            target,
            error,
            temp_removed,
            target_state_unknown,
        } => match (temp_removed, target_state_unknown) {
            (_, true) => format!("export failed: {error}; the state of {target} is unknown"),
            (true, false) => format!("export failed: {error} (the temporary file was removed)"),
            (false, false) => format!(
                "export failed: {error}; the temporary file next to {target} could not be \
                 confirmed removed"
            ),
        },
    }
}
