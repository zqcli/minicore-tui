//! Terminal rendering fixtures use native Host values and an in-process connection.

use std::path::PathBuf;
use std::sync::Arc;

use minicore_host_core::{
    AgentSummary, Host, ResourceFuture, ResourceResolver, SessionInfo, SessionSnapshot,
    SessionSummary, TurnStateData,
};
use minicore_host_protocol::MessageMode::{FollowUp, Steer};
use minicore_host_protocol::{
    Fault, FaultKind, MessageMode, Scheduling, SessionConfig, SessionRef, Timestamp, ToolCallRef,
    TurnId, TurnOutcome,
};
use minicore_host_transport::HostClient;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::app::{App, LiveTurn, Msg, Overlay, Scroll, Session};
use crate::transcript::{Block, ChangeSummary, RequestKey, ToolCall, ToolState, UserKind};
use crate::ui::theme::{Palette, Styles};

pub(super) struct Fixture {
    pub app: App,
    pub _runtime: tokio::runtime::Runtime,
}
impl std::ops::Deref for Fixture {
    type Target = App;
    fn deref(&self) -> &App {
        &self.app
    }
}
impl std::ops::DerefMut for Fixture {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}
struct NoExecution;
impl ResourceResolver for NoExecution {
    fn validate(&self, _: &SessionConfig) -> Result<(), Fault> {
        Ok(())
    }
    fn resolve<'a>(&'a self, _: &'a SessionConfig) -> ResourceFuture<'a> {
        Box::pin(async {
            Err(Fault::new(
                FaultKind::SessionResourceMissing,
                "render fixture has no execution resources",
            ))
        })
    }
}

pub(super) fn native_id(index: u64) -> String {
    format!("{index:020}")
}
pub(super) fn turn_id(index: u64) -> TurnId {
    native_id(index).parse().unwrap()
}
fn stamp(value: &str) -> Option<Timestamp> {
    Some(serde_json::from_value(json!(value)).unwrap())
}

pub(super) fn app() -> Fixture {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let client = runtime.block_on(async {
        HostClient::in_process(Host::memory(Arc::new(NoExecution)).unwrap()).unwrap()
    });
    let (tx, _rx) = mpsc::channel::<Msg>(crate::app::tasks::MESSAGE_CAPACITY);
    let mut app = App::new(client, tx, PathBuf::from("/work/minicore-tui"));
    app.styles = Styles::of(Palette::dark());
    app.ready = true;
    app.agents = vec![agent(1, "Coder")];
    app.selected_agent = Some(native_id(1).parse().unwrap());
    let session = session(&app, 1, "Cache the parser");
    app.sessions = vec![catalog(&session)];
    app.resident.insert(session);
    app.resident.select(Some(1));
    Fixture {
        app,
        _runtime: runtime,
    }
}

pub(super) fn info(index: u64, title: &str) -> SessionInfo {
    let mut config = serde_json::to_value(SessionConfig::default()).unwrap();
    config["model"] = json!("gpt-6-luna");
    config["reasoning"] = json!("max");
    config["workspace"] = json!({"path":"/work/minicore-tui"});
    serde_json::from_value(json!({
        "id":native_id(index),"agent_id":native_id(1),"title":title,"mode":"memory",
        "created_at":"2026-10-20T21:38:00Z","last_activity_at":"2026-10-20T21:38:00Z",
        "agent_config_revision":0,"config_revision":0,"config":config
    }))
    .unwrap()
}
fn owned_work() -> Value {
    json!({"model":false,"tools":[],"preparing":false,"history_save":false,"metadata_save":false,"compaction":null,"tool_cleanup":false,"scope_cleanup":false,"result_handoff":false})
}
pub(super) fn session(app: &App, index: u64, title: &str) -> Session {
    let info = info(index, title);
    let target = SessionRef {
        agent_id: info.agent_id.clone(),
        session_id: info.id.clone(),
    };
    let state: SessionSnapshot = serde_json::from_value(json!({
        "host_id":native_id(1),"host_incarnation":native_id(1),"agent_id":info.agent_id,"session_id":info.id,
        "data":{"owned_work":owned_work(),"end_reason":null,"deletion":null,"mode":"memory","controller_revision":0,
        "lifecycle":"open","scheduling":"auto","pause_reasons":[],"residency":"loaded","load_id":native_id(1),"runner_revision":0,
        "active_turn_id":null,"pending_interactions":[],"next_step":[],"next_turn":[],"preparing":false,"compaction":null,"saving":false,
        "metadata_saving":false,"metadata_fault":null,"block_reason":null,"last_result":null}
    })).unwrap();
    Session::new(info, state, index, app.client.session(target))
}
pub(super) fn catalog(session: &Session) -> SessionSummary {
    let info = &session.info;
    let state = &session.state.data;
    SessionSummary {
        created_at: info.created_at,
        last_activity_at: Timestamp::now(),
        workspace: info.config.workspace.clone(),
        model: info.config.model.clone(),
        reasoning: info.config.reasoning,
        id: info.id.clone(),
        agent_id: info.agent_id.clone(),
        title: info.title.clone(),
        first_user_preview: info.first_user_preview.clone(),
        mode: info.mode,
        config_revision: info.config_revision,
        lifecycle: state.lifecycle,
        scheduling: state.scheduling,
        pause_reasons: state.pause_reasons.clone(),
        residency: state.residency,
        active_turn_id: state.active_turn_id.clone(),
        next_step: state.next_step.len(),
        next_turn: state.next_turn.len(),
        pending_interactions: usize::from(session.interaction.is_some()),
        owned_work: state.owned_work.clone(),
        block_reason: state.block_reason.clone(),
        metadata_fault: None,
        deletion: None,
        end_reason: None,
        delegated_by: info.delegated_by.clone(),
        delegation_root: info.delegation_root.clone(),
    }
}
fn agent(index: u64, name: &str) -> AgentSummary {
    serde_json::from_value(json!({"id":native_id(index),"host_id":native_id(1),"name":name,"mode":"memory","lifecycle":"active",
        "config_revision":0,"default_session_id":null,"metadata_saving":false,"metadata_fault":null,"deletion":null,"sessions":[]})).unwrap()
}
pub(super) fn live(start: usize, index: u64) -> LiveTurn {
    let state: TurnStateData = serde_json::from_value(json!({"state":"running","runner_revision":0,"config_revision":0,
        "outcome":null,"persistence":"not_applicable","requests":1,"tool_rounds":0,"phase":"model","model_retry":null,
        "model_attempts":[1],"fault":null,"persistence_fault":null,"cleanup_faults":[]})).unwrap();
    LiveTurn {
        turn_id: turn_id(index),
        load_id: Some(native_id(1).parse().unwrap()),
        state,
        started: tokio::time::Instant::now(),
        running_tool: None,
        start,
        interrupting: false,
    }
}
fn tool() -> ToolCall {
    let mut call = ToolCall::new(
        ToolCallRef {
            turn_id: turn_id(1),
            tool_call_id: "call_1".to_owned().into(),
        },
        "bash",
    );
    call.detail = "$ cargo test --lib".into();
    call.state = ToolState::Succeeded;
    call.duration = Some(std::time::Duration::from_millis(4200));
    call.exit_code = Some(0);
    call.output = vec!["running 131 tests".into(), "test result: ok".into()];
    call.output_dropped = 14;
    call
}

fn conversation(app: &mut App) {
    let session = app.resident.current_mut().expect("session");
    let transcript = &mut session.transcript;
    transcript.push(Block::User {
        text: "Why does the parser re-parse the whole message on every delta?".into(),
        kind: UserKind::Prompt,
        at: None,
        expanded: None,
    });
    transcript.push(Block::Assistant {
        key: RequestKey::response(turn_id(1), 0),
        reasoning: "The layout cache only keyed on the block revision...".into(),
        text: "It does not any more. The **streaming renderer** keeps the stable prefix and\nre-renders only the tail, so a delta costs one small parse.".into(),
        streaming: false,
        partial: false,
        expanded: None,
    });
    transcript.push(Block::Tool(Box::new(tool())));
    transcript.push(Block::TurnEnd {
        turn_id: turn_id(1),
        outcome: TurnOutcome::Completed,
        reason: None,
        duration: Some(std::time::Duration::from_secs(12)),
        requests: 2,
        tool_rounds: 1,
        usage: Default::default(),
        changes: Some(ChangeSummary {
            files: 2,
            lines: Some((40, 7)),
        }),
        saving: None,
    });
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for row in 0..buffer.area.height {
        let mut line = String::new();
        for column in 0..buffer.area.width {
            line.push_str(buffer[(column, row)].symbol());
        }
        out.push_str(line.trim_end_matches(' '));
        out.push('\n');
    }
    out
}

