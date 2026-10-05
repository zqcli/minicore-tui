//! Dock feedback contracts through production layout/rendering, including
//! crowded terminals and cached transcript reuse. Optional captures contain
//! synthetic fixtures only, never a real session or provider request.

use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Modifier};

use crate::{
    app::{App, NoticeLevel},
    event::AppEvent,
    protocol::{CompactionPhaseWire, CompactionProgressWire, SessionStatusWire},
    state::{
        selection::Dock,
        turn::{SteerQueueItem, SteerQueueState},
    },
    theme::{Theme, ThemeKind},
    ui::{
        component_tests::{buffer_lines, draw},
        feedback, layout, testapp, transcript,
    },
};

pub(super) fn compacting(theme: ThemeKind, phase: CompactionPhaseWire) -> App {
    let items = (0..24)
        .map(|i| testapp::user_entry(i, &format!("loop_{i}"), &format!("Message {i}")))
        .collect();
    let mut app = testapp::open_with(theme, "ses_1", Some("Phase preview"), "high", items);
    let state = app
        .sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .state
        .as_mut()
        .unwrap();
    state.status = SessionStatusWire::Finishing;
    state.compaction = Some(CompactionProgressWire {
        operation_id: "phase-preview".into(),
        phase,
        covered_item_count: 20,
        retained_item_count: 4,
    });
    app
}

fn assert_blank(terminal: &Terminal<TestBackend>, area: Rect) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            assert_eq!(
                terminal.backend().buffer()[(x, y)].symbol(),
                " ",
                "blank at {x},{y}"
            );
        }
    }
}

fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(dir) = std::env::var_os("MCT_FEEDBACK_CAPTURE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.txt")),
        buffer_lines(terminal).join("\n") + "\n",
    )
    .unwrap();
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer
        .content()
        .iter()
        .map(|cell| {
            serde_json::json!({
                "text": cell.symbol(), "fg": format!("{:?}", cell.fg),
                "bg": format!("{:?}", cell.bg), "modifiers": format!("{:?}", cell.modifier)
            })
        })
        .collect();
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "width": buffer.area.width, "height": buffer.area.height, "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn phases_share_tail_editor_spacing_and_style_at_supported_sizes() {
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        let theme = Theme::for_kind(kind);
        for (width, height) in [(60, 16), (80, 24), (120, 40)] {
            for phase in [
                CompactionPhaseWire::Preparing,
                CompactionPhaseWire::Summarizing,
                CompactionPhaseWire::Merging,
                CompactionPhaseWire::Committing,
            ] {
                let app = compacting(kind, phase);
                let screen = layout::screen_layout(&app, Rect::new(0, 0, width, height));
                let status = screen.status.unwrap();
                let terminal = draw(&app, width, height);
                let label = format!(
                    "Compacting · {}",
                    super::status::compaction_phase_label(phase)
                );
                assert!(buffer_lines(&terminal)[status.y as usize].contains(&label));
                assert_eq!(screen.panel.y, status.bottom() + 1);
                assert_eq!(screen.panel.height, 4);
                assert_blank(
                    &terminal,
                    Rect::new(status.x, status.y - 1, status.width, 1),
                );
                assert_blank(
                    &terminal,
                    Rect::new(status.x, status.bottom(), status.width, 1),
                );
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(status.x, status.y)].fg, theme.accent);
                assert_eq!(buffer[(status.x + 2, status.y)].fg, theme.muted);
                assert!(
                    !buffer[(status.x + 2, status.y)]
                        .modifier
                        .intersects(Modifier::BOLD | Modifier::DIM)
                );
                assert_eq!(
                    buffer[(screen.panel.x, screen.panel.y)].bg,
                    theme.user_message_bg
                );
                capture(&terminal, &format!("{kind:?}-{phase:?}-{width}x{height}"));
            }
        }
    }
}

#[test]
fn notices_and_busy_feedback_share_one_gap_and_keep_severity() {
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        let theme = Theme::for_kind(kind);
        for (width, height) in [(60, 16), (80, 24), (120, 40)] {
            for busy in [false, true] {
                for (level, color) in [
                    (NoticeLevel::Info, theme.muted),
                    (NoticeLevel::Warning, theme.warning),
                    (NoticeLevel::Error, theme.error),
                ] {
                    let mut app = compacting(kind, CompactionPhaseWire::Summarizing);
                    if !busy {
                        let state = app
                            .sessions
                            .known
                            .get_mut("ses_1")
                            .unwrap()
                            .state
                            .as_mut()
                            .unwrap();
                        state.status = SessionStatusWire::Idle;
                        state.compaction = None;
                    }
                    app.notice(level, "Review this notice");
                    let screen = layout::screen_layout(&app, Rect::new(0, 0, width, height));
                    let notice = screen.notice.unwrap();
                    let terminal = draw(&app, width, height);
                    assert_eq!(screen.panel.y, notice.bottom() + 1);
                    assert_eq!(terminal.backend().buffer()[(notice.x, notice.y)].fg, color);
                    assert_blank(
                        &terminal,
                        Rect::new(notice.x, notice.bottom(), notice.width, 1),
                    );
                    let first = screen.status.unwrap_or(notice);
                    assert_blank(&terminal, Rect::new(first.x, first.y - 1, first.width, 1));
                    if let Some(status) = screen.status {
                        assert_eq!(notice.y, status.bottom());
                    }
                    capture(
                        &terminal,
                        &format!("{kind:?}-{level:?}-busy{busy}-{width}x{height}"),
                    );
                }
            }
        }
    }
}

