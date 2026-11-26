//! The `Ctrl+B` / `/tasks` async tasks workspace: a searchable task list beside
//! the real, interactive child conversation (`Working`/`Worked`, folding,
//! scrolling, and direct child messaging from the bottom composer), built on
//! the shared [`super::split`] two-pane engine.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::overlay::{AsyncTaskItem, TaskFocus, async_tasks, matching_async_tasks};
use crate::app::{App, Overlay};
use crate::transcript::layout::Fold;
use crate::ui::chrome;
use crate::ui::panel::truncate;
use crate::ui::theme::Styles;
use crate::wrap;

use super::chrome as draw_chrome;
use super::split;
pub use super::split::MIN_SPLIT_WIDTH;

/// Width in cells of the searchable task list beside the child conversation.
const LIST_COLUMN: u16 = 38;
pub const ITEM_HEIGHT: usize = 2;

/// Renders the `Ctrl+B` Async Tasks workspace directly from `&mut App` so its
/// right pane updates the incremental child transcript cache and hit geometry.
pub fn render(frame: &mut Frame, area: Rect, styles: Styles, app: &mut App) {
    let Overlay::Delegations {
        filter,
        selected,
        focus,
        detail_open,
    } = &app.overlay
    else {
        return;
    };
    let filter = filter.clone();
    let selected = *selected;
    let focus = *focus;
    let detail_open = *detail_open;

    app.task_inspector.hit.reset(ITEM_HEIGHT);

    let (
        title,
        list_rows,
        total,
        needs_more,
        header_line,
        target_ref,
        needs_input_banner,
        fallback_lines,
    ) = {
        let all = async_tasks(app);
        let matched = matching_async_tasks(&all, &filter);
        let needs_input = all.iter().filter(|item| item.needs_input()).count();
        let running = all
            .iter()
            .filter(|item| item.is_running() && !item.needs_input())
            .count();
        let failed = all.iter().filter(|item| item.is_failed()).count();
        let completed = all.iter().filter(|item| item.is_completed()).count();

        let mut summary = Vec::new();
        if !filter.trim().is_empty() {
            summary.push(format!("showing {}/{}", matched.len(), all.len()));
        }
        if needs_input > 0 {
            summary.push(format!("{needs_input} needs input"));
        }
        if running > 0 {
            summary.push(format!("{running} running"));
        }
        if failed > 0 {
            summary.push(format!("{failed} failed"));
        }
        if completed > 0 || summary.is_empty() {
            summary.push(if all.is_empty() {
                "0 tasks".to_owned()
            } else {
                format!("{completed} completed")
            });
        }
        let title = format!("Async Tasks · {}", summary.join(" · "));

        let root_ref = app.resident.current().map(|session| session.address());
        let task_names: std::collections::HashMap<String, String> = all
            .iter()
            .filter_map(|item| {
                item.task_name()
                    .map(|name| (item.session_id().to_owned(), name.to_owned()))
            })
            .collect();

        let clamped = selected.min(matched.len().saturating_sub(1));
        let current = matched.get(clamped).copied();

        let rows: Vec<ListRow> = matched
            .iter()
            .map(|item| {
                let (glyph, glyph_style) = item.glyph(styles);
                let mut subtitle = item.subtitle();
                if let Some(by) = item.dispatched_by(root_ref.as_ref(), &task_names) {
                    subtitle = format!("{subtitle} · {by}");
                }
                if item.is_running()
                    && let Some((target, false)) = item.open_target()
                    && let Some(session) = app.resident.find(&target)
                    && let Some(live) = &session.live
                {
                    let elapsed = crate::ui::status::minutes(live.started.elapsed());
                    if !elapsed.is_empty() {
                        subtitle = format!("{elapsed} · {subtitle}");
                    }
                }
                ListRow {
                    glyph,
                    glyph_style,
                    kind: item.kind_label(),
                    title: item.title(),
                    short_id: item.short_id().to_owned(),
                    subtitle,
                    needs_input: item.needs_input(),
                    is_failed: item.is_failed(),
                }
            })
            .collect();

        let has_more = app.resident.current().is_some_and(|session| {
            session
                .history
                .as_ref()
                .is_some_and(crate::history::HistoryWindow::has_more)
        });

        let header_line =
            current.map(|item| detail_header(item, root_ref.as_ref(), &task_names, focus, styles));
        let target_ref = current.and_then(|item| item.open_target());
        let needs_input_banner = current.is_some_and(|item| item.needs_input());
        let fallback_lines = current.and_then(|item| match item {
            AsyncTaskItem::StandaloneDelegated { summary, .. } => {
                Some(crate::ui::delegated::detail_lines(
                    summary,
                    styles,
                    usize::from(area.width)
                        .saturating_sub(LIST_COLUMN as usize + 4)
                        .max(20),
                ))
            }
            AsyncTaskItem::Agent(_) => None,
        });

        (
            title,
            rows,
            all.len(),
            has_more,
            header_line,
            target_ref,
            needs_input_banner,
            fallback_lines,
        )
    };

    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    let hints: Vec<(&str, &str)> = match focus {
        TaskFocus::Composer => vec![
            ("enter", "send"),
            ("alt+enter", "follow-up"),
            ("pgup/pgdn", "scroll"),
            ("ctrl+o", "fold"),
            ("esc", "list"),
        ],
        TaskFocus::Detail => vec![
            ("↑↓/jk", "scroll"),
            ("ctrl+o", "fold"),
            ("enter", "message"),
            ("ctrl+e", "full view"),
            ("ctrl+y", "copy"),
            ("←/esc", "list"),
        ],
        TaskFocus::List => {
            let mut h = vec![
                ("type", "filter"),
                ("↑↓", "select"),
                ("→/tab", "scroll"),
                ("enter", "message"),
                ("ctrl+e", "full view"),
                ("ctrl+g", "goto"),
                ("ctrl+y", "copy"),
            ];
            if !filter.is_empty() {
                h.push(("ctrl+u", "clear"));
            }
            h.push(("esc", "back"));
            h
        }
    };
    draw_chrome(frame, area, styles, &title, &hints);

    let panes = split::layout_panes(
        frame,
        body,
        LIST_COLUMN,
        detail_open || focus != TaskFocus::List,
        focus == TaskFocus::Detail,
        styles,
    );

    if let Some(list_area) = panes.list {
        app.task_inspector.hit.list_area = list_area;
        app.task_inspector.hit.list_first = render_list(
            frame,
            list_area,
            styles,
            &list_rows,
            &filter,
            selected,
            total,
            needs_more,
            focus == TaskFocus::List,
        );
    }

    let Some(detail_area) = panes.detail else {
        return;
    };
    app.task_inspector.hit.detail_area = detail_area;
    if detail_area.is_empty() {
        return;
    }

    let Some(header) = header_line else {
        let message = if !filter.trim().is_empty() {
            "No matching task selected."
        } else if needs_more {
            "No async task loaded yet. Press Esc and Home to load earlier history."
        } else {
            "No async task to inspect."
        };
        frame.buffer_mut().set_line(
            detail_area.x,
            detail_area.y,
            &chrome::styled(message, styles.dim()),
            detail_area.width,
        );
        return;
    };

    let mut transcript_area = split::draw_detail_header(
        frame,
        detail_area,
        &header,
        focus == TaskFocus::Detail,
        styles,
    );
    if transcript_area.is_empty() {
        return;
    }

    // Highlight pending approvals/questions prominently so stalled long tasks stand out.
    if needs_input_banner && transcript_area.height > 1 {
        let banner = Line::from(vec![
            Span::styled("⚠ ", styles.warning()),
            Span::styled(
                "Waiting for approval or input · press ctrl+e to open and answer",
                styles.warning(),
            ),
        ]);
        frame.buffer_mut().set_line(
            transcript_area.x,
            transcript_area.y,
            &banner,
            transcript_area.width,
        );
        transcript_area = Rect {
            y: transcript_area.y + 1,
            height: transcript_area.height.saturating_sub(1),
            ..transcript_area
        };
    }

    let fold = Fold {
        expanded: app.tools_expanded,
        show_reasoning: app.show_reasoning,
    };
    let elapsed = app.started.elapsed();
    let show_scrollbar = app.scrollbar_shown();

    // Prefer the real live resident child Session so Working/Worked, tool cards,
    // follow-up turns and full history render identically to the main conversation.
    if let Some((target, unavailable)) = &target_ref
        && !unavailable
        && let Some(session) = app.resident.find(target)
    {
        let transcript = &session.transcript;
        if transcript.blocks().is_empty() {
            frame.buffer_mut().set_line(
                transcript_area.x,
                transcript_area.y,
                &chrome::styled("Waiting for child output…", styles.dim()),
                transcript_area.width,
            );
            return;
        }
        crate::ui::transcript::draw_embedded(
            frame,
            transcript_area,
            styles,
            transcript,
            &mut app.task_inspector.layout,
            &mut app.task_inspector.scroll,
            app.task_inspector.held,
            fold,
            elapsed,
            show_scrollbar,
        );
        return;
    }

    // Fallback while background open settles (~5ms) or for released/unloaded tasks.
    if let Some((target, _)) = &target_ref
        && let Some(parent) = app.resident.current()
        && let Some(card) = parent
            .transcript
            .blocks()
            .iter()
            .rev()
            .find_map(|block| {
                let crate::transcript::Block::Tool(call) = block else {
                    return None;
                };
                call.agent_card
                    .as_deref()
                    .filter(|card| &card.key.child == target)
            })
            .or_else(|| {
                app.agent_cards
                    .cards_for_root(&parent.address())
                    .into_iter()
                    .find(|card| &card.key.child == target)
            })
    {
        if !card.transcript.blocks().is_empty() {
            crate::ui::transcript::draw_embedded(
                frame,
                transcript_area,
                styles,
                &card.transcript,
                &mut app.task_inspector.layout,
                &mut app.task_inspector.scroll,
                app.task_inspector.held,
                fold,
                elapsed,
                show_scrollbar,
            );
            return;
        }
        let lines = crate::ui::agent_cards::detail_lines(
            card,
            styles,
            usize::from(transcript_area.width),
            app.show_reasoning,
        );
        app.task_inspector.fallback_scroll = chrome::render_scrollable_lines(
            frame,
            transcript_area,
            &lines,
            app.task_inspector.fallback_scroll,
            styles,
        );
        return;
    }

    if let Some(lines) = fallback_lines {
        app.task_inspector.fallback_scroll = chrome::render_scrollable_lines(
            frame,
            transcript_area,
            &lines,
            app.task_inspector.fallback_scroll,
            styles,
        );
    }
}

