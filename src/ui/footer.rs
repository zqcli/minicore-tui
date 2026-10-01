//! The one-row Rail footer. Formatting is pure: it consumes the session
//! snapshot and optional `session.presentation` data already held by `App`.
//! It never reads the workspace, Store, or network.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ConnectionState};
use crate::markdown::column_width;
use crate::protocol::Reasoning;
use crate::state::selection::reasoning_label;
use crate::state::session::{SessionView, UsageCompleteness, UsageProjection};
use crate::state::turn::PendingSteerState;
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::rail;

const CWD_MAX_WIDTH: usize = 20;
const BRANCH_MAX_WIDTH: usize = 16;
const MODEL_MAX_WIDTH: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterView {
    pub left: String,
    pub right: String,
    pub status: FooterStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterStatus {
    Ready,
    Working,
    Blocked,
    Starting,
    ShuttingDown,
    Disconnected,
}

#[derive(Clone, Debug)]
struct FooterPart {
    text: String,
    color: Option<Color>,
}

#[derive(Clone, Debug)]
struct FooterParts {
    left: Vec<FooterPart>,
    right: Vec<FooterPart>,
    status: FooterStatus,
}

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let parts = footer_parts(app, theme);
    let fitted = fit_aligned_parts(&parts.left, &parts.right, area.width as usize, theme);
    let line = Line::from(
        fitted
            .into_iter()
            .map(|part| match part.color {
                Some(color) => Span::styled(part.text, Style::new().fg(color)),
                None => Span::raw(part.text),
            })
            .collect::<Vec<_>>(),
    );
    frame.render_widget(Paragraph::new(line), area);
}

pub fn footer_view(app: &App) -> FooterView {
    let theme = Theme::for_kind(app.theme);
    let parts = footer_parts(app, &theme);
    FooterView {
        left: parts_text(&parts.left),
        right: parts_text(&parts.right),
        status: parts.status,
    }
}

fn footer_parts(app: &App, theme: &Theme) -> FooterParts {
    let Some(view) = app.active_view() else {
        let status = match app.connection {
            ConnectionState::Starting => FooterStatus::Starting,
            ConnectionState::ShuttingDown => FooterStatus::ShuttingDown,
            ConnectionState::Failed(_) => FooterStatus::Disconnected,
            ConnectionState::Ready if app.startup_create_pending() => FooterStatus::Starting,
            ConnectionState::Ready => FooterStatus::Ready,
        };
        return FooterParts {
            left: vec![FooterPart {
                text: status_label(status, false).to_owned(),
                color: Some(footer_status_color(status, theme)),
            }],
            right: Vec::new(),
            status,
        };
    };

    let status = if view.is_blocked() || view.unsaved_loop.is_some() {
        FooterStatus::Blocked
    } else if layout::busy(app) {
        FooterStatus::Working
    } else {
        FooterStatus::Ready
    };
    let workspace = workspace_basename(&view.info.workspace);
    // A confirmed non-repository retains the plain workspace identity. An
    // unknown/failed observation has an explicit git? / stale marker instead.
    let observed = &view.workspace_status;
    let branch = if !observed.stale
        && !observed.error
        && observed
            .value
            .as_ref()
            .is_some_and(|v| v.complete && !v.repo_available)
    {
        None
    } else {
        Some(observed.label())
    };
    let model = current_model(view);
    let model = short_model(model);
    let reasoning = current_reasoning(view);
    let mut left = Vec::new();
    push_identity_parts(&mut left, &workspace, branch.as_deref(), theme);
    push_separator(&mut left, theme);
    push_part(&mut left, model, theme.footer_sky);
    push_separator(&mut left, theme);
    push_part(
        &mut left,
        reasoning_label(reasoning).to_owned(),
        theme.footer_amber,
    );
    push_separator(&mut left, theme);
    push_part(
        &mut left,
        status_label(status, view.event_gap).to_owned(),
        footer_status_color(status, theme),
    );
    if let Some(duration) = loop_duration(view) {
        push_separator(&mut left, theme);
        push_part(&mut left, duration, theme.footer_amber);
    }
    // Footer `queued`: locally unsent + in-flight (accepted) entries only;
    // receipt-proven applied steers are not queued.
    let pending_inflight = view
        .live
        .as_ref()
        .map(|live| {
            live.pending_steers
                .iter()
                .filter(|steer| {
                    matches!(
                        steer.state,
                        PendingSteerState::Sending
                            | PendingSteerState::Queued
                            | PendingSteerState::Unconfirmed
                    )
                })
                .count()
        })
        .unwrap_or(0);
    if view.steer_queue.len() + pending_inflight > 0 {
        push_separator(&mut left, theme);
        push_part(
            &mut left,
            format!("queued {}", view.steer_queue.len() + pending_inflight),
            theme.footer_amber,
        );
    }
    if app.selection_copied() {
        push_separator(&mut left, theme);
        push_part(&mut left, "selection copied".to_owned(), theme.footer_mint);
    }

    FooterParts {
        left,
        right: right_usage_parts(view, theme),
        status,
    }
}

