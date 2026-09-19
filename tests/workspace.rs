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
    for path in ["src/main.rs", "空 格/\"quote\".txt", "a\\b", "line\nname"] {
        let token = reference_token(path);
        assert_eq!(serde_json::from_str::<String>(&token[1..]).unwrap(), path);
        assert!(!token.contains('\n'));
    }
}
