# minicore-tui v0.4 — Rewrite Design

Status: authoritative plan for the v0.4 rewrite on branch `refactor/v0.4`.
Breaking changes are accepted (CLI, config, keys, commands, internal APIs).
Backend: minicore-agent 0.7.0 (`4102719`, Protocol v1), Runtime 0.7.0 (`b93aea3`).

Inputs (read before working):

- `/tmp/mct-v04/analysis/architecture.md` — v0.3 architecture + measured hotspots.
- `/tmp/mct-v04/analysis/rpc.md` — complete Agent 0.7.0 wire reference (normative
  source remains `/Users/zzq/Develops/minicore-agent/docs/rpc.md`).
- `/tmp/mct-v04/analysis/features-ux.md` — v0.3 feature/UX inventory and critique.
- `/tmp/mct-v04/baseline/README.md`, `/tmp/mct-v04/live-baseline/README.md` —
  observed defects of v0.3 with a mock and with a real model (gpt-6-luna).
- v0.3 source for reference only: `/Users/zzq/Develops/minicore-tui.worktrees/v03-ref`.

## 1. Goals

1. Carry large, long-running coding sessions smoothly: hours of work, hundreds
   of turns, thousands of tool calls, repeated compaction, max-reasoning waits.
2. One coherent pi-style "rail" UI (see `reference.png`): transcript, one status
   line, composer, one footer. One visual system for every panel. English only.
3. Never stuck, never lying: the UI state always converges to the Agent's
   authoritative state (`turn.wait`, `turn.result`, `session.state`).
4. Elegant, lean code. No speculative abstraction, no defensive re-checking of
   invariants, no spec-citation comments, no generation counters per field.
   Target size: roughly 15–25k lines including tests (v0.3: 102k).

Non-goals: inline (non-fullscreen) mode, session branching/fork, MCP/plugins,
images, i18n, a TUI-side permission layer, `!bash` local shell.

## 2. Defects that the design must eliminate (from the baselines)

| v0.3 defect | Design answer |
|---|---|
| Completed turn stuck "incomplete" + "Unsupported interaction" after auto compaction; Esc can't clear | Turn lifecycle is driven by `turn.wait` result only (§5.3). Events are hints. A turn ends exactly when its `turn.wait` resolves. |
| Ctrl+R reopen fails with `invalid_state` (stale history pin) | Pins are never cached across operations; every read operation starts with a fresh probe (§5.4). |
| Prose/table cells wrapped inside words | Real word wrapping with CJK-aware break opportunities (§6.3). |
| Post-turn compaction replaces the answer the user is reading | Compaction never removes loaded content; it inserts a one-line marker (§5.6). |
| PageUp/Ctrl+Home ineffective while streaming | Scroll position anchored to content, not to the bottom (§6.2). |
| Live streaming re-parses the whole message per frame; O(rows×sections) selection; uncapped tool cards | Incremental markdown, per-block layout cache, viewport-only work, capped card bodies (§6). |
| Fatal screen shows stderr byte counts | Show the agent's stderr tail text (Agent stderr carries no secrets by its contract). |
| Approval/interaction unsupported | Inline interaction prompt (§7.6). |
| workspace.files/search cursors broken on 0.7.0 | Cursors are opaque `serde_json::Value`, passed back verbatim. |
| Three visual systems, Chinese/English mix, raw enum debug text | One panel style, English copy, human labels only. |
| Context page contradictory numbers | One clear context summary (§7.8). |

## 3. Architecture

Single-threaded state + async IO tasks. The `App` is mutated only on the main
loop. IO happens in spawned tasks that `await` RPC calls and post results back
as messages. There is no request-kind registry: each operation is an `async`
function whose completion message carries what the reducer needs.

