//! Owned local side-effect jobs (spec §5.5).
//!
//! Every local effect has exactly one owner here: the native clipboard today,
//! and the explicit export/draft-editor commands when they land. A job runs on
//! a blocking thread (the adapter itself enforces a deadline and a kill+wait
//! on its child), and its *only* way back into the app is an
//! [`AppEvent::JobFinished`] result carrying the capture identity. The main
//! loop remains the single place that mutates app state.
//!
//! No blocking call happens in `App::update`, in draw, or in the RPC command
//! dispatch: `run_commands` starts a job and returns.

use std::collections::BTreeMap;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::clipboard::ClipboardPort;
use crate::event::{AppEvent, JobOutcome};

/// Owner-local identifier for one started job. It is only used to drop
/// finished handles; results are identified by their capture identity.
pub type JobId = u64;

/// Owns every local job for this process.
pub struct LocalJobs {
    next_id: JobId,
    in_flight: BTreeMap<JobId, JoinHandle<()>>,
    events_tx: mpsc::UnboundedSender<AppEvent>,
    events_rx: mpsc::UnboundedReceiver<AppEvent>,
}

impl Default for LocalJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalJobs {
    pub fn new() -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            next_id: 0,
            in_flight: BTreeMap::new(),
            events_tx,
            events_rx,
        }
    }

    /// The completion stream the main loop selects on. Only the main loop
    /// polls it, so job results flow through `App::update` like every other
    /// event.
    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<AppEvent> {
        &mut self.events_rx
    }

    /// Starts one owned clipboard write against the platform adapter. The job
    /// owns its blocking thread and its child; a hung helper is killed by the
    /// adapter's own deadline and then waited, so this cannot leave a
    /// detached process behind.
    pub fn copy_to_clipboard(&mut self, session_id: &str, revision: u64, text: String) -> JobId {
        let mut clipboard = crate::clipboard::terminal_clipboard();
        self.spawn_clipboard_job(session_id, revision, text, move |captured| {
            clipboard.set_text(captured)
        })
    }

    /// Test seam: the same owner/deadline contract with an injected adapter.
    pub fn copy_with<P>(
        &mut self,
        session_id: &str,
        revision: u64,
        text: String,
        adapter: P,
    ) -> JobId
    where
        P: ClipboardPort + Send + 'static,
    {
        self.spawn_clipboard_job(session_id, revision, text, move |captured| {
            let mut adapter = adapter;
            adapter.set_text(captured)
        })
    }

    fn spawn_clipboard_job<F>(
        &mut self,
        session_id: &str,
        revision: u64,
        text: String,
        write: F,
    ) -> JobId
    where
        F: FnOnce(&str) -> std::io::Result<()> + Send + 'static,
    {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let events = self.events_tx.clone();
        let session_id = session_id.to_owned();
        let handle = tokio::task::spawn_blocking(move || {
            let result = write(&text).map_err(|error| format!("copy failed: {error}"));
            let _ = events.send(AppEvent::JobFinished(JobOutcome::Clipboard {
                session_id,
                revision,
                result,
            }));
        });
        self.in_flight.insert(id, handle);
        id
    }

    /// Joins every job that already finished and drops its handle, keeping the
    /// ownership map bounded. Unfinished jobs stay owned.
    pub async fn reap_finished(&mut self) {
        let finished: Vec<JobId> = self
            .in_flight
            .iter()
            .filter(|(_, handle)| handle.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in finished {
            if let Some(handle) = self.in_flight.remove(&id) {
                let _ = handle.await;
            }
        }
    }

    /// Whether any job is still in flight.
    pub fn has_in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Joins every owned job. Each worker is itself deadline-bounded, so this
    /// waits for local cleanup but never for a hung helper indefinitely.
    pub async fn shutdown(&mut self) {
        let jobs = std::mem::take(&mut self.in_flight);
        for (_, handle) in jobs {
            let _ = handle.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MockClipboard;

    #[tokio::test]
    async fn a_clipboard_job_reports_its_capture_identity() {
        let mut jobs = LocalJobs::new();
        jobs.copy_with("ses_1", 7, "selected".to_owned(), MockClipboard::default());
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

    #[tokio::test]
    async fn shutdown_joins_every_owned_job() {
        let mut jobs = LocalJobs::new();
        for index in 0..4u64 {
            jobs.copy_with(
                "ses_1",
                index,
                format!("selection {index}"),
                MockClipboard::default(),
            );
        }
        assert!(jobs.has_in_flight());
        jobs.shutdown().await;
        assert!(!jobs.has_in_flight());
    }
}
