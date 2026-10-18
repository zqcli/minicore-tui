# minicore-tui

`minicore-tui` is the terminal frontend for [`minicore-agent`]. It owns one
fullscreen "rail" UI — transcript, status line, composer, footer — and talks to
a single Agent child process over stdio NDJSON JSON-RPC. It does not link the
`minicore-agent` or `minicore-runtime` crates.

Version 0.4.0 is a rewrite of the frontend against Agent 0.7.0 (Protocol v1).
The redesign targets long, large coding sessions: a turn ends exactly when the
Agent's authoritative `turn.wait` resolves, streaming renders incrementally, the
transcript is cached per block, and scrolling stays anchored to what you are
reading. The UI is English-only with one visual system for every surface.

## Requirements

- Rust 1.88 or newer (edition 2024; `rust-version` in `Cargo.toml`);
- a `minicore-agent` 0.7.0 executable (or `--agent <path>`) with an Agent TOML
  configuration;
- a terminal with an alternate screen, raw mode and bracketed paste.

## Build

```bash
cargo build --locked --release
```

The binary is `target/release/minicore-tui`. The package uses Rust edition 2024
and forbids unsafe code.

## Run

```bash
minicore-tui --agent minicore-agent --config ./agent.toml --workspace ./project
```

Startup spawns the Agent before entering the alternate screen, so a missing
binary or rejected configuration stays an ordinary terminal error. The TUI then
opens a session and waits for the first message; it does not call a model until
you submit one. A draft typed during startup is kept.

### CLI

| Option | Meaning |
|---|---|
| `--agent <PATH>` | Agent executable. Defaults to `minicore-agent` on `PATH`. |
| `--config <PATH>` | Agent TOML configuration. May also come from `agent_config` in the TUI config file. |
| `--workspace <DIR>` | Workspace passed to a new session. Defaults to the current directory. |
| `--session <ID>` | Open this exact session. |
| `--continue` | Open the most recent session for the workspace. |
| `--profile <ID>` | Default profile for a new session. |
| `--model <ID>` | Default model for a new session. |
| `--reasoning <LEVEL>` | Default reasoning: `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`. |
| `--theme <dark\|light>` | Palette for this run. |
| `-h`, `--help` | Print usage. |
| `-V`, `--version` | Print the version. |

`--session` and `--continue` take precedence over creating a session. Explicit
`--profile`, `--model` and `--reasoning` override only those fields.

### TUI config

`~/.config/minicore-tui/config.toml` (or `$MINICORE_TUI_CONFIG`) holds local
preferences only — never Agent credentials or provider catalogs:

```toml
agent = "minicore-agent"
agent_config = "/path/to/agent.toml"
theme = "dark"
editor = "vim"
```

Command-line values take precedence. `/theme` writes the chosen palette back to
this file. `Ctrl+G` opens `editor` from this file, falling back to `$VISUAL`
then `$EDITOR`; there is no separate environment variable for it.

## Screen

```text
 transcript (scrollable, pi-style rail)
 …
 ⠋ Thinking · 1m12s · request 7 · 12 tools · esc to interrupt
 ↳ steer · focus on the parser first
▎ composer (1–10 rows, grows with the draft)
 minicore-tui@main · gpt-6-luna · max · ● working      ↑50.5k ↓1.8k · ctx 41%
```

The status line appears only while a turn is busy and names the current phase:
`Thinking`, `Writing`, `Running <tool> <detail>`, `Waiting for approval`,
`Compacting context`, `Interrupting`, or `Waiting for compaction`. It always
shows the elapsed turn time and the interrupt key.

The footer shows the workspace directory and git branch, the session title, the
model and reasoning level, a state dot, token totals and the context percentage
(`?` while unknown). The branch is `—` until the first `workspace.status` answer
arrives: `session.presentation` only caches it, and the TUI refreshes that query
on session open and when the workspace changed on disk at turn end, never on
every frame. Notices appear on one line under the status line; info notices hide
after a few seconds, errors stay until the next key press.

## Conversation

- **User** messages are a tinted card with a blue rail.
- **Assistant** answers render Markdown as they stream, at a one-column inset.
  Reasoning appears above the answer on a purple rail, folded while streaming
  and collapsed to `Thought for 42s` when finished.
- **Tool cards** start folded: status glyph, tool name, target, duration and
  exit code. Bash shows its last output lines live, edits show the start of the
  diff, `read` shows its line count. After a reload a bash card shows its line
  count and opens the full output on demand instead of replaying the tail.
  `Ctrl+O` expands or collapses all cards, and clicking one toggles it.
