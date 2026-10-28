//! Phone queries: bounded Agent reads for one device's view, answered when the Agent replies.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::wire::Query;
use crate::app::{Session, diff, overlay, session::reported_percent, tasks};
use crate::protocol::{
    self, ChangeList, ContextUsage, FilesPage, SessionContext, ToolOutputPage, changes::DiffPage,
};
use crate::rpc::Client;

/// Queries one connection may have waiting on the Agent.
pub const IN_FLIGHT: u8 = 4;
const DIFF_BYTES: usize = 256 * 1024;
const OUTPUT_BYTES: usize = 64 * 1024;
const FILES: usize = 50;

pub type Work = BoxFuture<'static, Result<Value, String>>;

/// The Agent reads `query` needs for `session`, or why it is refused at once.
pub fn start(client: Arc<Client>, session: Option<&Session>, query: Query) -> Result<Work, String> {
    let session = session.ok_or("No session")?;
    let session_id = session.info.session_id.clone();
    let params = |mut params: Value| {
        params["session_id"] = json!(session_id);
        params
    };
    Ok(match query {
        Query::Context => {
            let usage = session
                .presentation
                .as_ref()
                .and_then(|p| p.context.clone());
            let params = params(json!({}));
            Box::pin(async move {
                let context = call(&client, protocol::METHOD_SESSION_CONTEXT, params).await?;
                Ok(context_data(&context, usage.as_ref()))
            })
        }
        Query::Changes => {
            let branch = session.branch().map(str::to_owned);
            let params = params(diff::workspace_changes());
            Box::pin(async move {
                let list = call(&client, protocol::METHOD_CHANGES_LIST, params).await?;
                Ok(changes_data(branch, &list))
            })
        }
        Query::Diff { path } => {
            let params = params(diff::workspace_changes());
            Box::pin(async move {
                // Only a path the Agent lists has change references to diff.
                let list: ChangeList = call(&client, protocol::METHOD_CHANGES_LIST, params).await?;
                let refs: Vec<String> = list
                    .records
                    .into_iter()
                    .filter(|record| record.path == path)
                    .map(|record| record.change_ref)
                    .collect();
                if refs.is_empty() {
                    return Err("stale".into());
                }
                let pages = tasks::read_diffs(client, session_id, refs)
                    .await
                    .map_err(|error| error.message)?;
                Ok(diff_data(path, &pages))
            })
        }
        Query::ToolOutput { call_id, offset } => {
            let key = &session
                .transcript
                .find_tool_by_call_id(&call_id)
                .ok_or("stale")?
                .key;
            let params = json!({
                "session_id": key.session_id, "loop_id": key.loop_id,
                "request_index": key.request_index, "tool_call_id": key.tool_call_id,
                "stream": "output", "offset": offset, "max_bytes": OUTPUT_BYTES,
            });
            Box::pin(async move {
                let page: ToolOutputPage =
                    call(&client, protocol::METHOD_TOOL_OUTPUT, params).await?;
                Ok(
                    json!({"text": page.data, "offset": page.base_offset, "next_offset": page.next_offset,
                    "eof": page.eof, "truncated": page.truncated}),
                )
            })
        }
        Query::Files { query } => {
            let params = params(overlay::files_params(&query, FILES));
            Box::pin(async move {
                let page: FilesPage =
                    call(&client, protocol::METHOD_WORKSPACE_FILES, params).await?;
                let truncated = page.truncated;
                Ok(json!({"paths": page.files(), "truncated": truncated}))
            })
        }
    })
}

async fn call<R: DeserializeOwned>(
    client: &Client,
    method: &'static str,
    params: Value,
) -> Result<R, String> {
    client
        .call(method, &params)
        .await
        .map_err(|error| error.message)
}

/// The figures the `/context` view shows, with the footer's percentage.
fn context_data(context: &SessionContext, usage: Option<&ContextUsage>) -> Value {
    let budget = context.budget.clone().unwrap_or_default();
    json!({
        "tokens": usage.and_then(|usage| usage.tokens),
        "window": usage.and_then(|usage| usage.window),
        "percent": usage.and_then(|usage| reported_percent(usage, Some(context))),
        "budget": budget.input_budget_tokens,
        "history_tokens": budget.estimated_history_tokens,
        "trigger_tokens": budget.trigger_tokens,
        "target_tokens": budget.target_tokens,
        "compacting": context.current_operation.is_some(),
        "coverage": context.coverage.as_ref().map(|coverage| json!({"turns": coverage.covered_loop_count,
            "items": coverage.covered_item_count, "kept": coverage.retained_item_count})),
        "last_compaction": context.last_result.as_ref().map(|last| json!({"status": last.status, "before": last.before_tokens, "after": last.after_tokens})),
        "failure": context.last_prepare_failure,
    })
}

/// One row per path, as the diff view lists them.
fn changes_data(branch: Option<String>, list: &ChangeList) -> Value {
    let mut files: Vec<Value> = Vec::new();
    for record in &list.records {
        if !files
            .iter()
            .any(|file| file["path"] == record.path.as_str())
        {
            files.push(json!({"path": record.path, "status": record.kind}));
        }
    }
    json!({"branch": branch, "files": files, "truncated": list.next_cursor.is_some()})
}

/// The diff view's rows as unified-diff lines, cut at `DIFF_BYTES`.
fn diff_data(path: String, pages: &[DiffPage]) -> Value {
    let (added, removed) = diff::measure(pages);
    let mut truncated = pages.iter().any(|page| page.truncated);
    let (mut lines, mut bytes) = (Vec::new(), 0);
    for row in diff::rows(pages, &[]) {
        let line = row.body();
        bytes += line.len() + 1;
        if bytes > DIFF_BYTES {
            truncated = true;
            break;
        }
        lines.push(line);
    }
    json!({"path": path, "lines": lines, "added": added, "removed": removed, "truncated": truncated})
}
