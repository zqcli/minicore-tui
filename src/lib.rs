//! minicore-tui: a pi-style coding-agent TUI for minicore-agent over stdio
//! JSON-RPC.
//!
//! Module direction: `protocol` ← `rpc` ← `transcript` ← `app` ← `ui`; `ui`
//! reads `&App` and draws it, and `app` keeps only the layout cache's render state
//! and the style palette from `ui`.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod editor;
pub mod prompt_history;
pub mod protocol;
pub mod remote;
pub mod rpc;
pub mod terminal;
#[cfg(test)]
pub mod testing;
pub mod title;
pub mod transcript;
pub mod ui;
pub mod wrap;

pub use app::App;
