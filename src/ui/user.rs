//! The user message card: prompt cards and compact steering cards (spec r2).

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use time::OffsetDateTime;
use time::UtcOffset;
use time::format_description::well_known::Rfc3339;

use crate::markdown::{CopyCells, MarkdownRenderer, RenderedMarkdown};
use crate::protocol::UserMessageKindWire;
use crate::state::transcript::UserBlock;
use crate::theme::Theme;
use crate::ui::rail;

pub fn lines(theme: &Theme, block: &UserBlock, width: usize) -> Vec<Line<'static>> {
    lines_with_timestamp(theme, block, width, None, false)
}

pub fn lines_with_timestamp(
    theme: &Theme,
    block: &UserBlock,
    width: usize,
    timestamp: Option<&str>,
    timestamp_pending: bool,
) -> Vec<Line<'static>> {
    lines_with_timestamp_metadata(theme, block, width, timestamp, timestamp_pending).lines
}

pub fn lines_with_timestamp_metadata(
    theme: &Theme,
    block: &UserBlock,
    width: usize,
    timestamp: Option<&str>,
    timestamp_pending: bool,
) -> RenderedMarkdown {
    if block.text.trim().is_empty() {
        return RenderedMarkdown::default();
    }
    if block.kind == UserMessageKindWire::Steering {
        let lines = steering_lines(theme, &block.text, width, timestamp, timestamp_pending);
        let len = lines.len();
        let copy_cells = vec![None; len];
        return RenderedMarkdown {
            lines,
            copy_cells,
            ..RenderedMarkdown::default()
        };
    }
    let inset = rail::SURFACE_CONTENT_START;
    let mut rendered = MarkdownRenderer::new(theme).render_with_metadata(
        &block.text,
        rail::content_width(width, inset),
        Style::new().fg(theme.text),
    );
    rendered.lines = rendered
        .lines
        .into_iter()
        .map(|line| rail::surface_row(width, rail::user_colors(theme), inset, line))
        .collect();
    for row in &mut rendered.link_cells {
        for range in row {
            *range = range.start + inset..range.end + inset;
        }
    }
    for copy in rendered.copy_cells.iter_mut().flatten() {
        copy.columns = copy.columns.start + inset..copy.columns.end + inset;
    }
    let blank = || rail::surface_row(width, rail::user_colors(theme), inset, Line::default());
    rendered.lines.insert(0, blank());
    rendered.link_cells.insert(0, Vec::new());
    rendered.hard_breaks.insert(0, false);
    rendered.copy_cells.insert(0, Some(CopyCells::decoration()));
    rendered.lines.push(rail::surface_row(
        width,
        rail::user_colors(theme),
        inset,
        Line::from(Span::styled(
            display_timestamp(timestamp, timestamp_pending),
            Style::new().fg(theme.tool_muted),
        )),
    ));
    rendered.lines.push(blank());
    // A timestamp is selectable visible text (Rail selection contract),
    // but whole-message source-copy omits this owned penultimate row.
    rendered.link_cells.push(Vec::new());
    rendered.hard_breaks.push(true);
    rendered.copy_cells.push(None);
    rendered.link_cells.push(Vec::new());
    rendered.hard_breaks.push(false);
    rendered.copy_cells.push(Some(CopyCells::decoration()));
    rendered
}

pub fn steering_lines(
    theme: &Theme,
    text: &str,
    width: usize,
    timestamp: Option<&str>,
    timestamp_pending: bool,
) -> Vec<Line<'static>> {
    let colors = rail::user_colors(theme);
    let mut out = vec![rail::surface_row(
        width,
        colors,
        rail::SURFACE_CONTENT_START,
        Line::default(),
    )];
    let prefix = "↪ ";
    let available = rail::content_width(width, rail::SURFACE_CONTENT_START);
    let lines = crate::markdown::wrap_plain(
        &format!("{prefix}{text}"),
        available,
        Style::new().fg(theme.text),
    );
    for line in lines {
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            line,
        ));
    }
    let timestamp = display_timestamp(timestamp, timestamp_pending);
    out.push(rail::surface_row(
        width,
        colors,
        rail::SURFACE_CONTENT_START,
        Line::from(Span::styled(timestamp, Style::new().fg(theme.tool_muted))),
    ));
    out.push(rail::surface_row(
        width,
        colors,
        rail::SURFACE_CONTENT_START,
        Line::default(),
    ));
    out
}

