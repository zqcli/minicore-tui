//! Local input acceptance ownership and explicit decisions about uncertain sends.

use super::{
    App, Msg, NoticeLevel, Overlay,
    composer::{Composer, Entry},
    resident::Target,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use minicore_host_protocol::{CallError, MessageAccepted, MessageId, MessageInput, MessageMode};

pub const MAX_OPERATIONS: usize = 64;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Retained review-editor memory, independent of native Host message admission.
pub const EDIT_BYTES: usize = MAX_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Sending,
    Unconfirmed,
    Rejected,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Unconfirmed => "unconfirmed",
            Self::Rejected => "rejected",
        }
    }
}

pub struct LocalInput {
    pub id: u64,
    pub target: Target,
    pub mode: MessageMode,
    pub message_id: MessageId,
    pub status: Status,
    pub retry_of: Option<u64>,
    pub entry: Entry,
    draft_version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewMode {
    List,
    Edit(u64),
    Resend(u64),
    ResendEdited(u64),
    Forget(u64),
}

impl ReviewMode {
    pub fn operation_id(self) -> Option<u64> {
        match self {
            Self::List => None,
            Self::Edit(id) | Self::Resend(id) | Self::ResendEdited(id) | Self::Forget(id) => {
                Some(id)
            }
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Edit(_) => "edit",
            Self::Resend(_) => "resend",
            Self::ResendEdited(_) => "resend_edited",
            Self::Forget(_) => "forget",
        }
    }
}

impl App {
    pub fn local_input_bytes(&self) -> usize {
        self.local_inputs
            .iter()
            .map(|input| input.entry.retained_bytes())
            .sum::<usize>()
            + self
                .pending_creations
                .values()
                .filter_map(|creation| creation.entry.as_ref())
                .map(Entry::retained_bytes)
                .sum::<usize>()
    }

    pub(super) fn local_input_room(&self, bytes: usize) -> bool {
        self.local_inputs.len()
            + self
                .pending_creations
                .values()
                .filter(|creation| creation.entry.is_some())
                .count()
            < MAX_OPERATIONS
            && MAX_BYTES
                .checked_sub(self.local_input_bytes())
                .is_some_and(|available| bytes <= available)
    }

    pub(super) fn push_local_input(
        &mut self,
        epoch: u64,
        mut entry: Entry,
        mode: MessageMode,
        retry_of: Option<u64>,
    ) -> bool {
        if !self.local_input_room(entry.retained_bytes()) {
            entry.discard_rollback();
        }
        if !self.local_input_room(entry.retained_bytes()) {
            self.notice(
                NoticeLevel::Error,
                "Local input confirmation budget is full; F8 to resolve retained inputs",
            );
            if self.composer.is_empty() {
                self.composer.restore_submission(entry);
            }
            return false;
        }
        let session = self
            .resident
            .get(epoch)
            .expect("local input has a resident session");
        let target = session.target();
        let id = self.next_operation();
        let message_id = match MessageId::new() {
            Ok(message_id) => message_id,
            Err(error) => {
                if retry_of.is_some() {
                    self.unconfirmed_editor.restore_submission(entry);
                } else {
                    self.composer.restore_submission(entry);
                }
                self.notice(NoticeLevel::Error, error.to_string());
                return false;
            }
        };
        let message = MessageInput {
            message_id: message_id.clone(),
            mode,
            text: entry.text().to_owned(),
            in_reply_to: None,
        };
        let client = self.client.session(target.session.clone());
        self.local_inputs.push_back(LocalInput {
            id,
            target,
            mode,
            message_id,
            status: Status::Sending,
            retry_of,
            entry,
            draft_version: self.composer.version(),
        });
        self.spawn_msg(async move {
            Msg::LocalPushed(id, client.send(message, super::tasks::options()).await)
        });
        true
    }

