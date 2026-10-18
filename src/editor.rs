//! External editor support: write the draft to a temp file, run the editor with
//! the real terminal, then read the file back.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A draft larger than this is not handed to an editor.
const MAX_DRAFT_BYTES: usize = 1 << 20;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Runs `command` (program plus arguments) on `draft` and returns the edited
/// text. `Ok(None)` means the file came back empty or unchanged in a way the
/// caller should keep as it was; the caller decides.
///
/// The caller restores the terminal before calling this and re-enters it after:
/// the editor needs cooked stdin and the normal screen.
pub async fn edit(command: &[String], draft: &str) -> Result<Option<String>, String> {
    let (program, args) = command
        .split_first()
        .ok_or("no editor is configured: set `editor` in the TUI config or $VISUAL/$EDITOR")?;
    if draft.len() > MAX_DRAFT_BYTES {
        return Err(format!("draft is {} bytes, too large to edit", draft.len()));
    }
    let path = temp_path();
    std::fs::write(&path, draft).map_err(|error| format!("{}: {error}", path.display()))?;
    let result = run(program, args, &path).await;
    let text = std::fs::read_to_string(&path).ok();
    let _ = std::fs::remove_file(&path);
    result?;
    Ok(text)
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
    async fn the_edited_text_comes_back_and_the_temp_file_is_removed() {
        // `sh -c` receives the path as its positional parameter.
        let command = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf 'edited' > \"$1\"".to_owned(),
            "sh".to_owned(),
        ];
        let text = edit(&command, "draft").await.expect("editor runs");
        assert_eq!(text.as_deref(), Some("edited"));
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
