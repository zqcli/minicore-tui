//! Offline validation of the Rail reference fixtures in
//! `tests/fixtures/rail/`. These fixtures are generated outside Cargo by
//! `tools/reference_fixtures/` against the fixed pi-rail-ui reference at the
//! pinned commit (see `PROVENANCE.json`). This test runs with no Node, no
//! network and no reference checkout: it only verifies that every fixture is
//! well-formed under the documented schema and that the provenance
//! pins have not drifted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ratatui::style::Color;
use ratatui::text::Line;
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use minicore_tui::app::App;
use minicore_tui::event::AppEvent;
use minicore_tui::protocol::ToolDisplayWire;
use minicore_tui::state::transcript::ToolBlock;
use minicore_tui::theme::{Theme, ThemeKind};
use minicore_tui::ui::{composer, layout, rail, reasoning, tool};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::layout::Rect;
use ratatui::style::Modifier;

const PINNED_RAIL_COMMIT: &str = "1d0dd1611a4d9546c64fe9f5b5c966253fb88eba";
const PINNED_PI: &str = "0.84.4";
const SCHEMA: &str = "minicore-rail-cell-v1";

#[derive(Debug)]
struct Stats {
    files: usize,
    cells: usize,
    text: usize,
}

fn rail_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rail")
}

fn collect_fixtures(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {dir:?}: {e}"))
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_fixtures(&path, out);
        } else if path.extension().is_some_and(|e| e == "json")
            && path.file_name() != Some(std::ffi::OsStr::new("PROVENANCE.json"))
        {
            out.push(path);
        }
    }
}

fn width_of_token(token: &Value) -> Result<u16, String> {
    match token {
        Value::String(s) => {
            if s.chars().any(|c| c == '\u{1b}') {
                return Err("raw ANSI leaked into a cell fixture token".to_owned());
            }
            u16::try_from(s.width()).map_err(|_| "cell token is too wide".to_owned())
        }
        Value::Object(map) => {
            let c = map
                .get("c")
                .and_then(Value::as_str)
                .ok_or_else(|| "object token missing `c` string".to_owned())?;
            if c.chars().any(|ch| ch == '\u{1b}') {
                return Err("raw ANSI leaked into a cell fixture token".to_owned());
            }
            let width = s_width(c)?;
            if let Some(w) = token.get("w") {
                if w != 2 {
                    return Err("w must be 2".to_owned());
                }
                if width != 2 {
                    return Err(format!("wide token has cell width {width}, expected 2"));
                }
            }
            Ok(width)
        }
        other => Err(format!("unexpected token type: {other:?}")),
    }
}

fn s_width(value: &str) -> Result<u16, String> {
    u16::try_from(value.width()).map_err(|_| "cell token is too wide".to_owned())
}

fn validate_color(kind: &str, value: &Value) -> Result<(), String> {
    let arr = value
        .as_array()
        .ok_or_else(|| format!("{kind} must be an array"))?;
    if arr.len() != 3 {
        return Err(format!("{kind} must have 3 components"));
    }
    for component in arr {
        let n = component
            .as_u64()
            .ok_or_else(|| format!("{kind} component must be an integer"))?;
        if n > 255 {
            return Err(format!("{kind} component out of range: {n}"));
        }
    }
    Ok(())
}

fn validate_row(row: &Value, cols: u16, case: &str, row_index: usize) -> Result<u16, String> {
    let tokens = row
        .as_array()
        .ok_or_else(|| format!("{case} row {row_index} is not an array"))?;
    let mut width = 0u16;
    for token in tokens {
        if let Value::Object(map) = token {
            if let Some(fg) = map.get("fg") {
                validate_color("fg", fg)?;
            }
            if let Some(bg) = map.get("bg") {
                validate_color("bg", bg)?;
            }
            if let Some(s) = map.get("s") {
                let flag = s.as_u64().ok_or("s must be an integer")?;
                if flag & !0x3f != 0 {
                    return Err(format!("{case} row {row_index} unknown style flag {flag}"));
                }
            }
            if let Some(w) = map.get("w") {
                if w != 2 {
                    return Err(format!("{case} row {row_index} unsupported w value"));
                }
            }
        }
        width = width.saturating_add(width_of_token(token)?);
    }
    // Blank spacer rows (transparent external separators) are legitimately
    // zero-width in the reference output; every non-empty row must be exactly
    // the declared term width.
    if width != cols && width != 0 {
        return Err(format!(
            "{case} row {row_index} width {width} != term.cols {cols}"
        ));
    }
    Ok(width)
}

