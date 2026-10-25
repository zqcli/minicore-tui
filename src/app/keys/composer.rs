//! Keys for the composer: editing, sending, scrolling and the shortcuts that
//! do not need an overlay.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::commands::{self, Executed};
use crate::app::composer::Composer;
use crate::app::{Action, App, NoticeLevel, Panel, Scroll};
use crate::protocol::Reasoning;
use crate::transcript::Block;

/// A second Ctrl+C within this long of the first quits.
const QUIT_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

impl App {
    /// A composer key, then the completion list brought in step with the draft.
    pub(super) fn on_composer_input(&mut self, key: KeyEvent) -> Option<Action> {
        let before = self.composer.version();
        if key.code != KeyCode::Enter {
            self.armed_unknown = None;
        }
        let action = self.on_composer_key(key);
        let edited = self.composer.version() != before;
        let typed = edited
            && matches!(
                key.code,
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
            );
        self.sync_completion(edited, typed);
        action
    }

    /// Copies the draft's selection and drops it, so Ctrl+C can quit again; a
    /// cut removes the text only when the clipboard will take it.
    fn copy_draft_selection(&mut self, cut: bool) {
        let Some(text) = self.composer.selected_text().map(str::to_owned) else {
            return;
        };
        if cut {
            if let Err(error) = crate::clipboard::check(&text) {
                self.notice(NoticeLevel::Error, error);
                return;
            }
            self.composer.delete_selection();
        } else {
            self.composer.clear_selection();
        }
        self.copy(text);
    }

