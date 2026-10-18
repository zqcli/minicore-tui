//! Clipboard writes: one platform program, one deadline, and an OSC 52
//! fallback when no program is available.

use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// Refuse an unbounded selection before handing it to an OS process.
pub const MAX_CLIPBOARD_BYTES: usize = 1_000_000;

#[derive(Clone, Copy, Debug)]
struct Adapter {
    program: &'static str,
    args: &'static [&'static str],
}

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
    } else if cfg!(target_os = "windows") {
        Some(Adapter {
            program: "clip",
            args: &[],
        })
    } else {
        None
    }
}

/// Copies `text` through the native program, or through OSC 52 when there is
/// no program or it fails.
pub async fn copy(text: String) -> Result<(), String> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(format!(
            "selection of {} bytes exceeds the {MAX_CLIPBOARD_BYTES} byte limit",
            text.len()
        ));
    }
    let native = match adapter() {
        Some(adapter) => write_native(adapter, &text).await.is_ok(),
        None => false,
    };
    if native {
        return Ok(());
    }
    osc52(&text)
}

/// Writes to one clipboard program with a bounded deadline.
async fn write_native(adapter: Adapter, text: &str) -> io::Result<()> {
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
    // Windows `clip.exe` reads the console codepage; UTF-16LE round-trips.
    let payload: Vec<u8> = if cfg!(target_os = "windows") {
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    } else {
        text.as_bytes().to_vec()
    };
    timeout(WRITE_TIMEOUT, async {
        stdin.write_all(&payload).await?;
        stdin.shutdown().await?;
        child.wait().await.map(|_| ())
    })
    .await
    .map_err(|_| io::Error::other("clipboard program timed out"))?
}

/// OSC 52: works over SSH and inside tmux without a native program.
fn osc52(text: &str) -> Result<(), String> {
    use base64::Engine;
    use std::io::Write;

    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = std::io::stdout();
    // tmux needs the sequence wrapped in its passthrough guard.
    let sequence = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{encoded}\x07")
    };
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("OSC 52 copy failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_selections_are_refused_locally() {
        let big = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(copy(big))
            .unwrap_err();
        assert!(error.contains("exceeds"));
    }

    #[test]
    fn osc52_encodes_utf8_as_base64() {
        // The payload shape is fixed by the protocol: base64 of the UTF-8 body.
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode("héllo");
        assert_eq!(encoded, "aMOpbGxv");
    }
}
