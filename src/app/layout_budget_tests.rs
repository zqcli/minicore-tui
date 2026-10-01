use super::*;
use crate::state::view::{
    DurableCacheKey, LayoutKey, SectionId, SectionKind, SectionLayout, SourceMap,
};

const MIB: usize = 1024 * 1024;

fn app() -> App {
    let mut app = crate::ui::testapp::open_with(
        ThemeKind::Dark,
        "ses_1",
        None,
        "high",
        vec![crate::ui::testapp::user_entry(
            0,
            "loop",
            "saved exact source",
        )],
    );
    crate::ui::testapp::open_session(&mut app, "ses_2");
    app.sessions.active = Some("ses_1".into());
    app.enable_async_layout();
    app
}

fn section(session: &str, index: usize, bytes: usize) -> Arc<SectionLayout> {
    let source: Arc<str> = "x".repeat(bytes).into();
    Arc::new(SectionLayout {
        key: LayoutKey {
            section: SectionId {
                session_id: session.into(),
                loop_id: None,
                request_index: None,
                kind: SectionKind::Summary,
                ordinal: index as u32,
                tool_call_id: None,
                history_index: Some(index),
            },
            revision: 0,
            width: 79,
            theme: ThemeKind::Dark,
            folded: false,
            reasoning_visible: true,
        },
        order: index,
        rows: Arc::new(vec!["visible".into()]),
        source: Arc::clone(&source),
        source_map: Arc::new(SourceMap {
            source,
            rows: Arc::new(Vec::new()),
        }),
        copy_ranges: Arc::new(Vec::new()),
        link_cells: Arc::new(Vec::new()),
        content_columns: 0..79,
        collapsible: false,
        folded: false,
    })
}

fn durable(
    app: &App,
    session: &str,
    width: u16,
    sections: Vec<Arc<SectionLayout>>,
) -> Arc<PreparedDurable> {
    Arc::new(PreparedDurable {
        key: DurableCacheKey::new(
            &app.sessions.known[session],
            width,
            app.theme,
            app.reasoning_visible,
        ),
        layout: Arc::new(ConversationLayout::from_sections(sections)),
    })
}

fn begin(app: &mut App, width: u16) -> DurableLayoutIdentity {
    let request = app.layout_request(width).expect("new layout requested");
    let identity = request.identity;
    app.mark_layout_pending(identity.clone());
    identity
}

fn batch(
    app: &mut App,
    identity: &DurableLayoutIdentity,
    parts: Vec<Arc<SectionLayout>>,
    complete: bool,
) {
    let durable = durable(app, &identity.session_id, identity.width, parts);
    app.update(AppEvent::DurableLayoutPrepared(DurableLayoutResult {
        identity: identity.clone(),
        durable,
        changed_sections: 0,
        tool_index_lookups: 0,
        complete,
    }));
}

fn indices(app: &App, width: u16) -> Vec<Option<usize>> {
    app.cached_durable(width)
        .unwrap()
        .layout
        .sections
        .iter()
        .map(|part| part.layout.key.section.history_index)
        .collect()
}

#[test]
fn layout_budget_background_eviction_preserves_empty_terminal_batch_and_later_suffix() {
    for later_suffix in [false, true] {
        let mut app = app();
        let background = durable(
            &app,
            "ses_2",
            79,
            (0..23).map(|i| section("ses_2", i, 2 * MIB)).collect(),
        );
        app.sessions
            .known
            .get_mut("ses_2")
            .unwrap()
            .transcript
            .render_cache = Some(background);
        let id = begin(&mut app, 79);
        batch(
            &mut app,
            &id,
            vec![section("ses_1", 0, 2 * MIB), section("ses_1", 1, 2 * MIB)],
            false,
        );
        assert!(
            app.sessions.known["ses_2"]
                .transcript
                .render_cache
                .is_none()
        );
        assert_eq!(app.layout_partial.as_ref().unwrap().1.sections.len(), 2);
        if later_suffix {
            batch(&mut app, &id, vec![section("ses_1", 2, 1024)], false);
        }
        batch(&mut app, &id, vec![], true);
        assert_eq!(
            indices(&app, 79),
            if later_suffix {
                vec![Some(0), Some(1), Some(2)]
            } else {
                vec![Some(0), Some(1)]
            }
        );
        assert!(app.layout_cache_bytes() <= crate::limits::LAYOUT_CACHE_BYTES);
        assert!(app.layout_request(79).is_none());
    }
}

#[test]
fn layout_budget_releases_old_active_cache_without_invalidating_replacement() {
    let mut app = app();
    let old = durable(
        &app,
        "ses_1",
        79,
        (0..23).map(|i| section("ses_1", i, 2 * MIB)).collect(),
    );
    let weak = Arc::downgrade(&old);
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .transcript
        .render_cache = Some(old);
    let id = begin(&mut app, 80);
    batch(
        &mut app,
        &id,
        vec![section("ses_1", 0, 2 * MIB), section("ses_1", 1, 2 * MIB)],
        false,
    );
    assert!(weak.upgrade().is_none());
    assert_eq!(app.layout_pending.as_ref(), Some(&id));
    batch(&mut app, &id, vec![], true);
    assert_eq!(indices(&app, 80), vec![Some(0), Some(1)]);
}