/// Compares `rendered` with the checked-in frame `tests/snapshots/{name}.txt`,
/// reviewed with the code that draws it; `UPDATE_SNAPSHOTS=1 cargo test` writes it.
fn snapshot(name: &str, rendered: String) {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(file, rendered).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&file).unwrap_or_else(|error| {
        panic!("{}: {error}; UPDATE_SNAPSHOTS=1 creates it", file.display())
    });
    assert_eq!(expected, rendered, "frame differs from {name}.txt");
}

#[test]
fn idle_screen_shows_the_rail_composer_and_footer() {
    let mut app = app();
    conversation(&mut app);
    snapshot("idle", render(&mut app, 70, 20));
}

#[test]
fn expanded_cards_show_hidden_lines_at_the_cut() {
    let mut app = app();
    let mut read = tool();
    read.name = "read".into();
    read.detail = "src/main.rs:1-40".into();
    read.duration = None;
    read.exit_code = None;
    read.output = vec!["1: fn main() {".into(), "2:     run();".into()];
    read.output_dropped = 0;
    read.output_lines = Some(40);
    let transcript = &mut app.resident.current_mut().unwrap().transcript;
    transcript.push(Block::Tool(Box::new(tool())));
    transcript.push(Block::Tool(Box::new(read)));
    app.tools_expanded = true;
    snapshot("expanded-cards", render(&mut app, 70, 32));
}

#[test]
fn the_draft_is_the_welcome_header_over_the_editor_and_the_footer() {
    let mut app = app();
    app.resident = Default::default();
    app.launch.model = Some("gpt-6-luna".parse().unwrap());
    app.launch.reasoning = Some(minicore_host_protocol::ReasoningPreference::Max);
    let screen = render(&mut app, 70, 14);
    snapshot("draft", screen.clone());
    assert!(!screen.contains("No session"), "a draft shows no status");

    app.ready = false;
    let screen = render(&mut app, 70, 16);
    assert!(screen.contains("╭─ minicore-tui "), "{screen}");
    assert!(screen.contains("Starting MiniCore Host…"), "{screen}");
}

#[test]
fn the_screen_shows_earlier_content_while_streaming() {
    let mut app = app();
    conversation(&mut app);
    let session = app.resident.current_mut().expect("session");
    session.transcript.push(Block::User {
        text: "Now stream a longer answer.".into(),
        kind: UserKind::Prompt,
        at: stamp("2026-10-20T21:41:09Z"),
        expanded: None,
    });
    session.transcript.push(Block::Assistant {
        key: RequestKey::response(turn_id(2), 0),
        reasoning: "I should explain the anchor rule first, then show it.".into(),
        text: "An anchored view keeps its block and row, so\nnew output below never moves the text you\nare reading. Only scrolling to the end returns\nto following.".into(),
        streaming: true,
        partial: false,
        expanded: None,
    });
    snapshot("streaming", render(&mut app, 70, 20));
}

