//! Windows-only frame output batching. This does not receive IME composition
//! events; it keeps the terminal from seeing separate paint/caret flushes.
//!
//! Cursor Hide/Show ordering is intentionally preserved for terminals that do
//! not implement synchronized updates. No blink mode or input mode is changed.
use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;
use ratatui::{Frame, Terminal};

const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";

#[derive(Default)]
struct State {
    active: bool,
    bytes: Vec<u8>,
}

/// Shared with the guard; avoids depending on Ratatui's unstable writer access.
#[derive(Clone, Default)]
pub struct FrameControl(Rc<RefCell<State>>);

impl FrameControl {
    fn begin(&self) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.active {
            return Err(io::Error::other("a terminal frame is already buffered"));
        }
        state.bytes.clear();
        state.bytes.extend_from_slice(BEGIN);
        state.active = true;
        Ok(())
    }

    fn abort(&self) {
        let mut state = self.0.borrow_mut();
        state.active = false;
        state.bytes.clear();
    }

    fn commit(&self, output: &mut impl Write) -> io::Result<()> {
        let mut bytes = {
            let mut state = self.0.borrow_mut();
            state.active = false;
            state.bytes.extend_from_slice(END);
            std::mem::take(&mut state.bytes)
        };
        // The complete frame, including final caret position, is ready before
        // any bytes reach stdout. write_all may split; DEC 2026 protects those
        // splits when supported. The original Hide/Show protects the fallback.
        let result = output.write_all(&bytes).and_then(|()| output.flush());
        if result.is_err() {
            // A partial output could have entered synchronized-update mode.
            // Make a best effort to release it while preserving the first error.
            let _ = output.write_all(END);
            let _ = output.flush();
        }
        bytes.clear();
        self.0.borrow_mut().bytes = bytes;
        result
    }
}

/// Pass-through outside a frame; unbounded-per-frame buffering inside one.
/// Unlike BufWriter, an internal capacity limit cannot emit a partial frame.
pub struct FrameWriter<W> {
    inner: W,
    control: FrameControl,
}

impl<W> FrameWriter<W> {
    pub fn new(inner: W, control: FrameControl) -> Self {
        Self { inner, control }
    }
}

impl<W: Write> Write for FrameWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.control.0.borrow_mut();
        if state.active {
            state.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        } else {
            drop(state);
            self.inner.write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.control.0.borrow().active {
            Ok(())
        } else {
            self.inner.flush()
        }
    }
}

