use std::time::Duration;

use crossterm::event::KeyCode;
use serde_json::{Value, json};

use super::harness::*;
use crate::protocol::ToolRef;
use crate::transcript::{Block, ToolCall};
use crate::ui::theme::{Palette, Styles};

fn record(change_ref: &str, path: &str, kind: &str) -> Value {
    json!({"change_ref": change_ref, "path": path, "kind": kind, "details_available": true})
}

/// One record's diff: `lines` are `(kind, text)` with `kind` `+` or `-`.
fn diff(path: &str, lines: &[(char, &str)]) -> Value {
    let mut old = 0;
    let mut new = 0;
    let rows: Vec<Value> = lines
        .iter()
        .map(|(kind, text)| {
            if *kind == '+' {
                new += 1;
                json!({"kind": "added", "new_index": new - 1, "text": text, "line_complete": true})
            } else {
                old += 1;
                json!({"kind": "removed", "old_index": old - 1, "text": text, "line_complete": true})
            }
        })
        .collect();
    json!({"path": path, "complete": true, "binary": false, "stale": false, "truncated": false,
           "hunks": [
        {"old_start": 0, "old_count": old, "new_start": 0, "new_count": new, "lines": rows}]})
}

async fn diff_view(records: Vec<Value>) -> Harness {
    let h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("changes.list", json!({"records": records, "warnings": []}));
    h
}

fn open_diff(h: &mut Harness) {
    h.type_text("/diff");
    h.press(KeyCode::Enter);
}

fn rows_naming(screen: &str, path: &str) -> Vec<String> {
    screen
        .lines()
        .map(str::trim_start)
        .filter(|row| row.contains(path) && !row.starts_with('←'))
        .map(str::to_owned)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_diff_view_lists_one_row_per_path_and_counts_every_record() {
    let mut h = diff_view(vec![
        record("r1", "app.py", "modified"),
        record("r2", "app.py", "modified"),
        record("r3", "notes.txt", "added"),
    ])
    .await;
    let mut first = diff("app.py", &[('-', "old"), ('+', "new")]);
    first["tool_ref"] = json!({"session_id": "s1", "loop_id": "l1", "request_index": 0,
                               "tool_call_id": "c1"});
    h.fake.script_sequence(
        "changes.diff",
        vec![
            first,
            diff("app.py", &[('+', "more")]),
            diff("notes.txt", &[('+', "1"), ('+', "2"), ('+', "3")]),
        ],
    );
    let edit = ToolCall::new(
        ToolRef {
            session_id: "s1".into(),
            loop_id: "l1".into(),
            request_index: 0,
            tool_call_id: "c1".into(),
        },
        "edit",
    );
    let session = h.app.session.as_mut().unwrap();
    session.transcript.push(Block::Tool(Box::new(edit)));

    open_diff(&mut h);
    h.settle().await;
    let screen = h.screen(100, 30);
    assert!(screen.starts_with(" ← Git diff · app.py"), "{screen}");
    let app_rows = rows_naming(&screen, "app.py");
    assert_eq!(app_rows.len(), 1, "{screen}");
    assert!(app_rows[0].starts_with("M +2 −1 app.py"), "{screen}");
    assert!(rows_naming(&screen, "notes.txt")[0].starts_with("A +3 −0 notes.txt"));
    for row in [
        "── edit · change 1 of 2 ──",
        "@@ -1,1 +1,1 @@",
        "── change 2 of 2 ──",
        "@@ -0,0 +1,1 @@",
    ] {
        assert!(screen.contains(row), "{row}:\n{screen}");
    }
    let reads: Vec<Value> = h
        .calls("changes.diff")
        .iter()
        .map(|params| params["change_ref"].clone())
        .collect();
    assert_eq!(reads, ["r1", "r2", "r3"], "every record is read once");
}

#[tokio::test(start_paused = true)]
async fn a_late_answer_lands_on_its_own_row_and_never_paints_another_file() {
    let mut h = diff_view(vec![
        record("ra", "a.py", "modified"),
        record("rb", "b.py", "modified"),
    ])
    .await;
    h.fake.script_sequence_later(
        "changes.diff",
        vec![
            (
                diff("a.py", &[('+', "a1"), ('+', "a2"), ('-', "a0")]),
                Duration::from_millis(400),
            ),
            (diff("b.py", &[('+', "b1")]), Duration::ZERO),
        ],
    );
    open_diff(&mut h);
    h.run_for(Duration::from_millis(100)).await;
    let screen = h.screen(100, 30);
    assert!(
        rows_naming(&screen, "a.py")[0].starts_with("M ·"),
        "{screen}"
    );

    h.press(KeyCode::Down);
    h.settle().await;
    let screen = h.screen(100, 30);
    assert!(screen.starts_with(" ← Git diff · b.py"), "{screen}");
    assert!(
        rows_naming(&screen, "a.py")[0].starts_with("M +2 −1 a.py"),
        "{screen}"
    );
    assert!(screen.contains("+b1"), "{screen}");
    assert!(!screen.contains("a1"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn selecting_another_file_clears_the_pane_before_its_answer() {
    let mut h = diff_view(vec![
        record("ra", "a.py", "modified"),
        record("rb", "b.py", "modified"),
    ])
    .await;
    h.fake.script_sequence_later(
        "changes.diff",
        vec![
            (diff("a.py", &[('+', "a1")]), Duration::ZERO),
            (diff("b.py", &[('+', "b1")]), Duration::from_millis(600)),
        ],
    );
    open_diff(&mut h);
    h.run_for(Duration::from_millis(100)).await;
    assert!(h.screen(100, 30).contains("+a1"));

    h.press(KeyCode::Down);
    let screen = h.screen(100, 30);
    assert!(screen.starts_with(" ← Git diff · b.py"), "{screen}");
    assert!(!screen.contains("a1"), "{screen}");
    h.settle().await;
    assert!(h.screen(100, 30).contains("+b1"));
    assert_eq!(
        h.calls("changes.diff").len(),
        2,
        "the row in flight is not read twice"
    );
}

#[tokio::test(start_paused = true)]
async fn slash_diff_lists_the_working_tree_without_a_turn() {
    let mut h = diff_view(vec![
        record("workspace:tracked", "a.py", "modified"),
        record("workspace:untracked", "new.txt", "added"),
    ])
    .await;
    h.fake.script_sequence(
        "changes.diff",
        vec![
            diff("a.py", &[('-', "old"), ('+', "new")]),
            diff("new.txt", &[('+', "untracked")]),
        ],
    );
    h.type_text("/diff");
    h.press(KeyCode::Enter);
    h.settle().await;
    let lists = h.calls("changes.list");
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0]["scope"], "workspace");
    assert_eq!(lists[0]["session_id"], "s1");
    let screen = h.screen(100, 20);
    assert!(screen.starts_with(" ← Git diff · a.py"), "{screen}");
    assert!(screen.contains("M +1 −1 a.py"), "{screen}");
    assert!(screen.contains("A +1 −0 new.txt"), "{screen}");
    let reads = h.calls("changes.diff");
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0]["change_ref"], "workspace:tracked");
    assert_eq!(reads[1]["change_ref"], "workspace:untracked");
    assert!(
        reads
            .iter()
            .all(|params| params.get("comparison").is_none())
    );
}

