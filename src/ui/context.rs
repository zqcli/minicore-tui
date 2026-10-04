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
        "上下文估算（≈，不是 Provider 实际用量）".into(),
        format!(
            "  ≈last prepared request:{} / input budget:{} tokens",
            number(b.estimated_request_context_tokens),
            number(b.input_budget_tokens)
        ),
        format!(
            "  ≈history tail tokens:{} bytes:{} items:{}",
            number(b.estimated_history_tokens),
            number(b.estimated_history_bytes),
            number(b.estimated_history_items)
        ),
        "  history tail excludes summary".into(),
        String::new(),
        format!(
            "Current compaction: {}",
            x.current_operation.as_ref().map_or(
                "no active operation observed".into(),
                |o| format!(
                    "{} {} covered:{} retained:{}",
                    safe(&o.operation_id),
                    crate::ui::status::compaction_phase_label(o.phase),
                    o.covered_item_count,
                    o.retained_item_count
                )
            )
        ),
    ];
    let result = x
        .last_result
        .as_ref()
        .or_else(|| v.manual_compact.as_ref().and_then(|m| m.result.as_ref()));
    r.push("最近压缩结果".into());
    if let Some(o) = result {
        let origin = if o.origin.is_none() {
            if v.manual_compact
                .as_ref()
                .is_some_and(|manual| manual.operation_id == o.operation_id)
            {
                "manual"
            } else {
                v.compaction_feedback
                    .iter()
                    .find(|entry| entry.operation_id == o.operation_id)
                    .map_or_else(|| o.origin_label(), |entry| entry.origin_label())
            }
        } else {
            o.origin_label()
        };
        r.push(format!(
            "  {} {} {:?} failure:{}",
            origin,
            safe(&o.operation_id),
            o.status,
            safe(o.failure_kind.as_deref().unwrap_or("none"))
        ));
        r.push("  compaction projection (not latest request)".into());
        r.push(format!(
            "  ≈before:{} after:{}",
            number(o.before_tokens),
            number(o.after_tokens)
        ));
        usage(&mut r, o.utility_usage.as_ref());
    } else {
        r.push("  unknown / 尚无结果；不是零 usage".into());
    }
    r.push(String::new());
    r.push("压缩覆盖（计数；不展示 summary 正文）".into());
    r.push(format!(
        "  loops:{} items:{} retained:{}",
        x.coverage.covered_loop_count,
        x.coverage.covered_item_count,
        x.coverage.retained_item_count
    ));
    r.push(String::new());
    r.push("详细诊断".into());
    r.push(format!(
        "  trigger:{} target:{} tokens",
        number(b.trigger_tokens),
        number(b.target_tokens)
    ));
    r.push(format!(
        "  runtime items:{} bytes:{} within:{}",
        number(b.max_history_items),
        number(b.max_history_bytes),
        b.within_runtime_limits
            .map_or("unknown".into(), |v| v.to_string())
    ));
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
    if x.automatic.current.is_none() && x.automatic.last.is_none() {
        r.push("自动 preparation 字段: 暂无观察记录（不代表没有自动压缩）".into());
    } else {
        if x.automatic.current.is_some() {
            automatic(
                &mut r,
                "自动 preparation current",
                x.automatic.current.as_ref(),
            );
        }
        if x.automatic.last.is_some() {
            automatic(&mut r, "自动 preparation last", x.automatic.last.as_ref());
        }
    }
    r.push(format!(
        "最近 preparation failure:{}",
        safe(x.last_prepare_failure.as_deref().unwrap_or("none observed"))
    ));
    if let Some(o) = &x.recovery {
        r.push("恢复观察（最近一次读取的快照；不代表当前状态；F5 刷新）".into());
        r.push(format!(
            "  loop:{} request:{} outcome:{} failure:{}",
            safe(&o.loop_id),
            o.request_index,
            safe(&o.outcome),
            safe(o.failure_kind.as_deref().unwrap_or("none"))
        ));
        r.push(format!(
            "  recovery full request ≈before:{} after:{}",
            number(o.before_tokens),
            number(o.after_tokens)
        ));
        usage(&mut r, o.utility_usage.as_ref());
    }
    r.push("上下文按事件/操作结果单次读取；无周期轮询；F5 显式刷新".into());
    r
}
pub fn render(frame: &mut Frame, area: Rect, scrollbar: Rect, app: &App, theme: &Theme) {
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
            Line::from(if !app.context_supported {
                "Agent context 不兼容；compact 已禁用"
            } else if enabled[2] {
                "压缩进行中；关闭本页不会取消，cancel 可停止该操作"
            } else if enabled[1] {
                "手动压缩可用；Esc 返回对话"
            } else {
                "手动压缩暂不可用：会话需已加载、空闲且历史已同步"
            }),
            Line::from(format!("{focus} · Tab 选择 Enter 执行 · F6 编辑 · F5 刷新")),
        ])
        .style(Style::new().fg(theme.muted)),
        Rect::new(area.x, area.y, area.width, area.height.min(4)),
    );
    let body = super::workspace::file_body(area);
    let scrollbar = crate::ui::layout::fit_scrollbar(scrollbar, body);
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
        body,
    );
    super::scrollbar::render(
        frame,
        scrollbar,
        all.len(),
        offset,
        theme,
        app.focused_region() == crate::state::panels::Focus::Main,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;
    use serde_json::json;

    #[test]
    fn context_leads_with_estimates_and_preserves_unknown_and_limits() {
        let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, "context-test", None, "high");
        app.sessions.known.get_mut("context-test").unwrap().context = Some(
            serde_json::from_value(json!({
                "session_id": "context-test",
                "coverage": {"covered_loop_count": 0, "covered_item_count": 0, "retained_item_count": 0},
                "budget": {"estimated_history_tokens": 0, "input_budget_tokens": 32000,
                    "trigger_tokens": 24000, "target_tokens": 16000,
                    "max_history_items": 4096, "max_history_bytes": 2097152},
                "automatic": {}
            })).unwrap()
        );
        app.open_context();
        let lines = rows(&app);
        assert!(lines[0].contains("不是 Provider 实际用量"));
        assert!(lines[1].contains("≈last prepared request:unknown / input budget:32000"));
        assert!(lines[2].contains("≈history tail tokens:0"));
        let text = lines.join("\n");
        assert!(text.contains("unknown / 尚无结果；不是零 usage"));
        assert!(text.contains("loops:0 items:0 retained:0"));
        assert!(text.contains("trigger:24000 target:16000"));
        assert!(text.contains("runtime items:4096 bytes:2097152 within:unknown"));
        assert!(text.find("最近压缩结果") < text.find("详细诊断"));
        assert!(text.contains("自动 preparation 字段: 暂无观察记录（不代表没有自动压缩）"));
        assert!(!text.contains("自动 preparation current: none observed"));
        assert!(lines.last().unwrap().contains("无周期轮询；F5 显式刷新"));
        assert!(!text.contains("500ms"));
        assert!(!text.contains("后台 2s"));
    }

    #[test]
    fn recovering_observation_is_a_latest_read_snapshot_with_explicit_refresh() {
        let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, "context-test", None, "high");
        app.sessions.known.get_mut("context-test").unwrap().context = Some(
            serde_json::from_value(json!({
                "session_id": "context-test",
                "coverage": {"covered_loop_count": 0, "covered_item_count": 0, "retained_item_count": 0},
                "budget": {}, "automatic": {},
                "recovery": {"loop_id": "loop", "request_index": 0,
                    "before_tokens": 1500, "after_tokens": 700, "outcome": "recovering"}
            }))
            .unwrap(),
        );
        app.open_context();
        let text = rows(&app).join("\n");
        assert!(text.contains("恢复观察（最近一次读取的快照；不代表当前状态；F5 刷新）"));
        assert!(text.contains("loop:loop request:0 outcome:recovering failure:none"));
        assert!(text.contains("recovery full request ≈before:1500 after:700"));
        assert!(text.contains("无周期轮询；F5 显式刷新"));
    }
    #[test]
    fn authoritative_automatic_result_supersedes_manual_cache() {
        let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, "context-test", None, "high");
        let view = app.sessions.known.get_mut("context-test").unwrap();
        let old: crate::protocol::CompactResultWire = serde_json::from_value(
            json!({"operation_id":"manual-old", "origin":"manual", "status":"compacted"}),
        )
        .unwrap();
        view.manual_compact = Some(crate::state::session::ManualCompactState {
            operation_id: "manual-old".into(),
            cancel_requested: false,
            result: Some(old),
            state_refresh_confirmed: false,
            context_refresh_confirmed: false,
        });
        view.context = Some(serde_json::from_value(json!({
            "session_id":"context-test", "coverage":{"covered_loop_count":2,"covered_item_count":8,"retained_item_count":2},
            "last_result":{"operation_id":"auto-new", "origin":"automatic", "status":"compacted", "before_tokens":1234,"after_tokens":456},
            "budget":{"estimated_request_context_tokens":900,"estimated_history_tokens":200}, "automatic":{},
            "recovery":{"loop_id":"loop", "request_index":0,"before_tokens":1500,"after_tokens":700,"outcome":"recovered"}
        })).unwrap());
        app.open_context();
        let text = rows(&app).join("\n");
        assert!(text.contains("automatic auto-new Compacted"));
        assert!(!text.contains("manual-old Compacted"));
        assert!(text.contains("last prepared request:900"));
        assert!(text.contains("history tail tokens:200"));
        assert!(
            text.contains("compaction projection (not latest request)\n  ≈before:1234 after:456")
        );
        assert!(text.contains("recovery full request ≈before:1500 after:700"));
        assert!(!text.contains("自动压缩: 暂无观察记录"));
        let view = app.sessions.known.get_mut("context-test").unwrap();
        let context = view.context.as_mut().unwrap();
        let result = context.last_result.as_mut().unwrap();
        result.operation_id = "manual-old".into();
        result.origin = None;
        result.before_tokens = Some(555);
        let text = rows(&app).join("\n");
        assert!(text.contains("manual manual-old Compacted"));
        assert!(text.contains("≈before:555"));
        let view = app.sessions.known.get_mut("context-test").unwrap();
        let known_manual = view.manual_compact.take().unwrap().result.unwrap();
        view.record_compaction_result(known_manual);
        assert!(
            rows(&app)
                .join("\n")
                .contains("manual manual-old Compacted")
        );
    }

    #[test]
    fn utility_usage_preserves_missing_cache_write_and_normalized_counts() {
        let value = serde_json::from_value(json!({"call_count":1,"complete":true,"usage":{
            "input_tokens":10,"output_tokens":20,"reasoning_tokens":30,"cache_read_tokens":40,"provider_total_tokens":100
        }})).unwrap();
        let mut rows = Vec::new();
        usage(&mut rows, Some(&value));
        let text = rows.join("\n");
        assert!(text.contains("utility input:10 output:20 reasoning:30"));
        assert!(text.contains("utility cache read:40 write:unknown provider-total:100"));
    }
}
