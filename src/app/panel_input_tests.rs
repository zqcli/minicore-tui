//! Terminal-event regressions for the three single-line panel inputs.
use super::*;
use crate::state::{export::ExportPhase, search::SearchScope, workspace::BrowserKind};
use crate::ui::testapp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app() -> App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.composer.set_text("retained composer 中🙂");
    app
}
fn key(app: &mut App, code: KeyCode) {
    assert!(
        app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE
        ))))
        .is_empty()
    );
}
fn paste(app: &mut App, text: &str) {
    assert!(
        app.update(AppEvent::Terminal(CrosstermEvent::Paste(text.into())))
            .is_empty()
    );
}
fn middle_edit(app: &mut App) {
    for _ in 0..3 {
        key(app, KeyCode::Left);
    }
    key(app, KeyCode::Char('X'));
    key(app, KeyCode::Delete);
}

#[test]
fn panel_input_search_prefill_cursor_is_a_utf8_byte_boundary() {
    let mut app = app();
    app.open_search("中🙂e\u{301}".into(), SearchScope::Loaded);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.search_panel().unwrap().query, "中🙂e");
    assert_eq!(app.search_panel().unwrap().query_cursor, "中🙂e".len());
}

#[test]
fn panel_input_search_paste_and_midline_edit_never_submit() {
    let mut app = app();
    app.open_search(String::new(), SearchScope::Loaded);
    paste(&mut app, "report.md");
    middle_edit(&mut app);
    assert_eq!(app.search_panel().unwrap().query, "reportXmd");
    key(&mut app, KeyCode::Home);
    key(&mut app, KeyCode::Delete);
    key(&mut app, KeyCode::End);
    paste(&mut app, " 中🙂e\u{301}");
    assert_eq!(app.search_panel().unwrap().query, "eportXmd 中🙂e\u{301}");
    assert_eq!(app.composer.content(), "retained composer 中🙂");
}

#[test]
fn panel_input_export_paste_midline_and_home_end() {
    let mut app = app();
    app.open_export_form(String::new(), false);
    paste(&mut app, "report.md");
    middle_edit(&mut app);
    assert_eq!(app.export_form().unwrap().target, "reportXmd");
    key(&mut app, KeyCode::Home);
    key(&mut app, KeyCode::Char('中'));
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.export_form().unwrap().target, "中reportXm");
    assert_eq!(app.composer.content(), "retained composer 中🙂");
}

#[test]
fn panel_input_export_running_and_cancelling_are_frozen() {
    for phase in [ExportPhase::Running, ExportPhase::Cancelling] {
        let mut app = app();
        let mut form = crate::state::ExportFormState::new("report.md".into());
        form.phase = phase;
        app.dock = Dock::Export(form);
        let before = app.export_form().unwrap().clone();
        for code in [
            KeyCode::Home,
            KeyCode::Right,
            KeyCode::Delete,
            KeyCode::Backspace,
            KeyCode::Char('X'),
        ] {
            key(&mut app, code);
        }
        paste(&mut app, "/quit");
        assert_eq!(app.export_form().unwrap(), &before);
        assert_eq!(app.composer.content(), "retained composer 中🙂");
    }
}

#[test]
fn panel_input_workspace_query_and_scope_have_independent_cursors() {
    for kind in [BrowserKind::Files, BrowserKind::Grep] {
        let mut app = app();
        app.open_workspace_browser(kind, "report.md".into(), false);
        middle_edit(&mut app);
        assert_eq!(app.workspace_browser().unwrap().query, "reportXmd");
        key(&mut app, KeyCode::Tab);
        paste(&mut app, "中🙂e\u{301}");
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Delete);
        assert_eq!(app.workspace_browser().unwrap().scope, "中🙂e");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.workspace_browser().unwrap().query, "reportmd");
        key(&mut app, KeyCode::Home);
        key(&mut app, KeyCode::Delete);
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Char('!'));
        assert_eq!(app.workspace_browser().unwrap().query, "eportmd!");
        assert_eq!(app.composer.content(), "retained composer 中🙂");
    }
}