fn display_timestamp(timestamp: Option<&str>, pending: bool) -> String {
    timestamp.map(format_user_timestamp).unwrap_or_else(|| {
        if pending {
            "time pending".to_owned()
        } else {
            "time unavailable".to_owned()
        }
    })
}

/// Formats an Agent RFC3339 acceptance timestamp in Rail's compact local-time
/// display form. The offset is resolved for the timestamp's instant, so DST
/// transitions and non-UTC Agent values are not treated as wall-clock text.
fn format_user_timestamp(timestamp: &str) -> String {
    let Ok(parsed) = OffsetDateTime::parse(timestamp, &Rfc3339) else {
        return "time unavailable".to_owned();
    };
    let Ok(offset) = UtcOffset::local_offset_at(parsed) else {
        return "time unavailable".to_owned();
    };
    format_user_timestamp_at_offset(parsed, offset)
}

fn format_user_timestamp_at_offset(parsed: OffsetDateTime, offset: UtcOffset) -> String {
    let local = parsed.to_offset(offset);
    let date = local.date();
    let hour = local.hour();
    let minute = local.minute();
    let hour12 = match hour % 12 {
        0 => 12,
        value => value,
    };
    let meridiem = if hour < 12 { "AM" } else { "PM" };
    format!(
        "{hour12}:{minute:02} {meridiem} · {}/{}/{}",
        date.month() as u8,
        date.day(),
        date.year()
    )
}

#[cfg(test)]
mod tests {
    use super::{format_user_timestamp, format_user_timestamp_at_offset};
    use time::format_description::well_known::Rfc3339;
    use time::{OffsetDateTime, UtcOffset};

    #[test]
    fn xml_prompt_card_retains_the_submitted_instructions() {
        let block = crate::state::transcript::UserBlock {
            index: None,
            loop_id: None,
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "<instructions>\nReview the release notes.\n</instructions>".into(),
            pending: true,
        };
        for theme in [crate::theme::Theme::dark(), crate::theme::Theme::light()] {
            for width in [20, 60, 80] {
                let rendered =
                    super::lines_with_timestamp_metadata(&theme, &block, width, None, true);
                let text: String = rendered
                    .lines
                    .iter()
                    .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                    .collect();
                assert!(text.contains("<instructions>"));
                assert!(text.contains("</instructions>"));
                assert!(text.contains("Review the"));
                assert!(rendered.copy_cells.iter().any(Option::is_none));
                assert_eq!(rendered.hard_breaks.len(), rendered.lines.len());
            }
        }
    }

    #[test]
    fn timestamp_matches_rail_display_format() {
        assert_eq!(
            format_user_timestamp_at_offset(
                OffsetDateTime::parse("2026-09-05T14:05:06.007Z", &Rfc3339).unwrap(),
                UtcOffset::UTC,
            ),
            "2:05 PM · 9/5/2026"
        );
        assert_eq!(
            format_user_timestamp_at_offset(
                OffsetDateTime::parse("2026-09-05T00:05:06Z", &Rfc3339).unwrap(),
                UtcOffset::UTC,
            ),
            "12:05 AM · 9/5/2026"
        );
        assert_eq!(
            format_user_timestamp_at_offset(
                OffsetDateTime::parse("2026-09-05T14:05:06Z", &Rfc3339).unwrap(),
                UtcOffset::from_hms(8, 0, 0).unwrap(),
            ),
            "10:05 PM · 9/5/2026"
        );
        assert_eq!(
            format_user_timestamp("2026-09-05T12:05:06"),
            "time unavailable"
        );
    }

    #[test]
    fn invalid_timestamp_is_not_replaced_with_current_time() {
        assert_eq!(format_user_timestamp("not-a-timestamp"), "time unavailable");
    }

    #[test]
    fn timestamp_fixture_matches_the_pinned_utc_source_rows() {
        let source: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/rail/user/timestamp-format.json"
        ))
        .expect("timestamp fixture");
        for row in source["rows"].as_array().expect("timestamp rows") {
            let row = row.as_str().expect("timestamp fixture row");
            let (timestamp, expected) = row.split_once(" -> ").expect("timestamp fixture shape");
            let parsed = OffsetDateTime::parse(timestamp, &Rfc3339).expect("valid fixture time");
            assert_eq!(
                format_user_timestamp_at_offset(parsed, UtcOffset::UTC),
                expected
            );
        }
    }
}
