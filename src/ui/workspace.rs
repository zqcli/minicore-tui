//! Pure workspace renderers: never touch the filesystem or initiate a query.
use crate::{
    app::App,
    protocol::workspace::{FileMatch, FileStatus, ScanStop},
    safe_text::safe_display,
    state::workspace::{BrowserKind, FilePreviewState, WorkspaceBrowser},
    theme::Theme,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

fn safe(text: &str) -> String {
    safe_display(text)
        .replace('\t', "    ")
        .replace('\n', "\\n")
}
pub fn file_body(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(4),
        area.width,
        area.height.saturating_sub(4),
    )
}
pub fn file_actions(area: Rect) -> (Rect, Rect, Rect) {
    let start = area.right().saturating_sub(27).max(area.x);
    (
        Rect::new(start, area.y, 9, 1),
        Rect::new(start + 9, area.y, 9, 1),
        Rect::new(start + 18, area.y, 9, 1),
    )
}
fn file_has_more(file: &FilePreviewState) -> bool {
    file.status == Some(FileStatus::Ok) && file.error.is_none() && file.next.is_some()
}
fn file_progress(file: &FilePreviewState) -> String {
    let mut parts = vec![format!("已读 {} bytes", file.content.bytes)];
    if file.status == Some(FileStatus::Changed) || file.error.is_some() {
        if file.content.bytes > 0 {
            parts.push("保留旧快照".into());
        }
    } else if file.status == Some(FileStatus::Ok) {
        if file.truncated || file.line_truncated || file.next.is_some() {
            parts.push("部分内容".into());
        } else {
            parts.push("已全部读取".into());
        }
        if file.line_truncated {
            parts.push("本行未完".into());
        }
        if file_has_more(file) {
            parts.push("Ctrl+N 继续读取".into());
        }
    }
    parts.join(" · ")
}
pub fn render_file(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(file) = app.file_preview() else {
        return;
    };
    let heading = format!("← 返回  {}", safe(&file.path));
    frame.render_widget(
        Paragraph::new(heading).style(Style::new().fg(theme.muted)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let (copy, more, refresh) = file_actions(area);
    for (rect, label) in [(copy, "[复制]"), (more, "[更多]"), (refresh, "[刷新]")] {
        if rect == more && !file_has_more(file) {
            continue;
        }
        frame.render_widget(
            Paragraph::new(label).style(Style::new().fg(theme.accent)),
            rect,
        );
    }
    let status = file
        .error
        .as_deref()
        .map(safe)
        .unwrap_or_else(|| match file.status {
            None => "读取中…".to_owned(),
            Some(FileStatus::Ok) => "ok · 只读路径预览，模型需要时再读（未附加内容）".into(),
            Some(FileStatus::Changed) => "changed · 文件已改变，保留旧内容；F5 重新读取".into(),
            Some(FileStatus::Binary) => "binary · 非 UTF-8 或含 NUL，不能预览".into(),
            Some(FileStatus::TooLarge) => {
                "too_large · 超出后端 512 KiB 文件上限，无部分预览".into()
            }
        });
    let lines = vec![
        Line::from(status),
        Line::from(file_progress(file)),
        Line::from(
            if file.status == Some(FileStatus::Ok)
                && file
                    .layout
                    .as_ref()
                    .is_none_or(|l| l.identity.revision != file.content_revision)
            {
                "布局中；复制只对应已经显示的快照 · Esc 返回"
            } else if file.layout.as_ref().is_some_and(|l| l.display_limited) {
                "显示含超长 grapheme 占位；复制无行号/软换行"
            } else if app.focused_region() == crate::state::panels::Focus::Editor {
                "Editor 焦点 · F6 返回正文 · 保持原输入键 · Esc 返回"
            } else if app.focused_region() == crate::state::panels::Focus::Main {
                "正文焦点 · F6 编辑 · F5 刷新 · Esc 返回"
            } else {
                "Dock 焦点 · Esc 先关闭 Dock，不会取消执行"
            },
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::new().fg(theme.muted)),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            3.min(area.height.saturating_sub(1)),
        ),
    );
    let body = file_body(area);
    if let Some(layout) = &file.layout {
        let offset = file.scroll_offset(body.height as usize);
        let rows: Vec<_> = layout
            .rows
            .iter()
            .skip(offset)
            .take(body.height as usize)
            .map(|row| {
                Line::from(vec![
                    Span::styled(
                        format!("{:>6} │", row.source.start_line),
                        Style::new().fg(theme.muted),
                    ),
                    Span::raw(layout.text[row.text.clone()].to_owned()),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(rows).style(Style::new().fg(theme.text)),
            Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height),
        );
        crate::ui::scrollbar::render(
            frame,
            body,
            layout.rows.len(),
            offset,
            theme,
            app.focused_region() == crate::state::panels::Focus::Main,
        );
    }
}
/// Verified byte ranges become styled safe spans, whose widths are terminal cells.
/// Only a bounded context around the first match is rendered; raw offsets stay untouched.
pub fn match_snippet(item: &FileMatch) -> Line<'static> {
    if !item.valid_ranges() {
        return Line::from("invalid UTF-8 match range");
    }
    use unicode_segmentation::UnicodeSegmentation;
    let start = (item.match_byte_ranges[0].start as usize).saturating_sub(64);
    let end = (start + 240).min(item.line_text.len());
    let mut pieces: Vec<(String, bool)> = Vec::new();
    for (offset, grapheme) in item.line_text.grapheme_indices(true) {
        if offset + grapheme.len() <= start {
            continue;
        }
        if offset >= end {
            break;
        }
        // A literal may match a combining mark or part of a ZWJ cluster.
        // Style the complete terminal grapheme rather than dropping its mark.
        let highlighted = item
            .match_byte_ranges
            .iter()
            .any(|r| (r.start as usize) < offset + grapheme.len() && r.end as usize > offset);
        let text = if grapheme.len() > 2048 {
            "[oversized grapheme]".to_owned()
        } else {
            safe(grapheme)
        };
        if let Some((previous, _)) = pieces
            .last_mut()
            .filter(|(_, marked)| *marked == highlighted)
        {
            previous.push_str(&text);
        } else {
            pieces.push((text, highlighted));
        }
    }
    let mut spans = vec![];
    if start > 0 {
        spans.push(Span::raw("…"));
    }
    spans.extend(pieces.into_iter().map(|(text, marked)| {
        if marked {
            Span::styled(
                text,
                Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )
        } else {
            Span::raw(text)
        }
    }));
    if end < item.line_text.len() || item.line_truncated {
        spans.push(Span::raw("…"));
    }
    Line::from(spans)
}
pub fn render_browser(frame: &mut Frame, area: Rect, b: &WorkspaceBrowser, theme: &Theme) {
    let title = if b.kind == BrowserKind::Files {
        "Files · 引用路径，模型需要时再读 · Enter 插入 / F4 预览"
    } else {
        "Grep · literal only · Enter 预览 · Ctrl+I 大小写"
    };
    let mut inputs = vec![Line::from(title)];
    for (scope, text, cursor, label) in [
        (false, b.query.as_str(), b.query_cursor, "query"),
        (
            true,
            b.scope.as_str(),
            b.scope_cursor,
            if b.kind == BrowserKind::Files {
                "directory"
            } else {
                "paths"
            },
        ),
    ] {
        let active = b.scope_focused == scope;
        let prefix = format!("{}{label}: ", if active { "▸ " } else { "  " });
        let prefix_width = crate::markdown::column_width(&prefix);
        let width = (area.width as usize).saturating_sub(prefix_width);
        let mut spans = vec![Span::raw(prefix)];
        if active {
            let (window, cell) = crate::ui::layout::single_line_window(
                text,
                cursor,
                width,
                Style::new().fg(theme.text),
            );
            spans.extend(window);
            let row = if scope { 2 } else { 1 };
            if width > 0 && area.height > row {
                frame.set_cursor_position((
                    area.x + prefix_width as u16 + cell as u16,
                    area.y + row,
                ));
            }
        } else {
            spans.push(Span::raw(crate::ui::layout::truncate(&safe(text), width)));
        }
        // Preserve the explicit case choice when it fits after the field.
        if scope && b.kind == BrowserKind::Grep {
            let used = spans.iter().map(Span::width).sum::<usize>();
            let label = if b.case_sensitive {
                " [case sensitive]"
            } else {
                " [ignore case]"
            };
            if used + label.len() <= area.width as usize {
                spans.push(Span::raw(label));
            }
        }
        inputs.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(inputs).style(Style::new().fg(theme.muted)),
        Rect::new(area.x, area.y, area.width, 3.min(area.height)),
    );
    let count = area.height.saturating_sub(6) as usize;
    let start = b.selected.saturating_sub(count.saturating_sub(1));
    let rows: Vec<_> = (start..b.len())
        .take(count)
        .map(|i| {
            let prefix = if i == b.selected { "▸ " } else { "  " };
            let mut line = if b.kind == BrowserKind::Files {
                Line::from(format!(
                    "{prefix}{} [{:?}]",
                    safe(&b.files[i].path),
                    b.files[i].kind
                ))
            } else {
                let item = &b.matches[i];
                let mut spans = vec![Span::raw(format!(
                    "{prefix}{}:{} ",
                    safe(&item.path),
                    item.line_number
                ))];
                spans.extend(match_snippet(item).spans);
                Line::from(spans)
            };
            if i == b.selected {
                line = line.style(Style::new().fg(theme.accent));
            }
            line
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rows).style(Style::new().fg(theme.text)),
        Rect::new(area.x, area.y + 3, area.width, count as u16),
    );
    let (progress, facts) = browser_progress(b);
    let summary = vec![
        Line::from(progress),
        Line::from(facts),
        Line::from(b.error.as_deref().map(safe).unwrap_or_else(|| {
            if b.limited {
                "本地 500 项 / 1 MiB 上限：缩小范围，未全部列出".into()
            } else if b.stopped_by == Some(ScanStop::Deadline) {
                "deadline：缩小范围或 F5 手动刷新，不会自动重扫".into()
            } else if area.width >= 100 {
                "Tab 查询/范围 · ←→ Home/End 编辑 · Del 删除 · Ctrl+U 清空 · F5 刷新".into()
            } else {
                "Tab 查询/范围 · Ctrl+U 清空 · F5 刷新".into()
            }
        })),
    ];
    frame.render_widget(
        Paragraph::new(summary).style(Style::new().fg(theme.muted)),
        Rect::new(
            area.x,
            area.bottom().saturating_sub(3),
            area.width,
            3.min(area.height),
        ),
    );
}

fn browser_progress(b: &WorkspaceBrowser) -> (String, String) {
    let count = if b.kind == BrowserKind::Files {
        format!("已列出 {} 项", b.len())
    } else {
        format!("{} 处匹配", b.len())
    };
    let status = if b.kind == BrowserKind::Grep && b.query.is_empty() {
        "输入要查找的文字"
    } else if b.stopped_by.is_none() && b.error.is_none() {
        if b.due.is_some() {
            "等待查询…"
        } else {
            "查询中…"
        }
    } else if b.limited {
        "已达本地保留上限"
    } else if b.cursor.is_some() && b.error.is_none() {
        "还有结果 · Ctrl+N 下一页"
    } else if b.scan_complete && !b.truncated {
        "扫描完成"
    } else {
        "扫描未完成"
    };
    let mut facts = Vec::new();
    if b.truncated {
        facts.push("本页为部分结果".to_owned());
    }
    if b.skipped > 0 {
        facts.push(format!("本页跳过 {} 项", b.skipped));
    }
    if let Some(reason) = match b.stopped_by {
        Some(ScanStop::Entries) => Some("达到扫描条目上限"),
        Some(ScanStop::Bytes) => Some("达到扫描字节上限"),
        Some(ScanStop::Depth) => Some("达到目录深度上限"),
        Some(ScanStop::Rules) => Some("扫描规则限制"),
        Some(ScanStop::Deadline) => Some("扫描超时"),
        _ => None,
    } {
        facts.push(reason.into());
    }
    (format!("{count} · {status}"), facts.join(" · "))
}
