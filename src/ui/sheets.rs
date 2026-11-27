//! Builders for the selectors and the inline completion list: the sessions,
//! model and reasoning pickers, the approval prompt and the find bar each turn
//! app state into one [`Sheet`]; the command and `@` file lists are plain rows.

use ratatui::text::{Line, Span};

use crate::app::commands::COMMANDS;
use crate::app::overlay::{self, matching_models, session_label, short_session_id, workspace_name};
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

/// Rows of the completion list that show at once.
const COMPLETION_ROWS: usize = 8;

/// The sheet for the current overlay; a pending interaction is one too, as it owns the keyboard.
pub fn sheet(app: &App, styles: Styles) -> Option<Sheet> {
    Some(match &app.overlay {
        Overlay::Confirm { title, lines, .. } => Sheet::new(title, "y confirm · n/esc cancel")
            .notes(
                lines
                    .iter()
                    .map(|line| Line::styled(format!("  {line}"), styles.text()))
                    .collect(),
            ),
        Overlay::Unconfirmed { mode, .. } => {
            use crate::app::unconfirmed::ReviewMode;
            if matches!(mode, ReviewMode::Edit(_)) {
                return None;
            }
            if app.local_inputs.is_empty() {
                return Some(
                    Sheet::new("Local input confirmations", "esc close").notes(vec![Line::styled(
                        "  No unconfirmed input: every send was accepted",
                        styles.dim(),
                    )]),
                );
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
            renaming,
        } => {
            let state = SessionsState {
                filter,
                selected: *selected,
                renaming: renaming.as_ref(),
            };
            sessions_sheet(app, rows, &state, styles)
        }
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
        // Inline completion popups float above the composer; full-screen views own main area.
        Overlay::Palette { .. }
        | Overlay::Files { .. }
        | Overlay::Delegations { .. }
        | Overlay::Context { .. }
        | Overlay::SessionInfo { .. }
        | Overlay::Help { .. }
        | Overlay::Error { .. }
        | Overlay::Remote { .. }
        | Overlay::Diff(_) => return None,
    })
}

/// Hotkey badges shown at the right edge of high-frequency slash commands.
fn command_hotkey(name: &str) -> Option<&'static str> {
    match name {
        "tasks" => Some("ctrl+b"),
        "sessions" => Some("ctrl+r"),
        "model" => Some("ctrl+l"),
        "reasoning" => Some("shift+tab"),
        "help" => Some("f1"),
        "unconfirmed" => Some("f8"),
        _ => None,
    }
}

