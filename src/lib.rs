//! minicore-tui: a pi-style coding agent TUI frontend for minicore-agent.
//!
//! This crate owns terminal lifecycle, themes, the RPC client, and rendering.
//! It talks to the agent exclusively over stdio JSON-RPC (see
//! `docs/rpc-contract.md`) and never depends on the agent or runtime crates.
//!
//! Phase 0 scaffold: `TerminalGuard`, the dark/light `Theme` palettes, the
//! empty fullscreen renderer, and the CLI/error plumbing. Phase 1 adds the
//! RPC layer: `protocol` (wire DTOs and frame parsing), `rpc` (the agent
//! child and its stdio tasks), and `event` (RPC events). The app loop that
//! owns RPC responses arrives in Phase 2; per the development spec, all
//! future app state is mutated only through `App::update`, never from tasks
//! or render code.

pub mod args;
pub mod error;
pub mod event;
pub mod protocol;
pub mod rpc;
pub mod terminal;
pub mod theme;
pub mod ui;
