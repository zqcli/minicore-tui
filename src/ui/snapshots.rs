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
