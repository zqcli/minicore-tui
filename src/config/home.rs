use std::io;
use std::path::{Path, PathBuf};

/// The TUI directory: settings, prompts, sessions and history all live here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home(pub(super) PathBuf);

impl Home {
    /// `--home`, else `$MINICORE_HOME`, else the directory of the running executable.
    pub fn resolve(flag: Option<&Path>) -> Result<Self, String> {
        let env = std::env::var_os("MINICORE_HOME").filter(|value| !value.is_empty());
        let exe = std::env::current_exe().map_err(|error| {
            format!(
                "cannot locate the running executable ({error}); pass --home or set MINICORE_HOME"
            )
        })?;
        Self::pick(flag, env.as_deref().map(Path::new), &exe)
            .map_err(|error| format!("cannot resolve the TUI directory: {error}"))
    }

    fn pick(flag: Option<&Path>, env: Option<&Path>, exe: &Path) -> io::Result<Self> {
        let dir = flag.or(env).unwrap_or_else(|| exe.parent().unwrap_or(exe));
        std::path::absolute(dir).map(Self)
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }

    pub fn config_file(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    pub fn models_file(&self) -> PathBuf {
        self.0.join("models.toml")
    }

    pub fn system_prompt_file(&self) -> PathBuf {
        self.0.join("prompt").join("SYSTEM_PROMPT.md")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.0.join("prompt").join("commands")
    }

    /// The Agent's `data_dir`: every session and Agent store file.
    pub fn session_dir(&self) -> PathBuf {
        self.0.join("session")
    }

    /// The generated Agent config, rewritten on every start.
    pub fn agent_config_file(&self) -> PathBuf {
        self.session_dir().join("agent.toml")
    }

    /// Prompt history is the composer's, not the Agent's, so it stays out of
    /// `session/`, which can be wiped without losing it.
    pub fn history_file(&self) -> PathBuf {
        self.0.join("history.jsonl")
    }

    /// A path from a settings file: relative ones start at the TUI directory.
    pub fn join(&self, path: &Path) -> PathBuf {
        self.0.join(path)
    }

    /// True when neither settings file exists, which means a first run.
    pub fn is_fresh(&self) -> bool {
        !self.config_file().exists() && !self.models_file().exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(flag: Option<&str>, env: Option<&str>, exe: &str) -> PathBuf {
        Home::pick(flag.map(Path::new), env.map(Path::new), Path::new(exe))
            .unwrap()
            .0
    }

    /// `path` made absolute the way `pick` does, so Windows gains its drive.
    fn absolute(path: &str) -> PathBuf {
        std::path::absolute(path).unwrap()
    }

    #[test]
    fn the_flag_beats_the_environment_which_beats_the_executable() {
        let exe = "/opt/mct/bin/minicore-tui";
        assert_eq!(pick(Some("/a"), Some("/b"), exe), absolute("/a"));
        assert_eq!(pick(None, Some("/b"), exe), absolute("/b"));
        assert_eq!(pick(None, None, exe), absolute("/opt/mct/bin"));
    }

    #[test]
    fn a_relative_flag_becomes_absolute() {
        assert!(pick(Some("rel/home"), None, "/x/minicore-tui").is_absolute());
    }

    #[test]
    fn every_path_sits_under_the_directory_with_the_documented_names() {
        let home = Home(PathBuf::from("/h"));
        assert_eq!(home.config_file(), Path::new("/h/config.toml"));
        assert_eq!(home.models_file(), Path::new("/h/models.toml"));
        assert_eq!(
            home.system_prompt_file(),
            Path::new("/h/prompt/SYSTEM_PROMPT.md")
        );
        assert_eq!(home.commands_dir(), Path::new("/h/prompt/commands"));
        assert_eq!(home.agent_config_file(), Path::new("/h/session/agent.toml"));
        assert_eq!(home.join(Path::new("p/x.md")), Path::new("/h/p/x.md"));
        assert_eq!(home.join(Path::new("/abs.md")), Path::new("/abs.md"));
    }
}
