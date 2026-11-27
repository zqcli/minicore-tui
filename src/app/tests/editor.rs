use super::harness::*;
use crate::app::{Action, Overlay, Panel};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn mouse(h: &mut Harness, kind: MouseEventKind, x: u16, y: u16) {
    assert!(h.app.on_mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE
    }));
}

#[tokio::test(start_paused = true)]
async fn editor_click_drag_shift_replace_and_undo_work_through_real_layout() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("ab中文cd\nsecond line");
    h.screen(60, 24);
    let area = h.app.composer_area;
    assert_eq!(area.height, 4);
    let y = area.y + 1;
    let x = area.x + 1; // skip the rail
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), x + 2, y);
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x + 2, y);
    assert_eq!(h.app.composer.cursor(), 2);
    assert!(h.app.composer.selection().is_none());
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), x + 2, y);
    mouse(&mut h, MouseEventKind::Drag(MouseButton::Left), x + 6, y);
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x + 6, y);
    assert_eq!(h.app.composer.selected_text(), Some("中文"));
    assert!(
        h.app.selection.is_none(),
        "editor drag never copies transcript text"
    );
    assert!(h.app.press.is_none());
    h.type_text("X");
    assert_eq!(h.app.composer.text(), "abXcd\nsecond line");
    h.ctrl(KeyCode::Char('z'));
    assert_eq!(h.app.composer.selected_text(), Some("中文"));
    h.press(KeyCode::Right);
    assert!(h.app.composer.selection().is_none());
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("文"));
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("中文"));
    h.key(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("文"));
    h.press(KeyCode::Delete);
    assert_eq!(h.app.composer.text(), "ab中cd\nsecond line");
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn editor_shift_selection_crosses_lines_without_changing_prompt_history() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("a中\nsecond");
    h.key(KeyCode::Home, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("second"));
    h.key(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("a中\nsecond"));
    h.key(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.app.composer.selected_text(), Some("a中\nsecond"));
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "a中\nsecond");
    assert!(h.app.composer.selection().is_none());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn completion_rows_and_fullscreen_sheets_are_not_editing_hit_targets() {
    let mut h = Harness::opened().await;
    h.type_text("/di");
    h.screen(80, 24);
    assert!(matches!(h.app.overlay, Overlay::Palette { .. }));
    let body = h.app.composer_area;
    mouse(
        &mut h,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y + 1,
    );
    mouse(
        &mut h,
        MouseEventKind::Up(MouseButton::Left),
        body.x + 1,
        body.y + 1,
    );
    assert_eq!(h.app.composer.cursor(), 0);
    let cursor = h.app.composer.cursor();
    mouse(
        &mut h,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 3,
        body.bottom(),
    );
    assert_eq!(
        h.app.composer.cursor(),
        cursor,
        "completion list is not text"
    );
    h.app.open_panel(Panel::Help);
    h.screen(80, 24);
    assert!(h.app.composer_area.is_empty());
    let before = h.app.composer.text().to_owned();
    h.app.on_paste("not in hidden draft");
    assert_eq!(h.app.composer.text(), before);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn dragging_outside_editor_scrolls_long_drafts_and_keeps_the_anchor() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text(&"line\n".repeat(20));
    h.screen(60, 24);
    let area = h.app.composer_area;
    let x = area.x + 1;
    mouse(
        &mut h,
        MouseEventKind::Down(MouseButton::Left),
        x,
        area.bottom() - 1,
    );
    let anchor = h.app.composer.cursor();
    for _ in 0..25 {
        mouse(
            &mut h,
            MouseEventKind::Drag(MouseButton::Left),
            x,
            area.y - 1,
        );
        h.screen(60, 24);
    }
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x, area.y - 1);
    assert_eq!(h.app.composer.cursor(), 0);
    assert_eq!(h.app.composer.selection(), Some(0..anchor));
    assert_eq!(h.app.scroll, crate::app::Scroll::Follow);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_multiline_paste_is_draft_text_and_never_runs_a_command() {
    for text in ["/new\r\nhello\n", "first\nsecond\n"] {
        let mut h = Harness::opened().await;
        let client = h.native.client.session(h.native.target.clone());
        let before_agents = h.native.client.agents(Default::default()).await.unwrap();
        let before = client.state(Default::default()).await.unwrap();
        let before_history = crate::history::HistoryWindow::tail(&client, 60)
            .await
            .unwrap();
        h.app.on_paste(text);
        assert_eq!(h.app.composer.text(), text.replace("\r\n", "\n"));
        let agents = h.native.client.agents(Default::default()).await.unwrap();
        let state = client.state(Default::default()).await.unwrap();
        let history = crate::history::HistoryWindow::tail(&client, 60)
            .await
            .unwrap();
        let queue = client.inputs(Default::default()).await.unwrap();
        assert_eq!(agents.len(), before_agents.len());
        assert_eq!(agents[0].sessions.len(), before_agents[0].sessions.len());
        assert_eq!(
            state.data.controller_revision,
            before.data.controller_revision
        );
        assert_eq!(state.data.runner_revision, before.data.runner_revision);
        assert_eq!(history.upper_bound, before_history.upper_bound);
        assert!(queue.next_step.is_empty() && queue.next_turn.is_empty());
        assert!(state.data.pending_interactions.is_empty());
        assert!(h.app.local_inputs.is_empty());
        assert!(h.native.calls.try_recv().is_err());
        h.finish().await;
    }
}

