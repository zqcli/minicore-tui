//! The composer as a Rail editor surface. The existing `Composer` remains the
//! editable text authority; this module only maps its wrapped rows into the
//! fixed blue-rail surface and positions the hardware cursor.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::theme::Theme;
use crate::ui::editor_layout::EditorLayout;
use crate::ui::rail;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let completion_rows =
        crate::ui::layout::composer_completion_rows_for_height(app, frame.area().height)
            .min(area.height.saturating_sub(1));
    let editor_height = area.height.saturating_sub(completion_rows);
    let editor_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: editor_height,
    };
    if editor_height > 0 {
        render_editor(frame, editor_area, app, theme);
    }
    if completion_rows > 0 {
        render_completion(
            frame,
            Rect {
                x: area.x,
                y: area.y + editor_height,
                width: area.width,
                height: completion_rows,
            },
            app,
            theme,
        );
    }
}

fn render_editor(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let view = app.active_view();
    let waiting = view.is_some_and(|view| {
        view.live.as_ref().is_some_and(|live| live.waiting)
            || view.state.as_ref().is_some_and(|state| {
                state.status == crate::protocol::SessionStatusWire::WaitingForInput
            })
    });
    let finishing = view.is_some_and(|view| {
        view.state
            .as_ref()
            .is_some_and(|state| state.status == crate::protocol::SessionStatusWire::Finishing)
    });
    let running = view.is_some_and(|view| view.is_running());
    let width = rail::content_width(area.width as usize, rail::RAIL_WIDTH);
    let height = area.height as usize;
    let display_content = app.composer.display_content();
    let display_lines = display_content
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let paste_markers = app.composer.display_paste_markers();
    let editor_layout = EditorLayout::new_with_atomic_ranges(
        &display_lines,
        width,
        height,
        app.composer.display_cursor(),
        &paste_markers,
    );
    let contents = compose_lines(app, theme, &editor_layout, running, waiting, finishing);
    let colors = rail::editor_colors(theme);
    let padded = editor_layout
        .visible_rows()
        .into_iter()
        .map(|source| {
            let line = source
                .and_then(|index| contents.get(index).cloned())
                .unwrap_or_default();
            rail::surface_row(area.width as usize, colors, rail::RAIL_WIDTH, line)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(padded), area);

    // Block cursor + hardware cursor at the (row, column) cell, so IME
    // composition and multi-line editing land on the right cell. The cursor
    // column can sit exactly at the wrap boundary (== width), which has no
    // cell: clamp drawing into the inner area while the helper keeps the
    // true boundary column for logic.
    let (cell_row, cell_col) = editor_layout.screen_cursor();
    let content_w = width;
    let draw_col = cell_col.min(content_w.saturating_sub(1));
    let x = area.x + rail::RAIL_WIDTH as u16 + draw_col as u16;
    let y = area.y + cell_row as u16;
    if app.focused_region() == crate::state::panels::Focus::Editor
        && x < area.x + area.width
        && y < area.y + area.height
        && content_w > 0
        && area.height > 0
    {
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            cell.set_bg(theme.user_message_bg);
            cell.set_style(Style::default().add_modifier(ratatui::style::Modifier::REVERSED));
        }
        frame.set_cursor_position((x, y));
    }
}

