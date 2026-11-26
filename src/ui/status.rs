//! The dock under the transcript: queued messages, the Working line, the key
//! hint, the notice, the editor with its completion rows, and the footer. Rows
//! are built for the band right of the 1-column gutter.

use std::time::Duration;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::composer::Editor;
use super::theme::Styles;
use super::{composer, sheets};
use crate::app::{App, Notice, NoticeLevel, Session};
use crate::wrap;
use minicore_host_core::InteractionRequestData;
use minicore_host_protocol::{
    CompactionState, InputQueue, MessageSource, ModelRetryPhase, QueuedInputRecord, TurnPhase,
    TurnState,
};
use minicore_host_runtime::tools::ToolInputAnswerKind;

pub struct Dock {
    /// Rows above the editor slot; the last is always the blank spacer.
    pub above: Vec<Line<'static>>,
    /// The editor body, then the completion rows.
    pub editor: Vec<Line<'static>>,
    pub editor_rows: u16,
    /// The cursor as (row, column) within `editor`.
    pub cursor: (u16, u16),
    pub footer: Line<'static>,
}

/// The dock for a band `width` cells wide on a terminal `height` rows tall.
pub fn dock(app: &App, styles: Styles, width: u16, height: u16) -> Dock {
    // One blank row parts the transcript from the dock.
    let mut above = vec![Line::default()];
    let unresolved = app
        .local_inputs
        .iter()
        .filter(|input| input.status != crate::app::unconfirmed::Status::Sending)
        .count();
    if unresolved > 0 {
        above.push(padded(&format!("{unresolved} retained local inputs · F8 review · unconfirmed may already be accepted"),width,styles.failure()));
    }

    let text_input = app.resident.current().filter(|session| {
            !matches!(app.overlay, crate::app::Overlay::Unconfirmed { .. })
                && session.interaction.as_ref().is_some_and(|pending| {
                    matches!(pending.request(), InteractionRequestData::ToolInput { request } if request.answer_kind == ToolInputAnswerKind::Text)
                })
        });
    if let Some(session) = text_input {
        let pending = session.interaction.as_ref().unwrap();
        above.push(padded(
            &format!("{} · input", pending.tool_name()),
            width,
            styles.title(),
        ));
        if let InteractionRequestData::ToolInput { request } = pending.request() {
            above.extend(super::panel::body_lines(
                request.prompt.as_str(),
                usize::from(width),
                styles.text(),
            ));
        }
        above.push(padded(
            if pending.submitted {
                "Answer submitted · waiting for Host confirmation"
            } else {
                "Text answer · enter send · esc cancel turn"
            },
            width,
            styles.faint(),
        ));
    }
    if matches!(
        app.overlay,
        crate::app::Overlay::Unconfirmed {
            mode: crate::app::unconfirmed::ReviewMode::Edit(_),
            ..
        }
    ) {
        let label = if app
            .selected_local_input()
            .is_some_and(|input| input.status == crate::app::unconfirmed::Status::Rejected)
        {
            "Editing rejected input · not accepted · enter review send · esc cancel edit"
        } else {
            "Editing unconfirmed input · acceptance is still unknown · enter review resend · esc cancel edit"
        };
        above.push(padded(label, width, styles.failure()));
    }
    if let Some(session) = app.resident.current() {
        if let Some(reason) = &session.state.data.block_reason {
            above.push(padded(
                &format!("{} · {}", reason.message, reason.resolution),
                width,
                styles.failure(),
            ));
        }
        if let Some(error) = &session.observation_error {
            above.push(padded(error, width, styles.failure()));
        }
    }
    // The activity row appears only with something to say, so an idle screen
    // has one blank row above the editor like every other gap. The editor is
    // pinned to the bottom; only the transcript gives up the row.
    let activity = activity_line(app, styles, width);
    if spans_width(&activity.spans) > 0 {
        above.push(activity);
        if let Some(session) = app.resident.current() {
            above.extend(queued_rows(session, styles, width));
        }
        // Status is not part of the draft: a blank row keeps it off the editor.
        above.push(Line::default());
    }
    let editor_mode = match &app.overlay {
        crate::app::Overlay::Delegations { focus, .. } => {
            if !app.inspected_can_send() {
                Editor::Closed
            } else if *focus == crate::app::TaskFocus::Composer {
                Editor::Live
            } else {
                Editor::TaskInactive
            }
        }
        overlay if overlay.is_view() => Editor::View,
        _ if app.resident.current().is_some_and(|session| {
            session.state.data.lifecycle != minicore_host_protocol::SessionLifecycle::Open
        }) =>
        {
            Editor::Closed
        }
        _ => Editor::Live,
    };
    let (editor, cursor) = composer::render(
        app.active_editor(),
        styles,
        width,
        composer::max_rows(height),
        editor_mode,
    );
    let editor_rows = editor.len() as u16;
    Dock {
        above,
        editor,
        editor_rows,
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
    // Text in the content column, like the activity line's.
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            wrap::truncate(&text, usize::from(width).saturating_sub(3)),
            style,
        ),
    ])
}

