//! Component-level rendering tests: exact colors, modifiers, preview bounds,
//! footer behavior, and cursor column math (development spec 15, 29, 31).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use crate::app::App;
use crate::event::{AppEvent, RpcEvent};
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::{
    AssistantBlock, AssistantPart, ToolBlock, TranscriptBlock, UserBlock,
};
use crate::theme::{Theme, ThemeKind};
use crate::ui::testapp;
use crate::ui::{assistant, footer, layout, reasoning, render, tool, transcript, user};

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

fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let width = terminal.backend().buffer().area.width as usize;
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn any_cell_matching(
    terminal: &Terminal<TestBackend>,
    predicate: impl Fn(&ratatui::buffer::Cell) -> bool,
) -> bool {
    terminal.backend().buffer().content().iter().any(predicate)
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn is_blank(line: &ratatui::text::Line<'_>) -> bool {
    line_text(line).trim().is_empty()
}

fn assert_section_is_vertically_padded(lines: &[ratatui::text::Line<'_>], label: &str) {
    assert!(
        lines.len() >= 3,
        "{label} needs top, content, and bottom rows"
    );
    assert!(is_blank(&lines[0]), "{label} needs one blank row above");
    assert!(
        !is_blank(&lines[1]),
        "{label} must start content immediately after the top row"
    );
    assert!(
        !is_blank(&lines[lines.len() - 2]),
        "{label} must end content immediately before the bottom row"
    );
    assert!(
        is_blank(&lines[lines.len() - 1]),
        "{label} needs one blank row below"
    );
}

fn assert_no_adjacent_blank_rows(lines: &[ratatui::text::Line<'_>], label: &str) {
    for pair in lines.windows(2) {
        assert!(
            !(is_blank(&pair[0]) && is_blank(&pair[1])),
            "{label} has duplicate boundary blank rows"
        );
    }
}

#[test]
fn message_and_tool_sections_have_symmetric_vertical_padding() {
    let theme = Theme::dark();

    let user_lines = user::lines(
        &theme,
        &UserBlock {
            index: Some(1),
            loop_id: Some("turn".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "user text".to_owned(),
            pending: false,
        },
        40,
    );
    assert_section_is_vertically_padded(&user_lines, "user message");

    let assistant_lines = assistant::lines(
        &theme,
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![AssistantPart::Text("assistant text".to_owned())],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    assert_section_is_vertically_padded(&assistant_lines, "assistant message");

    let visible_thinking = reasoning::visible_lines(&theme, "thinking text", 40);
    let hidden_thinking = reasoning::thinking_line(&theme);
    let live_thinking = reasoning::live_lines(&theme, "live thinking", 40, true);
    for (label, lines) in [
        ("visible thinking", visible_thinking),
        ("hidden thinking", hidden_thinking),
        ("live thinking", live_thinking),
    ] {
        assert_section_is_vertically_padded(&lines, label);
        assert!(
            line_text(&lines[1]).starts_with(' '),
            "{label} content must use the same one-column left padding"
        );
    }

    let tool_lines = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "turn".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: Some("command result".to_owned()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: true,
        },
        40,
        false,
    );
    assert_section_is_vertically_padded(&tool_lines, "durable tool call");

    let live_tool_lines = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            progress: Some("running command".to_owned()),
        },
        40,
    );
    assert_section_is_vertically_padded(&live_tool_lines, "live tool call");
}

#[test]
fn assistant_parts_keep_order_and_share_boundary_padding() {
    let lines = assistant::lines(
        &Theme::dark(),
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![
                AssistantPart::Text("first".to_owned()),
                AssistantPart::Reasoning("thinking".to_owned()),
                AssistantPart::Text("second".to_owned()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    let text: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(text, vec!["", " first", "", " thinking", "", " second", ""]);
    assert_no_adjacent_blank_rows(&lines, "assistant text/reasoning/text");
}

#[test]
fn adjacent_assistant_sections_share_one_boundary_row() {
    let theme = Theme::dark();
    let make = |text: &str, index| AssistantBlock {
        index,
        loop_id: "turn".to_owned(),
        request_index: 0,
        model: "model".to_owned(),
        reasoning_level: crate::protocol::Reasoning::Auto,
        parts: vec![AssistantPart::Text(text.to_owned())],
        tool_calls: vec![],
        usage: Default::default(),
        finish_reason: "stop".to_owned(),
        terminal_error: None,
    };
    let mut lines = Vec::new();
    layout::append_section(
        &mut lines,
        assistant::lines(&theme, &make("first", 2), 40, true),
    );
    layout::append_section(
        &mut lines,
        assistant::lines(&theme, &make("second", 3), 40, true),
    );
    assert_eq!(
        lines.iter().map(line_text).collect::<Vec<_>>(),
        vec!["", " first", "", " second", ""]
    );
    assert_no_adjacent_blank_rows(&lines, "adjacent assistant sections");
}

#[test]
fn empty_reasoning_renders_nothing_and_does_not_hide_the_next_run() {
    let theme = Theme::dark();
    assert!(reasoning::reasoning_lines(&theme, "", 40, false, false).is_empty());
    assert!(reasoning::live_lines(&theme, "", 40, false).is_empty());

    let lines = assistant::lines(
        &theme,
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![
                AssistantPart::Text("answer".to_owned()),
                AssistantPart::Reasoning(String::new()),
                AssistantPart::Reasoning("hidden".to_owned()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        false,
    );
    let text: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(text, vec!["", " answer", "", " Thinking...", ""]);
    assert_no_adjacent_blank_rows(&lines, "empty reasoning followed by hidden reasoning");
}

#[test]
fn explicit_markdown_blank_lines_survive_section_padding() {
    let lines = assistant::lines(
        &Theme::dark(),
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![AssistantPart::Text("one\n\nthree".to_owned())],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    let text: Vec<String> = lines.iter().map(line_text).collect();
    assert_eq!(text, vec!["", " one", " ", " three", ""]);
}

#[test]
fn user_assistant_and_tool_boundaries_share_one_blank_row() {
    let theme = Theme::dark();
    let user_lines = user::lines(
        &theme,
        &UserBlock {
            index: Some(1),
            loop_id: Some("turn".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "user".to_owned(),
            pending: false,
        },
        40,
    );
    let assistant_lines = assistant::lines(
        &theme,
        &AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![AssistantPart::Text("assistant".to_owned())],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
    );
    let tool_lines = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            progress: None,
        },
        40,
    );
    let mut lines = Vec::new();
    layout::append_section(&mut lines, user_lines);
    layout::append_section(&mut lines, assistant_lines);
    layout::append_section(&mut lines, tool_lines);
    assert_no_adjacent_blank_rows(&lines, "user/assistant/tool");
    assert_eq!(
        lines.iter().filter(|line| !is_blank(line)).count(),
        3,
        "each section contributes one content row"
    );
}

#[test]
fn cached_and_fallback_transcripts_have_identical_section_spacing() {
    let theme = Theme::dark();
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .transcript
        .blocks = vec![
        TranscriptBlock::User(UserBlock {
            index: Some(1),
            loop_id: Some("turn".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "user".to_owned(),
            pending: false,
        }),
        TranscriptBlock::Assistant(AssistantBlock {
            index: 2,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::Auto,
            parts: vec![
                AssistantPart::Text("first".to_owned()),
                AssistantPart::Reasoning("thinking".to_owned()),
                AssistantPart::Text("second".to_owned()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        }),
        TranscriptBlock::Tool(ToolBlock {
            index: None,
            loop_id: "turn".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: None,
            outcome: None,
            live_status: None,
            progress: None,
            expanded: false,
        }),
    ];

    let fallback = transcript::all_lines(&theme, &app, 80);
    let prepared = transcript::prepare_cache(&app, 80).expect("durable cache preparation");
    app.update(AppEvent::TranscriptCachePrepared(prepared));
    let cached = transcript::all_lines(&theme, &app, 80);
    assert_eq!(cached, fallback);
    assert_no_adjacent_blank_rows(&cached, "cached transcript");
}

#[test]
fn durable_and_live_tool_sections_have_the_same_padding_shape() {
    let theme = Theme::dark();
    let durable = tool::durable(
        &theme,
        &ToolBlock {
            index: None,
            loop_id: "turn".to_owned(),
            request_index: 0,
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            result: None,
            outcome: None,
            live_status: None,
            progress: None,
            expanded: false,
        },
        40,
        false,
    );
    let live = tool::live(
        &theme,
        &LiveTool {
            tool_call_id: "call".to_owned(),
            name: "bash".to_owned(),
            status: ToolStatus::Running,
            progress: None,
        },
        40,
    );
    assert_eq!(durable.len(), live.len());
    assert_section_is_vertically_padded(&durable, "durable collapsed tool");
    assert_section_is_vertically_padded(&live, "live tool");
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
    let make = |outcome: Option<crate::protocol::ToolOutcomeWire>| ToolBlock {
        index: None,
        loop_id: "t".into(),
        request_index: 0,
        tool_call_id: "c".into(),
        name: "read".into(),
        result: Some("data".into()),
        outcome,
        live_status: None,
        progress: None,
        expanded: true,
    };
    // Exact card backgrounds per state, asserted at the line level so the
    // viewport cannot hide them.
    for (outcome, expected) in [
        (
            Some(crate::protocol::ToolOutcomeWire::Success),
            theme.tool_success_bg,
        ),
        (
            Some(crate::protocol::ToolOutcomeWire::Denied),
            theme.tool_error_bg,
        ),
        (
            Some(crate::protocol::ToolOutcomeWire::Failed),
            theme.tool_error_bg,
        ),
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
        index: None,
        loop_id: "t".into(),
        request_index: 0,
        tool_call_id: "c".into(),
        name: "bash".into(),
        result: Some(result.to_owned()),
        outcome: Some(crate::protocol::ToolOutcomeWire::Success),
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
fn last_result_renders_outcome_and_persistence_in_status_and_transcript() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let turn = |loop_id: &str| crate::protocol::TurnRef {
        session_id: "ses_1".to_owned(),
        loop_id: loop_id.to_owned(),
    };

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_done"),
            outcome: crate::protocol::LoopOutcomeWire::Completed,
            persistence: crate::protocol::TurnPersistenceWire::Persisted,
            usage: Default::default(),
            requests: 1,
            tool_rounds: 0,
            final_config_revision: 0,
        });
    }
    let content = text(&draw(&app, 120, 40));
    assert!(content.contains("completed · persisted"));
    assert!(content.contains("Turn completed"));

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_cancelled"),
            outcome: crate::protocol::LoopOutcomeWire::Cancelled {
                reason: crate::protocol::CancelReasonWire::Unknown("sandbox_evicted".to_owned()),
            },
            persistence: crate::protocol::TurnPersistenceWire::Persisted,
            usage: Default::default(),
            requests: 1,
            tool_rounds: 0,
            final_config_revision: 0,
        });
    }
    let content = text(&draw(&app, 80, 24));
    assert!(content.contains("cancelled (sandbox_evicted)"));
    assert!(content.contains("persisted"));

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_shutdown"),
            outcome: crate::protocol::LoopOutcomeWire::Cancelled {
                reason: crate::protocol::CancelReasonWire::Shutdown,
            },
            persistence: crate::protocol::TurnPersistenceWire::Persisted,
            usage: Default::default(),
            requests: 1,
            tool_rounds: 0,
            final_config_revision: 0,
        });
    }
    let content = text(&draw(&app, 80, 24));
    assert!(content.contains("cancelled (shutdown) · persisted"));

    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: turn("loop_failed"),
            outcome: crate::protocol::LoopOutcomeWire::Failed {
                kind: "model_error".to_owned(),
                model_error: Some(crate::protocol::ModelErrorWire {
                    kind: "rate_limit".to_owned(),
                    delivery: "upstream".to_owned(),
                    retryable: true,
                    retry_after_millis: None,
                }),
            },
            persistence: crate::protocol::TurnPersistenceWire::Failed,
            usage: Default::default(),
            requests: 1,
            tool_rounds: 0,
            final_config_revision: 0,
        });
        view.state.as_mut().unwrap().status = crate::protocol::SessionStatusWire::Blocked;
        view.state.as_mut().unwrap().block_reason =
            Some(crate::protocol::SessionBlockReasonWire::Persistence);
    }
    let content = text(&draw(&app, 120, 40));
    assert!(content.contains("failed: model_error: rate_limit"));
    assert!(content.contains("persistence failed"));
    assert!(content.contains("Blocked · persistence"));
}

