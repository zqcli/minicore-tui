//! The inline completion list under the composer: the `/command` palette and
//! the `@path` mentions.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::commands::{self, Executed};
use crate::app::overlay::matching_paths;
use crate::app::{Action, App, Overlay};
use crate::transcript::NoticeLevel;

impl App {
    /// Keeps the inline completion list in step with the draft: an edit resets
    /// the selection to the best match and a draft it no longer fits closes it.
    /// Typing in the `/name` of a one-line draft opens the command list.
    pub(super) fn sync_completion(&mut self, edited: bool, typed: bool) {
        let fits = match self.overlay {
            Overlay::Palette { .. } => self.palette_query().is_some(),
            Overlay::Files { .. } => self.mention_query().is_some(),
            Overlay::None => {
                if typed && self.palette_query().is_some() && self.word_start() == 0 {
                    self.overlay = Overlay::Palette { selected: 0 };
                }
                return;
            }
            _ => return,
        };
        match &mut self.overlay {
            _ if !fits => self.overlay = Overlay::None,
            Overlay::Palette { selected } | Overlay::Files { selected, .. } if edited => {
                *selected = 0;
            }
            _ => {}
        }
    }

    /// Rows the completion list offers for the current draft.
    pub(super) fn completion_len(&self) -> usize {
        match &self.overlay {
            Overlay::Palette { .. } => self.palette_rows().len(),
            Overlay::Files { paths, .. } => self
                .mention_query()
                .map_or(0, |query| matching_paths(paths, query).len()),
            _ => 0,
        }
    }

    /// Keys while the completion list is open. It takes the arrows, Tab, Enter
    /// and Esc while it has rows; every other key edits the draft that filters it.
    pub(super) fn on_completion_key(&mut self, key: KeyEvent) -> Option<Action> {
        let rows = self.completion_len();
        let plain = key.modifiers.is_empty();
        match key.code {
            // The draft stays: it may be a path rather than a command.
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up if plain && rows > 0 => self.overlay_move(-1),
            KeyCode::Down if plain && rows > 0 => self.overlay_move(1),
            KeyCode::Tab if plain && rows > 0 => self.apply_completion(),
            KeyCode::Enter if plain && rows > 0 => return self.accept_completion(),
            // An `@` with no match never sends the draft by accident.
            KeyCode::Enter if plain && matches!(self.overlay, Overlay::Files { .. }) => {}
            // No command matches: the line is a prompt.
            KeyCode::Enter if plain => {
                self.overlay = Overlay::None;
                return self.on_composer_input(key);
            }
            _ => return self.on_composer_input(key),
        }
        None
    }

    /// The selected command or path, if the list has one.
    fn selected_completion(&self) -> Option<String> {
        match &self.overlay {
            Overlay::Palette { selected } => self
                .palette_rows()
                .get(*selected)
                .map(|(name, _)| name.clone()),
            Overlay::Files { selected, paths } => matching_paths(paths, self.mention_query()?)
                .get(*selected)
                .map(|path| (*path).clone()),
            _ => None,
        }
    }

    /// Tab: puts the selected completion into the draft and closes the list.
    /// A command keeps the argument typed after it, an argument value replaces
    /// the typed one; a path ends the `@` token.
    fn apply_completion(&mut self) {
        let Some(choice) = self.selected_completion() else {
            return;
        };
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Palette { .. } => {
                let text = self.composer.text();
                if let Some((command, _)) = self.palette_arg() {
                    let completed = format!("/{command} {choice}");
                    self.composer.restore_text(&completed);
                    return;
                }
                let rest = &text[text.find(char::is_whitespace).unwrap_or(text.len())..];
                let rest = if rest.trim().is_empty() { " " } else { rest };
                self.composer.restore_text(&format!("/{choice}{rest}"));
            }
            _ => {
                let start = self.word_start();
                while self.composer.cursor() > start {
                    self.composer.backspace();
                }
                self.composer.insert(&format!("@{choice} "));
            }
        }
    }

    /// Enter: a command runs with the argument typed after its name, which
    /// may belong to another fuzzy match than the typed name; a path is inserted.
    fn accept_completion(&mut self) -> Option<Action> {
        if !matches!(self.overlay, Overlay::Palette { .. }) {
            self.apply_completion();
            return None;
        }
        let choice = self.selected_completion()?;
        // In an argument list the command is already typed and the row is its argument.
        let (name, arg) = match self.palette_arg() {
            Some((command, _)) => (command.to_owned(), choice),
            None => {
                let typed = commands::parse(self.composer.text()).map_or("", |(_, arg)| arg);
                (choice, typed.to_owned())
            }
        };
        match commands::execute(&name, &arg) {
            Some(Executed::Action(action)) => {
                self.overlay = Overlay::None;
                self.composer.clear();
                Some(action)
            }
            Some(Executed::Panel(panel)) => {
                self.composer.clear();
                self.open_panel(panel);
                None
            }
            // The completed name stays in the draft for the argument it needs.
            _ => {
                if let Some(text) = self.expand_template(&name, &arg) {
                    self.overlay = Overlay::None;
                    self.composer.set_text(&text);
                    return Some(Action::Submit);
                }
                self.apply_completion();
                self.notice(NoticeLevel::Error, commands::needs_argument(&name));
                None
            }
        }
    }
}
