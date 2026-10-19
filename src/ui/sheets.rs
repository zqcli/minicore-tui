//! The bottom sheets that are not pickers: the approval/interaction prompt and
//! the find bar. Both are built here from app state so the reducer and the
//! renderer agree on rows, hints and key handling.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Palette;

use super::panel::{Row, Sheet, Tone, body_lines};

/// What an interaction is about: the tool, its one-line detail (a path, a
/// command) and the start of its input (the content or script it would run).
pub struct Subject<'a> {
    pub tool: &'a str,
    pub detail: &'a str,
    pub input: Option<&'a str>,
}

/// Input lines an approval shows before it says how many more there are.
const INPUT_PREVIEW: usize = 6;

/// The sheet for a pending interaction: the question, what would run, then an
/// approval with two rows or a choice list. A text answer is not offered
/// because the Agent's tool-input interactions are choices.
pub fn interaction_sheet(
    prompt: &str,
    subject: &Subject<'_>,
    options: &[String],
    selected: usize,
    palette: &Palette,
) -> Sheet {
    let mut body = body_lines(prompt, 72, Style::default().fg(palette.text));
    if !subject.tool.is_empty() {
        body.push(Line::from(vec![
            Span::styled(
                format!("{}  ", subject.tool),
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                super::panel::truncate(subject.detail.lines().next().unwrap_or(""), 100),
                Style::default().fg(palette.text),
            ),
        ]));
    }
    if let Some(input) = subject.input.filter(|input| !input.trim().is_empty()) {
        let style = Style::default().fg(palette.dim);
        let lines: Vec<&str> = input.lines().collect();
        for line in lines.iter().take(INPUT_PREVIEW) {
            body.push(Line::styled(
                format!("  {}", super::panel::truncate(line, 100)),
                style,
            ));
        }
        if lines.len() > INPUT_PREVIEW {
            body.push(Line::styled(
                format!("  … {} more lines", lines.len() - INPUT_PREVIEW),
                Style::default().fg(palette.faint),
            ));
        }
    }
    let rows = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let tone = if index == 0 {
                Tone::Success
            } else {
                Tone::Failure
            };
            Row::new(option.clone(), String::new()).tone(tone)
        })
        .collect();
    Sheet::new("Approval", "enter confirm · y allow · n deny · esc deny")
        .body(body)
        .rows(rows)
        .selected(Some(selected.min(options.len().saturating_sub(1))))
}

/// Hint row of the find bar, which changes with the match count.
pub fn find_hint(total: usize, current: usize) -> String {
    match total {
        0 => "no matches · enter next · esc close".into(),
        _ => format!(
            "{} of {} · enter next · shift+enter previous · esc close",
            current + 1,
            total
        ),
    }
}

/// Title row of the find bar.
pub fn find_title(query: &str) -> String {
    if query.is_empty() {
        "Find".into()
    } else {
        format!("Find: {query}")
    }
}

/// One sessions-picker row: `label` (the title, or the first prompt), marked
/// when it is the resident session, with model and age on the right.
pub fn session_row(
    info: &crate::protocol::SessionInfo,
    label: String,
    resident: bool,
    now_seconds: u64,
) -> Row {
    let age = age_label(&info.updated_at, now_seconds);
    let detail = if age.is_empty() {
        info.model.clone()
    } else {
        format!("{} · {age}", info.model)
    };
    Row::new(label, detail).mark(resident)
}

