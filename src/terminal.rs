//! Terminal lifecycle: raw mode, the alternate screen, bracketed paste, mouse
//! capture, keyboard enhancement and a panic hook that restores the terminal
//! before the previously installed hook runs.

use std::io::{self, Write};

use crossterm::cursor::{Hide, SetCursorStyle, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Position;
use ratatui::{Frame, Terminal};

pub mod input;

#[cfg(windows)]
pub mod windows_frames;

#[cfg(not(windows))]
pub type Term = Terminal<CrosstermBackend<io::Stdout>>;
#[cfg(windows)]
pub type Term = Terminal<CrosstermBackend<windows_frames::FrameWriter<io::Stdout>>>;

/// Draw with the hardware cursor hidden, then position it before showing it.
///
/// Ratatui 0.30.2 applies the buffer diff before hiding/showing the cursor, and
/// shows a requested cursor before moving it. With Crossterm's separate flushes
/// that exposes the output cursor beside animated glyphs. The callback returns
/// the desired caret instead of calling `Frame::set_cursor_position`, so Ratatui
/// keeps the cursor hidden until this function has moved it to the input field.
pub fn draw_frame<B: Backend>(
    terminal: &mut Terminal<B>,
    render: impl FnOnce(&mut Frame) -> Option<Position>,
) -> Result<(), B::Error> {
    terminal.hide_cursor()?;
    let mut cursor = None;
    terminal.draw(|frame| cursor = render(frame))?;
    if let Some(position) = cursor {
        terminal.set_cursor_position(position)?;
        terminal.show_cursor()?;
    }
    Ok(())
}

/// Shift+Enter and Alt+Enter only reach us as distinct keys when the terminal
/// speaks the kitty keyboard protocol. The push is unconditional: terminals
/// without support ignore the sequence, and probing support costs a blocking
/// round trip on startup.
fn push_keyboard_flags<W: Write>(writer: &mut W) -> io::Result<()> {
    execute!(
        writer,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

fn enter_screen<W: Write>(writer: &mut W) -> io::Result<()> {
    execute!(
        writer,
        EnterAlternateScreen,
        SetCursorStyle::SteadyBlock,
        EnableBracketedPaste,
        EnableMouseCapture,
        Hide
    )
}

fn leave_screen<W: Write>(writer: &mut W) -> io::Result<()> {
    execute!(
        writer,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        Show
    )
}

/// Owns the fullscreen terminal and restores it on drop.
pub struct Guard {
    terminal: Term,
    #[cfg(windows)]
    frame_control: windows_frames::FrameControl,
    #[cfg(windows)]
    buffer_frames: bool,
    /// False while the screen is handed to a child process (the editor) or
    /// after a completed restore.
    active: bool,
    /// Whether this process pushed the enhancement flags, so [`Self::restore`]
    /// pops exactly what it pushed.
    enhanced_keys: bool,
}

impl Guard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        let enhanced_keys = push_keyboard_flags(&mut stdout).is_ok();
        let screen = enter_screen(&mut stdout);
        #[cfg(windows)]
        let frame_control = windows_frames::FrameControl::default();
        let terminal = screen.and_then(|()| {
            let stdout = io::stdout();
            #[cfg(windows)]
            let stdout = windows_frames::FrameWriter::new(stdout, frame_control.clone());
            Terminal::new(CrosstermBackend::new(stdout)).inspect_err(|_| {
                let mut stdout = io::stdout();
                let _ = leave_screen(&mut stdout);
            })
        });
        match terminal {
            Ok(terminal) => Ok(Self {
                terminal,
                #[cfg(windows)]
                frame_control,
                #[cfg(windows)]
                buffer_frames: crossterm::ansi_support::supports_ansi(),
                active: true,
                enhanced_keys,
            }),
            Err(error) => {
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }

    pub fn terminal(&mut self) -> &mut Term {
        &mut self.terminal
    }

    /// The live render path; keeps animation updates from exposing the cursor.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame) -> Option<Position>) -> io::Result<()> {
        // WinAPI cursor commands bypass Write, so only batch the VT path.
        // Unix/macOS and legacy Windows consoles keep the existing sequence.
        #[cfg(windows)]
        if self.buffer_frames {
            return windows_frames::draw_synchronized_frame(
                &mut self.terminal,
                &self.frame_control,
                render,
            );
        }
        draw_frame(&mut self.terminal, render)
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
            self.enhanced_keys = false;
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

/// Restores the terminal before the previous hook runs, so a panic never
/// leaves the shell in raw mode on the alternate screen.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stdout = io::stdout();
        let _ = leave_screen(&mut stdout);
        let _ = stdout.flush();
        let _ = disable_raw_mode();
        previous(info);
    }));
}