    pub(super) fn on_local_pushed(&mut self, id: u64, result: Result<MessageAccepted, CallError>) {
        let Some(index) = self.local_inputs.iter().position(|input| input.id == id) else {
            return;
        };
        if self.local_inputs[index].status != Status::Sending {
            return;
        }
        let target = self.local_inputs[index].target.clone();
        let result = result.and_then(|receipt| {
            if receipt.host_id != self.client.initial_info().host_id
                || receipt.host_incarnation != target.incarnation
                || receipt.receiver() != target.session
                || receipt.message_id != self.local_inputs[index].message_id
            {
                Err(CallError::Protocol {
                    message:
                        "message receipt does not identify the original Host, Session and input"
                            .into(),
                })
            } else {
                Ok(receipt)
            }
        });
        match result {
            Ok(receipt) => {
                let input = self.local_inputs.remove(index).unwrap();
                if self.composer.remember(input.entry)
                    && let Some(home) = &self.config_directory
                {
                    self.local_files.history(
                        &self.tx,
                        home.history_file(),
                        self.composer.history_records(),
                    );
                }
                if let Some(error) = receipt.result.start_error {
                    self.with_notice_for(&target.session, |app| {
                        app.notice(
                            NoticeLevel::Error,
                            format!("Input tui-{id} was accepted; {}", error.message()),
                        )
                    });
                }
                if self
                    .resident
                    .get(target.epoch)
                    .is_some_and(|session| session.target() == target)
                {
                    self.refresh_control_for(target.epoch);
                }
            }
            Err(CallError::Business(error)) => {
                let input = &mut self.local_inputs[index];
                input.status = Status::Rejected;
                let restore = input.retry_of.is_none()
                    && self.composer.is_empty()
                    && self.composer.version() == input.draft_version
                    && self
                        .resident
                        .current()
                        .is_some_and(|session| session.target() == target);
                if restore {
                    let input = self.local_inputs.remove(index).unwrap();
                    self.composer.restore_submission(input.entry);
                }
                self.with_notice_for(&target.session, |app| {
                    app.notice(NoticeLevel::Error, error.message())
                });
            }
            Err(error) => {
                let input = &mut self.local_inputs[index];
                input.status = Status::Unconfirmed;
                input.entry.discard_rollback();
                self.with_notice_for(&target.session, |app| app.notice(NoticeLevel::Error,format!("Input tui-{id} for {} is unconfirmed and may already be accepted; F8 to review: {}",target.session.session_id,error)));
            }
        }
    }

    pub(super) fn open_unconfirmed(&mut self) {
        self.unconfirmed_editor = Composer::new();
        self.overlay = Overlay::Unconfirmed {
            selected: 0,
            mode: ReviewMode::List,
        };
    }

    pub(crate) fn selected_local_input(&self) -> Option<&LocalInput> {
        let Overlay::Unconfirmed { selected, mode } = self.overlay else {
            return None;
        };
        match mode.operation_id() {
            Some(id) => self.local_inputs.iter().find(|input| input.id == id),
            None => self.local_inputs.get(selected),
        }
    }

    fn set_review_mode(&mut self, mode: ReviewMode) {
        let selection = self.selected_local_input().and_then(|input| {
            self.local_inputs
                .iter()
                .position(|known| known.id == input.id)
        });
        if let Overlay::Unconfirmed {
            selected,
            mode: current,
        } = &mut self.overlay
        {
            *selected = selection.unwrap_or(0);
            *current = mode;
        }
    }

