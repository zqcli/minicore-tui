//! Live process events must reach inline cards without a detail-panel read.
use super::*;
use crate::protocol::{ToolDataStreamWire as Stream, ToolRefWire};
use crate::state::view::SectionKind;
use crate::ui::{testapp, transcript};
use base64::Engine;
use serde_json::{Value, json};

fn notify(app: &mut App, value: Value) {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(serde_json::from_value(value).unwrap()),
    ))));
}

fn fixture(command: &str) -> (App, ToolKey) {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let view = app.active_session_mut().unwrap();
    let request = &mut view.live.as_mut().unwrap().requests[0];
    request.parts.clear();
    request.tools.clear();
    Arc::make_mut(&mut view.tool_presentations).clear();
    let key = ToolKey::new("ses_1", "loop_live", 0, "build");
    notify(
        &mut app,
        json!({"type":"tool_started", "data": {
            "turn":{"session_id":"ses_1", "loop_id":"loop_live"}, "request_index":0,
            "tool_call_id":"build", "tool_name":"bash", "meta":{"session_id":"ses_1", "dropped_before":0}
        }}),
    );
    notify(
        &mut app,
        json!({"type":"tool_invocation", "data": {
            "turn":{"session_id":"ses_1", "loop_id":"loop_live"},
            "data":{"tool_ref":ToolRefWire::from(&key), "name":"bash",
                "subject":{"kind":"command", "script":command, "cwd":"."}, "subject_truncated":false,
                "input":{"total_bytes":command.len(), "preview":"{}", "truncated":false, "encoding":"utf8_json"}},
            "meta":{"session_id":"ses_1", "dropped_before":0}
        }}),
    );
    (app, key)
}

fn chunk(app: &mut App, key: &ToolKey, stream: Stream, base: u64, text: &str) {
    notify(
        app,
        json!({"type":"tool_process", "data": {
            "turn":{"session_id":key.session_id, "loop_id":key.loop_id},
            "data":{"tool_ref":ToolRefWire::from(key), "chunk":{"stream":stream, "encoding":"base64",
                "data":base64::engine::general_purpose::STANDARD.encode(text), "base_offset":base,
                "next_offset":base + text.len() as u64, "observed_end":base + text.len() as u64,
                "dropped":false, "expired":false}},
            "meta":{"session_id":key.session_id, "dropped_before":0}
        }}),
    );
}

fn render(app: &App, width: u16) -> (String, bool) {
    let prepared = transcript::prepare_conversation(app, width);
    let folded = prepared
        .sections
        .iter()
        .find(|section| section.id.tool_call_id.as_deref() == Some("build"))
        .unwrap()
        .folded;
    (
        prepared
            .lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        folded,
    )
}

#[test]
fn inline_bash_updates_while_default_folded_and_preserves_manual_expansion() {
    let (mut app, key) = fixture("cargo build");
    app.active_session_mut().unwrap().scroll.follow_tail = false;
    chunk(&mut app, &key, Stream::Stdout, 0, "Compiling");
    chunk(&mut app, &key, Stream::Stderr, 0, "warning");
    assert!(app.tool_detail().is_none());
    assert!(
        app.active_view().unwrap().tool_presentations[&key]
            .result
            .is_none()
    );
    assert!(app.active_view().unwrap().scroll.new_content);
    let (text, folded) = render(&app, 80);
    assert!(folded);
    assert!(text.contains("Compiling") && text.contains("warning"));
    assert!(!text.contains("stdout:") && !text.contains("stderr:"));
    let more = "\nline".repeat(19);
    chunk(&mut app, &key, Stream::Stdout, 9, &more);
    let (text, folded) = render(&app, 80);
    assert!(folded);
    assert!(text.contains("16 earlier lines"), "{text}");
    chunk(
        &mut app,
        &key,
        Stream::Stdout,
        9 + more.len() as u64,
        "\nnext",
    );
    assert!(render(&app, 80).0.contains("17 earlier lines"));
    Arc::make_mut(&mut app.active_session_mut().unwrap().tool_folds)
        .insert(key.clone(), FoldOverride::Expanded);
    chunk(
        &mut app,
        &key,
        Stream::Stdout,
        14 + more.len() as u64,
        "\nlatest",
    );
    let (text, folded) = render(&app, 80);
    assert!(!folded);
    assert!(text.contains("latest"));
}

