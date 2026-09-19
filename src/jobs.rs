//! Owned local side-effect jobs (spec §5.5).
//!
//! Every local effect has exactly one owner here: the native clipboard, the
//! single serialized durable-layout worker, and the single serialized
//! canonical-item decode worker. There is at most **one** clipboard job, one
//! layout build, and one decode item in flight; their request queues and the
//! completion backlog are bounded by construction. A
//! second copy while one is running is refused with [`CopyAdmission::Busy`]
//! instead of spawning another blocking thread; the refused text is dropped
//! and never overwrites a newer selection later.
//!
//! Jobs run as owned tasks and their *only* way back into the app is a bounded
//! `AppEvent` completion channel; the main loop remains the single place that
//! mutates app state.
//!
//! No blocking call happens in `App::update`, in draw, or in the RPC command
//! dispatch: `run_commands` starts a job and returns. The clipboard adapter
//! bounds the whole write+wait with one shared deadline, owns the child's
//! stdin, and kills and waits the direct child (`src/clipboard.rs`). Dropping
//! the task closes this process's pipe end, so no writer thread can outlive
//! its owner and `shutdown` never detaches one.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::clipboard::ClipboardPort;
use crate::event::{AppEvent, JobOutcome};
use crate::protocol::TurnRef;
use crate::protocol::read::{EncodedHistoryItem, RawHistoryItem};

/// Exact identity carried through one serialized history-item decode. A stale
/// result may release the worker slot but can never install into a newer
/// session epoch or read chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeTarget {
    History {
        session_id: String,
        index: usize,
    },
    TurnResult {
        turn: TurnRef,
        index: usize,
    },
    /// One item of an explicit full-session search scan: the worker decodes
    /// and scans it, then drops the body (spec §17.1).
    SearchScan {
        session_id: String,
        generation: u64,
        index: usize,
    },
    /// One item of an explicit export: the worker decodes it and renders the
    /// bounded Markdown for the owned writer, then drops the body (spec
    /// §17.4). The App never walks a history body.
    ExportItem {
        session_id: String,
        export_id: u64,
        index: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeIdentity {
    pub session_epoch: u64,
    pub read_chain: u64,
    pub target: DecodeTarget,
}

#[derive(Clone, Debug)]
pub struct DecodeRequest {
    pub identity: DecodeIdentity,
    pub item: EncodedHistoryItem,
    pub fingerprint: u64,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    /// When set, the worker scans the decoded item for this literal and
    /// returns only bounded match summaries.
    pub scan: Option<Box<crate::state::search::ScanSpec>>,
    /// When set, the worker renders this item's export Markdown (spec §17.4)
    /// and returns only that bounded text.
    pub export: Option<Box<crate::state::export::ExportSpec>>,
}

/// The bounded result of scanning one decoded item. The item body is not
/// returned to the App.
#[derive(Debug)]
pub struct ScanItemOutcome {
    pub index: usize,
    pub matches: Vec<crate::state::search::SearchMatch>,
}

/// One rendered export item. It is produced on the decode worker's blocking
/// thread; the item body itself never returns to the App.
#[derive(Debug)]
pub struct ExportItemOutcome {
    pub index: usize,
    pub markdown: String,
    pub opaque_parts: usize,
}

#[derive(Debug)]
pub struct DecodeOutcome {
    pub identity: DecodeIdentity,
    pub fingerprint: u64,
    pub result: Result<RawHistoryItem, String>,
    pub cancelled: bool,
    /// Present for a `DecodeTarget::SearchScan` request; `result` then holds
    /// the decoded item only so the worker's decode status stays uniform.
    pub scan: Option<Box<ScanItemOutcome>>,
    /// Present for a `DecodeTarget::ExportItem` request.
    pub export: Option<Box<ExportItemOutcome>>,
}

/// Identity of one loaded-content search scan. A late result whose identity or
/// generation no longer matches the open panel is dropped (spec §17.1).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalScanIdentity {
    pub session_id: String,
    pub session_epoch: u64,
    pub generation: u64,
}

/// One live-loop text piece captured for a loaded-content scan. Live text is
/// bounded by the running turn, so it is copied here; durable history is
/// shared by `Arc` and never copied.
#[derive(Clone, Debug)]
pub struct LiveScanText {
    pub source: crate::state::search::SearchSource,
    pub index: Option<usize>,
    pub loop_id: Option<String>,
    pub request_index: Option<u32>,
    pub ordinal: u32,
    pub tool_call_id: Option<String>,
    pub text: String,
}

/// One loaded-content literal scan. It runs in an owned worker: the App never
/// walks a large body on its update thread (spec §17.1). The Debug impl is
/// length-only: a scan request must never print the bodies it walks.
pub struct LocalScanRequest {
    pub identity: LocalScanIdentity,
    pub needle: String,
    pub include_thinking: bool,
    pub blocks: Arc<Vec<Arc<crate::state::transcript::TranscriptBlock>>>,
    pub live: Vec<LiveScanText>,
}

