//! Drives a real `App` against the scripted Agent in `crate::testing`.
//!
//! Tests run on paused tokio time, so retries and quiet periods cost nothing.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::app::keys::Input;
use crate::app::{Action, App, Msg, OpDone, OpKind};
use crate::testing::FakeAgent;
use crate::transcript::Block;

pub use crate::testing::{chunk, display_read, session_info, turn};

/// The channel counts as settled after this much quiet; longer than any retry
/// or scripted delay.
const QUIET: Duration = Duration::from_secs(5);

pub struct Harness {
    pub app: App,
    pub fake: FakeAgent,
    rx: UnboundedReceiver<Msg>,
}

impl Harness {
    /// A fake Agent with the catalog and the per-session reads scripted.
    pub fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        let (fake, app) = FakeAgent::start(tx);
        fake.script(
            "agent.ping",
            json!({"version": "0.7.0", "protocol_version": 1, "capabilities": [
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
        fake.script("session.open", json!({"session": session_info("s1")}));
        fake.script("session.rename", json!({"session": session_info("s1")}));
        fake.script(
            "session.presentation",
            json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
                   "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
        );
        fake.script(
            "session.state",
            json!({"session_id": "s1", "status": "idle"}),
        );
        fake.script("session.context", json!({"session_id": "s1"}));
        fake.script("session.read", display_read(0, 0));
        fake.script(
            "changes.list",
            json!({"records": [], "total": 0, "complete": true}),
        );
        Self { app, fake, rx }
    }

    /// Booted, with `s1` open and its tail read from `read`.
    pub async fn opened(read: Value) -> Self {
        let mut harness = Self::new();
        harness.fake.script("session.read", read);
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
        self.until(|app| app.session.is_some()).await;
        self.settle().await;
    }

    /// Applies messages until none arrives for a while.
    pub async fn settle(&mut self) {
        while let Ok(Some(message)) = tokio::time::timeout(QUIET, self.rx.recv()).await {
            self.app.on_msg(message);
        }
    }

    /// Applies the messages that arrive within `duration`.
    pub async fn run_for(&mut self, duration: Duration) {
        let deadline = tokio::time::Instant::now() + duration;
        while let Ok(Some(message)) = tokio::time::timeout_at(deadline, self.rx.recv()).await {
            self.app.on_msg(message);
        }
    }

    /// Applies messages until `check` holds.
    pub async fn until(&mut self, check: impl Fn(&App) -> bool) {
        for _ in 0..2000 {
            if check(&self.app) {
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
        if let Input::Action(action) = self.app.on_key(KeyEvent::new(code, modifiers)) {
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

    /// Sends an Agent event for `s1`; `meta` is filled in unless given.
    pub async fn emit(&mut self, kind: &str, mut data: Value) {
        if data.get("meta").is_none() {
            data["meta"] = json!({"session_id": "s1", "dropped_before": 0});
        }
        self.fake.emit(json!({"type": kind, "data": data})).await;
    }

    /// A `session.state` answer arriving for the open session.
    pub fn state(&mut self, state: Value) {
        self.op(OpKind::State(Ok(serde_json::from_value(state).unwrap())));
    }

    /// A `session.context` answer arriving for the open session.
    pub fn context(&mut self, context: Value) {
        self.op(OpKind::Context(
            Ok(serde_json::from_value(context).unwrap()),
        ));
    }

    fn op(&mut self, kind: OpKind) {
        let epoch = self.app.session.as_ref().map_or(0, |session| session.epoch);
        self.app.on_msg(Msg::Op(Box::new(OpDone {
            session: Some("s1".into()),
            epoch,
            kind,
        })));
    }

    pub fn blocks(&self) -> &[Block] {
        self.app.transcript_blocks()
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

    pub fn queue(&self) -> Vec<String> {
        let session = self.app.session.as_ref().expect("a session");
        session
            .queue
            .iter()
            .map(|queued| queued.text.clone())
            .collect()
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

    /// The texts of every `turn.send`, in order.
    pub fn sends(&self) -> Vec<String> {
        self.calls("turn.send")
            .iter()
            .filter_map(|params| params["text"].as_str().map(str::to_owned))
            .collect()
    }

    /// Rows the transcript showed in the last frame.
    pub fn viewport(&self) -> usize {
        self.app.layout.cache.viewport.height
    }

    /// One frame of the whole screen as text.
    pub fn screen(&mut self, width: u16, height: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut self.app))
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

/// A display `session.read` page holding `items`.
pub fn page(total: usize, first_item: usize, items: Vec<Value>) -> Value {
    let mut page = display_read(total, first_item);
    page["items"] = json!(items);
    page
}

pub fn user(index: usize, loop_id: &str, text: &str) -> Value {
    chunk(
        index,
        json!({"type": "user", "data": {"loop_id": loop_id, "kind": "prompt",
                                        "input": {"text": text}}}),
    )
}

pub fn assistant(index: usize, loop_id: &str, text: &str) -> Value {
    chunk(
        index,
        json!({"type": "assistant", "data": {"loop_id": loop_id, "request_index": 0,
            "model": "mock", "content": [{"type": "text", "data": text}]}}),
    )
}

/// A `turn.wait` answer for `loop_id`.
pub fn waited(loop_id: &str, outcome: Value) -> Value {
    json!({"turn": turn(loop_id), "outcome": outcome})
}

pub fn completed() -> Value {
    json!({"type": "completed"})
}

pub fn cancelled() -> Value {
    json!({"type": "cancelled", "reason": "user"})
}

/// A `turn.result` page with `items`.
pub fn turn_result(loop_id: &str, items: Vec<Value>) -> Value {
    json!({"turn": turn(loop_id), "outcome": {"type": "cancelled", "reason": "user"},
           "total": items.len(), "items": items, "next_cursor": null})
}

/// A refusal in the shape `FakeAgent::script_sequence` accepts.
pub fn refusal(code: i64, kind: &str) -> Value {
    json!({"error": {"code": code, "message": kind, "data": {"kind": kind}}})
}
