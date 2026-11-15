use std::{io::IsTerminal, path::Path, time::Duration};

use minicore_host_core::{LogLevel, LogOutput, LogSettings};
use minicore_host_diagnostics::{Guard, Health, Options, Output};
use tracing::Level;

pub(super) struct Diagnostics {
    guard: Guard,
}

impl Diagnostics {
    pub(super) fn install(data_root: &Path, settings: LogSettings) -> Result<Self, &'static str> {
        if matches!(&settings.output, LogOutput::Stderr) && std::io::stderr().is_terminal() {
            return Err(
                "diagnostic stderr is connected to the terminal; configure an absolute log file or redirect stderr before starting the full-screen UI",
            );
        }
        validate_output(data_root, &settings.output)?;
        let output = match settings.output {
            LogOutput::Stderr => Output::Stderr,
            LogOutput::File { path } => Output::File(path.into()),
        };
        let options = Options {
            service: "minicore-tui",
            service_version: Some(env!("CARGO_PKG_VERSION")),
            level: match settings.level {
                LogLevel::Error => Level::ERROR,
                LogLevel::Warn => Level::WARN,
                LogLevel::Info => Level::INFO,
                LogLevel::Debug => Level::DEBUG,
                LogLevel::Trace => Level::TRACE,
            },
            output,
        };
        let guard = minicore_host_diagnostics::install(options)
            .map_err(|_| "cannot initialize configured diagnostics")?;
        Ok(Self { guard })
    }

    pub(super) fn health(&self) -> Health {
        self.guard.health()
    }

    pub(super) fn finish(self) {
        // This bounds diagnostic flushing, never accepted work or persistence.
        let _ = self.guard.shutdown(Duration::from_secs(1));
    }
}

fn validate_output(data_root: &Path, output: &LogOutput) -> Result<(), &'static str> {
    let LogOutput::File { path } = output else {
        return Ok(());
    };
    let root = std::fs::canonicalize(data_root).map_err(|_| "cannot resolve Host data root")?;
    let path = Path::new(path);
    let resolved = match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::canonicalize(path).map_err(|_| "cannot resolve diagnostic output")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or("cannot resolve diagnostic output parent")?;
            let name = path
                .file_name()
                .ok_or("cannot resolve diagnostic output file")?;
            std::fs::canonicalize(parent)
                .map_err(|_| "cannot resolve diagnostic output parent")?
                .join(name)
        }
        Err(_) => return Err("cannot inspect diagnostic output"),
    };
    // Diagnostics may never append into Host metadata or Session history.
    if resolved == root.join("host.json")
        || resolved.starts_with(root.join("agents"))
        || resolved.starts_with(root.join("sessions"))
    {
        return Err("diagnostic output conflicts with Host business storage");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_append_cannot_target_host_records_or_session_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("agents")).unwrap();
        std::fs::create_dir_all(root.join("sessions").join("s")).unwrap();
        for path in [
            root.join("host.json"),
            root.join("agents/a.json"),
            root.join("sessions/s/history.jsonl"),
        ] {
            assert_eq!(
                validate_output(
                    root,
                    &LogOutput::File {
                        path: path.to_string_lossy().into()
                    }
                ),
                Err("diagnostic output conflicts with Host business storage")
            );
        }
        assert!(
            validate_output(
                root,
                &LogOutput::File {
                    path: root.join("diagnostics.jsonl").to_string_lossy().into()
                }
            )
            .is_ok()
        );
    }
}
