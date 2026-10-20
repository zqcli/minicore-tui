//! The help view: the key table, then the slash commands.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::commands::Command;

use super::{chrome, styled};
use crate::ui::theme::Styles;

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

/// The key table, then the commands.
pub fn lines(commands: &[Command<'static>], styles: Styles) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(styled("Keys", styles.dim()));
    for row in KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<28}", row.keys), styles.accent()),
            Span::styled(row.action, styles.text()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(styled("Commands", styles.dim()));
    for command in commands {
        lines.push(Line::from(vec![
            Span::styled(format!("/{:<27}", command.name), styles.accent()),
            Span::styled(command.description, styles.text()),
        ]));
    }
    lines
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    lines: &[Line<'static>],
    scroll: usize,
) {
    chrome(
        frame,
        area,
        styles,
        "Help",
        &[("pgup/pgdn", "scroll"), ("esc", "back")],
    );
    let bottom = area.bottom().saturating_sub(1);
    let rows = bottom.saturating_sub(area.y + 1) as usize;
    let start = scroll.min(lines.len());
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

    #[test]
    fn help_lists_every_key_row_and_command() {
        let styles = Styles::of(Palette::dark());
        let lines = lines(COMMANDS, styles);
        let text: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("ctrl+f"), "{text}");
        assert!(text.contains("/compact"), "{text}");
        assert!(text.contains("Compact the session context now"), "{text}");
    }

    #[test]
    fn help_scrolling_shows_a_later_window() {
        let styles = Styles::of(Palette::dark());
        let lines = lines(&[], styles);
        let text = capture(
            |frame| render(frame, frame.area(), styles, &lines, 5),
            80,
            10,
        );
        assert!(text[0].starts_with("← Help"), "{:?}", text[0]);
        assert!(!text.join("\n").contains("send, or steer"), "{text:?}");
    }
}
