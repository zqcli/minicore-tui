//! Key routing: each surface (approval prompt, overlay, completion list,
//! composer) gets the keys it owns.

mod completion;
mod composer;
mod overlay;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Action, Answer, App, InteractionKind};

impl App {
    /// Routes a key event to the focused surface; the action it asks for, if any.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notices.clear();
        self.selection = None;
        self.composer_dragging = false;
        if self.fatal.is_some() {
            return matches!(key.code, KeyCode::Char('q') | KeyCode::Esc).then_some(Action::Quit);
        }
        if self.remote.active()
            && key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.composer.selection().is_none()
        {
            return Some(Action::Quit);
        }
        if !self.overlay.is_none() {
            return self.on_overlay_key(key);
        }
        if self.has_interaction() && !self.remote.active() {
            return self.on_interaction_key(key);
        }
        self.on_composer_input(key)
    }

    /// Approval prompts own the keyboard until answered; other keys are
    /// swallowed rather than reaching the draft behind the prompt.
    fn on_interaction_key(&mut self, key: KeyEvent) -> Option<Action> {
        let interaction = self.session.as_mut()?.interaction.as_mut()?;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let answer = match key.code {
            KeyCode::Char('y') if !ctrl => Answer::Allow,
            KeyCode::Char('a')
                if !ctrl && matches!(interaction.kind, InteractionKind::Approval) =>
            {
                Answer::AllowTool
            }
            KeyCode::Char('n') if !ctrl => Answer::Deny,
            KeyCode::Esc => Answer::Deny,
            KeyCode::Enter => Answer::Choice(interaction.selected),
            KeyCode::Up => {
                interaction.selected = interaction.selected.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                let last = interaction.options().len() - 1;
                interaction.selected = (interaction.selected + 1).min(last);
                return None;
            }
            _ => return None,
        };
        Some(Action::Answer(answer))
    }
}
