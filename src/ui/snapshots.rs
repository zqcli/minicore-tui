//! Stable text snapshots of the fullscreen renderer (an insta-equivalent
//! built into the crate: offline, no external tool). Fixtures are always
//! built through `App::update`; the artifacts live in `snapshots/*.txt`.
//! Regenerate with `MCT_UPDATE_SNAPSHOTS=1 cargo test --locked`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::App;
use crate::event::AppEvent;
use crate::theme::ThemeKind;
use crate::ui::{render, testapp};

fn capture(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    let rows: Vec<String> = buffer
        .content()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    rows.join("\n") + "\n"
}

fn snapshot(app: &App, name: &str, width: u16, height: u16) {
    let actual = capture(app, width, height);
    let dir = format!("{}/snapshots", env!("CARGO_MANIFEST_DIR"));
    let path = format!("{dir}/{name}.txt");
    if std::env::var_os("MCT_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| "<missing>".to_owned());
    assert_eq!(
        expected, actual,
        "\n--- snapshot `{name}` differs ---\nACTUAL:\n{actual}\n--- EXPECTED:\n{expected}\n"
    );
}

#[test]
fn empty_dark_80x24() {
    snapshot(&testapp::fresh(ThemeKind::Dark), "empty_dark_80x24", 80, 24);
}

#[test]
fn empty_light_80x24() {
    snapshot(
        &testapp::fresh(ThemeKind::Light),
        "empty_light_80x24",
        80,
        24,
    );
}

#[test]
fn chat_dark_80x24() {
    snapshot(&testapp::chat(ThemeKind::Dark), "chat_dark_80x24", 80, 24);
}

#[test]
fn chat_light_80x24() {
    snapshot(&testapp::chat(ThemeKind::Light), "chat_light_80x24", 80, 24);
}

#[test]
fn reasoning_hidden_80x24() {
    let mut app = testapp::chat_with_reasoning(ThemeKind::Dark);
    app.update(AppEvent::ToggleReasoning);
    snapshot(&app, "reasoning_hidden_80x24", 80, 24);
}

#[test]
fn tools_80x24() {
    snapshot(&testapp::tools(ThemeKind::Dark), "tools_80x24", 80, 24);
}

#[test]
fn running_gap_80x24() {
    snapshot(
        &testapp::live_turn(ThemeKind::Dark),
        "running_gap_80x24",
        80,
        24,
    );
}

#[test]
fn scroll_top_80x24() {
    snapshot(
        &testapp::scrolled(ThemeKind::Dark),
        "scroll_top_80x24",
        80,
        24,
    );
}

#[test]
fn cjk_80x24() {
    snapshot(&testapp::cjk(ThemeKind::Dark), "cjk_80x24", 80, 24);
}

#[test]
fn wide_120x40() {
    snapshot(&testapp::chat(ThemeKind::Dark), "wide_120x40", 120, 40);
}

#[test]
fn small_50x10() {
    snapshot(&testapp::chat(ThemeKind::Dark), "small_50x10", 50, 10);
}

#[test]
fn new_session_dark_80x24() {
    snapshot(
        &testapp::new_session(ThemeKind::Dark),
        "new_session_dark_80x24",
        80,
        24,
    );
}

#[test]
fn new_session_light_120x40() {
    snapshot(
        &testapp::new_session(ThemeKind::Light),
        "new_session_light_120x40",
        120,
        40,
    );
}

#[test]
fn model_selector_dark_80x24() {
    snapshot(
        &testapp::model_selector(ThemeKind::Dark),
        "model_selector_dark_80x24",
        80,
        24,
    );
}

#[test]
fn model_selector_light_120x40() {
    snapshot(
        &testapp::model_selector(ThemeKind::Light),
        "model_selector_light_120x40",
        120,
        40,
    );
}

#[test]
fn reasoning_selector_dark_80x24() {
    snapshot(
        &testapp::reasoning_selector(ThemeKind::Dark),
        "reasoning_selector_dark_80x24",
        80,
        24,
    );
}

#[test]
fn session_selector_dark_80x24() {
    snapshot(
        &testapp::session_selector(ThemeKind::Dark),
        "session_selector_dark_80x24",
        80,
        24,
    );
}

#[test]
fn session_selector_light_120x40() {
    snapshot(
        &testapp::session_selector(ThemeKind::Light),
        "session_selector_light_120x40",
        120,
        40,
    );
}

#[test]
fn profile_selector_dark_80x24() {
    snapshot(
        &testapp::profile_selector(ThemeKind::Dark),
        "profile_selector_dark_80x24",
        80,
        24,
    );
}

#[test]
fn empty_model_search_dark_80x24() {
    snapshot(
        &testapp::empty_model_search(ThemeKind::Dark),
        "empty_model_search_dark_80x24",
        80,
        24,
    );
}

#[test]
fn narrow_model_selector_dark_60x16() {
    snapshot(
        &testapp::narrow_selector(ThemeKind::Dark),
        "narrow_model_selector_dark_60x16",
        60,
        16,
    );
}

#[test]
fn help_dark_80x24() {
    snapshot(&testapp::help(ThemeKind::Dark), "help_dark_80x24", 80, 24);
}

#[test]
fn help_light_120x40() {
    snapshot(
        &testapp::help(ThemeKind::Light),
        "help_light_120x40",
        120,
        40,
    );
}

#[test]
fn logs_dark_80x24() {
    snapshot(&testapp::logs(ThemeKind::Dark), "logs_dark_80x24", 80, 24);
}

#[test]
fn multiline_composer_dark_80x24() {
    snapshot(
        &testapp::multiline_composer(ThemeKind::Dark),
        "multiline_composer_dark_80x24",
        80,
        24,
    );
}

#[test]
fn search_query_dark_80x24() {
    snapshot(
        &testapp::search_query(ThemeKind::Dark),
        "search_query_dark_80x24",
        80,
        24,
    );
}

#[test]
fn new_output_marker_dark_80x24() {
    snapshot(
        &testapp::new_output_marker(ThemeKind::Dark),
        "new_output_marker_dark_80x24",
        80,
        24,
    );
}

#[test]
fn unsaved_gap_dark_80x24() {
    let app = testapp::unsaved_gap(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("UNSAVED TURN"));
    assert!(cap.contains("This turn finished, but the Agent did not confirm saving it."));
    assert!(cap.contains("The session is blocked. Tool side effects may already exist."));
    assert!(
        cap.contains(
            "Closing releases this result; reopening reads whatever the Store can recover."
        )
    );
    assert!(cap.contains("Some live output may be missing."));
    snapshot(&app, "unsaved_gap_dark_80x24", 80, 24);
}

#[test]
fn unsaved_gap_light_120x40() {
    let app = testapp::unsaved_gap(ThemeKind::Light);
    let cap = capture(&app, 120, 40);
    assert!(cap.contains("UNSAVED TURN"));
    assert!(cap.contains("This turn finished, but the Agent did not confirm saving it."));
    snapshot(&app, "unsaved_gap_light_120x40", 120, 40);
}

#[test]
fn unsaved_gap_dark_60x16() {
    let app = testapp::unsaved_gap(ThemeKind::Dark);
    let cap = capture(&app, 60, 16);
    assert!(cap.contains("UNSAVED TURN"));
    assert!(cap.contains("This turn finished"));
    snapshot(&app, "unsaved_gap_dark_60x16", 60, 16);
}

#[test]
fn unsaved_gap_dark_160x50() {
    let app = testapp::unsaved_gap(ThemeKind::Dark);
    let cap = capture(&app, 160, 50);
    assert!(cap.contains("UNSAVED TURN"));
    assert!(cap.contains("This turn finished, but the Agent did not confirm saving it."));
    snapshot(&app, "unsaved_gap_dark_160x50", 160, 50);
}

#[test]
fn steering_dark_80x24() {
    let app = testapp::steering(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("Steering"));
    assert!(cap.contains("Focus on memory safety instead"));
    snapshot(&app, "steering_dark_80x24", 80, 24);
}

#[test]
fn steering_light_120x40() {
    let app = testapp::steering(ThemeKind::Light);
    let cap = capture(&app, 120, 40);
    assert!(cap.contains("Steering"));
    snapshot(&app, "steering_light_120x40", 120, 40);
}

#[test]
fn steering_dark_60x16() {
    let app = testapp::steering(ThemeKind::Dark);
    let cap = capture(&app, 60, 16);
    assert!(cap.contains("Steering"));
    snapshot(&app, "steering_dark_60x16", 60, 16);
}

#[test]
fn steering_dark_160x50() {
    let app = testapp::steering(ThemeKind::Dark);
    let cap = capture(&app, 160, 50);
    assert!(cap.contains("Steering"));
    snapshot(&app, "steering_dark_160x50", 160, 50);
}

#[test]
fn pending_model_dark_80x24() {
    let app = testapp::pending_model(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("claude-3-7-sonnet") || cap.contains("applies at next"));
    snapshot(&app, "pending_model_dark_80x24", 80, 24);
}

#[test]
fn pending_model_light_120x40() {
    let app = testapp::pending_model(ThemeKind::Light);
    let cap = capture(&app, 120, 40);
    assert!(cap.contains("claude-3-7-sonnet") || cap.contains("applies at next"));
    snapshot(&app, "pending_model_light_120x40", 120, 40);
}

#[test]
fn pending_model_dark_60x16() {
    let app = testapp::pending_model(ThemeKind::Dark);
    let cap = capture(&app, 60, 16);
    assert!(cap.contains("claude-3-7-sonnet") || cap.contains("applies at next"));
    snapshot(&app, "pending_model_dark_60x16", 60, 16);
}

#[test]
fn finishing_dark_80x24() {
    let app = testapp::finishing(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("Saving turn…") || cap.contains("finishing") || cap.contains("Finishing"));
    snapshot(&app, "finishing_dark_80x24", 80, 24);
}

#[test]
fn finishing_light_120x40() {
    let app = testapp::finishing(ThemeKind::Light);
    let cap = capture(&app, 120, 40);
    assert!(cap.contains("Saving turn…") || cap.contains("finishing") || cap.contains("Finishing"));
    snapshot(&app, "finishing_light_120x40", 120, 40);
}

#[test]
fn close_user_dark_80x24() {
    let app = testapp::close_user(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("/close confirm"));
    assert!(cap.contains("cancelled (user)"));
    assert!(cap.contains("persisted"));
    snapshot(&app, "close_user_dark_80x24", 80, 24);
}

#[test]
fn close_user_light_120x40() {
    let app = testapp::close_user(ThemeKind::Light);
    let cap = capture(&app, 120, 40);
    assert!(cap.contains("/close confirm"));
    assert!(cap.contains("cancelled (user)"));
    assert!(cap.contains("persisted"));
    snapshot(&app, "close_user_light_120x40", 120, 40);
}

#[test]
fn unknown_cancel_result_dark_80x24() {
    let app = testapp::unknown_cancel_result(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("cancelled (sandbox_evicted)"));
    assert!(cap.contains("persisted"));
    snapshot(&app, "unknown_cancel_result_dark_80x24", 80, 24);
}

#[test]
fn shutdown_cancel_result_dark_80x24() {
    let app = testapp::shutdown_cancel_result(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("cancelled (shutdown)"));
    assert!(cap.contains("persisted"));
    snapshot(&app, "shutdown_cancel_result_dark_80x24", 80, 24);
}

#[test]
fn store_error_dark_80x24() {
    let app = testapp::store_error(ThemeKind::Dark);
    let cap = capture(&app, 80, 24);
    assert!(cap.contains("Unable to open this session") || cap.contains("unsupported format"));
    snapshot(&app, "store_error_dark_80x24", 80, 24);
}

#[test]
fn store_error_light_120x40() {
    let app = testapp::store_error(ThemeKind::Light);
    let cap = capture(&app, 120, 40);
    assert!(cap.contains("Unable to open this session") || cap.contains("unsupported format"));
    snapshot(&app, "store_error_light_120x40", 120, 40);
}

#[test]
fn store_error_dark_60x16() {
    let app = testapp::store_error(ThemeKind::Dark);
    let cap = capture(&app, 60, 16);
    assert!(cap.contains("Unable to open") || cap.contains("unsupported"));
    snapshot(&app, "store_error_dark_60x16", 60, 16);
}
