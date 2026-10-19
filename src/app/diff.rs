//! The diff view: which changes to list and the reads that fill it.

use serde_json::json;

use super::{App, Overlay, SessionOp, session::Session, tasks};
use crate::protocol::changes::DiffPage;
use crate::protocol::{self, ChangeKind, ChangeList, ChangeRecord, ChangeScope, RpcError};
use crate::transcript::{Block, Transcript};
use crate::ui::views::{self, DiffFile, DiffView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffScope {
    Turn,
    Session,
    Workspace,
}

/// Which path's hunks a read is for and which scope asked: an answer only
/// paints the pane when it matches the diff that is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffQuery {
    pub scope: DiffScope,
    pub path: String,
}

impl App {
    /// Rebuilds the diff view from the cards the session holds; the hunks
    /// appear when `changes.diff` answers.
    pub(super) fn open_diff(&mut self, scope: DiffScope) {
        let Some(session) = self.session.as_ref() else {
            self.diff = DiffView::new("Diff");
            return;
        };
        let title = match scope {
            DiffScope::Turn => "Diff · turn",
            DiffScope::Session => "Diff · session",
            DiffScope::Workspace => "Diff · workspace",
        };
        let mut view = DiffView::new(title);
        view.scope = scope;
        let wire = match (scope, latest_loop(session)) {
            (DiffScope::Turn, Some(loop_id)) => {
                view.set_files(collect_changed_files(&session.transcript, &loop_id));
                ChangeScope::Turn(loop_id)
            }
            (DiffScope::Turn, None) => {
                view.status = "No turn has finished yet; tab shows the session.".into();
                self.diff = view;
                return;
            }
            (DiffScope::Session, _) => ChangeScope::Session,
            (DiffScope::Workspace, _) => ChangeScope::Workspace,
        };
        if view.files.is_empty() {
            view.status = "No file changes are recorded for this scope.".into();
        }
        self.diff = view;
        let fields = json!({"scope": wire.value(), "limit": 200, "max_bytes": 262_144});
        self.call(
            protocol::METHOD_CHANGES_LIST,
            fields,
            move |result: Result<ChangeList, RpcError>| {
                SessionOp::ChangeFiles(scope, result.map(|list| list.records))
            },
        );
    }

    /// Tab: turn, session, workspace, then turn again.
    pub fn diff_cycle_scope(&mut self) {
        let Overlay::Diff {
            scope,
            file,
            scroll,
        } = &mut self.overlay
        else {
            return;
        };
        *scope = match scope {
            DiffScope::Turn => DiffScope::Session,
            DiffScope::Session => DiffScope::Workspace,
            DiffScope::Workspace => DiffScope::Turn,
        };
        *file = 0;
        *scroll = 0;
        let scope = *scope;
        self.open_diff(scope);
    }

    /// Fills the file column, then asks for the hunks of every path.
    pub(super) fn on_change_files(
        &mut self,
        scope: DiffScope,
        result: Result<Vec<ChangeRecord>, RpcError>,
    ) {
        let records = match result {
            Ok(records) => records,
            Err(error) => {
                self.diff.status = error.message;
                return;
            }
        };
        // One row per path: a turn can change a file several times. A row keeps
        // its counts only while its records are the same set.
        let previous = std::mem::take(&mut self.diff.files);
        let mut files: Vec<DiffFile> = Vec::new();
        for record in &records {
            match files.iter_mut().find(|file| file.path == record.path) {
                Some(file) => file.change_refs.push(record.change_ref.clone()),
                None => {
                    let measured = previous
                        .iter()
                        .find(|file| file.path == record.path)
                        .filter(|file| file.change_refs == [record.change_ref.clone()]);
                    files.push(DiffFile {
                        path: record.path.clone(),
                        kind: record.kind,
                        lines: measured.and_then(|file| file.lines),
                        failed: measured.is_some_and(|file| file.failed),
                        change_refs: vec![record.change_ref.clone()],
                    });
                }
            }
        }
        self.diff.set_files(files);
        if self.diff.files.is_empty() {
            self.diff.status = "No file changes are recorded for this scope.".into();
            return;
        }
        self.diff.status.clear();
        // Every unmeasured row is read once, the selected one first, so the
        // list carries counts instead of unknowns.
        self.diff_in_flight = None;
        self.diff_pending = self
            .diff
            .files
            .iter()
            .filter(|file| file.lines.is_none() && !file.failed && !file.change_refs.is_empty())
            .map(|file| {
                let query = DiffQuery {
                    scope,
                    path: file.path.clone(),
                };
                (query, file.change_refs.clone())
            })
            .collect();
        self.measure_next_diff_row();
        if self.diff_in_flight.is_none() {
            self.load_selected_diff();
        }
    }

