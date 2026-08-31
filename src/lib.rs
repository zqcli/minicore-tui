//! minicore-tui: a pi-style coding agent TUI frontend for minicore-agent.
//!
//! This crate owns terminal lifecycle, themes, and rendering. It talks to the
//! agent exclusively over stdio JSON-RPC (see `docs/rpc-contract.md`) and
//! never depends on the agent or runtime crates.
//!
//! Phase 0 scaffold: only `TerminalGuard`, the dark/light `Theme` palettes,
//! and the empty fullscreen renderer exist, together with the CLI and error
//! plumbing. The RPC process and app state arrive in later phases; per the
//! development spec, all future app state is mutated only through
//! `App::update`, never from tasks or render code.

pub mod args;
pub mod error;
pub mod terminal;
pub mod theme;
pub mod ui;