#[test]
fn process_preview_does_not_leak_to_another_complete_tool_identity() {
    let (mut app, key) = fixture("cargo build");
    chunk(&mut app, &key, Stream::Stdout, 0, "FIRST");
    for other in [
        ToolKey::new("ses_1", "loop_live", 1, "build"),
        ToolKey::new("ses_1", "other", 0, "build"),
        ToolKey::new("ses_1", "loop_live", 0, "other"),
    ] {
        app.accept_tool_process(crate::protocol::ToolProcessWire {
            tool_ref: (&other).into(),
            command: None,
            chunk: Some(crate::protocol::ToolProcessChunkWire {
                stream: Stream::Stdout,
                encoding: "base64".into(),
                data: base64::engine::general_purpose::STANDARD.encode("SECOND"),
                base_offset: 0,
                next_offset: 6,
                observed_end: 6,
                dropped: false,
                expired: false,
            }),
        });
        let view = app.active_view().unwrap();
        assert_eq!(
            view.tool_presentations[&other]
                .process_output
                .as_ref()
                .unwrap()[0]
                .display_text(),
            "SECOND"
        );
        assert_eq!(
            view.tool_presentations[&key]
                .process_output
                .as_ref()
                .unwrap()[0]
                .display_text(),
            "FIRST"
        );
    }
}

#[test]
fn durable_running_projection_uses_the_same_default_fold_and_stream_preview() {
    let (mut app, key) = fixture("cargo build");
    chunk(&mut app, &key, Stream::Stdout, 0, &"line\n".repeat(20));
    let view = app.active_session_mut().unwrap();
    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
        index: Some(0),
        loop_id: key.loop_id.clone(),
        request_index: 0,
        tool_call_id: key.tool_call_id.clone(),
        name: "bash".into(),
        result: None,
        outcome: None,
        live_status: Some(ToolStatus::Running),
        progress: None,
        expanded: false,
    }));
    assert!(render(&app, 80).1);
    let prepared = transcript::prepare_conversation(&app, 80);
    app.install_conversation(prepared);
    let before = app.active_view().unwrap().transcript.render_revision;
    chunk(&mut app, &key, Stream::Stdout, 100, "cached-tail");
    assert!(app.active_view().unwrap().transcript.render_revision > before);
    Arc::make_mut(&mut app.active_session_mut().unwrap().tool_folds)
        .insert(key, FoldOverride::Expanded);
    let (text, folded) = render(&app, 80);
    assert!(!folded);
    assert!(text.contains("stdout:"));
    assert!(text.contains("cached-tail"));
}

#[test]
fn silent_multiline_command_folds_before_the_first_output_chunk() {
    let (app, _) = fixture(&"build\n".repeat(20));
    assert!(render(&app, 80).1);
}

#[test]
fn presentation_before_result_cannot_reduce_reported_hidden_rows() {
    let (mut app, key) = fixture("build");
    let facts = Arc::make_mut(
        Arc::make_mut(&mut app.active_session_mut().unwrap().tool_presentations)
            .get_mut(&key)
            .unwrap(),
    );
    Arc::make_mut(&mut facts.display).hidden_line_count = Some(25);
    assert!(render(&app, 80).1);
}

