use std::path::PathBuf;

use crossterm::event::KeyCode;

use super::harness::Harness;
use crate::app::{Msg, Overlay, Panel, files::path_reference};
use crate::clipboard::PastePayload;

fn deliver(h: &mut Harness, payload: PastePayload) {
    let target = h.app.clipboard_target().unwrap();
    h.app.on_msg(Msg::ClipboardPasted(target, Ok(payload)));
}

#[tokio::test(start_paused = true)]
async fn native_paste_inserts_file_and_image_references_without_sending() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("Review");
    let file = h.app.workspace.join("design notes.md");
    let image = std::env::temp_dir().join("minicore-clipboard-test.png");
    let expected = format!(
        "Review {} {} ",
        path_reference(&file, &h.app.workspace),
        path_reference(&image, &h.app.workspace)
    );
    deliver(&mut h, PastePayload::Paths(vec![file, image]));
    assert_eq!(h.app.composer.text(), expected);
    assert!(h.native.calls.try_recv().is_err());
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.text(), "Review");

    h.app.composer.set_text("replace all");
    h.app
        .composer
        .extend_selection(|composer| composer.move_home());
    let file = h.app.workspace.join("file.txt");
    deliver(&mut h, PastePayload::Paths(vec![file]));
    assert_eq!(h.app.composer.text(), "@file.txt ");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn native_text_paste_uses_existing_long_text_chips_and_failure_preserves_draft() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("Draft\n");
    let text = "long pasted line\n".repeat(40);
    deliver(&mut h, PastePayload::Text(text.clone()));
    assert_eq!(h.app.composer.text(), format!("Draft\n{text}"));
    assert_eq!(h.app.composer.pastes().len(), 1);
    assert!(h.native.calls.try_recv().is_err());
    let target = h.app.clipboard_target().unwrap();
    let before = h.app.composer.text().to_owned();
    h.app.on_msg(Msg::ClipboardPasted(
        target,
        Err("clipboard unavailable".into()),
    ));
    assert_eq!(h.app.composer.text(), before);
    assert!(h.notices().contains(&"clipboard unavailable"));
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn late_clipboard_results_never_paste_into_changed_cursor_draft_or_session() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("original");
    let target = h.app.clipboard_target().unwrap();
    h.press(KeyCode::Left);
    h.app.on_msg(Msg::ClipboardPasted(
        target,
        Ok(PastePayload::Text("old".into())),
    ));
    assert_eq!(h.app.composer.text(), "original");
    let target = h.app.clipboard_target().unwrap();
    h.type_text("x");
    let draft = h.app.composer.text().to_owned();
    h.app.on_msg(Msg::ClipboardPasted(
        target,
        Ok(PastePayload::Text("old".into())),
    ));
    assert_eq!(h.app.composer.text(), draft);
    let target = h.app.clipboard_target().unwrap();
    let other = h.another_session().await;
    h.open(&other).await;
    h.app.on_msg(Msg::ClipboardPasted(
        target,
        Ok(PastePayload::Text("old".into())),
    ));
    assert!(h.app.composer.is_empty());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn native_paste_in_file_browser_updates_only_its_filter_and_reopen_rejects_old_reply() {
    let mut h = Harness::opened().await;
    h.type_text("Find @");
    let target = h.app.clipboard_target().unwrap();
    h.app.on_msg(Msg::ClipboardPasted(
        target,
        Ok(PastePayload::Paths(vec![PathBuf::from(
            "/path with spaces",
        )])),
    ));
    assert_eq!(h.app.composer.text(), "Find @");
    assert!(
        matches!(&h.app.overlay, Overlay::Files { browser, .. } if browser.filter == "/path with spaces")
    );
    let old = h.app.clipboard_target().unwrap();
    h.press(KeyCode::Esc);
    h.app.open_panel(Panel::Files);
    h.app.on_msg(Msg::ClipboardPasted(
        old,
        Ok(PastePayload::Text("old".into())),
    ));
    assert!(matches!(&h.app.overlay, Overlay::Files { browser, .. } if browser.filter.is_empty()));
    assert_eq!(h.app.composer.text(), "Find @");
    h.finish().await;
}
