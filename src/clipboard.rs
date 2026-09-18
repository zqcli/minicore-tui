//! The one native clipboard boundary used by the TUI.
//!
//! Copying is an outbound effect, not a renderer side effect. The production
//! adapter invokes exactly one platform clipboard program selected at compile
//! time and never falls back to another protocol. A child that fails, hangs,
//! or cannot accept its pipe is killed and reported; no partial state is left
//! behind.
//!
//! Reclamation is bounded but not omniscient: the call kills the direct child
//! and detaches its writer thread on the deadline, so a descendant that
//! inherited the pipe's read end can, in principle, keep that thread alive
//! until the pipe closes or the process exits. The fixed platform adapters
//! (`pbcopy`, `xclip`, `clip.exe`) do not spawn descendants; the residual
//! leak is documented here rather than assumed away.
//!
//! Encoding is per platform: macOS (`pbcopy`) and Linux (`xclip`) receive
//! `text` as UTF-8; Windows `clip.exe` interprets console input in the OEM
//! codepage, so it receives UTF-16LE with a byte-order mark (the encoding
//! `clip` actually round-trips). Only the macOS adapter is exercised on real
//! hardware in this repo; the Windows and Linux branches are compile-time
//! adapters verified by unit tests and honest documentation, not native
//! machine runs.

use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Refuse an unbounded selection before handing it to an OS clipboard
/// process. The limit is on UTF-8 bytes, which is also what the child receives.
pub const MAX_CLIPBOARD_BYTES: usize = 1_000_000;

pub trait ClipboardPort {
    fn set_text(&mut self, text: &str) -> io::Result<()>;
}

/// The platform-native clipboard adapter. Its command is fixed by target
/// platform; it never probes or falls back to another clipboard protocol.
#[derive(Debug, Clone, Copy)]
pub struct NativeClipboard {
    program: &'static str,
    args: &'static [&'static str],
}

impl NativeClipboard {
    pub fn new() -> Self {
        Self {
            program: native_program(),
            args: native_arguments(),
        }
    }

    pub fn program(&self) -> &'static str {
        self.program
    }
}

impl Default for NativeClipboard {
    fn default() -> Self {
        Self::new()
    }
}

/// How long a clipboard child may run (the whole write+wait, one wall clock)
/// before it is treated as hung.
const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll `try_wait` until `deadline`; `None` means the child is still alive at
/// the deadline. The deadline is shared with the write phase so the whole
/// `set_text` call is bounded by one wall clock, never by pipe capacity.
fn reap_until(
    child: &mut Child,
    deadline: Instant,
) -> io::Result<Option<std::process::ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// The platform-correct byte encoding for one clipboard program. `clip.exe`
/// decodes its stdin in the OEM/ANSI codepage, so UTF-8 would corrupt every
/// non-ASCII run; UTF-16LE with a BOM is what it round-trips. All other
/// targets feed the text as UTF-8 bytes.
fn clipboard_payload(text: &str) -> Vec<u8> {
    #[cfg(target_os = "windows")]
    {
        let mut payload = Vec::with_capacity(2 + text.len() * 2 + 2);
        payload.extend_from_slice(&[0xFF, 0xFE]); // UTF-16LE BOM
        for unit in text.encode_utf16() {
            payload.extend_from_slice(&unit.to_le_bytes());
        }
        payload
    }
    #[cfg(not(target_os = "windows"))]
    {
        text.as_bytes().to_vec()
    }
}