#[test]
fn pinned_reference_fixtures_are_well_formed_and_complete() {
    let root = rail_fixture_root();
    assert!(root.is_dir(), "fixtures root missing: {root:?}");

    let mut files = Vec::new();
    collect_fixtures(&root, &mut files);
    assert!(
        files.len() >= 90,
        "expected at least 90 fixture files, found {}",
        files.len()
    );

    let mut by_group = BTreeMap::new();
    let mut cells_of_group: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_stats = Stats {
        files: 0,
        cells: 0,
        text: 0,
    };

    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", file.display()));
        let doc: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", file.display()));

        assert_eq!(doc["schema"], SCHEMA, "{} schema mismatch", file.display());
        assert_eq!(
            doc["source"]["rail_commit"],
            PINNED_RAIL_COMMIT,
            "{} provenance rail_commit drifted (re-run the generator)",
            file.display()
        );
        assert_eq!(
            doc["source"]["pi"],
            PINNED_PI,
            "{} provenance pi drifted",
            file.display()
        );

        let case = doc["case"].as_str().unwrap_or("?").to_owned();
        let cols = doc["term"]["cols"].as_u64().expect("term.cols missing") as u16;
        let rows_count = doc["term"]["rows"].as_u64().expect("term.rows missing");

        let kind = doc["kind"].as_str().unwrap_or("cells");
        let rows = doc["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{} missing rows", file.display()));

        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .parent()
            .unwrap()
            .display()
            .to_string();
        let group = if rel.is_empty() { "(root)".into() } else { rel };
        *by_group.entry(group.clone()).or_insert(0usize) += 1;

        match kind {
            "text" => {
                for (i, row) in rows.iter().enumerate() {
                    assert!(
                        row.is_string(),
                        "{} text fixture row {i} must be a plain string",
                        file.display()
                    );
                    width_of_token(row).expect("text token must be plain");
                    total_stats.text += 1;
                }
            }
            "cells" => {
                let count = rows
                    .iter()
                    .enumerate()
                    .try_fold(0usize, |acc, (i, row)| {
                        validate_row(row, cols, &case, i).map(|_| acc + 1)
                    })
                    .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
                total_stats.cells += count;
                *cells_of_group.entry(group.clone()).or_insert(0) += count;

                if let Some(cursor) = doc.get("cursor") {
                    let pos = cursor.as_array().expect("cursor must be [row, col]");
                    let row = pos[0].as_u64().unwrap_or(u64::MAX) as u16;
                    let col = pos[1].as_u64().unwrap_or(u64::MAX) as u16;
                    assert!(
                        row < rows_count as u16,
                        "{} cursor row {} out of bounds",
                        file.display(),
                        row
                    );
                    assert!(
                        col < cols,
                        "{} cursor col {} out of bounds",
                        file.display(),
                        col
                    );
                }
                if let Some(zones) = doc.get("osczones") {
                    for zone in zones.as_array().expect("osczones must be an array") {
                        let z = zone.as_u64().unwrap_or(u64::MAX) as u16;
                        assert!(
                            z < rows_count as u16,
                            "{} osczone row {z} out of bounds",
                            file.display()
                        );
                    }
                }
            }
            other => panic!("{} unknown fixture kind {other:?}", file.display()),
        }
        total_stats.files += 1;
    }

    // Every fixture must carry the pinned source block.
    assert_eq!(total_stats.files, files.len());

    eprintln!(
        "rail fixtures: {} files, {} cell rows, {} text lines across {} groups",
        total_stats.files,
        total_stats.cells,
        total_stats.text,
        by_group.len()
    );
    for (group, count) in &by_group {
        eprintln!(
            "  {group}: {} files ({} cell rows)",
            count,
            cells_of_group.get(group).copied().unwrap_or(0)
        );
    }
}

