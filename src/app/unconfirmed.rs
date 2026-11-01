//! Local input acceptance ownership and explicit decisions about uncertain sends.

use super::{
    App, Msg, NoticeLevel, Overlay,
    composer::{Composer, Entry},
    resident::Target,
};
use crate::protocol::{self, Acceptance, InputMode, InputPushResult, RpcError};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

pub const MAX_OPERATIONS: usize = 64;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const EDIT_BYTES: usize = crate::rpc::MAX_REQUEST_BYTES;

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
    pub mode: InputMode,
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
            + self.first_submission.as_ref().map_or(0, |entry| {
                entry.retained_bytes() + self.first_message.as_ref().map_or(0, String::capacity)
            })
    }

    pub(super) fn local_input_room(&self, bytes: usize) -> bool {
        self.local_inputs.len() + usize::from(self.first_submission.is_some()) < MAX_OPERATIONS
            && bytes <= MAX_BYTES - self.local_input_bytes()
    }

    pub(super) fn push_local_input(
        &mut self,
        epoch: u64,
        mut entry: Entry,
        mode: InputMode,
        retry_of: Option<u64>,
    ) {
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
            return;
        }
        let session = self
            .resident
            .get(epoch)
            .expect("local input has a resident session");
        let target = session.target();
        let id = self.next_operation();
        let params = json!({"session_id":target.session_id,"load":target.load,"mode":mode,"text":entry.text().trim_end()});
        self.local_inputs.push_back(LocalInput {
            id,
            target,
            mode,
            status: Status::Sending,
            retry_of,
            entry,
            draft_version: self.composer.version(),
        });
        #[cfg(feature = "e2e-observe")]
        self.observe_checkpoint(false);
        let client = self.client.clone();
        self.spawn_msg(async move {
            Msg::LocalPushed(id, client.call(protocol::METHOD_INPUT_PUSH, &params).await)
        });
    }

    pub(super) fn on_local_pushed(&mut self, id: u64, result: Result<InputPushResult, RpcError>) {
        let Some(index) = self.local_inputs.iter().position(|input| input.id == id) else {
            return;
        };
        if self.local_inputs[index].status != Status::Sending {
            return;
        }
        let target = self.local_inputs[index].target.clone();
        let result = result.and_then(|receipt| {
            super::tasks::accepted_input(receipt, &target.session_id, &target.load)
        });
        match result {
            Ok(receipt) => {
                let input = self.local_inputs.remove(index).unwrap();
                if self.composer.remember(input.entry)
                    && let Some(home) = &self.home
                {
                    self.local_files.history(
                        &self.tx,
                        home.history_file(),
                        self.composer.history_records(),
                    );
                }
                if let Some(error) = receipt.start_error {
                    self.notice(
                        NoticeLevel::Error,
                        format!("Input tui-{id} was accepted; {}", error.message),
                    );
                }
                if self
                    .resident
                    .get(target.epoch)
                    .is_some_and(|session| session.load == target.load)
                {
                    self.refresh_control_for(target.epoch);
                }
            }
            Err(error) if error.acceptance == Some(Acceptance::Rejected) => {
                let input = &mut self.local_inputs[index];
                input.status = Status::Rejected;
                let restore = input.retry_of.is_none()
                    && self.composer.is_empty()
                    && self.composer.version() == input.draft_version
                    && !self.remote.active()
                    && self.resident.current().is_some_and(|session| {
                        session.epoch == target.epoch && session.load == target.load
                    });
                if restore {
                    let input = self.local_inputs.remove(index).unwrap();
                    self.composer.restore_submission(input.entry);
                }
                self.notice(NoticeLevel::Error, error.message);
            }
            Err(error) => {
                let input = &mut self.local_inputs[index];
                input.status = Status::Unconfirmed;
                input.entry.discard_rollback();
                self.notice(NoticeLevel::Error,format!("Input tui-{id} for {} is unconfirmed and may already be accepted; F8 to review: {}",target.session_id,error.message));
            }
        }
        #[cfg(feature = "e2e-observe")]
        self.observe_checkpoint(false);
    }

    pub(super) fn open_unconfirmed(&mut self) {
        self.unconfirmed_editor = Composer::new();
        self.overlay = Overlay::Unconfirmed {
            selected: 0,
            mode: ReviewMode::List,
        };
        #[cfg(feature = "e2e-observe")]
        self.observe_checkpoint(false);
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
        #[cfg(feature = "e2e-observe")]
        self.observe_checkpoint(false);
    }

    fn resend_local_input(&mut self, edited: bool) {
        if self.remote.active() {
            self.remote_flash();
            return;
        }
        let Some(input) = self.selected_local_input() else {
            return;
        };
        let target = input.target.clone();
        let mode = input.mode;
        let retry_of = input.id;
        if self.fatal.is_some()
            || !self
                .live(&target.session_id)
                .is_some_and(|session| session.load == target.load && session.epoch == target.epoch)
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
        self.push_local_input(target.epoch, entry, mode, Some(retry_of));
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
                    #[cfg(feature = "e2e-observe")]
                    self.observe_checkpoint(false);
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