    fn on_composer_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // With no draft, Shift+Home and Shift+End keep scrolling like Home and End.
        if shift && !self.composer.is_empty() {
            let word = ctrl || alt;
            let motion: Option<fn(&mut Composer)> = match key.code {
                KeyCode::Left if word => Some(Composer::move_word_left),
                KeyCode::Left => Some(Composer::move_left),
                KeyCode::Right if word => Some(Composer::move_word_right),
                KeyCode::Right => Some(Composer::move_right),
                KeyCode::Home => Some(Composer::move_home),
                KeyCode::End => Some(Composer::move_end),
                KeyCode::Up if !word => Some(|c| {
                    c.move_up();
                }),
                KeyCode::Down if !word => Some(|c| {
                    c.move_down();
                }),
                _ => None,
            };
            if let Some(motion) = motion {
                self.composer.extend_selection(motion);
                return None;
            }
        }
        if !matches!(key.code, KeyCode::Char('c') if ctrl) {
            self.quit_armed = None;
        }
        match key.code {
            KeyCode::Enter if alt => return Some(Action::QueueFollowUp),
            KeyCode::Enter if shift => self.composer.newline(),
            KeyCode::Enter => return self.submit_or_command(),
            KeyCode::Char('j') if ctrl => self.composer.newline(),
            KeyCode::Esc if self.composer.selection().is_some() => self.composer.clear_selection(),
            KeyCode::Esc => self.escape(),
            KeyCode::Char('c') if ctrl && self.composer.selection().is_some() => {
                self.copy_draft_selection(false);
                self.quit_armed = None;
            }
            KeyCode::Char('c') if ctrl && self.remote.active() => return Some(Action::Quit),
            KeyCode::Char('c') if ctrl => {
                let now = tokio::time::Instant::now();
                if self.quit_armed.is_some_and(|at| now - at <= QUIT_WINDOW) {
                    return Some(Action::Quit);
                }
                self.quit_armed = Some(now);
                if self.composer.is_empty() {
                    self.notice(NoticeLevel::Info, "Press Ctrl+C again to quit");
                } else {
                    self.composer.clear();
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    return Some(Action::Quit);
                }
                self.composer.delete();
            }
            KeyCode::Char('g') if ctrl => return Some(Action::EditDraft),
            KeyCode::Char('x') if ctrl && self.composer.selection().is_some() => {
                self.copy_draft_selection(true);
            }
            KeyCode::Char('x') if ctrl => return Some(Action::CopyLastAnswer),
            KeyCode::Char('o') if ctrl => {
                self.pin_view();
                self.tools_expanded = !self.tools_expanded;
                let state = if self.tools_expanded {
                    "expanded"
                } else {
                    "collapsed"
                };
                // A flash leaves the dock alone, so the pinned view does not move.
                self.flash = Some((format!("Tool output: {state}"), tokio::time::Instant::now()));
            }
            KeyCode::Char('t') if ctrl => {
                self.pin_view();
                self.show_reasoning = !self.show_reasoning;
                let state = if self.show_reasoning {
                    "visible"
                } else {
                    "hidden"
                };
                self.flash = Some((
                    format!("Thinking blocks: {state}"),
                    tokio::time::Instant::now(),
                ));
            }
            KeyCode::Char('r') if ctrl => self.open_panel(Panel::Sessions),
            KeyCode::Char('l') if ctrl => self.open_panel(Panel::Models),
            KeyCode::Char('p' | 'P') if ctrl => {
                let backward = shift || key.code == KeyCode::Char('P');
                return Some(Action::CycleModel { forward: !backward });
            }
            KeyCode::Char('f') if ctrl => self.open_panel(Panel::Find),
            KeyCode::Char('a') if ctrl => self.composer.move_home(),
            KeyCode::Char('e') if ctrl => self.composer.move_end(),
            KeyCode::Char('u') if ctrl => self.composer.delete_to_line_start(),
            KeyCode::Char('k') if ctrl => self.composer.delete_to_line_end(),
            // Many terminals send Ctrl+Backspace as ^H.
            KeyCode::Char('w' | 'h') if ctrl => self.composer.delete_word_left(),
            KeyCode::Char('z') if ctrl => {
                self.composer.undo();
            }
            KeyCode::Char('y') if ctrl => {
                self.composer.redo();
            }
            KeyCode::BackTab => return self.next_reasoning().map(Action::SetReasoning),
            // Mid-word `@` is text, as in an address.
            KeyCode::Char('@') if self.word_start() == self.composer.cursor() => {
                self.composer.type_char('@');
                self.open_panel(Panel::Files);
            }
            KeyCode::Char('d') if alt => self.composer.delete_word_right(),
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char(character) => self.composer.type_char(character),
            KeyCode::Backspace if ctrl || alt => self.composer.delete_word_left(),
            KeyCode::Backspace => self.composer.backspace(),
            KeyCode::Delete if alt => self.composer.delete_word_right(),
            KeyCode::Delete => self.composer.delete(),
            KeyCode::Left if ctrl || alt => self.composer.move_word_left(),
            KeyCode::Right if ctrl || alt => self.composer.move_word_right(),
            KeyCode::Left => self.composer.move_left(),
            KeyCode::Right => self.composer.move_right(),
            // Arrows move inside a multi-line draft and only browse history at
            // its first and last line.
            KeyCode::Up if alt => return Some(Action::RecallQueued),
            KeyCode::Up if ctrl => self.jump_to_user_message(false),
            KeyCode::Down if ctrl => self.jump_to_user_message(true),
            KeyCode::Up => {
                if !self.composer.move_up() {
                    self.composer.history_prev();
                }
            }
            KeyCode::Down => {
                if !self.composer.move_down() {
                    self.composer.history_next();
                }
            }
            // Home and End move in a draft; with no draft Home and always End
            // also scroll, so the jump label's `End` works mid-draft too.
            KeyCode::Home if ctrl || self.composer.is_empty() => {
                self.scroll = Scroll::Anchor {
                    block: 0,
                    offset: 0,
                };
                // The top of the loaded window is also where the next page begins.
                self.load_older();
            }
            KeyCode::End => {
                self.composer.move_end();
                self.scroll = Scroll::Follow;
            }
            KeyCode::Home => self.composer.move_home(),
            // Four rows of the previous page stay in view.
            KeyCode::PageUp => self.scroll_by(-self.page_rows()),
            KeyCode::PageDown => self.scroll_by(self.page_rows()),
            KeyCode::F(1) => self.open_panel(Panel::Help),
            _ => {}
        }
        None
    }

    /// A terminal paste. The composer decides whether the payload becomes a
    /// single token; a pasted `/name` opens the command list like typing does.
    pub fn on_paste(&mut self, text: &str) {
        if (self.has_interaction() && !self.remote.active())
            || self.fatal.is_some()
            || !matches!(
                self.overlay,
                crate::app::Overlay::None
                    | crate::app::Overlay::Palette { .. }
                    | crate::app::Overlay::Files { .. }
            )
        {
            return;
        }
        self.composer.insert(text);
        self.sync_completion(true, true);
    }

    /// A draft is a command only when its first word names one; anything else
    /// starting with `/` (a path, say) is a prompt.
    fn submit_or_command(&mut self) -> Option<Action> {
        let text = self.composer.text().to_owned();
        let executed =
            commands::parse(&text).and_then(|(name, argument)| commands::execute(name, argument));
        match executed {
            Some(Executed::Action(action)) => {
                if !self.local_allowed(&action) {
                    self.remote_flash();
                    return None;
                }
                self.composer.clear();
                Some(action)
            }
            Some(Executed::Panel(panel)) => {
                if !self.local_panel_allowed(&panel) {
                    self.remote_flash();
                    return None;
                }
                self.composer.clear();
                self.open_panel(panel);
                None
            }
            Some(Executed::Noop) => {
                let name = commands::parse(self.composer.text()).map_or("", |(name, _)| name);
                self.notice(NoticeLevel::Error, commands::needs_argument(name));
                None
            }
            None => self.submit_line(&text),
        }
    }

    /// A draft no built-in command claims: a template, a typo that Enter warns
    /// about once, or a prompt. `//` escapes a leading slash.
    fn submit_line(&mut self, text: &str) -> Option<Action> {
        if self.remote.active() {
            return Some(Action::Submit);
        }
        if let Some(escaped) = text.strip_prefix('/').filter(|rest| rest.starts_with('/')) {
            self.composer.set_text(escaped);
        } else if let Some(expanded) =
            commands::parse(text).and_then(|(name, argument)| self.expand_template(name, argument))
        {
            self.composer.set_text(&expanded);
        } else if let Some(name) = commands::bare_word(text)
            && self.armed_unknown.as_deref() != Some(text.trim())
        {
            let known = commands::COMMANDS
                .iter()
                .map(|command| command.name)
                .chain(self.templates.iter().map(|template| template.name.as_str()));
            let notice = commands::unknown_notice(name, commands::closest(name, known));
            self.armed_unknown = Some(text.trim().to_owned());
            self.notice(NoticeLevel::Error, notice);
            return None;
        }
        Some(Action::Submit)
    }

    fn page_rows(&self) -> isize {
        self.viewport_rows().saturating_sub(4).max(1) as isize
    }

    /// Ctrl+↑/↓: puts the previous or next user message at the top. Past the
    /// last one the view follows the tail again.
    fn jump_to_user_message(&mut self, forward: bool) {
        let blocks = self.transcript_blocks();
        let layout = &self.layout;
        let (top, offset) = match self.scroll {
            Scroll::Anchor { block, offset } => (block, offset),
            Scroll::Follow if forward => return,
            Scroll::Follow => {
                let max_top = layout.total_rows().saturating_sub(layout.viewport.height);
                layout.anchor(max_top)
            }
        };
        let is_user = |block: &Block| matches!(block, Block::User { .. });
        let target = if forward {
            blocks
                .iter()
                .enumerate()
                .skip(top + 1)
                .find(|(_, block)| is_user(block))
                .map(|(index, _)| index)
        } else {
            let end = if offset > 0 { top + 1 } else { top };
            blocks[..end.min(blocks.len())].iter().rposition(is_user)
        };
        self.scroll = match target {
            Some(block) => Scroll::Anchor { block, offset: 0 },
            None if forward => Scroll::Follow,
            None => self.scroll,
        };
    }

    /// Shift+Tab: the next supported reasoning level.
    fn next_reasoning(&self) -> Option<Reasoning> {
        let current = self.reasoning()?;
        let levels = self.reasoning_levels();
        let next = levels
            .iter()
            .position(|level| *level == current)
            .map_or(0, |at| (at + 1) % levels.len());
        levels.get(next).copied()
    }
}
