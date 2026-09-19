use minicore_tui::{protocol::changes::*, state::changes::*};
use serde_json::{Value, json};
fn fixture(name: &str) -> Value {
    serde_json::from_str::<Value>(
        &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{name}.json")).unwrap(),
    )
    .unwrap()["result"]
        .clone()
}
#[test]
fn pinned_changes_and_status_schemas_additive_and_malformed() {
    for name in [
        "changes-list-workspace",
        "changes-list-page",
        "changes-list-stale",
    ] {
        let mut v = fixture(name);
        v["future"] = true.into();
        let page: ChangesList = serde_json::from_value(v.clone()).unwrap();
        assert!(page.valid_cursor());
        v["complete"] = "wrong".into();
        assert!(serde_json::from_value::<ChangesList>(v).is_err());
    }
    for name in ["changes-diff-workspace", "changes-diff-fragments"] {
        let mut v = fixture(name);
        v["future"] = true.into();
        let page: DiffPage = serde_json::from_value(v.clone()).unwrap();
        assert!(!format!("{page:?}").contains("workspace:"));
        v["hunks"] = 1.into();
        assert!(serde_json::from_value::<DiffPage>(v).is_err());
    }
    for name in [
        "workspace-status",
        "workspace-status-detached",
        "workspace-status-unavailable",
    ] {
        let mut v = fixture(name);
        v["future"] = true.into();
        serde_json::from_value::<WorkspaceStatus>(v.clone()).unwrap();
        v["repo_available"] = "wrong".into();
        assert!(serde_json::from_value::<WorkspaceStatus>(v).is_err());
    }
}
fn fragment(offset: usize, text: &str, complete: bool) -> Vec<DiffHunk> {
    vec![DiffHunk {
        old_start: 0,
        old_count: 0,
        new_start: 0,
        new_count: 1,
        lines: vec![DiffLine {
            kind: DiffKind::Added,
            old_index: None,
            new_index: Some(0),
            line_byte_offset: offset,
            line_byte_len: 9,
            text: text.into(),
            line_complete: complete,
        }],
    }]
}
#[test]
fn fragments_preserve_utf8_crlf_and_require_exact_identity_and_completion() {
    let mut b = DiffBuffer::default();
    b.append(fragment(0, "中", false)).unwrap();
    assert!(b.partial_line());
    let snapshot = b.clone();
    assert!(b.append(fragment(4, "🙂\r\n", true)).is_err());
    assert_eq!(b.bytes, 3);
    b.append(fragment(3, "🙂\r\n", true)).unwrap();
    assert!(!b.partial_line());
    assert_eq!(snapshot.bytes, 3);
    assert_eq!(
        b.lines[0]
            .body
            .chunks
            .iter()
            .map(AsRef::as_ref)
            .collect::<String>(),
        "中🙂\r\n"
    );
}
#[test]
fn incomplete_line_is_not_silently_skipped_and_budget_is_real() {
    let mut b = DiffBuffer::default();
    b.append(fragment(0, "中", false)).unwrap();
    assert!(b.append(fragment(0, "中", false)).is_err());
    assert!(b.append(fragment(3, "🙂\r\n", false)).is_err());
    assert_eq!(b.bytes, 3);
    let mut b = DiffBuffer::default();
    let mut rows = fragment(0, "中🙂\r\n", true);
    rows[0].new_count = 20_000;
    rows[0].lines = (0..20_000)
        .map(|i| DiffLine {
            kind: DiffKind::Added,
            old_index: None,
            new_index: Some(i),
            line_byte_offset: 0,
            line_byte_len: 1,
            text: "\n".into(),
            line_complete: true,
        })
        .collect();
    assert!(b.append(rows).is_err());
    assert!(b.lines.is_empty());
}
#[test]
fn diff_layout_copy_has_only_safe_source_not_labels_or_wraps() {
    use minicore_tui::state::workspace::FileLayoutIdentity;
    use std::sync::{Arc, atomic::AtomicBool};
    let mut b = DiffBuffer::default();
    b.append(fragment(0, "中🙂\r\n", true)).unwrap();
    let layout = DiffLayout::build(DiffLayoutRequest {
        identity: FileLayoutIdentity {
            generation: 1,
            revision: 1,
            width: 2,
        },
        buffer: b,
        cancel: Arc::new(AtomicBool::new(false)),
    })
    .unwrap();
    assert_eq!(&*layout.copy_text, "中🙂\r\n");
    assert!(layout.text.contains("@@ -0,0 +1,1 @@"));
    assert!(layout.rows.len() > 2);
}
#[test]
fn no_repository_is_different_from_failed_observation_and_old_branch_is_stale() {
    let mut value: WorkspaceStatus = serde_json::from_value(fixture("workspace-status")).unwrap();
    value.branch = Some("branch-secret".into());
    value.complete = true;
    value.repo_available = true;
    let mut observed = StatusObservation {
        value: Some(value),
        ..Default::default()
    };
    assert!(observed.label().contains("seen"));
    assert!(!format!("{observed:?}").contains("branch-secret"));
    observed.error = true;
    assert!(observed.label().contains("stale"));
    let value = observed.value.as_mut().unwrap();
    value.repo_available = false;
    value.complete = false;
    assert_eq!(observed.label(), "git? [incomplete]");
    observed.value.as_mut().unwrap().complete = true;
    observed.error = false;
    assert_eq!(observed.label(), "no-git");
}
#[test]
fn opaque_references_and_cursors_are_not_decoded_or_rebuilt() {
    use minicore_tui::protocol::{OutgoingRequest, RequestId};
    let cursor = json!({"future":"kept","hunk_index":7});
    let req = OutgoingRequest::changes_diff(
        RequestId(1),
        "s",
        "opaque/private reference",
        Comparison::HeadToIndex,
        Some(&cursor),
    );
    assert_eq!(req.params["change_ref"], "opaque/private reference");
    assert_eq!(req.params["cursor"], cursor);
    assert_eq!(req.params["max_bytes"], 65536);
}

