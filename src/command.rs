//! Outbound effects produced by `App::update` and executed by the future
//! main loop (development spec 9.1). Executing a command never touches the
//! app; failures flow back as `AppEvent`s (e.g. `AppEvent::RpcSendFailed`).

use crate::protocol::OutgoingRequest;

/// A side effect the main loop must perform on behalf of `App::update`.
#[derive(Debug)]
pub enum AppCommand {
    /// Write one already-numbered request line to the agent. The request id
    /// was allocated and registered in `pending_requests` inside `update`,
    /// before this command left it.
    Rpc(OutgoingRequest),
    /// Kill the agent child (the shutdown fallback path).
    KillChild,
    /// Leave the TUI.
    Quit,
}