fn push_part(parts: &mut Vec<FooterPart>, text: String, color: Color) {
    // Footer text mixes workspace paths, branch names, model ids, and usage
    // metadata. All of it crosses the same safe-display boundary as the rest
    // of the UI (spec §19).
    let text = crate::safe_text::safe_display(&text)
        .replace('\n', "\\n")
        .replace('\t', "    ");
    if !text.is_empty() {
        parts.push(FooterPart {
            text,
            color: Some(color),
        });
    }
}

fn push_separator(parts: &mut Vec<FooterPart>, theme: &Theme) {
    parts.push(FooterPart {
        text: " · ".to_owned(),
        color: Some(theme.footer_muted),
    });
}

fn push_identity_parts(
    parts: &mut Vec<FooterPart>,
    workspace: &str,
    branch: Option<&str>,
    theme: &Theme,
) {
    let identity = format!("▸ {}", fit_width(workspace, CWD_MAX_WIDTH));
    push_part(parts, identity, theme.footer_text);
    if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
        push_part(
            parts,
            format!("@{}", fit_width(branch, BRANCH_MAX_WIDTH)),
            theme.footer_mint,
        );
    }
}

fn footer_status_color(status: FooterStatus, theme: &Theme) -> Color {
    match status {
        FooterStatus::Working => theme.footer_amber,
        FooterStatus::Blocked | FooterStatus::Disconnected => theme.error,
        FooterStatus::Starting | FooterStatus::ShuttingDown => theme.footer_muted,
        FooterStatus::Ready => theme.footer_mint,
    }
}

fn current_reasoning(view: &SessionView) -> Reasoning {
    // The durable session setting is the Agent-acknowledged truth: a
    // session.update ack updates `view.info`, so the footer reflects the
    // selected reasoning immediately (idle and live) without a new turn.
    // Per-request metadata stays immutable and is not shown here.
    view.info.reasoning
}

fn current_model(view: &SessionView) -> &str {
    // Also the acknowledged session model; a stale presentation label or past
    // request must never override the current model identity.
    &view.info.model
}

fn loop_duration(view: &SessionView) -> Option<String> {
    let last_loop = view.presentation.as_ref()?.last_loop.as_ref()?;
    let started = crate::state::selection::parse_rfc3339(last_loop.started_at.as_deref()?)?;
    let finished = crate::state::selection::parse_rfc3339(last_loop.finished_at.as_deref()?)?;
    let minutes = finished.duration_since(started).ok()?.as_secs() / 60;
    Some(format_duration(minutes))
}

pub fn format_duration(minutes: u64) -> String {
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h{}m", minutes / 60, minutes % 60)
}

fn status_label(status: FooterStatus, event_gap: bool) -> &'static str {
    match status {
        FooterStatus::Ready if event_gap => "⚠ incomplete",
        FooterStatus::Working if event_gap => "⚠ incomplete",
        FooterStatus::Ready => "● ready",
        FooterStatus::Working => "● working",
        FooterStatus::Blocked => "● blocked",
        FooterStatus::Starting => "starting",
        FooterStatus::ShuttingDown => "shutting down",
        FooterStatus::Disconnected => "disconnected",
    }
}