fn render_completion(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(completion) = app.slash_completion.as_ref() else {
        return;
    };
    let geometry =
        crate::ui::layout::slash_completion_geometry(completion, area.height, frame.area().height);
    let content_width = area.width.saturating_sub(rail::RAIL_WIDTH as u16) as usize;
    let colors = rail::editor_colors(theme);
    let row = |text: String, selected: bool| {
        rail::surface_row(
            area.width as usize,
            colors,
            rail::RAIL_WIDTH,
            Line::from(Span::styled(
                rail::clip_cells(&text, content_width),
                Style::new().fg(if selected {
                    theme.rail_editor
                } else {
                    theme.text
                }),
            )),
        )
    };
    let mut lines = Vec::new();
    if geometry.show_header {
        lines.push(row(completion_title(app, completion), false));
    }
    let choices = completion.popup.is_some();
    for index in geometry.start..geometry.end {
        let item = &completion.items[index];
        let selected = index == completion.selected;
        let text = if choices {
            choice_row(app, item, selected, content_width)
        } else {
            object_row(app, item, selected, content_width)
        };
        lines.push(row(text, selected));
    }
    if geometry.show_parameter {
        if let Some(hint) = completion.parameter_hint() {
            lines.push(row(format!("  {hint}"), false));
        }
    }
    if completion.items.is_empty() {
        lines.push(row(
            if choices {
                "No matching choices"
            } else {
                "No matching commands"
            }
            .to_owned(),
            false,
        ));
    }
    if geometry.show_hint {
        let enter = if completion.popup.is_some() {
            "fill"
        } else if completion.opens_choices_on_enter(&app.composer.content()) {
            "choose"
        } else {
            "run typed"
        };
        let position = format!(
            "{}/{}",
            if completion.items.is_empty() {
                0
            } else {
                completion.selected + 1
            },
            completion.items.len()
        );
        let escape = if completion.popup.is_some() && !completion.filter.is_empty() {
            "clear"
        } else {
            "close"
        };
        let hint = if completion.popup.is_some() {
            if content_width >= 76 {
                format!(
                    "↑↓ / PgUp PgDn choose · type to filter · Enter/Tab fill · Esc {escape} · {position}"
                )
            } else if content_width >= 52 {
                format!("↑↓ · type filter · Enter/Tab fill · Esc {escape} · {position}")
            } else {
                format!("↑↓ · Enter/Tab fill · Esc {escape} · {position}")
            }
        } else if content_width >= 76 {
            format!("↑↓ choose · Ctrl+Space choices · Tab fill · Enter {enter} · Esc · {position}")
        } else if content_width >= 52 {
            format!("↑↓ · ^Space · Tab · Enter {enter} · Esc · {position}")
        } else {
            format!("↑↓ · ^Space · Tab · Enter · Esc · {position}")
        };
        lines.push(row(hint, false));
    }
    lines.truncate(area.height as usize);
    frame.render_widget(Paragraph::new(lines), area);
}

/// Current values come only from acknowledged settings or new-session defaults,
/// never from the highlighted menu candidate.
fn current_value(app: &App, name: &str) -> Option<String> {
    match name {
        "model" => Some(
            app.new_session()
                .map(|draft| draft.model.as_str())
                .or_else(|| app.active_view().map(|view| view.info.model.as_str()))
                .or(app.catalogs.next_model.as_deref())
                .unwrap_or("default")
                .to_owned(),
        ),
        "reasoning" => Some(
            crate::state::selection::reasoning_label(
                app.new_session()
                    .map(|draft| draft.reasoning)
                    .or_else(|| app.active_view().map(|view| view.info.reasoning))
                    .or(app.catalogs.next_reasoning)
                    .unwrap_or(crate::protocol::Reasoning::Auto),
            )
            .to_owned(),
        ),
        _ => None,
    }
}

fn entry_value(app: &App, item: &crate::command::MenuEntry) -> String {
    match item.kind {
        crate::command::MenuKind::Command(name)
        | crate::command::MenuKind::ArgumentChoice(name) => {
            current_value(app, name).unwrap_or_else(|| item.action_name().to_owned())
        }
        crate::command::MenuKind::Group(_) => item.action_name().to_owned(),
    }
}

fn completion_title(app: &App, completion: &crate::app::SlashCompletionState) -> String {
    let title = if let Some(popup) = completion.popup.as_ref() {
        if let Some(name) = completion.argument_command {
            current_value(app, name).map_or_else(
                || format!("{} · {name} choices", popup.target.object_name()),
                |value| format!("{} [{value}]", popup.target.object_name()),
            )
        } else {
            format!("{} actions", popup.target.object_name())
        }
    } else {
        "Commands".to_owned()
    };
    if completion.popup.is_some() || !completion.filter.is_empty() {
        format!("{title} · filter: {}", completion.filter)
    } else {
        title
    }
}

