//! External editor support: write the draft to a temp file, run the editor with
//! the real terminal, then read the file back.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A draft larger than this is not handed to an editor.
const MAX_DRAFT_BYTES: usize = 1 << 20;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Edits `draft` with the terminal handed to the editor: the alternate screen
/// and raw mode are released first, then retaken, so the editor gets a normal
/// screen and cooked input. The app keeps running; the main loop skips drawing
/// while this is in flight.
pub async fn edit_in_terminal(
    guard: &mut crate::terminal::Guard,
    command: &[String],
    draft: &str,
) -> Result<String, String> {
    guard
        .suspend()
        .map_err(|error| format!("cannot hand the terminal to the editor: {error}"))?;
    edit_and_resume(command, draft, || guard.resume()).await
}

/// Writes `draft` to a temporary Markdown file, runs `command` (program plus
/// arguments) on it and returns the new contents. Failed edits retain the file.
pub async fn edit(command: &[String], draft: &str) -> Result<String, String> {
    edit_and_resume(command, draft, || Ok(())).await
}

async fn edit_and_resume(
    command: &[String],
    draft: &str,
    resume: impl FnOnce() -> std::io::Result<()>,
) -> Result<String, String> {
    let result = edit_file(command, draft).await;
    // The editor may have written anything; always take the screen back.
    let resumed = resume();
    match (result, resumed) {
        (Ok((text, path)), Ok(())) => {
            let _ = std::fs::remove_file(path);
            Ok(text)
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(restore)) => {
            Err(format!("{error}; cannot restore the terminal: {restore}"))
        }
        (Ok((_, path)), Err(error)) => Err(format!(
            "cannot restore the terminal: {error}; edited draft file: {}",
            path.display()
        )),
    }
}

async fn edit_file(command: &[String], draft: &str) -> Result<(String, PathBuf), String> {
    let (program, args) = command
        .split_first()
        .ok_or("no editor is configured: set `editor` in the TUI config or $VISUAL/$EDITOR")?;
    if draft.len() > MAX_DRAFT_BYTES {
        return Err(format!("draft is {} bytes, too large to edit", draft.len()));
    }
    let path = temp_path();
    std::fs::write(&path, draft).map_err(|error| format!("{}: {error}", path.display()))?;
    run(program, args, &path)
        .await
        .map_err(|error| format!("{error}; draft file: {}", path.display()))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read edited draft {}: {error}", path.display()))?;
    Ok((text, path))
}

fn temp_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "minicore-tui-draft-{}-{nanos}-{sequence}.md",
        std::process::id()
    ))
}

async fn run(program: &str, args: &[String], path: &Path) -> Result<(), String> {
    let status = tokio::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .await
        .map_err(|error| format!("{program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restore_failure_retains_edited_file_and_reports_its_path() {
        let directory = tempfile::tempdir().unwrap();
        let recorded = directory.path().join("draft-path");
        let command = [
            "sh".into(),
            "-c".into(),
            r#"printf 'new 中文\nsecond line\n' > "$2"; printf '%s' "$2" > "$1""#.into(),
            "sh".into(),
            recorded.to_str().unwrap().into(),
        ];
        let error = edit_and_resume(&command, "old draft", || {
            Err(std::io::Error::other("injected restore failure"))
        })
        .await
        .unwrap_err();
        let path = std::fs::read_to_string(recorded).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).expect("edited file must survive"),
            "new 中文\nsecond line\n"
        );
        assert!(error.contains(&path), "{error}");
        assert!(error.contains("injected restore failure"), "{error}");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn an_empty_command_names_the_config_and_the_environment() {
        let error = edit(&[], "draft").await.unwrap_err();
        assert!(error.contains("editor"), "{error}");
        assert!(error.contains("VISUAL"), "{error}");
    }

    #[tokio::test]
    async fn a_missing_program_is_reported() {
        let error = edit(&["definitely-not-an-editor".into()], "draft")
            .await
            .unwrap_err();
        assert!(error.contains("definitely-not-an-editor"), "{error}");
    }

    #[tokio::test]
    async fn the_edited_file_comes_back_as_the_draft() {
        // `cat` rewrites the file with itself: the draft must survive verbatim.
        let text = edit(&["cat".to_owned()], "line one\nline two\n")
            .await
            .unwrap();
        assert_eq!(text, "line one\nline two\n");
    }

    #[tokio::test]
    async fn the_edited_text_comes_back_and_the_temp_file_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let recorded = directory.path().join("draft-path");
        let command = [
            "sh".into(),
            "-c".into(),
            r#"printf 'edited' > "$2"; printf '%s' "$2" > "$1""#.into(),
            "sh".into(),
            recorded.to_str().unwrap().into(),
        ];
        let text = edit_and_resume(&command, "draft", || {
            let path = std::fs::read_to_string(&recorded).unwrap();
            assert_eq!(std::fs::read_to_string(path).unwrap(), "edited");
            Ok(())
        })
        .await
        .expect("editor runs");
        assert_eq!(text, "edited");
        let path = std::fs::read_to_string(recorded).unwrap();
        assert!(!Path::new(&path).exists());
    }

    #[tokio::test]
    async fn an_editor_that_writes_nothing_keeps_the_draft() {
        let text = edit(&["true".to_owned()], "keep me").await.unwrap();
        assert_eq!(text, "keep me");
    }

    #[tokio::test]
    async fn clearing_a_draft_is_distinct_from_removing_its_file() {
        for (script, cleared) in [(r#"> "$1""#, true), (r#"rm -- "$1""#, false)] {
            let result = edit(
                &["sh".into(), "-c".into(), script.into(), "sh".into()],
                "draft",
            )
            .await;
            if cleared {
                assert_eq!(result.unwrap(), "");
            } else {
                assert!(result.unwrap_err().contains("cannot read edited draft"));
            }
        }
    }

    #[tokio::test]
    async fn unreadable_edited_content_is_retained_for_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let recorded = directory.path().join("draft-path");
        let command = [
            "sh".into(),
            "-c".into(),
            r#"printf '\377' > "$2"; printf '%s' "$2" > "$1""#.into(),
            "sh".into(),
            recorded.to_str().unwrap().into(),
        ];
        let error = edit(&command, "original draft").await.unwrap_err();
        let path = std::fs::read_to_string(recorded).unwrap();
        assert!(error.contains(&path));
        assert_eq!(std::fs::read(&path).unwrap(), [255]);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn editor_and_restore_failures_are_both_reported_and_keep_the_file() {
        let error = edit_and_resume(&["false".into()], "keep me", || {
            Err(std::io::Error::other("injected restore failure"))
        })
        .await
        .unwrap_err();
        assert!(error.contains("exited with"), "{error}");
        assert!(error.contains("injected restore failure"), "{error}");
        let path = error
            .split("draft file: ")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep me");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn a_failing_editor_is_an_error_not_a_silent_draft() {
        let error = edit(&["false".to_owned()], "draft").await.unwrap_err();
        assert!(error.contains("exited with"), "{error}");
    }

    #[tokio::test]
    async fn an_oversized_draft_is_refused_without_spawning() {
        let command = vec!["sh".to_owned()];
        let error = edit(&command, &"x".repeat(MAX_DRAFT_BYTES + 1))
            .await
            .unwrap_err();
        assert!(error.contains("too large"), "{error}");
    }

    #[test]
    fn temp_paths_are_unique_per_call() {
        assert_ne!(temp_path(), temp_path());
    }
}
