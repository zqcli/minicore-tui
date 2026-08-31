//! Events produced by the RPC background tasks. Tasks only send events; app
//! state is mutated exclusively by the future `App::update` (development
//! spec 9.1).
//!
//! Ordering contract: frames and log lines arrive in the order their bytes
//! were read on their own pipe, but `Frame`, `AgentLogLine`,
//! `ConnectionClosed`, `Exited`, and `ProtocolError` are produced by four
//! independent tasks, so no total order is promised between them. The app
//! must latch the first connection-terminating event (`ProtocolError`,
//! `ConnectionClosed`, or `Exited`) into a terminal state and then ignore
//! later termination events idempotently (Phase 2 contract; see also
//! `RpcProcess::recv`).

use std::process::ExitStatus;

use crate::protocol::{FrameError, IncomingFrame};

/// A transport event from the agent process or its pipes. Events from
/// different tasks arrive without a promised total order; see the module
/// docs for the termination semantics.
#[derive(Debug)]
pub enum RpcEvent {
    /// One complete response or notification frame.
    Frame(IncomingFrame),
    /// One captured agent stderr line, UTF-8 and capped at 4096 bytes
    /// (spec 10.8). Stderr is never printed to the terminal.
    AgentLogLine(String),
    /// The agent's stdout pipe reached EOF.
    ConnectionClosed,
    /// Fatal protocol or pipe failure; the connection must be considered
    /// dead and frames must not be scanned ahead.
    ProtocolError(FrameError),
    /// The agent child ended. `None` means the exit status could not be
    /// obtained (the kill fallback path failed to reap).
    Exited(Option<ExitStatus>),
}
