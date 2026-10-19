//! Builders for the bottom sheets: the command palette, the pickers, the
//! approval prompt and the find bar. Each turns app state into one [`Sheet`].

use ratatui::text::{Line, Span};

use crate::app::commands;
use crate::app::overlay;
use crate::app::session::auto_title;
use crate::app::{App, Overlay, Session};
use crate::protocol::SessionInfo;

use super::panel::{Row, Sheet, Tone, body_lines, truncate};
use super::picker::fuzzy_match;
use super::theme::Styles;

/// The sheet for the current overlay, or `None` when no sheet is open. A
/// pending interaction is a sheet too: it owns the keyboard until answered.
pub fn sheet(app: &App, styles: Styles) -> Option<Sheet> {
    Some(match &app.overlay {
        Overlay::None => return interaction_sheet(app, styles),
        Overlay::Palette { filter, selected } => palette_sheet(filter, *selected),
        Overlay::Sessions {
            filter,
            selected,
            rows,
            armed,
        } => sessions_sheet(app, rows, filter, *selected, armed.is_some()),
        Overlay::Models { selected } => models_sheet(app, *selected),
        Overlay::Reasoning { selected } => reasoning_sheet(app, *selected),
        Overlay::Files {
            filter,
            selected,
            paths,
        } => files_sheet(paths, filter, *selected),
        Overlay::Find {
            query,
            matches,
            current,
        } => Sheet::new(find_title(query), find_hint(matches.len(), *current)),
        // Views own the transcript area instead of a sheet.
        Overlay::Context | Overlay::Help { .. } | Overlay::Diff(_) | Overlay::Tool(_) => {
            return None;
        }
    })
}

fn palette_sheet(filter: &str, selected: usize) -> Sheet {
    let rows = commands::filter(filter)
        .into_iter()
        .map(|command| Row::new(command.name, command.description))
        .collect();
    Sheet::new("Commands", "enter run · esc close")
        .rows(rows)
        .selected(Some(selected))
}

fn sessions_sheet(
    app: &App,
    rows: &[SessionInfo],
    filter: &str,
    selected: usize,
    armed: bool,
) -> Sheet {
    let resident = app
        .session
        .as_ref()
        .map(|session| session.info.session_id.as_str());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let rows = filtered_sessions(rows, filter, app.session.as_ref())
        .into_iter()
        .map(|info| {
            let label = session_label(info, app.session.as_ref());
            let current = resident == Some(info.session_id.as_str());
            session_row(info, label, current, now)
        })
        .collect();
    let hint = if armed {
        "ctrl+d again deletes · esc cancels"
    } else {
        "enter open · type filter · ctrl+d delete · esc close"
    };
    Sheet::new("Sessions", hint)
        .subtitle(filter_subtitle(filter))
        .rows(rows)
        .selected(Some(selected))
}

/// The typed filter, shown after a picker's title so it is visible somewhere.
fn filter_subtitle(filter: &str) -> Option<String> {
    (!filter.is_empty()).then(|| filter.to_owned())
}

fn models_sheet(app: &App, selected: usize) -> Sheet {
    let current = app
        .session
        .as_ref()
        .map(|session| session.info.model.clone());
    let rows = app
        .models
        .iter()
        .map(|model| {
            Row::new(
                model.id.clone(),
                match model.supported_reasoning.len() {
                    0 => String::new(),
                    count => format!("{count} reasoning levels"),
                },
            )
            .mark(Some(&model.id) == current.as_ref())
        })
        .collect();
    Sheet::new("Model", "enter switch · esc close")
        .rows(rows)
        .selected(Some(selected))
}

fn reasoning_sheet(app: &App, selected: usize) -> Sheet {
    let levels = app.reasoning_levels();
    let current = app.session.as_ref().map(|session| session.info.reasoning);
    let rows = levels
        .iter()
        .map(|level| Row::new(level.label(), String::new()).mark(Some(*level) == current))
        .collect();
    Sheet::new("Reasoning", "enter apply · shift+tab cycles · esc close")
        .rows(rows)
        .selected(Some(selected))
}

