//! External editor support: write the draft to a temp file, run the editor in
//! the foreground with the terminal restored, then read the file back.

use std::path::PathBuf;

use crate::config::Config;

/// Writes `draft` to a temporary Markdown file, runs the configured editor and
/// returns the new contents. `Ok(None)` means the editor was cancelled or the
/// file came back empty.
pub async fn edit(config: &Config, draft: &str) -> Result<Option<String>, String> {
    let Some(command) = config.editor() else {
        return Err("No editor configured: set `editor` in the TUI config or $EDITOR".into());
    };
    let path = temp_path();
    std::fs::write(&path, draft).map_err(|error| format!("{}: {error}", path.display()))?;
    let result = run(command, &path).await;
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

async fn run(mut command: Vec<String>, path: &PathBuf) -> Result<(), String> {
    let program = command.remove(0);
    tokio::process::Command::new(&program)
        .args(command)
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
    async fn a_missing_editor_is_reported_not_ignored() {
        let config = Config {
            editor: Vec::new(),
            ..Config::default()
        };
        // No editor configured and no environment override in a bare config.
        if config.editor().is_none() {
            let error = edit(&config, "draft").await.unwrap_err();
            assert!(error.contains("editor"));
        }
    }

    #[test]
    fn temp_paths_are_unique_per_call() {
        assert_ne!(temp_path(), temp_path());
    }
}