#[test]
fn session_budget_charges_all_live_preview_windows() {
    let (mut app, _) = fixture("build");
    let bytes = b"x\n".repeat(crate::limits::TOOL_STREAM_BYTES / 2);
    for index in 0..20 {
        let key = ToolKey::new("ses_1", "loop_live", 0, &format!("budget-{index}"));
        let facts = app.tool_facts_mut(&key, "bash").unwrap();
        for stream in [Stream::Stdout, Stream::Stderr] {
            facts.accept_process_chunk(&crate::protocol::ToolProcessChunkWire {
                stream,
                encoding: "base64".into(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                base_offset: 0,
                next_offset: bytes.len() as u64,
                observed_end: bytes.len() as u64,
                dropped: false,
                expired: false,
            });
        }
    }
    app.enforce_tool_budget();
    let facts = &app.active_view().unwrap().tool_presentations;
    assert!(
        facts
            .values()
            .all(|facts| facts.retained_bytes() <= crate::limits::TOOL_STREAM_BYTES)
    );
    assert!(
        facts
            .values()
            .map(|facts| facts.retained_bytes())
            .sum::<usize>()
            <= crate::limits::TOOL_TOTAL_BYTES
    );
    for (key, facts) in facts
        .iter()
        .filter(|(key, _)| key.tool_call_id.starts_with("budget-"))
    {
        assert_eq!(
            facts.output_line_count,
            Some(bytes.len() + 2),
            "{}",
            key.tool_call_id
        );
    }
}

#[test]
fn rendered_frame_has_one_short_command_but_preserves_clipped_and_multiline_input() {
    for width in [60, 80, 120] {
        let (mut app, key) = fixture("cargo build");
        chunk(&mut app, &key, Stream::Stdout, 0, "result");
        let (text, _) = render(&app, width);
        assert_eq!(text.matches("cargo build").count(), 1);
        Arc::make_mut(&mut app.active_session_mut().unwrap().tool_folds)
            .insert(key, FoldOverride::Collapsed);
        assert!(render(&app, width).0.contains("result"));
        assert!(!render(&app, width).0.contains("lines hidden"));
        for command in [
            "cargo build\n".to_owned(),
            "cargo build\ncargo test".to_owned(),
            format!("cargo build {}", "--verbose ".repeat(30)),
        ] {
            let (mut app, key) = fixture(&command);
            Arc::make_mut(&mut app.active_session_mut().unwrap().tool_folds)
                .insert(key, FoldOverride::Expanded);
            let prepared = transcript::prepare_conversation(&app, width);
            let section = prepared
                .sections
                .iter()
                .find(|section| section.id.kind == SectionKind::Tool)
                .unwrap();
            assert!(!section.folded);
            let body = (section.rows.start + 3..section.rows.end - 2)
                .filter_map(|row| prepared.row(row))
                .flat_map(|line| &line.spans)
                .filter(|span| span.style.fg == Some(app.theme.theme().tool_output))
                .map(|span| {
                    span.content
                        .strip_prefix("  ")
                        .unwrap_or(&span.content)
                        .trim_end()
                })
                .collect::<String>();
            // Rail padding can be omitted, but source words and every physical
            // input line must survive even when the compact target is clipped.
            assert_eq!(body.replace(' ', ""), command.replace(['\n', ' '], ""));
        }
    }
}

#[test]
fn cached_durable_stream_stays_folded_and_reuses_unrelated_history() {
    for fallback in [false, true] {
        let (mut app, key) = fixture("cargo build");
        chunk(&mut app, &key, Stream::Stdout, 0, "initial");
        let view = app.active_session_mut().unwrap();
        view.transcript.push_block(TranscriptBlock::User(UserBlock {
            index: Some(0),
            loop_id: Some("earlier".into()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "Earlier history must keep its prepared layout".repeat(100),
            pending: false,
        }));
        if fallback {
            let request = &mut view.live.as_mut().unwrap().requests[0];
            request.parts.clear();
            request.tools.clear();
            let call = crate::protocol::ToolCallViewWire {
                tool_call_id: key.tool_call_id.clone(),
                name: "bash".into(),
                call_index: 0,
                display: None,
            };
            view.transcript
                .push_block(TranscriptBlock::Assistant(AssistantBlock {
                    index: 1,
                    loop_id: key.loop_id.clone(),
                    request_index: 0,
                    model: "model".into(),
                    reasoning_level: Reasoning::High,
                    parts: vec![AssistantPart::ToolCall(call.clone())],
                    tool_calls: vec![call],
                    usage: Default::default(),
                    finish_reason: "tool_calls".into(),
                    terminal_error: None,
                }));
        } else {
            view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
                index: Some(1),
                loop_id: key.loop_id.clone(),
                request_index: 0,
                tool_call_id: key.tool_call_id.clone(),
                name: "bash".into(),
                result: None,
                outcome: None,
                live_status: Some(ToolStatus::Running),
                progress: None,
                expanded: false,
            }));
        }
        assert!(render(&app, 80).1);
        let prepared = transcript::prepare_conversation(&app, 80);
        let previous = prepared.durable.as_ref().unwrap().clone();
        app.install_conversation(prepared);
        let revision = app.active_view().unwrap().transcript.render_revision;
        let tail = "\nmore".repeat(20);
        chunk(&mut app, &key, Stream::Stdout, 7, &tail);
        assert!(app.active_view().unwrap().transcript.render_revision > revision);
        let current = transcript::prepare_conversation(&app, 80);
        let section = current
            .sections
            .iter()
            .find(|section| section.id.tool_call_id.as_deref() == Some("build"))
            .unwrap();
        assert!(section.folded);
        let text = current
            .lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("16 earlier lines"));
        let old = &previous
            .layout
            .sections
            .iter()
            .find(|section| section.layout.key.section.kind == SectionKind::User)
            .unwrap()
            .layout;
        let new = &current
            .durable
            .as_ref()
            .unwrap()
            .layout
            .sections
            .iter()
            .find(|section| section.layout.key.section.kind == SectionKind::User)
            .unwrap()
            .layout;
        assert!(
            Arc::ptr_eq(old, new),
            "unrelated durable layout should be reused"
        );
    }
}