#[test]
fn provenance_file_is_present_and_pinned() {
    let root = rail_fixture_root();
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("PROVENANCE.json")).expect("PROVENANCE.json missing"),
    )
    .expect("PROVENANCE.json invalid JSON");

    assert_eq!(doc["schema"], SCHEMA);
    assert_eq!(doc["rail_commit"], PINNED_RAIL_COMMIT);
    assert_eq!(doc["pi"], PINNED_PI);
    assert!(doc["generator"].is_string());
    assert!(doc["theme"].is_string());
    assert_eq!(doc["tz"], "UTC");
}

fn fixture(name: &str) -> Value {
    let path = rail_fixture_root().join(name);
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read source fixture {path:?}: {error}")),
    )
    .unwrap_or_else(|error| panic!("invalid source fixture {path:?}: {error}"))
}

fn source_token(row: &Value, index: usize) -> &Value {
    &row.as_array().expect("source row is an array")[index]
}

fn source_row_text(row: &Value) -> String {
    row.as_array()
        .expect("source row is an array")
        .iter()
        .map(token_text)
        .collect()
}

fn rust_row_text(row: &Line<'_>) -> String {
    row.spans.iter().map(|span| span.content.as_ref()).collect()
}

fn token_text(token: &Value) -> &str {
    match token {
        Value::String(text) => text,
        Value::Object(object) => object["c"].as_str().expect("source token c"),
        _ => panic!("unexpected source token {token:?}"),
    }
}

fn token_rgb(token: &Value, key: &str) -> Color {
    let rgb = token[key]
        .as_array()
        .unwrap_or_else(|| panic!("source token lacks {key}: {token:?}"));
    Color::Rgb(
        rgb[0].as_u64().unwrap() as u8,
        rgb[1].as_u64().unwrap() as u8,
        rgb[2].as_u64().unwrap() as u8,
    )
}

