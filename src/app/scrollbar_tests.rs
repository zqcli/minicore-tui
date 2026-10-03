use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

fn ready() -> App {
    let mut app = crate::ui::testapp::tools(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
    app.update(AppEvent::Viewport {
        total_lines: prepared.total_rows(),
        visible_rows: screen.transcript.height as usize,
    });
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Rendered);
    app
}

#[test]
fn only_the_dedicated_main_track_can_hover_or_capture() {
    let mut app = ready();
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    for column in [screen.content.right() - 1, screen.right_gap.x] {
        mouse(
            &mut app,
            MouseEventKind::Moved,
            column,
            1,
            KeyModifiers::NONE,
        );
        assert!(!app.scrollbar_active());
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            column,
            1,
            KeyModifiers::NONE,
        );
        assert!(app.scrollbar_drag.is_none());
    }
    let cursor = app.composer.cursor();
    for row in [screen.panel.y, screen.footer.y] {
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            screen.scrollbar.x,
            row,
            KeyModifiers::NONE,
        );
        assert!(app.scrollbar_drag.is_none());
        assert_eq!(app.composer.cursor(), cursor);
    }
    mouse(
        &mut app,
        MouseEventKind::Moved,
        screen.scrollbar.x,
        1,
        KeyModifiers::NONE,
    );
    assert!(app.scrollbar_active());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        screen.scrollbar.x,
        1,
        KeyModifiers::NONE,
    );
    assert!(app.scrollbar_drag.is_some());
}

#[test]
fn pending_async_resize_fences_the_external_scrollbar_as_well_as_the_body() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = ready();
    app.enable_async_layout();
    let prepared = crate::ui::transcript::prepare_conversation(&app, app.terminal_content_width());
    app.update(AppEvent::ConversationPrepared(prepared));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let frame = terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    app.remember_transcript_frame(frame.buffer);
    let before = app.active_view().unwrap().scroll.clone();
    app.update(AppEvent::TerminalSize {
        width: 100,
        height: 24,
    });
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Down(MouseButton::Left),
    ] {
        mouse(&mut app, kind, 99, 1, KeyModifiers::NONE);
        assert!(!app.scrollbar_active());
        assert!(app.scrollbar_drag.is_none());
        assert!(app.selection.is_none());
        let scroll = &app.active_view().unwrap().scroll;
        assert_eq!(
            (scroll.offset, scroll.follow_tail),
            (before.offset, before.follow_tail)
        );
    }
}

#[test]
fn early_scrollbar_tick_does_not_redraw() {
    let mut app = ready();
    let base = Instant::now();
    app.monotonic_now = Arc::new(move || base);
    mouse(
        &mut app,
        MouseEventKind::ScrollUp,
        4,
        1,
        KeyModifiers::empty(),
    );
    app.update(AppEvent::Rendered);
    app.update(AppEvent::Tick);
    assert!(!app.dirty);
    assert!(app.scrollbar_visible(app.viewport.0, app.viewport.1));
}

#[test]
fn any_mouse_release_ends_scrollbar_capture() {
    let mut app = ready();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        79,
        0,
        KeyModifiers::empty(),
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Right),
        4,
        5,
        KeyModifiers::empty(),
    );
    assert!(app.scrollbar_drag.is_none());
    assert!(!app.scrollbar_active());
}

#[test]
fn selectors_do_not_activate_underlying_scrollbar() {
    use crate::state::selection::{SelectorKind, SelectorState, SessionSelectorState};
    for dock in [
        Dock::ModelSelector(SelectorState::new(SelectorKind::Model)),
        Dock::SessionSelector(SessionSelectorState::new(Some("ses_1".into()))),
    ] {
        let mut app = ready();
        mouse(
            &mut app,
            MouseEventKind::Moved,
            79,
            1,
            KeyModifiers::empty(),
        );
        assert!(app.scrollbar_active());
        app.dock = dock;
        mouse(
            &mut app,
            MouseEventKind::Moved,
            79,
            1,
            KeyModifiers::empty(),
        );
        assert!(!app.scrollbar_active());
        assert!(!app.scrollbar_visible(app.viewport.0, app.viewport.1));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            79,
            1,
            KeyModifiers::empty(),
        );
        assert!(app.scrollbar_drag.is_none());
    }
}

#[test]
fn resize_reuses_capture_with_current_geometry_and_fit_releases_it() {
    let mut app = ready();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        79,
        0,
        KeyModifiers::empty(),
    );
    app.update(AppEvent::TerminalSize {
        width: 100,
        height: 30,
    });
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 100, 30));
    let prepared = crate::ui::transcript::prepare_conversation(&app, screen.content.width);
    app.update(AppEvent::Viewport {
        total_lines: prepared.total_rows(),
        visible_rows: screen.transcript.height as usize,
    });
    app.update(AppEvent::ConversationPrepared(prepared));
    assert!(app.scrollbar_drag.is_some());
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        99,
        29,
        KeyModifiers::empty(),
    );
    assert!(app.active_view().unwrap().scroll.follow_tail);
    app.update(AppEvent::Viewport {
        total_lines: 1,
        visible_rows: screen.transcript.height as usize,
    });
    assert!(app.scrollbar_drag.is_none());
    assert!(!app.scrollbar_active());
}