/// Keep the object identity and its actionable value visible before spending
/// room on supplementary usage text. The right-aligned bracket is the control.
fn object_row(app: &App, item: &crate::command::MenuEntry, selected: bool, width: usize) -> String {
    let prefix = if selected { "→ " } else { "  " };
    let name = item.object_name();
    let left = format!("{prefix}{name}");
    let left_width = UnicodeWidthStr::width(left.as_str());
    let value_width = width.saturating_sub(left_width + 5);
    let value = rail::clip_cells(&entry_value(app, item), value_width);
    let control = format!("[{value} ▾]");
    let control_width = UnicodeWidthStr::width(control.as_str());
    let middle_width = width.saturating_sub(left_width + control_width);
    let hint = item
        .argument_hint()
        .filter(|hint| hint.len() <= 28)
        .unwrap_or("");
    let detail = if !hint.is_empty() {
        hint
    } else if width >= 76 {
        item.summary
    } else {
        ""
    };
    let detail = rail::clip_cells(detail, middle_width.saturating_sub(4));
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("  {detail}")
    };
    let padding = middle_width.saturating_sub(UnicodeWidthStr::width(detail.as_str()));
    format!("{left}{detail}{}{control}", " ".repeat(padding))
}

fn choice_row(app: &App, item: &crate::command::MenuEntry, selected: bool, width: usize) -> String {
    let prefix = if selected { "→ " } else { "  " };
    let label = item.choice_label();
    let current = match item.kind {
        crate::command::MenuKind::ArgumentChoice(name) => {
            current_value(app, name).is_some_and(|value| value == label)
        }
        _ => false,
    };
    let suffix = if current { "  current" } else { "" };
    let hint = if matches!(item.kind, crate::command::MenuKind::ArgumentChoice(_)) {
        ""
    } else {
        item.argument_hint().unwrap_or("")
    };
    let mut text = format!("{prefix}{label}{suffix}");
    if !hint.is_empty() {
        text.push_str("  ");
        text.push_str(hint);
    } else if width >= 76
        && !matches!(item.kind, crate::command::MenuKind::ArgumentChoice(_))
        && !item.summary.is_empty()
    {
        text.push_str("  ");
        text.push_str(item.summary);
    }
    text
}

/// The wrapped composer rows, styled as plain text. Empty input deliberately
/// has no instructional paragraph; mode/error state is carried by status.
fn compose_lines(
    app: &App,
    theme: &Theme,
    layout: &EditorLayout,
    running: bool,
    waiting: bool,
    finishing: bool,
) -> Vec<Line<'static>> {
    let style = Style::new().fg(theme.text);
    let selection = app.composer_selection_range();
    if app.composer.is_empty() {
        let blocked = app.active_view().is_some_and(|view| {
            view.state
                .as_ref()
                .is_some_and(|state| state.status == crate::protocol::SessionStatusWire::Blocked)
        });
        let placeholder = if blocked {
            "Session blocked"
        } else if running {
            ""
        } else if waiting {
            "Unsupported interaction — Esc to cancel"
        } else if finishing {
            "Saving turn…"
        } else {
            ""
        };
        return vec![Line::styled(placeholder, Style::new().fg(theme.muted))];
    }
    layout
        .rows
        .iter()
        .map(|row| {
            let start = display_row_start(layout, row);
            marker_line(
                &row.text,
                start,
                &app.composer.display_paste_markers(),
                selection.as_ref(),
                style,
                theme,
            )
        })
        .collect()
}

fn display_row_start(layout: &EditorLayout, row: &crate::ui::editor_layout::VisualLine) -> usize {
    let mut offset = 0;
    for logical in 0..row.logical_line {
        offset += layout
            .rows
            .iter()
            .filter(|candidate| candidate.logical_line == logical)
            .map(|candidate| candidate.text.chars().count())
            .sum::<usize>();
        offset += 1;
    }
    offset + row.start_char
}

fn marker_line(
    text: &str,
    global_start: usize,
    markers: &[std::ops::Range<usize>],
    selection: Option<&std::ops::Range<usize>>,
    normal: Style,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let marker_style = normal
        .fg(theme.rail_editor)
        .bg(theme.selection_bg)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let selection_style = normal.fg(theme.selection_fg).bg(theme.selection_bg);
    for (global, character) in (global_start..).zip(text.chars()) {
        let marker = markers.iter().any(|range| range.contains(&global));
        let selected = selection.is_some_and(|range| range.contains(&global));
        if marker || selected {
            if !current.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut current), normal));
            }
            let style = if selected {
                selection_style
            } else {
                marker_style
            };
            spans.push(Span::styled(character.to_string(), style));
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, normal));
    }
    Line::from(spans)
}