```
src/
  main.rs          parse CLI, load config, start agent, run terminal loop
  cli.rs           clap-free small arg parser (keep v0.3 style) 
  config.rs        TUI config file (theme, editor); never Agent credentials
  rpc.rs           Agent child process + JSON-RPC client (see §4)
  protocol/        serde DTOs only (methods, events, history items, tools,
                   changes, workspace). Unknown enum values decode to `Unknown`.
  app/
    mod.rs         App struct, Msg enum, main loop glue, dispatch
    session.rs     Session model: transcript, live turn, queue, usage, status
    turn.rs        send / steer / follow-up / cancel / wait; event application
    history.rs     history paging + chunk assembly (async ops)
    commands.rs    slash command table + execution
    keys.rs        key/mouse → actions for the focused surface
    tasks.rs       spawn helpers, the app message channel
  transcript/
    block.rs       Block enum and ToolCall model
    layout.rs      per-block line cache, prefix heights, viewport, scroll anchor
  ui/
    mod.rs         draw(frame, &App)
    theme.rs       palettes
    markdown.rs    markdown → lines (port of v0.3 core, + incremental streaming)
    wrap.rs        word wrapping, width, sanitizing control chars
    blocks.rs      render each Block kind to lines (user, assistant, reasoning,
                   tool card, notices, markers)
    composer.rs    composer widget (editing model + render), no tui-textarea
    status.rs      status line + footer
    panel.rs       the single bottom-sheet panel style (title, list, hints)
    picker.rs      generic filterable list (commands, sessions, models,
                   reasoning, @files)
    views.rs       full-screen views: diff, tool output, context, help
  terminal.rs      enter/restore, panic hook (port v0.3)
  clipboard.rs     native clipboard + OSC 52 fallback (port v0.3)
  editor.rs        external editor (Ctrl+G)
```

Names may change while implementing; responsibilities and dependency direction
may not: `protocol` ← `rpc` ← `app` → `transcript` → `ui`. `ui` draws from `&App`
and never mutates domain state (it may update layout caches it owns).

### 3.1 Main loop

```text
loop select! {
  terminal event  -> app.on_input(ev)
  app message     -> app.on_msg(msg)      // RPC results, agent events, task results
  tick (only while something animates or a deadline is pending)
  render deadline -> draw if dirty (cap ~60 fps; coalesce bursts)
}
```

Agent events are drained in batches before drawing. Drawing is skipped unless
dirty. No duplicate render paths.

### 3.2 Messages, not request kinds

```rust
enum Msg {
    Agent(AgentEvent),                       // notifications
    Turn(TurnMsg),                           // sent / waited / steered / cancelled
    History { session: SessionId, page: Result<HistoryPage> },
    Sessions(Result<Vec<SessionInfo>>),
    ...
}
```

Stale results are discarded by checking the session id (and, where an operation
can be superseded, a single per-session `epoch: u64` bumped when the session is
reopened). Do not add other generation counters.

## 4. RPC client

- `Client` is `Clone` (`Arc` inside). `client.call::<P, R>(method, params).await
  -> Result<R, RpcError>`. Responses are routed by id to a `oneshot`.
- Writer task (bounded queue), stdout reader task (line framing with `memchr`-
  style splitting, 32 MiB max frame), stderr reader keeping the last ~200 lines
  of text, child-exit watcher. Notifications go to the app channel as
  `Msg::Agent`. Request line limit 1 MiB checked before sending (the Agent
  terminates on oversized frames).
- `RpcError { code, kind, message, retryable }` plus `Closed`/`TooLarge`.
- Deferred methods (`turn.wait`, `turn.result`, `session.compact`, `tool.*`,
  `workspace.*`, `changes.*`) are ordinary awaits. Respect the Agent caps
  (4 concurrent queries, 32 deferred): a small semaphore for query methods;
  retry `-32019`/`-32020` with a short backoff only in the few call sites that
  need it (history paging, tool output).
- Handshake: `agent.ping`, require `protocol_version == 1` and the capabilities
  actually used (`session.read`, `session.read.display`, `tool.read`,
  `tool.read.display`, `tool.output`, `turn.result`, `session.context`,
  `workspace.files`, `workspace.search`, `changes.list`, `changes.diff`).
- Shutdown: `agent.shutdown`, then wait up to 3 s, then kill.

## 5. Domain model and flows

### 5.1 Session

```rust
struct Session {
    info: SessionInfo,
    epoch: u64,
    transcript: Transcript,        // ordered blocks + layout cache
    live: Option<LiveTurn>,        // the running turn, if any
    queue: Vec<Queued>,            // steering (sent, awaiting apply) + follow-ups (local)
    usage: UsageTotals,            // session totals for the footer
    context: Option<ContextInfo>,  // last presentation/context read
    history: HistoryCursor,        // what range of source items is loaded
    compaction: Option<CompactionPhase>,
    interaction: Option<PendingInteraction>,
}
```