fn right_usage_parts(view: &SessionView, theme: &Theme) -> Vec<FooterPart> {
    let projection = &view.usage_projection;
    let usage = (!matches!(projection.completeness, UsageCompleteness::Unknown))
        .then_some(projection.usage);
    let mut parts = Vec::new();
    if let Some(usage) = usage {
        let input_output = [
            usage
                .input_tokens
                .map(|value| format!("↑{}", format_num(value))),
            usage
                .output_tokens
                .map(|value| format!("↓{}", format_num(value))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let cache = [
            usage
                .cache_read_tokens
                .map(|value| format!("R{}", format_num(value))),
            usage
                .cache_write_tokens
                .map(|value| format!("W{}", format_num(value))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if !input_output.is_empty() {
            let mut text = input_output.join(" ");
            if !cache.is_empty() {
                text.push(' ');
            }
            push_part(&mut parts, text, theme.footer_sky);
        }
        if !cache.is_empty() {
            push_part(&mut parts, cache.join(" "), theme.footer_lilac);
        }
        if projection.completeness == UsageCompleteness::Partial {
            if !parts.is_empty() {
                push_separator(&mut parts, theme);
            }
            push_part(&mut parts, "usage ?".to_owned(), theme.footer_amber);
        }
    } else if projection.completeness == UsageCompleteness::Partial {
        push_part(&mut parts, "usage ?".to_owned(), theme.footer_amber);
    }
    append_unsaved_usage(&mut parts, projection, theme);
    if let Some(presentation) = &view.presentation {
        let context = match (presentation.context.kind, presentation.context.percent) {
            (crate::protocol::ContextKindWire::Estimated, Some(percent)) => {
                format!("ctx ~{percent:.2}%")
            }
            (_, Some(percent)) => format!("ctx {percent:.2}%"),
            _ => "ctx ?".to_owned(),
        };
        if !parts.is_empty() {
            push_separator(&mut parts, theme);
        }
        let context_color = presentation
            .context
            .percent
            .filter(|percent| *percent >= 70.0)
            .map_or(theme.footer_lilac, |_| theme.footer_amber);
        push_part(&mut parts, context, context_color);
        if let Some(cost) = presentation.cost_usd.filter(|cost| {
            cost.is_finite() && (*cost > 0.0 || presentation.using_subscription == Some(true))
        }) {
            let mut cost = format_cost(cost);
            if presentation.using_subscription == Some(true) {
                cost.push_str(" (sub)");
            }
            push_separator(&mut parts, theme);
            push_part(&mut parts, cost, theme.footer_mint);
        }
    } else {
        if !parts.is_empty() {
            push_separator(&mut parts, theme);
        }
        push_part(&mut parts, "ctx ?".to_owned(), theme.footer_lilac);
    }
    parts
}

fn append_unsaved_usage(parts: &mut Vec<FooterPart>, projection: &UsageProjection, theme: &Theme) {
    let Some(usage) = projection.unsaved_usage else {
        if projection.unsaved_completeness != UsageCompleteness::Unknown {
            push_separator(parts, theme);
            push_part(parts, "unsaved usage ?".to_owned(), theme.error);
        } else {
            return;
        }
        return;
    };
    let values = [
        usage
            .input_tokens
            .map(|value| format!("↑{}", format_num(value))),
        usage
            .output_tokens
            .map(|value| format!("↓{}", format_num(value))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if values.is_empty() {
        return;
    }
    push_separator(parts, theme);
    push_part(parts, format!("unsaved {}", values.join(" ")), theme.error);
    if projection.unsaved_completeness == UsageCompleteness::Partial {
        push_part(parts, "?".to_owned(), theme.error);
    }
}

pub fn format_num(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value < 1_000_000 {
        return format_one_decimal(value as f64 / 1_000.0, "k");
    }
    format_one_decimal(value as f64 / 1_000_000.0, "m")
}

fn format_one_decimal(value: f64, suffix: &str) -> String {
    format!("{value:.1}{suffix}")
}

pub fn format_cost(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        "$0".to_owned()
    } else if value < 0.01 {
        format!("${value:.4}")
    } else if value < 1.0 {
        format!("${value:.3}")
    } else {
        format!("${value:.2}")
    }
}

pub fn fit_aligned(left: &str, right: &str, width: usize) -> (String, String) {
    if width == 0 {
        return (String::new(), String::new());
    }
    let right = fit_width(right, width);
    let right_width = column_width(&right);
    if right_width >= width {
        return (String::new(), right);
    }
    let left_width = width.saturating_sub(right_width + 1);
    (fit_width(left, left_width), right)
}

fn parts_text(parts: &[FooterPart]) -> String {
    parts.iter().map(|part| part.text.as_str()).collect()
}

fn fit_parts(parts: &[FooterPart], width: usize, _theme: &Theme) -> Vec<FooterPart> {
    if width == 0 {
        return Vec::new();
    }
    let text = parts_text(parts);
    if column_width(&text) <= width {
        return parts.to_vec();
    }
    if width == 1 {
        return vec![FooterPart {
            text: "…".to_owned(),
            color: None,
        }];
    }
    let limit = width - 1;
    let mut used: usize = 0;
    let mut fitted = Vec::new();
    for part in parts {
        let mut text = String::new();
        for character in part.text.chars() {
            let cells = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
            if used.saturating_add(cells) > limit {
                break;
            }
            text.push(character);
            used = used.saturating_add(cells);
        }
        if !text.is_empty() {
            fitted.push(FooterPart {
                text,
                color: part.color,
            });
        }
        if used >= limit {
            break;
        }
    }
    fitted.push(FooterPart {
        text: "…".to_owned(),
        color: None,
    });
    fitted
}

fn fit_aligned_parts(
    left: &[FooterPart],
    right: &[FooterPart],
    width: usize,
    theme: &Theme,
) -> Vec<FooterPart> {
    if width == 0 {
        return Vec::new();
    }
    if right.is_empty() {
        return fit_parts(left, width, theme);
    }
    let right = fit_parts(right, width, theme);
    let right_width = column_width(&parts_text(&right));
    if right_width >= width {
        return right;
    }
    let left = fit_parts(left, width.saturating_sub(right_width + 1), theme);
    let left_width = column_width(&parts_text(&left));
    let gap = width.saturating_sub(left_width + right_width).max(1);
    let gap_color = left.last().and_then(|part| part.color);
    let mut fitted = left;
    fitted.push(FooterPart {
        text: " ".repeat(gap),
        color: gap_color,
    });
    fitted.extend(right);
    fitted
}

fn short_model(model: &str) -> String {
    let mut value = model
        .replace("claude-", "claude ")
        .replace("gemini-", "gemini ")
        .replace("gpt-", "gpt ");
    if value.len() >= 9 {
        let suffix_start = value.len() - 9;
        if value.as_bytes().get(suffix_start) == Some(&b'-')
            && value.as_bytes()[suffix_start + 1..]
                .iter()
                .all(u8::is_ascii_digit)
            && value.as_bytes()[suffix_start + 1] == b'2'
            && value.as_bytes()[suffix_start + 2] == b'0'
        {
            value.truncate(suffix_start);
        }
    }
    for suffix in ["-latest", "-preview"] {
        if let Some(stripped) = value.strip_suffix(suffix) {
            value = stripped.to_owned();
        }
    }
    value = value.replace('-', " ");
    value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    fit_width(&value, MODEL_MAX_WIDTH)
}

fn fit_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if column_width(text) <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }
    format!("{}…", rail::clip_cells(text, width - 1))
}

fn workspace_basename(workspace: &str) -> String {
    Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace)
        .to_owned()
}

/// The user's home directory for workspace shortening; `None` keeps paths
/// unshortened (no canonicalization anywhere, spec 31.1).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// `/home/user/project` under home `/home/user` becomes `~/project`. The
/// prefix must end at a path component; no canonicalization is performed.
pub fn shorten_workspace(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.to_string_lossy().into_owned();
    };
    let path_text = path.to_string_lossy();
    let home_text = home.to_string_lossy();
    if path == home {
        return "~".to_owned();
    }
    if let Some(rest) = path_text.strip_prefix(home_text.as_ref()) {
        if rest.is_empty() {
            return "~".to_owned();
        }
        if rest.starts_with('/') || rest.starts_with('\\') {
            return format!("~{rest}");
        }
    }
    path_text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_cost_and_right_group_follow_rail_boundaries() {
        assert_eq!(format_num(999), "999");
        assert_eq!(format_num(1_000), "1.0k");
        assert_eq!(format_num(1_000_000), "1.0m");
        assert_eq!(format_cost(0.0012), "$0.0012");
        assert_eq!(format_cost(0.12), "$0.120");
        assert_eq!(format_cost(2.0), "$2.00");
        assert_eq!(format_cost(f64::NAN), "$0");
        assert_eq!(
            fit_aligned("abcdef", "ctx ?", 10),
            ("abc…".to_owned(), "ctx ?".to_owned())
        );
    }

    #[test]
    fn model_shortening_preserves_unknown_model_identity_without_raw_prefixes() {
        assert_eq!(short_model("claude-3-7-sonnet-latest"), "claude 3 7 sonnet");
        assert_eq!(short_model("gpt-5.6-sol"), "gpt 5.6 sol");
    }

    #[test]
    fn fit_aligned_matches_reference_narrow_footer_text() {
        let left = "▸ project · gpt 4o · high · ● ready · 1h1m";
        let right = "↑4.2k ↓67.1k R7.9m W0 · ctx ? · $0.020";
        let render = |width| {
            let (left, right) = fit_aligned(left, right, width);
            if right.is_empty() {
                left
            } else {
                format!("{left} {right}")
            }
        };
        assert_eq!(render(40), "… ↑4.2k ↓67.1k R7.9m W0 · ctx ? · $0.020");
        assert_eq!(
            render(60),
            "▸ project · gpt 4o ·… ↑4.2k ↓67.1k R7.9m W0 · ctx ? · $0.020"
        );
    }

    #[test]
    fn duration_format_matches_rail_boundaries() {
        assert_eq!(format_duration(0), "0m");
        assert_eq!(format_duration(59), "59m");
        assert_eq!(format_duration(61), "1h1m");
    }

    #[test]
    fn ready_footer_styled_cells_match_the_source_fixture() {
        let mut app = crate::ui::testapp::open_empty(
            crate::theme::ThemeKind::Dark,
            "ses_1",
            Some("Task"),
            "high",
        );
        let view = app.sessions.known.get_mut("ses_1").expect("active view");
        view.info.model = "gpt-4o".to_owned();
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: crate::protocol::TurnRef {
                session_id: "ses_1".to_owned(),
                loop_id: "loop_1".to_owned(),
            },
            outcome: crate::protocol::LoopOutcomeWire::Completed,
            usage: Some(crate::protocol::UsageWire {
                input_tokens: Some(4_200),
                output_tokens: Some(67_100),
                reasoning_tokens: None,
                cache_read_tokens: Some(7_900_000),
                cache_write_tokens: Some(0),
                provider_total_tokens: None,
            }),
            requests: Some(1),
            tool_rounds: Some(0),
            final_config_revision: Some(0),
            persistence: Some(crate::protocol::TurnPersistenceWire::Persisted),
            accepted_at: None,
            completed_at: None,
        });
        view.recompute_usage_projection();
        view.presentation = Some(crate::protocol::SessionPresentationWire {
            session_id: "ses_1".to_owned(),
            model_label: None,
            git_branch: None,
            context: crate::protocol::ContextUsageWire {
                tokens: None,
                window: None,
                percent: None,
                kind: crate::protocol::ContextKindWire::Unknown,
            },
            cost_usd: Some(0.02),
            using_subscription: Some(false),
            last_loop: None,
            steer_progress: None,
        });

        let theme = Theme::dark();
        let parts = footer_parts(&app, &theme);
        let actual = fit_aligned_parts(&parts.left, &parts.right, 80, &theme)
            .into_iter()
            .flat_map(|part| {
                let color = part.color;
                part.text
                    .chars()
                    .map(move |character| (character, color))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let source: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/rail/footer/ready.json"))
                .expect("ready footer fixture");
        let expected = source["rows"][0]
            .as_array()
            .expect("ready footer row")
            .iter()
            .flat_map(|token| {
                let text = token
                    .as_str()
                    .or_else(|| token.get("c").and_then(serde_json::Value::as_str))
                    .expect("footer token text");
                let color = token.get("fg").map(|rgb| {
                    let rgb = rgb.as_array().expect("footer rgb");
                    Color::Rgb(
                        rgb[0].as_u64().unwrap() as u8,
                        rgb[1].as_u64().unwrap() as u8,
                        rgb[2].as_u64().unwrap() as u8,
                    )
                });
                text.chars()
                    .map(move |character| (character, color))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn partial_usage_annotation_gets_a_separator_not_a_jammed_suffix() {
        let mut app = crate::ui::testapp::open_empty(
            crate::theme::ThemeKind::Dark,
            "ses_1",
            Some("Task"),
            "high",
        );
        let view = app.sessions.known.get_mut("ses_1").expect("active view");
        // One known metric with another missing: a partially-known total.
        view.transcript
            .push_block(crate::state::transcript::TranscriptBlock::Assistant(
                crate::state::transcript::AssistantBlock {
                    index: 0,
                    loop_id: "loop_1".to_owned(),
                    request_index: 0,
                    model: "deep".to_owned(),
                    reasoning_level: crate::protocol::Reasoning::High,
                    parts: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: crate::protocol::UsageWire {
                        input_tokens: Some(4_200),
                        output_tokens: None,
                        cache_read_tokens: Some(0),
                        cache_write_tokens: Some(0),
                        ..crate::protocol::UsageWire::default()
                    },
                    finish_reason: "stop".to_owned(),
                    terminal_error: None,
                },
            ));
        view.transcript.complete = true;
        view.recompute_usage_projection();
        view.presentation = Some(crate::protocol::SessionPresentationWire {
            session_id: "ses_1".to_owned(),
            model_label: None,
            git_branch: None,
            context: crate::protocol::ContextUsageWire {
                tokens: None,
                window: None,
                percent: None,
                kind: crate::protocol::ContextKindWire::Unknown,
            },
            cost_usd: None,
            using_subscription: None,
            last_loop: None,
            steer_progress: None,
        });

        let theme = Theme::dark();
        let parts = footer_parts(&app, &theme);
        let right = parts_text(&parts.right);
        assert_eq!(
            right, "↑4.2k R0 W0 · usage ? · ctx ?",
            "the partial annotation must be separated from the numbers and context"
        );
    }
}