#[test]
fn an_anchored_view_does_not_move_when_content_grows() {
    let mut app = app();
    conversation(&mut app);
    // Scroll up: the anchor is the first user block.
    app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    let before = render(&mut app, 70, 20);
    let session = app.resident.current_mut().expect("session");
    session.transcript.push(Block::Assistant {
        key: RequestKey::response(turn_id(3), 0),
        reasoning: String::new(),
        text: (0..60)
            .map(|index| format!("appended line {index}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        streaming: true,
        partial: false,
        expanded: None,
    });
    let after = render(&mut app, 70, 20);
    let kept: Vec<&str> = before.lines().take(10).collect();
    assert!(
        kept.iter()
            .all(|line| after.lines().any(|new| new == *line)),
        "the anchored top must not be replaced:\n{before}\n---\n{after}"
    );
    assert!(
        after.contains("↓ Jump to latest message · End"),
        "the label marks content below an anchored view:\n{after}"
    );
    assert!(
        matches!(app.scroll, Scroll::Anchor { .. }),
        "{:?}",
        app.scroll
    );
}

#[test]
fn the_editor_grows_with_its_content_and_caps_at_twelve_rows() {
    let mut app = app();
    app.composer.set_text(
        &(0..20)
            .map(|index| format!("line {index} of a long draft"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let rendered = render(&mut app, 60, 60);
    let editor_rows = rendered
        .lines()
        .filter(|line| line.starts_with(" ▎"))
        .count();
    assert_eq!(
        editor_rows, 12,
        "the editor caps at twelve rows:\n{rendered}"
    );
    assert!(rendered.contains("line 19 of a long draft"), "{rendered}");
}

#[test]
fn the_footer_reports_model_and_reasoning_without_duplicate_status() {
    let mut app = app();
    app.composer.set_text("hello");
    let rendered = render(&mut app, 80, 12);
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(footer.starts_with(" ▸ minicore-tui"), "{footer}");
    assert!(
        footer.ends_with("gpt 6 luna · max · approval ask"),
        "where on the left, what runs on the right: {footer}"
    );
    assert!(
        !footer.contains('@'),
        "an unknown branch is left out: {footer}"
    );
    assert!(
        !footer.contains("ctx"),
        "no context before it is known: {footer}"
    );
    assert!(
        !footer.contains('↑'),
        "no token totals before any usage: {footer}"
    );
}

#[test]
fn the_footer_leaves_response_usage_to_the_context_view() {
    let mut app = app();
    let session = app.resident.current_mut().unwrap();
    session.branch = Some("main".into());
    session.context = Some(context(
        session,
        json!({"input_tokens":50500,"output_tokens":1800,"cache_read_tokens":33800}),
    ));
    let rendered = render(&mut app, 110, 12);
    let footer = rendered.lines().last().unwrap();
    assert!(footer.contains("minicore-tui@main"), "{footer}");
    assert!(!footer.contains('↑'), "usage lives in /context: {footer}");
    assert!(!footer.contains("ctx") && !footer.contains('%'), "{footer}");
    assert_eq!(crate::wrap::width(footer), 110);
    for columns in [60, 50, 30] {
        let narrow = render(&mut app, columns, 12);
        assert!(
            narrow
                .lines()
                .all(|row| crate::wrap::width(row) <= columns as usize)
        );
    }
}

pub(super) fn context(
    session: &Session,
    usage: Value,
) -> minicore_host_core::SessionContextSnapshot {
    serde_json::from_value(json!({
        "host_id":session.state.host_id,"host_incarnation":session.state.host_incarnation,"agent_id":session.info.agent_id,"session_id":session.info.id,
        "data":{"controller_revision":0,"residency":"loaded","load_id":native_id(1),"history_loaded":true,"config_revision":0,
        "model":"gpt-6-luna","compaction_settings":session.info.config.execution.compaction,"active_turn":null,
        "queued_inputs":{"next_step":0,"next_turn":0},"history":{"position":8,"raw_items":6,"effective_items":6,"projection":null,
        "latest_response":{"raw_item_index":5,"model":"gpt-6-luna","covered_by_compaction":false,"usage":usage}},"compaction":null,
        "history_save":{"pending":false,"running":false,"fault":null},"block_reason":null}
    })).unwrap()
}

/// The last transcript row never touches the stack below it, and the
/// composer's tint spans the whole row.
#[test]
fn a_blank_row_separates_the_transcript_and_the_composer_band_is_full_width() {
    let mut app = app();
    for index in 0..40 {
        app.resident
            .current_mut()
            .expect("session")
            .transcript
            .push(Block::User {
                text: format!("prompt {index}"),
                kind: UserKind::Prompt,
                at: None,
                expanded: None,
            });
    }
    let backend = TestBackend::new(50, 14);
    let mut terminal = Terminal::new(backend).expect("terminal");
    crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, &mut app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let row = |y: u16| -> String {
        (0..50)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    let composer = terminal.backend().cursor_position().y;
    assert_eq!(row(composer), " ▎", "the empty editor has no hint");
    assert_eq!(row(composer - 1), " ▎", "the editor's top pad row");
    assert_eq!(
        row(composer - 2),
        "",
        "one gap row; an idle activity line takes no row"
    );
    assert_eq!(row(composer - 3).trim(), "▎", "the card's bottom pad row");
    assert!(
        row(composer - 4).contains("prompt 39"),
        "{}",
        row(composer - 4)
    );
    assert_eq!(
        buffer[(0u16, composer)].bg,
        Color::Reset,
        "the gutter is never painted"
    );
    assert_eq!(
        buffer[(49u16, composer)].bg,
        Palette::dark().composer_bg,
        "the band reaches the last column"
    );
}

#[test]
fn the_status_spinner_moves_with_time() {
    use std::time::Duration;
    let frames: std::collections::HashSet<&str> = (0..10)
        .map(|step| crate::ui::status::spinner(Duration::from_millis(step * 80)))
        .collect();
    assert_eq!(frames.len(), 10, "every tick shows the next frame");
    assert_eq!(
        crate::ui::status::spinner(Duration::from_millis(800)),
        crate::ui::status::spinner(Duration::ZERO)
    );
}

#[test]
fn a_fatal_host_error_explains_how_to_quit() {
    let mut app = app();
    app.fatal = Some(crate::app::Fatal {
        reason: "Host connection closed".into(),
    });
    let rendered = render(&mut app, 60, 14);
    assert!(rendered.contains("Fatal error") && rendered.contains("Host connection closed"));
    assert!(rendered.to_lowercase().contains("press q"));
}

#[test]
fn no_color_renders_the_same_layout_without_color() {
    let mut app = app();
    conversation(&mut app);
    let colored = render(&mut app, 70, 20);
    app.styles = Styles::of(Palette::plain());
    let plain = render(&mut app, 70, 20);
    assert_eq!(colored, plain, "NO_COLOR changes colors, never layout");
}

#[test]
fn a_narrow_terminal_keeps_every_row_within_the_width() {
    let mut app = app();
    conversation(&mut app);
    let rendered = render(&mut app, 30, 24);
    for line in rendered.lines() {
        assert!(
            crate::wrap::width(line) <= 30,
            "a row exceeds the terminal width: {line:?}"
        );
    }
}

#[test]
fn typing_keeps_the_draft_visible_and_marked_with_the_rail() {
    let mut app = app();
    for character in "hello".chars() {
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(character),
            crossterm::event::KeyModifiers::NONE,
        );
        assert_eq!(app.on_key(key), None);
    }
    let rendered = render(&mut app, 40, 10);
    assert!(rendered.contains(" ▎hello"), "{rendered}");
    assert!(
        !rendered.contains("Ask anything"),
        "the placeholder is gone once there is a draft:\n{rendered}"
    );
}

#[test]
fn an_open_overlay_is_not_drawn_over_the_composer() {
    let mut app = app();
    app.composer.set_text("draft");
    app.overlay = Overlay::Help {
        scroll: 0,
        filter: String::new(),
    };
    let rendered = render(&mut app, 40, 10);
    assert!(
        rendered.contains(" ▎draft"),
        "the composer stays visible under a view:\n{rendered}"
    );
}

#[test]
fn the_session_view_identifies_its_agent_and_native_storage_mode() {
    let mut app = app();
    app.overlay = Overlay::SessionInfo { scroll: 0 };
    let screen = render(&mut app, 100, 24);
    assert!(screen.contains("Coder"), "{screen}");
    assert!(screen.contains("storage     memory"), "{screen}");
    assert!(!screen.contains("allowed"), "{screen}");
}

#[test]
fn a_view_dims_the_composer_and_keys_typed_in_it_do_not_edit_the_draft() {
    let mut app = app();
    app.overlay = Overlay::Help {
        scroll: 0,
        filter: String::new(),
    };
    let rendered = render(&mut app, 60, 12);
    assert!(rendered.contains("view open · esc back"), "{rendered}");
    assert!(!rendered.contains("Ask anything"), "{rendered}");
    app.on_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('x'),
    ));
    assert!(app.composer.is_empty(), "the view owns the keys");

    app.overlay = Overlay::None;
    let rendered = render(&mut app, 60, 12);
    assert!(!rendered.contains("view open"), "{rendered}");
}

fn queue(app: &mut App, messages: &[(&str, MessageMode)]) {
    let session = app.resident.current_mut().unwrap();
    let rows: Vec<Value> = messages.iter().enumerate().map(|(index,(text,mode))| json!({
        "input":{"message_id":native_id(index as u64+1),"text":text,"mode":mode},"source":{"kind":"user"},
        "queue":mode.requested_queue(),"order":index as u64+1
    })).collect();
    session.queue = Some(serde_json::from_value(json!({
        "host_id":session.state.host_id,"host_incarnation":session.state.host_incarnation,"agent_id":session.info.agent_id,"session_id":session.info.id,
        "controller_revision":0,"next_step":rows.iter().filter(|row| row["queue"] == "next-step").collect::<Vec<_>>(),
        "next_turn":rows.iter().filter(|row| row["queue"] == "next-turn").collect::<Vec<_>>()
    })).unwrap());
}

/// The activity line when it carries key hints, trimmed.
fn hint_row(rendered: &str) -> Option<&str> {
    rendered
        .lines()
        .find(|line| {
            ["steer ·", "queued ·", "paused ·", "esc interrupt"]
                .iter()
                .any(|hint| line.contains(hint))
        })
        .map(str::trim)
}

#[test]
fn the_dock_lists_queued_messages_in_the_order_the_agent_runs_them_with_the_keys() {
    let mut app = app();
    conversation(&mut app);
    running_bash(&mut app);
    queue(
        &mut app,
        &[
            ("focus on the parser first", Steer),
            ("and the changelog", Steer),
            ("then update the docs", FollowUp),
        ],
    );
    app.composer.set_text("one more thing");
    snapshot("dock-queue", render(&mut app, 70, 20));
}

#[test]
fn observation_failure_stays_visible_without_claiming_an_empty_history() {
    let mut app = app();
    conversation(&mut app);
    app.resident.current_mut().unwrap().observation_error =
        Some("History read interrupted; refresh required".into());
    assert!(render(&mut app, 70, 14).contains("History read interrupted; refresh required"));
}

#[test]
fn a_paused_queue_says_how_to_go_on() {
    let mut app = app();
    queue(&mut app, &[("after the stop", FollowUp)]);
    app.resident.current_mut().unwrap().state.data.scheduling = Scheduling::Paused;
    assert_eq!(
        hint_row(&render(&mut app, 70, 12)),
        Some("paused · enter resumes  1 queued · alt+↑ edit")
    );
    app.composer.set_text("draft");
    assert_eq!(
        hint_row(&render(&mut app, 70, 12)),
        Some("paused · enter sends  1 queued · alt+↑ edit")
    );
}

#[test]
fn delegated_result_notifications_stay_out_of_the_dock_queue_preview() {
    let mut app = app();
    let session = app.resident.current_mut().unwrap();
    let child_agent = native_id(10);
    let child_a = native_id(11);
    let child_b = native_id(12);
    let result_json = format!(
        r#"{{"discarded_inputs":[],"results":[{{"outcome":"completed","output":["ok"]}}],"source":{{"agent_id":"{child_agent}","session_id":"{child_a}"}},"status":"ended"}}"#
    );
    let rows = vec![
        json!({
            "input": {"message_id": native_id(1), "text": "also check Wuhan", "mode": Steer},
            "source": {"kind": "user"},
            "queue": "next-step",
            "order": 1
        }),
        json!({
            "input": {"message_id": native_id(2), "text": result_json, "mode": Steer},
            "source": {"kind": "agent", "source": {"agent_id": child_agent, "session_id": child_a}},
            "queue": "next-step",
            "order": 2
        }),
        json!({
            "input": {"message_id": native_id(3), "text": result_json, "mode": Steer},
            "source": {"kind": "agent", "source": {"agent_id": child_agent, "session_id": child_b}},
            "queue": "next-step",
            "order": 3
        }),
    ];
    session.queue = Some(
        serde_json::from_value(json!({
            "host_id": session.state.host_id,
            "host_incarnation": session.state.host_incarnation,
            "agent_id": session.info.agent_id,
            "session_id": session.info.id,
            "controller_revision": 0,
            "next_step": rows,
            "next_turn": []
        }))
        .unwrap(),
    );
    session.state.data.scheduling = Scheduling::Paused;
    let rendered = render(&mut app, 100, 14);
    assert!(rendered.contains("also check Wuhan"), "{rendered}");
    assert!(!rendered.contains("discarded_inputs"), "{rendered}");
    assert!(!rendered.contains(&child_a), "{rendered}");
    let hints = hint_row(&rendered).unwrap();
    assert!(hints.contains("1 queued · alt+↑ edit"), "{hints}");
    assert!(hints.contains("2 agent results"), "{hints}");
}

#[test]
fn open_working_group_only_spins_actually_streaming_thinking_blocks() {
    let mut app = app();
    let turn: TurnId = native_id(1).parse().unwrap();
    {
        let session = app.resident.current_mut().unwrap();
        let mut tool = ToolCall::new(
            ToolCallRef {
                turn_id: turn.clone(),
                tool_call_id: "call-1".to_owned().into(),
            },
            "read",
        );
        tool.set_state(ToolState::Succeeded);
        session.transcript.push(Block::User {
            text: "check weather".into(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
        });
        session.transcript.push(Block::Assistant {
            key: RequestKey::response(turn.clone(), 0),
            reasoning: "Fetching weather".into(),
            text: String::new(),
            streaming: false,
            partial: false,
            expanded: None,
        });
        session.transcript.push(Block::Tool(Box::new(tool)));
        session.transcript.push(Block::Assistant {
            key: RequestKey::response(turn.clone(), 1),
            reasoning: "Verifying sources".into(),
            text: String::new(),
            streaming: true,
            partial: false,
            expanded: None,
        });
        session.transcript.regroup(&turn);
    }

    // Closed Working: latest step is streaming thinking, so it spins.
    let closed = render(&mut app, 80, 18);
    let latest_line = closed
        .lines()
        .find(|line| line.contains("Verifying sources"))
        .expect("latest thinking row");
    assert!(
        !latest_line.contains("∴ Verifying sources"),
        "live thinking should spin while closed: {latest_line:?}"
    );

    // Open Working: Head block is finished (`streaming: false`), so it stays `∴`;
    // only the bottom streaming block spins.
    app.resident
        .current_mut()
        .unwrap()
        .transcript
        .toggle_group(1);
    let open = render(&mut app, 80, 22);
    let head_line = open
        .lines()
        .find(|line| line.contains("Fetching weather"))
        .expect("head thinking row");
    assert!(
        head_line.contains("∴ Fetching weather"),
        "finished head thinking must not spin when open: {head_line:?}"
    );
    let bottom_line = open
        .lines()
        .find(|line| line.contains("Verifying sources"))
        .expect("bottom thinking row");
    assert!(
        !bottom_line.contains("∴ Verifying sources"),
        "active bottom thinking should spin when open: {bottom_line:?}"
    );

    // Once the bottom thinking finishes (`streaming: false`), closed Working settles back to `∴`.
    {
        let session = app.resident.current_mut().unwrap();
        session.transcript.toggle_group(1);
        if let Some(Block::Assistant { streaming, .. }) = session.transcript.block_mut(3) {
            *streaming = false;
        }
        session.transcript.regroup(&turn);
    }
    let settled = render(&mut app, 80, 18);
    let settled_line = settled
        .lines()
        .find(|line| line.contains("Verifying sources"))
        .expect("settled latest thinking row");
    assert!(
        settled_line.contains("∴ Verifying sources"),
        "settled thinking must not spin while closed: {settled_line:?}"
    );
}

#[test]
fn the_dock_hint_follows_what_the_keys_can_do() {
    let mut app = app();
    assert_eq!(hint_row(&render(&mut app, 70, 12)), None, "idle");

    running_bash(&mut app);
    let hints = |app: &mut App| hint_row(&render(app, 90, 12)).map(str::to_owned);
    let running = hints(&mut app).unwrap();
    assert!(
        running.starts_with("⠋ Working 0m · running bash"),
        "{running}"
    );
    assert!(running.ends_with("  esc interrupt"), "{running}");

    app.composer.set_text("draft");
    let drafting = hints(&mut app).unwrap();
    assert!(
        drafting.ends_with("  enter steer · alt+enter follow-up"),
        "{drafting}"
    );

    queue(&mut app, &[("later", FollowUp)]);
    let queued = hints(&mut app).unwrap();
    assert!(
        queued.ends_with("  enter steer · alt+enter follow-up  1 queued · alt+↑ edit"),
        "{queued}"
    );

    app.composer.clear();
    let waiting = hints(&mut app).unwrap();
    assert!(
        waiting.ends_with("  esc interrupt  1 queued · alt+↑ edit"),
        "{waiting}"
    );

    // A narrow dock drops the mode hint before the counts and keeps the status.
    app.composer.set_text("draft");
    let narrow = hint_row(&render(&mut app, 60, 12)).unwrap().to_owned();
    assert!(
        narrow.starts_with("⠋ Working 0m · running bash"),
        "{narrow}"
    );
    assert!(narrow.ends_with("  1 queued · alt+↑ edit"), "{narrow}");
    assert!(!narrow.contains("enter steer"), "{narrow}");
}

#[test]
fn a_queue_shows_its_count_and_text_within_the_dock_width() {
    let mut app = app();
    queue(
        &mut app,
        &[("a very long follow-up message that cannot fit", FollowUp)],
    );
    let rendered = render(&mut app, 30, 12);
    assert!(rendered.contains("1 queued · alt+↑ edit"), "{rendered}");
    assert!(
        rendered.contains("   ↳ a very long follow-up"),
        "{rendered}"
    );
    assert!(rendered.lines().all(|row| crate::wrap::width(row) <= 30));
}

#[test]
fn a_running_turn_shows_its_whole_minutes_in_the_activity_line_only() {
    let mut app = app();
    running_bash(&mut app);
    let rendered = render(&mut app, 110, 14);
    let working = rendered
        .lines()
        .find(|line| line.contains("Working"))
        .unwrap();
    assert!(working.contains("Working 0m · running bash"), "{working}");
    assert!(!rendered.lines().last().unwrap().contains("0m"));
}

/// A left press and release on one cell: a click, not a drag.
fn click(app: &mut App, column: u16, row: u16) {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
}

fn row_of(screen: &str, prefix: &str) -> u16 {
    screen
        .lines()
        .position(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no row starts with {prefix:?}:\n{screen}")) as u16
}

#[test]
fn clicking_a_hidden_lines_row_collapses_the_expanded_card_without_opening_an_overlay() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 30);
    let header = row_of(&folded, " ✓ bash");
    click(app, 10, header);
    let expanded = render(app, 70, 30);
    assert!(expanded.contains("… 14 earlier lines"), "{expanded}");
    assert!(!app.tools_expanded, "only that card changed");

    click(app, 10, row_of(&expanded, "   ▎ … 14 earlier lines"));
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    let refolded = render(app, 70, 30);
    assert!(!refolded.contains("… 14 earlier lines"), "{refolded}");
}

#[test]
fn clicking_the_body_or_the_footer_of_an_expanded_card_collapses_it() {
    for prefix in ["   ▎ test result: ok", "   ▎ Took 4.2s"] {
        let mut fixture = app();
        let _runtime = fixture._runtime.enter();
        let app = &mut fixture.app;
        conversation(app);
        let folded = render(app, 70, 34);
        click(app, 10, row_of(&folded, " ✓ bash"));
        let open = render(app, 70, 34);
        assert!(open.contains("   ▎ $ cargo test --lib"), "{open}");
        click(app, 10, row_of(&open, prefix));
        let collapsed = render(app, 70, 34);
        assert!(
            !collapsed.contains("   ▎ $ cargo test --lib"),
            "a click on {prefix:?} left the card open:\n{collapsed}"
        );
        assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    }
}

#[test]
fn a_drag_across_an_expanded_card_body_selects_and_leaves_it_open() {
    use crossterm::event::MouseEventKind::{Down, Drag, Up};
    let mut fixture = app();
    // The copy is queued on the runtime and never runs.
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 30);
    click(app, 10, row_of(&folded, " ✓ bash"));
    let open = render(app, 70, 30);
    let body = row_of(&open, "   ▎ running 131 tests");
    left(app, Down, 5, body);
    left(app, Drag, 12, body + 1);
    left(app, Up, 12, body + 1);
    assert!(app.selection.is_some());
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    let after = render(app, 70, 30);
    assert!(after.contains("   ▎ $ cargo test --lib"), "{after}");
}

#[test]
fn a_card_opens_downward_from_its_header_by_click_or_ctrl_o() {
    for by_click in [true, false] {
        let mut fixture = app();
        let app = &mut fixture.app;
        conversation(app);
        let session = app.resident.current_mut().unwrap();
        let mut call = tool();
        call.state = ToolState::Running;
        call.duration = None;
        call.exit_code = None;
        call.output = (1..=12).map(|index| format!("item {index}")).collect();
        call.output_dropped = 0;
        session.transcript.push(Block::Tool(Box::new(call)));
        let folded = render(app, 70, 20);
        let header = |screen: &str| {
            let rows: Vec<&str> = screen.lines().collect();
            rows.iter().rposition(|row| row.starts_with(" ● bash"))
        };
        let before = header(&folded).expect("running card");
        if by_click {
            click(app, 10, before as u16);
        } else {
            app.on_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('o'),
                crossterm::event::KeyModifiers::CONTROL,
            ));
        }
        let expanded = render(app, 70, 20);
        // Ctrl+O also opens the thinking row and its flash takes the activity
        // row, so only a click must leave the rows above the card in place.
        let rows = if by_click { 3 } else { 0 };
        let top = |screen: &str| {
            screen
                .lines()
                .skip(1)
                .take(rows)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(top(&expanded), top(&folded), "{expanded}");
        if by_click {
            let title = expanded.lines().nth(before + 2).unwrap_or_default();
            assert!(title.contains("$ cargo test --lib"), "{expanded}");
        }
    }
}

#[test]
fn collapsing_a_card_at_the_end_closes_in_place_until_the_view_scrolls() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 16);
    let header = row_of(&folded, " ✓ bash");
    click(app, 10, header);
    let open = render(app, 70, 16);
    assert_eq!(row_of(&open, " ✓ bash"), header, "{open}");
    assert!(
        open.lines()
            .nth(header as usize + 1)
            .is_some_and(|line| line.starts_with("   ▎")),
        "{open}"
    );
    // The card folds back toward its kept header; nothing above it moves, even
    // though the shorter transcript leaves rows free below.
    click(app, 10, header);
    let collapsed = render(app, 70, 16);
    assert_eq!(
        open.lines().take(header as usize).collect::<Vec<_>>(),
        collapsed.lines().take(header as usize).collect::<Vec<_>>(),
        "{collapsed}"
    );
    assert_eq!(row_of(&collapsed, " ✓ bash"), header, "{collapsed}");
    assert!(
        !collapsed
            .lines()
            .nth(header as usize + 1)
            .is_some_and(|line| line.starts_with("   ▎")),
        "{collapsed}"
    );
    // Scrolling to the end follows again with no blank rows.
    app.scroll_to(usize::MAX);
    let followed = render(app, 70, 16);
    let end = |screen: &str| row_of(screen, "   completed in 12s");
    assert_eq!(end(&followed), end(&folded), "{followed}");
    assert_eq!(app.scroll, Scroll::Follow);
}

