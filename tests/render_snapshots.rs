//! Integration-level render checks against the crate's stable snapshots
//! (`snapshots/*.txt`). These reuse the production `ui::render` + ratatui
//! `TestBackend` path used by the in-crate snapshot suite and pin it from
//! the outside of the crate, so a public-API change cannot silently drift
//! the fullscreen layout. Snapshots are regenerated with
//! `MCT_UPDATE_SNAPSHOTS=1` in the in-crate suite; this file only compares.

use std::path::PathBuf;

use minicore_tui::app::App;
use minicore_tui::event::AppEvent;
use minicore_tui::state::Dock;
use minicore_tui::theme::ThemeKind;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn capture(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| minicore_tui::ui::render(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn snapshot(name: &str, width: u16, height: u16) {
    let actual = capture(&snapshot_app(name), width, height);
    let expected = std::fs::read_to_string(format!(
        "{}/snapshots/{name}_{width}x{height}.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|_| "<missing>".to_owned());
    assert_eq!(
        expected, actual,
        "\n--- integration snapshot `{name}` differs ---\n"
    );
}

/// Rebuilds the scene through public API only. The empty scenes are pure
/// theme; the help panel is staged directly because the renderer (not the
/// input flow) is what this file pins.
fn snapshot_app(name: &str) -> App {
    match name {
        "empty_dark" => App::new(PathBuf::from("/project")),
        "empty_light" => {
            let mut app = App::new(PathBuf::from("/project"));
            app.update(AppEvent::SetTheme(ThemeKind::Light));
            app
        }
        "help_dark" => {
            let mut app = App::new(PathBuf::from("/project"));
            app.update(AppEvent::SetTheme(ThemeKind::Dark));
            app.dock = Dock::Help;
            app
        }
        other => panic!("unknown integration snapshot scene `{other}`"),
    }
}

#[test]
fn empty_dark_scene_matches_the_committed_snapshot() {
    snapshot("empty_dark", 80, 24);
}

#[test]
fn empty_light_scene_matches_the_committed_snapshot() {
    snapshot("empty_light", 80, 24);
}

#[test]
fn help_dark_scene_matches_the_committed_snapshot() {
    snapshot("help_dark", 80, 24);
}

#[test]
fn render_is_read_only_the_app_is_left_untouched() {
    let mut app = App::new(PathBuf::from("/project"));
    app.update(AppEvent::SetTheme(ThemeKind::Dark));
    let snapshot_bytes = capture(&app, 80, 24);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    // Two draws must not change app state (the renderer never writes).
    terminal
        .draw(|frame| minicore_tui::ui::render(frame, &app))
        .unwrap();
    terminal
        .draw(|frame| minicore_tui::ui::render(frame, &app))
        .unwrap();
    assert!(app.dirty, "rendering must not touch the dirty flag");
    assert_eq!(capture(&app, 80, 24), snapshot_bytes);
}
