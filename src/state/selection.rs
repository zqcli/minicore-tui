//! The dock and its panels (development spec 24-28): the composer, the
//! new-session form, and the session/model/reasoning/profile selectors.
//! Pure data plus the filter/sort helpers shared by the update and render
//! phases; every mutation happens inside `App::update`.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::protocol::{ModelInfo, ProfileInfo, Reasoning, SessionInfo};

/// Fixed page step for `PageSelector`. The app has no terminal geometry, so
/// paging uses a stable constant rather than a viewport-dependent height.
pub const SELECTOR_PAGE: usize = 6;

/// What occupies the dock area below the transcript (spec 24.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dock {
    Composer,
    NewSession(NewSessionState),
    SessionSelector(SelectorState),
    ModelSelector(SelectorState),
    ReasoningSelector(SelectorState),
    ProfileSelector(SelectorState),
    Help,
    Logs,
}

/// The highlighted field in the new-session form (spec 25.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSessionField {
    Workspace,
    Profile,
    Model,
    Reasoning,
    Title,
    Create,
}

/// The new-session form (spec 25.1). Selecting a profile/model/reasoning
/// happens through the matching selector on top of this form; the form is
/// the single working draft the user is assembling, never the active
/// session or the catalog defaults (spec 25.2, 26.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionState {
    pub workspace: String,
    pub profile: String,
    pub model: String,
    pub reasoning: Reasoning,
    pub title: String,
    pub field: NewSessionField,
    pub submitting: bool,
    pub error: Option<String>,
    /// Char offset inside the editable workspace/title field being typed;
    /// always on a char boundary.
    pub field_cursor: usize,
    /// Marker carried by the `session.create` request issued for this
    /// draft; a response only touches the matching draft (spec 25.5).
    pub draft_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorKind {
    Session,
    Model,
    Reasoning,
    Profile,
}

/// One selector panel (spec 24.1). `cursor` indexes into the filtered item
/// list; the filter/sort helpers below are shared with the render phase so
/// the update and render phases always agree on the listed items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorState {
    pub kind: SelectorKind,
    pub query: String,
    pub cursor: usize,
    /// True while a `session.open` from this panel is in flight; further
    /// confirms are ignored until the response lands.
    pub submitting: bool,
    /// A failed open, kept with the panel so the query and selection
    /// survive the error (spec 28.6).
    pub error: Option<String>,
}

impl SelectorState {
    pub fn new(kind: SelectorKind) -> Self {
        Self {
            kind,
            query: String::new(),
            cursor: 0,
            submitting: false,
            error: None,
        }
    }
}

/// Models matching the case-insensitive substring over `id` and
/// `model_ref`, in catalog order (spec 26.2).
pub fn filtered_models<'a>(models: &'a [ModelInfo], query: &str) -> Vec<&'a ModelInfo> {
    let needle = query.trim().to_lowercase();
    models
        .iter()
        .filter(|model| {
            needle.is_empty()
                || model.id.to_lowercase().contains(&needle)
                || model.model_ref.to_lowercase().contains(&needle)
        })
        .collect()
}

/// Profiles matching the case-insensitive substring over `id`, in catalog
/// order.
pub fn filtered_profiles<'a>(profiles: &'a [ProfileInfo], query: &str) -> Vec<&'a ProfileInfo> {
    let needle = query.trim().to_lowercase();
    profiles
        .iter()
        .filter(|profile| needle.is_empty() || profile.id.to_lowercase().contains(&needle))
        .collect()
}

/// Sessions matching the case-insensitive substring over
/// title/workspace/session_id/model/profile, newest `updated_at` first.
/// RFC3339 timestamps compare stably as strings at the same precision, so
/// no clock is involved (spec 28.2, 28.3).
pub fn filtered_sessions<'a>(sessions: &'a [SessionInfo], query: &str) -> Vec<&'a SessionInfo> {
    let needle = query.trim().to_lowercase();
    let mut out: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|session| needle.is_empty() || session_matches(session, &needle))
        .collect();
    out.sort_by(session_order);
    out
}

/// Total order for the session selector: valid `updated_at` timestamps by
/// absolute instant (newest first), so equal wall-clock text at different
/// offsets still orders by real time. Unparsable timestamps sort last;
/// ties break on the `updated_at` string then the `session_id`, both
/// deterministic (spec 28.2).
fn session_order(a: &&SessionInfo, b: &&SessionInfo) -> Ordering {
    let tie = b
        .updated_at
        .cmp(&a.updated_at)
        .then_with(|| a.session_id.cmp(&b.session_id));
    match (parse_rfc3339(&a.updated_at), parse_rfc3339(&b.updated_at)) {
        (Some(ta), Some(tb)) => tb.cmp(&ta).then(tie),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => tie,
    }
}

fn session_matches(session: &SessionInfo, needle: &str) -> bool {
    session
        .title
        .as_deref()
        .unwrap_or("")
        .to_lowercase()
        .contains(needle)
        || session.workspace.to_lowercase().contains(needle)
        || session.session_id.to_lowercase().contains(needle)
        || session.model.to_lowercase().contains(needle)
        || session.profile.to_lowercase().contains(needle)
}