/// Rows of queued text shown under the activity line.
const QUEUE_ROWS: usize = 3;

/// Whether a queued entry is a child Agent's settled JSON result notification.
/// Those are internal handoffs for the model, never user-editable prompts.
fn is_delegated_result(queued: &QueuedInputRecord) -> bool {
    matches!(queued.source, MessageSource::Agent { .. })
        && super::delegated::is_result(&queued.input.text)
}

/// What will be sent next, under the activity line: `↗` a steer for the next
/// step, `↳` a follow-up for the next Turn, oldest first. Internal delegation
/// result JSON notifications stay out of this preview.
fn queued_rows(session: &Session, styles: Styles, width: u16) -> Vec<Line<'static>> {
    let Some(queue) = &session.queue else {
        return Vec::new();
    };
    let room = usize::from(width).saturating_sub(6);
    let visible: Vec<&QueuedInputRecord> = queue
        .next_step
        .iter()
        .chain(&queue.next_turn)
        .filter(|queued| !is_delegated_result(queued))
        .collect();
    let mut rows: Vec<Line<'static>> = visible
        .iter()
        .take(QUEUE_ROWS)
        .map(|queued| {
            let glyph = match queued.queue {
                InputQueue::NextStep => "↗ ",
                InputQueue::NextTurn => "↳ ",
            };
            let text: String = queued.input.text.chars().take(room + 1).collect();
            let text = wrap::sanitize(&text.replace('\n', " ")).into_owned();
            Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph, styles.meta()),
                Span::styled(wrap::truncate(&text, room), styles.dim()),
            ])
        })
        .collect();
    if visible.len() > QUEUE_ROWS {
        rows.push(padded(
            &format!("… {} more queued", visible.len() - QUEUE_ROWS),
            width,
            styles.faint(),
        ));
    }
    rows
}

fn user_queued_count(session: &Session) -> usize {
    session.queue.as_ref().map_or(0, |queue| {
        queue
            .next_step
            .iter()
            .chain(&queue.next_turn)
            .filter(|queued| matches!(queued.source, MessageSource::User))
            .count()
    })
}

fn delegated_queued_count(session: &Session) -> usize {
    session.queue.as_ref().map_or(0, |queue| {
        queue
            .next_step
            .iter()
            .chain(&queue.next_turn)
            .filter(|queued| is_delegated_result(queued))
            .count()
    })
}