#[test]
fn crowded_queue_and_tall_panels_keep_usable_editor_and_footer() {
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        for (width, height) in [(60, 16), (80, 24), (120, 40)] {
            for tall_panel in [false, true] {
                let mut app = compacting(kind, CompactionPhaseWire::Committing);
                app.notice(NoticeLevel::Warning, "Check queued messages");
                for local_id in 0..8 {
                    app.sessions
                        .known
                        .get_mut("ses_1")
                        .unwrap()
                        .steer_queue
                        .push(SteerQueueItem {
                            local_id,
                            text: format!("Queued {local_id}"),
                            state: SteerQueueState::Unsent,
                            editor_revision: None,
                            handoff: false,
                        });
                }
                app.composer_mut().set_text(&"draft line\n".repeat(20));
                if tall_panel {
                    app.open_settings();
                }
                let screen = layout::screen_layout(&app, Rect::new(0, 0, width, height));
                let terminal = draw(&app, width, height);
                assert!(screen.panel.height >= 4);
                assert!(screen.footer.height >= 1);
                assert_eq!(screen.footer.bottom(), height);
                assert!(screen.panel.bottom() <= screen.footer.y);
                for rect in [
                    screen.transcript,
                    screen.panel,
                    screen.footer,
                    screen.status.unwrap(),
                    screen.notice.unwrap(),
                ] {
                    assert!(rect.right() <= width && rect.bottom() <= height);
                }
                if let Some(queue) = screen.queue {
                    assert_eq!(screen.status.unwrap().y, queue.bottom() + 1);
                }
                if tall_panel && height == 16 {
                    assert_eq!(
                        screen.panel.y,
                        screen.notice.unwrap().bottom(),
                        "no spare row: gap collapses"
                    );
                }
                capture(
                    &terminal,
                    &format!("{kind:?}-crowded-panel{tall_panel}-{width}x{height}"),
                );
            }
        }
    }
}

#[test]
fn idle_tail_separator_is_single_and_notice_changes_reuse_cached_rows() {
    for mut app in [
        compacting(ThemeKind::Dark, CompactionPhaseWire::Preparing),
        testapp::live_turn(ThemeKind::Dark),
    ] {
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.live = None;
        view.state.as_mut().unwrap().status = SessionStatusWire::Idle;
        view.state.as_mut().unwrap().compaction = None;
        let prepared = transcript::prepare_conversation(&app, 77);
        let rows = prepared.lines();
        assert!(layout::line_is_blank(rows.last().unwrap()));
        assert!(!layout::line_is_blank(&rows[rows.len() - 2]));
        assert!(
            prepared
                .copy_ranges
                .row(prepared.total_rows() - 1)
                .is_none()
        );
        app.update(AppEvent::ConversationPrepared(prepared));
        let before = app.prepared_conversation(77).unwrap().total_rows();
        app.notice(NoticeLevel::Info, "A short-lived notice");
        app.notices.back_mut().unwrap().created_at =
            std::time::Instant::now() - std::time::Duration::from_secs(60);
        app.update(AppEvent::Tick);
        assert!(app.notices.is_empty());
        assert_eq!(app.prepared_conversation(77).unwrap().total_rows(), before);
    }
}

#[test]
fn scrolled_history_keeps_navigation_and_feedback_does_not_cover_it() {
    let mut app = compacting(ThemeKind::Dark, CompactionPhaseWire::Merging);
    let view = app.sessions.known.get_mut("ses_1").unwrap();
    view.scroll.follow_tail = false;
    view.scroll.offset = 5;
    view.scroll.new_content = true;
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    let terminal = draw(&app, 80, 24);
    let rows = buffer_lines(&terminal);
    assert!(rows[screen.transcript.bottom() as usize - 1].contains("new output"));
    assert!(rows[screen.status.unwrap().y as usize].contains("Compacting · Merging"));
    assert_eq!(screen.panel.y, screen.status.unwrap().bottom() + 1);
    capture(&terminal, "Dark-scrolled-80x24");
}

