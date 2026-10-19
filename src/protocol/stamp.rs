//! The UTC timestamps the Agent writes.

/// Milliseconds since the Unix epoch for a `YYYY-MM-DDTHH:MM:SS(.fff)Z` stamp.
pub fn unix_millis(stamp: &str) -> Option<u64> {
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
    let fraction: String = stamp[19..]
        .strip_prefix('.')
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_digit)
        .chain("00".chars())
        .take(3)
        .collect();
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(u64::try_from(seconds).ok()? * 1000 + fraction.parse::<u64>().unwrap_or(0))
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let adjusted_month = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * adjusted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_parsing_handles_the_agent_format() {
        assert_eq!(
            unix_millis("2026-01-02T03:04:05.006Z"),
            Some(1_767_323_045_006)
        );
        assert_eq!(unix_millis("2026-01-02T03:04:05Z"), Some(1_767_323_045_000));
        assert_eq!(
            unix_millis("2026-01-02T03:04:05.5Z"),
            Some(1_767_323_045_500)
        );
        assert_eq!(unix_millis("1767323045"), None);
        assert_eq!(unix_millis("2026-13-02T03:04:05Z"), None);
    }
}
