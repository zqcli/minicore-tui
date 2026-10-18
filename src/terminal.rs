//! Terminal entry, restoration, suspension and the panic hook.

use std::io::{self, Stdout, Write};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Pushes the keyboard-enhancement flags so Shift+Enter and Alt+Enter carry
/// distinct codes. Terminals without support simply report failure and the
/// keys fall back to their plain forms.
fn push_keyboard_flags(writer: &mut impl Write) -> io::Result<()> {
    execute!(
        writer,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

fn enter_screen(writer: &mut impl Write) -> io::Result<()> {
    execute!(
        writer,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        cursor::Hide
    )
}

fn leave_screen(writer: &mut impl Write) -> io::Result<()> {
    execute!(
        writer,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    )
}

/// Owns the alternate screen. Restores it exactly once, and can hand it to a
/// child process (the external editor) and take it back.
pub struct Guard {
    terminal: Term,
    active: bool,
    enhanced_keys: bool,
}

impl Guard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        let enhanced_keys = push_keyboard_flags(&mut stdout).is_ok();
        let screen = enter_screen(&mut stdout);
        let terminal = screen.and_then(|()| {
            Terminal::new(CrosstermBackend::new(io::stdout())).inspect_err(|_| {
                let mut stdout = io::stdout();
                let _ = leave_screen(&mut stdout);
                let _ = disable_raw_mode();
            })
        });
        Ok(Self {
            terminal: terminal?,
            active: true,
            enhanced_keys,
        })
    }

    pub fn terminal(&mut self) -> &mut Term {
        &mut self.terminal
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Hands the real terminal to a child process: an editor needs the normal
    /// screen and cooked input. Drawing and input pause until [`Self::resume`].
    pub fn suspend(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.restore()
    }

    /// Takes the alternate screen back after [`Self::suspend`].
    pub fn resume(&mut self) -> io::Result<()> {
        if self.active {
            return Ok(());
        }
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        self.enhanced_keys = push_keyboard_flags(&mut stdout).is_ok();
        enter_screen(&mut stdout)?;
        self.active = true;
        self.terminal.clear()
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let mut stdout = io::stdout();
        if self.enhanced_keys {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        leave_screen(&mut stdout)?;
        stdout.flush()?;
        disable_raw_mode()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Installs a hook that restores the terminal before the previous hook runs, so
/// a panic never leaves the shell in raw mode.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            cursor::Show
        );
        previous(info);
    }));
}