/// The reasoning levels a model supports, in the agent's listed order;
/// empty when the model is unknown (spec 27.1).
pub fn supported_reasoning(models: &[ModelInfo], model_id: &str) -> Vec<Reasoning> {
    models
        .iter()
        .find(|model| model.id == model_id)
        .map(|model| model.supported_reasoning.clone())
        .unwrap_or_default()
}

pub fn reasoning_label(reasoning: Reasoning) -> &'static str {
    match reasoning {
        Reasoning::Disabled => "disabled",
        Reasoning::Auto => "auto",
        Reasoning::Low => "low",
        Reasoning::Medium => "medium",
        Reasoning::High => "high",
    }
}

pub fn reasoning_description(reasoning: Reasoning) -> &'static str {
    match reasoning {
        Reasoning::Disabled => "No reasoning",
        Reasoning::Auto => "Provider default",
        Reasoning::Low => "Light reasoning",
        Reasoning::Medium => "Moderate reasoning",
        Reasoning::High => "Deep reasoning",
    }
}

/// `2026-01-02T03:04:05.006Z` (also accepts `[+-]HH:MM` offsets; fraction
/// precision beyond the second is ignored). The presentation helpers parse
/// with the same function, so sorting and the rendered relative age always
/// agree; unparsable text yields `None`.
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let (date, tail) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (clock, offset) = if tail.ends_with('Z') || tail.ends_with('z') {
        (&tail[..tail.len() - 1], 0i64)
    } else {
        split_offset(tail)
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.split('.').next()?.parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let total = days * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    UNIX_EPOCH.checked_add(Duration::from_secs(total as u64))
}

fn split_offset(tail: &str) -> (&str, i64) {
    let bytes = tail.as_bytes();
    let len = bytes.len();
    if len >= 6 && (bytes[len - 6] == b'+' || bytes[len - 6] == b'-') && bytes[len - 3] == b':' {
        let hour: i64 = tail[len - 5..len - 3].parse().unwrap_or(0);
        let minute: i64 = tail[len - 2..].parse().unwrap_or(0);
        let sign = if bytes[len - 6] == b'-' { -1 } else { 1 };
        return (&tail[..len - 6], sign * (hour * 3_600 + minute * 60));
    }
    (tail, 0)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Reasoning as R;

    fn session(id: &str, updated_at: &str) -> SessionInfo {
        SessionInfo {
            session_id: id.to_owned(),
            title: None,
            profile: "p".to_owned(),
            workspace: "/w".to_owned(),
            model: "m".to_owned(),
            reasoning: R::High,
            loaded: false,
            instance_id: None,
            created_at: updated_at.to_owned(),
            updated_at: updated_at.to_owned(),
        }
    }

    #[test]
    fn parse_rfc3339_handles_fractions_offsets_and_future() {
        let parsed = parse_rfc3339("2027-01-15T08:00:00.000Z").unwrap();
        assert_eq!(parsed, UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        // A -03:00 offset displaying 05:00 is the same absolute instant as
        // 08:00Z.
        let parsed = parse_rfc3339("2027-01-15T05:00:00.000-03:00").unwrap();
        assert_eq!(parsed, UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        assert_eq!(
            parse_rfc3339("2027-01-15T08:00:00Z"),
            parse_rfc3339("2027-01-15T08:00:00.000Z")
        );
        assert!(parse_rfc3339("not a timestamp").is_none());
        assert!(parse_rfc3339("2027-13-15T08:00:00.000Z").is_none());
        assert!(parse_rfc3339("2027-01-15T99:00:00.000Z").is_none());
    }

    #[test]
    fn sessions_sort_by_absolute_instant_across_offsets() {
        let sessions = vec![
            // Same displayed clock, different offsets: identical instants.
            session("west", "2027-01-15T05:00:00-03:00"),
            session("utc", "2027-01-15T08:00:00Z"),
            // A genuinely later instant: 11:00+02:00 is 09:00Z, after utc's
            // 08:00Z even though its digits sort below utc's wall clock.
            session("later", "2027-01-15T11:00:00+02:00"),
        ];
        let ids: Vec<&str> = filtered_sessions(&sessions, "")
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        // later (09:00Z-ish) precedes the two equal instants, which tie on
        // the updated_at string (descending) then id.
        assert_eq!(ids[0], "later");
        assert!(ids[1] == "utc" || ids[1] == "west");
        assert_eq!(ids[2], if ids[1] == "utc" { "west" } else { "utc" });
    }

    #[test]
    fn session_sort_ties_by_string_then_id_and_invalid_sorts_last() {
        let sessions = vec![
            session("older", "2027-01-15T08:00:00.600Z"),
            session("newer", "2027-01-15T08:00:00.900Z"),
            session("broken", "not-a-time"),
            session("broken2", "also-broken"),
        ];
        let ids: Vec<&str> = filtered_sessions(&sessions, "")
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        // Same absolute second; the fraction makes the string tie-break put
        // the .900 entry first. Both unparsable rows sort last, stably.
        assert_eq!(ids[0], "newer");
        assert_eq!(ids[1], "older");
        assert!(ids.ends_with(&["broken", "broken2"]));
    }
}
