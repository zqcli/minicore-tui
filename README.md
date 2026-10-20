# minicore-tui

`minicore-tui` is a fullscreen terminal frontend for [`minicore-agent`]: a
scrolling transcript, a status line, a composer and a footer, over one Agent
child process that it talks to with stdio NDJSON JSON-RPC (Protocol v1). It does
not link the `minicore-agent` or `minicore-runtime` crates. Version 0.4 is a
rewrite of the v0.3 frontend; see [CHANGELOG.md](CHANGELOG.md).

## Requirements

- Rust 1.88 or newer (edition 2024, `rust-version` in `Cargo.toml`) to build;
- a `minicore-agent` 0.7.0 executable and an Agent TOML configuration;
- a terminal with an alternate screen, raw mode and bracketed paste. Mouse
  reporting is used for the wheel, clicks and selection. `Shift+Enter` and
  `Alt+Enter` need a terminal with the kitty keyboard protocol; `Ctrl+J` always
  inserts a newline.

Backend contract: Agent 0.7.0 (`4102719`) and Runtime 0.7.0 (`b93aea3`) are the
versions CI pins. The Runtime is linked by the Agent, not by the TUI. At startup
the TUI requires Protocol v1 and these Agent capabilities, and refuses an Agent
that lacks one: `session.read`, `session.read.display`, `session.context`,
`turn.result`, `tool.read`, `tool.read.display`, `tool.output`,
`workspace.files`, `changes.list`, `changes.diff`.

## Build and run

```bash
cargo build --locked --release
target/release/minicore-tui --config ./agent.toml --workspace ./project
```

The Agent is spawned as `<agent> --config <agent.toml> --stdio` before the
screen switches, so a missing binary or a rejected configuration is an ordinary
terminal error. The TUI then opens or creates a session and waits for your first
message; no model is called until you send one. A draft typed meanwhile is kept.

### Options

| Option | Meaning |
|---|---|
| `--agent <BIN>` | Agent executable. Default: `minicore-agent` from `PATH`. |
| `--config <PATH>` | Agent TOML configuration. Required, unless `agent_config` is set in the TUI config. |
| `--workspace <DIR>` | Workspace of a new session. Default: the current directory. |
| `--session <ID>` | Open this session. |
| `--continue` | Open the most recent session of the workspace, or create one. Cannot be combined with `--session`. |
| `--profile <ID>` | Profile of a new session. |
| `--model <ID>` | Model for the session; applied to an opened session with `session.update`. |
| `--reasoning <LEVEL>` | `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max` or `ultra`; applied like `--model`. |
| `--theme <dark\|light>` | Palette for this run. |
| `-h`, `--help`, `-V`, `--version` | Usage, version. |

Bad arguments print the usage and exit with status 2.

### TUI config

Local preferences only; never Agent credentials or provider catalogs. The file
is `$MINICORE_TUI_CONFIG`, else `$XDG_CONFIG_HOME/minicore-tui/config.toml`,
else `~/.config/minicore-tui/config.toml`:

```toml
agent = "/usr/local/bin/minicore-agent"   # same as --agent
agent_config = "/path/to/agent.toml"      # same as --config
theme = "dark"                            # "dark" or "light"
editor = ["nvim"]                         # command and arguments for Ctrl+G
```

Command-line options win. A missing file gives the defaults; a file with a
syntax error or an unknown key is ignored as a whole. `/theme` saves the palette
by rewriting the file, which drops comments. `Ctrl+G` uses `editor`, then
`$VISUAL`, then `$EDITOR`. With `NO_COLOR` set the UI uses no colors.
The prompt history is saved as `history.jsonl` in the same directory.

## Screen

```text
 minicore-tui v0.4.0                                  (welcome header)
 esc interrupt · ctrl+c clear · / commands · @ files · ctrl+o expand · ctrl+r sessions
 transcript (scrollable, 1-column gutter, ▎ rails)
 …
 Follow-up: also update the README
 ⠋ Working (esc to interrupt) · Thinking · 1m12s · request 7 · 12 tools

 ▎ editor band (4–12 rows; the / and @ lists open inside it)
 ▸ minicore-tui@main · gpt 6 luna · max · working · 1m · queued   ↑50.5k ↓1.8k R33.8k · ctx 41.00%
```

The look follows pi-rail-ui. While something is in flight a Working line sits
above the editor with the phase (`Thinking`, `Running <tool> <detail>`,
`Waiting for approval`, `Compacting context`, …), elapsed time, request and
tool counts. Queued messages show as `Steering:` / `Follow-up:` rows above it.
The latest notice is a railed line in the band: info fades after 5 seconds,
errors stay until the next key press.

