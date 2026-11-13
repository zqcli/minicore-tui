use std::io;
use std::path::{Path, PathBuf};

/// The user's home directory, when the platform's variable names one.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The TUI directory contains initial configuration and separate Host data.
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
        let dir = flag
            .or(env)
            .unwrap_or_else(|| exe.parent().expect("an executable has a directory"));
        std::path::absolute(dir).map(Self)
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }

    pub fn config_file(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    pub fn host_config_file(&self) -> PathBuf {
        self.0.join("host-config.json")
    }

    pub fn agent_defaults_file(&self) -> PathBuf {
        self.0.join("agent-defaults.json")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.0.join("prompt").join("commands")
    }

    /// Host owns this directory, independently of the old Agent's session store.
    pub fn data_root(&self) -> PathBuf {
        self.0.join("host")
    }

    pub fn history_file(&self) -> PathBuf {
        self.0.join("history.jsonl")
    }

    /// A path from a settings file: relative ones start at the TUI directory.
    pub fn join(&self, path: &Path) -> PathBuf {
        self.0.join(path)
    }

    /// Existing Agent settings require an explicit migration, never scaffolding over them.
    pub fn is_fresh(&self) -> bool {
        !self.config_file().exists()
            && !self.host_config_file().exists()
            && !self.0.join("models.toml").exists()
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
        assert_eq!(home.host_config_file(), Path::new("/h/host-config.json"));
        assert_eq!(
            home.agent_defaults_file(),
            Path::new("/h/agent-defaults.json")
        );
        assert_eq!(home.commands_dir(), Path::new("/h/prompt/commands"));
        assert_eq!(home.data_root(), Path::new("/h/host"));
        assert_eq!(home.join(Path::new("p/x.md")), Path::new("/h/p/x.md"));
        assert_eq!(home.join(Path::new("/abs.md")), Path::new("/abs.md"));
    }
}