#[test]
fn live_preview_eviction_preserves_cached_history_and_count() {
    let (mut app, key) = fixture("build");
    app.active_session_mut()
        .unwrap()
        .transcript
        .push_block(TranscriptBlock::User(UserBlock {
            index: Some(0),
            loop_id: Some("old".into()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "unchanged history".repeat(100),
            pending: false,
        }));
    let prepared = transcript::prepare_conversation(&app, 80);
    let durable = prepared.durable.as_ref().unwrap().clone();
    app.install_conversation(prepared);
    let revision = app.active_view().unwrap().transcript.render_revision;
    let page = "x\n".repeat(crate::limits::TOOL_PAGE_BYTES / 2);
    for index in 0..80 {
        chunk(
            &mut app,
            &key,
            Stream::Stdout,
            (index * page.len()) as u64,
            &page,
        );
        assert_eq!(
            app.active_view().unwrap().transcript.render_revision,
            revision
        );
        assert!(Arc::ptr_eq(
            app.active_view()
                .unwrap()
                .transcript
                .render_cache
                .as_ref()
                .unwrap(),
            &durable
        ));
    }
    let facts = &app.active_view().unwrap().tool_presentations[&key];
    assert!(facts.retained_bytes() <= crate::limits::TOOL_STREAM_BYTES);
    assert_eq!(facts.output_line_count, Some(80 * page.len() / 2 + 1));
    assert!(facts.process_output_partial());
    assert!(!facts.process_count_partial());
    assert!(Arc::ptr_eq(
        transcript::prepare_conversation(&app, 80)
            .durable
            .as_ref()
            .unwrap(),
        &durable
    ));
}

#[test]
fn late_invocation_and_terminal_execution_refresh_cached_fallback_body() {
    let (mut app, key) = fixture("build");
    Arc::make_mut(&mut app.active_session_mut().unwrap().tool_folds)
        .insert(key.clone(), FoldOverride::Expanded);
    let view = app.active_session_mut().unwrap();
    let request = &mut view.live.as_mut().unwrap().requests[0];
    request.parts.clear();
    request.tools.clear();
    let call = crate::protocol::ToolCallViewWire {
        tool_call_id: key.tool_call_id.clone(),
        name: "bash".into(),
        call_index: 0,
        display: None,
    };
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 0,
            loop_id: key.loop_id.clone(),
            request_index: 0,
            model: "model".into(),
            reasoning_level: Reasoning::High,
            parts: vec![AssistantPart::ToolCall(call.clone())],
            tool_calls: vec![call],
            usage: Default::default(),
            finish_reason: "tool_calls".into(),
            terminal_error: None,
        }));
    let prepared = transcript::prepare_conversation(&app, 80);
    app.install_conversation(prepared);
    let mut invocation = (**app.active_view().unwrap().tool_presentations[&key]
        .invocation
        .as_ref()
        .unwrap())
    .clone();
    invocation.subject = crate::protocol::ToolSubjectWire::Command {
        script: "first\nsecond".into(),
        cwd: ".".into(),
    };
    app.accept_tool_invocation(invocation);
    assert!(render(&app, 80).0.contains("second"));
    app.accept_tool_process(crate::protocol::ToolProcessWire {
        tool_ref: (&key).into(),
        command: None,
        chunk: Some(crate::protocol::ToolProcessChunkWire {
            stream: Stream::Stdout,
            encoding: "base64".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"prefix\xe4"),
            base_offset: 0,
            next_offset: 7,
            observed_end: 7,
            dropped: false,
            expired: false,
        }),
    });
    let prepared = transcript::prepare_conversation(&app, 80);
    assert!(
        !prepared
            .lines()
            .iter()
            .map(ToString::to_string)
            .collect::<String>()
            .contains('�')
    );
    app.install_conversation(prepared);
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/agent-v1/tool-read-terminal.json"
    ))
    .unwrap();
    let mut execution: crate::protocol::ToolExecutionWire =
        serde_json::from_value(fixture["result"]["execution"].clone()).unwrap();
    execution.tool_ref = (&key).into();
    execution.command = None;
    execution.output_line_count = None;
    app.accept_tool_execution(execution, false);
    let text = render(&app, 80).0;
    assert!(text.contains("prefix�"), "{text}");
    assert!(text.contains("completed"));
    assert!(
        app.active_view().unwrap().tool_presentations[&key]
            .result
            .is_none()
    );
}