/// Spawn `program`, stream `text` into its stdin, wait for it to exit, and
/// return — all bounded by one shared `timeout` wall clock.
///
/// A synchronous `write_all` alone can hang forever: a child that never
/// drains its pipe fills the OS pipe buffer (~64 KiB) while a multi-hundred-
/// KiB selection is still queued. The write therefore runs on a controlled
/// writer thread. On the deadline the direct child is killed; that normally
/// closes the pipe and unblocks the writer with `EPIPE`. If the child spawned
/// a descendant that inherited the read end, killing the direct child does
/// not close the pipe and the writer can stay blocked: the thread is then
/// detached instead of joined so this function and the UI stay bounded, and
/// the detached thread exits when the pipe finally closes or the process
/// exits. The fixed platform adapters (`pbcopy`/`xclip`/`clip.exe`) spawn no
/// descendants in practice; the residual leak is a real, documented risk (see
/// the module docs), not a claim that a hung helper can never block.
fn run_clipboard_with_timeout(
    program: &str,
    args: &[&str],
    text: &str,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("native clipboard `{program}` unavailable: {error}"),
            )
        })?;
    let payload = clipboard_payload(text);
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other(format!(
            "native clipboard `{program}` has no stdin"
        )));
    };

    // The payload is streamed by a short-lived writer thread. It gains no
    // independent lifetime while it can make progress: the caller either
    // joins it here or, on the deadline, kills the direct child first (which
    // normally breaks the pipe and unblocks the write with EPIPE). If a child
    // descendant still holds the read end, the thread is detached rather than
    // joined so the caller stays bounded; see the function docs for the
    // residual risk.
    let writer = thread::spawn(move || {
        let outcome = stdin.write_all(&payload).map(drop);
        drop(stdin); // EOF to the child after a successful write
        outcome
    });

    let write_outcome = loop {
        if writer.is_finished() {
            break writer
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("clipboard writer thread panicked")));
        }
        if Instant::now() >= deadline {
            // The child never drained the pipe: kill the direct child and
            // detach the writer (do not join) so a descendant that inherited
            // the read end cannot block this bounded call.
            let _ = child.kill();
            drop(writer);
            let _ = child.wait();
            return Err(io::Error::other(format!(
                "native clipboard `{program}` did not drain its input within {timeout:?}"
            )));
        }
        thread::sleep(Duration::from_millis(5));
    };

    // The write is done; reap the child with the same shared deadline.
    let status = reap_until(&mut child, deadline)?;
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other(format!(
            "native clipboard `{program}` did not exit within {timeout:?}"
        )));
    };
    match (write_outcome, status.success()) {
        // The child gave up on its input; a broken pipe is a symptom of its
        // own exit, and its status is the more useful signal to report.
        (Err(_error), true) => Err(io::Error::other(format!(
            "native clipboard `{program}` closed its input before reading all of it"
        ))),
        (_, false) => Err(io::Error::other(format!(
            "native clipboard `{program}` exited with {status}"
        ))),
        (Ok(()), true) => Ok(()),
    }
}

fn run_clipboard(program: &str, args: &[&str], text: &str) -> io::Result<()> {
    run_clipboard_with_timeout(program, args, text, CHILD_TIMEOUT)
}

impl ClipboardPort for NativeClipboard {
    fn set_text(&mut self, text: &str) -> io::Result<()> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "selection is too large for the native clipboard",
            ));
        }
        run_clipboard(self.program, self.args, text)
    }
}

#[cfg(target_os = "macos")]
fn native_program() -> &'static str {
    "pbcopy"
}

#[cfg(target_os = "macos")]
fn native_arguments() -> &'static [&'static str] {
    &[]
}

#[cfg(target_os = "windows")]
fn native_program() -> &'static str {
    "clip"
}

#[cfg(target_os = "windows")]
fn native_arguments() -> &'static [&'static str] {
    &[]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_program() -> &'static str {
    // XClip is deliberately the only Linux/Unix adapter. Wayland-only or
    // headless environments fail clearly instead of silently trying OSC 52 or
    // another executable.
    "xclip"
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_arguments() -> &'static [&'static str] {
    &["-selection", "clipboard"]
}

#[cfg(not(any(unix, target_os = "windows")))]
fn native_program() -> &'static str {
    "minicore-native-clipboard-unavailable"
}

#[cfg(not(any(unix, target_os = "windows")))]
fn native_arguments() -> &'static [&'static str] {
    &[]
}

pub type TerminalClipboard = NativeClipboard;

pub fn terminal_clipboard() -> TerminalClipboard {
    NativeClipboard::new()
}

/// A deterministic clipboard double for App and integration tests.
#[derive(Debug, Default)]
pub struct MockClipboard {
    pub text: Option<String>,
    pub error: Option<String>,
}