- **Turn end** is one dim line with outcome, duration, request and tool counts,
  and the change summary when the turn touched files. A failed or cancelled
  turn shows its reason.
- **Compaction** inserts a one-line marker; the summary is available from it.
  Content that is already loaded is never removed from the transcript.

## Turns, steering, compaction

Enter sends while the session is idle. While a turn is running, Enter steers it
and `Alt+Enter` queues a follow-up that is sent after the turn ends. The queue
is shown above the composer; `Alt+Up` takes the last queued message back into
the composer. If the Agent is still busy with post-turn compaction, a new
message waits in the queue and the status line says `Waiting for compaction`.

`Esc` interrupts a running turn. The turn ends when the Agent reports the
cancelled outcome. Unsent steering text returns to the queue as a follow-up.

`/compact` runs manual compaction when the session is idle. Automatic
compaction by the Agent is shown in the status line and leaves a marker in the
transcript.

## Keys and commands

The full map is in [docs/keybindings.md](docs/keybindings.md). The essentials:

- `Enter` send/steer, `Alt+Enter` newline or queued follow-up, `Esc` close or
  interrupt, `Ctrl+C` clear / quit, `Ctrl+D` quit;
- `Ctrl+R` sessions, `Ctrl+L` models, `Shift+Tab` reasoning, `Ctrl+O` tool
  cards, `Ctrl+T` reasoning, `Ctrl+F` find, `F1` help;
- `PageUp`/`PageDown`, `Ctrl+Home`/`Ctrl+End`, the wheel and `Ctrl+↑`/`Ctrl+↓`
  navigate the transcript;
- `/` opens the command palette and `@` the workspace file picker;
- commands: `/new`, `/sessions` (`/resume`), `/rename`, `/model`, `/reasoning`,
  `/compact`, `/diff`, `/context`, `/copy`, `/export`, `/theme`, `/reload`,
  `/help`, `/quit`.

## Views

A bottom sheet above the composer hosts pickers and approval prompts. The diff,
tool-output, context and help views replace the transcript area and keep the
footer; `Esc` returns.

The diff view lists changed files next to a colored unified diff and toggles
its scope (turn, session, workspace). The tool view shows a tool's full output
and its input. The context view explains the context budget, the last
compaction and what the summary covers. The help view lists keys and commands.

## Copy and export

Drag in the transcript to copy the selection (native clipboard, OSC 52
fallback). `/copy` copies the last assistant answer as Markdown. `/export
[path]` writes the whole conversation as Markdown, defaulting to
`./minicore-<session>-<date>.md`.

## Backend contract

- `minicore-agent` 0.7.0 (`4102719`), Protocol v1, with the capabilities the TUI
  uses: `session.read`, `session.read.display`, `tool.read`, `tool.read.display`,
  `tool.output`, `turn.result`, `session.context`, `session.presentation`,
  `workspace.status`, `workspace.files`, `workspace.search`, `changes.list`,
  `changes.diff`. These are entries in the Agent's advertised capability list;
  the handshake checks the list, not method names.
- `minicore-runtime` 0.7.0 (`b93aea3`) is linked by the Agent, not by the TUI.
- One Agent process per data directory. The TUI never reads the Agent store,
  workspace files, provider configuration or credentials.
- Shutdown sends `agent.shutdown`, waits briefly, then terminates the child.
- The Agent executes tools; the TUI adds no permission layer beyond the profile
  the Agent configuration selects.

## Troubleshooting

- **Agent not found** — set `--agent` or put `minicore-agent` on `PATH`.
- **Fatal screen** — the Agent exited or the protocol failed. The screen shows
  the reason, exit status and the last lines of Agent stderr; `q` quits.
- **`session_busy`** — a post-turn compaction is still running. Keep the draft;
  it is queued and sent when the session is idle.
- **`invalid_state` on a history read** — the TUI repeats the read once with a
  fresh probe. If it persists, reopen the session.

## Testing

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

The suite covers pure logic (wrapping, markdown and its incremental path,
composer editing, chunk assembly, event application, scroll anchoring, layout
caching), `TestBackend` render snapshots for the main screen states, and one
fake-Agent process test that drives startup, send, stream, wait and turn end
over the real transport. Real-model end-to-end runs use the external tmux
harness; they are not part of the default suite.

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at
your option.

[`minicore-agent`]: https://github.com/zqcli/minicore-agent