#[test]
fn marker_click_uses_prepared_geometry_before_viewport_receipt() {
    let mut app = ready();
    app.active_session_mut().unwrap().scroll.follow_tail = false;
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    app.viewport = (0, screen.transcript.height as usize);
    let indicator = crate::ui::transcript::marker_area(screen.transcript, "↑ scroll position");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        indicator.x,
        indicator.y,
        KeyModifiers::empty(),
    );
    assert!(app.active_view().unwrap().scroll.follow_tail);
    assert!(app.selection.is_none());
}

#[test]
fn wheel_uses_current_prepared_extent_before_viewport_receipt() {
    let mut app = ready();
    let mut prepared = crate::ui::transcript::prepare_conversation(&app, 77);
    let mut base = prepared.lines();
    base.extend((0..100).map(|_| ratatui::text::Line::from("growth")));
    prepared.set_test_rows(base.len());
    let maximum = prepared.total_rows() - app.viewport.1;
    app.update(AppEvent::ConversationPrepared(prepared));
    mouse(
        &mut app,
        MouseEventKind::ScrollUp,
        4,
        1,
        KeyModifiers::empty(),
    );
    assert_eq!(app.active_view().unwrap().scroll.offset, maximum - 1);
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) {
    app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })));
}

#[test]
fn pi_wheel_steps_and_page_overlap_preserve_prepared_rows() {
    let mut app = ready();
    let maximum = app.viewport.0 - app.viewport.1;
    let rows = app.prepared_conversation(77).unwrap().history_ptr();
    mouse(
        &mut app,
        MouseEventKind::ScrollUp,
        4,
        1,
        KeyModifiers::empty(),
    );
    assert_eq!(app.active_view().unwrap().scroll.offset, maximum - 1);
    mouse(&mut app, MouseEventKind::ScrollUp, 4, 1, KeyModifiers::ALT);
    assert_eq!(app.active_view().unwrap().scroll.offset, maximum - 6);
    app.update(AppEvent::Terminal(CrosstermEvent::Key(
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::PageUp, KeyModifiers::empty()),
    )));
    assert_eq!(
        app.active_view().unwrap().scroll.offset,
        maximum - 6 - app.viewport.1.saturating_sub(4).max(1)
    );
    assert_eq!(app.prepared_conversation(77).unwrap().history_ptr(), rows);
}

#[test]
fn pi_auto_hide_hover_and_idle_deadline() {
    let mut app = ready();
    let elapsed = Arc::new(AtomicU64::new(0));
    let clock = Arc::clone(&elapsed);
    let base = Instant::now();
    app.monotonic_now =
        Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
    assert!(!app.scrollbar_visible(app.viewport.0, app.viewport.1));
    assert_eq!(app.next_tick(), None);
    mouse(
        &mut app,
        MouseEventKind::ScrollUp,
        4,
        1,
        KeyModifiers::empty(),
    );
    assert!(app.scrollbar_visible(app.viewport.0, app.viewport.1));
    assert_eq!(app.next_tick(), Some(Duration::from_secs(1)));
    elapsed.store(999, Ordering::Relaxed);
    app.update(AppEvent::Tick);
    assert!(app.scrollbar_visible(app.viewport.0, app.viewport.1));
    elapsed.store(1000, Ordering::Relaxed);
    app.update(AppEvent::Tick);
    assert!(!app.scrollbar_visible(app.viewport.0, app.viewport.1));
    assert_eq!(app.next_tick(), None);
    mouse(
        &mut app,
        MouseEventKind::Moved,
        79,
        1,
        KeyModifiers::empty(),
    );
    assert!(app.scrollbar_active());
    assert_eq!(app.next_tick(), None);
    app.update(AppEvent::Rendered);
    mouse(
        &mut app,
        MouseEventKind::Moved,
        79,
        2,
        KeyModifiers::empty(),
    );
    assert!(!app.dirty);
    elapsed.store(6000, Ordering::Relaxed);
    app.update(AppEvent::Tick);
    assert!(app.scrollbar_visible(app.viewport.0, app.viewport.1));
    mouse(
        &mut app,
        MouseEventKind::Moved,
        78,
        1,
        KeyModifiers::empty(),
    );
    assert_eq!(app.next_tick(), Some(Duration::from_secs(1)));
    elapsed.store(7000, Ordering::Relaxed);
    app.update(AppEvent::Tick);
    assert!(!app.scrollbar_visible(app.viewport.0, app.viewport.1));
    assert_eq!(app.next_tick(), None);
}

