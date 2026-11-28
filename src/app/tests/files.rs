use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use minicore_host_protocol::{
    DirectoryListResult, NullablePatch, SessionConfigPatch, SessionOverrides, WorkspaceRef,
};

use super::harness::Harness;
use crate::app::{Msg, Overlay, files::path_reference, tasks};

async fn opened(workspace: &Path) -> Harness {
    let mut h = Harness::opened().await;
    let target = tasks::create_session(
        &h.native.client,
        h.native.target.agent_id.clone(),
        SessionOverrides(SessionConfigPatch {
            workspace: NullablePatch::Set(WorkspaceRef {
                path: workspace.to_string_lossy().into_owned(),
            }),
            ..SessionConfigPatch::default()
        }),
    )
    .await
    .unwrap();
    h.open(&target).await;
    h
}

async fn loaded(h: &mut Harness) {
    h.until(
        |app| matches!(&app.overlay, Overlay::Files { browser, .. } if browser.pending.is_none()),
    )
    .await;
}

fn assert_directory(h: &Harness, expected: &Path) {
    let Overlay::Files { browser, .. } = &h.app.overlay else {
        panic!("file browser closed")
    };
    // Host presents normal Windows paths; canonicalize may add the verbatim prefix.
    assert_eq!(
        Path::new(&browser.directory).canonicalize().unwrap(),
        expected.canonicalize().unwrap()
    );
}

#[tokio::test(start_paused = true)]
async fn file_browser_navigates_filters_and_inserts_reference_without_reading_or_sending() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let folder = root.join("source tree");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(
        folder.join("design notes.md"),
        "private contents are not a mention",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "readme").unwrap();
    let mut h = opened(&root).await;
    h.type_text("Review @");
    loaded(&mut h).await;
    assert_directory(&h, &root);
    let screen = h.render_text(100, 28);
    assert!(screen.contains("source tree/"), "{screen}");
    assert!(screen.contains("README.md"), "{screen}");
    h.type_text("source");
    assert_eq!(h.app.composer.text(), "Review @");
    h.press(KeyCode::Enter);
    loaded(&mut h).await;
    assert_directory(&h, &folder);
    assert!(matches!(&h.app.overlay, Overlay::Files { browser, .. } if browser.filter.is_empty()));

    h.type_text("..");
    h.press(KeyCode::Enter);
    loaded(&mut h).await;
    assert_directory(&h, &root);
    h.type_text("source");
    h.screen(100, 28);
    let hit = h.app.sheet_hit;
    for _ in 0..2 {
        assert!(h.app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.area.x + 2,
            row: hit.list_y,
            modifiers: KeyModifiers::NONE
        }));
    }
    loaded(&mut h).await;
    assert_directory(&h, &folder);
    h.type_text("notes");
    h.press(KeyCode::Tab);
    assert!(h.app.overlay.is_none());
    let reference = path_reference(&folder.join("design notes.md"), &root);
    let expected = format!("Review {reference} ");
    assert_eq!(h.app.composer.text(), expected);
    assert!(
        h.native.calls.try_recv().is_err(),
        "picking never submits a prompt"
    );
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.text(), "Review @");
    h.ctrl(KeyCode::Char('y'));
    assert_eq!(h.app.composer.text(), expected);
    h.press(KeyCode::Enter);
    let call = h.next_call().await;
    assert!(call.users().iter().any(|text| text.contains(&reference)));
    assert!(
        !call
            .users()
            .iter()
            .any(|text| text.contains("private contents"))
    );
    call.text("noted");
    call.finish();
    h.until(|app| !app.is_busy()).await;
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn absolute_file_paths_and_denied_paths_keep_browser_authority_and_draft() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let folder = root_path.join("docs space");
    std::fs::create_dir(&folder).unwrap();
    let file = folder.join("中文 notes.md");
    std::fs::write(&file, "body").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut h = opened(&root_path).await;
    h.type_text("Read @");
    loaded(&mut h).await;
    h.app.on_paste(folder.to_str().unwrap());
    assert_eq!(h.app.composer.text(), "Read @");
    h.press(KeyCode::Enter);
    loaded(&mut h).await;
    assert_directory(&h, &folder);
    h.app.on_paste(outside.path().to_str().unwrap());
    h.press(KeyCode::Enter);
    loaded(&mut h).await;
    assert_directory(&h, &folder);
    assert!(
        matches!(&h.app.overlay, Overlay::Files { browser, .. } if browser.error.is_some() && browser.filter == outside.path().to_str().unwrap())
    );
    h.ctrl(KeyCode::Char('u'));
    h.type_text("no match");
    h.press(KeyCode::Enter);
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    assert!(h.native.calls.try_recv().is_err());
    assert_eq!(h.app.composer.text(), "Read @");
    h.ctrl(KeyCode::Char('u'));
    h.app.on_paste(file.to_str().unwrap());
    h.press(KeyCode::Enter);
    h.until(|app| app.overlay.is_none()).await;
    assert_eq!(
        h.app.composer.text(),
        format!("Read {} ", path_reference(&file, &root_path))
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn closed_or_reopened_file_browser_ignores_old_results() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let mut h = opened(&root_path).await;
    h.type_text("draft @");
    let request = match &h.app.overlay {
        Overlay::Files { browser, .. } => browser.pending.as_ref().unwrap().0,
        _ => unreachable!(),
    };
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "draft @");
    h.app.open_panel(crate::app::Panel::Files);
    h.app.on_msg(Msg::Files(
        request,
        Ok(DirectoryListResult {
            path: root_path.join("old.txt").to_string_lossy().into_owned(),
            parent: None,
            is_file: true,
            entries: vec![],
            next_cursor: None,
        }),
    ));
    assert_eq!(h.app.composer.text(), "draft @");
    assert!(
        matches!(&h.app.overlay, Overlay::Files { browser, .. } if browser.pending.as_ref().unwrap().0 != request)
    );
    loaded(&mut h).await;
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "draft @");
    h.finish().await;
}

#[test]
fn references_quote_paths_and_keep_outside_paths_absolute() {
    let workspace = std::env::temp_dir().join("project");
    assert_eq!(
        path_reference(&workspace.join("README.md"), &workspace),
        "@README.md"
    );
    assert_eq!(
        path_reference(&workspace.join("file space.md"), &workspace),
        "@\"file space.md\""
    );
    let outside = std::env::temp_dir().join("screenshot.png");
    let reference = path_reference(&outside, &workspace);
    assert!(reference.contains("screenshot.png"));
    let quoted = reference.strip_prefix('@').unwrap();
    let decoded = if quoted.starts_with('"') {
        serde_json::from_str::<String>(quoted).unwrap()
    } else {
        quoted.to_owned()
    };
    assert_eq!(Path::new(&decoded), outside);
}

#[cfg(windows)]
#[test]
fn ordinary_windows_paths_are_relative_to_the_matching_verbatim_workspace() {
    for (file, workspace) in [
        (r"C:\work\note.md", r"\\?\C:\work"),
        (r"\\server\share\work\note.md", r"\\?\UNC\server\share\work"),
    ] {
        assert_eq!(
            path_reference(Path::new(file), Path::new(workspace)),
            "@note.md"
        );
    }

    for file in [r"D:\work\note.md", r"C:\workspace\note.md"] {
        assert_eq!(
            path_reference(Path::new(file), Path::new(r"\\?\C:\work")),
            path_reference(Path::new(file), Path::new(r"C:\work"))
        );
    }
}