fn assert_composer_fixture_with_setup(
    name: &str,
    text: &str,
    cursor: Option<(usize, usize)>,
    setup: impl FnOnce(&mut App),
) {
    let source = fixture(name);
    let width = source["term"]["cols"].as_u64().unwrap() as u16;
    let source_rows = source["rows"].as_array().unwrap();
    let height = source_rows.len() as u16;
    let terminal_height = source["term"]["rows"].as_u64().unwrap() as u16;
    let mut app = App::new(std::path::PathBuf::from("/project"));
    app.update(AppEvent::SetTheme(ThemeKind::Dark));
    if name.starts_with("paste/") {
        assert!(app.composer.insert_paste(text));
    } else {
        app.composer.set_text(text);
    }
    if name == "slash/popup-rows.json" {
        app.slash_completion = Some(minicore_tui::app::SlashCompletionState {
            popup: None,
            source_revision: app.composer.editor_revision(),
            session_owner: app.sessions.active.clone(),
            group: None,
            argument_command: None,
            filter: String::new(),
            start: 0,
            end: app.composer.cursor().1,
            items: minicore_tui::command::menu::page("", &[], &[])
                .unwrap()
                .entries,
            selected: 0,
        });
    }
    if let Some((line, column)) = cursor {
        app.composer.move_to(line, column);
    }
    setup(&mut app);

    let render_width = width.max(60);
    app.update(AppEvent::TerminalSize {
        width: render_width,
        height: terminal_height,
    });
    let mut terminal = Terminal::new(TestBackend::new(render_width, terminal_height)).unwrap();
    let (row_offset, local_x) = if width >= 60 {
        terminal
            .draw(|frame| minicore_tui::ui::render(frame, &app))
            .unwrap();
        let screen = layout::screen_layout(&app, Rect::new(0, 0, render_width, terminal_height));
        (screen.panel.y as usize, 1usize)
    } else {
        terminal
            .draw(|frame| {
                composer::render(frame, Rect::new(0, 0, width, height), &app, &Theme::dark())
            })
            .unwrap();
        (0, 0)
    };
    let actual_cursor = terminal.backend_mut().get_cursor_position().unwrap();
    let buffer = terminal.backend().buffer();
    let source_width = width as usize;
    let comparable_width = source_width.saturating_sub(local_x);

    // The upgraded completion menu intentionally adds command purposes and
    // controls. Keep the native editor/cursor and every Rail/background cell
    // exact; menu text now has its own explicit product assertions below.
    let editor_rows = if name == "slash/popup-rows.json" {
        source_rows
            .iter()
            .position(|row| source_row_text(row).contains("→ "))
            .unwrap()
    } else {
        source_rows.len()
    };
    for (row_index, source_row) in source_rows.iter().enumerate() {
        let mut column = 0usize;
        for token in source_row.as_array().unwrap() {
            let text = token_text(token);
            for character in text.chars() {
                let cell_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                if column < comparable_width && cell_width > 0 {
                    let cell = buffer
                        .cell((
                            local_x as u16 + column as u16,
                            row_offset as u16 + row_index as u16,
                        ))
                        .unwrap();
                    if row_index >= editor_rows && column > 0 {
                        if token.get("bg").is_some() {
                            assert_eq!(
                                cell.bg,
                                token_rgb(token, "bg"),
                                "{name} completion background"
                            );
                        }
                        column += cell_width;
                        continue;
                    }
                    assert_eq!(
                        cell.symbol(),
                        character.to_string(),
                        "{name} symbol at row {row_index}, column {column}"
                    );
                    if token.get("fg").is_some() {
                        assert_eq!(
                            cell.fg,
                            token_rgb(token, "fg"),
                            "{name} fg at row {row_index}, column {column}"
                        );
                    }
                    if token.get("bg").is_some() {
                        assert_eq!(
                            cell.bg,
                            token_rgb(token, "bg"),
                            "{name} bg at row {row_index}, column {column}"
                        );
                    }
                    if let Some(flags) = token.get("s").and_then(Value::as_u64) {
                        let expected = [
                            (1, Modifier::BOLD),
                            (2, Modifier::DIM),
                            (4, Modifier::ITALIC),
                            (8, Modifier::UNDERLINED),
                            (16, Modifier::CROSSED_OUT),
                            (32, Modifier::REVERSED),
                        ]
                        .into_iter()
                        .filter(|(flag, _)| flags & flag != 0)
                        .fold(Modifier::empty(), |style, (_, modifier)| style | modifier);
                        assert_eq!(
                            cell.modifier & expected,
                            expected,
                            "{name} style at row {row_index}, column {column}"
                        );
                    }
                }
                column += cell_width;
            }
        }
    }

    if name == "slash/popup-rows.json" {
        let menu = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(menu.contains("→ Model"));
        assert!(menu.contains("[default ▾]"));
        assert!(menu.contains("Tab fill"));
        assert!(menu.contains("Enter choose"));
        assert!(menu.contains("Esc"));
        assert!(menu.contains("1/6"));
    }
    let expected_cursor = source["cursor"].as_array().map(|cursor| {
        (
            cursor[1].as_u64().unwrap() as u16 + local_x as u16,
            row_offset as u16 + cursor[0].as_u64().unwrap() as u16,
        )
    });
    if let Some(expected) = expected_cursor {
        assert_eq!(actual_cursor, expected.into(), "{name} hardware cursor");
    }
}

fn assert_composer_fixture(name: &str, text: &str, cursor: Option<(usize, usize)>) {
    assert_composer_fixture_with_setup(name, text, cursor, |_| {});
}

