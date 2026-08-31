//! minicore-tui: a pi-style coding agent TUI frontend for minicore-agent.
//!
//! This crate owns terminal lifecycle, themes, the RPC client, and rendering.
//! It talks to the agent exclusively over stdio JSON-RPC (see
//! `docs/rpc-contract.md`) and never depends on the agent or runtime crates.
//!
//! Phase 0 scaffold: `TerminalGuard`, the dark/light `Theme` palettes, the
//! empty fullscreen renderer, and the CLI/error plumbing. Phase 1 adds the
//! RPC layer: `protocol` (wire DTOs and frame parsing), `rpc` (the agent
//! child and its stdio tasks), and `event` (RPC events). Phase 2 adds the
//! app state machine: `app` (the single `App::update` mutation point), the
//! pure-data `state` modules, and the `command` side-effect enum. Phase 3
//! adds the Pi-style fullscreen conversation rendering: `markdown` (a small
//! pulldown-cmark wrapper) and the `ui` module (transcript, block cards,
//! dock, composer, footer). Per the development spec, all app state is
//! mutated only through `App::update`, never from tasks or render code.

pub mod app;
pub mod args;
pub mod command;
pub mod error;
pub mod event;
pub mod markdown;
pub mod protocol;
pub mod rpc;
pub mod state;
pub mod terminal;
pub mod theme;
pub mod ui;
