use super::*;

const CLUSTERS: &[&str] = &["e\u{301}", "👩🏽‍💻", "🇨🇳", "❤️", "1️⃣", "क्‍ष", "中", "\u{301}"];

fn assert_valid(composer: &Composer) {
    let (row, column) = composer.cursor();
    assert_eq!(
        grapheme_column_bounds(&composer.lines()[row], column),
        (column, column),
        "cursor splits an extended grapheme"
    );
    assert_eq!(composer.byte_len(), composer.content().len());
}

#[test]
fn backspace_and_forward_delete_remove_one_grapheme_with_one_undo() {
    // A standalone leading combining mark has no preceding base; test it at
    // column zero separately below rather than making it part of prefix A.
    for &cluster in &CLUSTERS[..CLUSTERS.len() - 1] {
        let original = format!("A{cluster}B\n第二行keep");
        let first_line_len = 2 + cluster.chars().count();
        let mut composer = Composer::new();
        composer.set_text(&original);
        composer.move_to(0, first_line_len);
        composer.backspace();
        assert_eq!(composer.content(), format!("A{cluster}\n第二行keep"));
        composer.backspace();
        assert_eq!(composer.content(), "A\n第二行keep");
        assert_eq!(composer.cursor(), (0, 1));
        assert_valid(&composer);
        composer.undo();
        assert_eq!(composer.content(), format!("A{cluster}\n第二行keep"));
        assert_eq!(composer.cursor(), (0, first_line_len - 1));
        composer.undo();
        assert_eq!(composer.content(), original);
        assert_eq!(composer.cursor(), (0, first_line_len));
        composer.redo();
        composer.redo();
        assert_eq!(composer.content(), "A\n第二行keep");

        composer.set_text(&original);
        composer.move_to(0, 1);
        composer.delete();
        assert_eq!(composer.content(), "AB\n第二行keep");
        assert_eq!(composer.cursor(), (0, 1));
        composer.undo();
        assert_eq!(composer.content(), original);
        assert_eq!(composer.cursor(), (0, 1));
        composer.redo();
        assert_eq!(composer.content(), "AB\n第二行keep");
        assert_valid(&composer);
    }
}

#[test]
fn a_leading_combining_cluster_and_document_edges_are_safe() {
    let mut composer = Composer::new();
    composer.set_text("\u{301}\u{308}B");
    composer.move_to(0, 0);
    composer.backspace();
    assert_eq!(composer.content(), "\u{301}\u{308}B");
    composer.delete();
    assert_eq!(composer.content(), "B");
    composer.delete();
    composer.delete();
    composer.backspace();
    assert_eq!(composer.content(), "");
    assert_valid(&composer);
}

#[test]
fn every_requested_scalar_position_lands_on_a_grapheme_boundary() {
    for &cluster in &CLUSTERS[..CLUSTERS.len() - 1] {
        let text = format!("A{cluster}B");
        let mut composer = Composer::new();
        composer.set_text(&text);
        for column in 2..1 + cluster.chars().count() {
            composer.move_to(0, column);
            assert_eq!(composer.cursor(), (0, 1));
            composer.move_to_display(0, column);
            assert_eq!(composer.cursor(), (0, 1));
            assert_valid(&composer);
        }
        composer.move_to(0, 1);
        composer.move_right();
        assert_eq!(composer.cursor(), (0, 1 + cluster.chars().count()));
        composer.move_left();
        assert_eq!(composer.cursor(), (0, 1));
    }
}

#[test]
fn vertical_movement_snaps_interior_scalar_columns() {
    let mut composer = Composer::new();
    composer.set_text("abcde\nA👩🏽‍💻B\nabcde");
    composer.move_to(0, 3);
    composer.move_down();
    assert_eq!(composer.cursor(), (1, 1));
    composer.move_to(2, 3);
    composer.move_up();
    assert_eq!(composer.cursor(), (1, 1));
    composer.delete();
    assert_eq!(composer.content(), "abcde\nAB\nabcde");
    composer.undo();
    assert_eq!(composer.content(), "abcde\nA👩🏽‍💻B\nabcde");
    assert_valid(&composer);
}

#[test]
fn insertion_merging_graphemes_preserves_exact_undo_and_redo_positions() {
    let mut composer = Composer::new();
    composer.set_text("👩💻");
    composer.move_to(0, 1);
    composer.type_char('\u{200d}');
    assert_eq!(composer.content(), "👩‍💻");
    assert_eq!(composer.cursor(), (0, 3));
    assert_valid(&composer);
    composer.undo();
    assert_eq!(composer.content(), "👩💻");
    assert_eq!(composer.cursor(), (0, 1));
    composer.redo();
    assert_eq!(composer.cursor(), (0, 3));
    composer.backspace();
    assert!(composer.is_empty());
    composer.undo();
    assert_eq!(composer.content(), "👩‍💻");
    assert_valid(&composer);

    composer.set_text("🇦🇧🇨");
    composer.move_to(0, 2);
    composer.type_char('🇽');
    assert_eq!(composer.content(), "🇦🇧🇽🇨");
    assert_eq!(composer.cursor(), (0, 4));
    composer.undo();
    assert_eq!(composer.cursor(), (0, 2));
    composer.redo();
    assert_eq!(composer.cursor(), (0, 4));
    composer.backspace();
    assert_eq!(composer.content(), "🇦🇧");
    assert_valid(&composer);
}

