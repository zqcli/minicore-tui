//! External editor support: write the draft to a temp file, run the editor in
//! the foreground with the terminal restored, then read the file back.

use std::path::{Path, PathBuf};

/// Edits `draft` with the terminal handed to the editor: the alternate screen
/// and raw mode are released first, then retaken, so the editor gets a normal
/// screen and cooked input. The app keeps running; the main loop skips drawing
/// while this is in flight.
pub async fn edit_in_terminal(
    guard: &mut crate::terminal::Guard,
    command: &[String],
    draft: &str,
) -> Result<Option<String>, String> {
    guard
        .suspend()
        .map_err(|error| format!("cannot hand the terminal to the editor: {error}"))?;
    let result = edit(command, draft).await;
    // The editor may have written anything; always take the screen back.
    let resumed = guard.resume();
    match (result, resumed) {
        (Ok(text), Ok(())) => Ok(text),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(format!("cannot restore the terminal: {error}")),
    }
}

/// Writes `draft` to a temporary Markdown file, runs `command` (program plus
/// arguments) on it and returns the new contents. `Ok(None)` means the file
/// came back empty.
pub async fn edit(command: &[String], draft: &str) -> Result<Option<String>, String> {
    let Some((program, args)) = command.split_first() else {
        return Err("No editor configured: set `editor` in the TUI config or $EDITOR".into());
    };
    let path = temp_path();
    std::fs::write(&path, draft).map_err(|error| format!("{}: {error}", path.display()))?;
    let result = run(program, args, &path).await;
    let text = std::fs::read_to_string(&path).ok();
    let _ = std::fs::remove_file(&path);
    result?;
    Ok(text.filter(|text| !text.trim().is_empty()))
}

fn temp_path() -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("minicore-tui-draft-{unique}.md"))
}

async fn run(program: &str, args: &[String], path: &Path) -> Result<(), String> {
    tokio::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .await
        .map_err(|error| format!("{program}: {error}"))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("{program} exited with {status}"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_empty_command_is_reported_not_ignored() {
        let error = edit(&[], "draft").await.unwrap_err();
        assert!(error.contains("editor"), "{error}");
    }

    #[tokio::test]
    async fn the_edited_file_comes_back_as_the_draft() {
        // `cat` rewrites the file with itself: the draft must survive verbatim.
        let text = edit(&["cat".to_owned()], "line one\nline two\n")
            .await
            .unwrap()
            .expect("a non-empty file is a draft");
        assert_eq!(text, "line one\nline two\n");
    }

    #[tokio::test]
    async fn an_editor_that_writes_nothing_returns_no_draft() {
        // `true` leaves the file empty of a write and never touches it; the
        // file is still the original draft, so this is the unchanged case.
        let text = edit(&["true".to_owned()], "keep me").await.unwrap();
        assert_eq!(text.as_deref(), Some("keep me"));
    }

    #[tokio::test]
    async fn a_failing_editor_is_an_error_not_a_silent_draft() {
        let error = edit(&["false".to_owned()], "draft").await.unwrap_err();
        assert!(error.contains("exited with"), "{error}");
    }

    #[test]
    fn temp_paths_are_unique_per_call() {
        assert_ne!(temp_path(), temp_path());
    }
}