    fn resend_local_input(&mut self, edited: bool) {
        let Some(input) = self.selected_local_input() else {
            return;
        };
        let target = input.target.clone();
        let mode = input.mode;
        let retry_of = input.id;
        if self.fatal.is_some()
            || !self
                .resident
                .get(target.epoch)
                .is_some_and(|session| session.target() == target)
        {
            self.notice(
                NoticeLevel::Error,
                "The original input target is unavailable; the unconfirmed input is retained",
            );
            return;
        }
        if !edited && !self.local_input_room(input.entry.retained_bytes()) {
            self.notice(
                NoticeLevel::Error,
                "Local input confirmation budget is full; forget a retained input before resending",
            );
            return;
        }
        let entry = if edited {
            self.unconfirmed_editor.take_submission()
        } else {
            input.entry.clone()
        };
        if entry.text().trim_end().is_empty() {
            if edited {
                self.unconfirmed_editor.restore_submission(entry);
                self.set_review_mode(ReviewMode::Edit(retry_of));
            }
            self.notice(
                NoticeLevel::Error,
                "Enter input text before sending; the original confirmation is retained",
            );
            return;
        }
        if !self.local_input_room(entry.retained_bytes()) {
            if edited {
                self.unconfirmed_editor.restore_submission(entry);
            }
            self.notice(
                NoticeLevel::Error,
                "Local input confirmation budget is full; forget a retained input before resending",
            );
            return;
        }
        if !self.push_local_input(target.epoch, entry, mode, Some(retry_of)) {
            return;
        }
        self.set_review_mode(ReviewMode::List);
        self.unconfirmed_editor = Composer::new();
    }

    pub(super) fn on_unconfirmed_key(&mut self, key: KeyEvent) {
        let Overlay::Unconfirmed { mode, .. } = self.overlay else {
            return;
        };
        match mode {
            ReviewMode::List => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => self.move_local_input(-1),
                KeyCode::Down | KeyCode::Tab => self.move_local_input(1),
                KeyCode::Char('e') | KeyCode::Enter => {
                    let Some(input) = self
                        .selected_local_input()
                        .filter(|input| input.status != Status::Sending)
                    else {
                        return;
                    };
                    if input.entry.text().len() > EDIT_BYTES {
                        self.notice(
                            NoticeLevel::Error,
                            "This input is too large for the edit buffer; copy or forget it",
                        );
                        return;
                    }
                    let id = input.id;
                    let text = input.entry.text().to_owned();
                    self.unconfirmed_editor.set_text(&text);
                    self.set_review_mode(ReviewMode::Edit(id));
                }
                KeyCode::Char('r') => {
                    if let Some(input) = self
                        .selected_local_input()
                        .filter(|input| input.status != Status::Sending)
                    {
                        self.set_review_mode(ReviewMode::Resend(input.id));
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(input) = self
                        .selected_local_input()
                        .filter(|input| input.status != Status::Sending)
                    {
                        self.set_review_mode(ReviewMode::Forget(input.id));
                    }
                }
                KeyCode::Char('c') => {
                    if let Some(input) = self.selected_local_input() {
                        self.copy(input.entry.text().to_owned());
                    }
                }
                _ => {}
            },
            ReviewMode::Edit(id) => {
                if key.code == KeyCode::Esc {
                    self.unconfirmed_editor = Composer::new();
                    self.set_review_mode(ReviewMode::List);
                    return;
                }
                if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.set_review_mode(ReviewMode::ResendEdited(id));
                    return;
                }
                let added = match key.code {
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => 1,
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        ch.len_utf8()
                    }
                    KeyCode::Enter => 1,
                    _ => 0,
                };
                if self.unconfirmed_editor.text().len()
                    - self
                        .unconfirmed_editor
                        .selection()
                        .map_or(0, |range| range.len())
                    + added
                    > EDIT_BYTES
                {
                    self.notice(
                        NoticeLevel::Error,
                        "The unconfirmed input edit buffer is full; its original is retained",
                    );
                    return;
                }
                edit_key(&mut self.unconfirmed_editor, key);
            }
            ReviewMode::Resend(id) | ReviewMode::ResendEdited(id) => match key.code {
                KeyCode::Char('y') if key.modifiers.is_empty() => {
                    self.resend_local_input(matches!(mode, ReviewMode::ResendEdited(_)))
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n') => {
                    self.set_review_mode(if matches!(mode, ReviewMode::ResendEdited(_)) {
                        ReviewMode::Edit(id)
                    } else {
                        ReviewMode::List
                    })
                }
                _ => {}
            },
            ReviewMode::Forget(id) => match key.code {
                KeyCode::Char('y') if key.modifiers.is_empty() => {
                    if let Some(index) = self.local_inputs.iter().position(|input| input.id == id) {
                        self.local_inputs.remove(index);
                    }
                    self.unconfirmed_editor = Composer::new();
                    self.overlay = Overlay::Unconfirmed {
                        selected: 0,
                        mode: ReviewMode::List,
                    };
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n') => {
                    self.set_review_mode(ReviewMode::List)
                }
                _ => {}
            },
        }
    }

    pub fn move_local_input(&mut self, delta: isize) {
        if let Overlay::Unconfirmed { selected, .. } = &mut self.overlay {
            *selected = super::picker::step(*selected, delta, self.local_inputs.len());
        }
    }
}