/// Builds the lightweight semi-popup [`Sheet`] for `/` commands and `@` file
/// mentions, rendered directly above the pinned composer.
pub fn completion_sheet(app: &App, styles: Styles) -> Option<Sheet> {
    match &app.overlay {
        Overlay::Palette { selected } => {
            if app.template_query().is_some() {
                let rows: Vec<Row> = app
                    .palette_rows()
                    .into_iter()
                    .map(|(name, detail)| Row::new(name, detail))
                    .collect();
                let sheet =
                    Sheet::new("Prompt templates", "↑↓ select · tab/enter fill · esc close")
                        .kind(Kind::Column)
                        .max_rows(COMPLETION_ROWS)
                        .status(plural(rows.len() as u64, "template"));
                return Some(if rows.is_empty() {
                    sheet.notes(vec![Line::styled("  No matching templates", styles.dim())])
                } else {
                    sheet.rows(rows).selected(Some(*selected))
                });
            }
            if let Some((cmd, _)) = app.palette_arg() {
                let current = app.current_value(cmd);
                let items = app.palette_rows();
                let rows: Vec<Row> = items
                    .into_iter()
                    .map(|(value, detail)| {
                        let is_current = current.as_deref() == Some(value.as_str());
                        let clean_detail = detail
                            .strip_prefix("✓ current · ")
                            .or_else(|| detail.strip_prefix("✓ current"))
                            .unwrap_or(&detail)
                            .to_owned();
                        Row::new(value, clean_detail).mark(is_current)
                    })
                    .collect();
                let mut sheet = Sheet::new(
                    format!("/{cmd}"),
                    "↑↓ select · tab fill · enter run · pgup/pgdn · esc close",
                )
                .kind(Kind::Column)
                .max_rows(COMPLETION_ROWS)
                .status(plural(rows.len() as u64, "choice"));
                if rows.is_empty() {
                    sheet = sheet.notes(vec![Line::styled("  No matching value", styles.dim())]);
                } else {
                    sheet = sheet.rows(rows).selected(Some(*selected));
                }
                return Some(sheet);
            }
            let query = app.palette_query()?;
            let matched = crate::app::commands::filter(query, &app.templates);
            let total = COMMANDS.len()
                + if query.is_empty() {
                    0
                } else {
                    app.templates
                        .iter()
                        .filter(|template| !crate::app::commands::is_builtin(&template.name))
                        .count()
                };
            let rows: Vec<Row> = matched
                .into_iter()
                .map(|command| {
                    let badge = match (
                        app.current_value(command.name),
                        command_hotkey(command.name),
                    ) {
                        (Some(val), Some(key)) => format!("● {val} · {key}"),
                        (Some(val), None) => format!("● {val}"),
                        (None, Some(key)) => key.to_owned(),
                        (None, None) => String::new(),
                    };
                    Row::new(format!("/{}", command.name), command.description)
                        .args(command.args)
                        .badge(badge)
                })
                .collect();
            let status = if rows.len() == total {
                plural(total as u64, "command")
            } else {
                format!("{} of {total}", rows.len())
            };
            let mut sheet = Sheet::new(
                "Commands",
                "↑↓ select · tab complete · enter run · pgup/pgdn · esc close",
            )
            .kind(Kind::Column)
            .max_rows(COMPLETION_ROWS)
            .status(status);
            if rows.is_empty() {
                sheet = sheet.notes(vec![Line::styled(
                    "  No matching command · enter sends as prompt",
                    styles.dim(),
                )]);
            } else {
                sheet = sheet.rows(rows).selected(Some(*selected));
            }
            Some(sheet)
        }
        Overlay::Files { selected, paths } => {
            let query = app.mention_query()?;
            let shown = overlay::matching_paths(paths, query);
            let rows: Vec<Row> = shown
                .iter()
                .map(|path| Row::new(overlay::file_name(path).to_owned(), (*path).clone()))
                .collect();
            let title = if query.is_empty() {
                "Files".to_owned()
            } else {
                format!("Files · @{query}")
            };
            let status = if rows.len() == paths.len() {
                plural(paths.len() as u64, "file")
            } else {
                format!("{} of {}", rows.len(), paths.len())
            };
            let mut sheet = Sheet::new(
                title,
                "↑↓ select · tab/enter insert · pgup/pgdn · esc close",
            )
            .kind(Kind::Column)
            .max_rows(COMPLETION_ROWS)
            .status(status);
            if rows.is_empty() {
                sheet = sheet.notes(vec![Line::styled("  No matching files", styles.dim())]);
            } else {
                sheet = sheet.rows(rows).selected(Some(*selected));
            }
            Some(sheet)
        }
        _ => None,
    }
}

/// The sessions picker's own state, as the sheet shows it.
struct SessionsState<'a> {
    filter: &'a str,
    selected: usize,
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
        renaming,
    } = *state;
    let foreground = app.resident.current().map(Session::address);
    let now = jiff::Timestamp::now().as_second();
    if let Some((target, title)) = renaming {
        let name = rows
            .iter()
            .find(|info| overlay::session_target(info) == *target)
            .map(session_label)
            .unwrap_or_else(|| short_session_id(target.session_id.as_str()));
        return Sheet::new("Rename Session", "enter save · esc cancel")
            .status(name)
            .search(title);
    }
    let shown = overlay::matching_sessions(rows, filter);
    let workspace = app.workspace.to_string_lossy();
    let status = if shown.len() == rows.len() {
        plural(rows.len() as u64, "session")
    } else {
        format!("{} of {}", shown.len(), rows.len())
    };
    let list = shown
        .iter()
        .map(|info| {
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
            };
            let row = session_row(info, &context, now);
            if live.is_some_and(failed_unseen) {
                row.tone(Tone::Failure)
            } else {
                row
            }
        })
        .collect::<Vec<_>>();
    let hint = if filter.is_empty() {
        "enter open · ctrl+n rename · ctrl+d delete · esc close"
    } else {
        "enter open · ctrl+n rename · ctrl+d delete · ctrl+u clear · esc close"
    };
    let notes = match shown.get(selected) {
        None => vec![Line::styled("  No sessions found", styles.dim())],
        Some(info) => session_preview(app, info, styles),
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
                format!(
                    "  {} context window",
                    super::format_tokens(model.context_window)
                ),
                styles.dim(),
            ),
        ],
        None => vec![Line::styled("  No matching models", styles.dim())],
    };
    let hint = if filter.is_empty() {
        "enter select · esc close"
    } else {
        "enter select · ctrl+u clear · esc close"
    };
    Sheet::new("Select Model", hint)
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
    let hint = if filter.is_empty() {
        "enter select · ctrl+d delete · esc close"
    } else {
        "enter select · ctrl+d delete · ctrl+u clear · esc close"
    };
    Sheet::new("Select Agent", hint)
        .search(filter)
        .rows(rows)
        .selected(Some(selected))
        .notes(notes)
        .status(plural(agents.len() as u64, "agent"))
}

