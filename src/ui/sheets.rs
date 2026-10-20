//! Builders for the selectors and the inline completion list: the sessions,
//! model and reasoning pickers, the approval prompt and the find bar each turn
//! app state into one [`Sheet`]; the command and `@` file lists are plain rows.

use ratatui::text::{Line, Span};

use crate::app::commands;
use crate::app::overlay::{self, matching_models};
use crate::app::session::{InteractionKind, auto_title};
use crate::app::{App, Overlay, Session};
use crate::protocol::{ModelInfo, Reasoning, SessionInfo};

use super::panel::{Kind, Row, Sheet, Tone, body_lines, truncate};
use super::picker::{self, fuzzy_match};
use super::theme::Styles;
use super::wrap::width;

/// Rows of the completion list that show at once.
const COMPLETION_ROWS: usize = 5;

/// The sheet for the current overlay; a pending interaction is one too, as it owns the keyboard.
pub fn sheet(app: &App, styles: Styles) -> Option<Sheet> {
    Some(match &app.overlay {
        Overlay::None => return interaction_sheet(app, styles),
        Overlay::Sessions {
            filter,
            selected,
            rows,
            armed,
        } => sessions_sheet(app, rows, filter, *selected, armed.as_deref(), styles),
        Overlay::Models { filter, selected } => models_sheet(app, filter, *selected, styles),
        Overlay::Reasoning { selected } => reasoning_sheet(app, *selected),
        Overlay::Find {
            query,
            matches,
            current,
        } => find_sheet(query, matches.len(), *current),
        // The completion lists live in the editor band and views own the transcript area.
        Overlay::Palette { .. }
        | Overlay::Files { .. }
        | Overlay::Context
        | Overlay::Help { .. }
        | Overlay::Diff(_)
        | Overlay::Tool(_) => return None,
    })
}

/// Rows of the inline completion list (commands, `@` files) shown inside the
/// composer band, as content only: the band adds the gutter, rail and tint.
pub fn completion(app: &App, styles: Styles, width: u16) -> Option<Vec<Line<'static>>> {
    let (items, selected) = match &app.overlay {
        Overlay::Palette { selected } => {
            let items = commands::filter(app.palette_query()?)
                .into_iter()
                .map(|command| {
                    let detail = if command.args.is_empty() {
                        command.description.to_owned()
                    } else {
                        format!("{} — {}", command.args, command.description)
                    };
                    (command.name.to_owned(), detail)
                })
                .collect::<Vec<_>>();
            (items, *selected)
        }
        Overlay::Files { selected, paths } => {
            let items = overlay::matching_paths(paths, app.mention_query()?)
                .into_iter()
                .map(|path| (overlay::file_name(path).to_owned(), path.clone()))
                .collect::<Vec<_>>();
            (items, *selected)
        }
        _ => return None,
    };
    (!items.is_empty()).then(|| completion_lines(&items, selected, styles, usize::from(width)))
}

/// The list of `(name, description)` items around `selected`, then a counter
/// row when some are out of view.
fn completion_lines(
    items: &[(String, String)],
    selected: usize,
    styles: Styles,
    cells: usize,
) -> Vec<Line<'static>> {
    let widest = items.iter().map(|(name, _)| width(name)).max().unwrap_or(0);
    let column = (widest + 2).clamp(12, 32);
    let window = picker::window(selected, items.len(), COMPLETION_ROWS);
    let mut lines: Vec<Line<'static>> = window
        .map(|index| {
            let (name, detail) = &items[index];
            let chosen = index == selected;
            let pointer = if chosen { "→ " } else { "  " };
            let name = truncate(name, column - 2);
            let gap = column.saturating_sub(width(&name));
            let room = cells.saturating_sub(2 + column);
            let (name_style, detail_style) = if chosen {
                (styles.user_rail(), styles.user_rail())
            } else {
                (styles.text(), styles.dim())
            };
            let mut spans = vec![Span::styled(format!("{pointer}{name}"), name_style)];
            if cells > 40 && room > 10 {
                spans.push(Span::styled(" ".repeat(gap), name_style));
                spans.push(Span::styled(truncate(detail, room), detail_style));
            }
            Line::from(spans)
        })
        .collect();
    if items.len() > COMPLETION_ROWS {
        let counter = format!("  ({}/{})", selected + 1, items.len());
        lines.push(Line::styled(truncate(&counter, cells), styles.dim()));
    }
    lines
}

