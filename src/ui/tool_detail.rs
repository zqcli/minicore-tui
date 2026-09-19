//! Pure rendering of the one tool detail. No reads, wrapping or byte decoding.
use crate::{app::App, protocol::ToolSubjectWire, safe_text::safe_display, theme::Theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

/// Exact title-only hit geometry, shared with drawing; no fold target changes.
pub fn detail_hits(
    prepared: &crate::state::view::PreparedConversation,
    area: Rect,
    offset: usize,
    visible: usize,
) -> Vec<(Rect, crate::state::tool::ToolKey)> {
    let mut hits = Vec::new();
    if area.width < 16 {
        return hits;
    }
    for local in 0..visible {
        let row = offset + local;
        let Some(section) = prepared.sections.at_row(row) else {
            continue;
        };
        let title_row = section.rows.start
            + usize::from(
                prepared
                    .row(section.rows.start)
                    .is_some_and(crate::ui::layout::line_is_blank),
            );
        if section.id.kind != crate::state::view::SectionKind::Tool || row != title_row {
            continue;
        }
        if let (Some(loop_id), Some(request_index), Some(call)) = (
            &section.id.loop_id,
            section.id.request_index,
            &section.id.tool_call_id,
        ) {
            hits.push((
                Rect::new(area.right().saturating_sub(9), area.y + local as u16, 6, 1),
                crate::state::tool::ToolKey::new(
                    &section.id.session_id,
                    loop_id,
                    request_index,
                    call,
                ),
            ));
        }
    }
    hits
}

pub fn body_area(area: Rect) -> Rect {
    let header = if area.width < 80 { 5 } else { 6 };
    Rect::new(
        area.x + 1,
        area.y + header.min(area.height),
        area.width.saturating_sub(2),
        area.height.saturating_sub(header),
    )
}
pub fn action_areas(area: Rect) -> (Rect, Rect) {
    (
        Rect::new(area.right().saturating_sub(15), area.y, 6, 1),
        Rect::new(area.right().saturating_sub(8), area.y, 6, 1),
    )
}
fn line(frame: &mut Frame, area: Rect, row: u16, text: String, theme: &Theme) {
    if row < area.height {
        frame.render_widget(
            Paragraph::new(safe_display(&text).into_owned()).style(Style::new().fg(theme.text)),
            Rect::new(area.x + 1, area.y + row, area.width.saturating_sub(2), 1),
        );
    }
}
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(detail) = app.tool_detail() else {
        return;
    };
    let facts = app.tool_facts();
    let execution = facts.and_then(|facts| facts.execution.as_deref());
    let invocation = facts.and_then(|facts| facts.invocation.as_deref());
    let name = execution
        .map(|execution| execution.name.as_str())
        .or_else(|| invocation.map(|inv| inv.name.as_str()))
        .unwrap_or("tool");
    line(
        frame,
        area,
        0,
        format!(
            "← 对话  {name} · request {}  {}",
            detail.key.request_index,
            execution.map_or("读取中".to_owned(), |execution| format!(
                "{:?} · recording {:?}",
                execution.state, execution.recording
            ))
        ),
        theme,
    );
    let (copy, refresh) = action_areas(area);
    for (hit, label) in [(copy, "[复制]"), (refresh, "[刷新]")] {
        crate::ui::layout::clear_wide_overlay_edges(frame.buffer_mut(), hit);
        frame.render_widget(
            Paragraph::new(label).style(Style::new().fg(theme.scrollbar_thumb).bg(theme.page_bg)),
            hit,
        );
    }
    let mut summary = Vec::new();
    if let Some(inv) = invocation {
        match &inv.subject {
            ToolSubjectWire::Command { script, cwd } => {
                summary.push(format!("$ {}", script.lines().next().unwrap_or_default()));
                summary.push(format!("cwd: {cwd}"));
            }
            ToolSubjectWire::File { path } => summary.push(format!("path: {path}")),
            ToolSubjectWire::Other => {}
        }
    }
    if let Some(command) = facts.and_then(|facts| facts.command.as_deref()) {
        if area.width < 80 {
            summary.clear();
            summary.push(format!(
                "{:?}{}",
                command.status,
                command.exit_code.map_or(String::new(), |code| format!(
                    " · exit {code}{}",
                    if code != 0 { " (nonzero)" } else { "" }
                ))
            ));
            summary.push(format!(
                "termination_confirmed={} output_complete={}",
                command.termination_confirmed, command.output_complete
            ));
        } else {
            summary.push(format!(
                "{:?}{} · termination_confirmed={} · output_complete={}{}",
                command.status,
                command.exit_code.map_or(String::new(), |code| format!(
                    " · exit {code}{}",
                    if code != 0 { " (退出码非零)" } else { "" }
                )),
                command.termination_confirmed,
                command.output_complete,
                if command.output_truncated {
                    " · truncated"
                } else {
                    ""
                }
            ));
        }
    } else if let Some(execution) = execution {
        summary.push(format!(
            "started: {} · input 为请求参数，未经工具验证",
            execution.started_at.as_deref().unwrap_or("未开始/未知")
        ));
    }
    let summary_rows = if area.width < 80 { 2 } else { 3 };
    for (index, text) in summary.into_iter().take(summary_rows).enumerate() {
        line(frame, area, index as u16 + 1, text, theme);
    }
    let tab_row = summary_rows as u16 + 1;
    let tabs = app
        .tool_tabs()
        .into_iter()
        .map(|tab| {
            if tab == detail.tab {
                Span::styled(
                    format!("[{}]  ", tab.label()),
                    Style::new().fg(theme.scrollbar_thumb),
                )
            } else {
                Span::raw(format!("{}  ", tab.label()))
            }
        })
        .collect::<Vec<_>>();
    if tab_row < area.height {
        frame.render_widget(
            Paragraph::new(Line::from(tabs)),
            Rect::new(
                area.x + 1,
                area.y + tab_row,
                area.width.saturating_sub(2),
                1,
            ),
        );
    }
    let stream = detail.stream();
    line(
        frame,
        area,
        tab_row + 1,
        detail.error.clone().unwrap_or_else(|| {
            format!(
                "{:?} · bytes {}..{} · EOF={}{}{} · F6 编辑 / Esc 返回 / F5 重读",
                stream.availability,
                stream.base_offset,
                stream.next_offset,
                stream.eof,
                if stream.gap {
                    " · 前缀缺失，可重读"
                } else {
                    ""
                },
                if stream.truncated {
                    " · partial/truncated"
                } else {
                    ""
                }
            )
        }),
        theme,
    );
    let body = body_area(area);
    if let Some(layout) = detail
        .layout
        .as_ref()
        .filter(|layout| layout.identity.width == body.width.saturating_sub(1).max(1))
    {
        let offset = detail.offset(body.height as usize);
        let lines: Vec<_> = layout
            .rows
            .iter()
            .skip(offset)
            .take(body.height as usize)
            .map(|range| Line::from(layout.text[range.clone()].to_owned()))
            .collect();
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().fg(theme.text)),
            body,
        );
        crate::ui::scrollbar::render(
            frame,
            body,
            layout.rows.len(),
            offset,
            theme,
            app.focus == crate::state::panels::Focus::Main,
        );
    } else {
        frame.render_widget(
            Paragraph::new("布局中…").style(Style::new().fg(theme.muted)),
            body,
        );
    }
}
