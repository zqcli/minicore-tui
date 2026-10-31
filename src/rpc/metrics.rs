//! Acceptance-build counters at actual query and reader ownership boundaries.

use std::sync::{Arc, Mutex};

use serde::Serialize;

tokio::task_local! {
    static RECOVERY: ();
}

pub async fn recovery_scope<T>(work: impl Future<Output = T>) -> T {
    RECOVERY.scope((), work).await
}

pub fn in_recovery() -> bool {
    RECOVERY.try_with(|_| ()).is_ok()
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Counters {
    pub query_requests: u64,
    pub query_in_flight: usize,
    pub query_peak: usize,
    pub read_pages: u64,
    pub recovery_queries: u64,
    pub recovery_pages: u64,
    pub workspaces: usize,
    pub workspaces_peak: usize,
    pub workspace_reserved_bytes: usize,
    pub workspace_reserved_peak_bytes: usize,
    pub recovery_workspaces: usize,
    pub recovery_workspaces_peak: usize,
    pub recovery_workspace_reserved_bytes: usize,
    pub recovery_workspace_reserved_peak_bytes: usize,
}

#[derive(Debug, Default)]
pub struct Metrics(Mutex<Counters>);

impl Metrics {
    pub fn snapshot(&self) -> Counters {
        self.0.lock().unwrap().clone()
    }

    pub fn sent(&self, method: &str) {
        let mut c = self.0.lock().unwrap();
        c.query_requests = c
            .query_requests
            .checked_add(1)
            .expect("query counter exhausted");
        let page = matches!(
            method,
            crate::protocol::METHOD_SESSION_READ | crate::protocol::METHOD_TURN_RESULT
        );
        if page {
            c.read_pages = c.read_pages.checked_add(1).expect("page counter exhausted");
        }
        if in_recovery() {
            c.recovery_queries = c
                .recovery_queries
                .checked_add(1)
                .expect("recovery query counter exhausted");
            if page {
                c.recovery_pages = c
                    .recovery_pages
                    .checked_add(1)
                    .expect("recovery page counter exhausted");
            }
        }
    }

    pub fn query(self: &Arc<Self>) -> Query {
        let mut c = self.0.lock().unwrap();
        c.query_in_flight += 1;
        c.query_peak = c.query_peak.max(c.query_in_flight);
        Query(self.clone())
    }

    pub fn workspace(self: &Arc<Self>, bytes: usize) -> Workspace {
        let recovery = in_recovery();
        let mut c = self.0.lock().unwrap();
        c.workspaces += 1;
        c.workspaces_peak = c.workspaces_peak.max(c.workspaces);
        if recovery {
            c.recovery_workspaces += 1;
            c.recovery_workspaces_peak = c.recovery_workspaces_peak.max(c.recovery_workspaces);
        }
        drop(c);
        let mut workspace = Workspace {
            metrics: self.clone(),
            bytes: 0,
            recovery,
        };
        workspace.charge(bytes);
        workspace
    }
}

pub struct Query(Arc<Metrics>);

impl Drop for Query {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().query_in_flight -= 1;
    }
}

#[derive(Debug)]
pub struct Workspace {
    metrics: Arc<Metrics>,
    bytes: usize,
    recovery: bool,
}

impl Workspace {
    pub fn charge(&mut self, bytes: usize) {
        self.bytes += bytes;
        let mut c = self.metrics.0.lock().unwrap();
        c.workspace_reserved_bytes += bytes;
        c.workspace_reserved_peak_bytes = c
            .workspace_reserved_peak_bytes
            .max(c.workspace_reserved_bytes);
        if self.recovery {
            c.recovery_workspace_reserved_bytes += bytes;
            c.recovery_workspace_reserved_peak_bytes = c
                .recovery_workspace_reserved_peak_bytes
                .max(c.recovery_workspace_reserved_bytes);
        }
    }

    pub fn release(&mut self, bytes: usize) {
        self.bytes -= bytes;
        let mut c = self.metrics.0.lock().unwrap();
        c.workspace_reserved_bytes -= bytes;
        if self.recovery {
            c.recovery_workspace_reserved_bytes -= bytes;
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.release(self.bytes);
        let mut c = self.metrics.0.lock().unwrap();
        c.workspaces -= 1;
        if self.recovery {
            c.recovery_workspaces -= 1;
        }
    }
}
