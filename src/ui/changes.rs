//! Pure single-column changes/diff presentation. No Git or RPC in rendering.
use crate::{app::App, protocol::changes::*, theme::Theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
fn safe(s: &str) -> String {
    crate::safe_text::safe_display(s)
        .replace('\n', "\\n")
        .replace('\t', "    ")
}
fn version(v: &ChangeRevision) -> String {
    match v {
        ChangeRevision::Missing => "missing".into(),
        ChangeRevision::Unknown => "unknown".into(),
        ChangeRevision::Content { sha256, bytes } => format!(
            "{}…/{bytes}B",
            safe(&sha256.chars().take(8).collect::<String>())
        ),
        ChangeRevision::Metadata {
            bytes,
            modified_unix_ms,
        } => format!("metadata {bytes}B mtime:{modified_unix_ms:?}"),
    }
}
pub fn render(frame: &mut Frame, area: Rect, scrollbar: Rect, app: &App, theme: &Theme) {
    let Some(s) = app.changes() else {
        return;
    };
    let mut headers = vec![Line::from(
        "← 返回 · Changes 只读 · Tab 范围/比较 · F5 刷新",
    )];
    let body = super::workspace::file_body(area);
    let scrollbar = crate::ui::layout::fit_scrollbar(scrollbar, body);
    let focused = app.focused_region() == crate::state::panels::Focus::Main;
    if s.in_diff {
        headers[0] = Line::from("← Diff · F5刷新 F6编辑 ^N更多 ^⇧C复制行源");
        let d = s.detail.as_ref().unwrap();
        headers.push(Line::from(format!(
            "{} · {:?} · {:?}",
            d.record.original_path.as_ref().map_or_else(
                || safe(&d.record.path),
                |old| format!("{} → {}", safe(old), safe(&d.record.path))
            ),
            d.comparison,
            d.record.origin
        )));
        headers.push(Line::from(if let Some(p) = &d.meta {
            format!(
                "base:{} → target:{}",
                version(&p.base_version),
                version(&p.target_version)
            )
        } else {
            "versions unknown · 读取中".into()
        }));
        headers.push(Line::from(if d.stale {
            "stale · 保留旧内容，F5 显式刷新".into()
        } else if let Some(e) = &d.error {
            safe(e)
        } else if let Some(p) = &d.meta {
            format!(
                "{} · {:?}/{:?} page:{} next:{} {}",
                if d.layout.as_ref().is_some_and(|l| l.display_limited) {
                    "display-limit".into()
                } else {
                    format!("{:?}", p.availability)
                },
                p.commit_state,
                p.coverage,
                if p.complete && !p.truncated {
                    "all"
                } else {
                    "partial"
                },
                d.cursor.is_some(),
                if d.buffer.partial_line() {
                    "line-partial"
                } else {
                    ""
                }
            )
        } else {
            "等待比较；Esc 返回列表，F6 编辑；关闭不取消执行".into()
        }));
        if let Some(layout) = &d.layout {
            let max = layout.rows.len().saturating_sub(body.height as usize);
            let offset = if d.follow { max } else { d.offset.min(max) };
            let rows: Vec<_> = layout
                .rows
                .iter()
                .skip(offset)
                .take(body.height as usize)
                .map(|row| {
                    let color = match row.kind {
                        Some(DiffKind::Added) => theme.success,
                        Some(DiffKind::Removed) => theme.error,
                        None => theme.accent,
                        _ => theme.text,
                    };
                    let prefix = match row.kind {
                        Some(DiffKind::Added) => "+",
                        Some(DiffKind::Removed) => "-",
                        Some(DiffKind::Context) => " ",
                        None => " ",
                    };
                    let num = |n: Option<usize>| {
                        n.map(|i| i.saturating_add(1).to_string())
                            .unwrap_or_default()
                    };
                    Line::from(vec![
                        Span::styled(
                            format!("{:>6} {:>6} {prefix} ", num(row.old), num(row.new)),
                            Style::new().fg(theme.muted),
                        ),
                        Span::styled(
                            layout.text[row.text.clone()].to_owned(),
                            Style::new().fg(color),
                        ),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(rows), body);
            super::scrollbar::render(frame, scrollbar, layout.rows.len(), offset, theme, focused);
        }
    } else {
        headers.push(Line::from(match &s.scope {
            ChangeScope::Workspace => "workspace · 归属未知（含工具、Bash、用户/外部编辑）".into(),
            ChangeScope::Session => "session · 仅原生 write/edit/apply_patch 记录".into(),
            ChangeScope::Turn { loop_id } => {
                format!("turn {} · 仅原生 write/edit/apply_patch", safe(loop_id))
            }
        }));
        headers.push(Line::from(s.error.as_deref().map(safe).unwrap_or_else(
            || {
                s.list_page.as_ref().map_or("读取中".into(), |p| {
                    format!(
                        "{}/{} {:?} complete:{} local_limit:{} {:?}",
                        s.records.len(),
                        p.total,
                        p.consistency,
                        p.complete,
                        s.limited,
                        p.warnings
                    )
                })
            },
        )));
        headers.push(Line::from(if focused {
            "正文焦点 · Enter 详情 · Ctrl+N 更多 · F6 编辑 · Esc 返回"
        } else {
            "Editor/Dock 焦点 · F6 正文 · Esc 返回，不取消执行"
        }));
        let rows: Vec<_> = s
            .records
            .iter()
            .enumerate()
            .skip(s.offset)
            .take(body.height as usize)
            .map(|(i, r)| {
                let source = r.tool_ref.as_ref().map_or("workspace_unknown".into(), |t| {
                    format!(
                        "{}/{}/{}",
                        safe(&t.loop_id),
                        safe(&t.tool_call_id),
                        t.request_index
                    )
                });
                Line::from(format!(
                    "{} {} · {:?} {:?} {:?} detail:{} · {}",
                    if i == s.selected { "›" } else { " " },
                    safe(&r.path),
                    r.kind,
                    r.commit_state,
                    r.coverage,
                    r.details_available,
                    source
                ))
                .style(if i == s.selected {
                    Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(theme.text)
                })
            })
            .collect();
        frame.render_widget(Paragraph::new(rows), body);
        super::scrollbar::render(frame, scrollbar, s.records.len(), s.offset, theme, focused);
    }
    frame.render_widget(
        Paragraph::new(headers).style(Style::new().fg(theme.muted)),
        Rect::new(area.x, area.y, area.width, area.height.min(4)),
    );
}
