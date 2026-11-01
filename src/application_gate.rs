//! A single opt-in acceptance-build pause before main applies a backend message.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use minicore_tui::app::{App, Msg, SessionOp};
use minicore_tui::protocol::AgentEvent;
use serde_json::json;

pub struct ApplicationGate {
    directory: PathBuf,
    fired: bool,
}

impl ApplicationGate {
    pub fn from_env() -> io::Result<Option<Self>> {
        std::env::var_os("MINICORE_E2E_APPLY_GATE")
            .map(|path| Self::new(PathBuf::from(path)))
            .transpose()
    }

    pub fn new(directory: PathBuf) -> io::Result<Self> {
        if !directory.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "application gate directory does not exist",
            ));
        }
        if directory.join("ready").exists() || directory.join("release").exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "application gate directory contains stale ready/release files",
            ));
        }
        Ok(Self {
            directory,
            fired: false,
        })
    }

    pub async fn before_apply(&mut self, message: &Msg, app: &mut App) -> io::Result<()> {
        if self.fired {
            return Ok(());
        }
        let session = match message {
            Msg::Agent { event, .. } => event.meta().and_then(|meta| app.live(&meta.session_id)),
            Msg::Session(epoch, _) => app
                .session
                .iter()
                .chain(&app.background)
                .find(|session| session.epoch == *epoch),
            Msg::LocalPushed(id, _) => app
                .local_inputs
                .iter()
                .find(|input| input.id == *id)
                .and_then(|input| {
                    app.live(&input.target.session_id).filter(|session| {
                        input.target.epoch == session.epoch
                            && session.load.as_deref() == Some(input.target.load.as_str())
                    })
                }),
            _ => None,
        };
        let Some(session) = session else {
            return Ok(());
        };
        let Some(load) = session.load.clone() else {
            return Ok(());
        };
        let id = session.info.session_id.clone();
        match std::fs::remove_file(self.directory.join("arm")) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        self.fired = true;
        let kind = match message {
            Msg::Agent { event, .. } if matches!(**event, AgentEvent::SessionSnapshot { .. }) => {
                "session_snapshot"
            }
            Msg::Agent { .. } => "body_event",
            Msg::LocalPushed(..) => "push_receipt",
            Msg::Session(_, op) => match **op {
                SessionOp::Recovered(..) => "recovery_result",
                SessionOp::Waited(..) => "wait_result",
                SessionOp::State(..) => "state_result",
                _ => "session_result",
            },
            _ => unreachable!("only resident backend messages reach the gate"),
        };
        let temporary = self.directory.join("ready.tmp");
        let ready = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer(
            ready,
            &json!({"session_id":id,"load":load,"held_kind":kind}),
        )
        .map_err(io::Error::other)?;
        std::fs::rename(temporary, self.directory.join("ready"))?;
        app.observe_application_gate(false, &id, &load, kind);
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match std::fs::remove_file(self.directory.join("release")) {
                    Ok(()) => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "application pause was not released within 30 seconds",
            )
        })??;
        app.observe_application_gate(true, &id, &load, kind);
        Ok(())
    }
}
