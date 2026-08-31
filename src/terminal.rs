//! Terminal lifecycle: alternate-screen entry, guarded restoration, raw mode,
//! and the panic hook that best-effort restores the terminal before the
//! previously installed hook runs.

use std::io::{self, Stdout};
use std::panic::{self, PanicHookInfo};
use std::sync::Arc;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::error::TerminalError;

/// Owns the fullscreen terminal and restores it on drop.
///
/// Entry enables raw mode, the alternate screen, bracketed paste and mouse
/// capture, then hides the cursor and clears the screen. `restore` runs the
/// full reverse sequence and only marks the terminal restored after every
/// step succeeded, so a failed restore is retried in full by `Drop`; a
/// completed restore is a no-op on later calls. The main flow calls `restore`
/// explicitly, and `Drop` performs a best-effort fallback that never panics.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    restored: RestoreLatch,
}

impl TerminalGuard {
    pub fn enter() -> Result<Self, TerminalError> {
        enable_raw_mode().map_err(|err| TerminalError::new("enable raw mode", err))?;
        let mut screen = ScreenState::default();
        let mut terminal = match screen
            .apply(&mut io::stdout())
            .and_then(|()| Self::create_terminal())
        {
            Ok(terminal) => terminal,
            Err(error) => {
                screen.rollback(&mut io::stdout());
                let _ = disable_raw_mode();
                return Err(error);
            }
        };
        if let Err(err) = terminal.clear() {
            screen.rollback(&mut io::stdout());
            let _ = disable_raw_mode();
            return Err(TerminalError::new("clear terminal", err));
        }
        Ok(Self {
            terminal,
            restored: RestoreLatch::default(),
        })
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    /// Restores the terminal in the exact reverse order of entry. Every step
    /// is attempted on every call; the first failure is reported and the
    /// guard stays retryable until a call fully succeeds.
    pub fn restore(&mut self) -> Result<(), TerminalError> {
        self.restored.attempt(|| attempt_restore(&mut io::stdout()))
    }

    fn create_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, TerminalError> {
        Terminal::new(CrosstermBackend::new(io::stdout()))
            .map_err(|err| TerminalError::new("create terminal", err))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Retry latch for restore: only a fully successful attempt marks the
/// terminal as restored, so a failed restore can be retried in full while a
/// completed one stays a no-op.
#[derive(Debug, Default)]
struct RestoreLatch {
    done: bool,
}

impl RestoreLatch {
    fn attempt(
        &mut self,
        restore: impl FnOnce() -> Result<(), TerminalError>,
    ) -> Result<(), TerminalError> {
        if self.done {
            return Ok(());
        }
        let result = restore();
        if result.is_ok() {
            self.done = true;
        }
        result
    }
}

/// Runs the four writer-based restore commands in their fixed reverse order:
/// show cursor, disable mouse capture, disable bracketed paste, leave
/// alternate screen. Every step is attempted; the first failure is returned.
/// Raw mode is not part of this sequence because it is a terminal attribute
/// rather than a writer command.
fn restore_sequence<W: io::Write>(writer: &mut W) -> Result<(), TerminalError> {
    let mut error = None;
    if let Err(err) = execute!(writer, Show) {
        error.get_or_insert_with(|| TerminalError::new("show cursor", err));
    }
    if let Err(err) = execute!(writer, DisableMouseCapture) {
        error.get_or_insert_with(|| TerminalError::new("disable mouse capture", err));
    }
    if let Err(err) = execute!(writer, DisableBracketedPaste) {
        error.get_or_insert_with(|| TerminalError::new("disable bracketed paste", err));
    }
    if let Err(err) = execute!(writer, LeaveAlternateScreen) {
        error.get_or_insert_with(|| TerminalError::new("leave alternate screen", err));
    }
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The full reverse restore: the writer commands followed by raw mode, with
/// every step attempted on every call.
fn attempt_restore<W: io::Write>(writer: &mut W) -> Result<(), TerminalError> {
    let mut error = restore_sequence(writer).err();
    if let Err(err) = disable_raw_mode() {
        error.get_or_insert_with(|| TerminalError::new("disable raw mode", err));
    }
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Best-effort terminal restore for the panic hook. Safe to run before the
/// terminal was ever entered (every command is an idempotent mode toggle that
/// no-ops on an untouched terminal), and safe to run alongside a later
/// `TerminalGuard` drop, which repeats the same commands.
fn best_effort_restore_terminal() {
    let _ = restore_sequence(&mut io::stdout());
    let _ = disable_raw_mode();
}

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// Swaps in a panic hook that best-effort restores the terminal before
/// delegating to the previously installed hook, and reinstates that hook when
/// dropped. The hook never swallows the panic. It is installed before
/// `TerminalGuard::enter`, so a panic at any point after entry still leaves
/// the terminal usable; panics before entry simply run the idempotent restore
/// against an untouched terminal. A single guard is expected per process.
pub struct PanicHookGuard {
    previous: Arc<PanicHook>,
}

impl PanicHookGuard {
    /// Saves the current hook and installs the terminal-restoring one.
    pub fn install() -> Self {
        let previous: Arc<PanicHook> = Arc::new(panic::take_hook());
        let hook = Arc::clone(&previous);
        panic::set_hook(Box::new(move |info| {
            best_effort_restore_terminal();
            hook(info);
        }));
        Self { previous }
    }
}

fn noop_hook(_: &PanicHookInfo<'_>) {}

impl Drop for PanicHookGuard {
    /// Reinstates the previously installed hook.
    fn drop(&mut self) {
        // Remove our hook first, which releases its `Arc` clone, then move
        // the saved hook out and reinstall it.
        let current = panic::take_hook();
        drop(current);
        let placeholder: Arc<PanicHook> = Arc::new(Box::new(noop_hook as fn(&PanicHookInfo<'_>)));
        match Arc::try_unwrap(std::mem::replace(&mut self.previous, placeholder)) {
            Ok(hook) => panic::set_hook(hook),
            // Only reachable with multiple guards: keep delegating instead of
            // leaving the process without a hook.
            Err(shared) => panic::set_hook(Box::new(move |info| shared(info))),
        }
    }
}

/// Tracks which entry steps completed so a failed entry can roll back exactly
/// those, in reverse order.
#[derive(Default)]
struct ScreenState {
    alternate: bool,
    bracketed_paste: bool,
    mouse_capture: bool,
    cursor_hidden: bool,
}

impl ScreenState {
    fn apply<W: io::Write>(&mut self, writer: &mut W) -> Result<(), TerminalError> {
        execute!(writer, EnterAlternateScreen)
            .map_err(|err| TerminalError::new("enter alternate screen", err))?;
        self.alternate = true;
        execute!(writer, EnableBracketedPaste)
            .map_err(|err| TerminalError::new("enable bracketed paste", err))?;
        self.bracketed_paste = true;
        execute!(writer, EnableMouseCapture)
            .map_err(|err| TerminalError::new("enable mouse capture", err))?;
        self.mouse_capture = true;
        execute!(writer, Hide).map_err(|err| TerminalError::new("hide cursor", err))?;
        self.cursor_hidden = true;
        Ok(())
    }

    fn rollback<W: io::Write>(&self, writer: &mut W) {
        if self.cursor_hidden {
            let _ = execute!(writer, Show);
        }
        if self.mouse_capture {
            let _ = execute!(writer, DisableMouseCapture);
        }
        if self.bracketed_paste {
            let _ = execute!(writer, DisableBracketedPaste);
        }
        if self.alternate {
            let _ = execute!(writer, LeaveAlternateScreen);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALT_ENTER: &str = "\x1b[?1049h";
    const ALT_LEAVE: &str = "\x1b[?1049l";
    const PASTE_ON: &str = "\x1b[?2004h";
    const PASTE_OFF: &str = "\x1b[?2004l";
    const MOUSE_ON: &str = "\x1b[?1000h";
    const MOUSE_OFF: &str = "\x1b[?1000l";
    const CURSOR_HIDE: &str = "\x1b[?25l";
    const CURSOR_SHOW: &str = "\x1b[?25h";

    /// Writer that fails once when a specific escape sequence is written and
    /// never records that sequence in its output.
    #[derive(Debug)]
    struct FailingWriter {
        out: Vec<u8>,
        fail_on: &'static str,
        failed: bool,
    }

    impl FailingWriter {
        fn fail_on(marker: &'static str) -> Self {
            Self {
                out: Vec::new(),
                fail_on: marker,
                failed: false,
            }
        }
    }

    impl io::Write for FailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if !self.failed && bytes_contain(buf, self.fail_on.as_bytes()) {
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected write failure",
                ));
            }
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn index_of(bytes: &[u8], needle: &str) -> usize {
        let needle = needle.as_bytes();
        bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("escape marker should be present")
    }

    /// These tests assert the ANSI command path. On Windows the mouse-capture
    /// command is applied through WinAPI rather than the writer, so the byte
    /// assertions only hold on unix.
    #[cfg(not(windows))]
    mod ansi_sequences {
        use super::*;

        #[test]
        fn entry_runs_screen_commands_in_spec_order() {
            let mut screen = ScreenState::default();
            let mut out = Vec::new();
            screen.apply(&mut out).unwrap();

            let alt = index_of(&out, ALT_ENTER);
            let paste = index_of(&out, PASTE_ON);
            let mouse = index_of(&out, MOUSE_ON);
            let hide = index_of(&out, CURSOR_HIDE);
            assert!(alt < paste && paste < mouse && mouse < hide);
            assert!(
                screen.alternate
                    && screen.bracketed_paste
                    && screen.mouse_capture
                    && screen.cursor_hidden
            );
        }

        #[test]
        fn restore_runs_commands_in_reverse_order() {
            let mut out = Vec::new();
            restore_sequence(&mut out).unwrap();

            let show = index_of(&out, CURSOR_SHOW);
            let mouse = index_of(&out, MOUSE_OFF);
            let paste = index_of(&out, PASTE_OFF);
            let leave = index_of(&out, ALT_LEAVE);
            assert!(show < mouse && mouse < paste && paste < leave);
        }

        #[test]
        fn restore_reports_first_error_but_still_runs_later_steps() {
            let mut writer = FailingWriter::fail_on(PASTE_OFF);
            let error = restore_sequence(&mut writer).unwrap_err();
            assert!(error.to_string().contains("disable bracketed paste"));
            assert!(bytes_contain(&writer.out, CURSOR_SHOW.as_bytes()));
            assert!(bytes_contain(&writer.out, MOUSE_OFF.as_bytes()));
            assert!(!bytes_contain(&writer.out, PASTE_OFF.as_bytes()));
            assert!(bytes_contain(&writer.out, ALT_LEAVE.as_bytes()));
        }

        #[test]
        fn partial_entry_failure_rolls_back_only_completed_steps() {
            let mut writer = FailingWriter::fail_on(PASTE_ON);
            let mut screen = ScreenState::default();
            let error = screen.apply(&mut writer).unwrap_err();
            assert!(error.to_string().contains("enable bracketed paste"));
            assert!(bytes_contain(&writer.out, ALT_ENTER.as_bytes()));
            assert!(!bytes_contain(&writer.out, CURSOR_HIDE.as_bytes()));

            assert!(screen.alternate);
            assert!(!screen.bracketed_paste);
            assert!(!screen.mouse_capture);
            assert!(!screen.cursor_hidden);

            let mut rollback = Vec::new();
            screen.rollback(&mut rollback);
            assert_eq!(rollback, ALT_LEAVE.as_bytes());
        }
    }

    #[test]
    fn latch_allows_retry_after_failure_and_noops_after_success() {
        use std::cell::Cell;

        fn failing(attempts: &Cell<usize>) -> Result<(), TerminalError> {
            attempts.set(attempts.get() + 1);
            Err(TerminalError::new("op", io::Error::other("boom")))
        }

        let attempts = Cell::new(0);
        let mut latch = RestoreLatch::default();
        assert!(latch.attempt(|| failing(&attempts)).is_err());
        assert_eq!(attempts.get(), 1);
        assert!(latch.attempt(|| failing(&attempts)).is_err());
        assert_eq!(attempts.get(), 2);
        assert!(
            latch
                .attempt(|| {
                    attempts.set(attempts.get() + 1);
                    Ok(())
                })
                .is_ok()
        );
        assert_eq!(attempts.get(), 3);
        assert!(latch.attempt(|| failing(&attempts)).is_ok());
        assert_eq!(attempts.get(), 3);
    }
}
