//! Builders for the selectors and the inline completion list: the sessions,
//! model and reasoning pickers, the approval prompt and the find bar each turn
//! app state into one [`Sheet`]; the command and `@` file lists are plain rows.

use std::collections::HashMap;

use ratatui::text::{Line, Span};

use crate::app::overlay::{self, matching_models, short_session_id, workspace_name};
use crate::app::picker;
use crate::app::{App, LiveStatus, Overlay, Session};
use crate::transcript::Block;
use minicore_host_core::{InteractionRequestData, ModelDescription, SessionSummary};
use minicore_host_protocol::{
    AgentLifecycle, ReasoningPreference, Residency, Scheduling, SessionLifecycle, StorageMode,
    Timestamp, TurnOutcome,
};
use minicore_host_runtime::tools::ToolInputAnswerKind;

use super::blocks::plural;
use super::panel::{Kind, Row, Sheet, Tone, body_lines, truncate};
use super::theme::Styles;
use crate::wrap::width;

/// Rows of the completion list that show at once.
const COMPLETION_ROWS: usize = 5;

/// The sheet for the current overlay; a pending interaction is one too, as it owns the keyboard.
pub fn sheet(app: &App, styles: Styles) -> Option<Sheet> {
    Some(match &app.overlay {
        Overlay::Unconfirmed { mode, .. } => {
            use crate::app::unconfirmed::ReviewMode;
            if matches!(mode, ReviewMode::Edit(_)) {
                return None;
            }
            let mut sheet = Sheet::new(
                "Local input confirmations",
                "e edit · r resend · c copy · d forget · esc close",
            )
            .rows(
                app.local_inputs
                    .iter()
                    .map(|input| {
                        Row::new(
                            format!("tui-{} · {}", input.id, input.status.label()),
                            format!(
                                "{} · {}",
                                input.target.session.agent_id, input.target.session.session_id
                            ),
                        )
                    })
                    .collect(),
            )
            .selected(app.selected_local_input().and_then(|input| {
                app.local_inputs
                    .iter()
                    .position(|known| known.id == input.id)
            }));
            if let Some(input) = app.selected_local_input() {
                sheet = sheet.notes(vec![Line::raw(crate::wrap::truncate(
                    input.entry.text(),
                    120,
                ))]);
            }
            match mode {
                ReviewMode::Resend(_)|ReviewMode::ResendEdited(_)=>sheet.hint="May already be accepted; sending again can duplicate input · y resend · enter/esc cancel".into(),
                ReviewMode::Forget(_)=>sheet.hint="Forgetting does not cancel a possibly accepted input · y forget · enter/esc cancel".into(),
                _=>{},
            }
            if matches!(mode, ReviewMode::Resend(_) | ReviewMode::ResendEdited(_))
                && app
                    .selected_local_input()
                    .is_some_and(|input| input.status == crate::app::unconfirmed::Status::Rejected)
            {
                sheet.hint =
                    "Rejected before acceptance · y send as a new operation · enter/esc cancel"
                        .into();
            }
            sheet.status("Unconfirmed is not unsent; no automatic resend")
        }
        Overlay::None => return interaction_sheet(app, styles),
        Overlay::Sessions {
            filter,
            selected,
            rows,
            armed,
            renaming,
        } => {
            let state = SessionsState {
                filter,
                selected: *selected,
                armed: armed.as_ref(),
                renaming: renaming.as_ref(),
            };
            sessions_sheet(app, rows, &state, styles)
        }
        Overlay::SessionInfo => session_info_sheet(app, styles),
        Overlay::Context => context_sheet(app, styles),
        Overlay::Models { filter, selected } => models_sheet(app, filter, *selected, styles),
        Overlay::Agents { filter, selected } => agents_sheet(app, filter, *selected, styles),
        Overlay::AgentDelete {
            name,
            agent_id,
            sessions,
            selected,
        } => Sheet::new(
            format!("Delete Agent: {name}"),
            "↑↓ review sessions · y delete captured scope · esc cancel",
        )
        .status(format!(
            "Permanently delete Agent and {}",
            plural(sessions.len() as u64, "session")
        ))
        .body(vec![
            Line::styled(format!("Agent {agent_id}"), styles.failure()),
            Line::styled(
                "This deletes the listed sessions and their stored history.",
                styles.failure(),
            ),
        ])
        .rows(
            sessions
                .iter()
                .map(|(id, title)| Row::new(title.clone(), id.to_string()).tone(Tone::Failure))
                .collect(),
        )
        .selected(Some(*selected))
        .notes(if sessions.is_empty() {
            vec![Line::styled(
                "No child sessions in the captured scope.",
                styles.dim(),
            )]
        } else {
            Vec::new()
        }),
        Overlay::Reasoning { selected } => reasoning_sheet(app, *selected),
        Overlay::Find {
            query,
            matches,
            current,
        } => find_sheet(query, matches.len(), *current),
        // The completion lists live in the editor band and views own the transcript area.
        Overlay::Palette { .. }
        | Overlay::Files { .. }
        | Overlay::Help { .. }
        | Overlay::Diff(_) => return None,
    })
}

