use minicore_tui::{protocol::workspace::*, state::workspace::*};
use std::sync::{Arc, atomic::AtomicBool};
fn fixture(name: &str) -> serde_json::Value {
    let bytes = std::fs::read(format!("tests/fixtures/agent-v1/{name}.json")).unwrap();
    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["result"].clone()
}
#[test]
fn pinned_workspace_schemas_additive_fields_and_malformed_known_fields() {
    for name in [
        "workspace-read-ok",
        "workspace-read-line-partial",
        "workspace-read-changed",
        "workspace-read-binary",
        "workspace-read-too-large",
    ] {
        let mut value = fixture(name);
        value["future"] = true.into();
        let page: FilePage = serde_json::from_value(value.clone()).unwrap();
        assert!(!format!("{page:?}").contains(&page.content) || page.content.is_empty());
        value["start_line"] = "wrong".into();
        assert!(serde_json::from_value::<FilePage>(value).is_err());
    }
    for name in [
        "workspace-files",
        "workspace-files-paged",
        "workspace-files-deadline",
    ] {
        let mut value = fixture(name);
        value["future"] = true.into();
        let page: FilesPage = serde_json::from_value(value.clone()).unwrap();
        assert!(page.validate());
        value["scan_complete"] = 0.into();
        assert!(serde_json::from_value::<FilesPage>(value).is_err());
    }
    for name in ["workspace-search", "workspace-search-deadline"] {
        let mut value = fixture(name);
        value["future"] = true.into();
        let page: SearchPage = serde_json::from_value(value.clone()).unwrap();
        assert!(page.validate());
        value["skipped_files"] = "wrong".into();
        assert!(serde_json::from_value::<SearchPage>(value).is_err());
    }
}
#[test]
fn deadline_is_partial_without_a_cursor() {
    let page: FilesPage = serde_json::from_value(fixture("workspace-files-deadline")).unwrap();
    assert_eq!(page.stopped_by, ScanStop::Deadline);
    assert!(page.truncated);
    assert!(!page.scan_complete);
    assert!(page.next_cursor.is_none());
    let page: SearchPage = serde_json::from_value(fixture("workspace-search-deadline")).unwrap();
    assert_eq!(page.stopped_by, ScanStop::Deadline);
    assert!(page.next_cursor.is_none());
}
fn layout(content: FileBuffer, width: u16) -> FileLayout {
    FileLayout::build(FileLayoutRequest {
        identity: FileLayoutIdentity {
            generation: 1,
            revision: 1,
            width,
        },
        content,
        cancel: Arc::new(AtomicBool::new(false)),
    })
    .unwrap()
}
#[test]
fn raw_chunks_crlf_same_line_unicode_and_copy_without_softwrap_or_numbers() {
    let mut content = FileBuffer::default();
    for part in ["中a", "bc\r", "\n🙂 no", " newline"] {
        content.append(part.to_owned()).unwrap();
    }
    let view = layout(content, 4);
    assert_eq!(&*view.copy_text, "中abc\r\n🙂 no newline");
    assert_eq!(
        view.rows[1].source,
        FileRange {
            start_line: 1,
            line_byte_offset: 5
        }
    );
    assert!(view.rows.iter().any(|r| r.source.start_line == 2));
    assert!(view.rows.iter().all(
        |r| view.text.is_char_boundary(r.text.start) && view.text.is_char_boundary(r.text.end)
    ));
}
#[test]
fn huge_line_and_tiny_chunks_stay_bounded_and_controls_do_not_execute() {
    let mut content = FileBuffer::default();
    for _ in 0..32000 {
        content.append("a".to_owned()).unwrap();
    }
    assert!(content.chunks.len() <= 2);
    content.append("\x1b]52;secret\x07\r".into()).unwrap();
    let view = layout(content, 55);
    assert!(view.rows.len() > 500);
    assert!(!view.copy_text.contains('\x1b'));
    assert!(!view.copy_text.contains('\r'));
    assert!(view.retained_bytes() < 1024 * 1024);
    let mut content = FileBuffer::default();
    content.append("x".repeat(512 * 1024)).unwrap();
    assert!(content.append("x".into()).is_err());
}
#[test]
fn grep_ranges_are_utf8_bytes_not_character_or_cell_indexes() {
    let mut item = FileMatch {
        path: "秘密.rs".into(),
        line_number: 4,
        line_text_byte_offset: 8,
        match_byte_ranges: vec![MatchRange { start: 3, end: 7 }],
        line_text: "中🙂x".into(),
        line_truncated: true,
    };
    assert!(item.valid_ranges());
    item.match_byte_ranges[0].start = 1;
    assert!(!item.valid_ranges());
    assert!(!format!("{item:?}").contains("秘密"));
}
#[test]
fn quoted_path_tokens_roundtrip_without_attaching_content() {
    for path in [
        "src/main.rs",
        "空 格/\"quote\".txt",
        "a\\b",
        "line\nname",
        "control\u{7f}\u{9b}\u{202e}name",
    ] {
        let token = reference_token(path);
        assert_eq!(serde_json::from_str::<String>(&token[1..]).unwrap(), path);
        assert!(!token.contains('\n'));
    }
}