#[tokio::test(start_paused = true)]
async fn follow_up_keys_start_idle_sessions_and_queue_distinct_running_turns() {
    use minicore_host_protocol::MessageMode;

    fn user_texts(call: &crate::testing::ModelCall) -> Vec<&str> {
        call.users()
            .into_iter()
            .map(|text| text.split_once("\n\n").expect("native input provenance").1)
            .collect()
    }

    for (code, modifiers) in [
        (KeyCode::Enter, KeyModifiers::ALT),
        (KeyCode::Enter, KeyModifiers::CONTROL),
        #[cfg(unix)]
        (
            KeyCode::Char('j'),
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        ),
    ] {
        let mut h = Harness::opened().await;
        h.key(code, modifiers);
        assert!(h.app.local_inputs.is_empty(), "an empty draft never sends");
        h.app.composer.set_text("idle follow-up");
        h.key(code, modifiers);
        assert_eq!(h.app.local_inputs[0].mode, MessageMode::FollowUp);
        let first = h.next_call().await;
        assert_eq!(user_texts(&first), ["idle follow-up"]);
        h.until(|app| app.is_busy() && app.local_inputs.is_empty())
            .await;

        h.app.composer.set_text("queued follow-up");
        h.key(code, modifiers);
        let message_id = h.app.local_inputs[0].message_id.clone();
        h.until(|app| {
            app.local_inputs.is_empty()
                && app
                    .resident
                    .current()
                    .unwrap()
                    .queue
                    .as_ref()
                    .is_some_and(|queue| queue.next_turn.len() == 1)
        })
        .await;
        let queue = h
            .native
            .client
            .session(h.native.target.clone())
            .inputs(Default::default())
            .await
            .unwrap();
        assert!(queue.next_step.is_empty());
        assert_eq!(queue.next_turn.len(), 1);
        assert_eq!(queue.next_turn[0].input.mode, MessageMode::FollowUp);
        assert_eq!(queue.next_turn[0].input.text, "queued follow-up");
        assert_eq!(queue.next_turn[0].input.message_id, message_id);
        assert!(h.native.calls.try_recv().is_err());

        let first_loop = first.context.loop_id;
        first.text("first answer");
        first.finish();
        let follow = h.next_call().await;
        assert_ne!(follow.context.loop_id, first_loop);
        assert_eq!(follow.context.request_index, 0);
        assert_eq!(user_texts(&follow), ["idle follow-up", "queued follow-up"]);
        follow.text("follow-up answer");
        follow.finish();
        h.until(|app| {
            !app.is_busy()
                && app
                    .transcript_blocks()
                    .iter()
                    .filter(|block| matches!(block, crate::transcript::Block::TurnEnd { .. }))
                    .count()
                    == 2
        })
        .await;
        h.finish().await;
    }
}

