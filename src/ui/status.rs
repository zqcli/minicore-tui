//! The dock under the transcript: queued messages, the Working line, the
//! notice, the editor with its completion rows, and the footer. Rows are built
//! for the band right of the 1-column gutter.

use std::time::Duration;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::Styles;
use super::{composer, sheets, wrap};
use crate::app::{App, Notice, NoticeLevel, QueuedKind, Session};
use crate::transcript::Block;

/// Queued messages listed before the rest are summarized.
const QUEUE_ROWS: usize = 3;

pub struct Dock {
    /// Rows above the editor slot; the last is always the blank spacer.
    pub above: Vec<Line<'static>>,
    /// The editor body, then the completion rows.
    pub editor: Vec<Line<'static>>,
    /// The cursor as (row, column) within `editor`.
    pub cursor: (u16, u16),
    pub footer: Line<'static>,
}

/// The dock for a band `width` cells wide on a terminal `height` rows tall.
pub fn dock(app: &App, styles: Styles, width: u16, height: u16) -> Dock {
    let mut above = Vec::new();
    if let Some(session) = app.session.as_ref().filter(|s| !s.queue.is_empty()) {
        above.push(Line::default());
        above.extend(queued_rows(session, styles, width));
    }
    if let Some(line) = working_line(app, styles, width) {
        above.push(Line::default());
        above.push(line);
    }
    if let Some(notice) = app.notices.back() {
        above.push(Line::default());
        above.push(notice_line(notice, styles, width));
    }
    above.push(Line::default());

    let (mut editor, cursor) =
        composer::render(&app.composer, styles, width, composer::max_rows(height));
    if let Some(rows) = sheets::completion(app, styles, width.saturating_sub(1)) {
        editor.extend(rows.into_iter().map(|line| {
            let spans = line
                .spans
                .into_iter()
                .map(|span| {
                    let style = line.style.patch(span.style);
                    span.style(style)
                })
                .collect();
            composer::row(spans, usize::from(width), styles)
        }));
    }
    Dock {
        above,
        editor,
        cursor,
        footer: footer(app, styles, width),
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How long each spinner frame shows.
pub const SPINNER_FRAME: Duration = Duration::from_millis(80);

/// The spinner glyph for a point in time. The main loop redraws on every tick
/// while something is in flight, so a frame derived from the clock animates.
pub fn spinner(elapsed: Duration) -> &'static str {
    let frame = elapsed.as_millis() / SPINNER_FRAME.as_millis();
    SPINNER[(frame % SPINNER.len() as u128) as usize]
}

/// A dim row with one cell of padding on each side, cut to one line.
fn padded(text: &str, width: u16, style: Style) -> Line<'static> {
    let text = wrap::sanitize(&text.replace('\n', " ")).into_owned();
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            wrap::truncate(&text, usize::from(width).saturating_sub(2)),
            style,
        ),
    ])
}

fn queued_rows(session: &Session, styles: Styles, width: u16) -> Vec<Line<'static>> {
    // Steering messages are applied first, so they are listed first.
    let mut queue: Vec<_> = session.queue.iter().collect();
    queue.sort_by_key(|queued| queued.kind == QueuedKind::Follow);
    let mut rows: Vec<Line<'static>> = queue
        .into_iter()
        .take(QUEUE_ROWS)
        .map(|queued| {
            let text = format!("{}: {}", queued.label(), queued.text);
            padded(&text, width, styles.faint())
        })
        .collect();
    if session.queue.len() > QUEUE_ROWS {
        let more = format!("+{} more queued", session.queue.len() - QUEUE_ROWS);
        rows.push(padded(&more, width, styles.faint()));
    }
    if session
        .queue
        .iter()
        .any(|queued| queued.kind == QueuedKind::Follow)
    {
        let hint = "↳ alt+up to edit the last follow-up";
        rows.push(padded(hint, width, styles.faint()));
    }
    rows
}

