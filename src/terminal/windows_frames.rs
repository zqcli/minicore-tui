//! Windows output. The console shows each flush crossterm makes inside a frame,
//! so a frame is collected and written once, as a synchronized update.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;
use ratatui::{Frame, Terminal};

const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";

/// The bytes of the frame being drawn; `None` lets writes through.
#[derive(Clone, Default)]
pub struct FrameBuffer(Rc<RefCell<Option<Vec<u8>>>>);

pub struct FrameWriter<W> {
    inner: W,
    frame: FrameBuffer,
}

impl<W> FrameWriter<W> {
    pub fn new(inner: W, frame: FrameBuffer) -> Self {
        Self { inner, frame }
    }
}

impl<W: Write> Write for FrameWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(frame) = self.frame.0.borrow_mut().as_mut() {
            frame.extend_from_slice(bytes);
            return Ok(bytes.len());
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.frame.0.borrow().is_some() {
            return Ok(());
        }
        self.inner.flush()
    }
}

/// Draws one frame like [`super::draw_frame`] and writes it with one flush.
pub fn draw<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<FrameWriter<W>>>,
    frame: &FrameBuffer,
    render: impl FnOnce(&mut Frame) -> Option<Position>,
) -> io::Result<()> {
    *frame.0.borrow_mut() = Some(BEGIN.to_vec());
    let drawn = super::draw_frame(terminal, render);
    let mut bytes = frame.0.take().unwrap_or_default();
    drawn?;
    bytes.extend_from_slice(END);
    let output = terminal.backend_mut();
    output.write_all(&bytes)?;
    output.flush()
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

    /// Records what reaches the terminal at each flush.
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

    fn terminal(
        writer: &Writer,
        frame: &FrameBuffer,
    ) -> Terminal<CrosstermBackend<FrameWriter<Writer>>> {
        let backend = CrosstermBackend::new(FrameWriter::new(writer.clone(), frame.clone()));
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 10)),
            },
        )
        .unwrap()
    }

    #[test]
    fn outside_a_frame_output_passes_through() {
        let writer = Writer::default();
        let mut output = FrameWriter::new(writer.clone(), FrameBuffer::default());
        output.write_all(b"normal output").unwrap();
        output.flush().unwrap();
        assert_eq!(writer.0.borrow().flushes, [b"normal output".to_vec()]);
    }

    #[test]
    fn each_frame_reaches_the_terminal_in_one_flush_with_the_caret_last() {
        let writer = Writer::default();
        let frame = FrameBuffer::default();
        let mut terminal = terminal(&writer, &frame);
        for step in 0..3 {
            let count = writer.0.borrow().flushes.len();
            draw(&mut terminal, &frame, |frame| {
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
    fn a_frame_without_a_caret_does_not_show_the_cursor() {
        let writer = Writer::default();
        let frame = FrameBuffer::default();
        let mut terminal = terminal(&writer, &frame);
        let count = writer.0.borrow().flushes.len();
        draw(&mut terminal, &frame, |_| None).unwrap();
        let trace = writer.0.borrow();
        assert_eq!(trace.flushes.len(), count + 1);
        let bytes = trace.flushes.last().unwrap();
        assert!(!bytes.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
        assert!(bytes.ends_with(END));
    }
}
