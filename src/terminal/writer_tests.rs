use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use super::TerminalWriter;

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    writes: usize,
    fail_write: bool,
    fail_flush: bool,
    panic_write: bool,
    interrupt_once: bool,
    max_write: Option<usize>,
}

#[derive(Clone, Default)]
struct Probe(Rc<RefCell<Output>>);

impl Write for Probe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self.0.borrow_mut();
        output.writes += 1;
        assert!(!output.panic_write, "injected writer panic");
        if std::mem::take(&mut output.interrupt_once) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if output.fail_write {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let count = bytes.len().min(output.max_write.unwrap_or(usize::MAX));
        output.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.0.borrow().fail_flush {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn small_writes_are_batched_until_explicit_flush() {
    let probe = Probe::default();
    let mut writer = TerminalWriter::new(probe.clone());
    for _ in 0..2000 {
        writer.write_all(b"x").unwrap();
    }
    assert_eq!(
        probe.0.borrow().writes,
        0,
        "small terminal writes must stay buffered"
    );
    writer.flush().unwrap();
    let output = probe.0.borrow();
    assert_eq!(output.writes, 1);
    assert_eq!(output.bytes, vec![b'x'; 2000]);
}

#[test]
fn drop_does_not_emit_pending_frame_after_emergency_restore() {
    let mut probe = Probe::default();
    let mut writer = TerminalWriter::new(probe.clone());
    writer.write_all(b"STALE_FRAME").unwrap();
    probe.write_all(b"RESTORED").unwrap();
    drop(writer);
    assert_eq!(probe.0.borrow().bytes, b"RESTORED");
}

#[test]
fn failed_write_on_flush_is_reported_without_drop_retry() {
    let probe = Probe::default();
    probe.0.borrow_mut().fail_write = true;
    let mut writer = TerminalWriter::new(probe.clone());
    writer.write_all(b"pending frame").unwrap();
    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    let writes = probe.0.borrow().writes;
    drop(writer);
    assert_eq!(probe.0.borrow().writes, writes);
}

#[test]
fn underlying_flush_error_is_not_swallowed() {
    let probe = Probe::default();
    probe.0.borrow_mut().fail_flush = true;
    let mut writer = TerminalWriter::new(probe.clone());
    writer.write_all(b"frame").unwrap();
    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(probe.0.borrow().bytes, b"frame");
}

#[test]
fn interrupted_and_short_writes_preserve_every_byte() {
    let probe = Probe::default();
    probe.0.borrow_mut().interrupt_once = true;
    probe.0.borrow_mut().max_write = Some(3);
    let mut writer = TerminalWriter::new(probe.clone());
    let text = "\x1b[38;2;12;34;56m中文 👩🏽‍💻 e\u{301}\x1b[0m";
    writer.write_all(text.as_bytes()).unwrap();
    writer.flush().unwrap();
    assert_eq!(probe.0.borrow().bytes, text.as_bytes());
}

#[test]
fn large_writes_are_not_truncated() {
    let probe = Probe::default();
    let mut writer = TerminalWriter::new(probe.clone());
    let bytes = vec![b'x'; 192 * 1024 + 7];
    writer.write_all(&bytes).unwrap();
    writer.flush().unwrap();
    assert_eq!(probe.0.borrow().bytes, bytes);
}

#[test]
fn full_buffers_flush_in_order() {
    let probe = Probe::default();
    let mut writer = TerminalWriter::new(probe.clone());
    let mut expected = Vec::new();
    for index in 0..1000 {
        let chunk = [(index % 251) as u8; 257];
        expected.extend_from_slice(&chunk);
        writer.write_all(&chunk).unwrap();
    }
    writer.flush().unwrap();
    assert_eq!(probe.0.borrow().bytes, expected);
    assert!(probe.0.borrow().writes <= 5);
}

#[test]
fn writer_panic_does_not_retry_io_during_unwind() {
    let probe = Probe::default();
    probe.0.borrow_mut().panic_write = true;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut writer = TerminalWriter::new(probe.clone());
        writer.write_all(b"pending frame").unwrap();
        writer.flush().unwrap();
    }));
    assert!(result.is_err());
    assert_eq!(probe.0.borrow().writes, 1);
}

#[cfg(not(windows))]
#[test]
fn crossterm_bytes_match_with_far_fewer_underlying_writes() {
    use ratatui::backend::{Backend, CrosstermBackend};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};

    let area = Rect::new(0, 0, 100, 24);
    let before = Buffer::empty(area);
    let mut after = Buffer::empty(area);
    for y in 0..area.height {
        after.set_string(
            0,
            y,
            "中文 👩🏽‍💻 e\u{301} mixed width ".repeat(4),
            Style::new()
                .fg(Color::Rgb(10, y as u8, 80))
                .bg(Color::Rgb(40, 40, 40)),
        );
    }
    let plain = Probe::default();
    let mut plain_backend = CrosstermBackend::new(plain.clone());
    plain_backend.draw(before.diff(&after).into_iter()).unwrap();
    Backend::flush(&mut plain_backend).unwrap();
    let buffered = Probe::default();
    let mut buffered_backend = CrosstermBackend::new(TerminalWriter::new(buffered.clone()));
    buffered_backend
        .draw(before.diff(&after).into_iter())
        .unwrap();
    Backend::flush(&mut buffered_backend).unwrap();
    let plain = plain.0.borrow();
    let buffered = buffered.0.borrow();
    assert_eq!(buffered.bytes, plain.bytes);
    eprintln!(
        "ANSI bytes={}, underlying writes={} -> {}",
        plain.bytes.len(),
        plain.writes,
        buffered.writes
    );
    assert!(plain.writes > 1000);
    assert!(buffered.writes <= 2, "buffered writes: {}", buffered.writes);
}

#[cfg(not(windows))]
#[test]
fn discard_prevents_ratatui_drop_from_flushing_a_stale_frame() {
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    let mut probe = Probe::default();
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(TerminalWriter::new(probe.clone())),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 20, 4)),
        },
    )
    .unwrap();
    terminal.hide_cursor().unwrap();
    probe.0.borrow_mut().bytes.clear();
    terminal.backend_mut().write_all(b"STALE_FRAME").unwrap();
    terminal.backend_mut().writer_mut().discard_pending();
    probe.write_all(b"RESTORED").unwrap();
    drop(terminal);
    assert_eq!(probe.0.borrow().bytes, b"RESTORED\x1b[?25h");
}
