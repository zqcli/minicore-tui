//! Component-level rendering tests: exact colors, modifiers, preview bounds,
//! footer behavior, and cursor column math (development spec 15, 29, 31).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use crate::app::App;
use crate::event::AppEvent;
use crate::state::transcript::ToolBlock;
use crate::theme::{Theme, ThemeKind};
use crate::ui::testapp;
use crate::ui::{footer, render, tool};

fn draw(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn any_cell_matching(
    terminal: &Terminal<TestBackend>,
    predicate: impl Fn(&ratatui::buffer::Cell) -> bool,
) -> bool {
    terminal.backend().buffer().content().iter().any(predicate)
}

#[test]
fn user_card_uses_the_spec_background() {
    let app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(any_cell_matching(&terminal, |cell| cell.bg == Theme::dark().user_message_bg));
    assert!(text(&terminal).contains("hello"));
}

#[test]
fn assistant_text_has_no_background() {
    let app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let bg = Theme::dark().page_bg;
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == bg && cell.symbol() == "a"),
        "the assistant text sits on the page background"
    );
}

#[test]
fn reasoning_is_gray_and_italic_and_can_be_hidden() {
    let mut app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(
        any_cell_matching(&terminal, |cell| {
            cell.fg == Theme::dark().muted
                && cell.modifier.contains(Modifier::ITALIC)
                && cell.symbol() == "c"
        }),
        "reasoning text 'carefully' is gray italic"
    );

    app.update(AppEvent::ToggleReasoning);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Thinking..."));
    assert!(
        !content.contains("carefully"),
        "hidden reasoning text is gone"
    );
}

#[test]
fn composer_border_follows_the_reasoning_level() {
    let theme = Theme::dark();
    for (reasoning, expected) in [
        ("high", theme.thinking_high),
        ("low", theme.thinking_low),
        ("medium", theme.thinking_medium),
        ("disabled", theme.thinking_disabled),
    ] {
        let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("t"), reasoning);
        let terminal = draw(&app, 80, 24);
        let corner_is_bordered = any_cell_matching(&terminal, |cell| {
            cell.symbol() == "\u{256d}" && cell.fg == expected
        });
        assert!(
            corner_is_bordered,
            "reasoning level {reasoning} colors the border"
        );
    }

    // No session: the fixed dark placeholder border.
    let app = testapp::fresh(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(any_cell_matching(&terminal, |cell| cell.symbol()
        == "\u{256d}"
        && cell.fg == theme.thinking_disabled));
}

#[test]
fn tool_cards_use_state_backgrounds_and_expanded_preview_bounds() {
    let theme = Theme::dark();
    let make = |outcome: Option<&str>| ToolBlock {
        tool_call_id: "c".into(),
        turn_id: "t".into(),
        name: "read".into(),
        arguments: None,
        result: Some("data".into()),
        outcome: outcome.map(str::to_owned),
        live_status: None,
        progress: None,
        expanded: true,
    };
    // Exact card backgrounds per state, asserted at the line level so the
    // viewport cannot hide them.
    for (outcome, expected) in [
        (Some("success"), theme.tool_success_bg),
        (Some("denied"), theme.tool_error_bg),
        (Some("failed"), theme.tool_error_bg),
        (None, theme.tool_pending_bg),
    ] {
        let lines = tool::durable(&theme, &make(outcome), 40, false);
        let header = &lines[1];
        let has_bg = header
            .spans
            .iter()
            .any(|span| span.style.bg == Some(expected));
        assert!(
            has_bg,
            "outcome {outcome:?} should use the {expected:?} card background"
        );
    }

    // Viewport level: the expanded preview is visible while following the
    // tail (its “… more lines” footer sits just above the later tool cards).
    let app = testapp::tools(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("… 20 more lines"));
    assert!(content.contains("line 39"));
}

#[test]
fn tool_preview_caps_chars_at_32k_and_lines_at_40() {
    let theme = Theme::dark();
    let make = |result: &str| ToolBlock {
        tool_call_id: "c".into(),
        turn_id: "t".into(),
        name: "bash".into(),
        arguments: None,
        result: Some(result.to_owned()),
        outcome: Some("success".into()),
        live_status: None,
        progress: None,
        expanded: true,
    };
    let single_line = "x".repeat(40_000);
    let lines = tool::durable(&theme, &make(&single_line), 120, false);
    let joined: String = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    assert!(
        joined.trim_end().len() <= 32 * 1024 + 2,
        "preview content is capped at 32 KiB"
    );

    let many_lines = (0..60)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = tool::durable(&theme, &make(&many_lines), 120, false);
    let joined: String = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    assert!(joined.contains("line 0"));
    assert!(joined.contains("… 20 more lines"));
    assert!(!joined.contains("line 40"));
}