#[test]
fn live_request_without_model_is_explicitly_unknown() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .live
        .as_mut()
        .unwrap()
        .requests[0]
        .model
        .clear();
    let content = text(&draw(&app, 120, 40));
    assert!(content.contains("Request #0 · config unknown"));
    assert!(content.contains("request 0 · config unknown"));
}

#[test]
fn footer_waiting_boundary_shows_next_config_with_current_request_preserved() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    // live_turn has request 0 with model "deep", reasoning High, rev 0
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.config_update = Some(crate::state::session::PendingConfigUpdate {
            loop_id: Some("loop_live".to_string()),
            model: Some("fast".to_string()),
            reasoning: Some(crate::protocol::Reasoning::Low),
            revision: Some(2),
            state: crate::state::session::ConfigUpdateState::WaitingBoundary,
        });
    }
    let terminal = draw(&app, 120, 40);
    let content = text(&terminal);
    // Preserves current request config:
    assert!(content.contains("request 0 · deep · high · rev 0"));
    // Displays next config without guessing or overwriting current:
    assert!(content.contains("next: fast • low · rev 2"));
}

#[test]
fn fatal_connection_renders_the_overlay() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "unfinished".into(),
    });
    app.update(AppEvent::Rpc(RpcEvent::AgentLogLine(
        "latest agent stderr".to_owned(),
    )));
    app.update(AppEvent::Rpc(RpcEvent::Exited(None)));
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Fatal error"));
    assert!(content.contains("Exit status: unavailable"));
    assert!(content.contains("result/save status unconfirmed"));
    assert!(content.contains("Tool side effects may already exist"));
    assert!(content.contains("latest agent stderr"));
    assert!(content.contains("Press q to quit"));
}

