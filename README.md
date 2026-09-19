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

Since 0.2.7, Debug builds use `opt-level=1` for this package and level 2 for
dependencies. Debug information and assertions remain enabled, but optimization
can affect stepping and local-variable visibility. Terminal output batches small
ANSI writes through a 64 KiB buffer; render rates are unchanged. The Release
profile is unchanged. The current release line is **0.3.0**, paired with Agent
**0.5.0** and Runtime **0.4.1** over Protocol v1; the fixed revisions are
listed in [the backend contract](docs/backend.md). The older 0.2.x release
notes remain historical and are not evidence for this release.

For a macOS x86_64 cross-build, put LLVM's `clang` and `ld64.lld` on `PATH`,
set `SDKROOT` to a macOS SDK, and run `scripts/build-macos-x86_64.sh`.
The script uses Rust 1.85, fixes `MACOSX_DEPLOYMENT_TARGET=11.0`, clears external
Cargo/Rust flag and target-directory overrides, and prints the ad-hoc-signed
artifact path `target/x86_64-apple-darwin/release/minicore-tui`. Verify it with
`scripts/verify-macos-binary.sh`; its `--self-test` mode provides portable parser
checks. LLVM 19 was verified in 0.2.7; the previous Zig 0.13 link path failed a
native standalone `catch_unwind` probe and is no longer used.

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
| `--config <PATH>` | Local TUI preferences; Agent credentials/catalogs are never loaded from it. |
| `--agent-bin <PATH>` | Agent executable; defaults to `minicore-agent` on `PATH`. |
| `--agent-config <PATH>` | Agent TOML configuration; required in run mode unless supplied by `--config`. |
| `--workspace <PATH>` | Workspace string used by a new session; defaults to the current directory. |
| `--continue` | Open the most recent session for the current workspace without prompting. |
| `--session <ID>` | Open this exact session ID without guessing. |
| `--profile <ID>` | Default profile for a new session. |
| `--model <ID>` | Default model for a new session. |
| `--reasoning <LEVEL>` | `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max`, or `ultra`; default is Agent/profile selection. |
| `--theme <dark\|light>` | Built-in palette; default is `dark`. |
| `--debug` | Write a bounded local temporary log (200 lines, 4096 bytes/line) with request metadata (method, id, byte count, duration); never message or tool content. |
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

A session title is edited from the Session panel through `session.rename`; the
TUI waits for the complete acknowledgement and never optimistically changes
metadata. A session's model and reasoning can be updated with `session.update`;
an active loop observes the new setting only at a later request boundary. Model A,
tool work, and model B therefore remain separate request views. A turn sends
`turn.send` and registers `turn.wait` immediately. If submitted while a loop
is running, the composer routes the text to `turn.steer`. Agent events are
best-effort live display data and may be dropped. `turn.wait`, `session.state`,
and the paged `session.read` path are authoritative; `session.history` remains a
compatibility/diagnostic method and is not the application history path.

Tools run automatically under the Agent. Bash is not sandboxed. The TUI supports
mid-turn steering via `turn.steer`. It does not add approval, live Bash PTY output,
MCP, plugins, skills, subagents, session branching, or reconnect/restart behavior.

## Keys And Commands

The complete current keymap and slash-command semantics are in
[docs/keybindings.md](docs/keybindings.md). The short list is:

- `F1` opens Help;
- `Ctrl+R` opens Sessions, `Ctrl+N` opens New Session, `Ctrl+L` opens Model, and `Shift+Tab` opens Reasoning;
- In Sessions, `F2` renames, `F5` refreshes, `Ctrl+W` closes, and `Delete`/`Ctrl+D` deletes after the required confirmations;
- `Ctrl+T` toggles reasoning and `Ctrl+O` toggles tool previews;
- `PageUp`/`PageDown` scroll with four rows of overlap; `Ctrl+Home`/`Ctrl+End` jump to the transcript ends;
- mouse wheel moves one row, or five with Alt; the Pi-style scrollbar appears on scrolling/hover, supports live dragging, and hides after one second;
- the bottom scroll-status hint overlays the transcript without consuming a row; see [scrollbar verification](docs/verification/scrollbar/README.md);
- `Esc` closes a dock or cancels the exact running turn;
- `Ctrl+C` clears non-empty input, then double-presses to quit; `/cancel` cancels the exact active loop, `/reload` reloads Agent configuration and safe read-only state, and `/quit` performs normal shutdown.

Implemented local commands include `/new`, `/resume`, `/sessions`, `/model`,
`/reasoning`, `/theme`, `/clear`, `/help`, `/logs`, `/cancel`, `/reload`,
`/quit`, `/close`, `/delete`, `/rename`, `/tool`, `/files`, `/grep`, `/diff`,
`/context`, `/compact`, `/refresh`, `/search`, `/prev`, `/next`, `/latest`,
`/copy`, `/export`, `/settings`, and `/editor`. Unknown commands never reach
the Agent. `/reload` sends empty `agent.reload` params, refreshes safe
catalog/session reads and rereads a retained turn result once without duplicating
an existing wait or replaying execution.

## Backend Contract And Scope

The wire contract is pinned in [docs/rpc-contract.md](docs/rpc-contract.md),
with the current source and backend revisions in [docs/backend.md](docs/backend.md).
Historical r2 and E3 verification records remain linked from the acceptance
matrix. `persisted` acknowledges appending the process's durable items, not
transaction/fsync/crash durability. A failed append blocks the Session while
retaining its in-process completion.

- Agent 0.5.0 commit `061743369459299e66be97bf97d2b27352a39914`;
- Runtime 0.4.1 commit `6cd2bdbc634437dea925495c61c7eb0be10ba171`;
- TUI code/test/snapshot baseline `0aa64c5e4d9211351123db059547beddb15c2cce`;
- RPC Protocol v1 with the required capability set;
- NDJSON over stdio, with one TUI writer, one stdout reader, one stderr reader,
  bounded frames (up to 32 MiB), request IDs, response/event interleaving, and no event replay.

The Agent executes native stateless `subagent` single/parallel/chain calls;
the TUI displays their ordinary Tool results. Persistent subagent orchestration,
manager/tree UI, approval UI, live Bash/PTY output, MCP, plugins, skills,
remote agents and image input remain outside this delivery. The TUI supports
bounded tool details, workspace file/search and preview views, read-only Changes/
Diff and Context views, local settings, and a direct external-editor draft
workflow. OSC52 copy remains outside the release boundary.

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
lifecycle tests. See [docs/testing.md](docs/testing.md) for the Rust 1.85/stable
commands, snapshot inventory, platform matrix, and E2E procedure. The current
refactor matrix is [docs/refactor-acceptance.md](docs/refactor-acceptance.md);
[docs/acceptance.md](docs/acceptance.md) is the historical v0.2 migration matrix.

A real-Agent E2E is ignored by default and must use the pinned Agent binary plus
a loopback mock endpoint; it does not require or permit access to a real
provider. Do not put secrets or real user data in fixtures, logs, E2E config, or
snapshots. Hosted CI builds the fixed Agent/Runtime sources separately and runs
the serial E2E job without provider credentials.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at
your option.

[`minicore-agent`]: https://github.com/zqcli/minicore-agent