/// `3h ago`, `2d ago`, or the empty string when the timestamp is not RFC 3339.
fn age_label(updated_at: &str, now: u64) -> String {
    let Some(then) = parse_unix_seconds(updated_at) else {
        return String::new();
    };
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// Seconds since the Unix epoch for a `YYYY-MM-DDTHH:MM:SS(.fff)Z` stamp.
fn parse_unix_seconds(stamp: &str) -> Option<u64> {
    let bytes = stamp.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

/// Howard Hinnant's days-from-civil, the inverse of the algorithm in
/// [`super::super::app::commands::today`].
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let adjusted_month = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * adjusted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `3m07s`, `1h02m`, `12s`. The status line and the turn-end line share this.
pub fn duration_label(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SessionInfo;
    use serde_json::json;

    fn info() -> SessionInfo {
        serde_json::from_value(json!({
            "session_id": "ses_1acdef0123456789abcdef", "title": "Fix the parser",
            "model": "gpt-6-luna",
            "updated_at": "2026-01-02T03:04:05.006Z"
        }))
        .unwrap()
    }

    fn now() -> u64 {
        parse_unix_seconds("2026-01-02T06:04:05.006Z").unwrap()
    }

    const NO_SUBJECT: Subject<'static> = Subject {
        tool: "",
        detail: "",
        input: None,
    };

    fn body_text(sheet: &Sheet) -> Vec<String> {
        sheet
            .body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_approval_sheet_offers_allow_then_deny() {
        let palette = Palette::dark();
        let options = vec!["Allow once".to_owned(), "Deny".to_owned()];
        let sheet = interaction_sheet("Write src/main.rs?", &NO_SUBJECT, &options, 0, &palette);
        assert_eq!(sheet.title, "Approval");
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.rows[0].label, "Allow once");
        assert_eq!(sheet.rows[0].tone, Tone::Success);
        assert_eq!(sheet.rows[1].tone, Tone::Failure);
        assert_eq!(sheet.selected, Some(0));
    }

    #[test]
    fn the_approval_sheet_wraps_the_prompt_and_shows_the_tool_detail() {
        let palette = Palette::dark();
        let options = vec!["Allow once".to_owned()];
        let sheet = interaction_sheet(
            "The agent wants to write a file that already exists and will be replaced entirely",
            &Subject {
                tool: "write",
                detail: "src/main.rs",
                input: None,
            },
            &options,
            0,
            &palette,
        );
        let body = body_text(&sheet);
        assert!(body.len() >= 3, "the prompt wraps: {body:?}");
        assert_eq!(body.last().unwrap(), "write  src/main.rs");
    }

    /// An approval says what would run: the command or the content, bounded.
    #[test]
    fn the_approval_sheet_previews_the_input_it_would_run() {
        let palette = Palette::dark();
        let options = vec!["Allow once".to_owned(), "Deny".to_owned()];
        let input = (1..=9)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let sheet = interaction_sheet(
            "Allow tool `write` for this call?",
            &Subject {
                tool: "write",
                detail: "notes.txt",
                input: Some(&input),
            },
            &options,
            0,
            &palette,
        );
        let body = body_text(&sheet);
        assert_eq!(body[1], "write  notes.txt");
        assert_eq!(body[2], "  line 1");
        assert_eq!(body[7], "  line 6");
        assert_eq!(body[8], "  … 3 more lines");
        assert_eq!(body.len(), 9);
    }

    #[test]
    fn a_selection_past_the_end_still_selects_a_row() {
        let palette = Palette::dark();
        let options = vec!["a".to_owned(), "b".to_owned()];
        let sheet = interaction_sheet("choose", &NO_SUBJECT, &options, 9, &palette);
        assert_eq!(sheet.selected, Some(1));
    }

    #[test]
    fn the_find_hint_counts_the_matches() {
        assert_eq!(find_hint(0, 0), "no matches · enter next · esc close");
        assert_eq!(
            find_hint(5, 2),
            "3 of 5 · enter next · shift+enter previous · esc close"
        );
    }

    #[test]
    fn the_find_title_shows_the_query_once_it_has_one() {
        assert_eq!(find_title(""), "Find");
        assert_eq!(find_title("parser"), "Find: parser");
    }

    #[test]
    fn a_session_row_marks_the_resident_session_and_shows_its_age() {
        let row = session_row(&info(), "Fix the parser".into(), true, now());
        assert_eq!(row.label, "Fix the parser");
        assert_eq!(row.mark, Some(true));
        assert_eq!(row.detail, "gpt-6-luna · 3h ago");
        let row = session_row(&info(), "Fix the parser".into(), false, now());
        assert_eq!(row.mark, Some(false));
    }

    #[test]
    fn the_resident_row_draws_an_accent_dot_and_others_stay_aligned() {
        let palette = Palette::dark();
        let sheet = Sheet::new("Sessions", "hint")
            .rows(vec![
                session_row(&info(), "Fix the parser".into(), true, now()),
                session_row(&info(), "Other".into(), false, now()),
            ])
            .selected(Some(1));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4)).unwrap();
        terminal
            .draw(|frame| super::super::panel::render(frame, frame.area(), &palette, &sheet))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = |y: u16| -> String { (0..60).map(|x| buffer[(x, y)].symbol()).collect() };
        assert!(row(1).starts_with(" ● Fix the parser"), "{:?}", row(1));
        assert!(row(2).starts_with("   Other"), "{:?}", row(2));
        assert_eq!(buffer[(1u16, 1u16)].fg, palette.accent);
    }

    #[test]
    fn ages_cover_every_unit() {
        let base = parse_unix_seconds("2026-01-02T03:04:05Z").unwrap();
        assert_eq!(age_label("2026-01-02T03:04:05Z", base + 30), "just now");
        assert_eq!(age_label("2026-01-02T03:04:05Z", base + 600), "10m ago");
        assert_eq!(age_label("2026-01-02T03:04:05Z", base + 7200), "2h ago");
        assert_eq!(age_label("2026-01-02T03:04:05Z", base + 172_800), "2d ago");
        assert_eq!(age_label("not a timestamp", base), "");
    }

    #[test]
    fn rfc3339_parsing_handles_the_agent_format() {
        let seconds = parse_unix_seconds("2026-01-02T03:04:05.006Z").unwrap();
        assert_eq!(seconds, 1_767_323_045);
        assert_eq!(parse_unix_seconds("1767323045"), None);
        assert_eq!(parse_unix_seconds("2026-13-02T03:04:05Z"), None);
    }

    #[test]
    fn durations_switch_units_at_a_minute_and_an_hour() {
        use std::time::Duration;
        assert_eq!(duration_label(Duration::from_secs(12)), "12s");
        assert_eq!(duration_label(Duration::from_secs(187)), "3m07s");
        assert_eq!(duration_label(Duration::from_secs(3720)), "1h02m");
    }
}