#[cfg(test)]
mod frame_tests {
    #[test]
    fn cursor_styles_are_steady_block_and_user_default_on_exit() {
        use crossterm::Command;
        let mut enter = String::new();
        super::SetCursorStyle::SteadyBlock
            .write_ansi(&mut enter)
            .unwrap();
        assert_eq!(enter, "\x1b[2 q");
        let mut leave = String::new();
        super::SetCursorStyle::DefaultUserShape
            .write_ansi(&mut leave)
            .unwrap();
        assert_eq!(leave, "\x1b[0 q");
    }

    use ratatui::backend::{ClearType, TestBackend, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::Size;

    use super::*;

    #[derive(Debug, PartialEq)]
    enum Op {
        Hide,
        Paint { cursor_visible: bool },
        Move { cursor_visible: bool },
        Show(Position),
        Flush,
    }

    struct RecordingBackend {
        inner: TestBackend,
        ops: Vec<Op>,
        fail_paint: bool,
        fail_move: bool,
    }

    impl RecordingBackend {
        fn new() -> Self {
            Self {
                inner: TestBackend::new(40, 10),
                ops: Vec::new(),
                fail_paint: false,
                fail_move: false,
            }
        }
    }

    impl Backend for RecordingBackend {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.ops.push(Op::Paint {
                cursor_visible: self.inner.cursor_visible(),
            });
            if self.fail_paint {
                return Err(io::Error::other("injected paint failure"));
            }
            // Like a real terminal, printing advances its output cursor.
            let mut after_last_cell = None;
            self.inner
                .draw(content.inspect(|(x, y, _)| {
                    after_last_cell = Some(Position::new(x + 1, *y));
                }))
                .unwrap();
            if let Some(position) = after_last_cell {
                self.inner.set_cursor_position(position).unwrap();
            }
            Ok(())
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.ops.push(Op::Hide);
            self.inner.hide_cursor().unwrap();
            Ok(())
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.ops.push(Op::Show(self.inner.cursor_position()));
            self.inner.show_cursor().unwrap();
            Ok(())
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(self.inner.cursor_position())
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.ops.push(Op::Move {
                cursor_visible: self.inner.cursor_visible(),
            });
            if self.fail_move {
                return Err(io::Error::other("injected move failure"));
            }
            self.inner.set_cursor_position(position).unwrap();
            Ok(())
        }

        fn clear(&mut self) -> io::Result<()> {
            self.inner.clear().unwrap();
            Ok(())
        }

        fn clear_region(&mut self, kind: ClearType) -> io::Result<()> {
            self.inner.clear_region(kind).unwrap();
            Ok(())
        }

        fn size(&self) -> io::Result<Size> {
            Ok(self.inner.size().unwrap())
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(self.inner.window_size().unwrap())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.ops.push(Op::Flush);
            Ok(())
        }
    }

    fn paint(frame: &mut Frame, step: u16) {
        frame.render_widget(format!("spinner {step}s"), frame.area());
    }