/// `⠋ Working (esc to interrupt) · Running bash … · 42s · request 3` while
/// something is in flight.
fn working_line(app: &App, styles: Styles, width: u16) -> Option<Line<'static>> {
    let live = app
        .session
        .as_ref()
        .and_then(|session| session.live.as_ref());
    let interrupting = live.is_some_and(|live| live.interrupting);
    let compaction = app
        .session
        .as_ref()
        .and_then(|session| session.compaction_label());
    let text = if let Some(label) = &compaction {
        label.as_str()
    } else if app.has_interaction() {
        // Esc answers the prompt with a denial; it does not cancel the turn.
        "Waiting for approval (esc to deny)"
    } else if interrupting {
        "Interrupting…"
    } else if live.is_some() {
        "Working (esc to interrupt)"
    } else if app.waiting_for_compaction {
        "Waiting for compaction…"
    } else {
        return None;
    };
    let mut extras = Vec::new();
    if let Some(live) = live {
        let quiet = interrupting || app.has_interaction() || app.compacting();
        match live.running_tool.as_ref().filter(|_| !quiet) {
            Some((tool, detail)) if detail.is_empty() => extras.push(format!("Running {tool}")),
            Some((tool, detail)) => {
                let detail = wrap::truncate(&detail.replace('\n', " "), 48);
                extras.push(format!("Running {tool} {detail}"));
            }
            None if !quiet => extras.push(live.phase.label().to_owned()),
            None => {}
        }
        extras.push(super::blocks::format_duration(live.started.elapsed()));
        if live.requests > 1 {
            extras.push(format!("request {}", live.requests));
        }
        let tools = app
            .session
            .as_ref()
            .map_or(0, |session| session.tools_since(live.start));
        if tools > 0 {
            extras.push(super::blocks::plural(tools, "tool"));
        }
    }
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(spinner(app.started.elapsed()), styles.accent()),
        Span::raw(" "),
        Span::styled(text.to_owned(), styles.dim()),
    ];
    for extra in extras {
        spans.push(Span::styled(" · ", styles.meta()));
        spans.push(Span::styled(
            wrap::sanitize(&extra).into_owned(),
            styles.meta(),
        ));
    }
    Some(Line::from(fit(spans, usize::from(width))))
}

/// The latest notice as a status line: a rail, then dim text, or the failure
/// colour for an error.
fn notice_line(notice: &Notice, styles: Styles, width: u16) -> Line<'static> {
    let style = match notice.level {
        NoticeLevel::Error => styles.failure(),
        NoticeLevel::Info => styles.faint(),
    };
    let text = wrap::sanitize(&notice.text.replace('\n', " ")).into_owned();
    Line::from(vec![
        Span::styled("▎", styles.status_rail()),
        Span::styled(
            wrap::truncate(&text, usize::from(width).saturating_sub(1)),
            style,
        ),
    ])
}