fn failed_turn(app: &mut App, reason: &str) {
    let transcript = &mut app.resident.current_mut().unwrap().transcript;
    transcript.push(Block::User {
        text: "Summarise the changes".into(),
        kind: UserKind::Prompt,
        at: None,
        expanded: None,
    });
    transcript.push(Block::TurnEnd {
        turn_id: turn_id(1),
        outcome: TurnOutcome::Failed,
        reason: Some(reason.into()),
        duration: Some(std::time::Duration::from_secs(47)),
        requests: 1,
        tool_rounds: 2,
        usage: Default::default(),
        changes: None,
        saving: None,
    });
}
fn rate_limited() -> &'static str {
    "Rate limit reached for gpt-6-luna in organization org-7Zq on tokens per min (TPM): Limit 30000, Used 29250, Requested 4012.\nPlease try again in 8.524s. Visit https://platform.example/account/rate-limits to learn more."
}

#[test]
fn a_failed_turn_names_the_provider_status_the_hint_and_clips_a_long_message() {
    let mut app = app();
    failed_turn(&mut app, rate_limited());
    snapshot("failed-provider", render(&mut app, 80, 16));
}

#[test]
fn ctrl_o_opens_the_whole_provider_message_and_closes_it_again() {
    let mut app = app();
    failed_turn(&mut app, rate_limited());
    app.tools_expanded = true;
    let open = render(&mut app, 80, 16);
    snapshot("failed-provider-expanded", open);
    app.tools_expanded = false;
    let shut = render(&mut app, 80, 16);
    assert!(shut.contains("full message"), "{shut}");
    assert!(!shut.contains("to learn more"), "{shut}");
}

