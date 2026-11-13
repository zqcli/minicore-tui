//! A terminal frontend using the native embedded MiniCore Host client.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod config;
pub mod editor;
pub mod history;
pub mod host;
pub mod prompt_history;
pub mod terminal;
#[cfg(test)]
pub mod testing;
pub mod title;
pub mod transcript;
pub mod ui;
pub mod wrap;

pub use app::App;