fn files_sheet(paths: &[String], filter: &str, selected: usize) -> Sheet {
    let rows = overlay::matching_paths(paths, filter)
        .into_iter()
        .map(|path| Row::new(format!("@{path}"), String::new()))
        .collect();
    Sheet::new(
        "Files",
        "enter insert the path · type to filter · esc close",
    )
    .subtitle(filter_subtitle(filter))
    .rows(rows)
    .selected(Some(selected))
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
    Some(approval_sheet(
        &interaction.prompt,
        &subject,
        &options,
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
        .selected(Some(selected))
}

/// Hint row of the find bar, which changes with the match count.
fn find_hint(total: usize, current: usize) -> String {
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
fn find_title(query: &str) -> String {
    if query.is_empty() {
        "Find".into()
    } else {
        format!("Find: {query}")
    }
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

/// Seconds since the Unix epoch for an RFC 3339 stamp.
fn parse_unix_seconds(stamp: &str) -> Option<u64> {
    crate::protocol::stamp::unix_millis(stamp).map(|millis| millis / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Palette;
    use serde_json::json;

    #[test]
    fn the_palette_lists_command_names_and_descriptions() {
        let sheet = palette_sheet("re", 1);
        assert_eq!(sheet.title, "Commands");
        assert!(sheet.rows.iter().any(|row| row.label == "reasoning"));
        assert!(
            sheet
                .rows
                .iter()
                .any(|row| row.detail.contains("reasoning level"))
        );
        assert_eq!(sheet.selected, Some(1));
    }

    #[test]
    fn the_palette_filters_down_to_matching_commands() {
        let sheet = palette_sheet("zzz", 0);
        assert!(sheet.rows.is_empty());
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
        let styles = Styles::of(Palette::dark());
        let options = vec!["Allow once".to_owned(), "Deny".to_owned()];
        let sheet = approval_sheet("Write src/main.rs?", &NO_SUBJECT, &options, 0, styles);
        assert_eq!(sheet.title, "Approval");
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.rows[0].label, "Allow once");
        assert_eq!(sheet.rows[0].tone, Tone::Success);
        assert_eq!(sheet.rows[1].tone, Tone::Failure);
        assert_eq!(sheet.selected, Some(0));
    }

    #[test]
    fn the_approval_sheet_wraps_the_prompt_and_shows_the_tool_detail() {
        let styles = Styles::of(Palette::dark());
        let options = vec!["Allow once".to_owned()];
        let sheet = approval_sheet(
            "The agent wants to write a file that already exists and will be replaced entirely",
            &Subject {
                tool: "write",
                detail: "src/main.rs",
                input: None,
            },
            &options,
            0,
            styles,
        );
        let body = body_text(&sheet);
        assert!(body.len() >= 3, "the prompt wraps: {body:?}");
        assert_eq!(body.last().unwrap(), "write  src/main.rs");
    }

    /// An approval says what would run: the command or the content, bounded.
    #[test]
    fn the_approval_sheet_previews_the_input_it_would_run() {
        let styles = Styles::of(Palette::dark());
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
            0,
            styles,
        );
        let body = body_text(&sheet);
        assert_eq!(body[1], "write  notes.txt");
        assert_eq!(body[2], "  line 1");
        assert_eq!(body[7], "  line 6");
        assert_eq!(body[8], "  … 3 more lines");
        assert_eq!(body.len(), 9);
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
        let styles = Styles::of(palette);
        let sheet = Sheet::new("Sessions", "hint")
            .rows(vec![
                session_row(&info(), "Fix the parser".into(), true, now()),
                session_row(&info(), "Other".into(), false, now()),
            ])
            .selected(Some(1));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 4)).unwrap();
        terminal
            .draw(|frame| crate::ui::panel::render(frame, frame.area(), styles, &sheet))
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
}