#[test]
fn grep_highlighting_preserves_combining_clusters_and_uses_cell_widths() {
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;
    let item = FileMatch {
        path: "x".into(),
        line_number: 1,
        line_text_byte_offset: 0,
        match_byte_ranges: vec![MatchRange { start: 6, end: 8 }],
        line_text: "中 e\u{301} y".into(),
        line_truncated: false,
    };
    // Byte 6 is inside the two-byte combining mark: malformed ranges fail closed.
    assert!(!item.valid_ranges());
    let mut item = item;
    item.match_byte_ranges = vec![MatchRange { start: 5, end: 7 }];
    let line = minicore_tui::ui::workspace::match_snippet(&item);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, item.line_text);
    let highlighted = line
        .spans
        .iter()
        .find(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
        .unwrap();
    assert_eq!(highlighted.content.as_ref(), "e\u{301}");
    assert_eq!(highlighted.content.width(), 1);
}

#[test]
fn pathological_grapheme_is_explicitly_bounded_in_display_not_silently_lost() {
    let mut content = FileBuffer::default();
    let text = format!("a{}", "\u{301}".repeat(8000));
    content.append(text.clone()).unwrap();
    let view = layout(content, 55);
    assert!(view.display_limited);
    assert!(view.text.len() < 64);
    assert_eq!(view.copy_text.as_ref(), text);
    assert_eq!(view.rows[0].source_bytes, 0..text.len());
}

fn workspace_app(kind: BrowserKind) -> minicore_tui::app::App {
    use minicore_tui::{app::App, event::AppEvent, state::selection::Dock};
    let mut app = App::new("/synthetic-workspace".into());
    app.update(AppEvent::Terminal(crossterm::event::Event::Resize(80, 24)));
    let mut browser = WorkspaceBrowser::new(
        kind,
        "fixture-session".into(),
        0,
        1,
        std::time::Instant::now(),
    );
    browser.due = None;
    browser.files = (0..3)
        .map(|i| FileEntry {
            path: format!("file-{i}.txt"),
            kind: FileKind::File,
            size: None,
        })
        .collect();
    browser.matches = (0..3)
        .map(|i| FileMatch {
            path: format!("file-{i}.txt"),
            line_number: 1,
            line_text_byte_offset: 0,
            match_byte_ranges: vec![MatchRange { start: 0, end: 1 }],
            line_text: "x".into(),
            line_truncated: false,
        })
        .collect();
    let mut info: minicore_tui::protocol::SessionInfo =
        serde_json::from_value(fixture("session-create")["session"].clone()).unwrap();
    info.session_id = "fixture-session".into();
    let view = minicore_tui::state::session::SessionView::new(info);
    browser.epoch = view.session_epoch;
    app.sessions.active = Some("fixture-session".into());
    app.sessions.known.insert("fixture-session".into(), view);
    app.dock = Dock::Workspace(Box::new(browser));
    app
}

#[test]
fn workspace_dock_wheel_moves_files_and_grep_and_clamps_at_bounds() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use minicore_tui::event::AppEvent;
    for kind in [BrowserKind::Files, BrowserKind::Grep] {
        let mut app = workspace_app(kind);
        let area =
            minicore_tui::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 80, 24))
                .panel;
        let scroll = |kind, row| {
            AppEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column: area.x + 2,
                row,
                modifiers: KeyModifiers::NONE,
            }))
        };
        app.update(scroll(MouseEventKind::ScrollDown, area.y + 3));
        assert_eq!(app.workspace_browser().unwrap().selected, 1);
        for _ in 0..5 {
            app.update(scroll(MouseEventKind::ScrollDown, area.y + 3));
        }
        assert_eq!(app.workspace_browser().unwrap().selected, 2);
        app.update(scroll(MouseEventKind::ScrollUp, area.y.saturating_sub(1)));
        assert_eq!(
            app.workspace_browser().unwrap().selected,
            2,
            "wheel outside the Dock does not change its candidate"
        );
        for _ in 0..5 {
            app.update(scroll(MouseEventKind::ScrollUp, area.y + 3));
        }
        assert_eq!(app.workspace_browser().unwrap().selected, 0);
    }
}

