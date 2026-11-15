//! The executable owns one persistent Host and its process diagnostics.

mod diagnostics;

use std::sync::Arc;

use minicore_host_core::{Host, HostProcessControl, HostProcessSnapshot};
use minicore_host_protocol::{
    Fault, FaultKind, HostLifecycle, HostShutdownReceipt, HostShutdownRequest,
    HostShutdownStrategy, OperationId,
};
use minicore_host_resources::HostResourceBuilder;
use minicore_host_transport::HostClient;

use crate::config::Launch;
use diagnostics::Diagnostics;

pub struct Hosted {
    _host: Arc<Host>,
    client: HostClient,
    control: HostProcessControl,
    diagnostics: Option<Diagnostics>,
}

impl Hosted {
    /// Lets Host load its bound directory before preparing production resources.
    pub async fn open(launch: &Launch) -> Result<Self, String> {
        let builder = HostResourceBuilder::builtins().map_err(startup_fault)?;
        let mut diagnostics = None;
        let mut preparation_error = None;
        let host = Host::open_directory_configured(
            &launch.data_root,
            &launch.config_dir,
            |config| async {
                if config.network.listen.is_some() {
                    let message = "this TUI embeds a local Host and does not start a network listener; network.listen must be null";
                    preparation_error = Some(message.to_owned());
                    return Err(Fault::new(FaultKind::HostConfigInvalid, message));
                }
                let data_root = launch.data_root.clone();
                let settings = config.logging.clone();
                let installed = tokio::task::spawn_blocking(move || {
                    Diagnostics::install(&data_root, settings)
                })
                .await
                .map_err(|_| "diagnostics initialization worker failed".to_owned())
                .and_then(|result| result.map_err(str::to_owned));
                match installed {
                    Ok(value) => diagnostics = Some(value),
                    Err(message) => {
                        preparation_error = Some(message.clone());
                        return Err(Fault::new(FaultKind::HostConfigInvalid, message));
                    }
                }
                builder.prepare(config).await
            },
        )
        .await
        .map_err(|fault| preparation_error.unwrap_or_else(|| startup_fault(fault)))?;
        if let Some(diagnostics) = diagnostics.as_ref() {
            let _ = host.bind_logging_health(diagnostics.health());
        }
        let control = host.process_control();
        let client = HostClient::in_process(Arc::clone(&host))
            .map_err(|_| "cannot establish the embedded Host connection".to_owned())?;
        Ok(Self {
            _host: host,
            client,
            control,
            diagnostics,
        })
    }

    pub fn client(&self) -> HostClient {
        self.client.clone()
    }

    pub fn process_control(&self) -> HostProcessControl {
        self.control.clone()
    }

    pub fn snapshot(&self) -> Result<HostProcessSnapshot, String> {
        self.control
            .snapshot()
            .map_err(|fault| fault.message().to_owned())
    }

    /// Requests the named strategy without changing it on a timeout or another request.
    pub fn request_shutdown(
        &self,
        strategy: HostShutdownStrategy,
    ) -> Result<HostShutdownReceipt, String> {
        let operation_id = OperationId::new().map_err(|error| error.to_string())?;
        self.control
            .shutdown(HostShutdownRequest {
                operation_id,
                strategy,
            })
            .map_err(|fault| fault.message().to_owned())
    }

    /// Waits for actual owner cleanup; paused work can keep graceful shutdown draining.
    pub async fn wait_stopped(&self) -> Result<(), String> {
        let mut current = self.snapshot()?;
        while current.lifecycle != HostLifecycle::Stopped {
            if let Some(fault) = &current.shutdown_fault {
                return Err(crate::wrap::sanitize(&format!(
                    "Host shutdown is blocked: {fault}; cleanup is not complete"
                ))
                .into_owned());
            }
            current = self
                .control
                .changed(&current)
                .await
                .map_err(|fault| fault.message().to_owned())?;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self, strategy: HostShutdownStrategy) -> Result<(), String> {
        self.request_shutdown(strategy)?;
        self.finish_shutdown().await
    }

    /// Releases the client and diagnostics after actual Host cleanup.
    pub async fn finish_shutdown(&mut self) -> Result<(), String> {
        self.wait_stopped().await?;
        self.client.close();
        if let Some(diagnostics) = self.diagnostics.take() {
            tokio::task::spawn_blocking(move || diagnostics.finish())
                .await
                .map_err(|_| "diagnostic flush worker failed after Host stopped".to_owned())?;
        }
        Ok(())
    }
}

fn startup_fault(fault: Fault) -> String {
    crate::wrap::sanitize(&format!("Host initialization failed: {fault}")).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_error_preserves_the_cause_without_terminal_controls() {
        let message = startup_fault(Fault::new(
            FaultKind::HostConfigInvalid,
            "Cannot load model\nMissing named credential\u{1b}[2J",
        ));
        assert!(message.contains("Cannot load model\nMissing named credential"));
        assert!(message.contains("HOST_CONFIG_INVALID (1601)"));
        assert!(!message.contains('\u{1b}'));
    }
}