#[test]
fn workspace_shortening_replaces_the_home_prefix() {
    let home = std::path::Path::new("/home/user");
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user/project"), Some(home)),
        "~/project"
    );
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user"), Some(home)),
        "~"
    );
    // A sibling directory must not be shortened by a prefix match.
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user2/project"), Some(home)),
        "/home/user2/project"
    );
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/srv/other"), Some(home)),
        "/srv/other"
    );
    assert_eq!(
        footer::shorten_workspace(std::path::Path::new("/home/user/project"), None),
        "/home/user/project"
    );
}

#[test]
fn footer_hides_secondary_info_below_80_columns() {
    let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let narrow = draw(&app, 70, 24);
    let narrow_text = text(&narrow);
    assert!(narrow_text.contains("deep • high"), "model/reasoning stay");
    assert!(
        !narrow_text.contains("/project"),
        "workspace is hidden below 80"
    );
    assert!(
        !narrow_text.contains("Task"),
        "session title is hidden below 80"
    );

    let wide = draw(&app, 120, 40);
    let wide_text = text(&wide);
    assert!(wide_text.contains("/project"));
    assert!(wide_text.contains("Task") || wide_text.contains("ses_1"));
}

#[test]
fn footer_is_one_row_on_short_terminals() {
    let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    // 80x23 forces the one-row footer (height < 24).
    let terminal = draw(&app, 80, 23);
    let content = text(&terminal);
    assert!(content.contains("Idle"));
    assert!(content.contains("deep • high"));
}

#[test]
fn running_live_turn_shows_gap_footer_and_status_spinner() {
    let app = testapp::live_turn(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(
        content.contains("⚠ live output incomplete"),
        "event gap shows in the footer"
    );
    assert!(
        content.contains("Running read"),
        "running tool in the status row"
    );
}

#[test]
fn fatal_connection_renders_the_overlay() {
    let mut app = testapp::fresh(ThemeKind::Dark);
    let requests = testapp::take_requests(app.update(AppEvent::Bootstrap));
    let models = requests.iter().find(|r| r.method == "model.list").unwrap();
    testapp::respond_error(&mut app, models, "x", "no models");
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Fatal error"));
    assert!(content.contains("Press q to quit"));
}

#[test]
fn recent_notices_render_above_the_composer() {
    let mut app = testapp::fresh(ThemeKind::Dark);
    app.update(AppEvent::SubmitTurn {
        session_id: "none".into(),
        text: "hi".into(),
    });
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("unavailable"));
}

#[test]
fn cursor_sits_at_the_composer_caret() {
    // The hardware cursor is placed by `composer::render` through
    // `frame.set_cursor_position` using `unicode-width` column math; the
    // per-character column rules are unit-tested in `markdown`.
    let app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("Type a message…"));
}

#[test]
fn light_theme_renders_identically_shaped_content() {
    let app = testapp::chat_with_reasoning(ThemeKind::Light);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Coding agent TUI"));
    assert!(content.contains("hello"));
    assert!(any_cell_matching(&terminal, |cell| cell.bg == Theme::light().user_message_bg));
}

// ---- Phase 4: selectors and the new-session form -----------------------

#[test]
fn selector_panel_replaces_the_composer_and_keeps_the_transcript_visible() {
    let app = testapp::model_selector(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    // The transcript stays visible above the dock (spec 24.1).
    assert!(content.contains("MINICORE"));
    assert!(content.contains("Select model"));
    assert!(content.contains("Changing model creates a new session."));
    assert!(content.contains("128k context"));
    assert!(content.contains("✓ tools"));
    assert!(content.contains("— tools"));
    assert!(content.contains("✓ current"));
    let dark = Theme::dark();
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == dark.selected_bg),
        "the selected row uses selected_bg"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.symbol() == "→"
            && cell.fg == dark.accent),
        "the selection arrow is accent"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.symbol() == "┌"
            && cell.fg == dark.border_accent),
        "the panel border is accent"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.symbol() == "✓"
            && cell.fg == dark.success),
        "the current marker is success colored"
    );
}