#[test]
fn a_short_host_failure_keeps_its_actual_message() {
    let mut app = app();
    failed_turn(&mut app, "Model request timed out");
    failed_turn(&mut app, "Output limit reached");
    snapshot("failed-plain", render(&mut app, 80, 18));
}

#[test]
fn clicking_a_failed_turn_opens_its_complete_safe_message() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    let reason = format!("{}\nFinal detail 中文\u{1b}[2J", rate_limited());
    failed_turn(app, &reason);
    let folded = render(app, 80, 20);
    click(app, 5, row_of(&folded, "   failed"));
    assert!(matches!(&app.overlay, Overlay::Error { text, .. } if text.contains(&reason)));
    let details = render(app, 100, 25);
    assert!(details.contains("Error details"), "{details}");
    assert!(details.contains("Final detail 中 文"), "{details}");
    assert!(!details.contains('\u{1b}'));
}

#[test]
fn a_drag_over_a_failed_turn_takes_the_message_without_rails_hints_or_ellipsis() {
    let mut app = app();
    failed_turn(&mut app, rate_limited());
    let screen = render(&mut app, 80, 16);
    let first = row_of(&screen, "   failed");
    let drag = crate::app::selection::Drag {
        from: (0, first),
        to: (79, first + 6),
    };
    let text = app.selected_text(drag).expect("text");
    assert_eq!(
        text,
        "Rate limit reached for gpt-6-luna in organization org-7Zq on tokens per\n\
         min (TPM): Limit 30000, Used 29250, Requested 4012.\n\
         Please try again in 8.524s. Visit"
    );
}

#[test]
fn clicking_a_turn_end_line_opens_the_git_diff() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    click(app, 5, row_of(&screen, "   completed"));
    assert!(
        matches!(&app.overlay, Overlay::Diff(_)),
        "{:?}",
        app.overlay
    );
}

#[test]
fn a_drag_over_a_card_header_selects_instead_of_toggling() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut fixture = app();
    // The copy is queued on the runtime and never runs.
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let header = row_of(&screen, " ✓ bash");
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 2),
        (MouseEventKind::Drag(MouseButton::Left), 8),
        (MouseEventKind::Up(MouseButton::Left), 8),
    ] {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row: header,
            modifiers: KeyModifiers::NONE,
        });
    }
    let after = render(app, 70, 20);
    assert!(
        !after.contains(" ▎ $ cargo test --lib"),
        "still folded:\n{after}"
    );
}

#[test]
fn a_drag_across_a_card_takes_its_text_without_rails_or_hints() {
    let mut fixture = app();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let header = row_of(&screen, " ✓ bash");
    let drag = crate::app::selection::Drag {
        from: (1, header),
        to: (69, header),
    };
    assert_eq!(
        app.selected_text(drag).as_deref(),
        Some("bash   cargo test --lib")
    );
}

#[test]
fn the_gutter_column_stays_blank_and_ignores_clicks() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let area = app.layout.viewport.area();
    assert_eq!(area.x, 1);
    for line in screen.lines().take(area.height as usize) {
        assert!(!line.starts_with(|c: char| c != ' '), "{line:?}\n{screen}");
    }
    app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    let top = render(app, 70, 20);
    assert!(
        top.contains(concat!("minicore-tui ", env!("CARGO_PKG_VERSION"))),
        "{top}"
    );
    assert!(
        top.contains("f1 keys and commands · / commands · @ files"),
        "{top}"
    );
    assert!(top.contains("/work/minicore-tui"), "{top}");
    app.scroll = Scroll::Follow;
    let header = row_of(&screen, " ✓ bash");
    click(app, 0, header);
    assert!(
        !render(app, 70, 20).contains(" ▎ $ cargo test --lib"),
        "still folded"
    );
}

/// A transcript whose block 0 is the card, so the welcome header sits in front of it.
fn card_first(app: &mut App) {
    let session = app.resident.current_mut().expect("session");
    session.transcript.push(Block::Tool(Box::new(tool())));
}

#[test]
fn a_click_on_a_card_in_block_0_maps_below_the_welcome_header() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    card_first(app);
    let folded = render(app, 70, 30);
    click(app, 10, row_of(&folded, " ✓ bash"));
    let expanded = render(app, 70, 30);
    assert!(expanded.contains("   ▎ $ cargo test --lib"), "{expanded}");

    click(app, 10, row_of(&expanded, "   ▎ … 14 earlier lines"));
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
    let refolded = render(app, 70, 30);
    assert!(!refolded.contains("… 14 earlier lines"), "{refolded}");
}

