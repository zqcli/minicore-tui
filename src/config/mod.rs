//! Terminal preferences and initial native Host/Agent configuration.

mod home;
mod scaffold;
mod settings;

use std::path::{Path, PathBuf};

use minicore_host_core::HostConfig;
use minicore_host_protocol::AgentConfig;
use serde::de::DeserializeOwned;

pub use home::{Home, home_dir};
pub use settings::{Settings, ThemeKind, Ui, editor_command, save_theme};

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
    fn at(file: &Path, message: impl Into<String>) -> Self {
        Self {
            file: file.to_owned(),
            line: None,
            message: message.into(),
        }
    }

    fn io(file: &Path, error: &std::io::Error) -> Self {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "not found".to_owned()
        } else {
            error.to_string()
        };
        Self::at(file, message)
    }
}

fn read_json<T: DeserializeOwned>(file: &Path) -> Result<T, ConfigError> {
    let text = std::fs::read_to_string(file).map_err(|error| ConfigError::io(file, &error))?;
    serde_json::from_str(&text).map_err(|error| ConfigError {
        file: file.to_owned(),
        line: Some(error.line()),
        message: error.to_string(),
    })
}

pub fn load_host_config(file: &Path) -> Result<HostConfig, ConfigError> {
    read_json(file)
}

pub fn load_new_agent(home: &Home) -> Result<AgentConfig, ConfigError> {
    let file = home.agent_defaults_file();
    if !file.exists() {
        return Ok(AgentConfig::default());
    }
    read_json(&file)
}

pub fn load_ui(home: &Home) -> Result<Ui, ConfigError> {
    let file = home.config_file();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ui::default()),
        Err(error) => return Err(ConfigError::io(&file, &error)),
    };
    toml::from_str::<Settings>(&text)
        .map(|settings| settings.ui)
        .map_err(|error| ConfigError {
            file,
            line: error
                .span()
                .map(|span| text[..span.start].matches('\n').count() + 1),
            message: format!(
                "{}; config.toml now contains only [ui]; use native host-config.json and agent-defaults.json for execution settings (the old Agent store is not migrated)",
                error.message()
            ),
        })
}

#[derive(Default)]
pub struct Request<'a> {
    pub data_root: Option<&'a Path>,
    pub host_config: Option<&'a Path>,
}

pub struct Launch {
    pub ui: Ui,
    pub data_root: PathBuf,
    pub host_config_file: PathBuf,
    pub initial_config: HostConfig,
    /// Applied only when a new Agent is explicitly created.
    pub new_agent: AgentConfig,
}

pub enum Prepared {
    FirstRun(String),
    Launch(Box<Launch>),
}

/// Loads startup inputs without reading or rewriting the Host's owned records.
pub fn prepare(home: &Home, request: &Request<'_>) -> Result<Prepared, String> {
    let data_root = match request.data_root {
        Some(path) => std::path::absolute(path).map_err(|error| error.to_string())?,
        None => home.data_root(),
    };
    let host_config_file = request
        .host_config
        .map(Path::to_owned)
        .unwrap_or_else(|| home.host_config_file());
    let saved_host = data_root.join("host.json").exists();
    if request.host_config.is_none() && home.is_fresh() && !saved_host {
        let created = scaffold::write(home).map_err(|error| error.to_string())?;
        return Ok(Prepared::FirstRun(scaffold::first_run_message(
            home, &created,
        )));
    }
    let initial_config = if host_config_file.exists() || request.host_config.is_some() {
        load_host_config(&host_config_file).map_err(|error| error.to_string())?
    } else if saved_host {
        HostConfig::default()
    } else {
        return Err(format!(
            "{}: not found; supply a native HostConfig with --host-config (the old Agent store is not migrated)",
            host_config_file.display()
        ));
    };
    Ok(Prepared::Launch(Box::new(Launch {
        ui: load_ui(home).map_err(|error| error.to_string())?,
        data_root,
        host_config_file,
        initial_config,
        new_agent: load_new_agent(home).map_err(|error| error.to_string())?,
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
