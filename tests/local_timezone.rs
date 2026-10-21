//! Integration tests compile the library without cfg(test): unlike UI snapshot
//! tests these exercise actual system-zone discovery and the production label.
use jiff::{Timestamp, tz::TimeZone};
use minicore_tui::{
    protocol::read::UserKind,
    transcript::{Block, layout::Fold},
    ui::{
        blocks,
        theme::{Palette, Styles},
    },
};

#[test]
fn system_zone_is_available_and_production_user_card_uses_it() {
    let zone = TimeZone::try_system().expect("system zone must not silently fall back to UTC");
    let stamp: Timestamp = "2026-06-01T06:54:00Z".parse().unwrap();
    let expected = stamp
        .to_zoned(zone.clone())
        .strftime("%H:%M · %m/%d/%Y")
        .to_string();
    let block = Block::User {
        text: "local time check".into(),
        kind: UserKind::Prompt,
        at: Some(stamp.as_millisecond() as u64),
    };
    let rows = blocks::render(
        &block,
        Styles::of(Palette::dark()),
        60,
        Fold {
            expanded: false,
            show_reasoning: true,
        },
    );
    let card = rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(card.contains(&expected), "expected {expected}: {card}");
    println!("system_zone={zone:?}; UTC 06:54 -> {expected}");
}

#[test]
fn named_zones_apply_date_rollover_and_daylight_saving_rules() {
    // Unix uses system zoneinfo; Windows uses the packaged IANA database.
    let shanghai = TimeZone::get("Asia/Shanghai").unwrap();
    let ny = TimeZone::get("America/New_York").unwrap();
    let label = |stamp: &str, zone: &TimeZone| {
        stamp
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(zone.clone())
            .strftime("%H:%M %Y-%m-%d %z")
            .to_string()
    };
    assert_eq!(
        label("2026-06-01T06:54:00Z", &shanghai),
        "14:54 2026-06-01 +0800"
    );
    assert_eq!(
        label("2026-06-01T20:54:00Z", &shanghai),
        "04:54 2026-06-02 +0800"
    );
    assert_eq!(label("2026-01-01T06:54:00Z", &ny), "01:54 2026-01-01 -0500");
    assert_eq!(label("2026-06-01T06:54:00Z", &ny), "02:54 2026-06-01 -0400");
}