#[test]
fn a_click_on_a_welcome_header_row_does_nothing() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    card_first(app);
    let folded = render(app, 70, 30);
    let rows = app.layout.state.header_rows as u16;
    assert!(rows > 0, "the header is cached in front of block 0");
    for row in 0..rows {
        click(app, 10, row);
    }
    assert_eq!(render(app, 70, 30), folded);
    assert!(matches!(app.overlay, Overlay::None), "{:?}", app.overlay);
}

fn left(
    app: &mut App,
    kind: fn(crossterm::event::MouseButton) -> crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
) {
    app.on_mouse(crossterm::event::MouseEvent {
        kind: kind(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
}

#[test]
fn a_double_click_selects_a_word_and_a_triple_click_the_row() {
    use crossterm::event::MouseEventKind::{Down, Up};
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let screen = render(app, 70, 20);
    let row = screen
        .lines()
        .position(|line| line.contains("re-renders only the tail"))
        .expect("answer row") as u16;
    let column = screen
        .lines()
        .nth(row as usize)
        .unwrap()
        .find("re-renders")
        .unwrap() as u16
        + 3;
    let selected = |app: &App| app.selection.and_then(|drag| app.selected_text(drag));
    for _ in 0..2 {
        left(app, Down, column, row);
        left(app, Up, column, row);
    }
    assert_eq!(selected(app).as_deref(), Some("re-renders"));
    left(app, Down, column, row);
    left(app, Up, column, row);
    assert_eq!(
        selected(app).as_deref(),
        Some("prefix and re-renders only the tail, so a delta costs one small"),
        "the row without its indent"
    );
    left(app, Down, column, row);
    assert_eq!(app.selection, None, "a fourth press starts over");
}

#[test]
fn a_detached_view_shows_the_jump_label_and_clicking_it_follows() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    conversation(app);
    let screen = render(app, 70, 20);
    assert!(!screen.contains("Jump to latest"), "following:\n{screen}");
    app.scroll_by(-3);
    let screen = render(app, 70, 20);
    let area = app.layout.viewport.area();
    let last = screen.lines().nth(area.bottom() as usize - 1).unwrap();
    assert!(last.contains("↓ Jump to latest message · End"), "{screen}");
    let column = last.find("Jump").unwrap() as u16;
    click(app, column, area.bottom() - 1);
    assert_eq!(app.scroll, Scroll::Follow);
}

#[test]
fn the_scrollbar_shows_after_a_scroll_and_its_track_jumps() {
    use crossterm::event::MouseEventKind::{Down, Drag, Up};
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    for _ in 0..3 {
        conversation(app);
    }
    let screen = render(app, 70, 20);
    assert!(!screen.contains('┃'), "hidden until a scroll:\n{screen}");
    app.scroll_by(-1);
    let screen = render(app, 70, 20);
    let area = app.layout.viewport.area();
    let bar: String = screen
        .lines()
        .take(area.height as usize)
        .map(|line| line.chars().nth(69).unwrap_or(' '))
        .collect();
    assert!(
        bar.ends_with('┃') && bar.starts_with('│'),
        "thumb at the bottom: {bar:?}"
    );
    left(app, Down, 69, area.y);
    let view = app.layout.viewport;
    let (_, len) =
        crate::app::scrollbar::thumb(view.height, app.layout.total_rows(), view.top).unwrap();
    assert_eq!(
        app.scrollbar_grab,
        Some(len / 2),
        "a track press grabs the thumb's centre"
    );
    render(app, 70, 20);
    assert_eq!(app.layout.viewport.top, 0, "and jumps it to the top");
    left(app, Drag, 69, area.bottom() - 1);
    left(app, Up, 69, area.bottom() - 1);
    assert_eq!(app.scroll, Scroll::Follow, "dragged to the end");
    assert_eq!(app.selection, None, "the bar never selects");
}
fn approval() -> crate::app::PendingInteraction {
    interaction(
        json!({"kind":"approval","prompt":"Allow tool `write` for this call?","risk":"medium"}),
        "write",
        "call_w",
    )
}
fn interaction(request: Value, name: &str, call: &str) -> crate::app::PendingInteraction {
    crate::app::PendingInteraction {
        data:serde_json::from_value(json!({"interaction_id":native_id(1),"tool_call_id":call,"name":name,"request":request})).unwrap(),
        turn_id:turn_id(9),load_id:native_id(1).parse().unwrap(),selected:0,submitted:false,
    }
}

#[test]
fn an_approval_shows_the_tool_its_target_and_its_input() {
    let mut app = app();
    let session = app.resident.current_mut().unwrap();
    let mut previous = ToolCall::new(
        ToolCallRef {
            turn_id: turn_id(8),
            tool_call_id: "call_w".to_owned().into(),
        },
        "write",
    );
    previous.detail = "previous-turn.txt".into();
    previous.arguments = Some(serde_json::json!({"content":"Wrong turn input"}));
    session.transcript.push(Block::Tool(Box::new(previous)));
    let mut call = ToolCall::new(
        ToolCallRef {
            turn_id: turn_id(9),
            tool_call_id: "call_w".to_owned().into(),
        },
        "write",
    );
    call.detail = "notes/approval.txt".into();
    call.arguments = Some(serde_json::json!({"content":"Approval fixture executed.\nsecond line"}));
    session.transcript.push(Block::Tool(Box::new(call)));
    session.interaction = Some(approval());
    let rendered = render(&mut app, 80, 24);
    assert!(rendered.contains("write  notes/approval.txt"), "{rendered}");
    assert!(
        rendered.contains("Approval fixture executed.") && rendered.contains("Allow once"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Waiting for approval") && rendered.contains("esc deny"),
        "{rendered}"
    );
    assert!(!rendered.contains("allow for session"));
    assert!(!rendered.contains("Wrong turn input"));
}

#[test]
fn a_choice_question_uses_input_wording_and_its_native_choice_index() {
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = app();
    app.resident.current_mut().unwrap().interaction = Some(interaction(
        json!({"kind":"tool_input","request":{"prompt":"Which branch?","choices":["main","feature"],"answer_kind":"single_choice"}}),
        "question",
        "question",
    ));
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("Waiting for input"), "{screen}");
    assert!(screen.contains("esc cancel turn"), "{screen}");
    assert!(!screen.contains("Waiting for approval"), "{screen}");
    assert!(app.on_key(KeyEvent::from(KeyCode::Char('y'))).is_none());
    app.on_key(KeyEvent::from(KeyCode::Down));
    assert_eq!(
        app.on_key(KeyEvent::from(KeyCode::Enter)),
        Some(crate::app::Action::Answer(crate::app::Answer::Choice(1)))
    );
}

#[test]
fn pending_and_submitted_interactions_allow_session_navigation_without_losing_the_answer() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    app.composer.set_text("conversation draft");
    let original = app.resident.current().unwrap().address();
    for request in [
        json!({"kind":"approval","prompt":"Allow this call?","risk":"low"}),
        json!({"kind":"tool_input","request":{"prompt":"Choose","choices":["one","two"],"answer_kind":"single_choice"}}),
        json!({"kind":"tool_input","request":{"prompt":"Answer","choices":[],"answer_kind":"text"}}),
    ] {
        for submitted in [false, true] {
            let session = app.resident.current_mut().unwrap();
            let mut pending = interaction(request.clone(), "question", "call-question");
            pending.submitted = submitted;
            let turn_id = pending.turn_id.clone();
            let interaction_id = pending.data.interaction_id.clone();
            session.interaction = Some(pending);
            session.interaction_editor.set_text("retained answer");
            assert!(
                app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
                    .is_none()
            );
            assert!(matches!(app.overlay, Overlay::Sessions { .. }));
            app.on_key(KeyEvent::from(KeyCode::Esc));
            let session = app.resident.current().unwrap();
            let pending = session.interaction.as_ref().unwrap();
            assert_eq!(session.address(), original);
            assert_eq!(pending.turn_id, turn_id);
            assert_eq!(pending.data.interaction_id, interaction_id);
            assert_eq!(pending.submitted, submitted);
            assert_eq!(session.interaction_editor.text(), "retained answer");
            assert_eq!(app.composer.text(), "conversation draft");
        }
    }
}

#[test]
fn a_text_question_uses_its_separate_editor_without_exposing_the_draft() {
    let mut app = app();
    app.composer.set_text("unsent conversation draft");
    let session = app.resident.current_mut().unwrap();
    session.interaction = Some(interaction(
        json!({"kind":"tool_input","request":{"prompt":"Which branch should be used?","choices":[],"answer_kind":"text"}}),
        "question",
        "question",
    ));
    session.interaction_editor.set_text("feature/answer-editor");
    let rendered = render(&mut app, 90, 25);
    assert!(
        rendered.contains("Which branch should be used?") && rendered.contains("question · input"),
        "{rendered}"
    );
    assert!(
        rendered.contains("feature/answer-editor") && rendered.contains("Waiting for input"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("unsent conversation draft") && !rendered.contains("Allow once"),
        "{rendered}"
    );
    assert_eq!(app.composer.text(), "unsent conversation draft");
}

fn running_bash(app: &mut App) {
    let session = app.resident.current_mut().unwrap();
    let mut live = live(session.transcript.len(), 2);
    live.running_tool = Some("bash".into());
    session.state.data.active_turn_id = Some(live.turn_id.clone());
    session.live = Some(live);
    app.started = std::time::Instant::now();
}

#[test]
fn the_sessions_picker_shows_live_and_unloaded_sessions_across_agents() {
    let mut app = app();
    let mut running = session(&app, 2, "Port the lexer");
    running.state.data.active_turn_id = Some(turn_id(2));
    running.live = Some(live(0, 2));
    let mut asking = session(&app, 3, "Clean the build");
    asking.interaction = Some(approval());
    let mut done = session(&app, 4, "Write the docs");
    done.unseen = true;
    let mut other = catalog(&session(&app, 5, "Review the parser"));
    other.agent_id = native_id(2).parse().unwrap();
    other.residency = minicore_host_protocol::Residency::Unloaded;
    app.agents.push(agent(2, "Reviewer"));
    app.sessions
        .extend([catalog(&running), catalog(&asking), catalog(&done), other]);
    for session in [running, asking, done] {
        app.resident.insert(session);
    }
    app.open_panel(crate::app::Panel::Sessions);
    let rendered = render(&mut app, 100, 20);
    assert!(
        rendered.contains("Reviewer") && rendered.contains("unloaded"),
        "{rendered}"
    );
    snapshot("sessions-picker-live", rendered);
}

fn session_picker_row<'a>(screen: &'a str, short_id: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(short_id))
        .unwrap_or_else(|| panic!("missing Session {short_id}:\n{screen}"))
}