fn sessions_sheet(
    app: &App,
    rows: &[SessionInfo],
    filter: &str,
    selected: usize,
    armed: Option<&str>,
    styles: Styles,
) -> Sheet {
    let resident = app
        .session
        .as_ref()
        .map(|session| session.info.session_id.as_str());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let shown = filtered_sessions(rows, filter, app.session.as_ref());
    let status = if shown.len() == rows.len() {
        count(rows.len(), "session")
    } else {
        format!("{} of {}", shown.len(), rows.len())
    };
    let list = shown
        .iter()
        .map(|info| {
            let label = session_label(info, app.session.as_ref());
            let current = resident == Some(info.session_id.as_str());
            let row = session_row(info, label, current, now);
            if armed == Some(info.session_id.as_str()) {
                row.tone(Tone::Failure)
            } else {
                row
            }
        })
        .collect::<Vec<_>>();
    let hint = if armed.is_some() {
        "ctrl+d delete again · esc cancel"
    } else {
        "enter open · ctrl+d delete · esc close"
    };
    let notes = if list.is_empty() {
        vec![Line::styled("  No sessions found", styles.dim())]
    } else {
        Vec::new()
    };
    Sheet::new("Resume Session", hint)
        .status(status)
        .search(filter)
        .kind(Kind::Session)
        .rows(list)
        .selected(Some(selected))
        .notes(notes)
}

fn models_sheet(app: &App, filter: &str, selected: usize, styles: Styles) -> Sheet {
    let current = app.session.as_ref().map(|session| &session.info.model);
    let shown = matching_models(&app.models, filter);
    let rows = shown
        .iter()
        .map(|model| {
            let provider = provider(model).map(|name| format!("[{name}]"));
            Row::new(model.id.clone(), provider.unwrap_or_default())
                .mark(Some(&model.id) == current)
        })
        .collect::<Vec<_>>();
    let notes = match shown.get(selected) {
        Some(model) => vec![
            Line::default(),
            Line::styled(
                format!("  Model Name: {}", model_details(model)),
                styles.dim(),
            ),
        ],
        None => vec![Line::styled("  No matching models", styles.dim())],
    };
    Sheet::new("Select Model", "enter select · esc cancel")
        .status(count(app.models.len(), "model"))
        .search(filter)
        .rows(rows)
        .selected(Some(selected))
        .notes(notes)
}

/// `1 session`, `3 sessions`.
fn count(total: usize, noun: &str) -> String {
    let plural = if total == 1 { "" } else { "s" };
    format!("{total} {noun}{plural}")
}

/// The provider part of a `provider/model` reference.
fn provider(model: &ModelInfo) -> Option<&str> {
    model
        .model_ref
        .as_deref()?
        .split_once('/')
        .map(|(name, _)| name)
}

/// The full reference and the context window, as the model note shows them.
fn model_details(model: &ModelInfo) -> String {
    let name = model.model_ref.as_deref().unwrap_or(&model.id);
    match model.context_window {
        Some(window) => format!("{name} · {} context", super::format_tokens(window)),
        None => name.to_owned(),
    }
}

fn reasoning_sheet(app: &App, selected: usize) -> Sheet {
    let current = app.session.as_ref().map(|session| session.info.reasoning);
    let rows = app
        .reasoning_levels()
        .iter()
        .map(|level| {
            Row::new(level.label(), reasoning_description(*level)).mark(Some(*level) == current)
        })
        .collect();
    Sheet::new(
        "Reasoning Level",
        "enter select · shift+tab cycles · esc cancel",
    )
    .kind(Kind::Column)
    .rows(rows)
    .selected(Some(selected))
}

fn reasoning_description(level: Reasoning) -> &'static str {
    match level {
        Reasoning::Auto => "Model default",
        Reasoning::Disabled => "No reasoning",
        Reasoning::Low => "Light reasoning",
        Reasoning::Medium => "Moderate reasoning",
        Reasoning::High => "Deep reasoning",
        Reasoning::XHigh => "Extra-high reasoning",
        Reasoning::Max => "Maximum reasoning",
        Reasoning::Ultra => "Beyond maximum reasoning",
        Reasoning::Unknown => "",
    }
}

