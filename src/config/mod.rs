//! Configuration. The TUI directory holds `config.toml` (system settings) and
//! `models.toml` (providers and models); startup validates both and composes the
//! one TOML file the Agent reads. No credentials are stored, only the name of the
//! environment variable that holds each key.

mod compose;
mod home;
mod models;
mod remote;
mod scaffold;
mod settings;

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use compose::AgentFile;
pub use home::Home;
pub use remote::{Remote, Tls};
pub use settings::{Settings, ThemeKind, Ui, editor_command, save_theme};

/// A configuration problem with the file, and the line when the parser knew it.
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

impl ConfigError {
    fn io(file: &Path, error: &std::io::Error) -> Self {
        let message = if error.kind() == std::io::ErrorKind::NotFound {
            "not found".to_owned()
        } else {
            error.to_string()
        };
        Self {
            file: file.to_owned(),
            line: None,
            message,
        }
    }
}

/// A settings file and its text, so errors can name a line.
struct Source {
    file: PathBuf,
    text: String,
}

impl Source {
    fn read(file: PathBuf) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(&file) {
            Ok(text) => Ok(Self { file, text }),
            Err(error) => Err(ConfigError::io(&file, &error)),
        }
    }

    fn error(&self, span: Option<Range<usize>>, message: impl Into<String>) -> ConfigError {
        let line = span.map(|span| self.text[..span.start].matches('\n').count() + 1);
        ConfigError {
            file: self.file.clone(),
            line,
            message: message.into(),
        }
    }

    fn parse<T: DeserializeOwned>(&self) -> Result<T, ConfigError> {
        toml::from_str(&self.text).map_err(|error| self.error(error.span(), error.message()))
    }
}

/// Both files read, checked against each other and turned into Agent terms.
pub struct Loaded {
    pub ui: Ui,
    pub remote: Remote,
    binary: Option<PathBuf>,
    agent: AgentFile,
}

/// Reads and validates `config.toml`, `models.toml` and the prompt files.
pub fn load(home: &Home) -> Result<Loaded, ConfigError> {
    let models_source = Source::read(home.models_file())?;
    let models = models_source.parse::<models::ModelsFile>()?;
    let models = models.build(&models_source)?;
    let settings_source = Source::read(home.config_file())?;
    let settings = settings_source.parse::<Settings>()?;
    let agent = settings.build(&settings_source, models, home)?;
    let remote = settings.remote.build(&settings_source)?;
    Ok(Loaded {
        remote,
        ui: settings.ui,
        binary: settings.agent.binary,
        agent,
    })
}

impl Loaded {
    /// Writes the Agent config under `session/` and returns its path.
    pub fn write(&self, home: &Home) -> Result<PathBuf, ConfigError> {
        let file = home.agent_config_file();
        let text = self
            .agent
            .render()
            .map_err(|message| ConfigError::io(&file, &std::io::Error::other(message)))?;
        std::fs::create_dir_all(home.session_dir())
            .and_then(|()| write_atomic(&file, &text, 0o600))
            .map_err(|error| ConfigError::io(&file, &error))?;
        Ok(file)
    }

    /// Catches what would only fail at the first turn: a profile or model
    /// named on the command line that does not exist, or a key that is not set.
    fn check_launch(
        &self,
        profile: Option<&str>,
        model: Option<&str>,
        has_env: &dyn Fn(&str) -> bool,
    ) -> Result<(), String> {
        let defined = |ids: Vec<&String>| {
            ids.into_iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let profile_id = profile.unwrap_or(&self.agent.default_profile);
        let Some(entry) = self.agent.profiles.get(profile_id) else {
            return Err(format!(
                "--profile {profile_id:?} is not defined in config.toml (defined: {})",
                defined(self.agent.profiles.keys().collect())
            ));
        };
        let model_id = model.unwrap_or(&entry.model);
        let Some(entry) = self.agent.models.get(model_id) else {
            return Err(format!(
                "--model {model_id:?} is not defined in models.toml (defined: {})",
                defined(self.agent.models.keys().collect())
            ));
        };
        if !has_env(&entry.api_key_env) {
            return Err(format!(
                "environment variable {} is not set or empty (api_key_env of model {model_id:?} in models.toml); export it and start again",
                entry.api_key_env
            ));
        }
        Ok(())
    }
}

/// What the command line asks of startup.
#[derive(Default)]
pub struct Request<'a> {
    pub agent: Option<&'a Path>,
    pub agent_config: Option<&'a Path>,
    pub profile: Option<&'a str>,
    pub model: Option<&'a str>,
}

pub struct Launch {
    pub ui: Ui,
    pub remote: Remote,
    pub agent_config: PathBuf,
    /// An external Agent; `None` runs the one built into this executable.
    pub agent_binary: Option<PathBuf>,
    /// False when `--agent-config` named a raw file, so `/reload` cannot recompose.
    pub composed: bool,
    /// The profile a new session takes without `--profile`; unknown for a raw Agent config.
    pub default_profile: Option<String>,
}

pub enum Prepared {
    /// Nothing was configured: starter files were written, and this says where.
    FirstRun(String),
    Launch(Box<Launch>),
}

/// Everything before the terminal is touched: scaffold, validate, compose.
pub fn prepare(
    home: &Home,
    request: &Request,
    has_env: &dyn Fn(&str) -> bool,
) -> Result<Prepared, String> {
    if let Some(raw) = request.agent_config {
        let source = if home.config_file().exists() {
            Source::read(home.config_file()).map_err(|error| error.to_string())?
        } else {
            Source {
                file: home.config_file(),
                text: String::new(),
            }
        };
        let (ui, binary, section) = if source.text.is_empty() {
            (Ui::default(), None, remote::RemoteSection::default())
        } else {
            let settings = source
                .parse::<Settings>()
                .map_err(|error| error.to_string())?;
            (settings.ui, settings.agent.binary, settings.remote)
        };
        let remote = section.build(&source).map_err(|error| error.to_string())?;
        return Ok(Prepared::Launch(Box::new(Launch {
            remote,
            ui,
            agent_config: raw.to_owned(),
            agent_binary: agent_binary(home, request.agent, binary),
            composed: false,
            default_profile: None,
        })));
    }
    if home.is_fresh() {
        let created = scaffold::write(home).map_err(|error| error.to_string())?;
        return Ok(Prepared::FirstRun(scaffold::first_run_message(
            home, &created,
        )));
    }
    let loaded = load(home).map_err(|error| error.to_string())?;
    loaded.check_launch(request.profile, request.model, has_env)?;
    let agent_config = loaded.write(home).map_err(|error| error.to_string())?;
    Ok(Prepared::Launch(Box::new(Launch {
        agent_binary: agent_binary(home, request.agent, loaded.binary),
        ui: loaded.ui,
        remote: loaded.remote,
        agent_config,
        composed: true,
        default_profile: Some(loaded.agent.default_profile),
    })))
}

/// `--agent`, else `[agent] binary`, else none: the built-in Agent.
fn agent_binary(home: &Home, flag: Option<&Path>, configured: Option<PathBuf>) -> Option<PathBuf> {
    flag.map(Path::to_owned)
        .or_else(|| configured.map(|configured| home.join(&configured)))
}

/// Replaces `path` through a temporary file, so a crash leaves the old content
/// or the new one.
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