/// `▸ dir@branch · model · reasoning · profile · state · 3m · queued` and,
/// right-aligned, `↑in ↓out Rcache · ctx N%`. Short of room, the directory and
/// branch give way first, then the elapsed time and queue mark, then the model;
/// the reasoning level stays.
fn footer(app: &App, styles: Styles, width: u16) -> Line<'static> {
    let width = usize::from(width);
    let session = app.session.as_ref();

    let workspace = app
        .workspace
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.workspace.display().to_string());
    let branch = session
        .and_then(|session| session.presentation.as_ref())
        .and_then(|presentation| presentation.git_branch.as_deref())
        .filter(|branch| !branch.trim().is_empty());

    let model = session.map_or_else(Vec::new, |session| {
        let model = wrap::truncate(&model_label(&session.info.model), 24);
        vec![Span::styled(model, styles.user_rail())]
    });

    let mut priority = Vec::new();
    if let Some(session) = session {
        priority.push(Span::styled(session.info.reasoning.label(), styles.amber()));
    }
    let mut extras = Vec::new();
    if let Some(profile) = session.and_then(|session| guarded_profile(app, session)) {
        extras.push(Span::styled(profile.to_owned(), styles.reasoning_rail()));
    }
    if let Some(session) = session.filter(|session| !session.allowed_tools.is_empty()) {
        let tools: Vec<&str> = session.allowed_tools.iter().map(String::as_str).collect();
        extras.push(Span::styled(
            format!("allow {}", tools.join(",")),
            styles.amber(),
        ));
    }
    let state = if app.has_interaction() {
        Some(("approval", styles.amber()))
    } else if app.compacting() {
        Some(("compacting", styles.user_rail()))
    } else if app.is_busy() {
        Some(("working", styles.user_rail()))
    } else if !app.ready || app.loading_session {
        Some(("connecting", styles.user_rail()))
    } else {
        None
    };
    extras.extend(state.map(|(text, style)| Span::styled(text, style)));
    for extra in extras {
        if !priority.is_empty() {
            priority.push(Span::styled(" · ", styles.meta()));
        }
        priority.push(extra);
    }

    let mut suffix = Vec::new();
    if let Some(elapsed) = session.and_then(|session| turn_elapsed(app, session)) {
        suffix.push(Span::styled(minutes(elapsed), styles.amber()));
    }
    if session.is_some_and(|session| !session.queue.is_empty()) {
        if !suffix.is_empty() {
            suffix.push(Span::styled(" · ", styles.meta()));
        }
        suffix.push(Span::styled("queued", styles.amber()));
    }

    let right = session.map_or_else(Vec::new, |session| totals(session, styles));
    let right_width = spans_width(&right);
    if right_width >= width {
        return Line::from(fit(right, width));
    }
    let room = if right.is_empty() {
        width
    } else {
        width - right_width - 1
    };
    let mut groups = [Vec::new(), model, priority, suffix];
    // The identity takes whatever the rest leaves.
    let rest = spans_width(&join(&groups, styles));
    let separator = if rest > 0 { 3 } else { 0 };
    groups[0] = identity(
        &workspace,
        branch,
        room.saturating_sub(rest + separator),
        styles,
    );
    for index in [3, 1, 2] {
        let over = spans_width(&join(&groups, styles)).saturating_sub(room);
        if over == 0 {
            break;
        }
        let own = spans_width(&groups[index]);
        // A group cut to a lone `…` says nothing; it goes with its separator.
        groups[index] = if own < over + 2 {
            Vec::new()
        } else {
            fit(std::mem::take(&mut groups[index]), own - over)
        };
    }
    let mut spans = join(&groups, styles);
    if !right.is_empty() {
        let gap = width - right_width - spans_width(&spans);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// The session's profile when it asks before acting, which the footer would
/// otherwise never reveal.
fn guarded_profile<'a>(app: &App, session: &'a Session) -> Option<&'a str> {
    let profile = app
        .profiles
        .iter()
        .find(|profile| profile.id == session.info.profile)?;
    let approval = profile.approval.as_deref()?;
    (approval != "auto").then_some(session.info.profile.as_str())
}

/// `▸ dir@branch` in at most `room` cells: the directory is cut first (to 6
/// cells at the least), then the branch goes, then the whole thing.
fn identity(
    workspace: &str,
    branch: Option<&str>,
    room: usize,
    styles: Styles,
) -> Vec<Span<'static>> {
    let branch = branch.map(|branch| format!("@{}", wrap::truncate(branch, 16)));
    let branch_width = branch.as_deref().map_or(0, wrap::width);
    let workspace = wrap::truncate(workspace, 20);
    let (name, branch) = if 2 + wrap::width(&workspace) + branch_width <= room {
        (workspace, branch)
    } else if room >= 2 + 6 + branch_width {
        (wrap::truncate(&workspace, room - 2 - branch_width), branch)
    } else if room >= 2 + 4 {
        (wrap::truncate(&workspace, room - 2), None)
    } else {
        return Vec::new();
    };
    let mut spans = vec![
        Span::styled("▸ ", styles.text()),
        Span::styled(name, styles.text()),
    ];
    spans.extend(branch.map(|branch| Span::styled(branch, styles.mint())));
    spans
}

