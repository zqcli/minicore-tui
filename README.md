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
renderer with a small-terminal safety hint.

Phase 1 adds the stdio RPC layer for the pinned agent contract:
`src/protocol.rs` (wire DTOs, NDJSON frame parser, request builder) and
`src/rpc.rs` (agent child lifecycle: one stdin writer fed only with requests
verified against the agent's 1 MiB line bound, one stdout reader under an
8 MiB frame bound, one stderr reader with UTF-8-safe 4096-byte lines, and a
bounded event channel).

Phase 2 adds the app state machine: `src/app.rs` owns all state via the
single `App::update(AppEvent)` entry point, `src/state/` holds the pure-data
session/transcript/turn/catalog structures, and `src/command.rs` describes
outbound side effects for the future main loop. It covers bootstrap
(ping + catalogs + sessions, Ready only after all four succeed), session
create/open with paged `session.transcript` loading, live turns with
`turn.send`/`turn.wait`/`turn.cancel`, and durable reconciliation: after a
wait, the transcript is re-fetched and the live turn is replaced by durable
blocks. 

Phase 3 renders that state as the Pi-style fullscreen conversation
layout: `src/ui/` (transcript scroll view, user/assistant/reasoning/tool
cards, dock with busy status/composer/footer, startup header, fatal
overlay) and `src/markdown.rs` (a small `pulldown-cmark` wrapper; see the
module docs for why `tui-markdown` was not a fit). New visual state moves only
through `AppEvent`s (`Tick`, `SetTheme`, `ToggleReasoning`, `ToggleTools`,
`ToggleTool`). Rendering is a pure read-only view: each frame is derived
tight from state, and the per-block line caches from spec 19 are deferred
to a later phase (they must be populated in `App::update`, never in
`render`). Full composer editing and slash commands are not implemented
yet; the Phase 3 main loop constructs the app but does not start an agent
(`q`/`Ctrl+C` still quit).

Phase 4 adds the dock selectors (development spec 24-28): the new-session
form and the session/model/reasoning/profile selectors replace the
composer inside the dock while the transcript stays visible. All selection
state moves through semantic `AppEvent`s (`OpenNewSession`,
`Open*Selector`, `SetSelectorQuery`, `MoveSelector`, `PageSelector`,
`ConfirmDock`, `CancelDock`, `DockFieldStep`, `NewSessionSetField`,
`SubmitNewSession`); key mapping arrives in Phase 5. Selecting a model or
reasoning opens (or keeps) a new-session draft and **creates a new session**
when confirmed — the active session is never modified, and `session.create`
is the only bridge to a real session. The session selector sorts
`session.list` by `updated_at` descending (real RFC3339 instants, with
deterministic fallbacks for unparsable timestamps), filters
case-insensitively over title/workspace/session_id/model/profile, and
opens via `session.open` (keep-on-error, one in-flight open at a time).
The new-session draft seeds workspace/profile/model/reasoning from the
catalog defaults; workspace stays a plain string the agent validates.
Tool cards, live-turn deltas, and the durable transcript behaviour from
Phases 2-3 are unchanged.

Phase 5 adds input and local commands (development spec 22-23, 43): the
main loop feeds raw `crossterm` terminal events into `AppEvent::Terminal`,
the fixed `src/keymap.rs` maps keys to `Action`s (no dynamic key
configuration), and only `App::update` mutates state. The composer is now
a real `tui-textarea` wrapper with in-process history (100 entries),
undo/redo, multi-line editing, and send-failure recovery. Slash commands
(`/new`, `/resume`, `/sessions`, `/model`, `/reasoning`, `/theme dark|light`,
`/clear`, `/help`, `/logs`, `/quit`) are parsed locally in `src/command.rs`;
unknown commands only show a notice and never touch the wire. Help and
Logs panels dock below the transcript (max 60%), and the transcript scrolls
(`PageUp/PageDown/Home/End`, mouse wheel, follow-the-tail with a
`↓ new output` marker) using geometry measured by the main loop and fed
back via `AppEvent::Viewport`. The full key and command reference lives in
docs/keybindings.md. Phase 5 still does not spawn the agent: RPC is
impossible while the connection is `Starting`, and any RPC command reaching
the main loop is reported as a send failure rather than silently dropped.