#[tokio::test(start_paused = true)]
async fn follow_up_alias_preserves_newlines_paste_and_focused_editors() {
    let mut h = Harness::opened().await;
    for (code, modifiers) in [
        (KeyCode::Char('j'), KeyModifiers::CONTROL),
        (KeyCode::Enter, KeyModifiers::SHIFT),
        #[cfg(windows)]
        (
            KeyCode::Char('j'),
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        ),
    ] {
        h.app.composer.set_text("draft");
        h.key(code, modifiers);
        assert_eq!(h.app.composer.text(), "draft\n");
    }
    assert!(matches!(
        h.app.on_key(KeyEvent::from(KeyCode::Enter)),
        Some(Action::Submit { follow: false })
    ));
    h.app.composer.set_text("draft");
    h.app.on_paste("\x1b\n中文\n");
    assert_eq!(h.app.composer.text(), "draft\n中文\n");

    let legacy = KeyEvent::new(
        KeyCode::Char('j'),
        KeyModifiers::ALT | KeyModifiers::CONTROL,
    );
    #[cfg(unix)]
    for (text, overlay) in [("/help", Overlay::Palette { selected: 0 })] {
        h.app.composer.set_text(text);
        h.app.overlay = overlay;
        assert!(matches!(
            h.app.on_key(legacy),
            Some(Action::Submit { follow: true })
        ));
        assert_eq!(h.app.composer.text(), text);
    }
    h.app.composer.set_text("retained draft");
    for panel in [Panel::Help, Panel::Sessions, Panel::Find, Panel::Files] {
        h.app.open_panel(panel);
        assert!(h.app.on_key(legacy).is_none());
        assert!(!h.app.overlay.is_none());
        assert_eq!(h.app.composer.text(), "retained draft");
    }
    h.app.overlay = Overlay::Unconfirmed {
        selected: 0,
        mode: crate::app::unconfirmed::ReviewMode::Edit(0),
    };
    h.app.unconfirmed_editor.set_text("review draft");
    assert!(h.app.on_key(legacy).is_none());
    assert_eq!(h.app.unconfirmed_editor.text(), "review draft\n");
    assert_eq!(h.app.composer.text(), "retained draft");
    assert!(h.app.local_inputs.is_empty());
    assert!(h.native.calls.try_recv().is_err());
    h.finish().await;
}

fn query_state(overlay: &Overlay) -> (&str, usize) {
    match overlay {
        Overlay::Find { query, current, .. } => (query, *current),
        Overlay::Sessions {
            filter, selected, ..
        }
        | Overlay::Models { filter, selected }
        | Overlay::Agents { filter, selected } => (filter, *selected),
        _ => panic!("query overlay closed unexpectedly"),
    }
}