#[test]
fn panel_input_workspace_navigation_does_not_rescan_and_bulk_paste_resets_once() {
    let mut app = app();
    app.open_workspace_browser(BrowserKind::Files, "中🙂e\u{301}".into(), false);
    let generation = app.workspace_browser().unwrap().generation;
    for code in [
        KeyCode::Left,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Right,
        KeyCode::Tab,
        KeyCode::BackTab,
    ] {
        key(&mut app, code);
    }
    assert_eq!(app.workspace_browser().unwrap().generation, generation);
    paste(&mut app, "/quit");
    assert_eq!(app.workspace_browser().unwrap().generation, generation + 1);
    assert_eq!(app.workspace_browser().unwrap().query, "中🙂e\u{301}/quit");
    assert!(!app.shutting_down());
}

#[test]
fn panel_input_rejects_multiline_and_control_paste_atomically_with_feedback() {
    for field in 0..4 {
        let mut app = app();
        match field {
            0 => {
                app.open_search(String::new(), SearchScope::Loaded);
            }
            1 => {
                app.open_export_form(String::new(), false);
            }
            _ => {
                app.open_workspace_browser(BrowserKind::Grep, String::new(), false);
                if field == 3 {
                    key(&mut app, KeyCode::Tab);
                }
            }
        }
        paste(&mut app, "safe中");
        let before = app.dock.clone();
        for text in [
            "oops\r\n/quit",
            "oops\n",
            "oops\r",
            "oops\0",
            "oops\x1b[31m",
        ] {
            let notices = app.notices().len();
            paste(&mut app, text);
            assert_eq!(app.dock, before);
            assert!(
                app.notices().len() > notices
                    || app.notices().back().unwrap().text.contains("single-line")
            );
        }
        assert_eq!(app.composer.content(), "retained composer 中🙂");
        assert!(!app.shutting_down());
    }
}