#[test]
fn deleting_newlines_or_separators_cannot_leave_an_interior_cursor() {
    for forward in [false, true] {
        let mut composer = Composer::new();
        composer.set_text("a\n\u{301}b");
        let before = if forward { (0, 1) } else { (1, 0) };
        composer.move_to(before.0, before.1);
        if forward {
            composer.delete();
        } else {
            composer.backspace();
        }
        assert_eq!(composer.content(), "a\u{301}b");
        assert_eq!(composer.cursor(), (0, 0));
        assert_valid(&composer);
        composer.undo();
        assert_eq!(composer.content(), "a\n\u{301}b");
        assert_eq!(composer.cursor(), before);
        composer.redo();
        assert_eq!(composer.cursor(), (0, 0));
        assert_valid(&composer);
    }
    let mut composer = Composer::new();
    composer.set_text("🇦x🇧");
    composer.move_to(0, 1);
    composer.delete();
    assert_eq!(composer.content(), "🇦🇧");
    assert_eq!(composer.cursor(), (0, 0));
    composer.undo();
    assert_eq!(composer.cursor(), (0, 1));
    assert_valid(&composer);
}

#[test]
fn completion_expands_nonempty_ranges_and_keeps_zero_width_insertions() {
    let mut composer = Composer::new();
    composer.set_text("A👩🏽‍💻B");
    composer.replace_range(0, 2, 3, "X");
    assert_eq!(composer.content(), "AXB");
    composer.undo();
    assert_eq!(composer.content(), "AB");
    composer.undo();
    assert_eq!(composer.content(), "A👩🏽‍💻B");
    assert_valid(&composer);
    composer.replace_range(0, 2, 2, "X");
    assert_eq!(composer.content(), "AX👩🏽‍💻B");
    assert_valid(&composer);

    // The intermediate deletion joins two regional indicators. Insertion
    // must still occur at the original scalar boundary, between them.
    composer.set_text("🇦x🇧");
    composer.replace_range(0, 1, 2, "y");
    assert_eq!(composer.content(), "🇦y🇧");
    assert_valid(&composer);
}

#[test]
fn paste_edges_that_join_neighbor_graphemes_expand_and_undo_atomically() {
    let mut composer = Composer::new();
    let pasted = format!("{}a", "x".repeat(1_000));
    composer.insert_paste(&pasted);
    assert_eq!(composer.paste_ranges().len(), 1);
    composer.type_char('\u{301}');
    assert!(composer.paste_ranges().is_empty());
    assert_eq!(composer.content(), format!("{pasted}\u{301}"));
    composer.undo();
    assert_eq!(composer.paste_ranges().len(), 1);
    assert_eq!(composer.display_content(), "[paste #1 1001 chars]");
    composer.redo();
    assert!(composer.paste_ranges().is_empty());
    composer.backspace();
    assert_eq!(composer.content(), "x".repeat(1_000));
    assert_valid(&composer);

    composer.set_text("a");
    composer.insert_paste(&format!("\u{301}{}", "x".repeat(1_001)));
    assert!(composer.paste_ranges().is_empty());
    composer.undo();
    assert_eq!(composer.content(), "a");
    assert_valid(&composer);
}

#[test]
fn large_coordinates_do_not_wrap_at_u16_and_clear_stays_one_undo() {
    let mut composer = Composer::new();
    let original = format!("{}👩🏽‍💻B", "x".repeat(70_000));
    composer.set_text(&original);
    composer.move_to(0, 70_002);
    assert_eq!(composer.cursor(), (0, 70_000));
    composer.move_right();
    assert_eq!(composer.cursor(), (0, 70_004));
    composer.backspace();
    assert_eq!(composer.content(), format!("{}B", "x".repeat(70_000)));
    composer.undo();
    assert_eq!(composer.content(), original);
    assert_eq!(composer.cursor(), (0, 70_004));
    composer.clear_undoable();
    composer.undo();
    assert_eq!(composer.cursor(), (0, 70_004));
    assert_eq!(composer.content(), original);
    assert_valid(&composer);

    let original = format!("{}A👩🏽‍💻B", "x\n".repeat(70_000));
    composer.set_text(&original);
    composer.move_to(70_000, 3);
    assert_eq!(composer.cursor(), (70_000, 1));
    composer.delete();
    assert!(composer.content().ends_with("AB"));
    composer.undo();
    assert_eq!(composer.content(), original);
    assert_eq!(composer.cursor(), (70_000, 1));
    assert_valid(&composer);
}

#[test]
fn budget_truncation_keeps_only_complete_graphemes() {
    let mut composer = Composer::new();
    let prefix = "x".repeat(MAX_COMPOSER_BYTES - 1);
    composer.set_text(&format!("{prefix}e\u{301}tail"));
    assert_eq!(composer.content(), prefix);
    assert_valid(&composer);
    let prefix = "x".repeat(MAX_COMPOSER_BYTES - 6);
    composer.set_text(&format!("{prefix}👩🏽‍💻tail"));
    assert_eq!(composer.content(), prefix);
    assert_valid(&composer);
}