fn rendered_workspace_browser(browser: &WorkspaceBrowser) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 18)).unwrap();
    terminal
        .draw(|frame| {
            minicore_tui::ui::workspace::render_browser(
                frame,
                frame.area(),
                browser,
                &minicore_tui::theme::ThemeKind::Dark.theme(),
            )
        })
        .unwrap();
    workspace_buffer_text(terminal.backend().buffer())
}

#[test]
fn workspace_status_is_readable_and_preserves_partial_stop_and_skipped_facts() {
    let app = workspace_app(BrowserKind::Files);
    let mut browser = app.workspace_browser().unwrap().clone();
    browser.scan_complete = true;
    browser.stopped_by = Some(ScanStop::End);
    let text = rendered_workspace_browser(&browser);
    assert!(text.contains("已列出 3 项 · 扫描完成"));
    assert!(!text.contains("false"));
    assert!(!text.contains("true"));
    assert!(!text.contains("下一页"));
    browser.scan_complete = false;
    browser.truncated = true;
    browser.skipped = 7;
    browser.stopped_by = Some(ScanStop::Page);
    browser.cursor = Some(serde_json::json!({"opaque":"cursor"}));
    let text = rendered_workspace_browser(&browser);
    assert!(text.contains("还有结果 · Ctrl+N 下一页"));
    assert!(text.contains("本页为部分结果 · 本页跳过 7 项"));
    browser.cursor = None;
    browser.stopped_by = Some(ScanStop::Deadline);
    let text = rendered_workspace_browser(&browser);
    assert!(text.contains("扫描未完成"));
    assert!(text.contains("扫描超时"));
    assert!(text.contains("不会自动重扫"));
    assert!(!text.contains("下一页"));
    browser.limited = true;
    let text = rendered_workspace_browser(&browser);
    assert!(text.contains("已达本地保留上限"));
    assert!(text.contains("500 项 / 1 MiB"));
    for (stop, reason) in [
        (ScanStop::Entries, "条目上限"),
        (ScanStop::Bytes, "字节上限"),
        (ScanStop::Depth, "深度上限"),
        (ScanStop::Rules, "规则限制"),
    ] {
        browser.stopped_by = Some(stop);
        assert!(rendered_workspace_browser(&browser).contains(reason));
    }
}

#[test]
fn empty_grep_prompts_for_query_instead_of_claiming_to_scan() {
    let app = workspace_app(BrowserKind::Grep);
    let text = rendered_workspace_browser(app.workspace_browser().unwrap());
    assert!(text.contains("输入要查找的文字"));
    assert!(!text.contains("查询中"));
    assert!(!text.contains("扫描完成"));
}

fn rendered_file(app: &minicore_tui::app::App) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 18)).unwrap();
    terminal
        .draw(|frame| {
            minicore_tui::ui::workspace::render_file(
                frame,
                frame.area(),
                app,
                &minicore_tui::theme::ThemeKind::Dark.theme(),
            )
        })
        .unwrap();
    workspace_buffer_text(terminal.backend().buffer())
}

#[test]
fn file_preview_more_is_shown_only_when_reading_can_continue() {
    use minicore_tui::state::panels::MainView;
    let mut app = workspace_app(BrowserKind::Files);
    app.open_file_preview("fixture.txt".into(), None, ReturnTarget::Conversation);
    assert!(!rendered_file(&app).contains("[更多]"));
    let MainView::FilePreview(file) = &mut app.main_view else {
        panic!("missing file preview")
    };
    file.status = Some(FileStatus::Ok);
    file.content.append("fixture\n".into()).unwrap();
    file.next = None;
    let text = rendered_file(&app);
    assert!(text.contains("已读 8 bytes · 已全部读取"));
    assert!(!text.contains("[更多]"));
    assert!(!text.contains("false"));
    let MainView::FilePreview(file) = &mut app.main_view else {
        unreachable!()
    };
    file.next = Some(FileRange {
        start_line: 2,
        line_byte_offset: 0,
    });
    file.truncated = true;
    file.line_truncated = true;
    let text = rendered_file(&app);
    assert!(text.contains("[更多]"));
    assert!(text.contains("部分内容 · 本行未完 · Ctrl+N 继续读取"));
    let MainView::FilePreview(file) = &mut app.main_view else {
        unreachable!()
    };
    file.status = Some(FileStatus::Changed);
    let text = rendered_file(&app);
    assert!(!text.contains("[更多]"));
    assert!(text.contains("保留旧快照"));
    let MainView::FilePreview(file) = &mut app.main_view else {
        unreachable!()
    };
    file.status = Some(FileStatus::Ok);
    file.error = Some("read unavailable".into());
    assert!(!rendered_file(&app).contains("[更多]"));
}

fn workspace_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut text = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        text.push('\n');
    }
    text
}