pub(crate) fn storage_label(mode: StorageMode) -> &'static str {
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
        "enter select · shift+tab cycles · esc close",
    )
    .kind(Kind::Column)
    .rows(rows)
    .selected(Some(selected))
}

pub fn reasoning_description(level: ReasoningPreference) -> &'static str {
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
        input: call.and_then(|call| call.expanded_input()),
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
}

/// Keep the short ID first in the detail column so clipped titles cannot hide it.
fn session_row(info: &SessionSummary, context: &RowContext, now_seconds: i64) -> Row {
    // An untitled row's label already is its short ID.
    let titled = session_label(info) != short_session_id(info.id.as_str());
    let mut parts: Vec<String> = titled
        .then(|| short_session_id(info.id.as_str()).to_owned())
        .into_iter()
        .collect();
    parts.extend(context.agent.iter().chain(&context.state).cloned());
    if context.other_workspace
        && let Some(name) = workspace_name(info)
    {
        parts.push(name.into_owned());
    }
    let age = age_label(info.last_activity_at, now_seconds);
    if !age.is_empty() {
        parts.push(age);
    }
    Row::new(session_label(info), parts.join(" · ")).mark(context.current)
}

/// The selected session's details under the list: where it works, what runs
/// it and, when its history is loaded, the request it started with.
fn session_preview(app: &App, info: &SessionSummary, styles: Styles) -> Vec<Line<'static>> {
    let mut facts = Vec::new();
    if let Some(workspace) = &info.workspace {
        facts.push(
            super::transcript::home_relative(std::path::Path::new(&workspace.path))
                .display()
                .to_string(),
        );
    }
    facts.push(format!(
        "{} · {}",
        info.model,
        reasoning_label(info.reasoning)
    ));
    let mut lines = vec![
        Line::default(),
        Line::styled(format!("  {}", facts.join(" · ")), styles.meta()),
    ];
    let first = app
        .resident
        .find(&overlay::session_target(info))
        .and_then(|session| {
            session
                .transcript
                .blocks()
                .iter()
                .find_map(|block| match block {
                    Block::User { text, .. } => Some(text.clone()),
                    _ => None,
                })
        });
    if let Some(first) = first {
        let text = crate::wrap::sanitize(&first.replace('\n', " ")).into_owned();
        lines.push(Line::styled(
            format!("  › {}", truncate(&text, 110)),
            styles.dim(),
        ));
    }
    lines
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

    #[test]
    fn the_slash_palette_builds_a_compact_sheet_with_hotkeys_and_empty_note() {
        let mut app = crate::ui::snapshots::app();
        app.composer.set_text("/tas");
        app.overlay = Overlay::Palette { selected: 0 };
        let sheet = completion_sheet(&app, styles()).expect("palette sheet");
        assert_eq!(sheet.title, "Commands");
        assert_eq!(sheet.max_rows, COMPLETION_ROWS);
        assert!(
            sheet
                .rows
                .iter()
                .any(|row| row.label == "/tasks" && row.badge == "ctrl+b")
        );

        app.composer.set_text("/zzzz-no-such-cmd");
        let empty = completion_sheet(&app, styles()).expect("empty palette sheet");
        assert!(empty.rows.is_empty());
        assert!(
            empty
                .notes
                .iter()
                .map(text)
                .any(|row| row.contains("No matching command"))
        );
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

    fn context(current: bool, other_workspace: bool) -> RowContext {
        RowContext {
            agent: None,
            state: None,
            current,
            other_workspace,
        }
    }

    #[test]
    fn a_session_row_marks_the_current_one_and_shows_its_age_not_the_model() {
        let row = session_row(&info(), &context(true, false), now());
        assert_eq!(row.label, "Fix the parser");
        // The same check mark as every picker, not a word.
        assert_eq!(row.mark, Some(true));
        assert_eq!(row.detail, "1acdef · 3h");
        let row = session_row(&info(), &context(false, false), now());
        assert_eq!(row.mark, Some(false));
        assert_eq!(row.detail, "1acdef · 3h");
    }

    #[test]
    fn a_session_id_precedes_agent_state_and_workspace_details() {
        let mut other = info();
        other.workspace = Some(serde_json::from_value(json!({"path":"/work/stockroom"})).unwrap());
        let mut context = context(false, true);
        context.agent = Some("Coder".into());
        context.state = Some("running 0m".into());
        let row = session_row(&other, &context, now());
        assert_eq!(row.detail, "1acdef · Coder · running 0m · stockroom · 3h");
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