/// `↑in ↓out Rcache · ctx N%`, each part once known; the context share turns amber from 70 %.
fn totals(session: &Session, styles: Styles) -> Vec<Span<'static>> {
    let usage = &session.usage;
    let tokens = crate::ui::format_tokens;
    let mut spans = Vec::new();
    // Totals count from when the session was opened, so zeros on a reopened
    // session would misstate it.
    if usage.input + usage.output + usage.cached > 0 {
        spans.push(Span::styled(
            format!("↑{} ↓{}", tokens(usage.input), tokens(usage.output)),
            styles.user_rail(),
        ));
        spans.push(Span::styled(
            format!(" R{}", tokens(usage.cached)),
            styles.reasoning_rail(),
        ));
    }
    // An unknown window is only worth a mark once the session has used some.
    let context = match super::views::context::percent(session) {
        Some(percent) if percent >= 70.0 => Some((format!("ctx {percent:.2}%"), styles.amber())),
        Some(percent) => Some((format!("ctx {percent:.2}%"), styles.reasoning_rail())),
        None if session.presentation.is_some() && usage.input > 0 => {
            Some(("ctx ?".to_owned(), styles.reasoning_rail()))
        }
        None => None,
    };
    if let Some((text, style)) = context {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", styles.meta()));
        }
        spans.push(Span::styled(text, style));
    }
    spans
}

/// The running turn's time, else the last finished turn's.
fn turn_elapsed(app: &App, session: &Session) -> Option<Duration> {
    app.busy_elapsed().or_else(|| {
        session
            .transcript
            .blocks()
            .iter()
            .rev()
            .find_map(|block| match block {
                Block::TurnEnd { duration, .. } => Some(*duration),
                _ => None,
            })
            .flatten()
    })
}

/// Whole minutes: `0m`, `42m`, `1h5m`.
fn minutes(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    if minutes >= 60 {
        format!("{}h{}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

/// A model id as people say it: `gpt-6-luna` is `gpt 6 luna`, and date,
/// `-latest` and `-preview` suffixes go.
fn model_label(id: &str) -> String {
    let mut name = id;
    while let Some(rest) = name
        .strip_suffix("-latest")
        .or_else(|| name.strip_suffix("-preview"))
    {
        name = rest;
    }
    if let Some((head, date)) = name.rsplit_once('-')
        && date.len() == 8
        && date.starts_with("20")
        && date.bytes().all(|byte| byte.is_ascii_digit())
    {
        name = head;
    }
    let words: Vec<&str> = name
        .split(['-', ' '])
        .filter(|word| !word.is_empty())
        .collect();
    wrap::sanitize(&words.join(" ")).into_owned()
}

/// The non-empty groups joined by a meta separator.
fn join(groups: &[Vec<Span<'static>>], styles: Styles) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for group in groups.iter().filter(|group| !group.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", styles.meta()));
        }
        spans.extend(group.iter().cloned());
    }
    spans
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| wrap::width(&span.content)).sum()
}

/// Cuts `spans` to `width` cells, ending in `…` when anything was cut.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut left = width;
    for span in spans {
        let own = wrap::width(&span.content);
        if own < left {
            left -= own;
            out.push(span);
            continue;
        }
        // A span that exactly fills the room still needs the ellipsis.
        let text = format!("{}…", span.content);
        out.push(Span::styled(wrap::truncate(&text, left), span.style));
        break;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_read_like_names() {
        assert_eq!(model_label("gpt-6-luna"), "gpt 6 luna");
        assert_eq!(model_label("claude-sonnet-4-20250514"), "claude sonnet 4");
        assert_eq!(model_label("gemini-2.5-pro-preview"), "gemini 2.5 pro");
        assert_eq!(model_label("deepseek-v4-pro-latest"), "deepseek v4 pro");
    }

    #[test]
    fn elapsed_time_is_whole_minutes() {
        assert_eq!(minutes(Duration::from_secs(59)), "0m");
        assert_eq!(minutes(Duration::from_secs(185)), "3m");
        assert_eq!(minutes(Duration::from_secs(3900)), "1h5m");
    }

    #[test]
    fn fitting_spans_ends_in_an_ellipsis() {
        let spans = vec![Span::raw("abc"), Span::raw("def")];
        let text = |spans: Vec<Span<'static>>| -> String {
            spans.iter().map(|span| span.content.as_ref()).collect()
        };
        assert_eq!(text(fit(spans.clone(), 6)), "abcdef");
        assert_eq!(text(fit(spans.clone(), 5)), "abcd…");
        assert_eq!(text(fit(spans, 3)), "ab…");
    }
}
