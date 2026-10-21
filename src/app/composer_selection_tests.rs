use super::*;

fn draft(text: &str) -> Composer {
    let mut c = Composer::new();
    c.set_text(text);
    c
}

#[test]
fn shift_arrows_extend_shrink_reverse_and_never_split_graphemes() {
    let mut c = draft("a中e\u{301}👨‍👩‍👧‍👦z");
    c.move_selection(true, Composer::move_left);
    assert_eq!(c.selected_text(), Some("z"));
    c.move_selection(true, Composer::move_left);
    assert_eq!(c.selected_text(), Some("👨‍👩‍👧‍👦z"));
    c.move_selection(true, Composer::move_right);
    assert_eq!(c.selected_text(), Some("z"));
    c.move_selection(true, Composer::move_right);
    assert!(c.selection().is_none());
    c.place_cursor(1, false);
    c.move_selection(true, Composer::move_left);
    assert_eq!(c.selected_text(), Some("a"));
    c.move_selection(true, Composer::move_right);
    c.move_selection(true, Composer::move_right);
    assert_eq!(c.selected_text(), Some("中"));
    c.move_right();
    assert_eq!(c.cursor(), "a中".len());
    assert!(c.selection().is_none());
}

#[test]
fn typing_pasting_newline_and_delete_replace_selection_in_one_undo_step() {
    for operation in 0..4 {
        let mut c = draft("one中two");
        c.place_cursor(3, false);
        c.place_cursor(6, true);
        match operation {
            0 => c.type_char('X'),
            1 => c.insert("X\nY"),
            2 => c.newline(),
            _ => c.backspace(),
        }
        assert_eq!(
            c.text(),
            ["oneXtwo", "oneX\nYtwo", "one\ntwo", "onetwo"][operation]
        );
        assert!(c.selection().is_none());
        assert!(c.undo());
        assert_eq!(c.text(), "one中two");
        assert_eq!(c.selected_text(), Some("中"));
        assert!(c.redo());
        assert!(c.selection().is_none());
    }
}

#[test]
fn paste_payload_is_selected_replaced_and_restored_as_an_atomic_chip() {
    let mut c = draft("prefix ");
    let payload = "line\n".repeat(12);
    c.insert(&payload);
    c.move_selection(true, Composer::move_left);
    assert_eq!(c.selected_text(), Some(payload.as_str()));
    c.type_char('X');
    assert_eq!(c.text(), "prefix X");
    assert!(c.pastes().is_empty());
    c.undo();
    assert_eq!(c.pastes().len(), 1);
    assert_eq!(c.selected_text(), Some(payload.as_str()));
}

#[test]
fn mouse_offsets_snap_to_clusters_and_vertical_shift_does_not_recall_history() {
    let mut c = draft("a中e\u{301}\nlast");
    c.place_cursor(2, false); // middle of a Chinese UTF-8 scalar
    assert_eq!(c.cursor(), 1);
    c.place_cursor(5, false); // middle of e + combining acute
    assert_eq!(c.cursor(), "a中".len());
    c.move_selection(true, |c| {
        c.move_down();
    });
    assert!(c.selected_text().unwrap().contains('\n'));
    c.set_text("fresh");
    assert!(c.selection().is_none());
    c.place_cursor(0, false);
    c.place_cursor(5, true);
    c.submit();
    assert!(c.selection().is_none());
    c.history_prev();
    assert!(c.selection().is_none());
}