#[tokio::test(start_paused = true)]
async fn clicking_a_turn_end_line_lists_the_working_tree() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app
        .session
        .as_mut()
        .unwrap()
        .transcript
        .push(Block::TurnEnd {
            loop_id: "l7".into(),
            outcome: crate::protocol::LoopOutcome::Completed,
            duration: None,
            requests: 1,
            tools: 0,
            usage: Default::default(),
            changes: Some(crate::transcript::ChangeSummary {
                files: 1,
                lines: Some((2, 1)),
            }),
        });
    let screen = h.screen(100, 20);
    assert!(
        screen.contains("1 file changed (+2 −1) · /diff"),
        "{screen}"
    );
    let row = screen
        .lines()
        .position(|line| line.contains("completed"))
        .unwrap() as u16;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.on_mouse(MouseEvent {
            kind,
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
    h.settle().await;
    let lists = h.calls("changes.list");
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0]["scope"], "workspace");
    assert!(h.screen(100, 20).starts_with(" ← Git diff"));
}

#[tokio::test(start_paused = true)]
async fn tab_does_not_change_the_diff_scope() {
    let mut h = diff_view(vec![record("workspace:a", "a.py", "modified")]).await;
    h.fake.script("changes.diff", diff("a.py", &[('+', "new")]));
    open_diff(&mut h);
    h.settle().await;
    let before = h.screen(100, 20);
    for _ in 0..3 {
        h.press(KeyCode::Tab);
        h.settle().await;
        assert_eq!(h.screen(100, 20), before);
    }
    assert!(!before.contains("tab scope"), "{before}");
    assert_eq!(h.calls("changes.list").len(), 1);
    assert_eq!(h.calls("changes.list")[0]["scope"], "workspace");
}

#[tokio::test(start_paused = true)]
async fn an_empty_working_tree_says_so() {
    let mut h = diff_view(Vec::new()).await;
    open_diff(&mut h);
    h.settle().await;
    let screen = h.screen(100, 20);
    assert!(
        screen.contains("No changes in the working tree"),
        "{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unavailable_working_tree_is_not_reported_as_clean() {
    for (warnings, expected) in [
        (
            json!(["no_repository", "status_incomplete"]),
            "Git working tree unavailable.",
        ),
        (json!(["no_repository"]), "Git working tree unavailable."),
        (json!(["status_incomplete"]), "Git status is incomplete."),
    ] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        h.fake
            .script("changes.list", json!({"records": [], "warnings": warnings}));
        open_diff(&mut h);
        h.settle().await;
        let screen = h.screen(100, 20);
        assert!(screen.contains(expected), "{screen}");
        assert!(
            !screen.contains("No changes in the working tree"),
            "{screen}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_git_diff_shows_the_agents_list_error() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error(
        "changes.list",
        crate::protocol::RESOURCE_EXHAUSTED,
        "query limit reached",
        "resource_exhausted",
    );
    open_diff(&mut h);
    h.settle().await;
    let screen = h.screen(100, 20);
    assert!(screen.contains("query limit reached"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn switching_the_theme_recolors_an_open_diff() {
    let mut h = diff_view(vec![record("r1", "a.py", "modified")]).await;
    h.fake.script("changes.diff", diff("a.py", &[('+', "new")]));
    open_diff(&mut h);
    h.settle().await;
    let dark = Palette::dark();
    assert_eq!(h.fg_of("+new"), dark.added);

    let light = Palette::light();
    h.app.styles = Styles::of(light);
    assert_eq!(h.fg_of("+new"), light.added);
    assert_ne!(dark.added, light.added);
}

#[tokio::test(start_paused = true)]
async fn switching_the_theme_keeps_the_context_view() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "session.context",
        json!({"session_id": "s1", "coverage":
            {"covered_loop_count": 6, "covered_item_count": 12, "retained_item_count": 3}}),
    );
    h.type_text("/context");
    h.press(KeyCode::Enter);
    h.settle().await;
    h.app.styles = Styles::of(Palette::light());
    let screen = h.screen(100, 20);
    assert!(screen.contains("Summary covers 6 turns"), "{screen}");
}
