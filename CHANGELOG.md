# Changelog

## Unreleased — 0.5

### Configuration (breaking)

- One TUI directory (`--home`, `$MINICORE_HOME`, else the executable's directory,
  or its parent named `bin`) holds everything: `config.toml` (profiles, `[ui]`,
  `[agent]`, `[loop]`, `[compaction]`), `models.toml` (providers and models),
  `prompt/SYSTEM_PROMPT.md`, `prompt/commands/*.md`, `session/` (all sessions)
  and `history.jsonl`. See [docs/configuration.md](docs/configuration.md).
- The TUI composes the Agent config from the two files into
  `session/agent.toml` on every start. `--config` is replaced by `--agent-config`
  (a raw file, no composition). `~/.config/minicore-tui/`, `$MINICORE_TUI_CONFIG`
  and `prompts/` are no longer read; move templates to `prompt/commands/`.
- A malformed file, unknown key, dangling reference, missing key variable or
  empty system prompt stops the start with `file:line: key: reason` before the
  screen changes, instead of silently falling back to defaults.
- A first run writes commented starter files and exits. `/reload` composes the
  Agent config again; `/theme` keeps the comments in `config.toml`.

### Transcript

- A user message card shows when it was sent, `21:38 · 10/20/2026` in local
  time, on the row under its text; steering cards add `↳ steering`. Live
  messages take the Agent's `accepted_at`, history the stored timestamp; old
  history without one shows no row.
- Rows added to the dock (a steer, a queued follow-up, a notice, an approval)
  no longer hide the bottom of an anchored view, such as a running tool card:
  the view gives up rows at its top instead.

## 0.4.0 — Rewrite

A rewrite of the frontend against `minicore-agent` 0.7.0 (`4102719`) and
`minicore-runtime` 0.7.0 (`b93aea3`) over Protocol v1. The CLI, config file,
keys and commands changed incompatibly. Requires Rust 1.88 or newer.

### Turns

- A turn ends exactly when `turn.wait` resolves; Agent events are only display
  hints. If the wait is lost, the stored `turn.result` ends the turn. A turn can
  no longer stay "incomplete" after a compaction.
- After a gap in events or a turn that did not complete, the turn's durable
  items replace what was streamed, so the transcript matches the store.
- A message sent while the Agent still compacts after a turn is queued and sent
  when the session is idle, instead of being lost.
- `Enter` steers a running turn; `Alt+Enter` queues a follow-up; `Alt+↑` takes
  the queued follow-ups back. Steers the Agent did not apply become follow-ups.
- Approval and choice prompts are answered inline (`y`, `n`, `Enter`).
- The turn-end line shows outcome, duration, requests, tools and, when files
  changed, `N files changed (+a −b)`. If a diff is binary, truncated or
  unreadable, the line gives only the file count rather than a wrong total.
- Untitled sessions are named from their first prompt.

### History and sessions

- The latest items load first; older pages load as you scroll to the top, and
  the history behind a compaction summary is reachable from the
  `earlier items` row. A session that opens on a summary reads back to the
  latest answer.
- Each history read takes its own snapshot pin, so reopening a session no longer
  fails with `invalid_state`; a stale pin is retried once.
- Only the open session is kept in memory; returning to another reloads it. A
  turn keeps running in the Agent while you look at another session.
- `/export` writes the canonical conversation as Markdown.

### Transcript and rendering

- Blocks are rendered once and cached by revision, width and fold state, and
  only visible rows are drawn, so cost no longer grows with history length.
- Streaming Markdown re-renders only the unfinished tail of the message.
- Word wrapping with CJK-aware breaks and preserved list indentation.
- Scrolling is anchored to what you read: streaming output does not move the
  view, and a `Jump to latest message` label shows when you are above the bottom.
- Tool cards are folded by default and bounded: duration, exit code, line
  changes, the output tail of a live `bash` call, and a tool view with the full
  stdout and stderr. After a reload a card shows its line count and a
  `click for full output` row.
- Reasoning folds to its first three rows when finished; `Ctrl+T` collapses it to one row.
- Compaction shows as a status phase and a one-line marker with token counts.
  Loaded content is never removed.

### Interface

- One visual system: a bottom-sheet style for pickers, approval prompts and
  find; full-screen views for diff, tool output, context and help; English copy;
  dark and light palettes, and `NO_COLOR`.
- Command palette with descriptions, `@` file picker, session picker with
  delete, model and reasoning pickers, find in the transcript, drag-to-copy,
  `/copy`, `/export`, `/theme`, `/tool`, `/diff [scope]` and `Ctrl+G` for an
  external editor.
- The footer's `ctx N%` is the context usage the Agent reported for its last
  request; it is `ctx ?` until reported, never an estimate.
- The fatal screen shows the reason and the Agent's stderr tail.
- The composer has grapheme-safe editing, undo/redo, prompt history and paste
  tokens; it no longer depends on `tui-textarea`.

### CLI, configuration and protocol

- Options: `--agent`, `--config`, `--workspace`, `--session`, `--continue`,
  `--profile`, `--model`, `--reasoning`, `--theme`, `-h`, `-V`. `--config` is
  required unless the TUI config sets `agent_config`.
- TUI config at `$MINICORE_TUI_CONFIG`, else `$XDG_CONFIG_HOME` or `~/.config`
  `/minicore-tui/config.toml`, with `agent`, `agent_config`, `theme` and
  `editor` (a list: command and arguments).
- The handshake requires Protocol v1 and ten capabilities (`session.read`,
  `session.read.display`, `session.context`, `turn.result`, `tool.read`,
  `tool.read.display`, `tool.output`, `workspace.files`, `changes.list`,
  `changes.diff`). The Agent is spawned before the alternate screen and shut down
  with a bounded wait.
- Dependencies: ratatui 0.30, crossterm 0.29, pulldown-cmark 0.13. `tui-textarea`,
  `icu_segmenter` and `time` are gone; `tempfile` is test-only.

### Removed

- The v0.3 state machines, request-kind registry, generation counters, test
  suites, scripts and historical specification documents.
