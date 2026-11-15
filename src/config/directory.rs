use std::path::{Path, PathBuf};

/// The current user's native home directory.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir()
}

/// One resolved configuration directory shared with the embedded Host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigDirectory {
    directory: PathBuf,
    data_root: PathBuf,
}

impl ConfigDirectory {
    pub fn resolve(config_dir: Option<&Path>, data_root: Option<&Path>) -> Result<Self, String> {
        let paths = minicore_host_config::resolve_config_paths(config_dir, data_root)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            directory: paths.config_dir,
            data_root: paths.data_root,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.directory
    }

    pub fn ui_file(&self) -> PathBuf {
        self.directory.join("tui.json")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.directory.join("commands")
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn history_file(&self) -> PathBuf {
        self.data_root.join("tui").join("history.jsonl")
    }

    pub(super) fn is_fresh(&self) -> bool {
        !self.directory.join("host.json").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_preferences_and_templates_use_config_while_recall_follows_data_root() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let data = temp.path().join("store");
        let directory = ConfigDirectory::resolve(Some(&config), Some(&data)).unwrap();
        assert_eq!(directory.dir(), config);
        assert_eq!(directory.ui_file(), config.join("tui.json"));
        assert_eq!(directory.commands_dir(), config.join("commands"));
        assert_eq!(directory.data_root(), data);
        assert_eq!(
            directory.history_file(),
            data.join("tui").join("history.jsonl")
        );
    }

    #[test]
    fn missing_host_is_fresh_even_when_agent_and_role_directories_exist() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        std::fs::create_dir_all(config.join("agents")).unwrap();
        std::fs::create_dir_all(config.join("roles")).unwrap();
        let directory = ConfigDirectory::resolve(Some(&config), None).unwrap();
        assert!(directory.is_fresh());
        std::fs::write(config.join("host.json"), "{}").unwrap();
        assert!(!directory.is_fresh());
    }

    #[test]
    fn data_root_default_is_independent_of_the_config_override() {
        let directory = ConfigDirectory::resolve(Some(Path::new("relative-config")), None).unwrap();
        assert!(directory.dir().is_absolute());
        assert_eq!(
            directory.data_root(),
            std::env::home_dir().unwrap().join(".minicore/data")
        );
    }
}
