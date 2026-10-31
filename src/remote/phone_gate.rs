//! Acceptance-only pausing of one real WebSocket's outbound drain.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

pub const WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Identity {
    pub connection_id: u64,
    pub view_epoch: String,
    pub session_id: Option<String>,
    pub load: Option<String>,
}

impl Identity {
    pub fn from_snapshot(connection_id: u64, snapshot: &Value) -> Result<Self, String> {
        let view_epoch = snapshot["view_epoch"]
            .as_str()
            .ok_or("phone snapshot lacks view identity")?
            .to_owned();
        let session = &snapshot["state"]["session"];
        let (session_id, load) = if session.is_null() {
            (None, None)
        } else {
            (
                Some(
                    session["id"]
                        .as_str()
                        .ok_or("phone snapshot lacks session identity")?
                        .to_owned(),
                ),
                Some(
                    session["load"]
                        .as_str()
                        .ok_or("phone snapshot lacks load identity")?
                        .to_owned(),
                ),
            )
        };
        Ok(Self {
            connection_id,
            view_epoch,
            session_id,
            load,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Paused,
    Resumed,
    OverflowClosed,
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub identity: Identity,
    pub phase: Phase,
}

impl Observation {
    pub fn event(&self) -> &'static str {
        match self.phase {
            Phase::Paused => "phone_send_paused",
            Phase::Resumed => "phone_send_resumed",
            Phase::OverflowClosed => "phone_overflow_closed",
        }
    }

    pub fn row(&self) -> Value {
        let mut row = serde_json::to_value(&self.identity).unwrap();
        if self.phase == Phase::OverflowClosed {
            row["reason"] = json!("overflow");
        }
        row
    }
}

pub struct Gate {
    directory: PathBuf,
    claimed: AtomicBool,
}

impl Gate {
    pub fn from_env() -> Result<Option<Arc<Self>>, String> {
        let Some(directory) = std::env::var_os("MINICORE_E2E_PHONE_GATE") else {
            return Ok(None);
        };
        if std::env::var_os("MINICORE_E2E_TRACE").is_none() {
            return Err("phone gate requires MINICORE_E2E_TRACE".into());
        }
        Self::new(PathBuf::from(directory)).map(|gate| Some(Arc::new(gate)))
    }

    pub(super) fn new(directory: PathBuf) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            return Err("phone gate requires a private directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err("phone gate directory must be private".into());
            }
        }
        for name in ["ready", "ready.tmp", "release"] {
            if directory
                .join(name)
                .try_exists()
                .map_err(|error| error.to_string())?
            {
                return Err(format!("phone gate has stale {name}"));
            }
        }
        Ok(Self {
            directory,
            claimed: AtomicBool::new(false),
        })
    }

    pub fn claim(&self) -> Result<bool, String> {
        if self.claimed.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let arm = self.directory.join("arm");
        let metadata = match std::fs::symlink_metadata(&arm) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.is_file() || metadata.len() != 0 {
            return Err("phone gate arm must be an empty file".into());
        }
        match std::fs::remove_file(arm) {
            Ok(()) => {
                self.claimed.store(true, Ordering::SeqCst);
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn ready(&self, identity: &Identity) -> Result<(), String> {
        for name in ["ready", "release"] {
            if self
                .directory
                .join(name)
                .try_exists()
                .map_err(|error| error.to_string())?
            {
                return Err(format!("phone gate has stale {name}"));
            }
        }
        let temporary = self.directory.join("ready.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut file, identity).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(temporary, self.directory.join("ready")).map_err(|error| error.to_string())
    }

    pub async fn wait_release(&self, deadline: tokio::time::Instant) -> Result<(), String> {
        tokio::time::timeout_at(deadline, async {
            loop {
                let release = self.directory.join("release");
                match std::fs::symlink_metadata(&release) {
                    Ok(metadata) => {
                        if !metadata.is_file() || metadata.len() != 0 {
                            return Err("phone gate release must be an empty file".into());
                        }
                        std::fs::remove_file(release).map_err(|error| error.to_string())?;
                        return Ok(());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "phone gate release timed out after 30 seconds".to_owned())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        directory
    }

    #[tokio::test(start_paused = true)]
    async fn the_private_one_shot_gate_publishes_exact_identity_and_consumes_release() {
        let directory = private_directory();
        let gate = Gate::new(directory.path().into()).unwrap();
        assert!(!gate.claim().unwrap());
        std::fs::write(directory.path().join("arm"), "").unwrap();
        assert!(gate.claim().unwrap());
        assert!(!directory.path().join("arm").exists());
        let identity = Identity::from_snapshot(
            19,
            &json!({"view_epoch":"view_19_0", "state":{"session":{"id":"s1", "load":"load_2"}}}),
        )
        .unwrap();
        gate.ready(&identity).unwrap();
        assert!(!directory.path().join("ready.tmp").exists());
        let ready: Value =
            serde_json::from_slice(&std::fs::read(directory.path().join("ready")).unwrap())
                .unwrap();
        assert_eq!(
            ready,
            json!({"connection_id":19, "view_epoch":"view_19_0", "session_id":"s1", "load":"load_2"})
        );
        std::fs::write(directory.path().join("release"), "").unwrap();
        gate.wait_release(tokio::time::Instant::now() + WAIT)
            .await
            .unwrap();
        assert!(!directory.path().join("release").exists());
        std::fs::write(directory.path().join("arm"), "").unwrap();
        assert!(!gate.claim().unwrap(), "reconnect cannot claim a used gate");
    }

    #[tokio::test(start_paused = true)]
    async fn gate_timeout_is_explicit_and_stale_files_never_resume_a_new_run() {
        let directory = private_directory();
        let gate = Gate::new(directory.path().into()).unwrap();
        assert!(
            gate.wait_release(tokio::time::Instant::now() + WAIT)
                .await
                .unwrap_err()
                .contains("timed out")
        );
        for name in ["ready", "ready.tmp", "release"] {
            std::fs::write(directory.path().join(name), "").unwrap();
            assert!(Gate::new(directory.path().into()).is_err(), "{name}");
            std::fs::remove_file(directory.path().join(name)).unwrap();
        }
    }
}