#[test]
fn bash_elapsed_tick_updates_only_its_card_without_reset_or_new_output() {
    for durable in [false, true] {
        let (mut app, key) = fixture("sleep 3");
        let wire: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent-v1/tool-read-running.json"
        ))
        .unwrap();
        let mut execution: crate::protocol::ToolExecutionWire =
            serde_json::from_value(wire["result"]["execution"].clone()).unwrap();
        execution.tool_ref = (&key).into();
        let start =
            crate::state::selection::parse_rfc3339(execution.started_at.as_deref().unwrap())
                .unwrap();
        app.accept_tool_execution(execution, false);
        let view = app.active_session_mut().unwrap();
        view.scroll.follow_tail = false;
        view.scroll.new_content = false;
        view.transcript.push_block(TranscriptBlock::User(UserBlock {
            index: Some(0),
            loop_id: Some("earlier".into()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "Earlier unrelated history".into(),
            pending: false,
        }));
        if durable {
            view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
                index: Some(1),
                loop_id: key.loop_id.clone(),
                request_index: 0,
                tool_call_id: key.tool_call_id.clone(),
                name: "bash".into(),
                result: None,
                outcome: None,
                live_status: Some(ToolStatus::Running),
                progress: None,
                expanded: false,
            }));
        }
        let facts = Arc::make_mut(
            Arc::make_mut(&mut view.tool_presentations)
                .get_mut(&key)
                .unwrap(),
        );
        facts.timing = facts.bash_timing_at(start + Duration::from_millis(1250));
        let prepared = transcript::prepare_conversation(&app, 80);
        assert!(
            prepared
                .lines()
                .iter()
                .any(|line| line.to_string().contains("Elapsed 1.2s"))
        );
        let old_durable = prepared.durable.clone().unwrap();
        app.install_conversation(prepared);
        let revision = app.active_view().unwrap().transcript.render_revision;
        app.refresh_bash_timers(start + Duration::from_millis(1750));
        assert!(
            app.prepared_conversation.is_some(),
            "redraws in the same second reuse their preparation"
        );
        assert_eq!(
            app.active_view().unwrap().transcript.render_revision,
            revision
        );
        app.refresh_bash_timers(start + Duration::from_millis(2250));
        assert!(app.prepared_conversation.is_none());
        assert_eq!(
            app.active_view().unwrap().transcript.render_revision != revision,
            durable
        );
        let prepared = transcript::prepare_conversation(&app, 80);
        assert!(
            prepared
                .lines()
                .iter()
                .any(|line| line.to_string().contains("Elapsed 2.2s"))
        );
        let old_user = &old_durable
            .layout
            .sections
            .iter()
            .find(|s| s.layout.key.section.kind == SectionKind::User)
            .unwrap()
            .layout;
        let new_user = &prepared
            .durable
            .as_ref()
            .unwrap()
            .layout
            .sections
            .iter()
            .find(|s| s.layout.key.section.kind == SectionKind::User)
            .unwrap()
            .layout;
        assert!(
            Arc::ptr_eq(old_user, new_user),
            "the timer reuses unrelated history"
        );
        app.install_conversation(prepared);
        assert!(!app.active_view().unwrap().scroll.new_content);
        app.update(AppEvent::Tick);
        assert!(
            app.prepared_conversation.is_none(),
            "the existing busy Tick drives tool timers"
        );
        assert!(!app.active_view().unwrap().scroll.new_content);

        let wire: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent-v1/tool-read-terminal.json"
        ))
        .unwrap();
        let mut execution: crate::protocol::ToolExecutionWire =
            serde_json::from_value(wire["result"]["execution"].clone()).unwrap();
        execution.tool_ref = (&key).into();
        app.accept_tool_execution(execution, false);
        let prepared = transcript::prepare_conversation(&app, 80);
        assert!(
            prepared
                .lines()
                .iter()
                .any(|line| line.to_string().contains("Took 2.0s"))
        );
        app.install_conversation(prepared);
        app.refresh_bash_timers(start + Duration::from_secs(86_400));
        assert!(
            app.prepared_conversation.is_some(),
            "a completed duration stops ticking"
        );
    }
}