Only the active session is fully resident. Switching sessions drops the old
transcript (it is reloaded from the Agent on return). Background loops of other
sessions are not supported in v0.4: switching away while a turn runs asks for
confirmation and cancels nothing — the turn continues in the Agent; when the
user comes back the history read shows its result (simple and honest).

### 5.2 Transcript blocks

```rust
enum Block {
    User { text, kind: Prompt|Steering, time },
    Assistant { request: RequestKey, reasoning: Text, text: Text, streaming: bool },
    Tool(ToolCall),
    TurnEnd { outcome, duration, usage, changes: Option<ChangeSummary> },
    Compaction { before, after, summary: Option<String> },   // one-line marker, expandable summary
    Notice { level, text },                                   // errors that belong to the conversation
    EarlierHistory { hidden_items },                          // "↑ N earlier messages · load"
}
struct ToolCall {
    key: ToolKey,                 // session, loop, request_index, tool_call_id
    name, detail, expanded_input: Option<String>,   // from ToolDisplay
    args_preview: Option<String>,                   // tool_arguments_preview while generating
    state: Pending|Running|Succeeded|Failed|Denied|Cancelled,
    started_at, finished_at,
    output: OutputTail,           // bounded tail (last 64 KiB / 400 lines); bash stdout+stderr chunks
    output_lines: Option<u64>, exit_code: Option<i64>, truncated: bool,
}
```

Each block carries `rev: u64` bumped on mutation (used only as the layout-cache
key). Blocks are identified by index in the transcript vector plus a stable id
for tool calls (`ToolKey` → index map).

### 5.3 Turn lifecycle (authoritative)

1. Enter (idle): `turn.send` → on success create `LiveTurn{loop_id}` and
   immediately spawn `turn.wait`. Append the user block locally.
   `session_busy` (post-turn compaction still running) → keep the text queued
   as a follow-up and retry when `session.state` shows idle (poll every 1 s,
   status line says "Waiting for compaction…"). Never lose a draft.
2. Events (`output_delta`, `tool_*`, `request_*`, `steer_progress`,
   `session_state`, `interaction_*`) update the live turn's blocks. They are
   hints: order across queues is not guaranteed; apply idempotently by key.
   `dropped_before > 0` marks the live turn `gap = true`.
3. `turn.wait` resolves → the turn is over, always. Then:
   - if `gap` or outcome is not `completed`: fetch `turn.result` (paged,
     chunk-assembled) and replace this turn's blocks with the durable items
     (tool summaries from `tool_summaries`, finished state from records);
   - append `TurnEnd` (outcome, duration, usage, `requests`, `tool_rounds`);
   - fetch `changes.list {scope:{turn:{loop_id}}}` once → fill the change summary;
   - refresh `session.presentation` (context %, branch) once;
   - send the next queued follow-up, if any.
4. `turn_finished` event only clears the spinner text early; it never ends the
   turn. Late `output_delta` after `turn.wait` for that loop are ignored.
5. Esc while running → `turn.cancel`. The UI shows "Interrupting…" until
   `turn.wait` resolves with `cancelled`.

### 5.4 History loading

- Open session: `session.open` (if not loaded), then load the tail in display
  view: probe read (`limit:1`) → learn `total`, `first_item`, pin; read the last
  ~60 items (or until 512 KiB) from `max(first_item, total-60)` using that pin;
  assemble chunks per item; convert items to blocks.
- Scroll to top → load the previous page (fresh probe each time; items are
  append-only so merging by source index is safe). When the next older page
  would go below `first_item` (compaction summary), show the summary as a
  `Compaction` block and an `EarlierHistory` block: Enter/click loads earlier
  items with `view:"canonical"` (raw bodies are truncated locally to the card
  bounds).
- A `-32005` stale pin at any point → restart that operation once with a new
  probe. Never surface "malformed history" for a stale pin.
- Decoding happens in the task (off the main loop); the reducer receives
  ready-made items.

### 5.5 Tool cards

Data sources: live events (`tool_started`, `tool_presentation`,
`tool_arguments_preview`, `tool_invocation`, `tool_process` chunks + command
state, `tool_execution`, `tool_finished.result.content` preview) and, for
history, `tool_summaries` in display items. Full output on demand via
`tool.output {stream:"output"|"stdout"|"stderr"}` in the tool view (§7.5).