fn interaction_sheet(app: &App, styles: Styles) -> Option<Sheet> {
    let session = app.session.as_ref()?;
    let interaction = session.interaction.as_ref()?;
    let options = interaction.options();
    // The card is read at draw time: its detail and input can arrive after
    // the prompt does.
    let call = session
        .transcript
        .find_tool_by_call_id(&interaction.tool_call_id);
    let subject = Subject {
        tool: call.map_or(interaction.tool_name.as_str(), |call| call.name.as_str()),
        detail: call.map_or("", |call| call.detail.as_str()),
        input: call.and_then(|call| call.expanded_input.as_deref()),
    };
    let approval = matches!(interaction.kind, InteractionKind::Approval);
    Some(approval_sheet(
        &interaction.prompt,
        &subject,
        &options,
        approval,
        interaction.selected,
        styles,
    ))
}

/// What an interaction is about: the tool, its one-line detail (a path, a
/// command) and the start of its input (the content or script it would run).
struct Subject<'a> {
    tool: &'a str,
    detail: &'a str,
    input: Option<&'a str>,
}

/// Input lines an approval shows before it says how many more there are.
const INPUT_PREVIEW: usize = 6;

/// The sheet for a pending interaction: the question, what would run, then
/// the approval's two rows or the choice list.
fn approval_sheet(
    prompt: &str,
    subject: &Subject<'_>,
    options: &[String],
    approval: bool,
    selected: usize,
    styles: Styles,
) -> Sheet {
    let mut body = body_lines(prompt, 72, styles.text());
    if !subject.tool.is_empty() {
        body.push(Line::from(vec![
            Span::styled(format!("{}  ", subject.tool), styles.title()),
            Span::styled(
                truncate(subject.detail.lines().next().unwrap_or(""), 100),
                styles.text(),
            ),
        ]));
    }
    if let Some(input) = subject.input.filter(|input| !input.trim().is_empty()) {
        let style = styles.dim();
        let lines: Vec<&str> = input.lines().collect();
        for line in lines.iter().take(INPUT_PREVIEW) {
            body.push(Line::styled(format!("  {}", truncate(line, 100)), style));
        }
        if lines.len() > INPUT_PREVIEW {
            body.push(Line::styled(
                format!("  … {} more lines", lines.len() - INPUT_PREVIEW),
                styles.faint(),
            ));
        }
    }
    let rows = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            if !approval {
                return Row::new(option.clone(), "");
            }
            match index {
                0 => Row::new(option.clone(), "y").tone(Tone::Success),
                _ => Row::new(option.clone(), "n").tone(Tone::Failure),
            }
        })
        .collect();
    let (title, hint) = if approval {
        ("Approval", "enter confirm · y allow · n deny · esc deny")
    } else {
        ("Question", "enter choose · esc deny")
    };
    Sheet::new(title, hint)
        .kind(Kind::Column)
        .body(body)
        .rows(rows)
        .selected(Some(selected))
}

/// The find bar: the query in the title, the match position on the right.
fn find_sheet(query: &str, total: usize, current: usize) -> Sheet {
    let title = if query.is_empty() {
        "Find".to_owned()
    } else {
        format!("Find: {query}")
    };
    let status = match total {
        0 => "no matches".to_owned(),
        _ => format!("{} of {total}", current + 1),
    };
    Sheet::new(title, "enter next · shift+enter previous · esc close").status(status)
}

/// A session's name in the picker: its title, else (for the resident session)
/// its first prompt, else its short id.
pub fn session_label(info: &SessionInfo, resident: Option<&Session>) -> String {
    let title = info
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty());
    title
        .map(str::to_owned)
        .or_else(|| {
            let resident = resident.filter(|session| session.info.session_id == info.session_id)?;
            auto_title(resident.first_prompt()?)
        })
        .unwrap_or_else(|| short_session_id(&info.session_id).to_owned())
}

/// `ses_8ca3c4cf...` → `ses_8ca3c4`, enough to tell sessions apart in a row.
fn short_session_id(session_id: &str) -> &str {
    let body = session_id.strip_prefix("ses_").unwrap_or(session_id);
    let cut = body
        .char_indices()
        .nth(6)
        .map_or(body.len(), |(index, _)| index);
    &session_id[..session_id.len() - body.len() + cut]
}

/// The picker rows whose label fuzzy-matches `filter`, in order.
pub fn filtered_sessions<'a>(
    rows: &'a [SessionInfo],
    filter: &str,
    resident: Option<&Session>,
) -> Vec<&'a SessionInfo> {
    let filter = filter.trim();
    rows.iter()
        .filter(|info| filter.is_empty() || fuzzy_match(&session_label(info, resident), filter))
        .collect()
}