#[test]
fn completed_lines_cannot_be_duplicated_or_reordered() {
    let mut b = DiffBuffer::default();
    b.append(fragment(0, "中🙂\r\n", true)).unwrap();
    assert!(b.append(fragment(0, "中🙂\r\n", true)).is_err());
    assert_eq!(b.bytes, 9);
    let mut malformed = fragment(0, "中🙂\r\n", true);
    malformed[0].old_start = usize::MAX;
    malformed[0].old_count = 1;
    assert!(DiffBuffer::default().append(malformed).is_err());
    let revision: ChangeRevision = serde_json::from_value(
        json!({"kind":"content","sha256":"provider-secret-not-a-hash","bytes":1}),
    )
    .unwrap();
    assert!(!revision.valid());
}

#[test]
fn extreme_width_diff_layout_is_bounded_and_labels_its_display_limit() {
    use minicore_tui::{limits, state::workspace::FileLayoutIdentity};
    use std::sync::{Arc, atomic::AtomicBool};
    let size = 200_000;
    let mut h = fragment(0, "", true);
    h[0].lines[0].line_byte_len = size;
    h[0].lines[0].text = "a".repeat(size);
    let mut b = DiffBuffer::default();
    b.append(h).unwrap();
    let layout = DiffLayout::build(DiffLayoutRequest {
        identity: FileLayoutIdentity {
            generation: 1,
            revision: 1,
            width: 1,
        },
        buffer: b,
        cancel: Arc::new(AtomicBool::new(false)),
    })
    .unwrap();
    assert!(layout.display_limited);
    assert_eq!(layout.rows.len(), limits::DIFF_LAYOUT_ROWS);
    assert_eq!(layout.copy_text.len(), size);
    assert!(layout.retained_bytes() < limits::LAYOUT_CACHE_BYTES);
}