Rendering (folded, default):

```
▎ ✓ bash  python3 -m unittest -v                      0.4s · exit 0
▎   Ran 30 tests in 0.21s
▎   OK
▎   … 12 earlier lines
```

- Header: status glyph (running spinner, ✓, ✗, ⊘), tool name, detail (path,
  `$ cmd`, range), right-aligned duration and exit code / error.
- Body (folded): bash = last 4 output lines; read = "N lines"; edit /
  apply_patch = first 8 lines of the diff from `expanded_input`, colored, with
  "+a −b" counts in the header; write = first 6 lines + "N lines"; failed tools
  = first 3 lines of the error. Expanded (Ctrl+O global, click per card): up to
  200 lines, then "… open full output (enter)" which opens the tool view.
- Live args preview: while the model is generating a write/edit, show the
  streaming preview in the card (dimmed) before execution starts.

### 5.6 Compaction

- Status line shows "Compacting context… <phase>" while `session_state.compaction`
  or `session.context.current_operation` is present.
- On completion (event, or `session.compact` result, or next `session.context`
  read), append a marker block `─ Context compacted · 32.2k → 2.7k tokens ─`
  (expand shows the summary text when available). Loaded blocks stay.
- `/compact` runs manual compaction (idle only), result shown the same way.

### 5.7 Steering and follow-ups

- While running: Enter = steer (`turn.steer`), Alt+Enter = queue follow-up
  (local, sent after the turn ends). Queue rows render above the composer
  (`↳ steer · text…` pending apply, `⏵ next · text…`), max 3 rows + "+N".
  Applied steers (from `steer_progress`) disappear from the queue and appear as
  a `User{Steering}` block in the transcript. Unapplied steers when the turn
  ends return to the queue as follow-ups (Agent discards them). Alt+Up pulls the
  last queued item back into the composer.

## 6. Rendering and performance

### 6.1 Layout cache

- `transcript/layout.rs` keeps per block: `(rev, width, fold flags) → Arc<[Line]>`
  plus its height, and a prefix-sum of heights (recomputed lazily only from the
  first changed block).
- A frame renders only the visible window: find the first visible block by
  binary search on the prefix sums, render lines from cached blocks.
- Streaming assistant text: split into a stable prefix (complete top-level
  markdown blocks: text up to the last blank line outside a code fence) and a
  tail. Cache the rendered prefix; re-render only the tail on each delta.
- Tool output tail is bounded; folded card bodies are tiny; expanded cards cap
  at 200 lines.
- Selection hit-testing and copy use the visible window and the block index,
  never a scan of all rows.

### 6.2 Scrolling

`Scroll::Follow` (stick to bottom) or `Scroll::Anchor { block, line_offset }`.
Any upward scroll switches to `Anchor`; new content never moves an anchored
view. Ctrl+End / scrolling to the bottom returns to `Follow`. A "↓ new output"
pill shows while anchored and content was appended. PageUp/PageDown = viewport
minus 2 rows. Mouse wheel = 3 rows. Ctrl+↑/Ctrl+↓ jump to previous/next user
message. Loading earlier history keeps the anchor.

### 6.3 Text

- Word wrapping: break at whitespace; between CJK/wide characters any break is
  allowed; overlong tokens (URLs, paths) hard-break. Leading indentation of
  wrapped list items is preserved. Code blocks wrap by character with a dim
  continuation marker.
- All agent/tool text is sanitized (control chars → visible replacements;
  ANSI sequences stripped) before layout.
- Width via `unicode-width`; graphemes via `unicode-segmentation`.

### 6.4 Budgets

Render ≤ 2 ms for a 45×140 frame regardless of history length; a streaming
delta ≤ 0.5 ms reducer cost for typical message sizes; memory proportional to
loaded blocks (tool outputs bounded). Verify with a simple `--bench`-free test
or a debug timing log, not a framework.

## 7. UI / UX

### 7.1 Screen

```
 transcript (scrollable, rail style, scrollbar on hover/scroll)
 …
 ⠋ Thinking · 1m12s · request 7 · 12 tools · esc to interrupt     (status, only when busy)
 ↳ steer · focus on the parser first                             (queue rows, if any)
▎ composer (1–10 rows, grows with content)
 minicore-tui@main · gpt-6-luna · max · ● working       ↑50.5k ↓1.8k · ctx 41%
```