/// One sessions-picker row: `label` (the title, or the first prompt), marked
/// when it is the resident session, with model and age on the right.
fn session_row(info: &SessionInfo, label: String, resident: bool, now_seconds: u64) -> Row {
    let age = age_label(&info.updated_at, now_seconds);
    let detail = if age.is_empty() {
        info.model.clone()
    } else {
        format!("{} {age}", info.model)
    };
    Row::new(label, detail).mark(resident)
}

/// `now`, `5m`, `3h`, `2d`, `1w`, `4mo` or `2y`; empty when the timestamp is not RFC 3339.
fn age_label(updated_at: &str, now: u64) -> String {
    let Some(then) = parse_unix_seconds(updated_at) else {
        return String::new();
    };
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        86_400..=604_799 => format!("{}d", seconds / 86_400),
        604_800..=2_591_999 => format!("{}w", seconds / 604_800),
        2_592_000..=31_535_999 => format!("{}mo", seconds / 2_592_000),
        _ => format!("{}y", seconds / 31_536_000),
    }
}

/// Seconds since the Unix epoch for an RFC 3339 stamp.
fn parse_unix_seconds(stamp: &str) -> Option<u64> {
    crate::protocol::stamp::unix_millis(stamp).map(|millis| millis / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Palette;
    use serde_json::json;

    fn styles() -> Styles {
        Styles::of(Palette::dark())
    }

    fn text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn items(count: usize) -> Vec<(String, String)> {
        (0..count)
            .map(|index| (format!("cmd{index:02}"), format!("Does thing {index}")))
            .collect()
    }

    #[test]
    fn the_completion_list_marks_the_selection_and_aligns_descriptions() {
        let lines = completion_lines(&items(3), 1, styles(), 80);
        let rows: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(rows[0], format!("  cmd00{}Does thing 0", " ".repeat(7)));
        assert_eq!(rows[1], format!("→ cmd01{}Does thing 1", " ".repeat(7)));
        assert_eq!(rows.len(), 3, "no counter for a short list");
    }

    #[test]
    fn the_selected_completion_row_is_one_colour_and_the_others_are_not() {
        let palette = Palette::dark();
        let lines = completion_lines(&items(2), 0, styles(), 80);
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|span| span.style.fg == Some(palette.user_rail))
        );
        assert_eq!(lines[1].spans[0].style.fg, Some(palette.text));
        assert_eq!(lines[1].spans[2].style.fg, Some(palette.dim));
    }

    #[test]
    fn five_rows_show_around_the_selection_with_a_counter() {
        let lines = completion_lines(&items(30), 10, styles(), 80);
        let rows: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(rows.len(), 6);
        assert!(rows[0].starts_with("  cmd08"), "{:?}", rows[0]);
        assert!(rows[2].starts_with("→ cmd10"), "{:?}", rows[2]);
        assert!(rows[4].starts_with("  cmd12"), "{:?}", rows[4]);
        assert_eq!(rows[5], "  (11/30)");
        let last = completion_lines(&items(30), 29, styles(), 80);
        assert!(text(&last[4]).starts_with("→ cmd29"));
        assert_eq!(text(&last[5]), "  (30/30)");
    }

    #[test]
    fn a_narrow_list_drops_descriptions_but_keeps_names() {
        let lines = completion_lines(&items(2), 0, styles(), 30);
        assert_eq!(text(&lines[0]), "→ cmd00");
    }

    #[test]
    fn the_name_column_follows_the_widest_name_within_bounds() {
        let long = vec![(
            "a-rather-long-command-name-that-exceeds-the-cap".into(),
            "d".into(),
        )];
        let rows = completion_lines(&long, 0, styles(), 100);
        assert_eq!(width(&text(&rows[0])), 2 + 32 + 1);
    }

    fn info() -> SessionInfo {
        serde_json::from_value(json!({
            "session_id": "ses_1acdef0123456789abcdef", "title": "Fix the parser",
            "profile": "coding", "workspace": "", "model": "gpt-6-luna", "reasoning": "auto",
            "loaded": false, "updated_at": "2026-01-02T03:04:05.006Z"
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
        sheet.body.iter().map(text).collect()
    }

    #[test]
    fn the_approval_sheet_offers_allow_then_deny_with_their_keys() {
        let options = vec!["Allow once".to_owned(), "Deny".to_owned()];
        let sheet = approval_sheet(
            "Write src/main.rs?",
            &NO_SUBJECT,
            &options,
            true,
            0,
            styles(),
        );
        assert_eq!(sheet.title, "Approval");
        assert!(sheet.hint.contains("y allow · n deny"));
        assert_eq!(sheet.rows[0].label, "Allow once");
        assert_eq!(sheet.rows[0].detail, "y");
        assert_eq!(sheet.rows[0].tone, Tone::Success);
        assert_eq!(sheet.rows[1].detail, "n");
        assert_eq!(sheet.rows[1].tone, Tone::Failure);
        assert_eq!(sheet.selected, Some(0));
    }

    #[test]
    fn a_choice_prompt_has_no_allow_deny_keys() {
        let options = vec!["red".to_owned(), "blue".to_owned()];
        let sheet = approval_sheet("Pick one", &NO_SUBJECT, &options, false, 0, styles());
        assert_eq!(sheet.title, "Question");
        assert!(sheet.rows.iter().all(|row| row.detail.is_empty()));
        assert!(!sheet.hint.contains("allow"));
    }

    #[test]
    fn the_approval_sheet_wraps_the_prompt_and_shows_the_tool_detail() {
        let options = vec!["Allow once".to_owned()];
        let sheet = approval_sheet(
            "The agent wants to write a file that already exists and will be replaced entirely",
            &Subject {
                tool: "write",
                detail: "src/main.rs",
                input: None,
            },
            &options,
            true,
            0,
            styles(),
        );
        let body = body_text(&sheet);
        assert!(body.len() >= 3, "the prompt wraps: {body:?}");
        assert_eq!(body.last().unwrap(), "write  src/main.rs");
    }

    /// An approval says what would run: the command or the content, bounded.
    #[test]
    fn the_approval_sheet_previews_the_input_it_would_run() {
        let options = vec!["Allow once".to_owned(), "Deny".to_owned()];
        let input = (1..=9)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let sheet = approval_sheet(
            "Allow tool `write` for this call?",
            &Subject {
                tool: "write",
                detail: "notes.txt",
                input: Some(&input),
            },
            &options,
            true,
            0,
            styles(),
        );
        let body = body_text(&sheet);
        assert_eq!(body[1], "write  notes.txt");
        assert_eq!(body[2], "  line 1");
        assert_eq!(body[7], "  line 6");
        assert_eq!(body[8], "  … 3 more lines");
        assert_eq!(body.len(), 9);
    }

    #[test]
    fn the_find_bar_shows_the_query_and_the_match_position() {
        let sheet = find_sheet("", 0, 0);
        assert_eq!(sheet.title, "Find");
        assert_eq!(sheet.status.as_deref(), Some("no matches"));
        let sheet = find_sheet("parser", 5, 2);
        assert_eq!(sheet.title, "Find: parser");
        assert_eq!(sheet.status.as_deref(), Some("3 of 5"));
    }

    #[test]
    fn a_session_row_marks_the_resident_session_and_shows_model_and_age() {
        let row = session_row(&info(), "Fix the parser".into(), true, now());
        assert_eq!(row.label, "Fix the parser");
        assert_eq!(row.mark, Some(true));
        assert_eq!(row.detail, "gpt-6-luna 3h");
        let row = session_row(&info(), "Fix the parser".into(), false, now());
        assert_eq!(row.mark, Some(false));
    }

    #[test]
    fn ages_cover_every_unit() {
        let stamp = "2026-01-02T03:04:05Z";
        let base = parse_unix_seconds(stamp).unwrap();
        let age = |seconds| age_label(stamp, base + seconds);
        assert_eq!(age(30), "now");
        assert_eq!(age(600), "10m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(172_800), "2d");
        assert_eq!(age(14 * 86_400), "2w");
        assert_eq!(age(100 * 86_400), "3mo");
        assert_eq!(age(800 * 86_400), "2y");
        assert_eq!(age_label("not a timestamp", base), "");
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(count(1, "model"), "1 model");
        assert_eq!(count(0, "session"), "0 sessions");
    }

    #[test]
    fn a_model_shows_its_provider_and_reference() {
        let model: ModelInfo = serde_json::from_value(json!({
            "id": "gpt-6-luna", "model_ref": "cus-oai/gpt-6-luna", "context_window": 400_000,
            "supported_reasoning": ["low"]
        }))
        .unwrap();
        assert_eq!(provider(&model), Some("cus-oai"));
        assert_eq!(model_details(&model), "cus-oai/gpt-6-luna · 400k context");
        let bare: ModelInfo =
            serde_json::from_value(json!({"id": "x", "supported_reasoning": []})).unwrap();
        assert_eq!(provider(&bare), None);
        assert_eq!(model_details(&bare), "x");
    }
}