fn detail_header(
    item: &AsyncTaskItem<'_>,
    root: Option<&minicore_host_protocol::SessionRef>,
    names: &std::collections::HashMap<String, String>,
    focus: TaskFocus,
    styles: Styles,
) -> Line<'static> {
    let (glyph, glyph_style) = item.glyph(styles);
    let focus_badge = match focus {
        TaskFocus::List => "",
        TaskFocus::Detail => " · scrolling",
        TaskFocus::Composer => " · messaging",
    };
    let status_style = if item.needs_input() {
        styles.warning()
    } else if item.is_failed() {
        styles.failure()
    } else if item.is_completed() {
        styles.success()
    } else {
        styles.accent()
    };
    let mut spans = vec![
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(format!("[{}] ", item.kind_label()), styles.meta()),
        Span::styled(item.short_id().to_owned(), styles.bold()),
    ];
    if let Some(name) = item.task_name() {
        spans.push(Span::styled(" · ", styles.meta()));
        spans.push(Span::styled(name.to_owned(), styles.bold()));
    }
    if let Some(by) = item.dispatched_by(root, names) {
        spans.push(Span::styled(" · ", styles.meta()));
        spans.push(Span::styled(by, styles.dim()));
    }
    spans.push(Span::styled(" · ", styles.meta()));
    spans.push(Span::styled(item.status_label(), status_style));
    spans.push(Span::styled(focus_badge.to_owned(), styles.accent()));
    Line::from(spans)
}

