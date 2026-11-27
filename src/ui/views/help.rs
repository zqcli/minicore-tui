//! The help view: the key table, then the slash commands.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::commands::Command;

use super::styled;
use crate::ui::theme::Styles;
use crate::wrap::wrap;

struct KeyRow {
    keys: &'static str,
    action: &'static str,
}

const KEYS: &[KeyRow] = &[
    KeyRow {
        keys: "enter",
        action: "send, or steer while a turn runs; with no draft, resume a paused queue",
    },
    KeyRow {
        keys: "alt+enter, ctrl+enter",
        action: "send a follow-up; wait for a new turn while busy",
    },
    KeyRow {
        keys: "macOS: option+return",
        action: "follow-up; enable Option as Meta / +Esc in the terminal profile",
    },
    KeyRow {
        keys: "shift+enter, ctrl+j",
        action: "newline",
    },
    KeyRow {
        keys: "esc",
        action: "close view · clear selection · back to the parent from a child · interrupt turn, keep queue · clear draft (ctrl+z restores)",
    },
    KeyRow {
        keys: "ctrl+c",
        action: "copy selection; otherwise clear, twice to quit",
    },
    KeyRow {
        keys: "ctrl+d",
        action: "delete after the cursor; quit when the composer is empty",
    },
    KeyRow {
        keys: "delete · ctrl+z / ctrl+y",
        action: "delete as one edit · undo / redo, including paste chips",
    },
    KeyRow {
        keys: "↑ ↓",
        action: "move in the text, then browse prompt history",
    },
    KeyRow {
        keys: "alt+↑",
        action: "recall queued messages into the draft",
    },
    KeyRow {
        keys: "ctrl+x",
        action: "cut selection; otherwise copy the last answer",
    },
    KeyRow {
        keys: "ctrl+g",
        action: "edit in `editor`, $VISUAL or $EDITOR; errors keep the draft",
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
        keys: "/command, /cmd",
        action: "browse prompt templates; enter/tab fills /cmd name without sending",
    },
    KeyRow {
        keys: "ctrl+v, alt+v",
        action: "paste clipboard text or file/image paths (never send)",
    },
    KeyRow {
        keys: "@",
        action: "file browser: filter, folders or absolute path",
    },
    KeyRow {
        keys: "ctrl+r",
        action: "sessions; untitled rows use their short ID",
    },
    KeyRow {
        keys: "ctrl+l",
        action: "model",
    },
    KeyRow {
        keys: "ctrl+p, shift+ctrl+p",
        action: "next or previous model",
    },
    KeyRow {
        keys: "shift+tab",
        action: "cycle the reasoning level",
    },
    KeyRow {
        keys: "ctrl+o",
        action: "expand or fold tools, thinking, summaries and failure details",
    },
    KeyRow {
        keys: "ctrl+t",
        action: "collapse or show thinking with its original line breaks",
    },
    KeyRow {
        keys: "ctrl+f",
        action: "find in the transcript",
    },
    KeyRow {
        keys: "query: paste",
        action: "append to Find, Sessions, Models, Agents or Async Tasks filter; preserve the draft",
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
        keys: "editor: click / drag",
        action: "place caret / select draft text",
    },
    KeyRow {
        keys: "shift+arrows/home/end",
        action: "extend or shrink editor selection",
    },
    KeyRow {
        keys: "transcript: drag, 2/3-click",
        action: "select text, a word, a line; copies on release",
    },
    KeyRow {
        keys: "async task: click",
        action: "click a dispatched agent row to inspect and message that child in Ctrl+B",
    },
    KeyRow {
        keys: "f8",
        action: "review retained local confirmations; edit separately, warn before resend, no automatic retry",
    },
    KeyRow {
        keys: "ctrl+b, /tasks",
        action: "searchable async tasks workspace: type to filter, →/tab scroll real Working/Worked conversation, enter message child, ctrl+e full view, ctrl+g goto, ctrl+y copy",
    },
    KeyRow {
        keys: "∴ thinking: click",
        action: "open or fold that thinking",
    },
    KeyRow {
        keys: "work group: click",
        action: "list a run's steps, then open a step's card; folds stay in place",
    },
    KeyRow {
        keys: "tasks: ctrl+x in detail",
        action: "stop the selected child’s turn without clearing its queue",
    },
    KeyRow {
        keys: "f1",
        action: "this help; type to filter",
    },
    KeyRow {
        keys: "text answer: enter / esc",
        action: "send the separate answer / cancel that turn; conversation draft stays",
    },
    KeyRow {
        keys: "approval: y / n, esc",
        action: "allow this call / deny this call",
    },
    KeyRow {
        keys: "choice: ↑ ↓, enter / esc",
        action: "select and answer / cancel that turn",
    },
    KeyRow {
        keys: "sessions: ctrl+n / ctrl+d",
        action: "rename / review deleting the selected session; y confirms",
    },
    KeyRow {
        keys: "agents: ctrl+d / y",
        action: "review the selected Agent and its child sessions / delete that captured set",
    },
    KeyRow {
        keys: "remote: /remote",
        action: "expose this Host over WSS/WS for minicore-web; c copy key · u copy url · s hide · r rotate · esc/q stop",
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
pub fn lines(
    commands: &[Command<'static>],
    styles: Styles,
    width: usize,
    filter: &str,
) -> Vec<Line<'static>> {
    let filter = filter.to_lowercase();
    let wanted = |a: &str, b: &str| {
        filter.is_empty()
            || a.to_lowercase().contains(&filter)
            || b.to_lowercase().contains(&filter)
    };
    let mut lines = vec![styled("Keys", styles.dim())];
    for row in KEYS.iter().filter(|row| wanted(row.keys, row.action)) {
        lines.extend(entry(row.keys.to_owned(), row.action, styles, width));
    }
    lines.push(Line::from(""));
    lines.push(styled("Commands", styles.dim()));
    for command in commands
        .iter()
        .filter(|command| wanted(command.name, command.description))
    {
        let name = format!("/{}", command.name);
        lines.extend(entry(name, command.description, styles, width));
    }
    lines
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    commands: &[Command<'static>],
    scroll: &mut usize,
    filter: &str,
) {
    let content_w = usize::from(area.width.saturating_sub(2)).max(1);
    let lines = lines(commands, styles, content_w, filter);
    let rows = usize::from(area.height.saturating_sub(4));
    let start = (*scroll).min(lines.len().saturating_sub(rows));
    let position = format!(
        "lines {}–{} of {}",
        start + 1,
        (start + rows).min(lines.len()),
        lines.len()
    );
    let mut hints = vec![("type", "filter")];
    if !filter.is_empty() {
        hints.push(("ctrl+u", "clear"));
    }
    if lines.len() > rows {
        hints.push(("↑↓/pgup/pgdn", position.as_str()));
    }
    hints.push(("esc", "back"));
    let title = if filter.is_empty() {
        "Help".to_owned()
    } else {
        format!("Help · {filter}")
    };
    let (clamped, _) = super::page::render(
        frame,
        area,
        styles,
        &title,
        &hints,
        Some((filter, "type to filter keys & commands…")),
        &lines,
        *scroll,
    );
    *scroll = clamped;
}

#[cfg(test)]
mod tests {
    use super::super::capture;
    use super::*;
    use crate::app::commands::COMMANDS;
    use crate::ui::theme::Palette;
    use crate::wrap::width as cells;

    fn styles() -> Styles {
        Styles::of(Palette::dark())
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    #[test]
    fn help_lists_every_key_row_and_command() {
        let text = text(&lines(COMMANDS, styles(), 80, "")).join("\n");
        assert!(text.contains("ctrl+f"), "{text}");
        assert!(text.contains("copy selection"), "{text}");
        assert!(text.contains("editor: click / drag"), "{text}");
        assert!(text.contains("shift+arrows"), "{text}");
        assert!(text.contains("/compact"), "{text}");
        assert!(text.contains("Compact the session context now"), "{text}");
    }

    #[test]
    fn help_scrolling_shows_a_later_window_and_where_it_is() {
        let total = lines(COMMANDS, styles(), 78, "").len();
        let shot = |mut scroll| {
            capture(
                |frame| render(frame, frame.area(), styles(), COMMANDS, &mut scroll, ""),
                80,
                12,
            )
        };
        let text = shot(5);
        assert!(text[0].starts_with("← Help"), "{:?}", text[0]);
        assert!(text[1].starts_with("/ type to filter"), "{:?}", text[1]);
        assert!(!text.join("\n").contains("send, or steer"), "{text:?}");
        assert!(
            text[11].contains(&format!("lines 6–13 of {total}")),
            "{:?}",
            text[11]
        );
        // Scrolling past the end stops at the last window.
        let last = shot(999);
        assert!(
            last[11].contains(&format!("lines {}–{total} of {total}", total - 7)),
            "{:?}",
            last[11]
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
            let rows = text(&lines(COMMANDS, styles(), width, ""));
            assert_eq!(words(&rows), expected, "width {width}");
            for row in &rows {
                assert!(cells(row) <= width, "{row:?} at width {width}");
            }
        }
    }
}
