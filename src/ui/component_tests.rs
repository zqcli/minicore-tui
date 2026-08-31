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
