//! The bottom sheets that are not pickers: the approval/interaction prompt and
//! the find bar. Both are built here from app state so the reducer and the
//! renderer agree on rows, hints and key handling.

use ratatui::style::Style;
use ratatui::text::Line;

use crate::theme::Palette;

use super::panel::{Row, Sheet, Tone, body_lines};

/// The sheet for a pending interaction: an approval with two rows, or a
/// choice list. A text answer is not offered because the Agent's tool-input
/// interactions are choices; the composer stays available when one appears.
pub fn interaction_sheet(
    prompt: &str,
    detail: Option<&str>,
    options: &[String],
    selected: usize,
    palette: &Palette,
) -> Sheet {
    let mut body = body_lines(prompt, 72, Style::default().fg(palette.text));
    if let Some(detail) = detail {
        body.push(Line::from(""));
        body.extend(body_lines(detail, 72, Style::default().fg(palette.dim)));
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

/// One sessions-picker row: the title (or the first prompt), marked when it is
/// the resident session, with model and age on the right.
pub fn session_row(info: &crate::protocol::SessionInfo, resident: bool, now_seconds: u64) -> Row {
    let title = info
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| crate::app::short_session_id(&info.session_id).to_owned());
    let label = format!("{}{}", if resident { "● " } else { "  " }, title);
    let age = age_label(&info.updated_at, now_seconds);
    let detail = format!("{} · {age}", info.model);
    Row::new(label, detail)
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

    #[test]
    fn the_approval_sheet_offers_allow_then_deny() {
        let palette = Palette::dark();
        let options = vec!["Allow once".to_owned(), "Deny".to_owned()];
        let sheet = interaction_sheet("Write src/main.rs?", None, &options, 0, &palette);
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
            Some("src/main.rs"),
            &options,
            0,
            &palette,
        );
        assert!(sheet.body.len() > 3, "{:?}", sheet.body);
    }

    #[test]
    fn a_selection_past_the_end_still_selects_a_row() {
        let palette = Palette::dark();
        let options = vec!["a".to_owned(), "b".to_owned()];
        let sheet = interaction_sheet("choose", None, &options, 9, &palette);
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
        let row = session_row(&info(), true, now());
        assert!(row.label.starts_with("● Fix the parser"), "{:?}", row.label);
        assert_eq!(row.detail, "gpt-6-luna · 3h ago");
        let row = session_row(&info(), false, now());
        assert!(row.label.starts_with("  Fix the parser"), "{:?}", row.label);
    }

    #[test]
    fn an_untitled_session_falls_back_to_its_short_id() {
        let mut info = info();
        info.title = None;
        let row = session_row(&info, false, now());
        assert_eq!(row.label, "  ses_1acdef", "{:?}", row.label);
        assert!(
            !row.label.contains("0123456789"),
            "a 32-hex id is unreadable as a row label: {:?}",
            row.label
        );
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
