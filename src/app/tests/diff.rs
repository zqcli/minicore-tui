use std::path::Path;

use crossterm::event::KeyCode;
use minicore_host_protocol::{NullablePatch, SessionConfigPatch, SessionOverrides, WorkspaceRef};
use minicore_host_workspace::{ChangeKind, DiffLineKind};

use super::harness::Harness;
use crate::app::diff::{DiffFile, DiffRow, DiffState, text};
use crate::app::{Overlay, Panel, tasks};

fn git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(path)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn refreshing_diff_preserves_path_and_reloads_the_selected_worktree_content() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    for name in ["a", "b", "c"] {
        std::fs::write(dir.path().join(name), format!("{name} baseline\n")).unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "fixture"]);
    std::fs::write(dir.path().join("b"), "b changed\n").unwrap();
    std::fs::write(dir.path().join("c"), "c initial\n").unwrap();

    let mut h = Harness::opened().await;
    let target = tasks::create_session(
        &h.native.client,
        h.native.target.agent_id.clone(),
        SessionOverrides(SessionConfigPatch {
            workspace: NullablePatch::Set(WorkspaceRef {
                path: dir.path().to_str().unwrap().into(),
            }),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    h.open(&target).await;
    h.app.open_panel(Panel::Diff);
    h.until(|app| matches!(&app.overlay, Overlay::Diff(d) if d.files.len() == 2 && d.files.iter().all(|f| f.lines.is_some())))
        .await;
    h.press(KeyCode::Down);
    h.until(|app| matches!(&app.overlay, Overlay::Diff(d) if text(d).contains("+c initial")))
        .await;

    std::fs::write(dir.path().join("a"), "a changed\n").unwrap();
    std::fs::write(dir.path().join("c"), "c refreshed\nsecond added line\n").unwrap();
    h.ctrl(KeyCode::Char('r'));
    h.until(|app| matches!(&app.overlay, Overlay::Diff(d) if d.files.len() == 3))
        .await;
    let Overlay::Diff(diff) = &h.app.overlay else {
        unreachable!()
    };
    assert_eq!(diff.current().map(|f| f.path.as_str()), Some("c"));
    assert!(
        diff.rows.is_empty(),
        "old hunks must be invalidated before the new read"
    );
    h.until(|app| matches!(&app.overlay, Overlay::Diff(d) if text(d).contains("+c refreshed") && d.files.iter().all(|f| f.lines.is_some())))
        .await;
    let Overlay::Diff(diff) = &h.app.overlay else {
        unreachable!()
    };
    assert_eq!(diff.current().unwrap().lines, Some((2, 1)));

    std::fs::write(dir.path().join("c"), "c baseline\n").unwrap();
    h.ctrl(KeyCode::Char('r'));
    h.until(|app| matches!(&app.overlay, Overlay::Diff(d) if d.files.len() == 2))
        .await;
    let Overlay::Diff(diff) = &h.app.overlay else {
        unreachable!()
    };
    assert_eq!(diff.current().map(|f| f.path.as_str()), Some("b"));
    assert!(diff.rows.is_empty());
    h.until(|app| matches!(&app.overlay, Overlay::Diff(d) if text(d).contains("+b changed")))
        .await;
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn diff_end_and_home_reach_wrapped_content_in_a_narrow_viewport() {
    let mut h = Harness::opened().await;
    let mut diff = DiffState::default();
    diff.set_files(vec![DiffFile {
        path: "long.rs".into(),
        kind: ChangeKind::Modified,
        lines: Some((1, 0)),
        failed: false,
        change_refs: Vec::new(),
    }]);
    diff.rows = vec![DiffRow::Line {
        kind: DiffLineKind::Added,
        gutter: "  1 ".into(),
        text: format!("{}TAIL_MARKER", "long text ".repeat(100)),
    }];
    h.app.overlay = Overlay::Diff(diff);
    h.press(KeyCode::Tab);
    let top = h.render_text(48, 16);
    assert!(!top.contains("TAIL_MARKER"));
    h.press(KeyCode::End);
    let end = h.render_text(48, 16);
    assert!(end.contains("TAIL_MARKER"), "{end}");
    h.press(KeyCode::Home);
    assert_eq!(h.render_text(48, 16), top);
    h.finish().await;
}
