//! Pure workspace renderers: never touch the filesystem or initiate a query.
use crate::{
    app::App,
    protocol::workspace::{FileMatch, FileStatus, ScanStop},
    safe_text::safe_display,
    state::workspace::{BrowserKind, WorkspaceBrowser},
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
        Line::from(format!(
            "已读 {} bytes · partial:{} line_partial:{} next:{}",
            file.content.bytes,
            file.truncated,
            file.line_truncated,
            file.next.is_some()
        )),
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
                "正文焦点 · F6 编辑 · Ctrl+N 更多 · F5 刷新 · Esc 返回"
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
    // Keep IME/caret positioning on the actual input field. The source remains
    // raw; only the visible safe representation is measured in terminal cells.
    if area.width > 0 && area.height > 2 {
        let (prefix, input, row) = if b.scope_focused {
            (
                if b.kind == BrowserKind::Files {
                    "▸ directory: "
                } else {
                    "▸ paths: "
                },
                b.scope.as_str(),
                2,
            )
        } else {
            ("▸ query: ", b.query.as_str(), 1)
        };
        let cells =
            crate::markdown::column_width(prefix) + crate::markdown::column_width(&safe(input));
        frame.set_cursor_position((
            area.x + cells.min(area.width.saturating_sub(1) as usize) as u16,
            area.y + row,
        ));
    }
    let title = if b.kind == BrowserKind::Files {
        "Files · 引用路径，模型需要时再读 · Enter 插入 / F4 预览"
    } else {
        "Grep · literal only · Enter 预览 · Ctrl+I 大小写"
    };
    let inputs = vec![
        Line::from(title),
        Line::from(format!(
            "{}query: {}",
            if !b.scope_focused { "▸ " } else { "  " },
            safe(&b.query)
        )),
        Line::from(format!(
            "{}{}: {}{}",
            if b.scope_focused { "▸ " } else { "  " },
            if b.kind == BrowserKind::Files {
                "directory"
            } else {
                "paths"
            },
            safe(&b.scope),
            if b.kind == BrowserKind::Grep {
                if b.case_sensitive {
                    " [case sensitive]"
                } else {
                    " [ignore case]"
                }
            } else {
                ""
            }
        )),
    ];
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
    let stop = b.stopped_by.map_or_else(
        || {
            if b.due.is_some() {
                "debounce"
            } else {
                "reading/idle"
            }
            .to_owned()
        },
        |s| format!("{s:?}"),
    );
    let summary = vec![
        Line::from(format!(
            "本页 partial:{} scan_complete:{} stopped_by:{}",
            b.truncated, b.scan_complete, stop
        )),
        Line::from(format!(
            "本页 skipped:{} · received:{} · cursor:{} · local_limit:{}",
            b.skipped,
            b.len(),
            b.cursor.is_some(),
            b.limited
        )),
        Line::from(b.error.as_deref().map(safe).unwrap_or_else(|| {
            if b.limited {
                "本地 500 项 / 1 MiB 上限：缩小范围，未全部列出".into()
            } else if b.stopped_by == Some(ScanStop::Deadline) {
                "deadline：缩小范围或 F5 手动刷新，不会自动重扫".into()
            } else {
                "Tab 查询/范围 · Ctrl+U 清空 · Ctrl+N 下一页 · F5 刷新".into()
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