#[tokio::test(start_paused = true)]
async fn query_paste_normalizes_single_line_text_on_all_four_surfaces() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("retained 中文 draft");
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    let cursor = h.app.composer.cursor();
    let selection = h.app.composer.selection();
    let version = h.app.composer.version();
    let pasted = "\r\n中\r文\n👩‍💻\te\u{301}\u{1b}[31mred\u{1b}[0m\u{1b}]52;c;cGF5bG9hZA==\u{7}\u{feff}\u{7}\u{7f}\u{202e}";
    let expected = "prefix 中 文 👩‍💻    e\u{301}red␇␡\\u{202e}";
    for panel in [Panel::Find, Panel::Sessions, Panel::Models, Panel::Agents] {
        h.app.open_panel(panel.clone());
        h.type_text("prefix");
        h.app.on_paste(pasted);
        assert_eq!(query_state(&h.app.overlay), (expected, 0));
        h.press(KeyCode::Esc);
        assert_eq!(h.app.composer.text(), "retained 中文 draft");
        assert_eq!(h.app.composer.cursor(), cursor);
        assert_eq!(h.app.composer.selection(), selection);
        assert_eq!(h.app.composer.version(), version);
        h.app.open_panel(panel);
        h.type_text(expected);
        assert_eq!(query_state(&h.app.overlay), (expected, 0));
        h.press(KeyCode::Esc);
    }
    assert!(h.app.local_inputs.is_empty());
    assert!(h.native.calls.try_recv().is_err());
    assert!(!h.app.quit);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn session_query_paste_targets_native_identity_and_delete_confirms_that_target() {
    let mut h = Harness::opened().await;
    h.another_session().await;
    h.app.refresh_catalogs();
    h.until(|app| app.sessions.len() == 2).await;
    h.app.composer.set_text("keep this draft");
    h.ctrl(KeyCode::Char('r'));
    h.press(KeyCode::Down);
    let target = crate::app::overlay::session_target(h.app.selected_session().unwrap());
    h.ctrl(KeyCode::Char('d'));
    assert!(
        matches!(&h.app.overlay, Overlay::Confirm { action: Action::DeleteSession(confirmed), .. } if *confirmed == target)
    );
    // Only y deletes; n leaves the session in place.
    h.press(KeyCode::Char('n'));
    assert!(matches!(h.app.overlay, Overlay::None));
    h.ctrl(KeyCode::Char('r'));
    h.app.on_paste("\u{1b}[2J\u{feff}");
    assert_eq!(query_state(&h.app.overlay).0, "");

    let id = target.session_id.as_str();
    h.app.on_paste(&id[..6]);
    assert_eq!(query_state(&h.app.overlay), (&id[..6], 0));
    h.app.on_paste(&id[6..]);
    assert_eq!(query_state(&h.app.overlay), (id, 0));
    assert_eq!(
        crate::app::overlay::session_target(h.app.selected_session().unwrap()),
        target
    );
    assert_eq!(h.app.resident.current().unwrap().address(), h.native.target);

    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "keep this draft");
    h.ctrl(KeyCode::Char('r'));
    h.type_text(id);
    assert_eq!(query_state(&h.app.overlay), (id, 0));
    let action = h
        .app
        .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::OpenSession(found)) if found == target));
    h.native
        .client
        .session(target)
        .info(Default::default())
        .await
        .unwrap();
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn agent_and_model_query_paste_only_selects_the_matching_native_values() {
    use minicore_host_core::{ModelDescription, OpenAiModelApi};
    use minicore_host_protocol::{AgentConfig, AgentCreate, OperationId, StorageMode};

    let mut h = Harness::opened().await;
    let created = h
        .native
        .client
        .create_agent(
            AgentCreate {
                operation_id: OperationId::new().unwrap(),
                name: "Paste 中文 Agent".into(),
                mode: StorageMode::Memory,
                config: AgentConfig::default(),
                default_session: None,
                initial_message: None,
            },
            Default::default(),
        )
        .await
        .unwrap();
    let agent_id = created.agent_id.unwrap();
    h.app.refresh_catalogs();
    h.until(|app| app.agents.len() == 2).await;
    h.app.composer.set_text("retained draft");
    h.app.open_panel(Panel::Agents);
    h.app.on_paste("Paste ");
    h.app.on_paste("中文");
    assert_eq!(query_state(&h.app.overlay), ("Paste 中文", 0));
    let action = h
        .app
        .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::SelectAgent(found)) if found == agent_id));
    assert_eq!(h.app.current_agent_id(), Some(&h.native.target.agent_id));

    // Native catalog values exercise filtering only; no model execution is claimed.
    h.app.models = ["fixture/base", "fixture/chat-alt"]
        .into_iter()
        .map(|model| ModelDescription {
            model: model.parse().unwrap(),
            api: OpenAiModelApi::ChatCompletions,
            context_window: 8192,
            supported_reasoning: Vec::new(),
            supports_tools: true,
            web_search: None,
        })
        .collect();
    h.app.open_panel(Panel::Models);
    h.press(KeyCode::Down);
    assert_eq!(query_state(&h.app.overlay), ("", 1));
    h.app.on_paste("\u{1b}]0;ignored\u{7}\u{feff}");
    assert_eq!(query_state(&h.app.overlay), ("", 1));
    h.app.on_paste("fixture/");
    h.app.on_paste("chat-alt");
    assert_eq!(query_state(&h.app.overlay), ("fixture/chat-alt", 0));
    let action = h
        .app
        .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::SwitchModel(model)) if model == "fixture/chat-alt"));
    assert_eq!(h.app.composer.text(), "retained draft");
    assert!(h.native.calls.try_recv().is_err());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn find_query_paste_appends_and_recomputes_actual_transcript_matches() {
    let mut h = Harness::opened().await;
    h.type_text("show fixture");
    h.press(KeyCode::Enter);
    let call = h.next_call().await;
    call.text("Search 中文 token once\nSearch 中文 token twice\nSearch elsewhere");
    call.finish();
    h.until(|app| {
        !app.is_busy()
            && app
                .transcript_blocks()
                .iter()
                .any(|block| matches!(block, crate::transcript::Block::TurnEnd { .. }))
    })
    .await;
    h.app.composer.set_text("keep my draft");
    h.screen(80, 24);
    h.ctrl(KeyCode::Char('f'));
    h.app.on_paste("Search ");
    assert!(
        matches!(&h.app.overlay, Overlay::Find { matches, current: 0, .. } if matches.len() == 3)
    );
    h.press(KeyCode::Enter);
    assert_eq!(query_state(&h.app.overlay), ("Search ", 1));
    let scroll = h.app.scroll;
    h.app.on_paste("\u{1b}[0m\u{feff}");
    assert_eq!(query_state(&h.app.overlay), ("Search ", 1));
    assert_eq!(h.app.scroll, scroll);
    h.app.on_paste("中文 token");
    assert_eq!(query_state(&h.app.overlay), ("Search 中文 token", 0));
    let Overlay::Find { matches, .. } = &h.app.overlay else {
        unreachable!()
    };
    assert_eq!(matches.len(), 2);
    let pasted_matches: Vec<_> = matches
        .iter()
        .map(|found| (found.block, found.field, found.ordinal))
        .collect();
    assert!(
        matches!(h.app.scroll, crate::app::Scroll::Anchor { block, .. } if block == matches[0].block)
    );
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "keep my draft");
    h.ctrl(KeyCode::Char('f'));
    h.type_text("Search 中文 token");
    let Overlay::Find {
        matches, current, ..
    } = &h.app.overlay
    else {
        unreachable!()
    };
    assert_eq!(*current, 0);
    assert_eq!(
        matches
            .iter()
            .map(|found| (found.block, found.field, found.ordinal))
            .collect::<Vec<_>>(),
        pasted_matches
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn query_paste_dispatch_preserves_other_editors_and_read_only_boundaries() {
    let mut h = Harness::opened().await;
    h.type_text("/");
    h.app.on_paste("help");
    assert_eq!(h.app.composer.text(), "/help");
    assert!(matches!(h.app.overlay, Overlay::Palette { .. }));
    h.app.composer.set_text("@");
    h.app.overlay = Overlay::Files {
        selected: 1,
        browser: Default::default(),
    };
    h.app.on_paste("src");
    assert_eq!(h.app.composer.text(), "@");
    assert!(
        matches!(&h.app.overlay, Overlay::Files { selected: 0, browser } if browser.filter == "src")
    );

    h.app.overlay = Overlay::Unconfirmed {
        selected: 0,
        mode: crate::app::unconfirmed::ReviewMode::Edit(0),
    };
    h.app.on_paste("line one\r\nline two");
    assert_eq!(h.app.unconfirmed_editor.text(), "line one\nline two");
    assert_eq!(h.app.composer.text(), "@");
    for panel in [
        Panel::Help,
        Panel::Error,
        Panel::SessionInfo,
        Panel::Reasoning,
    ] {
        h.app.open_panel(panel);
        h.app.on_paste("/new\ny\n");
        assert_eq!(h.app.composer.text(), "@");
    }
    h.app.open_panel(Panel::Sessions);
    h.ctrl(KeyCode::Char('n'));
    let Overlay::Sessions {
        renaming: Some((target, title)),
        ..
    } = &h.app.overlay
    else {
        unreachable!()
    };
    let before = (target.clone(), title.clone());
    h.app.on_paste("unrequested rename change");
    assert!(
        matches!(&h.app.overlay, Overlay::Sessions { filter, renaming: Some(value), .. } if filter.is_empty() && *value == before)
    );
    h.app.open_panel(Panel::Find);
    h.app.fatal = Some(crate::app::Fatal {
        reason: "terminal fixture failure".into(),
    });
    h.app.on_paste("blocked query");
    assert_eq!(query_state(&h.app.overlay), ("", 0));
    assert_eq!(h.app.composer.text(), "@");
    assert!(h.native.calls.try_recv().is_err());
    h.finish().await;
}

#[tokio::test]
async fn paste_and_follow_up_alias_keep_native_interaction_focus() {
    use crate::app::Answer;
    use crate::testing::{production::with_input_tool, real_bounded};

    let (native, mut provider) = real_bounded(with_input_tool()).await;
    let mut h = Harness::from_native(native).await;
    h.type_text("request interactive fixture tool");
    h.press(KeyCode::Enter);
    h.until(|app| app.has_interaction()).await;
    provider.request().await;
    h.app.composer.set_text("retained composer");
    h.app.on_paste("y\n");
    h.key(
        KeyCode::Char('j'),
        KeyModifiers::ALT | KeyModifiers::CONTROL,
    );
    assert_eq!(h.app.composer.text(), "retained composer");
    assert!(
        !h.app
            .resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .unwrap()
            .submitted
    );
    h.app.on_action(Action::Answer(Answer::Allow));
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .is_some_and(|pending| pending.is_text())
    })
    .await;
    h.app.on_paste("answer\r\n中文");
    h.key(
        KeyCode::Char('j'),
        KeyModifiers::ALT | KeyModifiers::CONTROL,
    );
    assert_eq!(
        h.app.resident.current().unwrap().interaction_editor.text(),
        "answer\n中文\n"
    );
    assert_eq!(h.app.composer.text(), "retained composer");
    h.ctrl(KeyCode::Char('r'));
    h.app.on_paste(h.native.target.session_id.as_str());
    assert_eq!(
        query_state(&h.app.overlay),
        (h.native.target.session_id.as_str(), 0)
    );
    assert_eq!(
        h.app.resident.current().unwrap().interaction_editor.text(),
        "answer\n中文\n"
    );
    h.press(KeyCode::Esc);
    h.app
        .on_action(Action::Answer(Answer::Text("answer\n中文\n".into())));
    h.until(|app| {
        !app.is_busy()
            && !app.has_interaction()
            && app
                .transcript_blocks()
                .iter()
                .any(|block| matches!(block, crate::transcript::Block::TurnEnd { .. }))
    })
    .await;
    let request = provider.request().await;
    let tool = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let result: serde_json::Value =
        serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["answer"], "answer\n中文\n");
    assert_eq!(h.app.composer.text(), "retained composer");
    h.finish().await;
    provider.finish().await;
}