/// Use only with a Windows VT-capable backend. Legacy WinAPI commands bypass
/// Write, so Guard keeps its old render path when ANSI is not supported.
pub fn draw_synchronized_frame<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<FrameWriter<W>>>,
    control: &FrameControl,
    render: impl FnOnce(&mut Frame) -> Option<Position>,
) -> io::Result<()> {
    control.begin()?;
    match super::draw_frame(terminal, render) {
        Ok(()) => control.commit(terminal.backend_mut()),
        Err(error) => {
            // Nothing from this frame has reached the terminal. The caller
            // restores the terminal and exits rather than retrying a partial diff.
            control.abort();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::{TerminalOptions, Viewport};

    use super::*;

    #[derive(Default)]
    struct Trace {
        pending: Vec<u8>,
        flushes: Vec<Vec<u8>>,
    }

    #[derive(Clone, Default)]
    struct Writer(Rc<RefCell<Trace>>);

    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().pending.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut trace = self.0.borrow_mut();
            let bytes = std::mem::take(&mut trace.pending);
            if !bytes.is_empty() {
                trace.flushes.push(bytes);
            }
            Ok(())
        }
    }

    #[test]
    fn outside_a_frame_output_is_unchanged() {
        let writer = Writer::default();
        let mut output = FrameWriter::new(writer.clone(), FrameControl::default());
        output.write_all(b"normal output").unwrap();
        output.flush().unwrap();
        assert_eq!(writer.0.borrow().flushes, [b"normal output".to_vec()]);
    }

    #[test]
    fn internal_flushes_do_not_reach_the_terminal() {
        let writer = Writer::default();
        let control = FrameControl::default();
        let mut output = FrameWriter::new(writer.clone(), control.clone());
        control.begin().unwrap();
        for part in [b"hide".as_slice(), b"paint", b"caret", b"show"] {
            output.write_all(part).unwrap();
            output.flush().unwrap();
            assert!(writer.0.borrow().pending.is_empty());
            assert!(writer.0.borrow().flushes.is_empty());
        }
        control.commit(&mut output).unwrap();
        assert_eq!(writer.0.borrow().flushes.len(), 1);
        assert_eq!(
            writer.0.borrow().flushes[0],
            b"\x1b[?2026hhidepaintcaretshow\x1b[?2026l"
        );
    }

    #[test]
    fn large_frames_do_not_flush_at_buffer_capacity() {
        let writer = Writer::default();
        let control = FrameControl::default();
        let mut output = FrameWriter::new(writer.clone(), control.clone());
        control.begin().unwrap();
        output.write_all(&vec![b'x'; 128 * 1024]).unwrap();
        output.flush().unwrap();
        assert!(writer.0.borrow().pending.is_empty());
        control.commit(&mut output).unwrap();
        assert_eq!(writer.0.borrow().flushes.len(), 1);
        assert_eq!(
            writer.0.borrow().flushes[0].len(),
            128 * 1024 + BEGIN.len() + END.len()
        );
    }

    #[test]
    fn abort_discards_the_frame_and_restores_pass_through() {
        let writer = Writer::default();
        let control = FrameControl::default();
        let mut output = FrameWriter::new(writer.clone(), control.clone());
        control.begin().unwrap();
        output.write_all(b"discard this").unwrap();
        control.abort();
        output.write_all(b"restore").unwrap();
        output.flush().unwrap();
        assert_eq!(writer.0.borrow().flushes, [b"restore".to_vec()]);
    }

    #[test]
    fn nested_frames_are_rejected_without_losing_the_outer_frame() {
        let control = FrameControl::default();
        control.begin().unwrap();
        assert!(control.begin().is_err());
        assert!(control.0.borrow().active);
        control.abort();
    }

    #[test]
    fn a_partial_write_releases_sync_and_preserves_the_original_error() {
        #[derive(Default)]
        struct FailsAfterBegin {
            bytes: Vec<u8>,
            failed: bool,
        }

        impl Write for FailsAfterBegin {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.bytes.is_empty() {
                    self.bytes.extend_from_slice(&bytes[..BEGIN.len()]);
                    return Ok(BEGIN.len());
                }
                if !self.failed {
                    self.failed = true;
                    return Err(io::Error::other("injected write failure"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let control = FrameControl::default();
        control.begin().unwrap();
        control.0.borrow_mut().bytes.extend_from_slice(b"frame");
        let mut output = FailsAfterBegin::default();
        let error = control.commit(&mut output).unwrap_err();
        assert_eq!(error.to_string(), "injected write failure");
        assert_eq!(output.bytes, [BEGIN, END].concat());
        assert!(!control.0.borrow().active);
        assert!(control.0.borrow().bytes.is_empty());
    }

    #[test]
    fn a_flush_failure_also_attempts_to_release_sync() {
        #[derive(Default)]
        struct FailsFlush {
            bytes: Vec<u8>,
            failed: bool,
        }

        impl Write for FailsFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                if !self.failed {
                    self.failed = true;
                    return Err(io::Error::other("injected flush failure"));
                }
                Ok(())
            }
        }

        let control = FrameControl::default();
        control.begin().unwrap();
        let mut output = FailsFlush::default();
        let error = control.commit(&mut output).unwrap_err();
        assert_eq!(error.to_string(), "injected flush failure");
        assert_eq!(output.bytes, [BEGIN, END, END].concat());
        assert!(!control.0.borrow().active);
    }

    #[test]
    fn real_crossterm_emits_only_complete_frames_with_the_caret_restored() {
        let writer = Writer::default();
        let control = FrameControl::default();
        let backend = CrosstermBackend::new(FrameWriter::new(writer.clone(), control.clone()));
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 10)),
            },
        )
        .unwrap();
        for step in 0..120 {
            let count = writer.0.borrow().flushes.len();
            draw_synchronized_frame(&mut terminal, &control, |frame| {
                assert_eq!(writer.0.borrow().flushes.len(), count);
                assert!(writer.0.borrow().pending.is_empty());
                frame.render_widget(format!("Working {step}"), Rect::new(2, 2, 70, 1));
                Some(Position::new(2, 8))
            })
            .unwrap();
            let trace = writer.0.borrow();
            assert_eq!(trace.flushes.len(), count + 1);
            let bytes = trace.flushes.last().unwrap();
            assert!(bytes.starts_with(BEGIN));
            assert!(bytes.ends_with(b"\x1b[9;3H\x1b[?25h\x1b[?2026l"));
        }
    }

    #[test]
    fn a_view_without_a_caret_does_not_show_the_cursor() {
        let writer = Writer::default();
        let control = FrameControl::default();
        let backend = CrosstermBackend::new(FrameWriter::new(writer.clone(), control.clone()));
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 10)),
            },
        )
        .unwrap();
        draw_synchronized_frame(&mut terminal, &control, |_| None).unwrap();
        let trace = writer.0.borrow();
        assert_eq!(trace.flushes.len(), 1);
        assert!(
            !trace.flushes[0]
                .windows(6)
                .any(|bytes| bytes == b"\x1b[?25h")
        );
        assert!(trace.flushes[0].ends_with(END));
    }
}
