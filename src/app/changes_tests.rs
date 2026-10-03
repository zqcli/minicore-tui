use super::*;
use crate::{
    protocol::changes::*,
    state::panels::MainView,
    ui::testapp::{self, respond, take_requests},
};
use serde_json::{Value, json};
fn app() -> App {
    testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high")
}
fn fixture(name: &str) -> Value {
    serde_json::from_str::<Value>(
        &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{name}.json")).unwrap(),
    )
    .unwrap()["result"]
        .clone()
}
fn list() -> Value {
    let mut v = fixture("changes-list-workspace");
    v["session_id"] = "ses_1".into();
    v
}
fn request(a: &mut App, scope: ChangeScope) -> OutgoingRequest {
    let requests = take_requests(a.open_changes(scope));
    for r in requests.iter().filter(|r| r.method == "workspace.status") {
        respond(a, r, fixture("workspace-status"));
    }
    requests
        .into_iter()
        .find(|r| r.method == "changes.list")
        .unwrap()
}
#[test]
fn changes_two_levels_restore_draft_and_ignore_hidden_diff_response() {
    let mut a = app();
    a.composer.type_text("draft");
    let r = request(&mut a, ChangeScope::Workspace);
    assert_eq!(r.params["limit"], 100);
    respond(&mut a, &r, list());
    let records = &a.changes().unwrap().records;
    assert!(!records.is_empty());
    let reference = records[0].change_ref.clone();
    let r = take_requests(a.changes_select()).remove(0);
    assert_eq!(r.params["change_ref"], reference);
    assert_eq!(r.params["context_lines"], 3);
    assert!(a.changes_back());
    assert_eq!(a.queries.in_flight_len(), 1);
    respond(&mut a, &r, fixture("changes-diff-workspace"));
    assert!(!a.changes().unwrap().in_diff);
    assert!(a.changes().unwrap().detail.as_ref().unwrap().meta.is_none());
    a.close_main_detail();
    assert_eq!(a.composer.content(), "draft");
    assert!(a.queries.is_empty());
}