#[tokio::test(start_paused = true)]
async fn forward_delete_keys_restore_their_edit_and_discard_only_abandoned_redo() {
    let mut h = Harness::opened().await;
    for (key, modifiers) in [
        (KeyCode::Delete, KeyModifiers::NONE),
        (KeyCode::Char('d'), KeyModifiers::CONTROL),
    ] {
        for paste in [false, true] {
            h.app.composer.set_text("");
            if paste {
                h.app.on_paste("abc");
            } else {
                h.type_text("abc");
            }
            h.press(KeyCode::Home);
            h.key(key, modifiers);
            assert_eq!(h.app.composer.text(), "bc");
            h.ctrl(KeyCode::Char('z'));
            assert_eq!(h.app.composer.text(), "abc");
            h.ctrl(KeyCode::Char('y'));
            assert_eq!(h.app.composer.text(), "bc");
            h.press(KeyCode::End);
            h.type_text("x");
            h.ctrl(KeyCode::Char('z'));
            assert_eq!(h.app.composer.text(), "bc");
            h.press(KeyCode::Home);
            h.key(key, modifiers);
            h.ctrl(KeyCode::Char('y'));
            assert_eq!(h.app.composer.text(), "c");
            h.ctrl(KeyCode::Char('z'));
            assert_eq!(h.app.composer.text(), "bc");
            h.press(KeyCode::End);
            h.key(key, modifiers);
            h.ctrl(KeyCode::Char('y'));
            assert_eq!(h.app.composer.text(), "c");
            assert!(!h.app.quit);
        }
    }
    h.app.composer.set_text("");
    assert!(matches!(
        h.app
            .on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        Some(Action::Quit)
    ));
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn copying_a_selection_drops_it_so_ctrl_c_quits_again() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("keep this");
    h.key(KeyCode::Left, KeyModifiers::SHIFT);
    h.ctrl(KeyCode::Char('c'));
    assert_eq!(h.app.composer.selection(), None);
    assert_eq!(h.app.composer.text(), "keep this");
    h.ctrl(KeyCode::Char('c'));
    h.ctrl(KeyCode::Char('c'));
    assert!(h.app.quit);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_cut_the_clipboard_would_refuse_keeps_the_text() {
    let mut h = Harness::opened().await;
    let big = "x".repeat(crate::clipboard::MAX_CLIPBOARD_BYTES + 1);
    h.app.composer.set_text(&big);
    h.key(KeyCode::Home, KeyModifiers::SHIFT);
    h.ctrl(KeyCode::Char('x'));
    assert_eq!(h.app.composer.text(), big);
    assert!(
        h.notices()[0].contains("clipboard limit"),
        "{:?}",
        h.notices()
    );
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn shift_end_with_no_draft_follows_the_tail_like_end() {
    let mut h = Harness::opened().await;
    h.app.scroll = crate::app::Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    h.key(KeyCode::End, KeyModifiers::SHIFT);
    assert_eq!(h.app.scroll, crate::app::Scroll::Follow);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_drag_in_the_editor_copies_on_release_and_stays_selected() {
    let mut h = Harness::opened().await;
    h.app.composer.set_text("copy me");
    h.screen(60, 24);
    let area = h.app.composer_area;
    let (x, y) = (area.x + 1, area.y + 1);
    mouse(&mut h, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut h, MouseEventKind::Drag(MouseButton::Left), x + 3, y);
    assert!(h.app.flash.is_none(), "nothing is copied mid-drag");
    mouse(&mut h, MouseEventKind::Up(MouseButton::Left), x + 3, y);
    h.until(|app| app.flash.is_some()).await;
    assert!(h.app.flash.is_some(), "{:?}", h.notices());
    assert_eq!(h.app.composer.selected_text(), Some("cop"));
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn template_picker_fills_before_sending_and_namespaces_builtin_collisions() {
    use crate::app::templates::Template;
    let mut h = Harness::opened().await;
    h.app.templates = vec![
        Template::parse("model".into(), "Template for $1"),
        Template::parse("review".into(), "Review $1"),
    ];
    h.type_text("/");
    let rows = h.app.palette_rows();
    assert!(rows.iter().any(|(name, _)| name == "command"));
    assert!(!rows.iter().any(|(name, _)| name == "review"));
    h.type_text("command");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.template_query(), Some(""));
    assert!(h.app.palette_rows().iter().any(|(name, _)| name == "model"));
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "/cmd ");
    h.app.composer.clear();

    h.type_text("/cmd");
    assert_eq!(h.app.template_query(), Some(""));
    assert_eq!(h.app.palette_rows().len(), 2);
    h.type_text(" rev");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.composer.text(), "/cmd review ");
    assert!(h.native.calls.try_recv().is_err());
    h.type_text(r#""src/main file.rs""#);
    h.press(KeyCode::Enter);
    let call = h.next_call().await;
    assert!(
        call.users()
            .last()
            .unwrap()
            .ends_with("\n\nReview src/main file.rs")
    );
    call.text("Review complete.");
    call.finish();
    h.until(|app| {
        !app.is_busy()
            && app
                .transcript_blocks()
                .iter()
                .any(|block| matches!(block, crate::transcript::Block::TurnEnd { .. }))
    })
    .await;

    h.type_text("/model");
    h.press(KeyCode::Enter);
    assert!(matches!(h.app.overlay, crate::app::Overlay::Models { .. }));
    h.press(KeyCode::Esc);
    h.type_text("/cmd model path");
    h.press(KeyCode::Enter);
    let call = h.next_call().await;
    assert!(
        call.users()
            .last()
            .unwrap()
            .ends_with("\n\nTemplate for path"),
        "{:?}",
        call.users()
    );
    call.finish();
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn template_completion_preserves_arguments_and_escape_keeps_the_draft() {
    use crate::app::templates::Template;
    let mut h = Harness::opened().await;
    h.app.templates = vec![Template::parse("review".into(), "Review $1")];

    h.type_text("/command");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "/cmd ");
    assert!(matches!(h.app.overlay, Overlay::Palette { .. }));
    h.type_text("rev");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "/cmd review ");
    assert!(h.app.overlay.is_none());
    assert!(h.native.calls.try_recv().is_err());

    h.app.composer.clear();
    h.type_text(r#"/rev "src/main file.rs" extra"#);
    h.press(KeyCode::Enter);
    assert_eq!(
        h.app.composer.text(),
        r#"/cmd review "src/main file.rs" extra"#
    );
    assert!(h.native.calls.try_recv().is_err());

    h.app.composer.clear();
    h.type_text("/cmd missing");
    h.press(KeyCode::Enter);
    assert_eq!(h.app.composer.text(), "/cmd missing");
    assert!(matches!(h.app.overlay, Overlay::Palette { .. }));
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "/cmd missing");
    assert!(h.native.calls.try_recv().is_err());

    h.app.composer.clear();
    h.type_text("/cmd");
    h.key(KeyCode::Enter, KeyModifiers::SHIFT);
    h.type_text("review");
    assert_eq!(h.app.template_query(), None);
    assert!(h.app.overlay.is_none());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn template_follow_up_keys_expand_before_sending() {
    use crate::app::templates::Template;
    for (code, modifiers) in [
        (KeyCode::Enter, KeyModifiers::ALT),
        (KeyCode::Enter, KeyModifiers::CONTROL),
        #[cfg(unix)]
        (
            KeyCode::Char('j'),
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        ),
    ] {
        let mut h = Harness::opened().await;
        h.app.templates = vec![Template::parse("review".into(), "Review $1")];
        h.type_text(r#"/cmd review "src/main file.rs""#);
        h.key(code, modifiers);
        assert_eq!(
            h.app.local_inputs[0].mode,
            minicore_host_protocol::MessageMode::FollowUp
        );
        let call = h.next_call().await;
        assert!(
            call.users()
                .last()
                .unwrap()
                .ends_with("\n\nReview src/main file.rs")
        );
        call.finish();
        h.finish().await;
    }
}