Phase 6 completes the wiring: the TUI owns `minicore-agent --stdio` for
real. The flat CLI takes `--agent-bin` / `--agent-config` / `--workspace`
plus per-session model/reasoning/profile defaults; the agent is spawned and
validated **before** the alternate screen so failures are ordinary stderr
errors. An async main loop (`tokio::select!`) multiplexes the RPC event
channel and Crossterm's asynchronous `EventStream` (the application does not
create a blocking terminal-reader thread), the 10 Hz tick, OS signals, and
the 5-second shutdown kill-deadline, and
applies every source through the single `App::update`. Rendering is
throttled to 30 FPS by a `dirty` flag cleared by an explicit `Rendered`
event. `agent.shutdown` is the only quit path for a live connection:
`shutting down → agent.shutdown → (response, EOF, exit in any order) →
Exit`, with a 5 s force-kill fallback; abnormal exits surface the fatal
overlay, not an auto-restart. Protocol fixtures and integration tests
(`tests/protocol.rs`, `tests/rpc_io.rs`, `tests/app_flow.rs`,
`tests/render_snapshots.rs`, `tests/terminal_restore.rs`) are fully offline
except the ignored real-agent E2E (`tests/agent_e2e.rs`).

## Requirements

- Rust 1.85.0 or newer (edition 2024)
- `unsafe_code` is forbidden in this crate (`[lints]` in `Cargo.toml`)

Pinned TUI dependencies (a single crossterm 0.28.x instance only):

```text
ratatui      = "=0.29.0"    default-features off, feature crossterm
crossterm    = "=0.28.1"    feature event-stream
tui-textarea = "=0.7.0"     default-features off, feature crossterm
```

## Keybindings and commands

The full key table and slash-command reference live in
[docs/keybindings.md](docs/keybindings.md). In short: `Ctrl+C` clears or
(double press) quits, `F1` opens Help, `Ctrl+R` opens the session
selector, `Ctrl+L` / `Shift+Tab` open the model/reasoning selectors, and
`/new`, `/resume`, `/model`, `/reasoning`, `/theme dark|light`, `/clear`,
`/help`, `/logs`, `/quit` are implemented locally.

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
cargo run -- --agent-config /path/to/agent.toml
cargo run -- --agent-config agent.toml --workspace . --profile coding --reasoning high
```

| Option | Description |
|---|---|
| `--agent-bin <PATH>` | minicore-agent binary (default `minicore-agent` on PATH) |
| `--agent-config <PATH>` | agent config file (required; must exist) |
| `--workspace <PATH>` | workspace for a new session (default: cwd; explicit values open a pre-filled new-session form) |
| `--profile <ID>` | default profile for new sessions |
| `--model <ID>` | default model for new sessions |
| `--reasoning <auto\|disabled\|low\|medium\|high>` | default reasoning for new sessions |
| `--theme <dark\|light>` | Color theme (default: `dark`) |
| `--debug` | log RPC method/id/bytes/timing to a temp file (never content) |
| `--version` | Print version |
| `--help` | Print usage |

The agent config must point at whatever model backend you use. This
terminal frontend never calls a provider directly; the agent owns all model
and data access.

## Agent E2E (optional, offline by default)

`tests/agent_e2e.rs` drives a real agent over stdio with the production
RPC client and app state machine:

```bash
MINICORE_AGENT_BIN=/path/to/minicore-agent \
MINICORE_AGENT_CONFIG=/path/to/loopback-mock-agent.toml \
cargo test --test agent_e2e -- --ignored
```

The config must target a loopback mock (local model endpoint); the test
refuses configs containing real provider keys/endpoints and never touches
your real config or data dir.

## Backend contract baseline

- Agent repository: `https://github.com/zqcli/minicore-agent` (branch `dev`)
- Fixed commit: `6d5e963031159c458212a92c690e515a2ac3761b`
- RPC version: `0.2.0`
- Contract doc blob: `b8f4d57c6931cad8b99b39fdda0647a2539824a6`

## Scope honesty

- Selecting a model or reasoning always drives a new-session draft; it is
  never a hot-swap of the current session. The current session only changes
  when a create/open response activates it.
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