#[test]
fn fragmented_diff_copy_uses_visible_safe_source_and_late_layout_cannot_install_in_file() {
    use crate::state::{changes::DiffLayout, workspace::ReturnTarget};
    let mut a = app();
    let list_request = request(&mut a, ChangeScope::Workspace);
    respond(&mut a, &list_request, list());
    let first = take_requests(a.changes_select()).remove(0);
    let reference = first.params["change_ref"].clone();
    let cursor = json!({"session_id":"ses_1","change_ref":reference,"ops_fingerprint":"a".repeat(64),"context_lines":3,"hunk_index":0,"line_index":0,"line_byte_offset":3,"future":"opaque additive"});
    let page = |offset, text: &str, complete: bool, next: Value| {
        let mut p = fixture("changes-diff-workspace");
        p["comparison"] = "head_to_worktree".into();
        p["complete"] = complete.into();
        p["truncated"] = (!complete).into();
        p["next_cursor"] = next;
        p["hunks"] = json!([{"old_start":0,"old_count":0,"new_start":0,"new_count":1,"lines":[{"kind":"added","new_index":0,"line_byte_offset":offset,"line_byte_len":9,"line_complete":complete,"text":text}]}]);
        p
    };
    respond(&mut a, &first, page(0, "中", false, cursor.clone()));
    let width = a.main_body_area().width.saturating_sub(16).max(1);
    let layout = a.diff_layout_request(width).unwrap();
    a.mark_diff_layout_pending(layout.identity.clone());
    a.update(AppEvent::DiffLayoutPrepared(
        DiffLayout::build(layout).unwrap(),
    ));
    let next = take_requests(a.changes_more(false)).remove(0);
    assert_eq!(next.params["cursor"], cursor);
    respond(&mut a, &next, page(3, "🙂\r\n", true, Value::Null));
    let copy = a.copy_diff();
    assert!(matches!(&copy[0],AppCommand::CopySelection(text) if text.as_str()=="中"));
    assert!(a.notices.back().unwrap().text.contains("部分"));
    let pending = a.diff_layout_request(width).unwrap();
    a.mark_diff_layout_pending(pending.identity.clone());
    a.open_file_preview("other".into(), None, ReturnTarget::Conversation);
    a.update(AppEvent::DiffLayoutPrepared(
        DiffLayout::build(pending).unwrap(),
    ));
    assert!(a.file_preview().unwrap().layout.is_none());
}
#[test]
fn changes_late_scope_and_comparison_pages_only_release_real_slots() {
    let mut a = app();
    let old = request(&mut a, ChangeScope::Workspace);
    a.open_changes(ChangeScope::Session);
    assert_eq!(a.changes().unwrap().scope, ChangeScope::Session);
    let commands = take_requests(respond(&mut a, &old, list()));
    assert!(a.changes().unwrap().records.is_empty());
    assert!(
        commands
            .iter()
            .any(|r| r.method == "changes.list" && r.params["scope"] == "session")
    );
}
#[test]
fn workspace_status_is_explicit_and_errors_do_not_claim_no_git_or_use_presentation_branch() {
    let mut a = app();
    assert_eq!(a.active_view().unwrap().workspace_status.label(), "no-git");
    for _ in 0..20 {
        assert!(a.update(AppEvent::Tick).is_empty());
    }
    a.arm_workspace_status("ses_1", false);
    let r = take_requests(a.poll_workspace_status()).remove(0);
    let mut status = fixture("workspace-status");
    status["branch"] = "new-observed".into();
    respond(&mut a, &r, status);
    let v = a.sessions.known.get_mut("ses_1").unwrap();
    v.presentation.as_mut().unwrap().git_branch = Some("old-presentation".into());
    assert!(v.workspace_status.label().contains("new-observed"));
    a.arm_workspace_status("ses_1", false);
    let r = take_requests(a.poll_workspace_status()).remove(0);
    testapp::respond_rpc_error(&mut a, &r, -32000, "private workspace");
    let label = a.active_view().unwrap().workspace_status.label();
    assert!(label.contains("new-observed"));
    assert!(label.contains("stale"));
    assert!(!label.contains("no-git"));
    assert!(a.update(AppEvent::Tick).is_empty());
}
#[test]
fn closed_workspace_never_implicitly_opens_and_scope_parser_is_exact() {
    let mut a = app();
    a.sessions.known.get_mut("ses_1").unwrap().info.loaded = false;
    assert!(a.open_changes(ChangeScope::Workspace).is_empty());
    assert!(a.changes().is_none());
    let r = take_requests(a.open_changes(ChangeScope::Turn {
        loop_id: "exact".into(),
    }))
    .remove(0);
    assert_eq!(r.params["scope"], json!({"turn":{"loop_id":"exact"}}));
    assert!(crate::command::parse_command("/diff turn").is_err());
    assert_eq!(
        crate::command::parse_command("/diff").unwrap(),
        LocalCommand::Diff(ChangeScope::Workspace)
    );
}
#[test]
fn list_deadline_stale_and_error_have_no_automatic_retries() {
    let mut a = app();
    let r = request(&mut a, ChangeScope::Workspace);
    respond(&mut a, &r, list());
    if let MainView::Changes(s) = &mut a.main_view {
        s.cursor = Some(
            json!({"session_id":"ses_1","scope":"workspace","offset":1,"observation":"opaque","future":17}),
        );
    }
    let r = take_requests(a.changes_more(false)).remove(0);
    assert_eq!(r.params["cursor"]["future"], 17);
    let mut stale = list();
    stale["stale"] = true.into();
    respond(&mut a, &r, stale);
    assert!(a.changes().unwrap().error.is_some());
    assert!(!a.changes().unwrap().records.is_empty());
    assert!(a.changes_more(false).is_empty());
    assert!(a.update(AppEvent::Tick).is_empty());
}