- Status line phases: `Thinking` (reasoning deltas or waiting for model),
  `Writing` (text deltas), `Running <tool> <detail>`, `Waiting for approval`,
  `Compacting context`, `Interrupting`, `Waiting for compaction` (busy retry).
  Always shows elapsed turn time and "esc to interrupt".
- Footer: workspace dir name + git branch, session title (if any, dim), model,
  reasoning, state dot, token totals (input/output/cached), context percent
  (from `session.presentation.context`, `?` when unknown).
- Notices: a single line under the status line, auto-hide after 5 s for info,
  errors stay until the next key press. No notice history panel.
- Fatal (agent exited / protocol error): full-screen message with reason, exit
  status and the last 20 stderr lines; `q` quits.

### 7.2 Blocks

- User: tinted card with editor-blue rail, text only (no timestamp row).
- Assistant: plain markdown at 1-col inset; reasoning above it as italic dim
  text with purple rail, folded to the last 3 lines while streaming and to a
  one-line "Thought for 42s" when done (Ctrl+T toggles showing all reasoning).
- Tool card: §5.5. Turn end: one dim line `── completed in 3m07s · 12 requests ·
  11 tools · 3 files changed (+40 −7) ──`; click/Enter on it opens the diff view
  scoped to that turn. Failed/cancelled turns show the reason in red/yellow.
- One blank line between blocks; no blank padding inside cards.

### 7.3 Composer and keys

| Key | Action |
|---|---|
| Enter | send (idle) / steer (running) |
| Alt+Enter | queue follow-up (running) / newline (idle) |
| Shift+Enter, Ctrl+J | newline (enable crossterm keyboard enhancement when supported) |
| Esc | close panel/view → clear selection → interrupt running turn |
| Ctrl+C | clear composer; on empty composer press twice to quit |
| Ctrl+D | quit when composer empty |
| ↑/↓ | move in multi-line text; at first/last line browse prompt history |
| Alt+↑ | pull last queued message back into the composer |
| Ctrl+G | edit draft in `$VISUAL`/`$EDITOR` (config override) |
| `/` at start | command palette (picker) |
| `@` | file picker (workspace.files / workspace.search) inserting the path |
| Ctrl+R | sessions picker |
| Ctrl+L | model picker |
| Shift+Tab | cycle reasoning level among the model's supported levels |
| Ctrl+O | expand/collapse all tool cards |
| Ctrl+T | show/hide reasoning |
| Ctrl+F | find in transcript (inline bar: Enter next, Shift+Enter prev, Esc close) |
| PgUp/PgDn, Ctrl+Home/End, wheel, Ctrl+↑/↓ | transcript navigation (§6.2) |
| F1 | help view |

Editing: grapheme-safe cursor movement, word motions (Alt/Ctrl+←/→), Ctrl+A/E,
Ctrl+W, Ctrl+U/K, undo/redo (Ctrl+Z / Ctrl+Shift+Z or Ctrl+Y), bracketed paste
(large pastes shown as a `[pasted 120 lines]` token that expands on send).

Mouse: wheel scroll; click a tool card header to toggle it; click a turn-end
line to open its diff; drag to select transcript text, copied on release
(native clipboard, OSC 52 fallback); scrollbar drag.

### 7.4 Commands (palette shows name + one-line description, fuzzy filter)

`/new`, `/sessions` (alias `/resume`), `/rename <title>`, `/model`,
`/reasoning`, `/compact`, `/diff`, `/context`, `/copy` (last answer),
`/export [path]`, `/theme`, `/reload`, `/help`, `/quit`.

Sessions get an automatic title from the first prompt (first line, ≤ 60
chars) via `session.rename` after the first turn is accepted, unless titled.

### 7.5 Panels and views (one style)

- Bottom-sheet panel above the composer (pickers, interaction prompt):
  title row in accent color, rows with the selected row highlighted by a
  background band, hint row in dim text. No box borders.
