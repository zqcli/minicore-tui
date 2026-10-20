//! minicore-tui: a pi-style coding-agent TUI for minicore-agent over stdio
//! JSON-RPC.
//!
//! Module direction: `protocol` ← `rpc` ← `app` → `transcript` → `ui`.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod editor;
pub mod prompt_history;
pub mod protocol;
pub mod rpc;
pub mod terminal;
#[cfg(test)]
pub mod testing;
pub mod transcript;
pub mod ui;

pub use app::App;
