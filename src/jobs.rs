//! Owned local side-effect jobs (spec §5.5).
//!
//! Every local effect has exactly one owner here: the native clipboard and
//! the single serialized durable-layout worker. There is at most **one**
//! clipboard job and one layout build in flight, so the thread count, retained
//! text bytes, request queue, and completion backlog are bounded by
//! construction. A
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
        Self {
            next_id: 0,
            clipboard: None,
            events_tx,
            events_rx,
            layout_tx: Some(layout_tx),
            layout_task: Some(layout_task),
            layout_cancel: None,
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