#[test]
fn layout_budget_active_overflow_is_truthful_noncopyable_and_does_not_spin() {
    let mut app = app();
    let source = Arc::clone(
        app.sessions.known["ses_1"]
            .transcript
            .window
            .item(0)
            .unwrap(),
    );
    let anchor = ScrollAnchor {
        section_id: SectionId {
            session_id: "ses_1".into(),
            loop_id: Some("loop".into()),
            request_index: None,
            kind: SectionKind::User,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(0),
        },
        source_offset: 4,
        screen_row: 3,
    };
    app.active_session_mut().unwrap().scroll.follow_tail = false;
    app.active_session_mut().unwrap().scroll.anchor = Some(anchor.clone());
    let id = begin(&mut app, 79);
    batch(
        &mut app,
        &id,
        (0..25).map(|i| section("ses_1", i, 2 * MIB)).collect(),
        false,
    );
    assert!(app.layout_pending.is_none());
    assert!(app.layout_partial.is_none());
    let fallback = app.cached_durable(79).unwrap();
    assert_eq!(indices(&app, 79), vec![None]);
    assert!(fallback.retained_bytes() < 4096);
    let prepared = app.prepared_conversation(79).unwrap();
    let text = prepared
        .lines()
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Conversation display limit reached (48 MiB)"));
    assert!(text.contains("Saved history is unchanged"));
    assert!(
        (0..prepared.total_rows())
            .all(|row| prepared.copy_row(row).is_none_or(|copy| copy.decorative))
    );
    assert_eq!(app.active_view().unwrap().scroll.anchor, Some(anchor));
    assert!(Arc::ptr_eq(
        &source,
        app.sessions.known["ses_1"]
            .transcript
            .window
            .item(0)
            .unwrap()
    ));
    for _ in 0..5 {
        batch(&mut app, &id, vec![section("ses_1", 99, 1024)], false);
        batch(&mut app, &id, vec![], true);
        app.update(AppEvent::Tick);
        assert!(app.layout_request(79).is_none());
        assert!(Arc::ptr_eq(&fallback, &app.cached_durable(79).unwrap()));
    }
    // A real geometry change permits a new, smaller layout; old chunks stay stale.
    let smaller = begin(&mut app, 159);
    batch(&mut app, &id, vec![], true);
    assert_eq!(app.layout_pending.as_ref(), Some(&smaller));
    batch(&mut app, &smaller, vec![section("ses_1", 0, 64)], false);
    batch(&mut app, &smaller, vec![], true);
    assert_eq!(indices(&app, 159), vec![Some(0)]);
    assert!(app.layout_request(159).is_none());
}

#[test]
fn layout_budget_complete_active_cache_overflow_uses_the_same_bounded_fallback() {
    let mut app = app();
    let large = durable(
        &app,
        "ses_1",
        79,
        (0..25).map(|i| section("ses_1", i, 2 * MIB)).collect(),
    );
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .transcript
        .render_cache = Some(large);
    app.enforce_layout_budget();
    assert_eq!(indices(&app, 79), vec![None]);
    assert!(app.layout_cache_bytes() < 4096);
    assert!(app.layout_request(79).is_none());
    app.active_session_mut().unwrap().transcript.invalidate();
    assert!(
        app.layout_request(79).is_some(),
        "changed source generation can retry"
    );
}

#[test]
fn layout_budget_stale_partial_never_marks_a_new_session_as_overflowed() {
    let mut app = app();
    let id = begin(&mut app, 79);
    let oversized = durable(
        &app,
        "ses_1",
        79,
        (0..25).map(|i| section("ses_1", i, 2 * MIB)).collect(),
    );
    app.layout_partial = Some((id, Arc::clone(&oversized.layout)));
    app.sessions.active = Some("ses_2".into());
    app.enforce_layout_budget();
    assert!(app.layout_pending.is_none());
    assert!(app.layout_partial.is_none());
    assert!(app.cached_durable(79).is_none());
    assert!(app.layout_request(79).is_some());
}

#[test]
fn layout_budget_fallback_guidance_is_visible_in_real_narrow_buffers() {
    for (width, height) in [(60, 16), (80, 24)] {
        let mut app = app();
        app.update(AppEvent::Terminal(crossterm::event::Event::Resize(
            width, height,
        )));
        app.viewport = (0, height as usize);
        let content_width =
            crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, width, height))
                .content
                .width;
        app.install_layout_limit(content_width);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for expected in [
            "Conversation display limit reached (48 MiB)",
            "Saved history is unchanged",
            "/search full",
            "/export",
            "/refresh",
        ] {
            assert!(
                text.contains(expected),
                "missing {expected} at {width}x{height}: {text}"
            );
        }
        assert!(!text.contains("Summary:"));
        let prepared = app.prepared_conversation(content_width).unwrap();
        assert!(
            (0..prepared.total_rows())
                .all(|row| prepared.copy_row(row).is_none_or(|copy| copy.decorative))
        );
        assert!(
            prepared
                .sections
                .iter()
                .all(|section| section.id.history_index.is_none())
        );
    }
}
