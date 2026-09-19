//! Context metadata only: no summary body and no rendering side effects.
use crate::{
    app::App,
    protocol::{AutomaticContextOperationWire, CompactUtilityUsageWire},
    theme::Theme,
};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};
fn safe(s: &str) -> String {
    crate::safe_text::safe_display(s)
        .replace('\n', "\\n")
        .replace('\t', "    ")
}
fn number(v: Option<u64>) -> String {
    v.map_or("unknown".into(), |v| v.to_string())
}
fn usage(rows: &mut Vec<String>, u: Option<&CompactUtilityUsageWire>) {
    let Some(u) = u else {
        rows.push("  utility usage: unknown（不计入普通 Loop usage）".into());
        return;
    };
    rows.push(format!(
        "  utility attempts:{} complete:{}",
        u.call_count, u.complete
    ));
    let get =
        |f: fn(&crate::protocol::UsageWire) -> Option<u64>| number(u.usage.as_ref().and_then(f));
    rows.push(format!(
        "  utility input:{} output:{} reasoning:{}",
        get(|u| u.input_tokens),
        get(|u| u.output_tokens),
        get(|u| u.reasoning_tokens)
    ));
    rows.push(format!(
        "  utility cache read:{} write:{} provider-total:{}",
        get(|u| u.cache_read_tokens),
        get(|u| u.cache_write_tokens),
        get(|u| u.provider_total_tokens)
    ));
}
fn automatic(rows: &mut Vec<String>, label: &str, v: Option<&AutomaticContextOperationWire>) {
    rows.push(format!(
        "{label}: {}",
        v.and_then(|o| o.operation_id.as_deref())
            .map(safe)
            .unwrap_or_else(|| "none observed".into())
    ));
    if let Some(o) = v {
        rows.push(format!(
            "  loop:{} request:{} outcome:{}",
            safe(o.loop_id.as_deref().unwrap_or("unknown")),
            o.request_index.map_or("unknown".into(), |v| v.to_string()),
            safe(o.outcome.as_deref().unwrap_or("unknown"))
        ));
        rows.push(format!(
            "  ≈before:{} after:{} hard:{}",
            number(o.before_tokens),
            number(o.after_tokens),
            number(o.hard_tokens)
        ));
        rows.push(format!(
            "  trigger:{} target:{} utility-before:{} after:{}",
            number(o.trigger_tokens),
            number(o.target_tokens),
            number(o.utility_before_tokens),
            number(o.utility_after_tokens)
        ));
        usage(rows, o.utility_usage.as_ref());
    }
}
pub fn rows(app: &App) -> Vec<String> {
    let Some(c) = app.context_panel() else {
        return vec![];
    };
    let Some(v) = app.sessions.known.get(&c.session) else {
        return vec![];
    };
    let Some(x) = &v.context else {
        return vec!["尚无 session.context 观察；F5 显式刷新".into()];
    };
    let b = &x.budget;
    let mut r = vec![
        "覆盖（计数，不展示 summary 正文）".into(),
        format!(
            "  loops:{} items:{} retained:{}",
            x.coverage.covered_loop_count,
            x.coverage.covered_item_count,
            x.coverage.retained_item_count
        ),
        "预算估算（≈非完整 Provider 输入统计）".into(),
        format!(
            "  ≈history tokens:{} bytes:{} items:{}",
            number(b.estimated_history_tokens),
            number(b.estimated_history_bytes),
            number(b.estimated_history_items)
        ),
        format!(
            "  ≈request context:{} input budget:{}",
            number(b.estimated_request_context_tokens),
            number(b.input_budget_tokens)
        ),
        format!(
            "  trigger:{} target:{}",
            number(b.trigger_tokens),
            number(b.target_tokens)
        ),
        format!(
            "  runtime items:{} bytes:{} within:{}",
            number(b.max_history_items),
            number(b.max_history_bytes),
            b.within_runtime_limits
                .map_or("unknown".into(), |v| v.to_string())
        ),
        format!(
            "当前 preparation: {}",
            x.current_operation
                .as_ref()
                .map_or("none observed".into(), |o| format!(
                    "{} {:?} covered:{} retained:{}",
                    safe(&o.operation_id),
                    o.phase,
                    o.covered_item_count,
                    o.retained_item_count
                ))
        ),
    ];
    automatic(
        &mut r,
        "自动 preparation current",
        x.automatic.current.as_ref(),
    );
    automatic(&mut r, "自动 preparation last", x.automatic.last.as_ref());
    if let Some(m) = &v.manual_compact {
        r.push(format!(
            "本地手动操作: {} cancel_requested:{}",
            safe(&m.operation_id),
            m.cancel_requested
        ));
        r.push(format!(
            "  state-confirmed:{} context-confirmed:{}",
            m.state_refresh_confirmed, m.context_refresh_confirmed
        ));
    }
    let result = v
        .manual_compact
        .as_ref()
        .and_then(|m| m.result.as_ref())
        .or(x.last_result.as_ref());
    r.push("手动 compact 最近结果".into());
    if let Some(o) = result {
        r.push(format!(
            "  {} {:?} failure:{}",
            safe(&o.operation_id),
            o.status,
            safe(o.failure_kind.as_deref().unwrap_or("none"))
        ));
        r.push(format!(
            "  ≈before:{} after:{}",
            number(o.before_tokens),
            number(o.after_tokens)
        ));
        usage(&mut r, o.utility_usage.as_ref());
    } else {
        r.push("  unknown / 尚无结果；不是零 usage".into());
    }
    r.push(format!(
        "最近 preparation failure:{}",
        safe(x.last_prepare_failure.as_deref().unwrap_or("none observed"))
    ));
    if let Some(o) = &x.recovery {
        r.push(format!(
            "恢复观察: loop:{} request:{} outcome:{} failure:{}",
            safe(&o.loop_id),
            o.request_index,
            safe(&o.outcome),
            safe(o.failure_kind.as_deref().unwrap_or("none"))
        ));
        r.push(format!(
            "  ≈before:{} after:{}",
            number(o.before_tokens),
            number(o.after_tokens)
        ));
        usage(&mut r, o.utility_usage.as_ref());
    }
    r
}
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(c) = app.context_panel() else {
        return;
    };
    let enabled = [
        app.context_supported,
        app.can_manual_compact(),
        app.context_cancel_target().is_some(),
    ];
    let actions = ["刷新", "compact", "cancel"]
        .iter()
        .enumerate()
        .map(|(i, label)| {
            format!(
                "{}[{}{}]",
                if c.action == i { "›" } else { " " },
                label,
                if enabled[i] { "" } else { " disabled" }
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let focus = if app.focused_region() == crate::state::panels::Focus::Main {
        "正文焦点"
    } else {
        "Editor/Dock 焦点"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("← 返回 · Context（快照/操作分开；关闭不取消）"),
            Line::from(actions),
            Line::from(if app.context_supported {
                "idle 不轮询；操作活跃时 500ms 前台 / 2s 后台"
            } else {
                "Agent context 不兼容；compact 已禁用"
            }),
            Line::from(format!("{focus} · Tab 选择 Enter 执行 · F6 编辑 · F5 刷新")),
        ])
        .style(Style::new().fg(theme.muted)),
        Rect::new(area.x, area.y, area.width, area.height.min(4)),
    );
    let body = super::workspace::file_body(area);
    let all = rows(app);
    let offset = c.offset.min(all.len().saturating_sub(body.height as usize));
    let visible: Vec<_> = all
        .iter()
        .skip(offset)
        .take(body.height as usize)
        .cloned()
        .map(Line::from)
        .collect();
    frame.render_widget(
        Paragraph::new(visible).style(Style::new().fg(theme.text)),
        Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height),
    );
    super::scrollbar::render(
        frame,
        body,
        all.len(),
        offset,
        theme,
        app.focused_region() == crate::state::panels::Focus::Main,
    );
}