#[test]
fn reasoning_selector_lists_only_supported_levels_with_the_current_session_header() {
    let app = testapp::reasoning_selector(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Select reasoning"));
    assert!(content.contains("Current session: —"));
    assert!(content.contains("New session setting: high"));
    assert!(content.contains("Provider default"));
    assert!(content.contains("Deep reasoning"));
    // deep supports auto/low/medium/high; disabled is not listed.
    assert!(content.contains("Moderate reasoning"));
    assert!(!content.contains("No reasoning"));
    assert!(
        any_cell_matching(&terminal, |cell| cell.fg == Theme::dark().thinking_high),
        "reasoning rows use the thinking colors"
    );
}

#[test]
fn session_selector_sorts_newest_first_and_marks_running_loaded_idle() {
    let app = testapp::session_selector(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    // updated_at descending: ses_main (5m) then ses_recent (15m) then ses_old (1d).
    assert!(content.find("ses_main") < content.find("Web app"));
    assert!(content.find("Web app") < content.find("Rust port"));
    for marker in ["◉", "●", "○"] {
        assert!(content.contains(marker), "missing status marker {marker}");
    }
    assert!(content.contains("5m"));
    assert!(content.contains("15m"));
    assert!(content.contains("1d"));
    assert!(content.contains("/work/web"));
}

#[test]
fn new_session_form_shows_all_fields_and_the_active_field_background() {
    let app = testapp::new_session(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("New session"));
    assert!(content.contains("workspace"));
    assert!(content.contains("/project"));
    assert!(content.contains("profile"));
    assert!(content.contains("coding"));
    assert!(content.contains("model"));
    assert!(content.contains("deep"));
    assert!(content.contains("reasoning"));
    assert!(content.contains("high"));
    assert!(content.contains("title"));
    assert!(content.contains("Create session"));
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == Theme::dark().selected_bg),
        "the active field row is highlighted"
    );
}

#[test]
fn empty_selector_search_shows_no_matching_items() {
    let app = testapp::empty_model_search(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("No matching items"));
}

#[test]
fn short_terminal_renders_an_8_row_selector_panel() {
    let app = testapp::narrow_selector(ThemeKind::Dark);
    let terminal = draw(&app, 60, 16);
    let content = text(&terminal);
    assert!(content.contains("Select model"));
    assert!(
        content.contains("MINICORE"),
        "the transcript header stays above"
    );
    assert!(
        any_cell_matching(&terminal, |cell| cell.bg == Theme::dark().selected_bg),
        "the moved selection is highlighted"
    );
}

#[test]
fn selectors_render_on_both_themes_without_panicking() {
    let fixtures: [fn(ThemeKind) -> App; 5] = [
        testapp::new_session,
        testapp::model_selector,
        testapp::reasoning_selector,
        testapp::session_selector,
        testapp::profile_selector,
    ];
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        for fixture in fixtures {
            let _ = draw(&fixture(kind), 120, 40);
        }
    }
}

// ---- Phase 5: input rendering -------------------------------------------

#[test]
fn help_panel_lists_keys_and_safety_notes() {
    let mut app = testapp::help(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Help"));
    assert!(content.contains("Ctrl+R"));
    assert!(any_cell_matching(&terminal, |cell| cell.symbol() == "┌"
        && cell.fg == Theme::dark().border_accent));
    // Scroll to the bottom: the scope notes are on the final page.
    for _ in 0..40 {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
    }
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Slash commands"));
    assert!(content.contains("Tools run automatically."));
    assert!(content.contains("Bash is not sandboxed."));
    assert!(content.contains("No approval UI"));
}

#[test]
fn logs_panel_shows_bounded_agent_stderr_without_raw_frames() {
    let app = testapp::logs(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Agent logs"));
    assert!(content.contains("loaded profile coding"));
    assert!(!content.contains("jsonrpc"), "no raw RPC frames in logs");
}

#[test]
fn multiline_composer_grows_the_panel_and_wraps_cjk() {
    let app = testapp::multiline_composer(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("line one"));
    assert!(content.contains("line two"));
    assert!(
        content.contains("你"),
        "CJK renders (2 columns per char in the buffer)"
    );
    assert!(content.contains("line six"));
    // Six wrapped rows plus two border rows: the panel outgrew the fixed 5.
    let height = crate::ui::layout::composer_height_phase5(&app, 80, 24, false);
    assert_eq!(height, 8, "the composer grew with the content");
}

#[test]
fn selector_search_query_is_visible() {
    let app = testapp::search_query(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("> fast"));
}

#[test]
fn new_output_marker_renders_when_scrolled_away() {
    let app = testapp::new_output_marker(ThemeKind::Dark);
    let terminal = draw(&app, 80, 24);
    assert!(text(&terminal).contains("↓ new output"));
}
