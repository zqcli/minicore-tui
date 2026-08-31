# minicore-tui

A coding agent TUI for [minicore-agent], implemented in Rust. It is the
terminal frontend of the MiniCore stack: the TUI talks to the agent
**exclusively** through the stdio JSON-RPC contract in
[docs/rpc-contract.md](docs/rpc-contract.md) and never links the
`minicore-agent` or `minicore-runtime` crates.

The visual hierarchy and interaction style follow the Pi coding agent TUI
fullscreen mode. This project is **not** a fork of Pi, does not copy Pi source,
and does not use Pi logos or branding.

## Status

Phase 0 scaffold: package layout, MSRV tooling, `TerminalGuard`
(alternate-screen lifecycle), dark/light `Theme`, and an empty fullscreen
renderer with a small-terminal safety hint. The agent RPC process, app state,
and the conversation UI arrive in later phases. Today `minicore-tui` shows a
blank fullscreen; press `q` or `Ctrl+C` to quit.

## Requirements

- Rust 1.85.0 or newer (edition 2024)
- `unsafe_code` is forbidden in this crate (`[lints]` in `Cargo.toml`)

Pinned TUI dependencies (a single crossterm 0.28.x instance only):

```text
ratatui      = "=0.29.0"    default-features off, feature crossterm
crossterm    = "=0.28.1"    feature event-stream
tui-textarea = "=0.7.0"     default-features off, feature crossterm
```

## Build and test

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo tree -p crossterm        # must list a single crossterm version
```

## Usage

```bash
cargo run -- --theme dark
cargo run -- --theme light
```

| Option | Description |
|---|---|
| `--theme <dark\|light>` | Color theme (default: `dark`) |
| `--version` | Print version |
| `--help` | Print usage |

## Backend contract baseline

- Agent repository: `https://github.com/zqcli/minicore-agent` (branch `dev`)
- Fixed commit: `6d5e963031159c458212a92c690e515a2ac3761b`
- RPC version: `0.2.0`
- Contract doc blob: `b8f4d57c6931cad8b99b39fdda0647a2539824a6`

## Scope honesty

- Model and reasoning are frozen when a session is created; changing them
  creates a new session.
- Agent events are best effort and may be dropped; `turn.wait`,
  `session.state`, and the durable transcript are authoritative.
- Tools run automatically; bash is not sandboxed.
- No approval UI, no steering, no compaction, no live bash output in v0.1.
- One agent process per `data_dir`; the store has no cross-process lock.

## License

Licensed under either of Apache License, Version 2.0 or the MIT license, at
your option.

[minicore-agent]: https://github.com/zqcli/minicore-agent