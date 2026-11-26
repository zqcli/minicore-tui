//! Local terminal preferences and shared Host configuration paths.

mod directory;
mod scaffold;
mod settings;

use std::path::{Path, PathBuf};

pub use directory::{ConfigDirectory, home_dir};
pub use settings::{Remote, Settings, ThemeKind, Ui, editor_command, save_theme};

#[derive(Debug, PartialEq, Eq)]
pub struct ConfigError {
    file: PathBuf,
    line: Option<usize>,
    message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ConfigError {}

impl ConfigError {
    fn io(file: &Path, error: &std::io::Error) -> Self {
        Self {
            file: file.to_owned(),
            line: None,
            message: error.to_string(),
        }
    }
}

fn load_settings(file: &Path) -> Result<Settings, ConfigError> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(error) => return Err(ConfigError::io(file, &error)),
    };
    serde_json::from_str(&text).map_err(|error| ConfigError {
        file: file.to_owned(),
        line: Some(error.line()),
        message: error.to_string(),
    })
}

pub fn load_ui(directory: &ConfigDirectory) -> Result<Ui, ConfigError> {
    load_settings(&directory.ui_file()).map(|settings| settings.ui)
}

pub struct Launch {
    pub ui: Ui,
    pub remote: Remote,
    pub config_dir: PathBuf,
    pub data_root: PathBuf,
}

pub enum Prepared {
    FirstRun(String),
    Launch(Box<Launch>),
}

/// Reads only terminal preferences; Host owns loading business configuration.
pub fn prepare(directory: &ConfigDirectory, workspace: Option<&Path>) -> Result<Prepared, String> {
    let settings = load_settings(&directory.ui_file()).map_err(|error| error.to_string())?;
    if directory.is_fresh() {
        let created = scaffold::write(directory, workspace)?;
        return Ok(Prepared::FirstRun(scaffold::first_run_message(
            directory, &created,
        )));
    }
    Ok(Prepared::Launch(Box::new(Launch {
        ui: settings.ui,
        remote: settings.remote,
        config_dir: directory.dir().to_owned(),
        data_root: directory.data_root().to_owned(),
    })))
}

pub(crate) fn write_atomic(path: &Path, text: &str, mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(&temp)?.write_all(text.as_bytes())?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests;
