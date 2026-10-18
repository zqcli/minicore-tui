# Changelog

## 0.4.0 — Rewrite

A full rewrite of the frontend against `minicore-agent` 0.7.0 and
`minicore-runtime` 0.7.0 over Protocol v1. Breaking changes to the CLI, the
config file, keys and commands are intentional.

### Turn lifecycle

- A turn now ends exactly when `turn.wait` resolves. Agent events are display
  hints and can no longer leave a completed turn stuck as incomplete; the
  post-compaction "completed but incomplete / Unsupported interaction" state
  and the inability to clear it with `Esc` are gone.
- A `session_busy` answer from `turn.send` (post-turn compaction still running)
  queues the message as a follow-up and sends it when the session is idle. The
  draft is never dropped.
- After a turn with a gap or a non-completed outcome, the durable turn result
  replaces the live blocks so the transcript matches the store.
- Steering and follow-ups share one queue shown above the composer.
  `Alt+Enter` queues a follow-up and `Alt+Up` takes one back.
- Late deltas after a turn ends are ignored instead of extending the turn.

### History and sessions

- History is loaded in display view from a fresh probe on every operation.
  Cached pins are gone, which removes the `invalid_state` failure when
  reopening a session.
- Older pages load as you scroll to the top; below the compaction boundary the
  summary and an "earlier messages" row lead to the canonical view.
- A stale pin is retried once with a new probe rather than reported as
  malformed history.
- Returning to a session reloads it from the Agent; only the active session is
  resident.

### Transcript and rendering

- New per-block layout cache with prefix heights and a viewport-only render
  path. Rendering cost no longer grows with history length, selection
  hit-testing is limited to the visible window, and tool card bodies are
  bounded.
- Incremental Markdown rendering for streaming: the stable prefix is cached and
  only the tail is re-rendered per delta. Streaming no longer re-parses the
  whole message each frame.
- Real word wrapping with CJK-aware break opportunities, preserved list
  indentation, and hard breaks for overlong tokens. Prose no longer breaks
  inside words.
- Scrolling is anchored to content: `PageUp` and `Ctrl+Home` hold the view
  while output streams, and a `↓ new output` pill marks appended content.
- One blank line between blocks, one rail style per block kind, tool cards
  folded by default with duration, exit code and change counts.
- Reasoning folds to one line when a turn finishes and expands with `Ctrl+T`.

### Compaction

- Compaction is shown as a status-line phase and leaves a one-line marker with
  before and after token counts. Loaded content is never removed from the
  transcript, so the answer being read survives a post-turn compaction.
- `/compact` runs manual compaction and reports its result the same way.

### UI

- One visual system: a single bottom-sheet style for every picker and prompt,
  full-screen views for diff, tool output, context and help, and one footer.
  English copy throughout, human labels instead of raw enum names.
- Command palette with descriptions, `@` file picker, session picker with
  delete, model and reasoning pickers, find-in-transcript, mouse selection with
  copy, `/copy`, `/export`, `/theme`, and the external editor.
- Approval and other interactions are answered inline and no longer reported as
  unsupported.
- The fatal screen shows the Agent stderr tail instead of byte counts.
- The composer is a purpose-built editing model (grapheme-safe movement,
  undo/redo, bracketed paste projection) and no longer depends on
  `tui-textarea`.

### Backend and protocol

- Targets Agent 0.7.0: workspace file and search cursors are opaque values
  passed back verbatim, `tool_process` chunks are decoded and bounded, and
  `turn.result` is assembled through the shared chunk reader.
- Handshake requires Protocol v1 and the capabilities actually used.
- The Agent child is spawned before the alternate screen, and shutdown drains
  the child with a bounded wait.

### CLI and configuration

- New CLI: `--agent`, `--config`, `--workspace`, `--session`, `--continue`,
  `--profile`, `--model`, `--reasoning`, `--theme`, `-h`, `-V`.
- The TUI config is `~/.config/minicore-tui/config.toml` (or
  `$MINICORE_TUI_CONFIG`) with `agent`, `agent_config`, `theme` and `editor`.
- Dependencies move to ratatui 0.30, crossterm 0.29 and pulldown-cmark 0.13;
  the runtime dependencies `tui-textarea`, `icu_segmenter`, `time` and
  `tempfile` are gone.
- The package version is 0.4.0.

### Removed

- The v0.3 state machines, request-kind registry and generation counters, the
  old test suites, snapshots, scripts, tools and historical specification
  documents.