#[test]
fn session_picker_keeps_ids_when_distinct_long_titles_clip_alike() {
    const FIRST: u64 = 11_111_100_000_000_000_001;
    const SECOND: u64 = 12_222_200_000_000_000_002;
    let mut app = app();
    let prefix = "Daily inventory review: inspect source inputs, validate business rules, compare reports, and prepare the requested updates for";
    let mut first = session(&app, FIRST, &format!("{prefix} CASE_CHARLIE"));
    let mut second = session(&app, SECOND, &format!("{prefix} CASE_DELTA"));
    for session in [&first, &second] {
        let mut info = catalog(session);
        info.residency = minicore_host_protocol::Residency::Unloaded;
        app.sessions.push(info);
    }
    app.open_panel(crate::app::Panel::Sessions);
    let idle = render(&mut app, 134, 20);
    let first_row = session_picker_row(&idle, "111111");
    let second_row = session_picker_row(&idle, "122222");
    assert!(first_row.contains("unloaded") && second_row.contains("unloaded"));
    assert_eq!(
        first_row.split_once("111111").unwrap().0.trim(),
        second_row.split_once("122222").unwrap().0.trim()
    );
    assert!(
        first_row.contains('…') && second_row.contains('…'),
        "{idle}"
    );
    assert!(!idle.contains("CASE_CHARLIE") && !idle.contains("CASE_DELTA"));

    for session in [&mut first, &mut second] {
        session.state.data.active_turn_id = Some(turn_id(session.epoch));
        session.live = Some(live(0, session.epoch));
        let info = app
            .sessions
            .iter_mut()
            .find(|info| info.id == session.info.id)
            .unwrap();
        *info = catalog(session);
    }
    app.resident.insert(first);
    app.resident.insert(second);
    app.open_panel(crate::app::Panel::Sessions);
    let running = render(&mut app, 134, 20);
    let first_row = session_picker_row(&running, "111111");
    let second_row = session_picker_row(&running, "122222");
    assert!(first_row.contains("running 0m") && second_row.contains("running 0m"));
    assert_eq!(
        first_row.split_once("111111").unwrap().0.trim(),
        second_row.split_once("122222").unwrap().0.trim()
    );
    assert!(
        first_row.contains('…') && second_row.contains('…'),
        "{running}"
    );
    assert!(!running.contains("CASE_CHARLIE") && !running.contains("CASE_DELTA"));

    app.resident.select(Some(FIRST));
    app.open_panel(crate::app::Panel::Sessions);
    let current = render(&mut app, 134, 20);
    let first_row = session_picker_row(&current, "111111");
    let second_row = session_picker_row(&current, "122222");
    assert!(first_row.contains("running 0m"), "{current}");
    assert!(first_row.trim_start().starts_with(['✓', '▎']), "{current}");
    assert!(second_row.contains("running 0m") && !second_row.contains('✓'));
    assert!(
        first_row.contains('…') && second_row.contains('…'),
        "{current}"
    );
    assert!(!current.contains("CASE_CHARLIE") && !current.contains("CASE_DELTA"));
}

#[test]
fn session_picker_keeps_ids_for_duplicate_and_renamed_titles() {
    let mut app = app();
    for index in [11_111_100_000_000_000_001, 12_222_200_000_000_000_002] {
        let session = session(&app, index, "SAME_NAME_CONTROL");
        app.sessions.push(catalog(&session));
    }
    app.open_panel(crate::app::Panel::Sessions);
    let screen = render(&mut app, 100, 20);
    for id in ["111111", "122222"] {
        let row = session_picker_row(&screen, id);
        assert!(row.contains("SAME_NAME_CONTROL"), "{screen}");
        assert!(row.contains("Coder"), "{screen}");
    }

    let first_id = native_id(11_111_100_000_000_000_001);
    let first = app
        .sessions
        .iter_mut()
        .find(|info| info.id.as_str() == first_id)
        .unwrap();
    first.title = Some("RENAMED_SESSION".into());
    app.open_panel(crate::app::Panel::Sessions);
    let renamed = render(&mut app, 100, 20);
    assert!(session_picker_row(&renamed, "111111").contains("RENAMED_SESSION"));
    assert!(session_picker_row(&renamed, "122222").contains("SAME_NAME_CONTROL"));

    if let Overlay::Sessions { filter, .. } = &mut app.overlay {
        *filter = "RENAMED_SESSION".into();
    }
    let filtered = render(&mut app, 100, 20);
    assert!(session_picker_row(&filtered, "111111").contains("RENAMED_SESSION"));
    assert!(!filtered.contains("122222"));
    app.open_panel(crate::app::Panel::Sessions);
    let reopened = render(&mut app, 100, 20);
    assert!(session_picker_row(&reopened, "111111").contains("RENAMED_SESSION"));
    assert!(session_picker_row(&reopened, "122222").contains("SAME_NAME_CONTROL"));
}

#[test]
fn session_picker_keeps_ids_when_narrow_rows_have_long_unicode_metadata() {
    let mut app = app();
    app.agents[0].name = "工作代理名称".repeat(20);
    for (index, suffix) in [
        (11_111_100_000_000_000_001, "甲"),
        (12_222_200_000_000_000_002, "乙"),
    ] {
        let title = format!("{}{suffix}", "共享会话标题".repeat(20));
        let mut session = session(&app, index, &title);
        session.info.config.workspace = Some(
            serde_json::from_value(json!({"path":format!("/work/{}", "项目工作区".repeat(20))}))
                .unwrap(),
        );
        session.state.data.active_turn_id = Some(turn_id(index));
        session.live = Some(live(0, index));
        app.sessions.push(catalog(&session));
        app.resident.insert(session);
    }
    app.open_panel(crate::app::Panel::Sessions);
    for columns in [9, 10, 11, 24, 40, 80] {
        let screen = render(&mut app, columns, 20);
        for id in ["111111", "122222"] {
            let row = session_picker_row(&screen, id);
            assert_eq!(row.contains('…'), columns >= 11, "{screen}");
        }
    }
}

#[test]
fn the_agent_picker_uses_real_owners_including_stopped_agents() {
    let mut app = app();
    let mut other = agent(2, "Reviewer");
    other.lifecycle = minicore_host_protocol::AgentLifecycle::Stopped;
    app.agents.push(other);
    app.open_panel(crate::app::Panel::Agents);
    let rendered = render(&mut app, 90, 18);
    assert!(
        rendered.contains("Coder") && rendered.contains("Reviewer") && rendered.contains("stopped"),
        "{rendered}"
    );
    snapshot("agents-picker", rendered);
}