struct ListRow {
    glyph: &'static str,
    glyph_style: Style,
    kind: &'static str,
    title: String,
    short_id: String,
    subtitle: String,
    needs_input: bool,
    is_failed: bool,
}

#[allow(clippy::too_many_arguments)]
fn render_list(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    items: &[ListRow],
    filter: &str,
    selected: usize,
    total: usize,
    has_more: bool,
    focused: bool,
) -> usize {
    let placeholder = if focused {
        "type to filter tasks…"
    } else {
        "esc to search tasks…"
    };
    let list_body = chrome::draw_filter_header(frame, area, filter, placeholder, focused, styles);
    if list_body.is_empty() {
        return 0;
    }
    let buf = frame.buffer_mut();
    if items.is_empty() {
        let message = if total == 0 {
            if has_more {
                "No async tasks loaded · Home loads earlier history"
            } else {
                "No async tasks in this conversation"
            }
        } else {
            "No matching tasks · ctrl+u clears filter"
        };
        buf.set_line(
            list_body.x,
            list_body.y,
            &chrome::styled(
                &truncate(message, usize::from(list_body.width)),
                styles.dim(),
            ),
            list_body.width,
        );
        return 0;
    }

    let (first, visible_slots) = split::list_window(
        selected,
        items.len(),
        usize::from(list_body.height),
        ITEM_HEIGHT,
    );
    let selected = selected.min(items.len().saturating_sub(1));
    let width = usize::from(list_body.width);

    for (slot, (idx, item)) in items
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_slots)
        .enumerate()
    {
        let y = list_body.y + (slot * ITEM_HEIGHT) as u16;
        if y >= list_body.bottom() {
            break;
        }
        let is_selected = idx == selected;
        let row_h = (list_body.bottom() - y).min(ITEM_HEIGHT as u16);
        if is_selected {
            chrome::paint_highlight_band(
                buf,
                Rect {
                    x: list_body.x,
                    y,
                    width: list_body.width,
                    height: row_h,
                },
                styles,
            );
        }
        let prefix = if is_selected { "▎" } else { " " };
        let prefix_style = if is_selected {
            styles.accent()
        } else {
            Style::default()
        };
        let title_style = if is_selected {
            styles.bold()
        } else {
            styles.text()
        };
        let meta_style = if is_selected {
            styles.text()
        } else {
            styles.meta()
        };
        let id_style = if is_selected {
            styles.text()
        } else {
            styles.faint()
        };
        let id_width = wrap::width(&item.short_id);
        let fixed = 1 + 1 + wrap::width(item.glyph) + 1 + wrap::width(item.kind) + 3 + id_width + 1;
        let title_room = width.saturating_sub(fixed).max(4);
        let title = truncate(&item.title, title_room);
        let used =
            1 + 1 + wrap::width(item.glyph) + 1 + wrap::width(item.kind) + 3 + wrap::width(&title);
        let pad = width.saturating_sub(used + id_width);

        let line1 = Line::from(vec![
            Span::styled(prefix, prefix_style),
            Span::raw(" "),
            Span::styled(item.glyph, item.glyph_style),
            Span::raw(" "),
            Span::styled(format!("[{}] ", item.kind), meta_style),
            Span::styled(title, title_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(item.short_id.clone(), id_style),
        ]);
        buf.set_line(list_body.x, y, &line1, list_body.width);

        if y + 1 < list_body.bottom() {
            let status_style = if item.needs_input {
                styles.warning()
            } else if item.is_failed {
                styles.failure()
            } else if is_selected {
                styles.text()
            } else {
                styles.dim()
            };
            let sub = truncate(&item.subtitle, width.saturating_sub(4));
            let line2 = Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::raw("   "),
                Span::styled(sub, status_style),
            ]);
            buf.set_line(list_body.x, y + 1, &line2, list_body.width);
        }
    }
    first
}
