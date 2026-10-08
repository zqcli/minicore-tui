//! On-demand inline bodies share the existing tool facts and bounded output stream.
use std::sync::Arc;

use super::queries::{QueryAdmission, QueryKey};
use super::{App, AppCommand, RequestKind};
use crate::protocol::{OutgoingRequest, RpcResponse, ToolDataStreamWire as Stream};
use crate::state::tool::{InlineToolLoad, StreamView, ToolKey};

impl App {
    pub(super) fn poll_inline_tools(&mut self) -> Vec<AppCommand> {
        if !self.can_send_requests() || self.has_main_detail() {
            return Vec::new();
        }
        let Some(view) = self.active_view() else {
            return Vec::new();
        };
        let Some(prepared) = self
            .prepared_conversation
            .as_ref()
            .and_then(|prepared| self.prepared_conversation(prepared.width))
        else {
            return Vec::new();
        };
        if self.viewport.1 == 0 {
            return Vec::new();
        }
        let now = self.instant_now();
        let position =
            crate::ui::transcript::scroll_position(self, prepared.total_rows(), self.viewport.1);
        let epoch = view.session_epoch;
        let keys: Vec<_> = (position.offset..position.offset + position.visible_rows)
            .filter_map(|row| {
                prepared.sections.at_row(row).filter(|section| {
                    section.id.kind == crate::state::view::SectionKind::Tool
                        && !section.folded
                        && row == section.rows.start.max(position.offset)
                })
            })
            .filter_map(|section| {
                Some(ToolKey::new(
                    &view.info.session_id,
                    section.id.loop_id.as_deref()?,
                    section.id.request_index?,
                    section.id.tool_call_id.as_deref()?,
                ))
            })
            .collect();
        let mut commands = Vec::new();
        for key in keys {
            if self.deferred_pending() + self.queries.in_flight_len()
                >= super::MAX_DEFERRED_REQUESTS
            {
                break;
            }
            let Some(facts) = self
                .sessions
                .known
                .get(&key.session_id)
                .and_then(|v| v.tool_presentations.get(&key))
            else {
                continue;
            };
            if facts.arguments_preview.is_some() || !facts.body_deferred {
                continue;
            }
            // Live cards already carrying both display and result need no recovery read.
            if facts.inline.is_none()
                && facts.result.is_some()
                && (facts.display.expanded_input.is_some()
                    || facts.display.input_line_count == Some(0))
            {
                continue;
            }
            if facts.inline.as_ref().is_some_and(|load| {
                load.epoch == epoch
                    && (load.pending || load.error.is_some() || load.output.eof || load.due > now)
            }) {
                continue;
            }
            self.tool_generation = self.tool_generation.wrapping_add(1);
            let generation = self.tool_generation;
            let facts = self
                .tool_facts_mut(&key, "tool")
                .expect("visible tool facts");
            if facts.inline.as_ref().is_none_or(|load| load.epoch != epoch) {
                facts.inline = Some(InlineToolLoad {
                    epoch,
                    generation,
                    read: false,
                    pending: false,
                    due: now,
                    error: None,
                    output: StreamView::new(Stream::Output),
                });
            }
            let load = facts.inline.as_ref().unwrap();
            let generation = load.generation;
            let output = load.read;
            let offset = load.output.next_offset;
            let id = self.next_request_id();
            if self
                .queries
                .request_query(QueryKey::Tool { key: key.clone() }, id)
                != QueryAdmission::Admitted
            {
                continue;
            }
            let request = if output {
                OutgoingRequest::tool_output(id, &(&key).into(), Stream::Output, offset)
            } else {
                OutgoingRequest::tool_read_display(id, &(&key).into())
            };
            self.tool_facts_mut(&key, "tool")
                .unwrap()
                .inline
                .as_mut()
                .unwrap()
                .pending = true;
            self.pending_requests.insert(
                id,
                RequestKind::ToolInline {
                    key,
                    epoch,
                    generation,
                    output,
                },
            );
            commands.push(AppCommand::Rpc(request));
        }
        commands
    }

    pub(super) fn inline_tool_failed(&mut self, key: &ToolKey, generation: u64, error: &str) {
        if let Some(load) = self
            .tool_facts_mut(key, "tool")
            .and_then(|facts| facts.inline.as_mut())
        {
            if load.generation == generation {
                load.pending = false;
                load.error = Some(error.to_owned());
            }
        }
    }

