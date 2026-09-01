# minicore-tui

`minicore-tui` is an independent Rust terminal frontend for
[`minicore-agent`]. It owns the fullscreen terminal UI and communicates with
one Agent child process exclusively through stdio NDJSON JSON-RPC. It does not
link the `minicore-agent` or `minicore-runtime` Rust crates.

The visual hierarchy and interaction style are inspired by Pi's fullscreen
coding-agent TUI: a scrollable transcript, a fixed dock, compact status,
composer, selectors, and overlays. This is not a Pi fork. It contains no Pi
source code, logo, or brand assets.

## Install And Build

Requirements:

- Rust 1.85.0 or newer;
- a supported terminal on Linux, macOS, or Windows;
- a compatible `minicore-agent` executable and Agent configuration.

Build the locked release locally:

```bash
cargo build --locked --release
```

The resulting binary is `target/release/minicore-tui` (or the platform
executable equivalent). The package is one Cargo package, uses Edition 2024,
and forbids unsafe code.

## Usage

A run requires `--agent-config`; `--help` and `--version` work without it.
The TUI starts the child before entering the alternate screen, so missing
configuration or spawn failures remain ordinary terminal errors.

```bash
minicore-tui \
  --agent-bin minicore-agent \
  --agent-config ./agent.toml \
  --workspace .
```

For a release checkout without installing the binary:

```bash
cargo run --locked -- \
  --agent-config ./agent.toml \
  --workspace ./project \
  --profile coding \
  --model deep \
  --reasoning high \
  --theme dark
```

### CLI

| Option | Meaning |
|---|---|
| `--agent-bin <PATH>` | Agent executable; defaults to `minicore-agent` on `PATH`. |
| `--agent-config <PATH>` | Agent TOML configuration; required in run mode. |
| `--workspace <PATH>` | Workspace string used by a new session; defaults to the current directory. |
| `--profile <ID>` | Default profile for a new session. |
| `--model <ID>` | Default model for a new session. |
| `--reasoning <LEVEL>` | `auto`, `disabled`, `low`, `medium`, or `high`; default is Agent/profile selection. |
| `--theme <dark\|light>` | Built-in palette; default is `dark`. |
| `--debug` | Append request metadata (method, id, byte count, duration) to a local temporary log; never message or tool content. |
| `--help`, `-h` | Print usage and exit. |
| `--version`, `-V` | Print the TUI version and exit. |

The TUI passes the Agent config path to `minicore-agent --config <path>
--stdio`. The Agent configuration owns provider URLs, credentials, profiles,
models, tools, and `data_dir`; the TUI never reads those files or calls a
provider directly. Keep one Agent process per `data_dir`; the Agent store does
not provide a cross-process lock.

## Sessions And Turns

Startup discovers the Agent, models, profiles, and sessions. `/new` opens a
new-session form; `/resume` and `/sessions` open the existing-session
selector. Workspace, profile, model, and reasoning are sent to
`session.create` as appropriate.

A session's model and reasoning are immutable after creation. Selecting a
model or reasoning level edits a new-session draft and creates a new session;
it never hot-swaps the active session. A turn sends `turn.send` and registers
`turn.wait` immediately. Agent events are best-effort live display data and
may be dropped. `turn.wait`, `session.state`, and `session.transcript` are the
authoritative sources; completed turns reconcile the live view with durable
transcript entries.

Tools run automatically under the Agent. Bash is not sandboxed. The TUI does
not add approval, steering, compaction, live Bash output, MCP, plugins,
skills, subagents, session branching, or reconnect/restart behavior.

## Keys And Commands

The complete current keymap and slash-command semantics are in
[docs/keybindings.md](docs/keybindings.md). The short list is:

- `F1` opens Help;
- `Ctrl+R` opens Sessions, `Ctrl+L` opens Model, and `Shift+Tab` opens Reasoning;
- `Ctrl+T` toggles reasoning and `Ctrl+O` toggles tool previews;
- `PageUp`/`PageDown`, `Ctrl+Home`/`Ctrl+End`, and mouse wheel scroll the transcript;
- `Esc` closes a dock or cancels the exact running turn;
- `Ctrl+C` clears non-empty input, then double-presses to quit; `/quit` performs normal shutdown.

Implemented local commands are `/new`, `/resume`, `/sessions`, `/model`,
`/reasoning`, `/theme dark`, `/theme light`, `/clear`, `/help`, `/logs`, and
`/quit`. Unknown commands never reach the Agent.

## Backend Contract And Scope

The wire contract is pinned in [docs/rpc-contract.md](docs/rpc-contract.md):

- Agent commit `6d5e963031159c458212a92c690e515a2ac3761b`;
- RPC version `0.2.0`;
- contract document blob `b8f4d57c6931cad8b99b39fdda0647a2539824a6`;
- NDJSON over stdio, with one TUI writer, one stdout reader, one stderr reader,
  bounded frames, request IDs, response/event interleaving, and no event replay.

The TUI deliberately does not implement Agent capabilities that are absent
from this interface: approval UI, steering or follow-up queue, compaction
controls, live Bash/PTY output, MCP, plugins, skills, subagents, remote
agents, image input, or session tree operations. External editor and OSC52
copy are optional follow-up work and are not part of v0.1.

## Platform And Troubleshooting

The intended platform matrix is Linux, macOS, and Windows with Rust 1.85+.
The terminal uses Crossterm alternate-screen/raw mode, bracketed paste, mouse
capture, and a real hardware cursor. A small terminal shows a safe-size hint.
Terminal restoration is attempted on normal, error, child-exit, shutdown-timeout,
and panic paths.

Common errors:

- **`--agent-config` is required**: supply the Agent TOML path; help/version do not need it.
- **Agent executable not found**: set `--agent-bin` or put `minicore-agent` on `PATH`.
- **Bootstrap failed**: inspect the Agent config/profile/model and the Help/Logs panel; the TUI does not auto-retry.
- **Session waiting for unsupported interaction**: use an Agent profile with automatic tool behavior; this TUI has no approval UI.
- **`Disconnected` or a fatal overlay**: the child or RPC stream ended; press `q` after reviewing the safe status/log tail.
- **Terminal too small**: enlarge it to at least 60×16.
- **Another Agent already uses the data directory**: stop the other Agent process before retrying.

## Testing

The default suite is offline and uses protocol fixtures, a production-driven
fake Agent harness, app-flow tests, TestBackend snapshots, and terminal
lifecycle tests. See [docs/testing.md](docs/testing.md) for the remote Rust
1.85 commands, snapshot inventory, ignored tests, Windows checks, and E2E
procedure. See [docs/acceptance.md](docs/acceptance.md) for the honest
MT-001–MT-144 status matrix and [docs/verification.md](docs/verification.md)
for the final delivery evidence.

A real-Agent E2E is ignored by default and must use a loopback mock endpoint;
it does not require or permit access to a real provider. Do not put secrets or
real user data in fixtures, logs, E2E config, or snapshots.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at
your option.

[`minicore-agent`]: https://github.com/zqcli/minicore-agent
