//! The help view: the key table, then the slash commands.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::commands::Command;

use super::{chrome, styled};
use crate::ui::theme::Styles;
use crate::ui::wrap::wrap;

struct KeyRow {
    keys: &'static str,
    action: &'static str,
}

const KEYS: &[KeyRow] = &[
    KeyRow {
        keys: "enter",
        action: "send, or steer while a turn runs",
    },
    KeyRow {
        keys: "alt+enter",
        action: "queue a follow-up (sends when idle)",
    },
    KeyRow {
        keys: "shift+enter, ctrl+j",
        action: "newline",
    },
    KeyRow {
        keys: "esc",
        action: "close view · interrupt turn · clear draft",
    },
    KeyRow {
        keys: "ctrl+c",
        action: "clear the composer; twice to quit",
    },
    KeyRow {
        keys: "ctrl+d",
        action: "quit when the composer is empty",
    },
    KeyRow {
        keys: "↑ ↓",
        action: "move in the text, then browse prompt history",
    },
    KeyRow {
        keys: "alt+↑",
        action: "pull the last queued message back",
    },
    KeyRow {
        keys: "ctrl+x",
        action: "copy the last answer",
    },
    KeyRow {
        keys: "ctrl+g",
        action: "edit the draft in `editor`, $VISUAL or $EDITOR",
    },
    KeyRow {
        keys: "ctrl+←/→, alt+←/→",
        action: "move by word",
    },
    KeyRow {
        keys: "ctrl+w, ctrl/alt+backspace",
        action: "delete the previous word",
    },
    KeyRow {
        keys: "alt+d, alt+delete",
        action: "delete the next word",
    },
    KeyRow {
        keys: "/",
        action: "command palette (at draft start)",
    },
    KeyRow {
        keys: "@",
        action: "file picker (at word start)",
    },
    KeyRow {
        keys: "ctrl+r",
        action: "sessions",
    },
    KeyRow {
        keys: "ctrl+l",
        action: "model",
    },
    KeyRow {
        keys: "shift+tab",
        action: "cycle the reasoning level",
    },
    KeyRow {
        keys: "ctrl+o",
        action: "expand or fold tool cards and thinking",
    },
    KeyRow {
        keys: "ctrl+t",
        action: "collapse or show thinking",
    },
    KeyRow {
        keys: "ctrl+f",
        action: "find in the transcript",
    },
    KeyRow {
        keys: "pgup/pgdn, wheel",
        action: "scroll the transcript (alt+wheel faster)",
    },
    KeyRow {
        keys: "home/end, ctrl+home/end",
        action: "top (home with an empty draft) · follow the end",
    },
    KeyRow {
        keys: "ctrl+↑ ↓",
        action: "jump between user messages",
    },
    KeyRow {
        keys: "drag, double/triple-click",
        action: "select text, a word, a line; copies on release",
    },
    KeyRow {
        keys: "f1",
        action: "this help",
    },
];

/// Width of the key column; a description wraps in what is left.
const KEY_COLUMN: usize = 28;

/// Below this the description goes under its key instead of beside it.
const SIDE_BY_SIDE: usize = KEY_COLUMN + 24;

/// One key or command and its description: side by side with a hanging indent
/// when `width` allows, else the description wrapped under the key.
fn entry(key: String, action: &str, styles: Styles, width: usize) -> Vec<Line<'static>> {
    let key = Span::styled(format!("{key:<KEY_COLUMN$}"), styles.accent());
    if width >= SIDE_BY_SIDE {
        let mut rows = wrap(action, width - KEY_COLUMN).into_iter();
        let first = rows.next().unwrap_or_default();
        let mut lines = vec![Line::from(vec![key, Span::styled(first, styles.text())])];
        lines.extend(rows.map(|row| {
            Line::from(vec![
                Span::raw(" ".repeat(KEY_COLUMN)),
                Span::styled(row, styles.text()),
            ])
        }));
        return lines;
    }
    let mut lines = vec![Line::from(Span::styled(
        key.content.trim_end().to_owned(),
        styles.accent(),
    ))];
    lines.extend(
        wrap(action, width.saturating_sub(2).max(1))
            .into_iter()
            .map(|row| Line::from(Span::styled(format!("  {row}"), styles.text()))),
    );
    lines
}