    pub(super) fn on_inline_tool_response(
        &mut self,
        key: ToolKey,
        epoch: u64,
        generation: u64,
        output: bool,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let valid = self
            .sessions
            .known
            .get(&key.session_id)
            .is_some_and(|view| {
                view.session_epoch == epoch
                    && view
                        .tool_presentations
                        .get(&key)
                        .and_then(|f| f.inline.as_ref())
                        .is_some_and(|load| load.generation == generation)
            });
        if !valid {
            return Vec::new();
        }
        if self.sessions.active.as_ref() != Some(&key.session_id) {
            self.inline_tool_failed(&key, generation, "tool load interrupted by session switch");
            return Vec::new();
        }
        let now = self.instant_now();
        let result = if output {
            response
                .parse_tool_output()
                .map_err(|e| e.to_string())
                .and_then(|page| {
                    if ToolKey::from(&page.tool_ref) != key || page.stream != Stream::Output {
                        return Err("tool output identity mismatch".to_owned());
                    }
                    use crate::protocol::ToolDataAvailabilityWire as Availability;
                    match page.availability {
                        Availability::Expired => return Err("Output expired".to_owned()),
                        Availability::Unavailable => return Err("Output unavailable".to_owned()),
                        Availability::Pending => {
                            let load = self
                                .tool_facts_mut(&key, "tool")
                                .unwrap()
                                .inline
                                .as_mut()
                                .unwrap();
                            load.read = false;
                            load.due = now + std::time::Duration::from_millis(500);
                            return Ok(());
                        }
                        _ => {}
                    }
                    let facts = self.tool_facts_mut(&key, "tool").unwrap();
                    let retained_output = facts.inline.as_ref().unwrap().output.capacity_bytes();
                    let room = crate::limits::TOOL_STREAM_BYTES
                        .saturating_sub(facts.retained_bytes().saturating_sub(retained_output));
                    let load = facts.inline.as_mut().unwrap();
                    let previous = load.output.next_offset;
                    load.output.accept_page(&page).map_err(str::to_owned)?;
                    // Stop at the same one-stream budget as the existing detail view.
                    let capped = load.output.capacity_bytes() >= room;
                    if load.output.eof || capped {
                        let mut text = load.output.display_text();
                        let mut end = room.min(text.len());
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        text.truncate(end);
                        facts.result = Some(Arc::from(text));
                        facts.result_truncated |= load.output.truncated
                            || load.output.gap
                            || capped
                            || page.availability == Availability::Partial;
                        load.output.chunks.clear();
                        load.output.retained_bytes = 0;
                        load.output.eof = true;
                    } else if load.output.next_offset == previous {
                        return Err("tool output did not advance".to_owned());
                    }
                    Ok(())
                })
        } else {
            response
                .parse_tool_read()
                .map_err(|e| e.to_string())
                .and_then(|read| {
                    if ToolKey::from(&read.execution.tool_ref) != key
                        || read
                            .invocation
                            .as_ref()
                            .is_some_and(|inv| ToolKey::from(&inv.tool_ref) != key)
                    {
                        return Err("tool read identity mismatch".to_owned());
                    }
                    if let Some(invocation) = read.invocation {
                        self.accept_tool_invocation(invocation);
                    }
                    self.accept_tool_execution(read.execution, true);
                    let facts = self.tool_facts_mut(&key, "tool").unwrap();
                    if let Some(display) = read.display {
                        facts.display = Arc::new(display);
                    }
                    facts.inline.as_mut().unwrap().read = true;
                    Ok(())
                })
        };
        let facts = self.tool_facts_mut(&key, "tool").unwrap();
        let load = facts.inline.as_mut().unwrap();
        load.pending = false;
        load.error = result.err();
        if let Some(view) = self.sessions.known.get_mut(&key.session_id) {
            view.transcript.invalidate();
        }
        self.prepared_conversation = None;
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AppEvent;
    use crate::protocol::{RequestId, ToolRefWire};
    use crate::ui::testapp::{respond, take_requests};
    use serde_json::{Value, json};

    fn fixture() -> (App, ToolKey) {
        let mut app =
            crate::ui::testapp::open_empty(crate::theme::ThemeKind::Dark, "ses_1", None, "high");
        app.terminal_size = (80, 24);
        let key = ToolKey::new("ses_1", "l", 0, "c");
        let item = crate::protocol::read::decode_item(&json!({
            "display":true,"item":{"type":"tool_result","data":{
                "loop_id":"l","request_index":0,"call_id":"c","tool_name":"write","outcome":"success"}},
            "tool_summaries":[{"tool_ref":ToolRefWire::from(&key),"tool_call_id":"c","name":"write",
                "display":{"detail":"a.rs","input_line_count":2},"output_line_count":1,
                "output_truncated":false,"count_state":"exact"}]
        }).to_string()).unwrap();
        crate::app::install_history_item(app.sessions.known.get_mut("ses_1").unwrap(), 0, &item)
            .unwrap();
        prepare(&mut app);
        (app, key)
    }

    fn prepare(app: &mut App) {
        let screen =
            crate::ui::layout::screen_layout(app, ratatui::layout::Rect::new(0, 0, 80, 24));
        let prepared = crate::ui::transcript::prepare_conversation(app, screen.content.width);
        app.viewport = (prepared.total_rows(), screen.transcript.height as usize);
        app.install_conversation(prepared);
    }

    fn toggle(app: &mut App, key: &ToolKey) {
        app.update(AppEvent::ToggleTool {
            session_id: key.session_id.clone(),
            loop_id: key.loop_id.clone(),
            request_index: key.request_index,
            tool_call_id: key.tool_call_id.clone(),
        });
        prepare(app);
    }

    fn read(key: &ToolKey) -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent-v1/tool-read-terminal.json"
        ))
        .unwrap();
        let mut value = fixture["result"].clone();
        value["execution"]["tool_ref"] = json!(ToolRefWire::from(key));
        value["execution"]["name"] = json!("write");
        value["execution"]["output_line_count"] = json!(1);
        value["invocation"] = Value::Null;
        value["display"] = json!({"detail":"a.rs","expanded_input":"one\ntwo","input_line_count":2,
            "hidden_line_count":3,"truncated":false,"body_truncated":false});
        value
    }

    fn page(key: &ToolKey, availability: &str, body: &str) -> Value {
        json!({"tool_ref":ToolRefWire::from(key),"stream":"output","encoding":"utf8","data":body,
            "base_offset":0,"next_offset":body.len(),"observed_end":body.len(),"eof":true,
            "truncated":false,"availability":availability})
    }

    #[test]
    fn summary_is_folded_until_clicked_then_loads_once_and_reuses_empty_eof() {
        let (mut app, key) = fixture();
        assert!(app.poll_inline_tools().is_empty());
        assert!(
            app.sessions.known["ses_1"].tool_presentations[&key]
                .result
                .is_none()
        );
        toggle(&mut app, &key);
        let requests = take_requests(app.poll_inline_tools());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "tool.read");
        assert_eq!(requests[0].params["display"], true);
        assert!(app.poll_inline_tools().is_empty());
        respond(&mut app, &requests[0], read(&key));
        prepare(&mut app);
        let output = take_requests(app.poll_inline_tools());
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].method, "tool.output");
        respond(&mut app, &output[0], page(&key, "available", ""));
        let facts = &app.sessions.known["ses_1"].tool_presentations[&key];
        assert_eq!(facts.result.as_deref(), Some(""));
        assert_eq!(facts.display.expanded_input.as_deref(), Some("one\ntwo"));
        toggle(&mut app, &key);
        toggle(&mut app, &key);
        assert!(app.poll_inline_tools().is_empty());
    }

    #[test]
    fn expired_is_not_empty_success_and_both_fold_actions_retry_errors() {
        let (mut app, key) = fixture();
        let epoch = app.sessions.known["ses_1"].session_epoch;
        let now = app.instant_now();
        app.tool_facts_mut(&key, "write").unwrap().inline = Some(InlineToolLoad {
            epoch,
            generation: 1,
            read: true,
            pending: true,
            due: now,
            error: None,
            output: StreamView::new(Stream::Output),
        });
        app.on_inline_tool_response(
            key.clone(),
            epoch,
            1,
            true,
            &RpcResponse {
                id: RequestId(999),
                result: Some(page(&key, "expired", "")),
                error: None,
            },
        );
        let facts = &app.sessions.known["ses_1"].tool_presentations[&key];
        assert!(facts.result.is_none());
        assert_eq!(
            facts.inline.as_ref().unwrap().error.as_deref(),
            Some("Output expired")
        );
        let before = crate::ui::tool::facts_revision(facts);
        toggle(&mut app, &key);
        assert!(
            app.sessions.known["ses_1"].tool_presentations[&key]
                .inline
                .as_ref()
                .unwrap()
                .error
                .is_none()
        );
        let generation = app.sessions.known["ses_1"].tool_presentations[&key]
            .inline
            .as_ref()
            .unwrap()
            .generation;
        app.inline_tool_failed(&key, generation, "send failed");
        app.update(AppEvent::ToggleTools {
            session_id: "ses_1".into(),
        });
        app.update(AppEvent::ToggleTools {
            session_id: "ses_1".into(),
        });
        let facts = &app.sessions.known["ses_1"].tool_presentations[&key];
        assert!(facts.inline.as_ref().unwrap().error.is_none());
        assert_ne!(before, crate::ui::tool::facts_revision(facts));
    }
    #[test]
    fn inline_pages_join_once_and_cap_without_restarting_from_zero() {
        let (mut app, key) = fixture();
        let epoch = app.sessions.known["ses_1"].session_epoch;
        let now = app.instant_now();
        app.tool_facts_mut(&key, "write").unwrap().inline = Some(InlineToolLoad {
            epoch,
            generation: 1,
            read: true,
            pending: true,
            due: now,
            error: None,
            output: StreamView::new(Stream::Output),
        });
        let mut first = page(&key, "available", "one\n");
        first["eof"] = json!(false);
        app.on_inline_tool_response(
            key.clone(),
            epoch,
            1,
            true,
            &RpcResponse {
                id: RequestId(99),
                result: Some(first),
                error: None,
            },
        );
        assert!(
            app.sessions.known["ses_1"].tool_presentations[&key]
                .result
                .is_none()
        );
        let mut second = page(&key, "partial", "two");
        second["base_offset"] = json!(4);
        second["next_offset"] = json!(7);
        second["observed_end"] = json!(7);
        app.on_inline_tool_response(
            key.clone(),
            epoch,
            1,
            true,
            &RpcResponse {
                id: RequestId(99),
                result: Some(second),
                error: None,
            },
        );
        assert_eq!(
            app.sessions.known["ses_1"].tool_presentations[&key]
                .result
                .as_deref(),
            Some("one\ntwo")
        );
        assert!(app.sessions.known["ses_1"].tool_presentations[&key].result_truncated);
        let commands = app.open_search("two".into(), crate::state::search::SearchScope::Loaded);
        assert!(
            commands
                .iter()
                .all(|command| !matches!(command, AppCommand::Rpc(_)))
        );
        let request = commands
            .into_iter()
            .find_map(|command| match command {
                AppCommand::LocalScan(request) => Some(request),
                _ => None,
            })
            .unwrap();
        assert!(
            crate::state::search::run_local_scan(&request)
                .matches
                .iter()
                .any(|result| result.source == crate::state::search::SearchSource::ToolResult)
        );
        app.close_search();

        let facts = app.tool_facts_mut(&key, "write").unwrap();
        facts.result = None;
        facts.inline.as_mut().unwrap().output = StreamView::new(Stream::Output);
        let bytes = "x".repeat(crate::limits::TOOL_PAGE_BYTES);
        for offset in (0..crate::limits::TOOL_STREAM_BYTES).step_by(bytes.len()) {
            let mut next = page(&key, "available", &bytes);
            next["base_offset"] = json!(offset);
            next["next_offset"] = json!(offset + bytes.len());
            next["observed_end"] = json!(crate::limits::TOOL_STREAM_BYTES * 2);
            next["eof"] = json!(false);
            app.on_inline_tool_response(
                key.clone(),
                epoch,
                1,
                true,
                &RpcResponse {
                    id: RequestId(99),
                    result: Some(next),
                    error: None,
                },
            );
            if app.sessions.known["ses_1"].tool_presentations[&key]
                .inline
                .as_ref()
                .unwrap()
                .output
                .eof
            {
                break;
            }
        }
        let facts = &app.sessions.known["ses_1"].tool_presentations[&key];
        assert!(facts.result_truncated);
        assert!(facts.retained_bytes() <= crate::limits::TOOL_STREAM_BYTES);
        assert!(facts.inline.as_ref().unwrap().output.eof);
        toggle(&mut app, &key);
        assert!(app.poll_inline_tools().is_empty());
    }

    #[test]
    fn switched_session_late_page_cannot_install_body_or_leave_pending() {
        let (mut app, key) = fixture();
        let epoch = app.sessions.known["ses_1"].session_epoch;
        let now = app.instant_now();
        app.tool_facts_mut(&key, "write").unwrap().inline = Some(InlineToolLoad {
            epoch,
            generation: 1,
            read: true,
            pending: true,
            due: now,
            error: None,
            output: StreamView::new(Stream::Output),
        });
        app.pending_requests.insert(
            RequestId(99),
            RequestKind::ToolInline {
                key: key.clone(),
                epoch,
                generation: 1,
                output: true,
            },
        );
        app.retire_session_operations(&key.session_id);
        assert!(
            !app.sessions.known["ses_1"].tool_presentations[&key]
                .inline
                .as_ref()
                .unwrap()
                .pending
        );
        app.sessions.active = Some("different".into());
        app.on_inline_tool_response(
            key.clone(),
            epoch,
            1,
            true,
            &RpcResponse {
                id: RequestId(99),
                result: Some(page(&key, "available", "WRONG SESSION BODY")),
                error: None,
            },
        );
        let facts = &app.sessions.known["ses_1"].tool_presentations[&key];
        assert!(facts.result.is_none());
        assert!(!facts.inline.as_ref().unwrap().pending);
        assert!(facts.inline.as_ref().unwrap().error.is_some());
        app.sessions.active = Some("ses_1".into());
        toggle(&mut app, &key);
        assert!(
            app.sessions.known["ses_1"].tool_presentations[&key]
                .inline
                .as_ref()
                .unwrap()
                .error
                .is_none()
        );
    }
}
