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
        crate::ui::layout::composer_completion_rows(app).min(area.height.saturating_sub(1));
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
    let colors = rail::editor_colors(theme);
    let show_header = area.height >= 3;
    let max_visible = (area.height as usize)
        .saturating_sub(1 + usize::from(show_header))
        .clamp(1, completion.visible_limit());
    let start = completion
        .selected
        .saturating_sub(max_visible / 2)
        .min(completion.items.len().saturating_sub(max_visible));
    let end = (start + max_visible).min(completion.items.len());
    let content_width = area.width.saturating_sub(rail::RAIL_WIDTH as u16) as usize;
    let label_width = completion
        .items
        .iter()
        .map(|item| {
            UnicodeWidthStr::width(item.as_str())
                + if matches!(item.kind, crate::command::MenuKind::Group(_)) {
                    2
                } else {
                    0
                }
        })
        .max()
        .unwrap_or(0)
        .clamp(10, 26);
    let mut lines = completion.items[start..end]
        .iter()
        .enumerate()
        .map(|(visible_index, item)| {
            let selected = start + visible_index == completion.selected;
            let prefix = if selected { "→ " } else { "  " };
            let summary = item.summary;
            let label = if matches!(item.kind, crate::command::MenuKind::Group(_)) {
                format!("{} ›", item.text)
            } else {
                item.text.clone()
            };
            let text = if content_width >= 44 && !summary.is_empty() {
                let label = rail::clip_cells(&label, label_width);
                format!(
                    "{prefix}{label}{}  {summary}",
                    " ".repeat(label_width.saturating_sub(UnicodeWidthStr::width(label.as_str())))
                )
            } else {
                format!("{prefix}{label}")
            };
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
        })
        .collect::<Vec<_>>();
    if completion.items.is_empty() {
        lines.push(rail::surface_row(
            area.width as usize,
            colors,
            rail::RAIL_WIDTH,
            Line::from(Span::styled(
                if completion.argument_command.is_some() {
                    "No matching values"
                } else {
                    "No matching commands"
                },
                Style::new().fg(theme.text),
            )),
        ));
    }
    if show_header {
        let breadcrumb = completion
            .group
            .map(|group| group.name())
            .or(completion.argument_command)
            .or_else(|| {
                (!completion.filter.is_empty())
                    .then(|| {
                        completion
                            .items
                            .get(completion.selected)
                            .map(|item| item.breadcrumb)
                    })
                    .flatten()
            });
        let title = match (completion.group, completion.argument_command) {
            (Some(group), Some(argument)) => format!("Commands › {} › {argument}", group.name()),
            _ => breadcrumb
                .filter(|name| *name != "Commands")
                .map_or("Commands".to_owned(), |name| format!("Commands › {name}")),
        };
        lines.insert(
            0,
            rail::surface_row(
                area.width as usize,
                colors,
                rail::RAIL_WIDTH,
                Line::from(Span::styled(
                    rail::clip_cells(&title, content_width),
                    Style::new().fg(theme.text),
                )),
            ),
        );
    }
    let selected = completion.items.get(completion.selected);
    let enter = if completion.submits_literal() {
        "apply typed"
    } else {
        match selected.map(|item| item.kind) {
            None => "check typed",
            Some(crate::command::MenuKind::Group(_)) => "open",
            _ if selected.is_some_and(|item| item.needs_input()) => "fill",
            _ => "run",
        }
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
    let escape = if completion.group.is_some() {
        if completion.filter.is_empty() {
            "back"
        } else {
            "clear"
        }
    } else {
        "close"
    };
    let hint = if content_width >= 76 {
        format!("↑↓ / PgUp PgDn choose · Tab fill · Enter {enter} · Esc {escape} · {position}")
    } else if content_width >= 52 {
        format!("↑↓ PgUp/Dn · Tab fill · Enter {enter} · Esc · {position}")
    } else {
        format!("↑↓ · Tab · Enter · Esc · {position}")
    };
    lines.push(rail::surface_row(
        area.width as usize,
        colors,
        rail::RAIL_WIDTH,
        Line::from(Span::styled(
            rail::clip_cells(&hint, content_width),
            Style::new().fg(theme.text),
        )),
    ));
    lines.truncate(area.height as usize);
    frame.render_widget(Paragraph::new(lines), area);
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

    #[test]
    fn theme_completion_keeps_prefix_then_executes_an_explicit_choice() {
        let mut app = app_with("");
        type_keys(&mut app, "/the");
        assert!(press_enter(&mut app).is_empty());
        assert_eq!(app.composer.content(), "/theme ");
        assert_eq!(
            app.slash_completion
                .as_ref()
                .unwrap()
                .items
                .iter()
                .map(|e| e.as_str())
                .collect::<Vec<_>>(),
            vec!["/theme dark", "/theme light"]
        );
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
        )));
        assert!(press_enter(&mut app).is_empty());
        assert_eq!(app.theme, ThemeKind::Light);
        assert!(app.composer.content().is_empty());
    }

    #[test]
    fn tool_completion_keeps_required_arguments_editable_without_requests() {
        let mut app = app_with("");
        type_keys(&mut app, "/too");
        assert!(press_enter(&mut app).is_empty());
        assert_eq!(app.composer.content(), "/tool ");
        assert!(app.slash_completion.is_none());
        assert!(
            app.notices()
                .iter()
                .any(|notice| notice.text.contains("<tool_call_id>"))
        );
    }

    #[test]
    fn completion_purpose_and_controls_fit_wide_and_narrow_menus() {
        let mut app = app_with("");
        type_keys(&mut app, "/");
        for width in [160, 80, 60, 40] {
            let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
            terminal
                .draw(|frame| {
                    render_completion(frame, Rect::new(0, 0, width, 8), &app, &Theme::dark())
                })
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("/model"));
            assert!(
                text.contains("Tab"),
                "completion controls missing at {width}"
            );
            assert!(text.contains("Esc"), "dismiss hint missing at {width}");
            assert!(text.contains("1/6"), "position missing at {width}");
            let rows = terminal
                .backend()
                .buffer()
                .content
                .chunks(width as usize)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();
            assert!(rows[1].contains("→ /model"));
            assert!(
                rows[7].contains("Esc") && rows[7].contains("1/6"),
                "controls own the row after all six root entries"
            );
            assert!(
                rows.iter()
                    .all(|row| UnicodeWidthStr::width(row.as_str()) <= width as usize)
            );
            if width >= 80 {
                assert!(text.contains("choose a model"));
            }
        }
        app.slash_completion.as_mut().unwrap().selected = 5;
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| render_completion(frame, Rect::new(0, 0, 80, 8), &app, &Theme::dark()))
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(rows[6].contains("→ /app"));
        assert!(rows[7].contains("6/6"));
        assert!(rows.iter().filter(|row| row.contains("→ /")).count() == 1);
        app.composer_mut().clear();
        type_keys(&mut app, "/help");
        assert!(
            crate::ui::layout::composer_completion_rows(&app) >= 3,
            "a filtered menu retains its breadcrumb and control hint"
        );
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
    fn command_menu_summaries_and_readable_hints_fit_every_supported_size_and_theme() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = Theme::for_kind(kind);
            for (width, height) in [(160, 48), (80, 24), (60, 16)] {
                let mut app = app_with("");
                app.update(AppEvent::SetTheme(kind));
                for spec in crate::command::COMMANDS {
                    app.composer.clear();
                    type_keys(&mut app, &format!("/{}", spec.name));
                    let area = Rect::new(0, 0, width, height);
                    let screen = crate::ui::layout::screen_layout(&app, area);
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|frame| crate::ui::render(frame, &app))
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let rows = buffer
                        .content
                        .chunks(width as usize)
                        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                        .collect::<Vec<_>>();
                    let selected_rows = rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| row.contains("→ /"))
                        .collect::<Vec<_>>();
                    assert_eq!(selected_rows.len(), 1, "only one selected command");
                    let (selected_y, selected_row) = selected_rows[0];
                    assert!(selected_row.contains(&format!("→ /{}", spec.name)));
                    assert!(
                        selected_row.contains(spec.menu_summary),
                        "/{} summary clipped at {width}×{height}: {selected_row}",
                        spec.name
                    );
                    let selected_cell = buffer
                        .cell((screen.panel.x + rail::RAIL_WIDTH as u16, selected_y as u16))
                        .unwrap();
                    assert_eq!(selected_cell.fg, theme.rail_editor);
                    assert_eq!(selected_cell.bg, theme.user_message_bg);

                    let hint_y = screen.panel.bottom() - 1;
                    let hint = &rows[hint_y as usize];
                    assert!(hint.contains("Tab fill"));
                    let enter = if matches!(
                        spec.args,
                        crate::command::CommandArgs::Theme | crate::command::CommandArgs::ToolRef
                    ) {
                        "Enter fill"
                    } else {
                        "Enter run"
                    };
                    assert!(hint.contains(enter), "{hint}");
                    assert!(hint.contains("Esc"));
                    assert!(hint.contains(&format!(
                        "1/{}",
                        app.slash_completion.as_ref().unwrap().items.len()
                    )));
                    assert_eq!(screen.footer.y, hint_y + 1, "menu cannot cover the footer");
                    for x in screen.panel.x + rail::RAIL_WIDTH as u16..screen.panel.right() {
                        let cell = buffer.cell((x, hint_y)).unwrap();
                        if cell.symbol().trim().is_empty() {
                            continue;
                        }
                        assert_eq!(cell.fg, theme.text);
                        assert_eq!(cell.bg, theme.user_message_bg);
                        assert!(
                            contrast_ratio(cell.fg, cell.bg) >= 4.5,
                            "shortcut hint needs 4.5:1 contrast in {kind:?}"
                        );
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
    fn required_theme_choice_keeps_the_menu_hint_and_draft_in_both_themes() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            for (width, height) in [(160, 48), (80, 24), (60, 16)] {
                let mut app = app_with("");
                app.update(AppEvent::SetTheme(kind));
                type_keys(&mut app, "/the");
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let capture = |terminal: &mut Terminal<TestBackend>, app: &App| {
                    terminal
                        .draw(|frame| crate::ui::render(frame, app))
                        .unwrap();
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                };
                let before = capture(&mut terminal, &app);
                assert!(before.contains("choose dark or light colors"));
                assert!(before.contains("Enter fill"));
                assert!(press_enter(&mut app).is_empty());
                assert_eq!(app.composer.content(), "/theme ");
                assert_eq!(app.theme, kind, "bare theme must not change the palette");
                let choices = capture(&mut terminal, &app);
                assert!(choices.contains("→ /theme dark"));
                assert!(choices.contains("/theme light"));
                assert!(choices.contains("Enter run"));
                assert!(choices.contains("1/2"));
                app.update(AppEvent::Terminal(crossterm::event::Event::Key(
                    crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Down,
                        crossterm::event::KeyModifiers::NONE,
                    ),
                )));
                let moved = capture(&mut terminal, &app);
                assert!(moved.contains("→ /theme light"));
                assert!(moved.contains("2/2"));
            }
        }
    }
}