fn edit_key(editor: &mut Composer, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        let motion: Option<fn(&mut Composer)> = match key.code {
            KeyCode::Left if ctrl || alt => Some(Composer::move_word_left),
            KeyCode::Right if ctrl || alt => Some(Composer::move_word_right),
            KeyCode::Left => Some(Composer::move_left),
            KeyCode::Right => Some(Composer::move_right),
            KeyCode::Home => Some(Composer::move_home),
            KeyCode::End => Some(Composer::move_end),
            _ => None,
        };
        if let Some(motion) = motion {
            editor.extend_selection(motion);
            return;
        }
    }
    match key.code {
        KeyCode::Backspace if ctrl || alt => editor.delete_word_left(),
        KeyCode::Left if alt => editor.move_word_left(),
        KeyCode::Right if alt => editor.move_word_right(),
        _ => super::keys::edit_text(editor, key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicore_host_core::{Host, ResourceFuture, ResourceResolver};
    use minicore_host_protocol::{
        AgentConfig, AgentCreate, CallOptions, Fault, FaultKind, HostLifecycle,
        HostShutdownRequest, HostShutdownStrategy, OperationId, SessionConfig,
        SessionControlRequest, StorageMode,
    };
    use minicore_host_transport::HostClient;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    // These receipt/queue tests keep real Sessions paused and never execute a model.
    struct PausedOnlyResources;
    impl ResourceResolver for PausedOnlyResources {
        fn validate(&self, config: &SessionConfig) -> Result<(), Fault> {
            config
                .validate()
                .map_err(|error| Fault::new(FaultKind::SessionConfigInvalid, error.to_string()))
        }
        fn resolve<'a>(&'a self, _: &'a SessionConfig) -> ResourceFuture<'a> {
            Box::pin(async { panic!("paused receipt fixture must not execute") })
        }
    }

    struct Fixture {
        host: Arc<Host>,
        app: App,
        messages: mpsc::Receiver<Msg>,
    }

    impl Fixture {
        async fn new() -> Self {
            let host = Host::memory(Arc::new(PausedOnlyResources)).unwrap();
            let client = HostClient::in_process(Arc::clone(&host)).unwrap();
            let receipt = client
                .create_agent(
                    AgentCreate {
                        operation_id: OperationId::new().unwrap(),
                        name: "Paused receipt fixture".into(),
                        mode: StorageMode::Memory,
                        config: AgentConfig::default(),
                        default_session: None,
                        initial_message: None,
                    },
                    CallOptions::default(),
                )
                .await
                .unwrap();
            let address = minicore_host_protocol::SessionRef {
                agent_id: receipt.agent_id.unwrap(),
                session_id: receipt.session_id.unwrap(),
            };
            let session = client.session(address);
            session
                .pause(
                    SessionControlRequest {
                        operation_id: OperationId::new().unwrap(),
                    },
                    CallOptions::default(),
                )
                .await
                .unwrap();
            let info = session.info(CallOptions::default()).await.unwrap();
            let state = session.state(CallOptions::default()).await.unwrap();
            let (tx, messages) = mpsc::channel(super::super::tasks::MESSAGE_CAPACITY);
            let mut app = App::new(client, tx, std::env::temp_dir());
            app.resident
                .insert(super::super::Session::new(info, state, 1, session));
            app.resident.select(Some(1));
            app.epoch = 1;
            app.ready = true;
            Self {
                host,
                app,
                messages,
            }
        }
        fn submit(&mut self, text: &str) -> u64 {
            self.app.composer.set_text(text);
            let entry = self.app.composer.take_submission();
            assert!(
                self.app
                    .push_local_input(1, entry, MessageMode::FollowUp, None)
            );
            self.app.local_inputs.back().unwrap().id
        }
        async fn reply(&mut self) -> (u64, Result<MessageAccepted, CallError>) {
            crate::testing::real_bounded(async {
                loop {
                    if let Msg::LocalPushed(id, result) = self.messages.recv().await.unwrap() {
                        return (id, result);
                    }
                }
            })
            .await
        }
        async fn finish(self) {
            let control = self.host.process_control();
            control
                .shutdown(HostShutdownRequest {
                    operation_id: OperationId::new().unwrap(),
                    strategy: HostShutdownStrategy::Force,
                })
                .unwrap();
            crate::testing::real_bounded(async {
                let mut state = control.snapshot().unwrap();
                while state.lifecycle != HostLifecycle::Stopped {
                    state = control.changed(&state).await.unwrap();
                }
            })
            .await;
        }
    }

    #[tokio::test]
    async fn accepted_input_keeps_exact_text_and_history_even_with_a_start_error() {
        let mut fixture = Fixture::new().await;
        let original = fixture.app.resident.current().unwrap().address();
        fixture.submit("  exact draft\n\n");
        let (id, result) = fixture.reply().await;
        let mut receipt = result.unwrap();
        let actual = fixture
            .app
            .resident
            .current()
            .unwrap()
            .client
            .inputs(CallOptions::default())
            .await
            .unwrap();
        assert_eq!(actual.next_turn[0].input.text, "  exact draft\n\n");
        // A valid accepted receipt may carry an independent startup failure.
        receipt.result.start_error = Some(Fault::new(
            FaultKind::SessionResourceMissing,
            "fixture startup failure",
        ));
        let other = super::super::tasks::create_session(
            &fixture.app.client,
            original.agent_id.clone(),
            Default::default(),
        )
        .await
        .unwrap();
        let opened = super::super::tasks::open(
            fixture.app.client.clone(),
            other.clone(),
            Default::default(),
        )
        .await
        .unwrap();
        fixture.app.adopt(*opened);
        fixture.app.on_msg(Msg::LocalPushed(id, Ok(receipt)));
        assert_eq!(fixture.app.resident.current().unwrap().address(), other);
        assert!(
            fixture
                .app
                .last_error
                .as_ref()
                .unwrap()
                .contains(original.session_id.as_str())
        );
        assert!(
            !fixture
                .app
                .last_error
                .as_ref()
                .unwrap()
                .contains(other.session_id.as_str())
        );
        assert!(fixture.app.local_inputs.is_empty());
        assert_eq!(
            fixture.app.composer.history_records()[0].text,
            "  exact draft\n\n"
        );
        assert!(
            fixture
                .app
                .notices
                .back()
                .unwrap()
                .text
                .contains("was accepted")
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn lost_confirmation_retains_original_until_explicit_new_id_resend() {
        let mut fixture = Fixture::new().await;
        let original = fixture.submit("may already be accepted");
        let (_, receipt) = fixture.reply().await;
        assert!(receipt.is_ok(), "the real Host already accepted this input");
        let original_message = fixture.app.local_inputs[0].message_id.clone();
        fixture
            .app
            .on_local_pushed(original, Err(CallError::WaitTimeout { wait_timeout_ms: 1 }));
        assert!(fixture.app.composer.history_records().is_empty());
        assert!(fixture.app.composer.is_empty());
        fixture.app.open_unconfirmed();
        fixture
            .app
            .on_unconfirmed_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(
            fixture.app.overlay,
            Overlay::Unconfirmed {
                mode: ReviewMode::Resend(_),
                ..
            }
        ));
        fixture
            .app
            .on_unconfirmed_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(fixture.app.local_inputs.len(), 1);
        fixture
            .app
            .on_unconfirmed_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        fixture
            .app
            .on_unconfirmed_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(fixture.app.local_inputs.len(), 2);
        assert_eq!(fixture.app.local_inputs[0].status, Status::Unconfirmed);
        assert_ne!(fixture.app.local_inputs[1].message_id, original_message);
        assert_eq!(
            fixture.app.local_inputs[1].target,
            fixture.app.local_inputs[0].target
        );
        let (retry, result) = fixture.reply().await;
        fixture.app.on_local_pushed(retry, result);
        assert_eq!(fixture.app.local_inputs.len(), 1);
        assert_eq!(fixture.app.local_inputs[0].id, original);
        assert_eq!(fixture.app.composer.history_records().len(), 1);
        let actual = fixture
            .app
            .resident
            .current()
            .unwrap()
            .client
            .inputs(CallOptions::default())
            .await
            .unwrap();
        assert_eq!(actual.next_turn.len(), 2);
        assert_ne!(
            actual.next_turn[0].input.message_id,
            actual.next_turn[1].input.message_id
        );
        fixture.finish().await;
    }

    #[tokio::test]
    async fn business_rejection_restores_the_draft_but_never_overwrites_newer_text() {
        let mut fixture = Fixture::new().await;
        fixture.app.client.close();
        fixture.submit("restore this draft");
        let (id, result) = fixture.reply().await;
        assert!(matches!(result, Err(CallError::Business(_))));
        fixture.app.on_local_pushed(id, result);
        assert_eq!(fixture.app.composer.text(), "restore this draft");
        assert!(fixture.app.local_inputs.is_empty());
        fixture.submit("retain this rejected draft");
        fixture.app.composer.set_text("newer draft");
        let (id, result) = fixture.reply().await;
        fixture.app.on_local_pushed(id, result);
        assert_eq!(fixture.app.composer.text(), "newer draft");
        assert_eq!(fixture.app.local_inputs[0].status, Status::Rejected);
        assert_eq!(
            fixture.app.local_inputs[0].entry.text(),
            "retain this rejected draft"
        );
        assert!(fixture.app.composer.history_records().is_empty());
        fixture.finish().await;
    }

    #[tokio::test]
    async fn protocol_failure_never_becomes_a_rejected_or_automatically_retried_input() {
        let mut fixture = Fixture::new().await;
        let id = fixture.submit("retain uncertain input");
        let _accepted = fixture.reply().await;
        fixture.app.on_local_pushed(
            id,
            Err(CallError::Protocol {
                message: "explicit fixture protocol failure".into(),
            }),
        );
        assert_eq!(fixture.app.local_inputs[0].status, Status::Unconfirmed);
        assert_eq!(
            fixture.app.local_inputs[0].entry.text(),
            "retain uncertain input"
        );
        assert!(fixture.app.composer.history_records().is_empty());
        let actual = fixture
            .app
            .resident
            .current()
            .unwrap()
            .client
            .inputs(CallOptions::default())
            .await
            .unwrap();
        assert_eq!(actual.next_turn.len(), 1);
        fixture.finish().await;
    }

    #[tokio::test]
    async fn a_receipt_for_another_incarnation_cannot_confirm_the_original_input() {
        let mut fixture = Fixture::new().await;
        let id = fixture.submit("keep original target");
        let (_, result) = fixture.reply().await;
        let mut receipt = result.unwrap();
        receipt.host_incarnation = minicore_host_protocol::HostIncarnation::new().unwrap();
        fixture.app.on_local_pushed(id, Ok(receipt));
        assert_eq!(fixture.app.local_inputs[0].status, Status::Unconfirmed);
        assert_eq!(
            fixture.app.local_inputs[0].entry.text(),
            "keep original target"
        );
        assert!(fixture.app.composer.history_records().is_empty());
        fixture.finish().await;
    }
}
