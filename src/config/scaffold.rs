//! Host owns its sample schema; this module adds terminal preferences.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{ConfigDirectory, Settings};

pub(super) fn write(
    directory: &ConfigDirectory,
    workspace: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    let mut created = minicore_host_config::create_config_directory(directory.dir(), workspace)
        .map_err(|error| error.to_string())?;
    let text =
        serde_json::to_string_pretty(&Settings::default()).map_err(|error| error.to_string())?;
    let path = directory.ui_file();
    if create_new(&path, &format!("{text}\n"))
        .map_err(|error| format!("{}: {error}", path.display()))?
    {
        created.push(path);
    }
    std::fs::create_dir_all(directory.commands_dir()).map_err(|error| error.to_string())?;
    Ok(created)
}

fn create_new(path: &Path, text: &str) -> io::Result<bool> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn first_run_message(directory: &ConfigDirectory, created: &[PathBuf]) -> String {
    let files = created
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Created initial JSON configuration:\n{files}\n\n\
Edit {} with your provider's full endpoint, model and declared capabilities.\n\
Set api to chat_completions or responses; the Host never guesses or falls back.\n\
api_key_env names an environment variable: export the key there, never in JSON.\n\
Edit agents/*.json for named Agents and Session defaults; roles/*.json holds reusable Roles.\n\
The sample Agent uses the workspace selected during initialization.\n\
Host loads this directory on every start and creates or updates declared Agents.\n\
Existing Sessions keep their own configuration. Use /reload to apply later file changes.\n\
Run minicore-tui again when ready. Runtime data is stored in {}.",
        directory.dir().join("host.json").display(),
        directory.data_root().display()
    )
}