impl std::fmt::Debug for LocalScanRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalScanRequest")
            .field("session_id", &self.identity.session_id)
            .field("generation", &self.identity.generation)
            .field("needle_bytes", &self.needle.len())
            .field("blocks", &self.blocks.len())
            .field("live", &self.live.len())
            .finish()
    }
}

#[derive(Debug)]
pub struct LocalScanOutcome {
    pub identity: LocalScanIdentity,
    pub matches: Vec<crate::state::search::SearchMatch>,
    pub truncated: bool,
}

/// One message for the owned export writer. The channel is bounded: when the
/// disk is slower than the read→decode chain, `try_send` reports `Full` and
/// the App pauses its paging instead of buffering the conversation.
pub enum ExportInbound {
    /// The file header: where the content came from and the choices made.
    Header(Box<ExportHeader>),
    /// One rendered item, or an oversized placeholder (`oversized` bytes).
    Item(Box<ExportRecord>),
    /// Begin a raw oversized item: its verified canonical chunks follow. The
    /// writer never typed-decodes these bytes.
    RawStart { index: usize, total_bytes: usize },
    /// One verbatim chunk of the current raw item. The writer re-verifies the
    /// encoding/offset/`total_bytes`/`complete` agreement itself.
    RawChunk(Box<crate::protocol::read::ReadChunk>),
    /// The read chain reached its end: write the trailing limitation notes and
    /// commit the file with an atomic no-clobber move.
    Finish(Box<crate::state::export::ExportLimitations>),
    /// The user cancelled: remove the uncommitted temp file.
    Abort,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExportHeader {
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExportRecord {
    pub markdown: String,
    /// `Some(total_bytes)` marks a placeholder for an oversized item.
    pub oversized: Option<usize>,
}

impl std::fmt::Debug for ExportInbound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header(header) => formatter
                .debug_struct("Header")
                .field("notes", &header.notes.len())
                .finish(),
            Self::Item(record) => formatter
                .debug_struct("Item")
                .field("markdown_bytes", &record.markdown.len())
                .field("oversized", &record.oversized)
                .finish(),
            Self::RawStart { index, total_bytes } => formatter
                .debug_struct("RawStart")
                .field("index", index)
                .field("total_bytes", total_bytes)
                .finish(),
            Self::RawChunk(chunk) => formatter
                .debug_struct("RawChunk")
                .field("index", &chunk.index)
                .field("offset", &chunk.offset)
                .field("data_bytes", &chunk.data.len())
                .field("complete", &chunk.complete)
                .finish(),
            Self::Finish(limitations) => {
                formatter.debug_tuple("Finish").field(limitations).finish()
            }
            Self::Abort => formatter.write_str("Abort"),
        }
    }
}

/// Identifies the one export job whose result is being reported. The App
/// records the same capture when it starts the export, so a completion for an
/// older target/session/epoch can never decorate a newer export (spec §17.4).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExportCapture {
    pub export_id: u64,
    pub session_id: String,
    pub session_epoch: u64,
}

/// The one owned writer slot is still occupied by an earlier export whose
/// typed completion has not been drained. The caller keeps the newer request
/// unstarted instead of overwriting the owner handle (spec §17.4).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ExportBusyError;

/// The final state of one owned export job.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExportOutcome {
    /// The temp file was moved onto the target (a no-clobber commit or an
    /// explicitly confirmed replace). A late cancel cannot undo this.
    Finished {
        target: String,
        bytes: usize,
        items: usize,
    },
    /// The target exists and was created after the export began, so the
    /// no-clobber commit refused to replace it. Nothing was written.
    WouldOverwrite { target: String },
    /// The job was cancelled before committing; the temp file was removed.
    Cancelled { target: String },
    /// The export stopped with an error. `temp_removed` says whether deleting
    /// the uncommitted temp file was confirmed. `target_state_unknown` is set
    /// when a commit attempt failed without proving the target was untouched:
    /// the UI must not claim a rollback.
    Failed {
        target: String,
        error: String,
        temp_removed: bool,
        target_state_unknown: bool,
    },
}

/// Owner-local identifier for one started job. It is only used for tests and
/// diagnostics; results are identified by their capture identity.
pub type JobId = u64;

/// The outcome of asking for a clipboard write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyAdmission {
    /// The one clipboard job started. Its result arrives as
    /// `AppEvent::JobFinished(JobOutcome::Clipboard { .. })`.
    Started(JobId),
    /// A clipboard job is already running. The new capture was refused and
    /// its text was not retained (bounded memory, no late overwrite).
    Busy,
}

/// The completion channel is bounded for the one clipboard owner and the one
/// layout owner; four slots cover one current result plus stale transitions
/// without allowing an unbounded completion backlog.
const JOB_EVENTS_CAPACITY: usize = 4;

