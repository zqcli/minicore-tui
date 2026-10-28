//! The UTC timestamps the Agent writes.

use jiff::Timestamp;

/// Milliseconds since the Unix epoch now.
pub fn now_millis() -> u64 {
    Timestamp::now().as_millisecond() as u64
}

/// Milliseconds since the Unix epoch for an RFC 3339 stamp.
pub fn unix_millis(stamp: &str) -> Option<u64> {
    u64::try_from(stamp.parse::<Timestamp>().ok()?.as_millisecond()).ok()
}

/// The stamp for `millis` since the Unix epoch, as the Agent writes it: `2026-01-02T03:04:05.006Z`.
pub fn rfc3339_millis(millis: u64) -> String {
    let at = Timestamp::from_millisecond(millis as i64).expect("a time jiff can hold");
    format!("{at:.3}")
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
        assert_eq!(
            rfc3339_millis(1_767_323_045_006),
            "2026-01-02T03:04:05.006Z"
        );
        assert_eq!(rfc3339_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_millis(1_767_323_045_000),
            "2026-01-02T03:04:05.000Z",
            "the fraction always has three digits"
        );
        assert_eq!(
            unix_millis(&rfc3339_millis(1_767_323_045_006)),
            Some(1_767_323_045_006)
        );
        assert_eq!(rfc3339_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(unix_millis("2026-13-02T03:04:05Z"), None);
    }
}
