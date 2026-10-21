use super::harness::*;
use crate::app::{Overlay, Panel};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn mouse(h: &mut Harness, kind: MouseEventKind, x: u16, y: u16) {
    assert!(h.app.on_mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE
    }));
}

#[tokio::test(start_paused = true)]
async fn editor_click_drag_shift_replace_and_undo_work_through_real_layout() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("ab中文cd\nsecond line");
    h.screen(60, 24);
    let area = h.app.composer_area;
    assert_eq!(area.height, 4);
    let y = area.y + 1;
    let x = area.x + 1; // skip the rail
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), x + 2, y);
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x + 2, y);
    assert_eq!(h.app.composer.cursor(), 2);
    assert!(h.app.composer.selection().is_none());
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), x + 2, y);
    mouse(&mut h, MouseEventKind::Drag(MouseButton::Left), x + 6, y);
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x + 6, y);
    assert_eq!(h.app.composer.selected_text(), Some("中文"));
    assert!(
        h.app.selection.is_none(),
        "editor drag never copies transcript text"
    );
    assert!(h.app.press.is_none());
    h.type_text("X");
    assert_eq!(h.app.composer.text(), "abXcd\nsecond line");
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.selected_text(), Some("中文"));
    h.press(KeyCode::Right);
    assert!(h.app.composer.selection().is_none());
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("文"));
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("中文"));
    h.key(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("文"));
    h.press(KeyCode::Delete);
    assert_eq!(h.app.composer.text(), "ab中cd\nsecond line");
}

#[tokio::test(start_paused = true)]
async fn editor_shift_selection_crosses_lines_without_changing_prompt_history() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("a中\nsecond");
    h.key(KeyCode::Home, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("second"));
    h.key(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("a中\nsecond"));
    h.key(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("a中\nsecond"));
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "a中\nsecond");
    assert!(h.app.composer.selection().is_none());
}

#[tokio::test(start_paused = true)]
async fn completion_rows_and_fullscreen_sheets_are_not_editing_hit_targets() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/di");
    h.screen(80, 24);
    assert!(matches!(h.app.overlay, Overlay::Palette { .. }));
    let body = h.app.composer_area;
    mouse(
        &mut h,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y + 1,
    );
    mouse(
        &mut h,
        MouseEventKind::Up(MouseButton::Left),
        body.x + 1,
        body.y + 1,
    );
    assert_eq!(h.app.composer.cursor(), 0);
    let cursor = h.app.composer.cursor();
    mouse(
        &mut h,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 3,
        body.bottom(),
    );
    assert_eq!(
        h.app.composer.cursor(),
        cursor,
        "completion list is not text"
    );
    h.app.open_panel(Panel::Help);
    h.screen(80, 24);
    assert!(h.app.composer_area.is_empty());
    let before = h.app.composer.text().to_owned();
    h.app.on_paste("not in hidden draft");
    assert_eq!(h.app.composer.text(), before);
}

#[tokio::test(start_paused = true)]
async fn dragging_outside_editor_scrolls_long_drafts_and_keeps_the_anchor() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text(&"line\n".repeat(20));
    h.screen(60, 24);
    let area = h.app.composer_area;
    let x = area.x + 1;
    mouse(
        &mut h,
        MouseEventKind::Down(MouseButton::Left),
        x,
        area.bottom() - 1,
    );
    let anchor = h.app.composer.cursor();
    for _ in 0..25 {
        mouse(
            &mut h,
            MouseEventKind::Drag(MouseButton::Left),
            x,
            area.y - 1,
        );
        h.screen(60, 24);
    }
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x, area.y - 1);
    assert_eq!(h.app.composer.cursor(), 0);
    assert_eq!(h.app.composer.selection(), Some(0..anchor));
    assert_eq!(h.app.scroll, crate::app::Scroll::Follow);
}

#[tokio::test(start_paused = true)]
async fn a_multiline_paste_is_draft_text_and_never_runs_a_command() {
    for text in ["/new\r\nhello\n", "first\nsecond\n"] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.app.on_paste(text);
        h.settle().await;
        assert_eq!(h.app.composer.text(), text.replace("\r\n", "\n"));
        for method in [
            "turn.send",
            "turn.steer",
            "session.create",
            "session.delete",
            "interaction.answer",
        ] {
            assert!(h.calls(method).is_empty(), "paste invoked {method}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn copying_a_selection_drops_it_so_ctrl_c_quits_again() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("keep this");
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    h.ctrl(KeyCode::Char('c'));
    assert_eq!(h.app.composer.selection(), None);
    assert_eq!(h.app.composer.text(), "keep this");
    h.ctrl(KeyCode::Char('c'));
    h.ctrl(KeyCode::Char('c'));
    assert!(h.app.quit);
}

#[tokio::test(start_paused = true)]
async fn a_cut_the_clipboard_would_refuse_keeps_the_text() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let big = "x".repeat(crate::clipboard::MAX_CLIPBOARD_BYTES + 1);
    h.app.composer.set_text(&big);
    h.key(KeyCode::Home, KeyModifiers::SHIFT);
    h.ctrl(KeyCode::Char('x'));
    assert_eq!(h.app.composer.text(), big);
    assert!(
        h.notices()[0].contains("clipboard limit"),
        "{:?}",
        h.notices()
    );
}

#[tokio::test(start_paused = true)]
async fn shift_end_with_no_draft_follows_the_tail_like_end() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.scroll = crate::app::Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    h.key(KeyCode::End, KeyModifiers::SHIFT);
    assert_eq!(h.app.scroll, crate::app::Scroll::Follow);
}

#[tokio::test(start_paused = true)]
async fn a_drag_in_the_editor_copies_on_release_and_stays_selected() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("copy me");
    h.screen(60, 24);
    let area = h.app.composer_area;
    let (x, y) = (area.x + 1, area.y + 1);
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut h, MouseEventKind::Drag(MouseButton::Left), x + 3, y);
    assert!(h.app.flash.is_none(), "nothing is copied mid-drag");
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x + 3, y);
    h.settle().await;
    assert!(h.app.flash.is_some(), "{:?}", h.notices());
    assert_eq!(h.app.composer.selected_text(), Some("cop"));
}