/// Owns every local job for this process: one clipboard write and one
/// serialized durable-layout worker.
pub struct LocalJobs {
    next_id: JobId,
    clipboard: Option<JoinHandle<()>>,
    events_tx: mpsc::Sender<AppEvent>,
    events_rx: mpsc::Receiver<AppEvent>,
    layout_tx: Option<mpsc::Sender<crate::ui::transcript::DurableLayoutRequest>>,
    layout_task: Option<JoinHandle<()>>,
    layout_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    decode_tx: Option<mpsc::Sender<DecodeRequest>>,
    decode_task: Option<JoinHandle<()>>,
    decode_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    scan_tx: Option<mpsc::Sender<LocalScanRequest>>,
    scan_task: Option<JoinHandle<()>>,
    export_task: Option<JoinHandle<()>>,
    /// Set by `shutdown` (and by a stray writer loss) to wake a writer blocked
    /// on its channel even while the App still holds its sender.
    export_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for LocalJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalJobs {
    pub fn new() -> Self {
        let (events_tx, events_rx) = mpsc::channel(JOB_EVENTS_CAPACITY);
        let (layout_tx, mut layout_rx) =
            mpsc::channel::<crate::ui::transcript::DurableLayoutRequest>(1);
        let layout_events = events_tx.clone();
        let layout_task = tokio::spawn(async move {
            while let Some(request) = layout_rx.recv().await {
                let mut request = request;
                while let Ok(newer) = layout_rx.try_recv() {
                    request = newer;
                }
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let identity = request.identity.clone();
                let theme_kind = request.identity.theme;
                let snapshot = request.snapshot;
                let tools_expanded = snapshot.tools_expanded;
                let previous = request.previous;
                let cancel = Arc::clone(&request.cancel);
                let cancel_for_build = Arc::clone(&cancel);
                let build_identity = identity.clone();
                let batch_events = layout_events.clone();
                let batch_identity = identity.clone();
                let batch_key = crate::state::view::DurableCacheKey {
                    revision: identity.transcript_revision,
                    width: identity.width,
                    theme: identity.theme,
                    reasoning_visible: identity.reasoning_visible,
                    tools_expanded,
                };
                let batch_cancel = Arc::clone(&cancel);
                let Ok(Some((layout, changed_sections, tool_index_lookups))) =
                    tokio::task::spawn_blocking(move || {
                        let theme = theme_kind.theme();
                        let mut sink = |sections: Vec<Arc<crate::state::view::SectionLayout>>| {
                            if sections.is_empty() || batch_cancel.load(Ordering::Relaxed) {
                                return !batch_cancel.load(Ordering::Relaxed);
                            }
                            let durable = Arc::new(crate::state::view::PreparedDurable {
                                key: batch_key.clone(),
                                layout: Arc::new(
                                    crate::state::view::ConversationLayout::from_sections(sections),
                                ),
                            });
                            batch_events
                                .blocking_send(AppEvent::DurableLayoutPrepared(
                                    crate::ui::transcript::DurableLayoutResult {
                                        identity: batch_identity.clone(),
                                        durable,
                                        changed_sections: 0,
                                        tool_index_lookups: 0,
                                        complete: false,
                                    },
                                ))
                                .is_ok()
                        };
                        crate::ui::transcript::build_durable_layout(
                            &theme,
                            theme_kind,
                            &snapshot,
                            build_identity.width,
                            build_identity.reasoning_visible,
                            previous.as_deref(),
                            request.viewport.clone(),
                            Some(&cancel_for_build),
                            Some(&mut sink),
                        )
                    })
                    .await
                else {
                    continue;
                };
                if cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let durable = Arc::new(crate::state::view::PreparedDurable {
                    key: crate::state::view::DurableCacheKey {
                        revision: identity.transcript_revision,
                        width: identity.width,
                        theme: identity.theme,
                        reasoning_visible: identity.reasoning_visible,
                        tools_expanded,
                    },
                    layout,
                });
                if layout_events
                    .send(AppEvent::DurableLayoutPrepared(
                        crate::ui::transcript::DurableLayoutResult {
                            identity,
                            durable,
                            changed_sections,
                            tool_index_lookups,
                            complete: true,
                        },
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let (decode_tx, mut decode_rx) = mpsc::channel::<DecodeRequest>(1);
        let decode_events = events_tx.clone();
        let decode_task = tokio::spawn(async move {
            while let Some(request) = decode_rx.recv().await {
                let identity = request.identity.clone();
                let fingerprint = request.fingerprint;
                let item = request.item;
                let scan = request.scan.map(|scan| *scan);
                let export = request.export.map(|spec| *spec);
                let cancel = Arc::clone(&request.cancel);
                if cancel.load(Ordering::Relaxed) {
                    let _ = decode_events
                        .send(AppEvent::HistoryItemDecoded(Box::new(DecodeOutcome {
                            identity,
                            fingerprint,
                            result: Err("history decode cancelled".to_owned()),
                            cancelled: true,
                            scan: None,
                            export: None,
                        })))
                        .await;
                    continue;
                }
                let cancel_for_decode = Arc::clone(&cancel);
                let (result, scan_outcome, export_outcome) =
                    tokio::task::spawn_blocking(move || {
                        if cancel_for_decode.load(Ordering::Relaxed) {
                            return (Err("history decode cancelled".to_owned()), None, None);
                        }
                        let decoded = crate::protocol::read::decode_item(&item.data);
                        let (scan_outcome, export_outcome) = match &decoded {
                            Ok(decoded) => {
                                let scan_outcome = scan.map(|scan| {
                                    let mut plan = crate::state::search::ScanPlan::new(
                                        &scan.needle,
                                        scan.include_thinking,
                                    );
                                    plan.scan_item(item.index, decoded);
                                    Box::new(ScanItemOutcome {
                                        index: item.index,
                                        matches: plan.collector.matches,
                                    })
                                });
                                let export_outcome = export.map(|spec| {
                                    let rendered =
                                        crate::state::export::item_markdown(decoded, spec);
                                    Box::new(ExportItemOutcome {
                                        index: item.index,
                                        markdown: rendered.markdown,
                                        opaque_parts: rendered.opaque_parts,
                                    })
                                });
                                (scan_outcome, export_outcome)
                            }
                            Err(_) => (None, None),
                        };
                        (decoded, scan_outcome, export_outcome)
                    })
                    .await
                    .unwrap_or_else(|error| {
                        (
                            Err(format!("history decode worker failed: {error}")),
                            None,
                            None,
                        )
                    });
                let cancelled = cancel.load(Ordering::Relaxed);
                if decode_events
                    .send(AppEvent::HistoryItemDecoded(Box::new(DecodeOutcome {
                        identity,
                        fingerprint,
                        result: if cancelled {
                            Err("history decode cancelled".to_owned())
                        } else {
                            result
                        },
                        cancelled,
                        scan: scan_outcome.filter(|_| !cancelled),
                        export: export_outcome.filter(|_| !cancelled),
                    })))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let (scan_tx, mut scan_rx) = mpsc::channel::<LocalScanRequest>(1);
        let scan_events = events_tx.clone();
        let scan_task = tokio::spawn(async move {
            while let Some(request) = scan_rx.recv().await {
                // The literal scan runs on a blocking thread: a large loaded
                // body must never stall the async runtime or the UI.
                let result = tokio::task::spawn_blocking(move || {
                    crate::state::search::run_local_scan(&request)
                })
                .await;
                let Ok(outcome) = result else {
                    continue;
                };
                if scan_events
                    .send(AppEvent::LocalScanFinished(Box::new(outcome)))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            next_id: 0,
            clipboard: None,
            events_tx,
            events_rx,
            layout_tx: Some(layout_tx),
            layout_task: Some(layout_task),
            layout_cancel: None,
            decode_tx: Some(decode_tx),
            decode_task: Some(decode_task),
            decode_cancel: None,
            scan_tx: Some(scan_tx),
            scan_task: Some(scan_task),
            export_task: None,
            export_cancel: None,
        }
    }

    /// Schedules the only production layout worker. The queue has one slot;
    /// a newer resize/generation replaces an idle queued request, while an
    /// active request is cooperatively cancelled through its token.
    pub fn try_schedule_layout(
        &mut self,
        request: crate::ui::transcript::DurableLayoutRequest,
    ) -> bool {
        let cancel = Arc::clone(&request.cancel);
        let Some(sender) = self.layout_tx.as_ref() else {
            return false;
        };
        match sender.try_send(request) {
            Ok(()) => {
                if let Some(previous) = self.layout_cancel.replace(cancel) {
                    previous.store(true, Ordering::Relaxed);
                }
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
                false
            }
        }
    }

    /// Schedules one bounded canonical-item decode. The App submits at most
    /// one item at a time; a newer lifecycle/read identity cancels the older
    /// queued or active request cooperatively.
    pub fn try_schedule_decode(&mut self, request: DecodeRequest) -> bool {
        let Some(sender) = self.decode_tx.as_ref() else {
            return false;
        };
        let cancel = Arc::clone(&request.cancel);
        match sender.try_send(request) {
            Ok(()) => {
                if let Some(previous) = self.decode_cancel.replace(cancel) {
                    previous.store(true, Ordering::Relaxed);
                }
                true
            }
            Err(mpsc::error::TrySendError::Full(_returned)) => {
                // The App retains its own pending owner when the single queue
                // is full, so no JSON is cloned or queued here.
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Schedules one loaded-content scan. The single queue slot drops a
    /// superseded queued scan; the generation check already refuses its
    /// result, so no state can be overwritten by an old generation.
    pub fn try_schedule_scan(&mut self, request: LocalScanRequest) -> bool {
        let Some(sender) = self.scan_tx.as_ref() else {
            return false;
        };
        match sender.try_send(request) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
                false
            }
        }
    }

    /// The completion stream the main loop selects on. Only the main loop
    /// polls it, so job results flow through `App::update` like every other
    /// event.
    pub fn events(&mut self) -> &mut mpsc::Receiver<AppEvent> {
        &mut self.events_rx
    }

    /// Starts one owned clipboard write against the platform adapter, unless a
    /// clipboard job is already running.
    pub fn copy_to_clipboard(
        &mut self,
        session_id: &str,
        revision: u64,
        text: String,
    ) -> CopyAdmission {
        let mut clipboard = crate::clipboard::terminal_clipboard();
        self.try_start_clipboard_job(session_id, revision, text, move |captured| async move {
            clipboard.set_text(&captured).await
        })
    }

    /// Test seam: the same single-owner contract with an injected adapter.
    pub fn copy_with<P>(
        &mut self,
        session_id: &str,
        revision: u64,
        text: String,
        adapter: P,
    ) -> CopyAdmission
    where
        P: ClipboardPort + Send + 'static,
    {
        self.try_start_clipboard_job(session_id, revision, text, move |captured| async move {
            let mut adapter = adapter;
            adapter.set_text(&captured).await
        })
    }

    fn try_start_clipboard_job<F, Fut>(
        &mut self,
        session_id: &str,
        revision: u64,
        text: String,
        write: F,
    ) -> CopyAdmission
    where
        F: FnOnce(String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = std::io::Result<()>> + Send,
    {
        if self
            .clipboard
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            // Drop the refused text here: no second thread, no queued payload.
            return CopyAdmission::Busy;
        }
        // The previous task (if any) already finished; dropping its handle is
        // enough, and this keeps exactly one owned handle at a time.
        self.clipboard = None;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let events = self.events_tx.clone();
        let session_id = session_id.to_owned();
        // One owned async task. Cancelling it drops the adapter future, which
        // drops the child's stdin and its `kill_on_drop` child: the production
        // path never leaves a writer thread behind.
        let handle = tokio::spawn(async move {
            let result = write(text)
                .await
                .map_err(|error| format!("copy failed: {error}"));
            // The channel has room for this single result (two slots, one job),
            // so no send path waits on the main loop.
            let _ = events
                .send(AppEvent::JobFinished(JobOutcome::Clipboard {
                    session_id,
                    revision,
                    result,
                }))
                .await;
        });
        self.clipboard = Some(handle);
        CopyAdmission::Started(id)
    }

    /// Starts the one owned export job. It owns the target path, the temp
    /// file and every byte of file I/O; the returned receiver is held by the
    /// App so the bounded channel provides backpressure for paging.
    ///
    /// The single writer slot is never overwritten: while a previous export is
    /// still running (or its completion has not been drained) this returns
    /// `Err(ExportBusy)` and the caller keeps the request unstarted, so no
    /// owner handle is orphaned and no second writer races the same target.
    pub fn start_export(
        &mut self,
        capture: ExportCapture,
        target: std::path::PathBuf,
        overwrite: bool,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        rx: mpsc::Receiver<ExportInbound>,
    ) -> Result<JobId, ExportBusyError> {
        if self.export_task.is_some() {
            return Err(ExportBusyError);
        }
        self.export_cancel = Some(Arc::clone(&cancel));
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let events = self.events_tx.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let outcome = run_export_job(&target, overwrite, rx, cancel);
            let _ = events.blocking_send(AppEvent::JobFinished(JobOutcome::Export {
                capture,
                outcome,
            }));
        });
        self.export_task = Some(handle);
        Ok(id)
    }

    /// Whether an export job handle is still owned (running or not yet
    /// reaped). The App uses this to refuse a second start.
    pub fn has_export_in_flight(&self) -> bool {
        self.export_task.is_some()
    }

    /// Signals the owned export writer to abort now. It does not wait for the
    /// writer to finish; `shutdown` joins the handle right after.
    fn request_export_abort(&mut self) {
        if let Some(cancel) = self.export_cancel.as_ref() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether the owned export writer slot is currently free. `true` means a
    /// new export may be started (spec §17.4).
    pub fn export_slot_free(&self) -> bool {
        self.export_task.is_none()
    }

    /// Joins the clipboard job if it already finished and drops its handle.
    /// Never waits for a running job, so it is safe on the input path.
    pub async fn reap_finished(&mut self) {
        if self
            .clipboard
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            if let Some(handle) = self.clipboard.take() {
                let _ = handle.await;
            }
        }
        if self
            .export_task
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            if let Some(handle) = self.export_task.take() {
                let _ = handle.await;
            }
        }
    }

    /// Whether the clipboard job is still in flight.
    pub fn has_in_flight(&self) -> bool {
        self.clipboard
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    /// Joins the owned job. The adapter is deadline-bounded and owns its
    /// child, so this waits for local cleanup but never for a hung helper
    /// indefinitely.
    ///
    /// While joining, completion results are drained so a bounded channel can
    /// never wedge either owned worker during shutdown.
    pub async fn shutdown(&mut self) {
        if let Some(cancel) = self.layout_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.layout_tx.take();
        if let Some(task) = self.layout_task.take() {
            while !task.is_finished() {
                while self.events_rx.try_recv().is_ok() {}
                tokio::task::yield_now().await;
            }
            let _ = task.await;
        }
        if let Some(cancel) = self.decode_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.decode_tx.take();
        if let Some(task) = self.decode_task.take() {
            while !task.is_finished() {
                while self.events_rx.try_recv().is_ok() {}
                tokio::task::yield_now().await;
            }
            let _ = task.await;
        }
        self.scan_tx.take();
        if let Some(task) = self.scan_task.take() {
            while !task.is_finished() {
                while self.events_rx.try_recv().is_ok() {}
                tokio::task::yield_now().await;
            }
            let _ = task.await;
        }
        // The export job's bounded channel is closed first: a job still holding
        // an uncommitted temp file aborts and removes it before it returns. The
        // App is gone by now, so the channel closes on its own; the completion
        // send uses `blocking_send` on a bounded channel that shutdown drains.
        self.request_export_abort();
        self.export_cancel = None;
        if let Some(task) = self.export_task.take() {
            while !task.is_finished() {
                while self.events_rx.try_recv().is_ok() {}
                tokio::task::yield_now().await;
            }
            let _ = task.await;
        }
        let Some(handle) = self.clipboard.take() else {
            return;
        };
        let handle = handle;
        while !handle.is_finished() {
            while self.events_rx.try_recv().is_ok() {}
            tokio::task::yield_now().await;
        }
        let _ = handle.await;
    }
}

/// Waits for the next export message without ever blocking indefinitely: a
/// cancel token or a closed channel both end the wait, so `shutdown` and a
/// user cancel never depend on the bounded channel having room.
fn next_export_message(
    rx: &mut mpsc::Receiver<ExportInbound>,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Option<ExportInbound> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Some(ExportInbound::Abort);
        }
        match rx.try_recv() {
            Ok(message) => return Some(message),
            Err(mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(mpsc::error::TryRecvError::Disconnected) => return None,
        }
    }
}

/// The owned export writer loop. Everything here runs on a blocking thread:
/// creating the temp file, writing records, the trailing notes, the atomic
/// rename and the cancel cleanup. The bounded channel is the only input, so
/// the App's `try_send` can never block the UI.
pub fn run_export_job(
    target: &std::path::Path,
    overwrite: bool,
    mut rx: mpsc::Receiver<ExportInbound>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> ExportOutcome {
    use crate::state::export::{
        EXPORT_OVERSIZED_NOTE, ExportCommitError, ExportStartError, ExportWriter, RawItemStream,
        header_text, raw_item_end_text, raw_item_start_text,
    };
    let target_display = target.display().to_string();
    let mut writer = match ExportWriter::create(target, overwrite) {
        Ok(writer) => writer,
        Err(ExportStartError::TargetExists) => {
            return ExportOutcome::WouldOverwrite {
                target: target_display,
            };
        }
        Err(ExportStartError::Io(error)) => {
            return ExportOutcome::Failed {
                target: target_display,
                error,
                temp_removed: true,
                target_state_unknown: false,
            };
        }
    };
    let mut items = 0usize;
    let mut failure: Option<String> = None;
    let mut cancelled = false;
    // The one raw item currently being streamed, if any. It owns the byte
    // verification; the writer only sees chunks that already passed it.
    let mut raw: Option<RawItemStream> = None;
    loop {
        let Some(message) = next_export_message(&mut rx, &cancel) else {
            // The App dropped the sender (shutdown or cancel): the temp file is
            // uncommitted and must not survive.
            cancelled = true;
            break;
        };
        let write = match message {
            ExportInbound::Header(header) => writer.write(&header_text(&header.notes)),
            ExportInbound::Item(record) => {
                let text = match record.oversized {
                    Some(bytes) => format!("<!-- {EXPORT_OVERSIZED_NOTE} ({bytes} bytes) -->\n\n"),
                    None => record.markdown,
                };
                let result = writer.write(&text);
                if result.is_ok() {
                    items += 1;
                }
                result
            }
            ExportInbound::RawStart { index, total_bytes } => {
                raw = Some(RawItemStream::start(index, total_bytes));
                writer.write(&raw_item_start_text(index, total_bytes))
            }
            ExportInbound::RawChunk(chunk) => match raw.as_mut() {
                Some(stream) => {
                    let outcome = stream.push(&chunk);
                    match outcome {
                        Ok(_) => writer.write(&chunk.data),
                        Err(_) => {
                            // The mismatch is recorded in the stream and the
                            // file marks the item incomplete at its end; the
                            // bytes already written stay, but the file never
                            // claims a complete item.
                            Ok(())
                        }
                    }
                }
                None => Ok(()),
            },
            ExportInbound::Finish(limitations) => {
                // A raw item that never received its closing chunk is not
                // silently closed: mark it incomplete before the notes.
                if let Some(stream) = raw.take() {
                    let complete = stream.complete && !stream.mismatch;
                    if let Err(error) = writer.write(&raw_item_end_text(stream.index, complete)) {
                        failure = Some(format!("cannot write the export: {error}"));
                        break;
                    }
                }
                let notes = limitations.notes();
                let result = if notes.is_empty() {
                    Ok(())
                } else {
                    let text = crate::state::export::limitations_text(&notes);
                    writer.write(&text)
                };
                if let Err(error) = result {
                    failure = Some(format!("cannot write the export notes: {error}"));
                }
                break;
            }
            ExportInbound::Abort => {
                cancelled = true;
                break;
            }
        };
        if let Err(error) = write {
            failure = Some(format!("cannot write the export: {error}"));
            break;
        }
        // Close a raw item as soon as its last chunk verified. The App sends
        // the closing marker through `Finish`, so a stored raw stream is only
        // closed there or on an error.
        if raw.as_ref().is_some_and(|stream| stream.complete) {
            let stream = raw.take().expect("present");
            if let Err(error) = writer.write(&raw_item_end_text(stream.index, !stream.mismatch)) {
                failure = Some(format!("cannot write the export: {error}"));
                break;
            }
            items += 1;
        }
    }
    if let Some(error) = failure {
        let temp_removed = writer.abort().is_ok();
        return ExportOutcome::Failed {
            target: target_display,
            error,
            temp_removed,
            target_state_unknown: false,
        };
    }
    if cancelled {
        // An unconfirmed removal is reported as a failure, never as a clean
        // cancel: an unknown I/O state must not claim a rollback.
        return match writer.abort() {
            Ok(()) => ExportOutcome::Cancelled {
                target: target_display,
            },
            Err(error) => ExportOutcome::Failed {
                target: target_display,
                error,
                temp_removed: false,
                target_state_unknown: false,
            },
        };
    }
    let bytes = writer.bytes();
    match writer.finish() {
        Ok(_) => ExportOutcome::Finished {
            target: target_display,
            bytes,
            items,
        },
        Err(ExportCommitError::TargetExists) => ExportOutcome::WouldOverwrite {
            target: target_display,
        },
        Err(ExportCommitError::Io {
            error,
            temp_removed,
            target_state_unknown,
        }) => ExportOutcome::Failed {
            target: target_display,
            error,
            temp_removed,
            target_state_unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MockClipboard;
    use std::time::{Duration, Instant};

    /// A clipboard adapter that waits until its gate is released. It stands
    /// in for a slow native helper while the main loop keeps running.
    struct GatedClipboard {
        gate: tokio::sync::oneshot::Receiver<()>,
    }

    impl ClipboardPort for GatedClipboard {
        async fn set_text(&mut self, _text: &str) -> std::io::Result<()> {
            let _ = (&mut self.gate).await;
            Ok(())
        }
    }

    fn gated() -> (GatedClipboard, tokio::sync::oneshot::Sender<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (GatedClipboard { gate: rx }, tx)
    }

    #[tokio::test]
    async fn serialized_decode_worker_returns_exact_identity_and_item() {
        let mut jobs = LocalJobs::new();
        let identity = DecodeIdentity {
            session_epoch: 4,
            read_chain: 9,
            target: DecodeTarget::History {
                session_id: "ses_decode".to_owned(),
                index: 3,
            },
        };
        let data: Arc<str> = Arc::from(
            r#"{"item":{"type":"summary","data":{"content":"decoded"}},"timestamp":"2026-01-01T00:00:00Z"}"#,
        );
        let item = EncodedHistoryItem { index: 3, data };
        let fingerprint = 17;
        assert!(jobs.try_schedule_decode(DecodeRequest {
            identity: identity.clone(),
            item,
            fingerprint,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan: None,
            export: None,
        }));
        let event = jobs.events().recv().await.expect("decode completion");
        match event {
            AppEvent::HistoryItemDecoded(outcome) => {
                assert_eq!(outcome.identity, identity);
                assert_eq!(outcome.fingerprint, fingerprint);
                assert!(!outcome.cancelled);
                let item = outcome.result.expect("valid Runtime item");
                assert_eq!(item.timestamp.as_deref(), Some("2026-01-01T00:00:00Z"));
                assert!(matches!(
                    item.item,
                    crate::protocol::RuntimeItem::Summary(_)
                ));
            }
            other => panic!("unexpected worker event: {other:?}"),
        }
        jobs.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_decode_still_releases_its_identity() {
        let mut jobs = LocalJobs::new();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(jobs.try_schedule_decode(DecodeRequest {
            identity: DecodeIdentity {
                session_epoch: 1,
                read_chain: 2,
                target: DecodeTarget::History {
                    session_id: "ses_cancel".to_owned(),
                    index: 0,
                },
            },
            item: EncodedHistoryItem {
                index: 0,
                data: Arc::from("{}"),
            },
            fingerprint: 0,
            cancel,
            scan: None,
            export: None,
        }));
        let event = jobs.events().recv().await.expect("cancel completion");
        match event {
            AppEvent::HistoryItemDecoded(outcome) => assert!(outcome.cancelled),
            other => panic!("unexpected worker event: {other:?}"),
        }
        jobs.shutdown().await;
    }

    #[tokio::test]
    async fn one_copy_starts_and_reports_its_capture_identity() {
        let mut jobs = LocalJobs::new();
        assert_eq!(
            jobs.copy_with("ses_1", 7, "selected".to_owned(), MockClipboard::default()),
            CopyAdmission::Started(0)
        );
        let event = jobs
            .events()
            .recv()
            .await
            .expect("the job reports exactly one result");
        match event {
            AppEvent::JobFinished(JobOutcome::Clipboard {
                session_id,
                revision,
                result,
            }) => {
                assert_eq!(session_id, "ses_1");
                assert_eq!(revision, 7);
                assert!(result.is_ok());
            }
            other => panic!("unexpected job event: {other:?}"),
        }
        jobs.shutdown().await;
        assert!(!jobs.has_in_flight());
    }

    #[tokio::test]
    async fn a_failing_clipboard_job_reports_a_safe_error() {
        let mut jobs = LocalJobs::new();
        jobs.copy_with(
            "ses_1",
            0,
            "selected".to_owned(),
            MockClipboard {
                text: None,
                error: Some("headless".to_owned()),
            },
        );
        let event = jobs.events().recv().await.expect("failure is reported");
        match event {
            AppEvent::JobFinished(JobOutcome::Clipboard { result, .. }) => {
                assert_eq!(result.expect_err("failure"), "copy failed: headless");
            }
            other => panic!("unexpected job event: {other:?}"),
        }
        jobs.shutdown().await;
    }

    /// The parent-review case: while one copy is blocked, a second copy is
    /// refused immediately, the main loop can still poll its other event
    /// sources, and after the first finishes a later copy is admitted again.
    #[tokio::test]
    async fn a_second_copy_is_refused_while_the_first_is_running() {
        let mut jobs = LocalJobs::new();
        let (adapter, gate) = gated();
        assert_eq!(
            jobs.copy_with("ses_1", 1, "first".to_owned(), adapter),
            CopyAdmission::Started(0)
        );

        let started = Instant::now();
        assert_eq!(
            jobs.copy_with("ses_1", 2, "second".to_owned(), MockClipboard::default()),
            CopyAdmission::Busy,
            "the second copy is refused, its text is dropped"
        );
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "admission must not wait for the running job"
        );
        assert!(jobs.has_in_flight());
        // The main loop's other sources stay live: polling the completion
        // channel with nothing to report returns immediately instead of
        // waiting for the clipboard.
        assert!(matches!(
            jobs.events().try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        gate.send(()).expect("release the adapter");
        let event = jobs.events().recv().await.expect("first result");
        assert!(matches!(
            event,
            AppEvent::JobFinished(JobOutcome::Clipboard { revision: 1, .. })
        ));
        // The completion event is sent just before the worker returns, so
        // reap until the finished handle is actually observable.
        let deadline = Instant::now() + Duration::from_secs(2);
        while jobs.has_in_flight() && Instant::now() < deadline {
            jobs.reap_finished().await;
            tokio::task::yield_now().await;
        }
        assert!(!jobs.has_in_flight(), "the finished job is reaped");

        assert_eq!(
            jobs.copy_with("ses_1", 3, "third".to_owned(), MockClipboard::default()),
            CopyAdmission::Started(1),
            "a finished job frees the single slot"
        );
        jobs.shutdown().await;
    }

    /// A refused copy does not keep its text alive and cannot later overwrite
    /// the clipboard: the first (admitted) job is the only writer.
    #[tokio::test]
    async fn a_refused_copy_never_overwrites_the_admitted_one() {
        let mut jobs = LocalJobs::new();
        let (adapter, gate) = gated();
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&observed);
        struct Recording {
            sink: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }
        impl ClipboardPort for Recording {
            async fn set_text(&mut self, text: &str) -> std::io::Result<()> {
                self.sink.lock().unwrap().push(text.to_owned());
                Ok(())
            }
        }
        // Interleave: admit the gated writer, refuse a recording writer.
        assert!(matches!(
            jobs.copy_with(
                "ses_1",
                1,
                "admitted".to_owned(),
                GatedClipboard { gate: adapter.gate }
            ),
            CopyAdmission::Started(_)
        ));
        assert_eq!(
            jobs.copy_with("ses_1", 2, "refused".to_owned(), Recording { sink }),
            CopyAdmission::Busy
        );
        gate.send(()).unwrap();
        let _ = jobs.events().recv().await;
        jobs.shutdown().await;
        assert!(
            observed.lock().unwrap().is_empty(),
            "a refused copy never reaches the clipboard"
        );
    }

    /// Shutdown joins the single owned job and drains its result even when the
    /// app never consumed it, so a bounded channel cannot deadlock it.
    #[tokio::test]
    async fn shutdown_joins_a_hung_result_without_wedging() {
        let mut jobs = LocalJobs::new();
        jobs.copy_with("ses_1", 1, "unread".to_owned(), MockClipboard::default());
        // Do not poll `events()`: shutdown must still complete.
        let started = Instant::now();
        jobs.shutdown().await;
        assert!(!jobs.has_in_flight());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown is bounded by the adapter deadline"
        );
        // A second shutdown is a no-op.
        jobs.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_waits_for_a_slow_but_bounded_job() {
        struct Slow;
        impl ClipboardPort for Slow {
            async fn set_text(&mut self, _text: &str) -> std::io::Result<()> {
                tokio::time::sleep(Duration::from_millis(60)).await;
                Ok(())
            }
        }
        let mut jobs = LocalJobs::new();
        jobs.copy_with("ses_1", 1, "slow".to_owned(), Slow);
        let started = Instant::now();
        jobs.shutdown().await;
        assert!(!jobs.has_in_flight());
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "shutdown waits for the owned job, it does not detach it"
        );
    }
}