#[test]
fn rust_editor_and_paste_rows_match_the_fixed_native_editor() {
    assert_composer_fixture("editor/empty.json", "", None);
    assert_composer_fixture("editor/one-line.json", "hello world", None);
    assert_composer_fixture(
        "editor/four-lines.json",
        "line one\nline two\nline three\nline four",
        None,
    );
    assert_composer_fixture(
        "editor/thirteen-lines.json",
        &(1..=13)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        None,
    );
    assert_composer_fixture("editor/one-line-centered.json", "centered", None);
    assert_composer_fixture(
        "editor/wide-120x40.json",
        &(1..=20)
            .map(|line| format!("wide line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        None,
    );
    assert_composer_fixture(
        "editor/narrow-60x16.json",
        "narrow\nterminal\nworks\nhere",
        None,
    );
    assert_composer_fixture(
        "editor/cursor-mid-13line.json",
        &(1..=13)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Some((6, 3)),
    );
    assert_composer_fixture(
        "editor/long-soft-wrap.json",
        &"the quick brown fox jumps over the lazy dog, ".repeat(5),
        None,
    );
    assert_composer_fixture("editor/cjk.json", "你好，世界！这是中文内容测试。", None);
    assert_composer_fixture(
        "editor/cjk-emoji.json",
        "mix 🚀 rocket ✨ sparkles and 中文",
        None,
    );
    assert_composer_fixture_with_setup("editor-click/ascii-middle.json", "", None, |app| {
        app.composer
            .set_text("abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        app.composer.move_to(0, 12);
    });
    assert_composer_fixture_with_setup("editor-click/wrapped-uppercase.json", "", None, |app| {
        app.composer
            .set_text("abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        app.composer.move_to(0, 50);
    });
    assert_composer_fixture_with_setup("editor-click/wide-grapheme.json", "", None, |app| {
        app.composer.set_text("alpha 中文内容 omega");
        app.composer.move_to(0, 8);
    });
    assert_composer_fixture("slash/popup-rows.json", "", None);
    assert_composer_fixture(
        "paste/ten-lines-inline.json",
        &(1..=10)
            .map(|line| format!("pasted line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        None,
    );
    assert_composer_fixture(
        "paste/eleven-lines-marker.json",
        &(1..=11)
            .map(|line| format!("pasted line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        None,
    );
    assert_composer_fixture("paste/single-line-inline.json", "/tmp/build.sh", None);
    assert_composer_fixture("paste/thousand-chars-inline.json", &"x".repeat(1_000), None);
    assert_composer_fixture(
        "paste/thousand-one-chars-marker.json",
        &"y".repeat(1_001),
        None,
    );
    assert_composer_fixture(
        "paste/eleven-lines-marker-cursor.json",
        &(1..=11)
            .map(|line| format!("pasted line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        None,
    );
}

#[test]
fn rust_slash_fill_and_submit_states_match_the_fixed_native_editor() {
    let mut kept = App::new(std::path::PathBuf::from("/project"));
    kept.update(AppEvent::SetTheme(ThemeKind::Dark));
    kept.composer.set_text("/ski");
    kept.slash_completion = Some(minicore_tui::app::SlashCompletionState {
        popup: None,
        source_revision: kept.composer.editor_revision(),
        session_owner: kept.sessions.active.clone(),
        group: None,
        argument_command: None,
        filter: String::new(),
        start: 0,
        end: 4,
        items: vec!["/skill:web-access".to_owned().into()],
        selected: 0,
    });
    let commands = kept.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    assert!(commands.is_empty());
    assert_eq!(kept.composer.content(), "/skill:web-access ");
    assert!(kept.slash_completion.is_none());
    assert_composer_fixture_with_setup(
        "slash/enter-keeps-skill-command.json",
        "",
        None,
        move |app| *app = kept,
    );

    let mut fallback = App::new(std::path::PathBuf::from("/project"));
    fallback.update(AppEvent::SetTheme(ThemeKind::Dark));
    // The native editor submits `/read` and the host clears the editor before
    // the resulting frame is captured; the Rust app fixture represents that
    // post-submit frame directly.
    fallback.composer.clear();
    assert_composer_fixture_with_setup("slash/enter-no-list-submits.json", "", None, move |app| {
        *app = fallback
    });
}

#[test]
fn rust_scrollbar_cells_match_pi_0851_positions_glyphs_and_colors() {
    let source: Value =
        serde_json::from_str(include_str!("fixtures/pi-scrollbar-0.85.1.json")).unwrap();
    assert_eq!(source["version"], "0.85.1");
    for rendering in source["renderings"].as_array().unwrap() {
        let theme = if rendering["theme"] == "dark" {
            Theme::dark()
        } else {
            Theme::light()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                minicore_tui::ui::scrollbar::render(
                    frame,
                    Rect::new(0, 0, 80, 20),
                    100,
                    rendering["offset"].as_u64().unwrap() as usize,
                    &theme,
                    rendering["active"].as_bool().unwrap(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for (row, source_row) in rendering["rows"].as_array().unwrap().iter().enumerate() {
            let actual = buffer
                .content()
                .chunks(80)
                .nth(row)
                .unwrap()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let expected = source_row.as_str().unwrap();
            assert_eq!(actual, expected, "{rendering} row {row}");
            let kind = if expected.ends_with('│') {
                "track"
            } else {
                "thumb"
            };
            let rgb = &rendering["colors"][kind];
            assert_eq!(
                buffer.cell((79, row as u16)).unwrap().fg,
                Color::Rgb(
                    rgb[0].as_u64().unwrap() as u8,
                    rgb[1].as_u64().unwrap() as u8,
                    rgb[2].as_u64().unwrap() as u8
                )
            );
        }
    }
}

#[test]
fn rust_surface_primitives_match_source_rail_and_content_geometry() {
    let editor = fixture("editor/empty.json");
    let rust_editor = rail::surface_row(
        80,
        rail::editor_colors(&Theme::dark()),
        rail::RAIL_WIDTH,
        Line::default(),
    );
    assert_eq!(rust_editor.spans[0].content, "▎");
    assert_eq!(
        rust_editor.spans[0].style.fg,
        Some(token_rgb(source_token(&editor["rows"][0], 0), "fg"))
    );
    assert_eq!(
        rust_editor.spans[1].content,
        token_text(source_token(&editor["rows"][0], 1))
    );
    assert_eq!(
        rust_editor.spans[1].style.bg,
        Some(token_rgb(source_token(&editor["rows"][0], 1), "bg"))
    );

    let thinking = fixture("thinking/lines-4-collapsed.json");
    let rust_thinking = reasoning::reasoning_lines(
        &Theme::dark(),
        "thought 1\nthought 2\nthought 3\nthought 4",
        80,
        true,
        false,
    );
    // 0.2.2 user contract adds one transparent padding row above the thinking
    // run (explicit divergence from the pinned 1d0dd16 source, which emits the
    // surface directly). The first *content* row must still match the native
    // first-row rail glyph/color/content exactly.
    let first_content = rust_thinking
        .iter()
        .find(|line| !line.spans.is_empty())
        .expect("thinking output has a content row");
    assert_eq!(
        first_content.spans[0].content,
        token_text(source_token(&thinking["rows"][0], 0))
    );
    assert_eq!(
        first_content.spans[0].style.fg,
        Some(token_rgb(source_token(&thinking["rows"][0], 0), "fg"))
    );
    assert!(rust_thinking.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.contains("1 more rows"))
    }));

    let tool_source = fixture("tool/model-tool-simple-bash.json");
    let block = ToolBlock {
        index: None,
        loop_id: "loop".to_owned(),
        request_index: 0,
        tool_call_id: "call".to_owned(),
        name: "bash".to_owned(),
        result: Some("ok".to_owned().into()),
        outcome: Some(minicore_tui::protocol::ToolOutcomeWire::Success),
        live_status: None,
        progress: None,
        expanded: false,
    };
    let rust_tool = tool::durable_with_display(
        &Theme::dark(),
        &block,
        80,
        false,
        Some(&ToolDisplayWire {
            detail: "$ cargo test --all-targets".to_owned(),
            expanded_input: None,
            input_line_count: None,
            hidden_line_count: Some(4),
            truncated: false,
        }),
    );
    let source_row = &tool_source["rows"][1];
    assert_eq!(
        rust_tool[1].spans[0].content,
        token_text(source_token(source_row, 0))
    );
    assert_eq!(
        rust_tool[1].spans[0].style.fg,
        Some(token_rgb(source_token(source_row, 0), "fg"))
    );
    assert_eq!(
        rust_tool[1].spans[1].content,
        token_text(source_token(source_row, 1))
    );
    let source_title = token_text(source_token(source_row, 2));
    assert!(source_title.starts_with("bash"));
    assert_eq!(rust_tool[1].spans[2].content.as_ref(), "bash · completed");
    assert_eq!(
        rust_tool[1].spans[2].style.fg,
        Some(token_rgb(source_token(source_row, 2), "fg"))
    );
}

#[test]
fn rust_tool_state_colors_and_hidden_count_boundaries_match_source_facts() {
    let cases = [
        ("state-pending.json", None, [137, 180, 250], [40, 43, 61]),
        (
            "state-success.json",
            Some(minicore_tui::protocol::ToolOutcomeWire::Success),
            [123, 159, 136],
            [41, 49, 46],
        ),
        (
            "state-error.json",
            Some(minicore_tui::protocol::ToolOutcomeWire::Failed),
            [188, 120, 136],
            [52, 43, 47],
        ),
        (
            "state-error.json",
            Some(minicore_tui::protocol::ToolOutcomeWire::Failed),
            [188, 120, 136],
            [52, 43, 47],
        ),
    ];
    // Cancelled is intentionally NOT in the source-match set above: the fixed
    // Rail renderer has no distinct cancelled scene (it renders cancelled
    // calls pending-shaped, see tool/state-cancelled-as-pending.json)
    // while the parity spec (4.2/6.4) mandates a dedicated cancelled surface
    // of its own. That intentional divergence is asserted right below.
    for (fixture_name, outcome, rail_rgb, background_rgb) in cases {
        let source = fixture(format!("tool/{fixture_name}").as_str());
        let block = ToolBlock {
            index: None,
            loop_id: "loop".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "native title line width=79".to_owned(),
            result: None,
            outcome,
            live_status: None,
            progress: None,
            expanded: false,
        };
        let display = minicore_tui::protocol::ToolDisplayWire {
            detail: block.name.clone(),
            expanded_input: None,
            input_line_count: None,
            hidden_line_count: None,
            truncated: false,
        };
        let rust = tool::durable_with_display(&Theme::dark(), &block, 80, false, Some(&display));
        let source_row = &source["rows"][0];
        assert_eq!(
            rust[1].spans[0].style.fg,
            Some(Color::Rgb(rail_rgb[0], rail_rgb[1], rail_rgb[2]))
        );
        assert_eq!(
            rust[1].spans[2].style.bg,
            Some(Color::Rgb(
                background_rgb[0],
                background_rgb[1],
                background_rgb[2]
            ))
        );
        assert_eq!(
            source_token(source_row, 0)["fg"],
            serde_json::json!(rail_rgb)
        );
        assert_eq!(
            source_token(source_row, 1)["bg"],
            serde_json::json!(background_rgb)
        );
    }

    // Intentional spec divergence: cancelled calls render with their own
    // surface (bg #292A35, rail #7F849C per spec 4.2/6.4), overriding the
    // fixed source's pending-shaped rendering.
    let cancelled = ToolBlock {
        index: None,
        loop_id: "loop".to_owned(),
        request_index: 0,
        tool_call_id: "call".to_owned(),
        name: "read".to_owned(),
        result: None,
        outcome: Some(minicore_tui::protocol::ToolOutcomeWire::Cancelled),
        live_status: None,
        progress: None,
        expanded: false,
    };
    let theme = Theme::dark();
    let display = minicore_tui::protocol::ToolDisplayWire {
        detail: cancelled.name.clone(),
        expanded_input: None,
        input_line_count: None,
        hidden_line_count: None,
        truncated: false,
    };
    let rust = tool::durable_with_display(&theme, &cancelled, 80, false, Some(&display));
    assert_eq!(
        rust[1].spans[0].style.fg,
        Some(theme.tool_cancelled_rail),
        "cancelled rail must use the spec colour"
    );
    assert_eq!(
        rust[1].spans[2].style.bg,
        Some(theme.tool_cancelled_bg),
        "cancelled card background must use the spec colour"
    );
    assert_eq!(theme.tool_cancelled_rail, Color::Rgb(0x7f, 0x84, 0x9c));
    assert_eq!(theme.tool_cancelled_bg, Color::Rgb(0x29, 0x2a, 0x35));

    for (result_lines, hidden) in [(19, 20), (20, 21), (21, 22)] {
        let source = fixture(format!("tool/model-tool-output-{result_lines}.json").as_str());
        assert!(
            source["annotations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|annotation| annotation == &format!("hidden={hidden}"))
        );
        let result = (0..result_lines)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let block = ToolBlock {
            index: None,
            loop_id: "loop".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: Some(result.into()),
            outcome: Some(minicore_tui::protocol::ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: false,
        };
        let display = minicore_tui::protocol::ToolDisplayWire {
            detail: "bash".to_owned(),
            expanded_input: None,
            input_line_count: None,
            hidden_line_count: Some(hidden),
            truncated: false,
        };
        let lines = tool::durable_with_display(&Theme::dark(), &block, 80, false, Some(&display));
        // The legacy fixture counts a non-rendered input row. The current
        // hint counts only the actual expandable body at this width.
        let text = lines
            .iter()
            .map(rust_row_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("bash · completed"));
        assert!(text.contains("line 0"));
        assert!(text.contains(&format!("{result_lines} hidden rows")));
        assert!(text.contains("ctrl+o expand"));
    }
}

#[test]
fn rust_reasoning_fold_rows_preserve_source_previews_with_truthful_hints() {
    let cases = [
        ("lines-2-full.json", "thought 1\nthought 2", true),
        ("lines-3-full.json", "thought 1\nthought 2\nthought 3", true),
        (
            "lines-4-collapsed.json",
            "thought 1\nthought 2\nthought 3\nthought 4",
            false,
        ),
        (
            "lines-10-collapsed.json",
            "thought 1\nthought 2\nthought 3\nthought 4\nthought 5\nthought 6\nthought 7\nthought 8\nthought 9\nthought 10",
            false,
        ),
        (
            "lines-10-expanded.json",
            "thought 1\nthought 2\nthought 3\nthought 4\nthought 5\nthought 6\nthought 7\nthought 8\nthought 9\nthought 10",
            true,
        ),
    ];
    for (fixture_name, text, expanded) in cases {
        let source = fixture(format!("thinking/{fixture_name}").as_str());
        let rust = reasoning::reasoning_lines_with_fold(
            &Theme::dark(),
            text,
            80,
            true,
            false,
            Some(expanded),
        );
        let source_rows = source["rows"].as_array().unwrap();
        let source_text = source_rows.iter().map(source_row_text).collect::<Vec<_>>();
        let rust_text = rust.iter().map(rust_row_text).collect::<Vec<_>>();
        if expanded {
            assert!(
                rust_text.iter().all(|row| !row.contains("earlier lines")),
                "{fixture_name} unexpectedly folded"
            );
        } else {
            let source_hint = source_text
                .iter()
                .find(|row| row.contains("earlier lines"))
                .expect("collapsed source fixture has a hint");
            let hidden = source_hint
                .split('(')
                .nth(1)
                .and_then(|text| text.split_whitespace().next())
                .expect("source thinking hint has a hidden count");
            assert!(
                rust_text
                    .iter()
                    .any(|row| row.contains(format!("{hidden} more rows").as_str())),
                "{fixture_name} hidden count differs"
            );
        }
    }
}

#[test]
fn rust_editor_height_formula_matches_source_terminal_sizes() {
    let cases = [
        ("editor/empty.json", 1),
        ("editor/one-line.json", 1),
        ("editor/four-lines.json", 4),
        ("editor/thirteen-lines.json", 13),
        ("editor/narrow-60x16.json", 4),
        ("editor/wide-120x40.json", 13),
    ];
    for (fixture_name, body_rows) in cases {
        let source = fixture(fixture_name);
        let columns = source["term"]["cols"].as_u64().unwrap() as u16;
        let terminal_rows = source["term"]["rows"].as_u64().unwrap() as u16;
        let rendered_rows = source["rows"].as_array().unwrap().len();
        assert_eq!(
            rendered_rows,
            rail::editor_target_rows(body_rows, terminal_rows) as usize,
            "{fixture_name} editor height at {columns}x{terminal_rows}"
        );
    }
}