The footer shows the workspace and git branch, the model and reasoning level,
the state while not idle, the current or last turn's minutes, `queued`, token
totals since the session was opened and `ctx N%` — the context usage the Agent
reported for its last request (amber from 70 %), `ctx ?` once tokens were used
without a report. The TUI never estimates it.

## Features

- **Conversation.** User messages and Markdown answers stream with an incremental
  renderer. User messages are tinted cards; thinking has a purple rail and
  folds to its first three rows plus `... (N more lines, ctrl+o to expand)`;
  `Ctrl+T` collapses it to one `Thinking...` row.
- **Tool cards** start folded to three rows on a state-coloured band (running,
  done, failed): tool name, target (`$ cmd`, `path:range`, `pattern in path`)
  and a hidden-line hint, with duration, exit code or line changes on the
  right. `Ctrl+O` expands all cards and thinking, a click toggles one, and an
  expanded card's `click for full output` row opens the tool view.
- **Turn end** is one dim line: outcome, duration, requests, tools and, when files
  changed, `N files changed (+a −b)`. A failed or cancelled turn shows its reason.
- **Steering and queue.** `Enter` while a turn runs steers it; `Alt+Enter` queues
  a follow-up sent after the turn; `Alt+↑` takes the newest follow-up back.
  `Esc` interrupts. A message sent while the Agent still compacts after a turn
  waits in the queue and is sent when the session is idle.
- **Compaction** shows as a status phase and leaves a `[compaction]` box with
  the token counts and the summary behind `Ctrl+O`; loaded content is never
  removed. `/compact` runs it manually.
- **History.** Opening a session loads the latest items; scrolling to the top
  loads older pages, including the history behind a compaction summary.
  A reloaded `bash` card shows its line count and a `click for full output` row;
  `/tool` opens the same view.
- **Views** replace the transcript and keep the footer: diff (turn, session or
  workspace scope), tool output, context budget and help. `Esc` returns.
- **Completion and selectors.** `/` and `@` open a list inside the editor that
  filters as you type (`Tab` inserts, `Enter` runs, `Esc` closes and keeps the
  text). Sessions with delete (`Ctrl+R`), model (`Ctrl+L`), reasoning
  (`Shift+Tab` cycles), find (`Ctrl+F`) and approval prompts replace the editor
  with a bordered selector.
- **Copy and export.** Dragging copies the selection on release (native
  clipboard, OSC 52 fallback) and flashes `Copied!`; double-click selects a
  word, triple-click a line. `/copy` copies the last answer, `/export [path]` writes the
  conversation as Markdown (default `./minicore-<session>-<date>.md`).
- **Composer.** Grapheme-safe editing, undo and redo, prompt history, paste
  tokens for large pastes, external editor on `Ctrl+G`.

## Keys and commands

The complete map is [docs/keybindings.md](docs/keybindings.md). The essentials:

- `Enter` send or steer, `Alt+Enter` follow-up, `Shift+Enter` / `Ctrl+J` newline,
  `Esc` interrupt (idle: clear the draft), `Ctrl+C` clear / quit, `Ctrl+D` quit;
- `Ctrl+R` sessions, `Ctrl+L` model, `Shift+Tab` reasoning, `Ctrl+O` tool cards,
  `Ctrl+T` reasoning text, `Ctrl+F` find, `F1` help;
- `PageUp` / `PageDown`, `Ctrl+Home` / `Ctrl+End`, `Ctrl+↑` / `Ctrl+↓` and the
  wheel navigate; `/` opens the command palette and `@` the file picker;
- `/new`, `/sessions` (`/resume`), `/rename`, `/model`, `/reasoning`, `/compact`,
  `/diff`, `/tool`, `/context`, `/copy`, `/export`, `/theme`, `/reload`, `/help`,
  `/quit`.

## Troubleshooting

- **Agent not found:** pass `--agent` or put `minicore-agent` on `PATH`.
- **Fatal screen:** the Agent exited, sent an unreadable frame or lacks a
  required capability. The screen shows the reason and the last 20 lines of the
  Agent's stderr; `q` or `Esc` quits.
- **Waiting for compaction:** the Agent is still compacting after the last turn;
  your message is queued and sent when it finishes.
- **Older history does not load:** the history changed while it was read. The
  read is retried once on its own; scroll up again if a notice says so.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

The suite needs no Agent: reducer tests drive the app through a scripted
in-process Agent, and `TestBackend` snapshot tests cover the main screen states.
CI runs the same checks on Linux, macOS and Windows with Rust 1.88 and stable.
Module layout and invariants are in [docs/architecture.md](docs/architecture.md).

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at
your option.

[`minicore-agent`]: https://github.com/zqcli/minicore-agent
