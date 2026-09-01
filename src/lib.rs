//! minicore-tui: a pi-style coding agent TUI frontend for minicore-agent.
//!
//! This crate owns terminal lifecycle, themes, the RPC client, and rendering.
//! It talks to the agent exclusively over stdio JSON-RPC (see
//! `docs/rpc-contract.md`) and never depends on the agent or runtime crates.
//!
//! The implementation includes the fullscreen terminal lifecycle, pinned
//! stdio JSON-RPC process adapter, single-writer App state machine, durable
//! transcript reconciliation, selectors, multiline composer, fixed keymap,
//! Pi-style rendering, and update-installed durable transcript line caches.
//! Markdown is wrapped by a small pulldown-cmark adapter; live streaming stays
//! plain and never enters the durable cache. All app state is mutated only
//! through `App::update`, never from tasks or render code.

pub mod app;
pub mod args;
pub mod command;
pub mod error;
pub mod event;
pub mod keymap;
pub mod markdown;
pub mod protocol;
pub mod rpc;
pub mod state;
pub mod terminal;
pub mod theme;
pub mod ui;
