//! Drives a real `App` against the scripted Agent in `crate::testing`.
//!
//! Tests run on paused tokio time, so retries and quiet periods cost nothing.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::app::{Action, App, Msg, Session, SessionOp};
use crate::testing::FakeAgent;
use crate::transcript::Block;

pub use crate::testing::fixtures::*;

/// The channel counts as settled after this much quiet; longer than any retry
/// or scripted delay.
const QUIET: Duration = Duration::from_secs(5);

/// More messages than any test needs; a run past it is a loop.
const MAX_MESSAGES: usize = 10_000;

pub struct Harness {
    pub app: App,
    pub fake: FakeAgent,
    rx: UnboundedReceiver<Msg>,
}

impl Harness {
    pub fn session(&self) -> &Session {
        self.app.resident.current().expect("a current session")
    }

    pub fn session_mut(&mut self) -> &mut Session {
        self.app.resident.current_mut().expect("a current session")
    }

    /// A fake Agent with the catalog and the per-session reads scripted.
    pub fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        let (fake, app) = FakeAgent::start(tx);
        fake.script(
            "agent.ping",
            json!({"version": "0.9.3", "protocol_version": 3, "capabilities": [
                "session.read", "session.read.display", "session.context", "turn.result",
                "tool.read", "tool.read.display", "tool.output", "workspace.files",
                "workspace.search", "workspace.status", "changes.list", "changes.diff"
            ]}),
        );
        fake.script(
            "model.list",
            json!({"models": [{
                "id": "mock", "model_ref": "mock/large", "context_window": 200000,
                "supports_tools": true, "supported_reasoning": ["low", "medium", "high"],
            }]}),
        );
        fake.script(
            "profile.list",
            json!({"profiles": [{"id": "coding", "model": "mock", "reasoning": "medium"}]}),
        );
        fake.script("session.list", json!({"sessions": [session_info("s1")]}));
        fake.script(
            "session.open",
            json!({"session": session_info("s1"), "load": "load_1",
                   "snapshot": snapshot("s1", "load_1", 1)}),
        );
        fake.script("session.rename", json!({"session": session_info("s1")}));
        fake.script(
            "session.create",
            json!({"session": session_info("s1"), "load": "load_1",
                   "snapshot": snapshot("s1", "load_1", 1)}),
        );
        fake.script(
            "session.presentation",
            json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
                   "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
        );
        fake.script(
            "session.state",
            state_response(snapshot("s1", crate::testing::LOAD, 1)),
        );
        fake.script("session.context", json!({"session_id": "s1"}));
        fake.script("workspace.status", json!({}));
        fake.script("input.list", queue(vec![], false));
        fake.script("input.remove", json!({"text": ""}));
        fake.script("input.recall", json!({"texts": []}));
        fake.script("input.resume", json!({"start_error": null}));
        fake.script("session.read", history(display_read(0, 0)));
        fake.script(
            "changes.list",
            json!({"records": [], "total": 0, "complete": true, "warnings": []}),
        );
        for method in [
            "session.close",
            "session.delete",
            "session.configure",
            "interaction.answer",
            "turn.cancel",
        ] {
            fake.script(method, json!({}));
        }
        Self { app, fake, rx }
    }

    /// Booted, with `s1` open and its tail read from `read`.
    pub async fn opened(read: Value) -> Self {
        let mut harness = Self::new();
        harness.fake.script("session.read", history(read));
        harness.open().await;
        harness
    }

    pub async fn boot(&mut self) {
        crate::app::tasks::bootstrap(self.fake.client.clone(), self.app.tx.clone()).await;
        self.until(|app| app.ready).await;
    }

    /// Boots and opens `s1`.
    pub async fn open(&mut self) {
        self.boot().await;
        self.action(Action::OpenSession("s1".into()));
        self.until(|app| app.resident.current().is_some()).await;
        self.settle().await;
    }

    async fn drain(&mut self) {
        for _ in 0..MAX_MESSAGES {
            let Ok(Some(message)) = tokio::time::timeout(QUIET, self.rx.recv()).await else {
                self.fake.assert_scripted();
                return;
            };
            self.app.on_msg(message);
        }
        panic!("messages never went quiet");
    }

    /// Applies messages until none arrives for a while.
    pub async fn settle(&mut self) {
        crate::testing::real_bounded(self.drain()).await;
    }

    pub async fn next_message(&mut self) -> Msg {
        crate::testing::real_bounded(self.rx.recv())
            .await
            .expect("an actual reducer message")
    }

    /// Applies the messages that arrive within `duration`.
    pub async fn run_for(&mut self, duration: Duration) {
        let deadline = tokio::time::Instant::now() + duration;
        for _ in 0..MAX_MESSAGES {
            let Ok(Some(message)) = tokio::time::timeout_at(deadline, self.rx.recv()).await else {
                return;
            };
            self.app.on_msg(message);
        }
        panic!("still busy after {MAX_MESSAGES} messages");
    }

    /// Applies messages until `check` holds.
    pub async fn until(&mut self, check: impl Fn(&App) -> bool) {
        for _ in 0..2000 {
            if check(&self.app) {
                self.fake.assert_scripted();
                return;
            }
            if let Ok(Some(message)) =
                tokio::time::timeout(Duration::from_millis(10), self.rx.recv()).await
            {
                self.app.on_msg(message);
            }
        }
        panic!(
            "never reached the expected state; notices: {:?}",
            self.notices()
        );
    }

    pub fn action(&mut self, action: Action) {
        self.app.on_action(action);
    }

    /// Puts `text` in the composer and presses Enter.
    pub fn submit(&mut self, text: &str) {
        self.app.composer.set_text(text);
        self.action(Action::Submit);
    }

    /// One key press; the action it resolves to runs as the main loop runs it.
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if let Some(action) = self.app.on_key(KeyEvent::new(code, modifiers)) {
            self.app.on_action(action);
        }
    }

    pub fn press(&mut self, code: KeyCode) {
        self.key(code, KeyModifiers::NONE);
    }

    pub fn ctrl(&mut self, code: KeyCode) {
        self.key(code, KeyModifiers::CONTROL);
    }

    pub fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.press(KeyCode::Char(character));
        }
    }

    /// Sends an Agent event exactly as given, `meta` included.
    pub async fn emit_no_meta(&mut self, kind: &str, data: Value) {
        self.fake.emit(json!({"type": kind, "data": data})).await;
    }

    /// Sends an Agent event for `s1`; `meta` is filled in unless given.
    pub async fn emit(&mut self, kind: &str, mut data: Value) {
        if data.get("meta").is_none() {
            data["meta"] =
                json!({"session_id": "s1", "load": crate::testing::LOAD, "dropped_before": 0});
        }
        if data["meta"]["load"].is_null() {
            data["meta"]["load"] = json!(crate::testing::LOAD);
        }
        self.fake.emit(json!({"type": kind, "data": data})).await;
    }

    /// Applies exactly the supplied snapshot through the query response path.
    pub fn state(&mut self, snapshot: Value) {
        self.op(SessionOp::State(Ok(serde_json::from_value(
            state_response(snapshot),
        )
        .unwrap())));
    }

    pub fn running_state(&mut self, loop_id: &str, input: Value) {
        self.state(crate::testing::snapshot_running(
            "s1",
            crate::testing::LOAD,
            self.next_revision(),
            loop_id,
            input,
        ));
    }

    /// A `session.context` answer arriving for the open session.
    pub fn context(&mut self, context: Value) {
        self.op(SessionOp::Context(Ok(
            serde_json::from_value(context).unwrap()
        )));
    }

    /// A result arriving for the open session.
    pub fn op(&mut self, op: SessionOp) {
        let epoch = self
            .app
            .resident
            .current()
            .map_or(0, |session| session.epoch);
        self.app.on_msg(Msg::Session(epoch, Box::new(op)));
    }

    pub fn blocks(&self) -> &[Block] {
        self.app.transcript_blocks()
    }

    /// The time of the user block with `text`.
    pub fn sent_at(&self, text: &str) -> Option<u64> {
        self.blocks().iter().find_map(|block| match block {
            Block::User {
                text: found, at, ..
            } if found == text => *at,
            _ => None,
        })
    }

    /// User and assistant texts, in transcript order.
    pub fn texts(&self) -> Vec<String> {
        self.blocks()
            .iter()
            .filter_map(|block| match block {
                Block::User { text, .. } | Block::Assistant { text, .. } if !text.is_empty() => {
                    Some(text.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The count on the earlier-history marker, if one is shown.
    pub fn marker(&self) -> Option<usize> {
        self.blocks().iter().find_map(|block| match block {
            Block::EarlierHistory { hidden_items } => Some(*hidden_items),
            _ => None,
        })
    }

    /// Operation ids of the compaction markers, in order.
    pub fn compactions(&self) -> Vec<String> {
        self.blocks()
            .iter()
            .filter_map(|block| match block {
                Block::Compaction { operation_id, .. } => Some(operation_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// The texts in the mirrored queue, in the order the Agent reported them.
    pub fn queue(&self) -> Vec<String> {
        let session = self.app.resident.current().expect("a session");
        session
            .queue
            .items
            .iter()
            .map(|queued| queued.text.clone())
            .collect()
    }

    /// Seeds the displayed queue; later Agent observations are scripted separately.
    pub fn mirror(&mut self, items: Vec<Value>, paused: bool) {
        self.app.resident.current_mut().expect("a session").queue =
            serde_json::from_value(queue(items, paused)).unwrap();
    }

    /// A `session_snapshot` event, which is how the Agent reports control.
    pub async fn snapshot_event(&mut self, snapshot: Value) {
        let session_id = snapshot["session_id"].as_str().unwrap_or("s1").to_owned();
        self.emit(
            "session_snapshot",
            json!({"meta": {"session_id": session_id, "dropped_before": 0},
                   "snapshot": snapshot}),
        )
        .await;
    }

    pub fn next_revision(&self) -> u64 {
        self.app
            .resident
            .current()
            .map_or(2, |session| session.recovery.revision + 1)
    }

    /// A control snapshot whose fields the caller patches, revised past the
    /// last one this session applied.
    pub fn control(&self, patch: Value) -> Value {
        let session = self.app.resident.current().expect("a session");
        let mut snapshot =
            crate::testing::snapshot("s1", &session.load, session.recovery.revision + 1);
        for (key, value) in patch.as_object().expect("a patch object") {
            snapshot[key] = value.clone();
        }
        snapshot
    }

    /// Reports control through the authoritative snapshot event.
    pub async fn control_event(&mut self, patch: Value) {
        let snapshot = self.control(patch);
        self.snapshot_event(snapshot).await;
    }

    /// The bells in the terminal bytes now, not counting a title sequence's terminator.
    pub fn bells(&mut self) -> usize {
        let bytes = self.app.terminal_bytes();
        bytes.matches('\x07').count() - bytes.matches("\x1b]0;").count()
    }

    pub fn is_busy(&self) -> bool {
        self.app.is_busy()
    }

    pub fn notices(&self) -> Vec<String> {
        self.app
            .notices
            .iter()
            .map(|notice| notice.text.clone())
            .collect()
    }

    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.fake.calls(method)
    }

    /// The mode and text of every `input.push`, in order.
    pub fn pushes(&self) -> Vec<(String, String)> {
        self.calls("input.push")
            .iter()
            .map(|params| {
                let field = |name: &str| params[name].as_str().unwrap().to_owned();
                (field("mode"), field("text"))
            })
            .collect()
    }

    /// The texts of every `input.push`, in order.
    pub fn sends(&self) -> Vec<String> {
        self.calls("input.push")
            .iter()
            .filter_map(|params| params["text"].as_str().map(str::to_owned))
            .collect()
    }

    /// Rows the transcript showed in the last frame.
    pub fn viewport(&self) -> usize {
        self.app.layout.viewport.height
    }

    /// The foreground of the first cell of `needle` on a 100x30 frame.
    pub fn fg_of(&mut self, needle: &str) -> ratatui::style::Color {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..30)
            .find_map(|y| {
                let row: String = (0..100).map(|x| buffer[(x, y)].symbol()).collect();
                row.find(needle).map(|x| buffer[(x as u16, y)].fg)
            })
            .unwrap_or_else(|| panic!("{needle:?} is not on screen"))
    }

    /// One frame of the whole screen as text.
    pub fn screen(&mut self, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        crate::terminal::draw_frame(&mut terminal, |frame| crate::ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let row: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Waits out the delay before an approval prompt takes answer keys.
pub async fn arm_prompt() {
    tokio::time::advance(crate::app::session::ARM_AFTER).await;
}