#[test]
fn deleting_an_agent_requires_a_key_and_keeps_the_captured_session_scope() {
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = app();
    let child = catalog(&session(&app, 2, "Captured session"));
    app.agents[0].sessions.push(child.clone());
    app.open_panel(crate::app::Panel::AgentDelete);
    assert!(app.on_key(KeyEvent::from(KeyCode::Enter)).is_none());
    assert!(matches!(app.overlay, Overlay::AgentDelete { .. }));
    let screen = render(&mut app, 100, 20);
    assert!(
        screen.contains("Captured session")
            && screen.contains("Permanently delete Agent and 1 session"),
        "{screen}"
    );
    let later = catalog(&session(&app, 3, "Later session"));
    app.agents[0].sessions.push(later);
    match app.on_key(KeyEvent::from(KeyCode::Char('y'))) {
        Some(crate::app::Action::DeleteAgent {
            agent_id,
            session_ids,
        }) => {
            assert_eq!(agent_id, child.agent_id);
            assert_eq!(session_ids, vec![child.id]);
        }
        other => panic!("expected captured deletion, got {other:?}"),
    }
}

#[test]
fn session_picker_identity_and_confirmation_include_the_owning_agent() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    let original = app.resident.current().unwrap().address();
    let mut same_id = app.sessions[0].clone();
    same_id.agent_id = native_id(2).parse().unwrap();
    same_id.title = Some("Other Agent's session".into());
    app.agents.push(agent(2, "Reviewer"));
    app.sessions.push(same_id.clone());
    app.open_panel(crate::app::Panel::Sessions);
    let sheet = crate::ui::sheets::sheet(&app, app.styles).unwrap();
    assert_eq!(
        sheet
            .rows
            .iter()
            .filter(|row| row.mark == Some(true))
            .count(),
        1
    );
    let confirmed = |app: &App| match &app.overlay {
        Overlay::Confirm {
            action: crate::app::Action::DeleteSession(target),
            ..
        } => target.clone(),
        _ => panic!("deletion needs explicit confirmation"),
    };
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(confirmed(&app), original);
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.open_panel(crate::app::Panel::Sessions);
    if let Overlay::Sessions { selected, .. } = &mut app.overlay {
        *selected = 1;
    }
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(
        confirmed(&app),
        crate::app::overlay::session_target(&same_id)
    );
}

#[test]
fn session_rename_keeps_its_target_when_the_catalog_reorders() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    let original = app.resident.current().unwrap().address();
    let other = catalog(&session(&app, 2, "Later session"));
    app.sessions.push(other);
    app.open_panel(crate::app::Panel::Sessions);
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    if let Overlay::Sessions { rows, .. } = &mut app.overlay {
        rows.reverse();
    }
    app.on_key(KeyEvent::from(KeyCode::Char('!')));
    assert_eq!(
        app.on_key(KeyEvent::from(KeyCode::Enter)),
        Some(crate::app::Action::RenameSession {
            target: original,
            title: "Cache the parser!".to_owned(),
        })
    );
}

#[test]
fn rapid_second_click_on_a_foldable_row_toggles_instead_of_selecting_a_word() {
    let mut fixture = app();
    let _runtime = fixture._runtime.enter();
    let app = &mut fixture.app;
    conversation(app);
    let folded = render(app, 70, 30);
    let header = row_of(&folded, " ✓ bash");
    click(app, 5, header);
    let expanded = render(app, 70, 30);
    assert!(expanded.contains(" ▎ $ cargo test --lib"), "{expanded}");
    assert!(app.selection.is_none());
    // Immediately clicking the same cell within DOUBLE_CLICK collapses again
    // without triggering double-click word selection.
    click(app, 5, header);
    assert!(app.selection.is_none(), "toggle click must not select word");
    let collapsed = render(app, 70, 30);
    assert!(!collapsed.contains(" ▎ $ cargo test --lib"), "{collapsed}");
}

#[test]
fn busy_child_session_shows_cancel_hint_alongside_esc_back() {
    let mut app = app();
    let parent = app.resident.current().unwrap().address();
    let child = session(&app, 2, "Child worker");
    let child_target = child.address();
    app.resident.insert(child);
    app.resident.select(Some(2));
    app.return_to = Some((child_target, parent));
    running_bash(&mut app);
    let rendered = render(&mut app, 110, 14);
    let activity = rendered
        .lines()
        .find(|line| line.contains("Working 0m"))
        .unwrap_or_default();
    assert!(activity.contains("↰ esc back"), "{activity}");
    assert!(activity.contains("ctrl+b · stop in Tasks"), "{activity}");
}

#[test]
fn find_opens_a_closed_work_group_to_reveal_its_match() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = app();
    let mut first = tool();
    first.detail = "$ echo alpha_unique".into();
    let mut second = tool();
    second.key = crate::transcript::ToolKey::Call(ToolCallRef {
        turn_id: turn_id(1),
        tool_call_id: "call_2".to_owned().into(),
    });
    second.detail = "$ echo beta_step".into();
    let transcript = &mut app.resident.current_mut().unwrap().transcript;
    transcript.push(Block::Tool(Box::new(first)));
    transcript.push(Block::Tool(Box::new(second)));
    transcript.regroup(&turn_id(1));
    assert!(!transcript.grouping(0).is_open());

    app.open_panel(crate::app::Panel::Find);
    for ch in "alpha_unique".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let transcript = &app.resident.current().unwrap().transcript;
    assert!(
        transcript.grouping(0).is_open(),
        "Find must open the enclosing work group"
    );
}

#[test]
fn slash_popup_floats_above_composer_and_double_click_confirms_sheets() {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    let mut app = app();
    app.composer.set_text("/tas");
    app.overlay = Overlay::Palette { selected: 0 };
    let screen = render(&mut app, 110, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let popup_row = lines
        .iter()
        .position(|line| line.contains("─ Commands"))
        .expect("floating Commands header");
    let composer_row = lines
        .iter()
        .position(|line| line.contains("▎/tas"))
        .expect("pinned composer row");
    assert!(
        popup_row < composer_row,
        "slash popup must float above the pinned composer:\n{screen}"
    );
    assert!(
        screen.contains("ctrl+b"),
        "hotkey badge in palette:\n{screen}"
    );

    // Single click on row 1 of the Agents sheet highlights without closing;
    // double-click on the same row confirms and closes the sheet.
    app.overlay = Overlay::None;
    app.composer.set_text("");
    let second_agent = crate::ui::snapshots::agent(2, "Reviewer");
    app.agents.push(second_agent);
    app.open_panel(crate::app::Panel::Agents);
    let _ = render(&mut app, 100, 20);
    assert!(app.sheet_hit.list_rows >= 2, "expected agent rows");
    let click_y = app.sheet_hit.list_y + 1;
    let click_x = app.sheet_hit.area.x + 3;
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: click_x,
        row: click_y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        matches!(app.overlay, Overlay::Agents { selected: 1, .. }),
        "single click must select row 1 without dismissing"
    );
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: click_x,
        row: click_y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.overlay.is_none(),
        "double click on the same sheet row must confirm and close"
    );

    // Unified /diff split view supports live filename filtering and detail focus.
    let mut diff = crate::app::diff::DiffState::default();
    diff.set_files(vec![
        crate::app::diff::DiffFile {
            path: "src/ui/chrome.rs".into(),
            kind: minicore_host_workspace::ChangeKind::Added,
            lines: Some((120, 0)),
            failed: false,
            change_refs: Vec::new(),
        },
        crate::app::diff::DiffFile {
            path: "src/ui/views/diff.rs".into(),
            kind: minicore_host_workspace::ChangeKind::Modified,
            lines: Some((42, 18)),
            failed: false,
            change_refs: Vec::new(),
        },
    ]);
    app.overlay = Overlay::Diff(diff);
    let wide = render(&mut app, 110, 20);
    assert!(wide.contains("Git diff · 2 files · +162 −18"), "{wide}");
    for ch in "diff".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let filtered = render(&mut app, 110, 20);
    assert!(filtered.contains("showing 1/2"), "{filtered}");
    assert!(filtered.contains("src/ui/views/diff.rs"), "{filtered}");
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(matches!(&app.overlay, Overlay::Diff(d) if d.filter.is_empty()));
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(
        &app.overlay,
        Overlay::Diff(d) if d.focus == crate::ui::views::split::PaneFocus::Detail
    ));
}
