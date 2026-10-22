# Changelog

## Unreleased

- Built-in backend: Agent 0.7.5, pinned by tag. Thinking from DeepSeek through
  a Responses proxy (the legacy `response.reasoning.delta` event) now shows,
  and providers that report no cache writes still report input tokens.
- A session exists only once you send a message. Starting without `--session`
  or `--continue` (or with `--continue` and nothing to continue) shows the
  conversation screen with an empty transcript; the first `Enter` creates the
  session and sends the text, and a failed creation puts the text back in the
  editor. Quitting before that leaves no empty session in the list.
- `/new [profile]` leaves the open session for that empty screen instead of
  creating one. Before the first message `/model`, `/reasoning`, `Ctrl+L`,
  `Ctrl+P`, `Shift+Tab` and `/new <profile>` choose the settings the session
  will start with, and the footer shows them; the commands that need a session
  and the `@` file picker say `No session yet: send a message first`.
- The welcome header (name, version, key hints, workspace) is gone: the
  transcript starts with the conversation, and the footer already shows the
  workspace.
- An expanded tool card always has a `click for full output` row directly
  under its title or command, above the output, for every tool. Clicking the
  title collapses the card and that row opens the tool view; clicking the
  output or the `Took` footer no longer collapses the card.

## 0.5.4 — 2026-10-22

- Built-in backend: Agent 0.7.4 with Runtime 0.7.4, pinned by release tag.
- A history too large to send is compacted first: the send waits under
  `Compacting context` and then runs. When that compaction fails, the text
  returns to the draft with a notice naming `/compact` and `/new`, and any
  failed automatic compaction now leaves an error notice.
- An open session takes a reloaded system prompt from its next turn, and a
  reopened session takes the current one; the new prompt replaces the old.
- On Windows, copying goes through the Win32 clipboard instead of `clip.exe`,
  which kept a byte order mark in the copied text. Pasted, recalled and drawn
  text drops byte order marks, which shifted the editor caret off its text.

## 0.5.3 — 2026-10-21

- Built-in backend: Agent 0.7.3 with Runtime 0.7.3, pinned by release tag.
- No minimum Rust version is declared or tested; build with current stable.
  Every dependency is at its latest release, including toml 1 and toml_edit
  0.25; the Agent no longer forces shared crates down.
- CI runs Clippy on Linux, macOS and Windows.
- The configuration examples use a public provider URL, and the repository no
  longer ships `reference.png`.

## 0.5.2 — 2026-10-21

### One executable

- The Agent is built into `minicore-tui`: the TUI starts itself as
  `minicore-tui agent --config … --stdio` and still talks to it over stdio
  JSON-RPC, so the process boundary, stderr capture and protocol checks are
  unchanged. Packages hold only `bin/minicore-tui`.
- `--agent` and `[agent] binary` now name an external Agent to run instead of
  the built-in one; there is no `bin/minicore-agent` or `PATH` lookup any more.
- Built-in backend: Agent 0.7.2 (`141396f`) with Runtime 0.7.1 (`c0181d1`).

### Terminal rendering and local time

- Paint with the hardware cursor hidden, position the editor caret before showing
  it, and leave the cursor hidden when painting or positioning fails. This
  prevents stray cursors beside the Working spinner and changing timer digits.
- On a Windows console that speaks ANSI, a frame is written in one flush inside a
  synchronized update. Other terminals and the legacy Windows console flush as
  before.
- Request a steady block cursor, restoring the terminal default on exit, panic
  cleanup and external-editor handoff.
- Bundle IANA time-zone data on platforms without system zoneinfo, notably
  Windows, so user-message timestamps use the system zone instead of silently
  falling back to UTC. Linux and macOS keep reading the system zoneinfo.

### Composer and paste

- Click to place the caret, drag or Shift+click to select, and extend or shrink
  selections with Shift+arrows or Shift+Home/End. Word modifiers select by word;
  graphemes and paste chips remain atomic across wrapping and scrolling.
- Replace selections by typing, pasting or inserting newlines; delete and
  undo/redo selections. Ctrl+C copies a selection and Ctrl+X cuts it, then drop
  it, so a second Ctrl+C quits again; a cut the clipboard would refuse keeps the
  text. Without a selection both keep their previous meaning.
- Selecting inside a recalled prompt no longer loses the draft `↓` returns to,
  and Shift+Home/End with no draft scroll like Home/End again.
- On Windows the console reports a bracketed paste as its keys; the markers are
  decoded so embedded commands, Ctrl+J and trailing newlines stay draft text, and
  a paste whose end marker never comes is released after a second instead of
  swallowing all input. Unbracketed multiline bursts within 30 ms count as one
  paste. Linux and macOS pass crossterm's paste events through with no delay.
- Keep editor hit targets and paste input out of full-screen views, approval
  sheets and completion rows.
- A tool card or thinking block opens and closes downward, whether by click,
  `Ctrl+O` or `Ctrl+T`: the top of the view stays put. `Ctrl+O` and `Ctrl+T`
  confirm with a flash, so the dock does not move the view either.
- Folded thinking always keeps its newest rows under an `earlier` hint, while it
  streams and after, instead of switching to its first rows when it ends.
- A drag in the editor copies on release, like one in the transcript, and the
  editor selection has a stronger background.
- A transcript selection takes only content: rails, padding, card durations,
  expand hints, timestamps, turn-end lines and the welcome header are neither
  highlighted nor copied.

### UI simplification

- Remove the empty editor hint, running-card spinner and live timer, the duplicate
  Working elapsed timer, and footer profile/redundant run-state labels.
- Stop invalidating running-card caches on every animation tick; real tool events
  still refresh them. Preserve the main Working spinner, completed tool durations,
  footer elapsed/model/reasoning/usage/context, queue and allowed-tool indicators,
  and all actual profile and approval behavior.

## 0.5.1 — 2026-10-21

0.5.0 was never tagged; 0.5.1 is the first 0.5 release and includes everything
below.

### Platforms and packaging

- CI uploads a package per platform (Linux x86_64, macOS arm64 and x86_64,
  Windows x86_64): `bin/minicore-tui` with the pinned `bin/minicore-agent`, the
  README, the changelog, the configuration reference and `BUILD.txt`.
- On Windows the bundled Agent is found as `bin/minicore-agent.exe`.
- The backend pins are Agent 0.7.1 (`004dbf2`) and Runtime 0.7.1 (`c0181d1`),
  by full SHA.
- The test suite passes on Windows: render fixtures need no child process,
  configuration tests compare paths the way the platform writes them, and the
  escape-sequence test runs only where crossterm writes sequences.

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

### Performance

- A running turn costs about half the CPU: the dock layout is plain `Rect`
  arithmetic instead of a constraint solver, rows are painted without ratatui's
  grapheme walk when they are ASCII or a lone narrow character, and terminal
  input no longer spins on `sched_yield`.
- A flash, the scrollbar or an info notice wakes the loop once, when it
  expires, instead of ticking 12 times a second for as long as it shows.
- Measured over an eight-turn, 70-minute coding session (gpt-6-luna, max
  reasoning, 140x45, release build): the TUI holds 7.6–10 MB and uses 0.9 % of a
  core while a turn runs (1.8 % before this release), 0 % idle. pi 0.87.1 with
  pi-rail-ui on the same prompts used 200–280 MB and 6–12 %.

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