#[test]
fn fatal_overlay_retains_a_known_result_summary() {
    let mut app = testapp::shutdown_cancel_result(ThemeKind::Dark);
    app.update(AppEvent::Rpc(RpcEvent::Exited(None)));
    let terminal = draw(&app, 80, 24);
    let content = text(&terminal);
    assert!(content.contains("Known turn result:"));
    assert!(content.contains("cancelled (shutdown) · persisted"));
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
    assert!(content.contains("Model applies at the next model request."));
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
    assert!(content.contains("/cancel"));
    assert!(content.contains("/refresh"));
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

#[test]
fn new_output_marker_gets_its_own_row_without_overwriting_transcript() {
    let mut app = testapp::scrolled(ThemeKind::Dark);
    let all = transcript::all_lines(&Theme::dark(), &app, 80);
    let total = all.len();
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: 16,
    });
    let terminal = draw(&app, 80, 24);
    let rows = buffer_lines(&terminal);
    let marker_row = rows
        .iter()
        .position(|row| row.starts_with("↓ new output"))
        .expect("scrolled transcript shows the new-output marker");
    assert_eq!(rows[marker_row].trim_end(), "↓ new output");
    assert!(!rows[marker_row].contains("wisdom"));

    assert_eq!(transcript::total_lines(&app, 80), total);
    assert_eq!(
        transcript::visible_rows(&app, total, 17),
        16,
        "the marker consumes one transcript row"
    );
    assert!(
        all.iter()
            .any(|line| line_text(line).contains("quoted wisdom")),
        "the covered transcript body remains available to scrolling"
    );
    let marker_cells =
        &terminal.backend().buffer().content()[marker_row * 80..(marker_row + 1) * 80];
    assert!(
        marker_cells
            .iter()
            .all(|cell| cell.bg == Theme::dark().page_bg),
        "the marker row clears the transcript background"
    );

    app.update(AppEvent::Terminal(crossterm::event::Event::Mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::empty(),
        },
    )));
    assert!(
        buffer_lines(&draw(&app, 80, 24))
            .iter()
            .any(|row| row.contains("wisdom")),
        "scrolling down exposes the transcript row below the marker window"
    );
}

#[test]
fn end_resumes_follow_after_marker_reservation() {
    let mut app = testapp::scrolled(ThemeKind::Dark);
    let total = transcript::total_lines(&app, 80);
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: 16,
    });
    assert!(!app.active_view().unwrap().scroll.follow_tail);

    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::End,
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )));
    assert!(app.active_view().unwrap().scroll.follow_tail);
    assert_eq!(transcript::visible_rows(&app, total, 17), 17);
    assert!(!text(&draw(&app, 80, 24)).contains("↓ new output"));
}

#[test]
fn empty_user_sections_do_not_add_spacer_rows() {
    let theme = Theme::dark();
    let make = |text: &str| UserBlock {
        index: None,
        loop_id: None,
        kind: crate::protocol::UserMessageKindWire::Prompt,
        text: text.to_owned(),
        pending: true,
    };
    for body in ["", " \n\t"] {
        assert!(
            user::lines(&theme, &make(body), 40).is_empty(),
            "empty user body {body:?} is an empty section"
        );
    }

    let normal = user::lines(&theme, &make("pending message"), 40);
    assert_section_is_vertically_padded(&normal, "non-empty pending user");
}