/// Rows of the inline completion list (commands, `@` files) shown inside the
/// composer band, as content only: the band adds the gutter, rail and tint.
pub fn completion(app: &App, styles: Styles, width: u16) -> Option<Vec<Line<'static>>> {
    let (items, selected) = match &app.overlay {
        Overlay::Palette { selected } => (app.palette_rows(), *selected),
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

/// The sessions picker's own state, as the sheet shows it.
struct SessionsState<'a> {
    filter: &'a str,
    selected: usize,
    /// The session a first Ctrl+D asked to delete.
    armed: Option<&'a minicore_host_protocol::SessionRef>,
    /// The title being typed, when a rename is under way.
    renaming: Option<&'a (minicore_host_protocol::SessionRef, String)>,
}

fn sessions_sheet(
    app: &App,
    rows: &[SessionSummary],
    state: &SessionsState,
    styles: Styles,
) -> Sheet {
    let SessionsState {
        filter,
        selected,
        armed,
        renaming,
    } = *state;
    let foreground = app.resident.current().map(Session::address);
    let now = jiff::Timestamp::now().as_second();
    let shown = app.filtered_sessions(rows, filter);
    if let Some((target, title)) = renaming {
        let name = rows
            .iter()
            .find(|info| overlay::session_target(info) == *target)
            .map(|info| app.session_title(info))
            .unwrap_or_else(|| target.session_id.to_string());
        return Sheet::new("Rename Session", "enter save · esc cancel")
            .status(name)
            .search(title);
    }
    let labels: Vec<String> = shown.iter().map(|info| app.session_title(info)).collect();
    let mut shared: HashMap<&str, usize> = HashMap::new();
    for label in &labels {
        *shared.entry(label).or_default() += 1;
    }
    let workspace = app.workspace.to_string_lossy();
    let status = if shown.len() == rows.len() {
        plural(rows.len() as u64, "session")
    } else {
        format!("{} of {}", shown.len(), rows.len())
    };
    let list = shown
        .iter()
        .zip(&labels)
        .map(|(info, label)| {
            let target = overlay::session_target(info);
            let current = foreground.as_ref() == Some(&target);
            let live = app.resident.find(&target);
            let context = RowContext {
                state: live.and_then(live_state).or_else(|| catalog_state(info)),
                current,
                other_workspace: info
                    .workspace
                    .as_ref()
                    .is_some_and(|value| value.path != workspace),
                agent: Some(agent_label(app, &info.agent_id)),
                duplicate: shared[label.as_str()] > 1,
            };
            let row = session_row(info, label.clone(), &context, now);
            if armed == Some(&target) || live.is_some_and(failed_unseen) {
                row.tone(Tone::Failure)
            } else {
                row
            }
        })
        .collect::<Vec<_>>();
    let hint = if armed.is_some() {
        "ctrl+d delete again · esc cancel"
    } else {
        "enter open · ctrl+n rename · ctrl+d delete · esc close"
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
    let current = app.model_id();
    let shown = matching_models(&app.models, filter);
    let rows = shown
        .iter()
        .map(|model| {
            let provider = provider(model).map(|name| format!("[{name}]"));
            Row::new(model.model.to_string(), provider.unwrap_or_default())
                .mark(Some(model.model.as_str()) == current)
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
        .status(plural(app.models.len() as u64, "model"))
        .search(filter)
        .rows(rows)
        .selected(Some(selected))
        .notes(notes)
}

/// The provider part of a `provider/model` reference.
fn provider(model: &ModelDescription) -> Option<&str> {
    model.model.as_str().split_once('/').map(|(name, _)| name)
}

/// The full reference and the context window, as the model note shows them.
pub fn model_details(model: &ModelDescription) -> String {
    format!(
        "{} · {} context",
        model.model,
        super::format_tokens(model.context_window)
    )
}

pub fn reasoning_label(level: ReasoningPreference) -> &'static str {
    match level {
        ReasoningPreference::Auto => "auto",
        ReasoningPreference::Disabled => "disabled",
        ReasoningPreference::Low => "low",
        ReasoningPreference::Medium => "medium",
        ReasoningPreference::High => "high",
        ReasoningPreference::XHigh => "xhigh",
        ReasoningPreference::Max => "max",
        ReasoningPreference::Ultra => "ultra",
    }
}

pub fn agent_label(app: &App, id: &minicore_host_protocol::AgentId) -> String {
    let Some(agent) = app.agents.iter().find(|agent| &agent.id == id) else {
        return short_session_id(id.as_str()).to_owned();
    };
    if app
        .agents
        .iter()
        .filter(|other| other.name == agent.name)
        .count()
        > 1
    {
        format!("{} ({})", agent.name, short_session_id(id.as_str()))
    } else {
        agent.name.clone()
    }
}

fn agents_sheet(app: &App, filter: &str, selected: usize, styles: Styles) -> Sheet {
    let agents = overlay::matching_agents(&app.agents, filter);
    let rows = agents
        .iter()
        .map(|agent| {
            let state = match agent.lifecycle {
                AgentLifecycle::Creating => "creating",
                AgentLifecycle::Active => "active",
                AgentLifecycle::Stopping => "stopping",
                AgentLifecycle::Stopped => "stopped",
                AgentLifecycle::Retiring => "retiring",
                AgentLifecycle::Retired => "retired",
            };
            Row::new(
                agent_label(app, &agent.id),
                format!(
                    "{state} · {} · {}",
                    storage_label(agent.mode),
                    plural(agent.sessions.len() as u64, "session")
                ),
            )
            .mark(app.current_agent_id() == Some(&agent.id))
        })
        .collect();
    let notes = agents
        .get(selected)
        .map(|agent| {
            let mut notes = vec![
                Line::styled(format!("  Agent {}", agent.id), styles.dim()),
                Line::styled(
                    format!(
                        "  Default session: {}",
                        agent
                            .default_session_id
                            .as_ref()
                            .map_or_else(|| "none".to_owned(), ToString::to_string)
                    ),
                    styles.dim(),
                ),
            ];
            if agent.metadata_saving {
                notes.push(Line::styled("  Saving Agent metadata", styles.dim()));
            }
            if let Some(error) = &agent.metadata_fault {
                notes.push(Line::styled(
                    format!("  Agent metadata save failed: {error}"),
                    styles.failure(),
                ));
            }
            if let Some(deletion) = &agent.deletion {
                use minicore_host_protocol::DataDeletion;
                let (text, style) = match deletion {
                    DataDeletion::Pending {} => ("  Deleting Agent data".into(), styles.dim()),
                    DataDeletion::Deleted {} => ("  Agent data deleted".into(), styles.dim()),
                    DataDeletion::Reclaimed {} => ("  Agent memory reclaimed".into(), styles.dim()),
                    DataDeletion::NotAttempted {} => {
                        ("  Agent data deletion not attempted".into(), styles.dim())
                    }
                    DataDeletion::Failed { error } => (
                        format!("  Agent data deletion failed: {error}"),
                        styles.failure(),
                    ),
                };
                notes.push(Line::styled(text, style));
            }
            notes
        })
        .unwrap_or_else(|| vec![Line::styled("  No matching agents", styles.dim())]);
    Sheet::new("Select Agent", "enter select · ctrl+d delete · esc close")
        .search(filter)
        .rows(rows)
        .selected(Some(selected))
        .notes(notes)
        .status(plural(agents.len() as u64, "agent"))
}

fn storage_label(mode: StorageMode) -> &'static str {
    match mode {
        StorageMode::Memory => "memory",
        StorageMode::Persistent => "persistent",
    }
}

fn reasoning_sheet(app: &App, selected: usize) -> Sheet {
    let current = app.reasoning();
    let rows = app
        .reasoning_levels()
        .iter()
        .map(|level| {
            Row::new(reasoning_label(*level), reasoning_description(*level))
                .mark(Some(*level) == current)
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

fn reasoning_description(level: ReasoningPreference) -> &'static str {
    match level {
        ReasoningPreference::Auto => "Model default",
        ReasoningPreference::Disabled => "No reasoning",
        ReasoningPreference::Low => "Light reasoning",
        ReasoningPreference::Medium => "Moderate reasoning",
        ReasoningPreference::High => "Deep reasoning",
        ReasoningPreference::XHigh => "Extra-high reasoning",
        ReasoningPreference::Max => "Maximum reasoning",
        ReasoningPreference::Ultra => "Beyond maximum reasoning",
    }
}

fn interaction_sheet(app: &App, styles: Styles) -> Option<Sheet> {
    let session = app.resident.current()?;
    let interaction = session.interaction.as_ref()?;
    let (prompt, options, approval) = match interaction.request() {
        InteractionRequestData::Approval { prompt, .. } => (
            prompt.as_str(),
            vec!["Allow once".to_owned(), "Deny".to_owned()],
            true,
        ),
        InteractionRequestData::ToolInput { request } => {
            if request.answer_kind == ToolInputAnswerKind::Text {
                return None;
            }
            (
                request.prompt.as_str(),
                request
                    .choices
                    .iter()
                    .map(|choice| choice.as_str().to_owned())
                    .collect(),
                false,
            )
        }
    };
    let call = session.transcript.tool_calls().find(|call| {
        call.key.turn_id() == Some(&interaction.turn_id)
            && call.key.tool_call_id() == interaction.tool_call_id()
    });
    let subject = Subject {
        tool: call.map_or(interaction.tool_name(), |call| call.name.as_str()),
        detail: call.map_or("", |call| call.detail.as_str()),
        input: call.and_then(|call| call.expanded_input.as_deref()),
    };
    let mut sheet = approval_sheet(
        prompt,
        &subject,
        &options,
        approval,
        interaction.selected,
        styles,
    );
    if let InteractionRequestData::Approval { risk, .. } = interaction.request() {
        let level = match risk {
            minicore_host_core::ApprovalRiskData::Low => "low",
            minicore_host_core::ApprovalRiskData::Medium => "medium",
            minicore_host_core::ApprovalRiskData::High => "high",
        };
        sheet.status = Some(format!("Risk: {level}"));
    }
    if interaction.submitted {
        sheet.hint = "Answer submitted · waiting for Host confirmation".into();
    }
    Some(sheet)
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
/// The same for the diff of an `edit` or a patch.
const DIFF_PREVIEW: usize = 12;

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
        let diff = matches!(subject.tool, "edit" | "apply_patch" | "patch");
        let lines: Vec<&str> = input
            .lines()
            .filter(|line| !diff || !is_diff_header(line))
            .collect();
        let cap = if diff { DIFF_PREVIEW } else { INPUT_PREVIEW };
        for line in lines.iter().take(cap) {
            let style = if diff {
                diff_style(line, styles)
            } else {
                styles.dim()
            };
            body.push(Line::styled(format!("  {}", truncate(line, 100)), style));
        }
        if lines.len() > cap {
            body.push(Line::styled(
                format!("  … {} more lines", lines.len() - cap),
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
                1 => Row::new(option.clone(), "n").tone(Tone::Failure),
                _ => Row::new(option.clone(), ""),
            }
        })
        .collect();
    let (title, hint) = if approval {
        ("Approval", "enter confirm · y allow · n deny · esc deny")
    } else {
        ("Question", "enter choose · esc cancel turn")
    };
    Sheet::new(title, hint)
        .kind(Kind::Column)
        .body(body)
        .rows(rows)
        .selected(Some(selected))
}

/// The envelope lines of an `edit` diff or a patch, which say nothing the card does not.
fn is_diff_header(line: &str) -> bool {
    [
        "--- before",
        "+++ after",
        "--- a/",
        "+++ b/",
        "@@",
        "*** Begin Patch",
        "*** End Patch",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn diff_style(line: &str, styles: Styles) -> ratatui::style::Style {
    match line.as_bytes().first() {
        Some(b'+') => styles.success(),
        Some(b'-') => styles.failure(),
        _ => styles.dim(),
    }
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

/// A live session's state, first on its picker row.
fn live_state(session: &Session) -> Option<String> {
    Some(match session.live_status() {
        LiveStatus::Running => {
            let started = session.live.as_ref()?.started;
            format!("running {}", super::status::minutes(started.elapsed()))
        }
        LiveStatus::Approval => match session.interaction.as_ref()?.request() {
            InteractionRequestData::Approval { .. } => "needs approval",
            InteractionRequestData::ToolInput { .. } => "needs input",
        }
        .to_owned(),
        LiveStatus::Compacting => "compacting".to_owned(),
        LiveStatus::Idle if failed_unseen(session) => "failed".to_owned(),
        LiveStatus::Idle if session.unseen => "new".to_owned(),
        LiveStatus::Idle => return None,
    })
}

/// A background turn ended in failure and the session was not opened since.
fn failed_unseen(session: &Session) -> bool {
    session.unseen
        && matches!(
            session.transcript.blocks().last(),
            Some(Block::TurnEnd {
                outcome: TurnOutcome::Failed,
                ..
            })
        )
}

fn catalog_state(info: &SessionSummary) -> Option<String> {
    Some(if info.lifecycle != SessionLifecycle::Open {
        match info.lifecycle {
            SessionLifecycle::Closed => "closed",
            _ => "closing",
        }
        .to_owned()
    } else if info.pending_interactions > 0 {
        "needs input".to_owned()
    } else if info.active_turn_id.is_some() {
        "running".to_owned()
    } else if info.block_reason.is_some() {
        "blocked".to_owned()
    } else if info.scheduling == Scheduling::Paused {
        "paused".to_owned()
    } else if info.residency != Residency::Loaded {
        match info.residency {
            Residency::Unloaded => "unloaded",
            Residency::Loading => "loading",
            _ => "unloading",
        }
        .to_owned()
    } else {
        return None;
    })
}

/// What sets one sessions-picker row apart from the others.
struct RowContext {
    agent: Option<String>,
    /// What a live session is doing.
    state: Option<String>,
    current: bool,
    other_workspace: bool,
    /// Another listed session has the same label.
    duplicate: bool,
}

/// One sessions-picker row: `label` (the title, or the first prompt), with
/// what tells the session apart on the right, then its age. The current
/// session says so, one in another workspace names it, and a label shared
/// with another session gets its short id.
fn session_row(
    info: &SessionSummary,
    label: String,
    context: &RowContext,
    now_seconds: i64,
) -> Row {
    let mut parts: Vec<String> = context
        .agent
        .iter()
        .chain(&context.state)
        .cloned()
        .collect();
    if context.current {
        parts.push("current".to_owned());
    }
    if context.other_workspace
        && let Some(name) = workspace_name(info)
    {
        parts.push(name.into_owned());
    }
    if context.duplicate {
        parts.push(short_session_id(info.id.as_str()).to_owned());
    }
    let age = age_label(info.last_activity_at, now_seconds);
    if !age.is_empty() {
        parts.push(age);
    }
    Row::new(label, parts.join(" · ")).mark(context.current)
}

fn context_sheet(app: &App, styles: Styles) -> Sheet {
    let lines = crate::ui::views::context::lines(app.resident.current(), styles);
    Sheet::new("Context", "r refresh · c copy · esc close").body(lines)
}

/// `/session`: the facts about the foreground session; Enter or `c` copies its id.
fn session_info_sheet(app: &App, styles: Styles) -> Sheet {
    let sheet = Sheet::new("Session", "enter copy id · c copy id · esc close");
    let Some(session) = app.resident.current() else {
        return sheet.body(vec![Line::styled("  No session is open", styles.dim())]);
    };
    let info = &session.info;
    let tokens = session
        .context
        .as_ref()
        .and_then(|context| context.data.history.as_ref())
        .and_then(|history| history.latest_response.as_ref())
        .map_or_else(
            || "not reported".to_owned(),
            |response| super::views::context::usage_text(&response.usage),
        );
    let facts = vec![
        ("id", info.id.to_string()),
        (
            "agent",
            format!("{} · {}", agent_label(app, &info.agent_id), info.agent_id),
        ),
        (
            "title",
            info.title
                .clone()
                .unwrap_or_else(|| "Untitled session".into()),
        ),
        (
            "workspace",
            info.config
                .workspace
                .as_ref()
                .map_or_else(|| "none".into(), |workspace| workspace.path.clone()),
        ),
        ("storage", storage_label(info.mode).to_owned()),
        (
            "lifecycle",
            format!("{:?}", session.state.data.lifecycle).to_lowercase(),
        ),
        (
            "residency",
            format!("{:?}", session.state.data.residency).to_lowercase(),
        ),
        (
            "scheduling",
            format!("{:?}", session.state.data.scheduling).to_lowercase(),
        ),
        (
            "approval",
            match info.config.execution.approval {
                minicore_host_protocol::ApprovalMode::Ask => "ask",
                minicore_host_protocol::ApprovalMode::Auto => "auto",
                minicore_host_protocol::ApprovalMode::ReadOnly => "read_only",
            }
            .to_owned(),
        ),
        (
            "model",
            format!(
                "{} · {}",
                info.config.model,
                reasoning_label(info.config.reasoning)
            ),
        ),
        (
            "items",
            session
                .context
                .as_ref()
                .and_then(|context| context.data.history.as_ref())
                .map(|history| history.raw_items.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
        ),
        ("last usage", tokens),
    ];
    let body = facts
        .into_iter()
        .map(|(name, value)| {
            Line::from(vec![
                Span::styled(format!("{name:<10}"), styles.dim()),
                Span::styled(truncate(&value, 120), styles.text()),
            ])
        })
        .collect();
    sheet.body(body)
}

/// `now`, `5m`, `3h`, `2d`, `1w`, `4mo` or `2y` from the Host timestamp.
fn age_label(updated_at: Timestamp, now: i64) -> String {
    let seconds = now.saturating_sub(super::blocks::timestamp(updated_at).as_second());
    match seconds {
        ..=59 => "now".into(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        86_400..=604_799 => format!("{}d", seconds / 86_400),
        604_800..=2_591_999 => format!("{}w", seconds / 604_800),
        2_592_000..=31_535_999 => format!("{}mo", seconds / 2_592_000),
        _ => format!("{}y", seconds / 31_536_000),
    }
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

    fn info() -> SessionSummary {
        let fixture = crate::ui::snapshots::app();
        let mut info = crate::ui::snapshots::catalog(fixture.resident.current().unwrap());
        info.id = "1acdef0123456789abcd".parse().unwrap();
        info.title = Some("Fix the parser".into());
        info.workspace = None;
        info.last_activity_at = stamp("2026-01-02T03:04:05.006Z");
        info
    }
    fn stamp(value: &str) -> Timestamp {
        serde_json::from_value(json!(value)).unwrap()
    }
    fn now() -> i64 {
        super::super::blocks::timestamp(stamp("2026-01-02T06:04:05.006Z")).as_second()
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

    fn context(current: bool, other_workspace: bool, duplicate: bool) -> RowContext {
        RowContext {
            agent: None,
            state: None,
            current,
            other_workspace,
            duplicate,
        }
    }

    #[test]
    fn a_session_row_says_if_it_is_current_and_shows_its_age_not_the_model() {
        let row = session_row(
            &info(),
            "Fix the parser".into(),
            &context(true, false, false),
            now(),
        );
        assert_eq!(row.label, "Fix the parser");
        assert_eq!(row.mark, Some(true));
        assert_eq!(row.detail, "current · 3h");
        let row = session_row(&info(), "x".into(), &context(false, false, false), now());
        assert_eq!(row.mark, Some(false));
        assert_eq!(row.detail, "3h");
    }

    #[test]
    fn rows_that_look_alike_get_a_short_id_and_other_workspaces_are_named() {
        let mut other = info();
        other.workspace = Some(serde_json::from_value(json!({"path":"/work/stockroom"})).unwrap());
        let row = session_row(&other, "Same".into(), &context(false, true, true), now());
        assert_eq!(row.detail, "stockroom · 1acdef · 3h");
    }

    #[test]
    fn ages_cover_every_unit() {
        let stamp = stamp("2026-01-02T03:04:05Z");
        let base = super::super::blocks::timestamp(stamp).as_second();
        let age = |seconds| age_label(stamp, base + seconds);
        assert_eq!(age(30), "now");
        assert_eq!(age(600), "10m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(172_800), "2d");
        assert_eq!(age(14 * 86_400), "2w");
        assert_eq!(age(100 * 86_400), "3mo");
        assert_eq!(age(800 * 86_400), "2y");
    }

    #[test]
    fn a_model_shows_its_provider_and_declared_window() {
        let model: ModelDescription = serde_json::from_value(json!({
            "model":"cus-oai/gpt-6-luna","api":"responses","context_window":400_000,"supported_reasoning":["low"],"supports_tools":true
        })).unwrap();
        assert_eq!(provider(&model), Some("cus-oai"));
        assert_eq!(model_details(&model), "cus-oai/gpt-6-luna · 400k context");
        let bare: ModelDescription = serde_json::from_value(json!({"model":"x","api":"responses","context_window":32000,"supported_reasoning":[],"supports_tools":true})).unwrap();
        assert_eq!(provider(&bare), None);
        assert_eq!(model_details(&bare), "x · 32k context");
    }
}
