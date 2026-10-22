//! Clipboard writes: the Win32 clipboard on Windows, a platform program with a
//! deadline elsewhere, and an OSC 52 escape as the fallback that also works
//! over SSH and inside tmux.

use std::io;
#[cfg(not(windows))]
use std::process::Stdio;
#[cfg(not(windows))]
use std::time::Duration;

use base64::Engine;
#[cfg(not(windows))]
use tokio::io::AsyncWriteExt;
#[cfg(not(windows))]
use tokio::process::Command;
#[cfg(not(windows))]
use tokio::time::timeout;

/// A selection larger than this is refused instead of handed to an OS process.
pub const MAX_CLIPBOARD_BYTES: usize = 1_000_000;

#[cfg(not(windows))]
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Which transport carried the text, so the caller can say so when the native
/// clipboard was not available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copied {
    Native,
    Osc52,
}

#[cfg(not(windows))]
#[derive(Clone, Copy, Debug)]
struct Adapter {
    program: &'static str,
    args: &'static [&'static str],
}

#[cfg(not(windows))]
const fn adapter() -> Option<Adapter> {
    if cfg!(target_os = "macos") {
        Some(Adapter {
            program: "pbcopy",
            args: &[],
        })
    } else if cfg!(target_os = "linux") {
        Some(Adapter {
            program: "xclip",
            args: &["-selection", "clipboard"],
        })
    } else {
        None
    }
}

/// Whether [`copy`] accepts `text`.
pub fn check(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("nothing to copy".into());
    }
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(format!(
            "selection is {} bytes, over the {MAX_CLIPBOARD_BYTES} byte clipboard limit",
            text.len()
        ));
    }
    Ok(())
}

/// Copies `text` through the native program, falling back to OSC 52.
pub async fn copy(text: &str) -> Result<Copied, String> {
    check(text)?;
    if write_native(text).await.is_ok() {
        return Ok(Copied::Native);
    }
    write_osc52(text)
}

// `clip.exe` needs a UTF-16 byte order mark and keeps it in the copied text,
// where a paste then carries an invisible U+FEFF.
#[cfg(windows)]
async fn write_native(text: &str) -> io::Result<()> {
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || clipboard_win::set_clipboard_string(&text))
        .await
        .map_err(io::Error::other)?
        .map_err(|error| io::Error::other(error.to_string()))
}

#[cfg(not(windows))]
async fn write_native(text: &str) -> io::Result<()> {
    let adapter = adapter().ok_or_else(|| io::Error::other("no clipboard program"))?;
    write_program(adapter, text).await
}

#[cfg(not(windows))]
async fn write_program(adapter: Adapter, text: &str) -> io::Result<()> {
    let mut child = Command::new(adapter.program)
        .args(adapter.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("clipboard stdin is unavailable"))?;
    let payload = text.as_bytes().to_vec();
    // One future owns stdin: dropping it at the deadline closes our write end,
    // so a child that never drains the pipe cannot hold the caller.
    let deadline = timeout(WRITE_TIMEOUT, async move {
        stdin.write_all(&payload).await?;
        stdin.shutdown().await
    });
    if deadline.await.is_err() {
        return Err(io::Error::other("clipboard program did not read its input"));
    }
    match timeout(WRITE_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(io::Error::other(format!(
            "{} exited with {status}",
            adapter.program
        ))),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(io::Error::other("clipboard program did not exit")),
    }
}

/// The OSC 52 sequence for `text`. tmux only forwards it when wrapped in its
/// passthrough guard, which escapes the inner escape byte.
fn osc52_sequence(text: &str, tmux: bool) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    if tmux {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{encoded}\x07")
    }
}

fn write_osc52(text: &str) -> Result<Copied, String> {
    use std::io::Write;

    let sequence = osc52_sequence(text, std::env::var_os("TMUX").is_some());
    let mut stdout = std::io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("OSC 52 copy failed: {error}"))?;
    Ok(Copied::Osc52)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_carries_base64_of_the_utf8_body() {
        assert_eq!(osc52_sequence("héllo", false), "\x1b]52;c;aMOpbGxv\x07");
        assert_eq!(osc52_sequence("abc", false), "\x1b]52;c;YWJj\x07");
    }

    #[test]
    fn tmux_wraps_the_same_payload_in_a_passthrough_guard() {
        let sequence = osc52_sequence("abc", true);
        assert_eq!(sequence, "\x1bPtmux;\x1b\x1b]52;c;YWJj\x07\x1b\\");
        assert!(sequence.contains("YWJj"));
    }

    #[tokio::test]
    async fn empty_and_oversized_selections_are_refused_before_spawning() {
        assert!(copy("").await.is_err());
        let big = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        let error = copy(&big).await.unwrap_err();
        assert!(error.contains("limit"), "{error}");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_child_that_never_drains_its_input_is_bounded() {
        // `cat` with stdout nulled reads its input; a sleeping child does not.
        let result = timeout(
            Duration::from_secs(10),
            write_program(
                Adapter {
                    program: "sleep",
                    args: &["30"],
                },
                &"x".repeat(1 << 20),
            ),
        )
        .await;
        assert!(matches!(result, Ok(Err(_))), "{result:?}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_windows_copy_holds_the_text_without_a_byte_order_mark() {
        assert_eq!(copy("作用abc").await, Ok(Copied::Native));
        assert_eq!(clipboard_win::get_clipboard_string().unwrap(), "作用abc");
    }
}