/// The facts and keys that matter now, right of the activity: what Esc or
/// Enter would do, the queue, a pause, the way back from a child, and the
/// single-line async task status summary (`needs input`, `running`, `failed`,
/// `completed` · `ctrl+b`).
fn right_keys(app: &App, styles: Styles) -> Vec<Vec<Span<'static>>> {
    let mut keys: Vec<Vec<Span<'static>>> = Vec::new();
    let faint = styles.faint();
    let plain = |text: String| vec![Span::styled(text, faint)];
    if let crate::app::Overlay::Delegations { focus, .. } = &app.overlay {
        match focus {
            crate::app::TaskFocus::Composer if app.inspected_can_send() => {
                keys.push(plain("enter steer · alt+enter follow-up".to_owned()));
                keys.push(plain("esc list".to_owned()));
            }
            crate::app::TaskFocus::Detail => {
                if app.inspected_can_send() {
                    keys.push(plain("enter message".to_owned()));
                }
                keys.push(plain("↑↓/jk scroll".to_owned()));
                keys.push(plain("ctrl+e full view".to_owned()));
                keys.push(plain("←/esc list".to_owned()));
            }
            _ => {
                if app.inspected_can_send() {
                    keys.push(plain("enter message".to_owned()));
                }
                keys.push(plain("→/tab scroll".to_owned()));
                keys.push(plain("ctrl+e full view".to_owned()));
                keys.push(plain("esc back".to_owned()));
            }
        }
        return keys;
    }
    if app.return_target().is_some() && app.composer.is_empty() {
        keys.push(plain("↰ esc back".to_owned()));
    }
    let Some(session) = app.resident.current() else {
        return keys;
    };
    if app.has_interaction() {
        return keys;
    }
    let drafting = !app.composer.is_empty();
    let user_queued = user_queued_count(session);
    let delegated_queued = delegated_queued_count(session);
    if session.paused() {
        keys.push(plain(
            if drafting {
                "paused · enter sends"
            } else {
                "paused · enter resumes"
            }
            .to_owned(),
        ));
    } else if drafting && (session.is_busy() || session.compacting()) {
        keys.push(plain("enter steer · alt+enter follow-up".to_owned()));
    } else if session.is_busy() {
        if app.return_target().is_some() {
            keys.push(plain("/cancel stop".to_owned()));
        } else {
            keys.push(plain("esc interrupt".to_owned()));
        }
    }
    if user_queued > 0 {
        keys.push(plain(format!("{user_queued} queued · alt+↑ edit")));
    }
    if delegated_queued > 0 {
        keys.push(plain(super::blocks::plural(
            delegated_queued as u64,
            "agent result",
        )));
    }
    let tasks = crate::app::overlay::async_tasks(app);
    if !tasks.is_empty() {
        let needs_input = tasks.iter().filter(|item| item.needs_input()).count();
        let running = tasks
            .iter()
            .filter(|item| item.is_running() && !item.needs_input())
            .count();
        let failed = tasks.iter().filter(|item| item.is_failed()).count();
        let completed = tasks.iter().filter(|item| item.is_completed()).count();
        let mut group = Vec::new();
        let push_part = |group: &mut Vec<Span<'static>>, text: String, style: Style| {
            if !group.is_empty() {
                group.push(Span::styled(" · ", styles.meta()));
            }
            group.push(Span::styled(text, style));
        };
        if needs_input > 0 {
            push_part(
                &mut group,
                format!("⚠ {needs_input} needs input"),
                styles.warning(),
            );
        }
        if running > 0 {
            push_part(&mut group, format!("{running} running"), styles.accent());
        }
        if failed > 0 {
            push_part(&mut group, format!("{failed} failed"), styles.failure());
        }
        if completed > 0 {
            push_part(
                &mut group,
                format!("{completed} completed"),
                styles.success(),
            );
        }
        if group.is_empty() {
            push_part(&mut group, format!("{} tasks", tasks.len()), styles.dim());
        }
        push_part(&mut group, "ctrl+b".to_owned(), faint);
        keys.push(group);
    }
    keys
}

/// Columns the activity text keeps before key hints are dropped.
const LEFT_ROOM: usize = 28;