/// The visual (row, col) of the block cursor in wrapped-cell space.
/// `Composer::cursor()` is (row, **char index**, as tui-textarea reports);
/// this converts to display columns through the same `EditorLayout` used by
/// the renderer and height estimator.
#[cfg(test)]
fn cursor_cell(app: &App, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let rows = EditorLayout::row_count(app.composer.lines(), width);
    EditorLayout::new(
        app.composer.lines(),
        width,
        rows.max(1),
        app.composer.cursor(),
    )
    .screen_cursor()
}

#[cfg(test)]
fn cursor_wrap_pos(line: &str, cursor_col: usize, width: usize) -> (usize, usize) {
    let lines = vec![line.to_owned()];
    let rows = EditorLayout::row_count(&lines, width.max(1));
    EditorLayout::new(&lines, width.max(1), rows.max(1), (0, cursor_col)).screen_cursor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::event::AppEvent;
    use crate::theme::ThemeKind;
    use ratatui::Terminal;
    use ratatui::backend::{Backend, TestBackend};

    fn app_with(text: &str) -> App {
        let mut app = App::new(std::path::PathBuf::from("/ws"));
        app.update(AppEvent::SetTheme(ThemeKind::Dark));
        // Paste is a single bounded edit through App::update.
        if !text.is_empty() {
            app.update(AppEvent::Terminal(crossterm::event::Event::Paste(
                text.to_owned(),
            )));
        }
        app
    }

    #[test]
    fn cursor_wrap_pos_matches_the_greedy_rule() {
        // ASCII at the end of a soft-wrapped line moves to the next visual
        // row, matching Pi's word-wrap cursor map.
        assert_eq!(cursor_wrap_pos("abcdef", 6, 3), (1, 3));
        assert_eq!(cursor_wrap_pos("abcdef", 5, 3), (1, 2));
        assert_eq!(
            cursor_wrap_pos("abcdef", 3, 3),
            (1, 0),
            "soft-wrap boundary belongs to the next row"
        );
        // CJK counts 2 columns each.
        assert_eq!(cursor_wrap_pos("你好世界", 4, 4), (1, 4));
        assert_eq!(cursor_wrap_pos("你好", 2, 4), (0, 4));
        // Emoji + combining marks use display widths.
        assert_eq!(cursor_wrap_pos("😀a\u{301}", 3, 4), (0, 3));
        // width 1 and 0 are defensive.
        assert_eq!(cursor_wrap_pos("ab", 2, 1), (1, 1));
        assert_eq!(cursor_wrap_pos("ab", 2, 0), (1, 1));
        // Multi logical lines: cursor_row counts whole previous lines.
        // A trailing cursor past the end lands at the end.
        assert_eq!(cursor_wrap_pos("hello world", 11, 4), (3, 1));
    }

    #[test]
    fn cursor_cell_is_the_visual_wrapped_position() {
        let app = app_with("abcdef\nghijkl");
        // cursor() col is a char index from tui-textarea.
        let (row, col) = app.composer.cursor();
        assert_eq!((row, col), (1, 6));
        assert_eq!(
            cursor_cell(&app, 3),
            (1 + 2, 3),
            "row 0 wraps to 2 rows then row 1 ends at col 3"
        );
    }

    #[test]
    fn long_wrapped_line_cursor_is_visible_with_correct_hardware_position() {
        let mut app = app_with("x".repeat(70).as_str());
        // Move the cursor to the very end (char index 70) — row 0, col 70.
        // Inner width is 78 -> 0..  ok, not wrapping. Now use a 79-char line
        // so it wraps into two visual rows inside the 78-column inner area.
        let long = "a".repeat(79);
        app.composer.set_text(&long);
        let (row, col) = app.composer.cursor();
        assert_eq!((row, col), (0, 79));
        let (cursor_row, cursor_col) = cursor_cell(&app, 78);
        assert_eq!(
            (cursor_row, cursor_col),
            (1, 1),
            "78 cols on row 0, cursor at 79th char starts row 1 col 1"
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .unwrap();
        let pos = terminal.backend_mut().get_cursor_position().unwrap();
        // Rail composer: dock = footer(1) + composer(4) => composer at
        // y=19..23. Two wrapped rows are vertically centered, so row 1 is
        // y=21; the rail and first content column place the cursor at x=3.
        assert_eq!((pos.x, pos.y), (3, 21));
    }

    #[test]
    fn cjk_cursor_hardware_position_uses_display_columns() {
        let mut app = app_with("你好abc");
        assert_eq!(app.composer.cursor(), (0, 5));
        assert_eq!(
            cursor_cell(&app, 78),
            (0, 7),
            "prefix 你好abc is 7 display columns"
        );
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Left,
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
        assert_eq!(app.composer.cursor(), (0, 4));
        assert_eq!(
            cursor_cell(&app, 78),
            (0, 6),
            "col 4 = prefix 你好ab, 6 display columns"
        );
    }
    fn type_keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.update(AppEvent::Terminal(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(c),
                    crossterm::event::KeyModifiers::NONE,
                ),
            )));
        }
    }

    fn press_enter(app: &mut App) -> Vec<crate::command::AppCommand> {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
        )))
    }

    fn press_key(
        app: &mut App,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Vec<crate::command::AppCommand> {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(code, modifiers),
        )))
    }

    fn open_choices(app: &mut App) {
        assert!(
            press_key(
                app,
                crossterm::event::KeyCode::Char(' '),
                crossterm::event::KeyModifiers::CONTROL
            )
            .is_empty()
        );
        assert!(app.slash_completion.as_ref().unwrap().popup.is_some());
    }

    fn capture(app: &App, width: u16, height: u16) -> (ratatui::buffer::Buffer, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = buffer
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        (buffer, rows)
    }

    #[test]
    fn theme_choice_fills_locally_before_a_separate_enter_applies_it() {
        let mut app = app_with("");
        type_keys(&mut app, "/theme");
        open_choices(&mut app);
        assert_eq!(app.composer.content(), "/theme");
        press_key(
            &mut app,
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        );
        assert!(press_enter(&mut app).is_empty());
        assert_eq!(app.composer.content(), "/theme light ");
        assert_eq!(app.theme, ThemeKind::Dark, "filling is not execution");
        assert!(press_enter(&mut app).is_empty());
        assert_eq!(app.theme, ThemeKind::Light);
        assert!(app.composer.content().is_empty());
    }

    #[test]
    fn required_arguments_remain_visible_in_the_compact_object_row() {
        let mut app = app_with("");
        type_keys(&mut app, "/tool");
        assert!(
            press_key(
                &mut app,
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE
            )
            .is_empty()
        );
        assert_eq!(app.composer.content(), "/tool ");
        let completion = app.slash_completion.as_ref().unwrap();
        assert!(completion.popup.is_none());
        assert_eq!(completion.items.len(), 1);
        let (_, rows) = capture(&app, 60, 16);
        assert!(rows.iter().any(|row| row.contains("<tool_call_id>")));
        assert!(rows.iter().any(|row| row.contains("[tool ▾]")));
    }

    #[test]
    fn root_objects_scroll_in_five_rows_or_three_on_a_tiny_terminal() {
        let mut app = app_with("");
        type_keys(&mut app, "/");
        for (width, height, count) in [(160, 48, 5), (80, 24, 5), (60, 16, 3)] {
            let area = Rect::new(0, 0, width, height);
            let screen = crate::ui::layout::screen_layout(&app, area);
            let (_, rows) = capture(&app, width, height);
            assert_eq!(
                crate::ui::layout::composer_completion_rows_for_height(&app, height),
                count + 2
            );
            assert_eq!(
                rows.iter().filter(|row| row.contains("▾]")).count(),
                count as usize
            );
            assert!(rows.iter().any(|row| row.contains("→ Model")));
            assert!(rows.iter().any(|row| row.contains("[default ▾]")));
            let hint = &rows[screen.panel.bottom() as usize - 1];
            assert!(hint.contains("Tab") && hint.contains("Esc") && hint.contains("1/6"));
            assert!(
                screen.transcript.height >= 6,
                "menu leaves conversation visible at {width}×{height}"
            );
            assert_eq!(screen.footer.y, screen.panel.bottom());
        }
        app.slash_completion.as_mut().unwrap().selected = 5;
        for (width, height) in [(80, 24), (60, 16)] {
            let (_, rows) = capture(&app, width, height);
            assert!(
                rows.iter()
                    .any(|row| row.contains("→ App") && row.contains("[settings ▾]"))
            );
            assert!(rows.iter().any(|row| row.contains("6/6")));
            assert_eq!(rows.iter().filter(|row| row.contains("→ ")).count(), 1);
        }
    }

    fn contrast_ratio(foreground: ratatui::style::Color, background: ratatui::style::Color) -> f64 {
        let luminance = |color| {
            let ratatui::style::Color::Rgb(red, green, blue) = color else {
                panic!("completion surface colors must be explicit RGB values");
            };
            let linear = |channel: u8| {
                let value = f64::from(channel) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
        };
        let foreground = luminance(foreground);
        let background = luminance(background);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    #[test]
    fn compact_commands_and_readable_controls_fit_every_supported_size_and_theme() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = Theme::for_kind(kind);
            for (width, height) in [(160, 48), (80, 24), (60, 16)] {
                for spec in crate::command::COMMANDS {
                    let mut app = app_with("");
                    app.update(AppEvent::SetTheme(kind));
                    type_keys(&mut app, &format!("/{}", spec.name));
                    let completion = app.slash_completion.as_ref().unwrap();
                    let entry = &completion.items[completion.selected];
                    let screen =
                        crate::ui::layout::screen_layout(&app, Rect::new(0, 0, width, height));
                    let (buffer, rows) = capture(&app, width, height);
                    let selected_rows = rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| row.contains("→ "))
                        .collect::<Vec<_>>();
                    assert_eq!(selected_rows.len(), 1, "only one selected object");
                    let (selected_y, selected_row) = selected_rows[0];
                    assert!(
                        selected_row.contains(&format!("→ {}", entry.object_name())),
                        "{selected_row}"
                    );
                    assert!(
                        selected_row.contains("[") && selected_row.contains("▾]"),
                        "{selected_row}"
                    );
                    let selected_cell = buffer
                        .cell((screen.panel.x + rail::RAIL_WIDTH as u16, selected_y as u16))
                        .unwrap();
                    assert_eq!(selected_cell.fg, theme.rail_editor);
                    assert_eq!(selected_cell.bg, theme.user_message_bg);
                    let hint_y = screen.panel.bottom() - 1;
                    let hint = &rows[hint_y as usize];
                    assert!(
                        hint.contains("Tab") && hint.contains("Enter") && hint.contains("Esc"),
                        "{hint}"
                    );
                    assert!(hint.contains(&format!("1/{}", completion.items.len())));
                    assert_eq!(screen.footer.y, hint_y + 1);
                    for x in screen.panel.x + rail::RAIL_WIDTH as u16..screen.panel.right() {
                        let cell = buffer.cell((x, hint_y)).unwrap();
                        if cell.symbol().trim().is_empty() {
                            continue;
                        }
                        assert_eq!(cell.fg, theme.text);
                        assert_eq!(cell.bg, theme.user_message_bg);
                        assert!(contrast_ratio(cell.fg, cell.bg) >= 4.5);
                        assert!(!cell.modifier.contains(ratatui::style::Modifier::DIM));
                    }
                    assert!(
                        rows.iter()
                            .all(|row| UnicodeWidthStr::width(row.as_str()) <= width as usize)
                    );
                }
            }
        }
    }

    #[test]
    fn one_session_row_is_replaced_by_a_scoped_bounded_dropdown() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            for (width, height, limit) in [(160, 48, 5), (80, 24, 5), (60, 16, 3)] {
                let mut app = app_with("");
                app.update(AppEvent::SetTheme(kind));
                type_keys(&mut app, "/session");
                assert_eq!(
                    crate::ui::layout::composer_completion_rows_for_height(&app, height),
                    2
                );
                let (_, rows) = capture(&app, width, height);
                assert_eq!(rows.iter().filter(|row| row.contains("▾]")).count(), 1);
                assert!(
                    rows.iter()
                        .any(|row| row.contains("Session") && row.contains("[list ▾]"))
                );
                open_choices(&mut app);
                assert_eq!(app.composer.content(), "/session");
                let completion = app.slash_completion.as_ref().unwrap();
                assert!(completion.items.len() > limit);
                assert_eq!(
                    crate::ui::layout::composer_completion_rows_for_height(&app, height),
                    limit as u16 + 2
                );
                let (_, rows) = capture(&app, width, height);
                assert!(
                    rows.iter()
                        .any(|row| row.contains("Session actions · filter:"))
                );
                assert!(
                    !rows.iter().any(|row| row.contains("▾]")),
                    "dropdown replaces root controls"
                );
                assert!(rows.iter().any(|row| row.contains("Enter/Tab fill")));
                type_keys(&mut app, "rename");
                assert_eq!(app.composer.content(), "/session");
                let (_, rows) = capture(&app, width, height);
                assert!(rows.iter().any(|row| row.contains("filter: rename")));
                assert!(rows.iter().any(|row| row.contains("→ rename")));
                assert!(rows.iter().any(|row| row.contains("Esc clear")));
            }
        }
    }

    #[test]
    fn candidate_highlight_never_masquerades_as_current_model_or_reasoning() {
        let mut app = crate::ui::testapp::chat(ThemeKind::Dark);
        app.catalogs.next_model = Some("next-session-model".into());
        app.catalogs.next_reasoning = Some(crate::protocol::Reasoning::Low);
        let active_model = app.active_view().unwrap().info.model.clone();
        let active_reasoning =
            crate::state::selection::reasoning_label(app.active_view().unwrap().info.reasoning);
        let model = crate::command::MenuEntry::command("model", "/model Other/Model".into());
        let reasoning = crate::command::MenuEntry::command("reasoning", "/reasoning ultra".into());
        for selected in [false, true] {
            let model_row = object_row(&app, &model, selected, 78);
            assert!(model_row.contains(&format!("[{active_model} ▾]")));
            assert!(
                !model_row.contains("Other/Model") && !model_row.contains("next-session-model")
            );
            let reasoning_row = object_row(&app, &reasoning, selected, 78);
            assert!(reasoning_row.contains(&format!("[{active_reasoning} ▾]")));
        }
    }

    #[test]
    fn compact_dropdown_preserves_notice_help_and_multiline_drafts() {
        use crossterm::event::{Event, KeyCode, KeyModifiers};
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            for (width, height) in [(60, 16), (80, 24), (160, 48)] {
                let mut app = app_with("");
                app.update(AppEvent::SetTheme(kind));
                app.notice(crate::app::NoticeLevel::Info, "Preserved notice");
                type_keys(&mut app, "/session");
                open_choices(&mut app);
                let screen = crate::ui::layout::screen_layout(&app, Rect::new(0, 0, width, height));
                let (_, rows) = capture(&app, width, height);
                assert!(rows.iter().any(|row| row.contains("Preserved notice")));
                assert!(screen.notice.unwrap().bottom() <= screen.panel.y);
                assert_eq!(screen.panel.bottom(), screen.footer.y);
                assert!(screen.transcript.height > 0);
                press_key(&mut app, KeyCode::F(1), KeyModifiers::NONE);
                assert!(matches!(app.dock, crate::state::selection::Dock::Help));
                assert_eq!(app.composer.content(), "/session");
                assert!(app.slash_completion.is_none());
                let (_, rows) = capture(&app, width, height);
                assert!(!rows.iter().any(|row| row.contains("Session actions")));
                press_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                app.composer.clear();
                let draft = "first draft line\nsecond draft line\nlast draft line";
                app.update(AppEvent::Terminal(Event::Paste(draft.into())));
                assert_eq!(app.composer.content(), draft);
                assert!(app.slash_completion.is_none());
                let (_, rows) = capture(&app, width, height);
                assert!(rows.iter().any(|row| row.contains("last draft line")));
                assert!(!rows.iter().any(|row| row.contains("▾]")));
            }
        }
    }

    #[test]
    fn long_unicode_values_keep_the_object_and_control_visible() {
        let mut app = app_with("");
        app.catalogs.next_model = Some("模型/😀".repeat(30));
        let item = crate::command::MenuEntry::command("model", "/model".into());
        for width in [20, 38, 58, 78, 158] {
            let row = object_row(&app, &item, true, width);
            assert!(row.starts_with("→ Model"));
            assert!(row.ends_with("▾]"));
            assert!(UnicodeWidthStr::width(row.as_str()) <= width);
        }
    }
}