#[test]
fn pi_track_click_and_drag_are_live_release_does_not_recalculate() {
    let mut app = ready();
    let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        79,
        0,
        KeyModifiers::empty(),
    );
    assert_eq!(app.active_view().unwrap().scroll.offset, 0);
    assert!(!app.active_view().unwrap().scroll.follow_tail);
    assert!(app.scrollbar_drag.is_some());
    let middle = screen.transcript.height / 2;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        20,
        middle,
        KeyModifiers::empty(),
    );
    let offset = app.active_view().unwrap().scroll.offset;
    assert!(offset > 0 && offset < app.viewport.0 - app.viewport.1);
    app.update(AppEvent::Rendered);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        20,
        middle,
        KeyModifiers::empty(),
    );
    assert!(!app.dirty, "same pointer position must not redraw");
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        20,
        0,
        KeyModifiers::empty(),
    );
    assert_eq!(app.active_view().unwrap().scroll.offset, offset);
    assert!(app.scrollbar_drag.is_none());
}

#[test]
fn boundary_wheel_is_noop_and_does_not_reveal_scrollbar() {
    let mut app = ready();
    let rows = app.prepared_conversation(77).unwrap().history_ptr();
    for _ in 0..100 {
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            4,
            1,
            KeyModifiers::empty(),
        );
        assert!(!app.dirty);
    }
    assert!(app.active_view().unwrap().scroll.follow_tail);
    assert!(!app.scrollbar_visible(app.viewport.0, app.viewport.1));
    assert_eq!(app.next_tick(), None);
    assert_eq!(app.prepared_conversation(77).unwrap().history_ptr(), rows);
}

#[test]
fn viewport_growth_keeps_capture_and_drag_uses_new_geometry() {
    let mut app = ready();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        79,
        0,
        KeyModifiers::empty(),
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        79,
        6,
        KeyModifiers::empty(),
    );
    let before = app.active_view().unwrap().scroll.offset;
    let mut prepared = crate::ui::transcript::prepare_conversation(&app, 77);
    let mut base = prepared.lines();
    base.extend((0..500).map(|_| ratatui::text::Line::from("synthetic growth")));
    prepared.set_test_rows(base.len());
    let total = prepared.total_rows();
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: app.viewport.1,
    });
    assert!(app.scrollbar_drag.is_some());
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        79,
        6,
        KeyModifiers::empty(),
    );
    assert!(app.active_view().unwrap().scroll.offset > before);
    let after = app.active_view().unwrap().scroll.offset;
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        79,
        0,
        KeyModifiers::empty(),
    );
    assert_eq!(app.active_view().unwrap().scroll.offset, after);
}

#[test]
fn app_scroll_and_visibility_trace_matches_pi() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/pi-scrollbar-0.85.1.json"
    ))
    .unwrap();
    let mut app = ready();
    let elapsed = Arc::new(AtomicU64::new(0));
    let clock = Arc::clone(&elapsed);
    let base = Instant::now();
    app.monotonic_now =
        Arc::new(move || base + Duration::from_millis(clock.load(Ordering::Relaxed)));
    for step in fixture["visibility"].as_array().unwrap() {
        elapsed.store(step["now"].as_u64().unwrap(), Ordering::Relaxed);
        match step["action"].as_str().unwrap() {
            "layout" => {
                let total = step["value"][0].as_u64().unwrap() as usize;
                let visible = step["value"][1].as_u64().unwrap() as usize;
                let screen = crate::ui::layout::screen_layout(
                    &app,
                    Rect::new(0, 0, app.terminal_size.0, app.terminal_size.1),
                );
                let chrome = app.terminal_size.1 - screen.transcript.height;
                app.update(AppEvent::TerminalSize {
                    width: 80,
                    height: visible as u16 + chrome,
                });
                let mut prepared = crate::ui::transcript::prepare_conversation(&app, 77);
                prepared.set_test_rows(total);
                app.update(AppEvent::Viewport {
                    total_lines: total,
                    visible_rows: visible,
                });
                app.update(AppEvent::ConversationPrepared(prepared));
            }
            "by" => app.transcript_scroll(step["value"].as_i64().unwrap() as i32),
            "active" => {
                let column = if step["value"].as_bool().unwrap() {
                    79
                } else {
                    4
                };
                mouse(
                    &mut app,
                    MouseEventKind::Moved,
                    column,
                    1,
                    KeyModifiers::empty(),
                );
            }
            "wait" => {
                app.update(AppEvent::Tick);
            }
            _ => unreachable!(),
        }
        let view = app.active_view().unwrap();
        let offset = if view.scroll.follow_tail {
            app.viewport.0.saturating_sub(app.viewport.1)
        } else {
            view.scroll.offset
        };
        assert_eq!(offset as u64, step["offset"].as_u64().unwrap(), "{step}");
        assert_eq!(
            view.scroll.follow_tail,
            step["following"].as_bool().unwrap(),
            "{step}"
        );
        assert_eq!(
            app.scrollbar_visible(app.viewport.0, app.viewport.1),
            step["visible"].as_bool().unwrap(),
            "{step}"
        );
    }
}