impl ClipboardPort for MockClipboard {
    fn set_text(&mut self, text: &str) -> io::Result<()> {
        if let Some(error) = &self.error {
            return Err(io::Error::other(error.clone()));
        }
        self.text = Some(text.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardPort, MAX_CLIPBOARD_BYTES, MockClipboard, NativeClipboard, clipboard_payload,
    };
    // Subprocess regression tests run on Unix/macOS only (`sleep`/`cat`/`sh` do
    // not exist as Windows commands), so their imports are gated with them to
    // keep `-D warnings` clean under the Windows cross clippy.
    #[cfg(not(target_os = "windows"))]
    use super::run_clipboard_with_timeout;
    #[cfg(not(target_os = "windows"))]
    use std::time::Duration;

    #[test]
    fn native_adapter_uses_one_compile_time_program() {
        let clipboard = NativeClipboard::new();
        assert!(!clipboard.program().is_empty());
    }

    #[test]
    fn mock_clipboard_records_text_and_can_fail() {
        let mut clipboard = MockClipboard::default();
        clipboard.set_text("select").expect("mock accepts text");
        assert_eq!(clipboard.text.as_deref(), Some("select"));

        let mut clipboard = MockClipboard {
            text: None,
            error: Some("headless".to_owned()),
        };
        assert!(clipboard.set_text("select").is_err());
        assert!(clipboard.text.is_none());
    }

    #[test]
    fn native_adapter_rejects_oversized_text_before_spawning() {
        let mut clipboard = NativeClipboard::new();
        let text = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        let error = clipboard.set_text(&text).expect_err("limit is enforced");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn clipboard_payload_encoding_matches_the_target_platform() {
        // Windows clip.exe receives UTF-16LE with a BOM (the only encoding it
        // round-trips); every other adapter is fed the text as UTF-8 bytes.
        let payload = clipboard_payload("café 中 🚀");
        #[cfg(target_os = "windows")]
        {
            let units: Vec<u16> = payload[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            assert_eq!(
                payload[..2],
                [0xFF, 0xFE],
                "UTF-16LE BOM must lead the payload"
            );
            assert_eq!(
                String::from_utf16(&units).expect("valid UTF-16"),
                "café 中 🚀"
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(payload, "café 中 🚀".as_bytes());
        }
    }

    /// A child that never drains its input must not hang the caller: the
    /// payload (much larger than a 64 KiB pipe buffer) is written by the
    /// controlled writer thread, the deadline kills the direct child and
    /// detaches the writer, and `set_text` returns bounded. (A descendant
    /// holding the pipe could keep the detached thread alive; the fixed
    /// adapters spawn none, and the risk is documented in the module docs.)
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_draining_child_is_bounded_and_killed() {
        let started = std::time::Instant::now();
        let result = run_clipboard_with_timeout(
            "sleep",
            &["30"],
            &"x".repeat(1_000_000), // 1 MiB >> the OS pipe buffer
            Duration::from_millis(500),
        );
        assert!(
            result.is_err(),
            "a non-draining clipboard child must be reported as a failure"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "write+wait must stay bounded (elapsed {:?})",
            started.elapsed()
        );
    }

    /// A real draining reader accepts the full payload and closes cleanly.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn draining_child_consumes_the_full_payload() {
        let result = run_clipboard_with_timeout(
            "cat",
            &[],
            &"payload-line\n".repeat(50_000),
            Duration::from_secs(5),
        );
        assert!(result.is_ok(), "draining `cat` must succeed: {result:?}");
    }

    /// A child that exits without reading its input (nonzero) is reaped and
    /// the failure is reported; the write error and child are cleaned up.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn nonzero_exit_without_reading_is_reported_bounded() {
        let started = std::time::Instant::now();
        let result = run_clipboard_with_timeout(
            "sh",
            &["-c", "exit 7"],
            &"x".repeat(200_000),
            Duration::from_secs(5),
        );
        assert!(result.is_err(), "exit 7 must be a failure");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "nonzero-exit cleanup must stay bounded"
        );
    }
}
