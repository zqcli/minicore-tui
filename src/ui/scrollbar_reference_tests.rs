use super::*;
use serde_json::{Value, json};

#[test]
fn overlay_edges_clear_wide_glyphs_but_keep_combining_clusters() {
    use ratatui::{
        buffer::Buffer,
        style::{Color, Style},
    };
    for symbol in ["界", "🙂", "👩‍💻"] {
        for row in [0, 8, 19] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 20));
            let background = Color::Rgb(21, 22, 23);
            buffer.set_string(29, row, symbol, Style::new().bg(background));
            buffer.set_string(40, row, symbol, Style::new().bg(background));
            crate::ui::layout::clear_wide_overlay_edges(&mut buffer, Rect::new(30, row, 11, 1));
            assert_eq!(buffer.cell((29, row)).unwrap().symbol(), " ", "{symbol}");
            assert_eq!(buffer.cell((30, row)).unwrap().bg, background);
            assert_eq!(buffer.cell((41, row)).unwrap().symbol(), " ");
            assert_eq!(buffer.cell((41, row)).unwrap().bg, background);
        }
    }
    let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 20));
    buffer.set_string(29, 0, "e\u{301}", Style::default());
    crate::ui::layout::clear_wide_overlay_edges(&mut buffer, Rect::new(30, 0, 11, 1));
    assert_eq!(buffer.cell((29, 0)).unwrap().symbol(), "e\u{301}");
}

#[test]
fn active_thumb_matches_pi_palette_and_preserves_background_without_wide_artifacts() {
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Modifier, Style},
    };
    for theme in [Theme::dark(), Theme::light()] {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let background = Color::Rgb(21, 22, 23);
        terminal
            .draw(|frame| {
                frame.buffer_mut().set_string(
                    78,
                    0,
                    "界",
                    Style::new()
                        .bg(background)
                        .add_modifier(Modifier::UNDERLINED),
                );
                render(frame, Rect::new(0, 0, 80, 20), 100, 40, &theme, true);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((78, 0)).unwrap().symbol(), " ");
        assert!(
            !buffer
                .cell((78, 0))
                .unwrap()
                .modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(buffer.cell((79, 0)).unwrap().symbol(), "│");
        assert_eq!(buffer.cell((79, 0)).unwrap().fg, theme.scrollbar_track);
        assert_eq!(buffer.cell((79, 0)).unwrap().bg, background);
        assert_eq!(buffer.cell((79, 8)).unwrap().symbol(), "█");
        assert_eq!(buffer.cell((79, 8)).unwrap().fg, theme.scrollbar_thumb);
    }
}

fn number(value: &Value, key: &str) -> usize {
    value[key].as_u64().unwrap() as usize
}

#[test]
fn geometry_and_pointer_mapping_match_released_pi_oracle() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/pi-scrollbar-0.85.1.json"
    ))
    .unwrap();
    for case in fixture["geometry"].as_array().unwrap() {
        let height = number(case, "height");
        let actual = geometry(
            Rect::new(2, 3, 80, height as u16),
            number(case, "total"),
            number(case, "offset"),
        );
        let expected = &case["geometry"];
        if expected.is_null() {
            assert_eq!(actual, None);
            continue;
        }
        let actual = actual.unwrap();
        assert_eq!(
            json!({
                "column": actual.column, "trackTop": actual.track_top,
                "trackHeight": actual.track_height, "thumbTop": actual.thumb_top,
                "thumbHeight": actual.thumb_height, "maxScrollTop": actual.max_scroll_top,
            }),
            *expected,
            "case {case}"
        );
        for drag in case["drag"].as_array().unwrap() {
            assert_eq!(
                scroll_top_at(actual, number(drag, "pointer"), number(drag, "grab")),
                number(drag, "offset"),
                "case {case}"
            );
        }
    }
}

#[test]
fn auto_visibility_matches_released_pi_oracle() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/pi-scrollbar-0.85.1.json"
    ))
    .unwrap();
    let base = Instant::now();
    let mut state = ScrollbarState::default();
    let mut overflow = false;
    let mut offset = 0;
    for step in fixture["visibility"].as_array().unwrap() {
        let now = base + Duration::from_millis(number(step, "now") as u64);
        match step["action"].as_str().unwrap() {
            "layout" => {
                overflow = step["value"][0].as_u64().unwrap() > step["value"][1].as_u64().unwrap();
                if !overflow {
                    state.hide_at = None;
                }
            }
            "active" => {
                state.set_active(step["value"].as_bool().unwrap(), now);
            }
            "by" if overflow && number(step, "offset") != offset => state.activity(now),
            _ => {}
        }
        offset = number(step, "offset");
        assert_eq!(
            overflow && state.visible(now),
            step["visible"].as_bool().unwrap(),
            "step {step}"
        );
        assert_eq!(state.active, step["active"].as_bool().unwrap());
    }
}