#[test]
fn collapsed_bash_soft_wrap_copy_and_fold_restore_full_source() {
    for durable in [false, true] {
        let (mut app, key) = fixture("printf output");
        let output = format!("PREFIX\n{}\nEND", "中👨‍👩‍👧e\u{301}".repeat(40));
        let facts = app.tool_facts_mut(&key, "bash").unwrap();
        facts.accept_finished(
            crate::protocol::ToolOutcomeWire::Success,
            Some(Arc::from(output.as_str())),
            false,
        );
        let view = app.active_session_mut().unwrap();
        if durable {
            view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
                index: Some(1),
                loop_id: key.loop_id.clone(),
                request_index: 0,
                tool_call_id: key.tool_call_id.clone(),
                name: "bash".into(),
                result: Some(Arc::from(output.as_str())),
                outcome: Some(crate::protocol::ToolOutcomeWire::Success),
                live_status: None,
                progress: None,
                expanded: false,
            }));
        }
        Arc::make_mut(&mut view.tool_folds).insert(key.clone(), FoldOverride::Collapsed);
        for width in [24, 60, 80, 120] {
            let prepared = transcript::prepare_conversation(&app, width);
            let section = prepared
                .sections
                .iter()
                .find(|s| s.id.tool_call_id.as_deref() == Some("build"))
                .unwrap();
            let rows: Vec<_> = prepared
                .copy_ranges
                .iter()
                .filter(|copy| {
                    section.rows.contains(&copy.row)
                        && !copy.decorative
                        && copy.columns.start == crate::ui::rail::SURFACE_CONTENT_START + 2
                })
                .collect();
            assert!(rows.len() <= 5);
            let mut copied = String::new();
            let mut hard_break = false;
            for row in &rows {
                if hard_break {
                    copied.push('\n');
                }
                copied.push_str(row.text);
                hard_break = row.hard_break_after;
            }
            assert!(output.ends_with(&copied), "{copied:?}");
            assert!(!copied.contains("earlier lines") && !copied.contains("ctrl+o"));
            assert_eq!(rows[0].source_offset, output.len() - copied.len());
        }
        app.update(AppEvent::ToggleTool {
            session_id: key.session_id.clone(),
            loop_id: key.loop_id.clone(),
            request_index: key.request_index,
            tool_call_id: key.tool_call_id.clone(),
        });
        let (text, folded) = render(&app, 80);
        assert!(!folded && text.contains("PREFIX") && text.contains("END"));
        assert_eq!(
            app.active_view().unwrap().tool_presentations[&key]
                .result
                .as_deref(),
            Some(output.as_str())
        );
    }
}