#[test]
fn row_helper_handles_empty_narrow_and_control_text_without_changing_neighbor_rows() {
    let mut terminal = Terminal::new(TestBackend::new(8, 3)).unwrap();
    terminal
        .draw(|frame| {
            feedback::render_row(
                frame,
                Rect::new(0, 0, 0, 0),
                None,
                "ignored",
                feedback::neutral_style(&Theme::dark()),
            );
            feedback::render_row(
                frame,
                Rect::new(0, 1, 3, 2),
                None,
                "e\u{301}🙂界",
                feedback::neutral_style(&Theme::dark()),
            );
        })
        .unwrap();
    let rows = buffer_lines(&terminal);
    assert_eq!(rows[1].trim(), "e\u{301}🙂");
    assert!(rows[0].trim().is_empty() && rows[2].trim().is_empty());
    let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
    terminal
        .draw(|frame| {
            feedback::render_row(
                frame,
                Rect::new(0, 0, 20, 1),
                None,
                "ok\u{1b}\r\u{202e}",
                feedback::neutral_style(&Theme::dark()),
            );
            feedback::render_row(
                frame,
                Rect::new(0, 1, 1, 2),
                Some(ratatui::text::Span::styled(
                    "⚠ ",
                    ratatui::style::Style::new().fg(Theme::dark().warning),
                )),
                "hidden",
                feedback::neutral_style(&Theme::dark()),
            );
        })
        .unwrap();
    let rows = buffer_lines(&terminal);
    assert!(rows[0].contains("ok␛␍\\u{202e}"));
    assert!(!rows[0].contains(['\u{1b}', '\r', '\u{202e}']));
    assert_eq!(rows[1].trim(), "⚠");
    assert_eq!(
        terminal.backend().buffer()[(0, 1)].fg,
        Theme::dark().warning
    );
    assert!(rows[2].trim().is_empty());
    let app = testapp::fresh(ThemeKind::Dark);
    let screen = layout::screen_layout(&app, Rect::new(0, 0, 80, 24));
    assert!(screen.status.is_none() && screen.notice.is_none());
    assert_eq!(screen.panel.y, screen.transcript.bottom());
    assert!(matches!(app.dock, Dock::Composer));
}

#[test]
fn live_tools_keep_a_transparent_row_before_the_visible_editor_surface() {
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        let theme = Theme::for_kind(kind);
        for (width, height) in [(60, 16), (80, 24), (120, 40)] {
            for with_notice in [false, true] {
                let mut app = testapp::live_turn(kind);
                if with_notice {
                    app.notice(NoticeLevel::Info, "Notice");
                }
                let screen = layout::screen_layout(&app, Rect::new(0, 0, width, height));
                let terminal = draw(&app, width, height);
                let status = screen.status.unwrap();
                assert!(buffer_lines(&terminal)[status.y as usize].contains("Working"));
                let feedback_end = screen.notice.unwrap_or(status).bottom();
                assert_eq!(screen.panel.y, feedback_end + 1);
                assert_blank(
                    &terminal,
                    Rect::new(screen.panel.x, feedback_end, screen.panel.width, 1),
                );
                let buffer = terminal.backend().buffer();
                for x in screen.panel.x..screen.panel.right() {
                    assert_eq!(buffer[(x, feedback_end)].bg, theme.page_bg);
                }
                assert_eq!(
                    buffer[(screen.panel.x, screen.panel.y)].bg,
                    theme.user_message_bg
                );
                assert_eq!(buffer[(screen.panel.x, screen.panel.y)].symbol(), "▎");
            }
        }
    }
}

#[test]
fn folded_and_expanded_tool_cards_have_no_detail_action_overlay() {
    use crate::state::{
        tool::{ToolFacts, ToolKey},
        transcript::{ToolBlock, TranscriptBlock},
        view::FoldOverride,
    };
    use std::sync::Arc;
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        for (width, height) in [(60, 16), (80, 24), (120, 40)] {
            for live in [false, true] {
                let mut app = if live {
                    testapp::live_turn(kind)
                } else {
                    testapp::open_empty(kind, "ses_1", None, "high")
                };
                let key = ToolKey::new("ses_1", "loop_live", 0, "c1");
                let view = app.sessions.known.get_mut("ses_1").unwrap();
                let mut facts = ToolFacts::new("bash");
                Arc::make_mut(&mut facts.display).detail = "cargo build".into();
                facts.result = Some(Arc::from("BODY MARKER"));
                if live {
                    let request = &mut view.live.as_mut().unwrap().requests[0];
                    request
                        .parts
                        .retain(|part| matches!(part, crate::state::turn::LivePart::Tool { .. }));
                    let tool = &mut request.tools[0];
                    tool.name = "bash".into();
                } else {
                    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
                        index: Some(0),
                        loop_id: "loop_live".into(),
                        request_index: 0,
                        tool_call_id: "c1".into(),
                        name: "bash".into(),
                        result: None,
                        outcome: None,
                        live_status: None,
                        progress: None,
                        expanded: false,
                    }));
                }
                Arc::make_mut(&mut view.tool_presentations).insert(key.clone(), Arc::new(facts));
                for expanded in [false, true, false, true] {
                    let view = app.sessions.known.get_mut("ses_1").unwrap();
                    Arc::make_mut(&mut view.tool_folds).insert(
                        key.clone(),
                        if expanded {
                            FoldOverride::Expanded
                        } else {
                            FoldOverride::Collapsed
                        },
                    );
                    view.transcript.invalidate();
                    let terminal = draw(&app, width, height);
                    let visible = buffer_lines(&terminal).join("\n");
                    assert!(!visible.contains("详情"));
                    assert!(visible.contains("bash") && visible.contains("cargo build"));
                    assert_eq!(visible.contains("BODY MARKER"), expanded, "{visible}");
                }
            }
        }
    }
}