/// The one row above the editor: what is running, else a fresh flash or the
/// latest notice, with [`right_keys`] at the right edge.
fn activity_line(app: &App, styles: Styles, width: u16) -> Line<'static> {
    let flash = app
        .flash
        .as_ref()
        .map(|(text, _)| vec![Span::raw("  "), Span::styled(text.clone(), styles.text())]);
    let left = if matches!(app.overlay, crate::app::Overlay::Delegations { .. }) {
        inspector_activity_left(app, styles).or(flash).or_else(|| {
            app.notices
                .back()
                .map(|notice| notice_spans(notice, styles))
        })
    } else {
        working_line(app, styles).or(flash).or_else(|| {
            app.notices
                .back()
                .map(|notice| notice_spans(notice, styles))
        })
    }
    .unwrap_or_default();
    let width = usize::from(width);
    // The status on the left keeps room to be read; key hints give way first,
    // the earliest (the mode hint) before the counts.
    let wanted = spans_width(&left).min(LEFT_ROOM);
    let mut keys = right_keys(app, styles);
    let keys_width = |keys: &[Vec<Span<'static>>]| {
        keys.iter()
            .map(|group| spans_width(group) + 2)
            .sum::<usize>()
            + 1
    };
    while !keys.is_empty() && keys_width(&keys) + wanted > width {
        keys.remove(0);
    }
    let right: Vec<Span<'static>> = keys
        .into_iter()
        .enumerate()
        .flat_map(|(index, group)| {
            let separator = (index > 0).then(|| Span::styled("  ", styles.faint()));
            separator.into_iter().chain(group)
        })
        .collect();
    let right_width = spans_width(&right);
    let left = fit(
        left,
        width.saturating_sub(if right_width > 0 { right_width + 2 } else { 0 }),
    );
    let used = spans_width(&left);
    let mut spans = left;
    if right_width > 0 && used + right_width < width {
        spans.push(Span::raw(" ".repeat(width - used - right_width - 1)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// Status row left side while `Ctrl+B` Async Tasks Inspector is open:
/// shows the selected child's own live Working state or a clear reply target.
fn inspector_activity_left(app: &App, styles: Styles) -> Option<Vec<Span<'static>>> {
    let crate::app::Overlay::Delegations {
        filter,
        selected,
        focus,
        ..
    } = &app.overlay
    else {
        return None;
    };
    let all = crate::app::overlay::async_tasks(app);
    let matched = crate::app::overlay::matching_async_tasks(&all, filter);
    let item = matched
        .get((*selected).min(matched.len().saturating_sub(1)))
        .copied()?;
    let short_id = item.short_id().to_owned();
    if let Some(child) = app.inspected_session()
        && child.is_busy()
    {
        let glyph = spinner(app.started.elapsed());
        let mut text = format!("Child {short_id} · Working");
        if let Some(live) = &child.live {
            if let Some(tool) = &live.running_tool {
                text.push_str(&format!(" · running {tool}"));
            } else if let Some(phase) = live.state.phase {
                let label = match phase {
                    TurnPhase::Model => "Model request",
                    TurnPhase::Tools => "Running tools",
                    TurnPhase::WaitingForInput => "Waiting for input",
                    TurnPhase::Finalizing => "Finalizing",
                };
                text.push_str(&format!(" · {label}"));
            }
        }
        return Some(vec![
            Span::styled(format!("{glyph} "), styles.accent()),
            Span::styled(text, styles.dim()),
        ]);
    }
    if item.needs_input() {
        return Some(vec![
            Span::raw("  "),
            Span::styled(
                format!("⚠ Child {short_id} needs input · ctrl+e to respond"),
                styles.warning(),
            ),
        ]);
    }
    if !app.inspected_can_send() {
        return Some(vec![
            Span::raw("  "),
            Span::styled(format!("Child {short_id} · read-only"), styles.meta()),
        ]);
    }
    let prompt = if *focus == crate::app::TaskFocus::Composer {
        format!("↳ Message to agent · Session {short_id}")
    } else {
        format!("↳ Session {short_id} · press enter or click editor to message")
    };
    Some(vec![
        Span::raw("  "),
        Span::styled(
            prompt,
            if *focus == crate::app::TaskFocus::Composer {
                styles.accent()
            } else {
                styles.dim()
            },
        ),
    ])
}

/// `⠋ Working (esc to interrupt) · Running bash … · request 3` while
/// something is in flight.
fn working_line(app: &App, styles: Styles) -> Option<Vec<Span<'static>>> {
    let session = app.resident.current();
    let live = session.and_then(|session| session.live.as_ref());
    let interrupting =
        live.is_some_and(|live| live.interrupting || live.state.state == TurnState::Cancelling);
    let compaction = session
        .and_then(|session| session.state.data.compaction.as_ref())
        .and_then(|activity| {
            Some(match activity.state {
                CompactionState::Preparing {} => "Preparing compaction",
                CompactionState::Summarizing {} => "Summarizing context",
                CompactionState::Merging {} => "Merging summary",
                CompactionState::Committing { .. } => "Saving compaction",
                CompactionState::Finished { .. } => return None,
            })
        });
    let text = if let Some(label) = &compaction {
        *label
    } else if app.has_interaction() {
        if app
            .resident
            .current()
            .and_then(|session| session.interaction.as_ref())
            .is_some_and(|pending| {
                matches!(pending.request(), InteractionRequestData::ToolInput { .. })
            })
        {
            "Waiting for input"
        } else {
            "Waiting for approval"
        }
    } else if interrupting {
        "Interrupting…"
    } else if session.is_some_and(Session::is_busy) {
        "Working"
    } else {
        return None;
    };
    let mut extras = Vec::new();
    if let Some(live) = live {
        let quiet = interrupting || app.has_interaction() || app.compacting();
        match live.running_tool.as_ref().filter(|_| !quiet) {
            Some(tool) => extras.push(format!("running {tool}")),
            None if !quiet => {
                if let Some(phase) = live.state.phase {
                    extras.push(
                        match phase {
                            TurnPhase::Model => "Model request",
                            TurnPhase::Tools => "Running tools",
                            TurnPhase::WaitingForInput => "Waiting for input",
                            TurnPhase::Finalizing => "Finalizing",
                        }
                        .to_owned(),
                    );
                }
            }
            None => {}
        }
        if let Some(retry) = &live.state.model_retry {
            extras.push(format!(
                "retry {}/{} {}",
                retry.attempt,
                retry.max_attempts,
                match retry.phase {
                    ModelRetryPhase::Waiting { .. } => "waiting",
                    ModelRetryPhase::Requesting => "requesting",
                }
            ));
        }
        if live.state.requests > 1 {
            extras.push(format!("request {}", live.state.requests));
        }
        if live.state.tool_rounds > 0 {
            extras.push(super::blocks::plural(live.state.tool_rounds, "tool round"));
        }
    }
    // The spinner in the glyph column, its text in the content column.
    let mut spans = vec![
        Span::styled(spinner(app.started.elapsed()), styles.accent()),
        Span::raw(" "),
        Span::styled(text.to_owned(), styles.dim()),
    ];
    // Whole minutes: the line redraws with the spinner, not per second.
    if let Some(live) = live.filter(|_| compaction.is_none()) {
        spans.push(Span::styled(
            format!(" {}", minutes(live.started.elapsed())),
            styles.meta(),
        ));
    }
    for extra in extras {
        spans.push(Span::styled(" · ", styles.meta()));
        spans.push(Span::styled(
            wrap::sanitize(&extra).into_owned(),
            styles.meta(),
        ));
    }
    Some(spans)
}

/// The latest notice: dim for information, warning colour for usage, failure
/// colour for an error, which also says where its full text is.
fn notice_spans(notice: &Notice, styles: Styles) -> Vec<Span<'static>> {
    let style = match notice.level {
        NoticeLevel::Error => styles.failure(),
        NoticeLevel::Usage => styles.warning(),
        NoticeLevel::Info => styles.dim(),
    };
    let text = wrap::sanitize(&notice.text.replace('\n', " ")).into_owned();
    let mut spans = vec![Span::raw("  "), Span::styled(text, style)];
    if notice.level == NoticeLevel::Error {
        spans.push(Span::styled(" · /error", styles.faint()));
    }
    spans
}