    #[test]
    fn stock_frame_cursor_exposes_the_output_position() {
        let mut terminal = Terminal::new(RecordingBackend::new()).unwrap();
        let caret = Position::new(2, 8);
        terminal.hide_cursor().unwrap();
        for step in 0..2 {
            terminal
                .draw(|frame| {
                    paint(frame, step);
                    frame.set_cursor_position(caret);
                })
                .unwrap();
        }
        let ops = &terminal.backend().ops;
        assert!(ops.contains(&Op::Paint {
            cursor_visible: true
        }));
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::Show(pos) if *pos != caret))
        );
    }

    #[test]
    fn every_frame_paints_hidden_and_moves_before_showing() {
        let mut terminal = Terminal::new(RecordingBackend::new()).unwrap();
        for step in 0..3 {
            terminal.backend_mut().ops.clear();
            let caret = Position::new(2 + step, 8);
            draw_frame(&mut terminal, |frame| {
                paint(frame, step);
                Some(caret)
            })
            .unwrap();
            assert_eq!(
                terminal.backend().ops,
                [
                    Op::Hide,
                    Op::Paint {
                        cursor_visible: false
                    },
                    Op::Hide,
                    Op::Flush,
                    Op::Move {
                        cursor_visible: false
                    },
                    Op::Show(caret),
                ]
            );
            assert!(terminal.backend().inner.cursor_visible());
            assert_eq!(terminal.backend().inner.cursor_position(), caret);
        }
    }

    #[test]
    fn a_view_without_a_caret_keeps_the_cursor_hidden() {
        let mut terminal = Terminal::new(RecordingBackend::new()).unwrap();
        draw_frame(&mut terminal, |frame| {
            paint(frame, 0);
            None
        })
        .unwrap();
        assert!(!terminal.backend().inner.cursor_visible());
        assert!(
            !terminal
                .backend()
                .ops
                .iter()
                .any(|op| matches!(op, Op::Show(_)))
        );
    }

    #[test]
    fn failed_paint_never_shows_the_output_cursor() {
        let mut backend = RecordingBackend::new();
        backend.fail_paint = true;
        let mut terminal = Terminal::new(backend).unwrap();
        let result = draw_frame(&mut terminal, |frame| {
            paint(frame, 0);
            Some(Position::new(2, 8))
        });
        assert!(result.is_err());
        assert!(!terminal.backend().inner.cursor_visible());
        assert!(
            !terminal
                .backend()
                .ops
                .iter()
                .any(|op| matches!(op, Op::Show(_)))
        );
    }

    #[test]
    fn failed_positioning_does_not_show_the_cursor_in_the_status_line() {
        let mut backend = RecordingBackend::new();
        backend.fail_move = true;
        let mut terminal = Terminal::new(backend).unwrap();
        let result = draw_frame(&mut terminal, |frame| {
            paint(frame, 0);
            Some(Position::new(2, 8))
        });
        assert!(result.is_err());
        assert!(!terminal.backend().inner.cursor_visible());
        assert!(
            !terminal
                .backend()
                .ops
                .iter()
                .any(|op| matches!(op, Op::Show(_)))
        );
    }
}

// Without ANSI support crossterm drives the Windows console API instead of
// writing escape sequences, so there are no bytes to compare.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn bytes(run: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        run(&mut out).expect("writing to a Vec never fails");
        String::from_utf8(out).expect("escape sequences are ASCII")
    }

    #[test]
    fn entering_sets_every_mode_it_restores() {
        let entered = bytes(|out| {
            push_keyboard_flags(out)?;
            enter_screen(out)
        });
        for sequence in [
            "\x1b[>1u",    // push keyboard enhancement flags
            "\x1b[?1049h", // alternate screen
            "\x1b[2 q",    // steady block cursor
            "\x1b[?2004h", // bracketed paste
            "\x1b[?1000h", // mouse capture
            "\x1b[?25l",   // hide cursor
        ] {
            assert!(
                entered.contains(sequence),
                "missing {sequence:?} in {entered:?}"
            );
        }

        let left = bytes(leave_screen);
        for sequence in [
            "\x1b[?1000l", // mouse capture off
            "\x1b[?2004l", // bracketed paste off
            "\x1b[?1049l", // alternate screen off
            "\x1b[0 q",    // user's default cursor shape
            "\x1b[?25h",   // show cursor
        ] {
            assert!(left.contains(sequence), "missing {sequence:?} in {left:?}");
        }
    }
}