/// The key table, then the commands, wrapped to `width`.
pub fn lines(commands: &[Command], styles: Styles, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![styled("Keys", styles.dim())];
    for row in KEYS {
        lines.extend(entry(row.keys.to_owned(), row.action, styles, width));
    }
    lines.push(Line::from(""));
    lines.push(styled("Commands", styles.dim()));
    for command in commands {
        let name = format!("/{}", command.name);
        lines.extend(entry(name, command.description, styles, width));
    }
    lines
}

pub fn render(frame: &mut Frame, area: Rect, styles: Styles, commands: &[Command], scroll: usize) {
    let lines = lines(commands, styles, area.width as usize);
    let bottom = area.bottom().saturating_sub(1);
    let rows = bottom.saturating_sub(area.y + 1) as usize;
    let start = scroll.min(lines.len().saturating_sub(rows));
    let position = format!(
        "scroll · lines {}–{} of {}",
        start + 1,
        (start + rows).min(lines.len()),
        lines.len()
    );
    let hints = if lines.len() > rows {
        vec![("pgup/pgdn", position.as_str()), ("esc", "back")]
    } else {
        vec![("esc", "back")]
    };
    chrome(frame, area, styles, "Help", &hints);
    for (row, line) in lines.iter().skip(start).take(rows).enumerate() {
        frame
            .buffer_mut()
            .set_line(area.x, area.y + 1 + row as u16, line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::super::capture;
    use super::*;
    use crate::app::commands::COMMANDS;
    use crate::ui::theme::Palette;
    use crate::ui::wrap::width as cells;

    fn styles() -> Styles {
        Styles::of(Palette::dark())
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    #[test]
    fn help_lists_every_key_row_and_command() {
        let text = text(&lines(COMMANDS, styles(), 80)).join("\n");
        assert!(text.contains("ctrl+f"), "{text}");
        assert!(text.contains("/compact"), "{text}");
        assert!(text.contains("Compact the session context now"), "{text}");
    }

    #[test]
    fn help_scrolling_shows_a_later_window_and_where_it_is() {
        let total = lines(COMMANDS, styles(), 80).len();
        let shot = |scroll| {
            capture(
                |frame| render(frame, frame.area(), styles(), COMMANDS, scroll),
                80,
                10,
            )
        };
        let text = shot(5);
        assert!(text[0].starts_with("← Help"), "{:?}", text[0]);
        assert!(!text.join("\n").contains("send, or steer"), "{text:?}");
        assert!(
            text[9].contains(&format!("lines 6–13 of {total}")),
            "{:?}",
            text[9]
        );
        // Scrolling past the end stops at the last window.
        let last = shot(999);
        assert!(
            last[9].contains(&format!("lines {}–{total} of {total}", total - 7)),
            "{:?}",
            last[9]
        );
    }

    #[test]
    fn narrow_help_wraps_without_losing_a_word() {
        let words = |rows: &[String]| -> Vec<String> {
            rows.iter()
                .flat_map(|row| row.split_whitespace().map(str::to_owned))
                .collect()
        };
        let mut expected = vec!["Keys".to_owned()];
        for row in KEYS {
            expected.extend(words(&[row.keys.to_owned(), row.action.to_owned()]));
        }
        expected.push("Commands".to_owned());
        for command in COMMANDS {
            expected.extend(words(&[
                format!("/{}", command.name),
                command.description.to_owned(),
            ]));
        }
        for width in [30, 50, 60, 80] {
            let rows = text(&lines(COMMANDS, styles(), width));
            assert_eq!(words(&rows), expected, "width {width}");
            for row in &rows {
                assert!(cells(row) <= width, "{row:?} at width {width}");
            }
        }
    }
}