#[test]
fn panel_input_search_results_keep_navigation_and_scope_keys() {
    use crate::state::search::{SearchPanelMode, SearchPanelState};
    let mut app = app();
    app.dock = Dock::Search(SearchPanelState::new(
        "ses_1".into(),
        0,
        "needle".into(),
        SearchScope::Loaded,
    ));
    for (code, expected) in [
        (KeyCode::Home, Action::SearchMove(-1000)),
        (KeyCode::End, Action::SearchMove(1000)),
        (KeyCode::Up, Action::SearchMove(-1)),
        (KeyCode::Down, Action::SearchMove(1)),
        (KeyCode::PageDown, Action::SearchMove(10)),
        (KeyCode::Char('n'), Action::SearchStep(1)),
        (KeyCode::Char('p'), Action::SearchStep(-1)),
        (KeyCode::Char('s'), Action::SearchStop),
    ] {
        assert_eq!(
            keymap::map(&app, KeyEvent::new(code, KeyModifiers::NONE)),
            expected
        );
    }
    assert_eq!(
        keymap::map(
            &app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        ),
        Action::SearchScopeToggle
    );
    key(&mut app, KeyCode::Left);
    assert_eq!(app.search_panel().unwrap().mode, SearchPanelMode::Input);
    assert_eq!(app.search_panel().unwrap().query_cursor, 5);
    assert_eq!(
        keymap::map(&app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
        Action::SearchHome
    );
    assert_eq!(
        keymap::map(&app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        Action::SearchEnd
    );
    assert_eq!(
        keymap::map(
            &app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        ),
        Action::SearchScopeToggle
    );
}

#[test]
fn panel_input_export_and_workspace_control_bindings_remain_local() {
    let mut app = app();
    app.open_export_form("report.md".into(), false);
    for (c, action) in [
        ('y', Action::ExportToggleOverwrite),
        ('r', Action::ExportToggleRaw),
        ('n', Action::ExportToggleUnsaved),
        ('p', Action::ExportToggleTool),
        ('t', Action::ExportToggleThinking),
        ('u', Action::ExportClear),
    ] {
        assert_eq!(
            keymap::map(&app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
            action
        );
    }
    for kind in [BrowserKind::Files, BrowserKind::Grep] {
        app.open_workspace_browser(kind, String::new(), false);
        for (c, action) in [
            ('n', Action::WorkspaceMore(false)),
            ('i', Action::WorkspaceCase),
            ('u', Action::WorkspaceClear),
        ] {
            assert_eq!(
                keymap::map(&app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
                action
            );
        }
        for (code, action) in [
            (KeyCode::Up, Action::WorkspaceMove(-1)),
            (KeyCode::PageDown, Action::WorkspaceMove(5)),
            (KeyCode::Enter, Action::WorkspaceSelect(false)),
            (KeyCode::F(4), Action::WorkspaceSelect(true)),
        ] {
            assert_eq!(
                keymap::map(&app, KeyEvent::new(code, KeyModifiers::NONE)),
                action
            );
        }
    }
}

#[test]
fn panel_input_limits_empty_edits_and_unicode_boundaries() {
    for (field, limit) in [
        (0, crate::state::search::MAX_SEARCH_QUERY_BYTES),
        (1, 1024),
        (2, 4096),
    ] {
        let mut app = app();
        if field == 0 {
            app.open_search(String::new(), SearchScope::Loaded);
        } else {
            app.open_workspace_browser(BrowserKind::Grep, String::new(), false);
            if field == 2 {
                key(&mut app, KeyCode::Tab);
            }
        }
        let before = app.dock.clone();
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Backspace,
            KeyCode::Delete,
        ] {
            key(&mut app, code);
        }
        assert_eq!(app.dock, before);
        let initial = format!("{}中🙂e\u{301}", "a".repeat(limit - "中🙂e\u{301}".len()));
        paste(&mut app, &initial);
        let before = app.dock.clone();
        paste(&mut app, "🙂");
        assert_eq!(app.dock, before, "over-limit insertion is atomic");
        for _ in 0..4 {
            key(&mut app, KeyCode::Left);
        }
        key(&mut app, KeyCode::Delete);
        paste(&mut app, "你");
        let (text, cursor) = if let Some(panel) = app.search_panel() {
            (panel.query.as_str(), panel.query_cursor)
        } else {
            let b = app.workspace_browser().unwrap();
            if field == 2 {
                (b.scope.as_str(), b.scope_cursor)
            } else {
                (b.query.as_str(), b.query_cursor)
            }
        };
        assert_eq!(text, initial.replace('中', "你"));
        assert!(text.is_char_boundary(cursor));
        assert_eq!(text.len(), limit);
    }
}

#[test]
fn panel_input_tabs_stay_literal_in_grep_json_scope_and_queries() {
    let mut app = app();
    app.open_workspace_browser(BrowserKind::Grep, String::new(), false);
    paste(&mut app, "a\tb");
    assert_eq!(app.workspace_browser().unwrap().query, "a\tb");
    key(&mut app, KeyCode::Tab);
    paste(&mut app, "[\t\"src\",\t\"dir with spaces\"]");
    assert_eq!(
        app.workspace_browser().unwrap().paths().unwrap(),
        vec!["src", "dir with spaces"]
    );
    assert_eq!(
        app.workspace_browser().unwrap().scope,
        "[\t\"src\",\t\"dir with spaces\"]"
    );
}

#[test]
fn panel_input_failed_export_keeps_outcome_and_retry_options_while_editing() {
    let mut app = app();
    app.open_export_form("bad/report.md".into(), true);
    let form = app.export_form_mut().unwrap();
    form.phase = ExportPhase::Failed;
    form.overwrite = true;
    form.notice = Some("synthetic write failed; retry".into());
    form.completion = Some(crate::state::export::ExportCompletion::TargetExists {
        target: "bad/report.md".into(),
    });
    let completion = form.completion.clone();
    key(&mut app, KeyCode::Home);
    for _ in 0..4 {
        key(&mut app, KeyCode::Delete);
    }
    paste(&mut app, "中/");
    let form = app.export_form().unwrap();
    assert_eq!(form.target, "中/report.md");
    assert_eq!(
        form.notice.as_deref(),
        Some("synthetic write failed; retry")
    );
    assert_eq!(form.completion, completion);
    assert!(form.overwrite && form.spec.raw_oversized);
    assert_eq!(form.phase, ExportPhase::Failed);
    assert_eq!(app.composer.content(), "retained composer 中🙂");
}

#[test]
fn panel_input_late_loaded_scan_keeps_the_edited_query_cursor_and_focus() {
    use crate::state::search::{SearchMatch, SearchPanelMode, SearchSource};
    let mut app = app();
    let commands = app.open_search("old".into(), SearchScope::Loaded);
    let request = commands
        .into_iter()
        .find_map(|c| {
            if let AppCommand::LocalScan(r) = c {
                Some(r)
            } else {
                None
            }
        })
        .unwrap();
    key(&mut app, KeyCode::Left);
    paste(&mut app, "新");
    let before = app.search_panel().unwrap().clone();
    app.on_local_scan_finished(crate::jobs::LocalScanOutcome {
        identity: request.identity,
        matches: vec![SearchMatch {
            index: Some(0),
            source: SearchSource::Prompt,
            loop_id: None,
            request_index: None,
            ordinal: 0,
            tool_call_id: None,
            preview: "old".into(),
            source_offset: 0,
            byte_range: 0..3,
        }],
        truncated: false,
    });
    let panel = app.search_panel().unwrap();
    assert_eq!(panel.mode, SearchPanelMode::Input);
    assert_eq!(panel.query, before.query);
    assert_eq!(panel.query_cursor, before.query_cursor);
    let commands = app.search_confirm();
    let request = commands
        .into_iter()
        .find_map(|c| {
            if let AppCommand::LocalScan(r) = c {
                Some(r)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(request.needle, "ol新d");
}

#[test]
fn panel_input_full_search_owns_its_original_needle_while_input_is_edited() {
    let mut app = app();
    app.open_search("old needle".into(), SearchScope::FullSession);
    let generation = app.search_scan.as_ref().unwrap().generation;
    key(&mut app, KeyCode::Home); // Result navigation retains query cursor.
    key(&mut app, KeyCode::Left);
    paste(&mut app, "/quit");
    let scan = app.search_scan.as_ref().unwrap();
    assert_eq!(scan.needle, "old needle");
    assert_eq!(scan.generation, generation);
    assert_eq!(app.search_panel().unwrap().query, "old needl/quite");
    assert_eq!(app.composer.content(), "retained composer 中🙂");
    assert!(!app.shutting_down());
}

#[test]
fn panel_input_late_full_decode_after_edit_keeps_attribution_and_new_scan_fences_it() {
    use crate::jobs::{DecodeOutcome, ScanItemOutcome};
    use crate::protocol::read::{EncodedHistoryItem, ReadCursor};
    use crate::state::search::{ScanPlan, SearchPanelMode};
    for restart in [false, true] {
        let mut app = app();
        app.open_search("old".into(), SearchScope::FullSession);
        let raw = testapp::user_entry(0, "loop_1", "old matching literal").to_string();
        let mut page = crate::app::history::ReadPage::new(ReadCursor::start(), None, 0);
        page.pending_encoded.push_back(EncodedHistoryItem {
            index: 0,
            data: raw.into(),
        });
        app.search_scan.as_mut().unwrap().page = Some(page);
        app.search_scan.as_mut().unwrap().terminal = true;
        app.queue_search_decode();
        let request = app.pending_decode_request().unwrap();
        app.mark_decode_scheduled();
        key(&mut app, KeyCode::Left);
        paste(&mut app, "新");
        assert_eq!(request.scan.as_ref().unwrap().needle, "old");
        assert!(
            app.search_panel()
                .unwrap()
                .status_label()
                .contains("previous query \"old\"")
        );
        let item = crate::protocol::read::decode_item(&request.item.data).unwrap();
        let mut plan = ScanPlan::new("old", false);
        plan.scan_item(0, &item);
        if restart {
            app.search_confirm();
        }
        let outcome = DecodeOutcome {
            identity: request.identity,
            fingerprint: request.fingerprint,
            result: Ok(item),
            cancelled: false,
            scan: Some(Box::new(ScanItemOutcome {
                index: 0,
                matches: plan.collector.matches,
            })),
            export: None,
        };
        app.update(AppEvent::HistoryItemDecoded(Box::new(outcome)));
        let panel = app.search_panel().unwrap();
        assert_eq!(panel.query, "ol新d");
        assert_eq!(panel.query_cursor, "ol新".len());
        if restart {
            assert!(
                panel.matches.is_empty(),
                "old-generation decode cannot install"
            );
            assert_eq!(panel.submitted_query.as_deref(), Some("ol新d"));
        } else {
            assert_eq!(panel.matches.len(), 1);
            assert_eq!(panel.submitted_query.as_deref(), Some("old"));
            assert_eq!(panel.mode, SearchPanelMode::Input);
            assert!(panel.status_label().contains("previous query \"old\""));
        }
        assert_eq!(app.composer.content(), "retained composer 中🙂");
    }
}

#[test]
fn panel_input_workspace_cursor_edits_preserve_inflight_owner_and_retry_generation() {
    use crate::ui::testapp::{respond, take_requests};
    use serde_json::json;
    let mut app = app();
    app.open_workspace_browser(BrowserKind::Files, "report.md".into(), false);
    let request = take_requests(app.workspace_more(true)).remove(0);
    let generation = app.workspace_browser().unwrap().generation;
    key(&mut app, KeyCode::Left);
    assert_eq!(app.workspace_browser().unwrap().generation, generation);
    paste(&mut app, "新");
    assert_eq!(app.queries.in_flight_len(), 1);
    assert!(app.request_is_pending(request.id));
    // Completing the old read cannot install its obsolete candidate.
    let value = json!({"directory":"","entries":[{"path":"old-result.md","kind":"file","size":10}],"next_cursor":null,"truncated":false,"scan_complete":true,"stopped_by":"end","skipped_count":0,"consistency":"live","observed_at_unix_ms":1});
    respond(&mut app, &request, value);
    assert!(app.workspace_browser().unwrap().files.is_empty());
    let request = take_requests(app.workspace_more(true)).remove(0);
    assert_eq!(request.params["query"], "report.m新d");
    app.workspace_send_failed(
        app.workspace_browser().unwrap().generation,
        workspace::WorkspaceQuery::Files,
    );
    let browser = app.workspace_browser().unwrap();
    let error = browser.error.clone();
    let due = browser.due;
    key(&mut app, KeyCode::Left);
    assert_eq!(app.workspace_browser().unwrap().error, error);
    assert_eq!(app.workspace_browser().unwrap().due, due);
    paste(&mut app, "X");
    assert!(app.workspace_browser().unwrap().error.is_none());
    assert!(app.workspace_browser().unwrap().due.is_some());
    assert_eq!(app.composer.content(), "retained composer 中🙂");
}

#[test]
fn panel_input_caret_is_visible_in_each_field_at_wide_and_minimum_sizes() {
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
    };
    for (width, height) in [(160, 48), (60, 16)] {
        for field in 0..4 {
            let mut app = app();
            app.terminal_size = (width, height);
            match field {
                0 => {
                    app.open_search(String::new(), SearchScope::Loaded);
                }
                1 => {
                    app.open_export_form(String::new(), false);
                }
                _ => {
                    app.open_workspace_browser(BrowserKind::Grep, String::new(), false);
                    if field == 3 {
                        key(&mut app, KeyCode::Tab);
                    }
                }
            }
            paste(&mut app, &format!("{}中🙂tail", "prefix/".repeat(30)));
            for home in [false, true] {
                if home {
                    key(&mut app, KeyCode::Home);
                }
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| crate::ui::render(frame, &app))
                    .unwrap();
                let cursor = terminal.backend_mut().get_cursor_position().unwrap();
                assert!(cursor.x < width && cursor.y < height - 1);
                let row: String = (0..width)
                    .map(|x| terminal.backend().buffer()[(x, cursor.y)].symbol())
                    .collect();
                assert!(
                    row.contains(if home { "prefix/" } else { "tail" }),
                    "field {field} at {width}x{height}: {row}"
                );
            }
        }
    }
}
