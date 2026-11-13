use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use minicore_host_protocol::{AgentConfig, SessionOverrides, SessionRef};
use ratatui::{Terminal, backend::TestBackend};
use tokio::sync::mpsc;

use crate::app::{App, Msg, tasks};
use crate::testing::{ModelCall, NativeHost, real_bounded};

pub struct Harness {
    pub app: App,
    pub native: NativeHost,
    pub messages: mpsc::Receiver<Msg>,
}

impl Harness {
    pub async fn opened() -> Self {
        let native = real_bounded(NativeHost::new()).await;
        Self::from_native(native).await
    }

    pub async fn from_native(native: NativeHost) -> Self {
        let (tx, messages) = mpsc::channel(tasks::MESSAGE_CAPACITY);
        let app = App::new(native.client.clone(), tx.clone(), std::env::temp_dir());
        tasks::bootstrap(
            native.client.clone(),
            tx,
            AgentConfig::default(),
            Some(native.target.agent_id.clone()),
            SessionOverrides::default(),
        )
        .await;
        let mut harness = Self {
            app,
            native,
            messages,
        };
        harness.until(|app| app.ready).await;
        let target = harness.native.target.clone();
        harness.app.open_session(target.clone());
        harness
            .until(|app| {
                !app.loading_session
                    && app
                        .resident
                        .current()
                        .is_some_and(|session| session.address() == target)
            })
            .await;
        harness
    }

    pub async fn until(&mut self, ready: impl Fn(&App) -> bool) {
        real_bounded(async {
            while !ready(&self.app) {
                let message = self
                    .messages
                    .recv()
                    .await
                    .expect("App message stream ended");
                self.app.on_msg(message);
                assert!(self.app.fatal.is_none(), "{:?}", self.app.fatal);
            }
        })
        .await;
    }

    pub async fn next_call(&mut self) -> ModelCall {
        real_bounded(async {
            loop {
                tokio::select! {
                    call = self.native.calls.recv() => return call.expect("model fixture ended"),
                    message = self.messages.recv() => {
                        self.app.on_msg(message.expect("App message stream ended"));
                        assert!(self.app.fatal.is_none(), "{:?}", self.app.fatal);
                    }
                }
            }
        })
        .await
    }

    pub async fn another_session(&mut self) -> SessionRef {
        real_bounded(tasks::create_session(
            &self.native.client,
            self.native.target.agent_id.clone(),
            SessionOverrides::default(),
        ))
        .await
        .unwrap()
    }

    pub async fn open(&mut self, target: &SessionRef) {
        self.app.open_session(target.clone());
        self.until(|app| {
            !app.loading_session
                && app
                    .resident
                    .current()
                    .is_some_and(|session| session.address() == *target)
        })
        .await;
    }

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
    pub fn notices(&self) -> Vec<&str> {
        self.app
            .notices
            .iter()
            .map(|notice| notice.text.as_str())
            .collect()
    }
    pub fn screen(&mut self, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                crate::ui::draw(frame, &mut self.app);
            })
            .unwrap();
    }
    pub async fn finish(mut self) {
        self.messages.close();
        self.app.local_files.finish().await;
        drop(self.app);
        self.native.finish().await;
    }
}