    /// Starts the next unmeasured row's read; one at a time keeps within the
    /// Agent's query allowance.
    fn measure_next_diff_row(&mut self) {
        if self.diff_in_flight.is_some() || self.diff_pending.is_empty() {
            return;
        }
        let (query, refs) = self.diff_pending.remove(0);
        self.start_diff_read(query, refs);
    }

    /// Reads the selected file's records; when the row is unmeasured this read
    /// is its measurement too.
    pub(super) fn load_selected_diff(&mut self) {
        let Some(file) = self
            .diff
            .current()
            .filter(|file| !file.change_refs.is_empty())
        else {
            return;
        };
        let query = DiffQuery {
            scope: self.diff.scope,
            path: file.path.clone(),
        };
        if self.diff_in_flight.as_ref() == Some(&query) {
            return;
        }
        let refs = file.change_refs.clone();
        self.diff_pending.retain(|(pending, _)| *pending != query);
        self.start_diff_read(query, refs);
    }

    fn start_diff_read(&mut self, query: DiffQuery, refs: Vec<String>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        self.diff_in_flight = Some(query.clone());
        let client = self.client.clone();
        let session_id = session.info.session_id.clone();
        self.spawn(async move {
            let pages = tasks::read_diffs(client, session_id, refs).await;
            SessionOp::DiffFile(query, pages)
        });
    }

    pub(super) fn on_diff_file(
        &mut self,
        query: DiffQuery,
        result: Result<Vec<DiffPage>, RpcError>,
    ) {
        let shown = self.diff.scope == query.scope
            && self
                .diff
                .current()
                .is_some_and(|file| file.path == query.path);
        let row = self
            .diff
            .files
            .iter_mut()
            .find(|file| file.path == query.path);
        if self.diff_in_flight.as_ref() == Some(&query) {
            self.diff_in_flight = None;
        }
        match result {
            Ok(pages) => {
                // Records are counted one by one: line indexes only identify a
                // line within a record.
                let (added, removed) = pages
                    .iter()
                    .map(|page| protocol::changes::count_lines(std::slice::from_ref(page)))
                    .fold((0, 0), |sum, count| (sum.0 + count.0, sum.1 + count.1));
                if let Some(file) = row {
                    file.lines = Some((added, removed));
                    file.failed = false;
                }
                self.measure_next_diff_row();
                if shown {
                    let tools: Vec<Option<String>> = pages
                        .iter()
                        .map(|page| {
                            let call = self
                                .session
                                .as_ref()?
                                .transcript
                                .find_tool(page.tool_ref.as_ref()?)?;
                            Some(call.name.clone())
                        })
                        .collect();
                    self.diff.hunks = views::records_lines(&pages, &tools, &self.palette);
                    self.diff.scroll = 0;
                    // Records with equal before and after content have no hunks.
                    let unchanged = pages
                        .iter()
                        .all(|page| page.hunks.is_empty() && !page.binary);
                    self.diff.status = if unchanged {
                        "No line changes in these records (content unchanged).".into()
                    } else {
                        String::new()
                    };
                }
            }
            Err(error) => {
                if let Some(file) = row {
                    file.failed = true;
                }
                self.measure_next_diff_row();
                if shown {
                    self.diff.hunks.clear();
                    self.diff.status = error.message;
                }
            }
        }
    }
}

/// The loop of the newest finished turn, for the turn scope.
fn latest_loop(session: &Session) -> Option<String> {
    session
        .transcript
        .blocks()
        .iter()
        .rev()
        .find_map(|block| match block {
            Block::TurnEnd { loop_id, .. } => Some(loop_id.clone()),
            _ => None,
        })
}

/// The files a turn touched, from its write and edit cards, so the view lists
/// something before `changes.list` answers.
fn collect_changed_files(transcript: &Transcript, loop_id: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    for block in transcript.blocks() {
        let Block::Tool(call) = block else {
            continue;
        };
        let path = call.detail.trim();
        let writes = matches!(
            call.name.as_str(),
            "write" | "edit" | "apply_patch" | "patch"
        );
        if call.key.loop_id != loop_id || !writes || path.is_empty() {
            continue;
        }
        if files.iter().any(|file| file.path == path) {
            continue;
        }
        files.push(DiffFile {
            path: path.to_owned(),
            kind: ChangeKind::Modified,
            lines: None,
            failed: false,
            change_refs: Vec::new(),
        });
    }
    files
}