- Full-screen views replace the transcript area, keep the footer: title row
  `← Diff · session · 4 files` and a dim hint row. Esc returns.
  - Diff view: file list (left, ~30%) + unified diff (right) colored
    (+green/−red/hunk header dim), scope toggle Tab (turn / session /
    workspace), ↑↓ select file, PgUp/PgDn scroll, `c` copy diff.
  - Tool view: full output of one tool call via paged `tool.output`
    (stdout/stderr interleaving not guaranteed → show sections), input
    (`expanded_input`) on top.
  - Context view: plain-English summary: "Context: 31.2k of 40k input budget
    (78%) — auto-compacts at 80%"; last compaction (when, before → after);
    coverage ("Summary covers 6 turns · 289 items"). Numbers come from
    `session.presentation.context` and `session.context`; label estimates
    as estimates.
  - Help: keys and commands.
- Sessions picker rows: `● title-or-first-prompt   model · 3h ago`, current
  workspace first; type to filter; Enter open; Ctrl+D delete (confirm with a
  second Ctrl+D; only closed sessions — close first if loaded and idle).

### 7.6 Interactions (approval profile)

`interaction_requested` shows a bottom-sheet: approval → `Allow once (y)`,
`Deny (n)`; text → an input line; choice → a list. Answer with
`interaction.answer`. Status line: "Waiting for approval".

### 7.7 Theme

Dark (default) and light palettes; respect `NO_COLOR` (no colors, keep
attributes). RGB colors close to `reference.png`: page bg terminal default,
composer band `#404256`-ish, tool card bands tinted by state, thinking rail
purple, user rail blue.

### 7.8 Copy and export

`/copy` copies the last assistant answer markdown. `/export [path]` writes the
full conversation (canonical history paged in a task) as Markdown (default
`./minicore-<session>-<date>.md`).

## 8. CLI and config

```
minicore-tui [--agent <bin>] [--config <agent.toml>] [--workspace <dir>]
             [--session <id> | --continue] [--profile <id>] [--model <id>]
             [--reasoning <level>] [--theme dark|light] [-h] [-V]
```

TUI config: `~/.config/minicore-tui/config.toml` (or `$MINICORE_TUI_CONFIG`):
`agent`, `agent_config`, `theme`, `editor`. `/theme` writes `theme` back.

## 9. Dependencies

Upgrade to current releases: ratatui 0.30, crossterm 0.29 (with
`event-stream`), pulldown-cmark 0.13. Drop `tui-textarea`, `icu_segmenter`,
`tempfile` (use a simple temp path), `time` if `SystemTime` + a tiny formatter
suffices. `tokio` current-thread runtime. Set `rust-version` to the highest
MSRV required by the chosen dependencies (verify on the remote host).

## 10. Testing

- Unit tests next to pure logic: wrap, markdown (incl. incremental streaming
  equivalence: incremental render == full render), composer editing, chunk
  assembly, event application to blocks, scroll anchoring, layout cache.
- A handful of `TestBackend` render tests for the main screen states (idle,
  streaming, tool cards folded/expanded, picker, diff view) comparing against
  checked-in text snapshots (`tests/snapshots/*.txt`, update with an env var).
- One fake-agent integration test (scripted NDJSON over a child process or
  in-memory duplex) covering startup → send → stream → wait → turn end.
- Real E2E is manual/scripted through the remote harness (§11) with
  gpt-6-luna at `max` reasoning, multi-turn coding through the composer,
  at least one compaction.
- Delete the v0.3 test suites, snapshots, scripts, tools and historical docs
  that no longer apply.

## 11. Build and verification environment

- NEVER run cargo on the Mac. Build/test remotely:
  `/tmp/mct-v04/rcargo.sh <worktree> build|test|clippy ...` (see
  `/tmp/mct-v04/harness/README.md`).
- Visual checks: `/tmp/mct-v04/rtui.sh <worktree> '<prompt>'` (mock model),
  `/tmp/mct-v04/rkeys.sh`, `/tmp/mct-v04/rsnap.sh`.
- Real model: `/tmp/mct-v04/rtui-live.sh <worktree>` + `/tmp/mct-v04/rdrive.py`.
  The harness launch command lives in `/root/mct/harness/launch-cmd.sh`;
  update it when the CLI changes.
- Quality gates: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`,
  `cargo test --locked`.

## 12. Agent / Runtime changes

None planned. The analysis found no gap that requires them for v0.4. If an
implementation need appears, stop and escalate with evidence; the backend
repos must not be modified by the team.