/// Where you are on the left — `▸ dir@branch · session · 1 running` — and
/// what runs it on the right — `model · reasoning · approval`. Short of
/// room, the session title goes first, then the directory; the right stays.
fn footer(app: &App, styles: Styles, width: u16) -> Line<'static> {
    let width = usize::from(width);
    let session = app.resident.current();

    let workspace_path = match session {
        Some(session) => session
            .info
            .config
            .workspace
            .as_ref()
            .map(|workspace| std::path::Path::new(&workspace.path)),
        None => Some(app.workspace.as_path()),
    };
    let workspace = workspace_path.map_or_else(
        || "no workspace".to_owned(),
        |path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        },
    );
    let branch = session
        .filter(|session| session.info.config.workspace.is_some())
        .and_then(|session| session.branch.as_deref());

    let mut right_groups = Vec::new();
    if let Some(id) = app.model_id() {
        right_groups.push(vec![Span::styled(
            wrap::truncate(&model_label(id), 24),
            styles.text(),
        )]);
    }
    if let Some(level) = app.reasoning() {
        right_groups.push(vec![Span::styled(
            sheets::reasoning_label(level),
            styles.meta(),
        )]);
    }
    if let Some(session) = session {
        right_groups.push(vec![Span::styled(
            format!(
                "approval {}",
                crate::app::overlay::approval_label(session.info.config.execution.approval)
            ),
            styles.meta(),
        )]);
    }
    let right = join(&right_groups, styles);
    let right_width = spans_width(&right);
    if right_width + 2 >= width {
        return Line::from(fit(right, width));
    }
    let room = width - right_width - 2;

    let title = session.map(|session| {
        let label = session
            .info
            .title
            .clone()
            .unwrap_or_else(|| session.info.id.as_str().chars().take(6).collect());
        vec![Span::styled(
            wrap::sanitize(&label).into_owned(),
            styles.meta(),
        )]
    });
    let mut groups = vec![Vec::new(), title.unwrap_or_default(), others(app, styles)];
    let rest = spans_width(&join(&groups[1..], styles));
    groups[0] = identity(
        &workspace,
        branch,
        room.saturating_sub(rest + 3).max(8),
        styles,
    );
    for index in [1, 0] {
        let over = spans_width(&join(&groups, styles)).saturating_sub(room);
        if over == 0 {
            break;
        }
        let own = spans_width(&groups[index]);
        groups[index] = if own < over + 2 {
            Vec::new()
        } else {
            fit(std::mem::take(&mut groups[index]), own - over)
        };
    }
    let mut spans = fit(join(&groups, styles), room);
    let gap = width - right_width - spans_width(&spans);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

/// `2 running · 1 needs input` over the background sessions, only the
/// parts that are not zero; empty when none needs attention.
pub fn others(app: &App, styles: Styles) -> Vec<Span<'static>> {
    let (working, input) = app.background_counts();
    let mut spans = Vec::new();
    if working > 0 {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", styles.meta()));
        }
        spans.push(Span::styled(format!("{working} running"), styles.text()));
    }
    if input > 0 {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", styles.meta()));
        }
        spans.push(Span::styled(
            format!("{input} needs input"),
            styles.warning(),
        ));
    }
    spans
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

/// Whole minutes: `0m`, `42m`, `1h5m`.
pub(super) fn minutes(elapsed: Duration) -> String {
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

pub(super) fn spans_width(spans: &[Span<'_>]) -> usize {
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
