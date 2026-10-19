# Architecture

This document describes how `minicore-tui` 0.4 is built. It is derived from the
rewrite design and states the invariants that later changes must keep: the
dependency direction, the single-writer rule, the turn lifecycle, history
paging, the layout cache and the scroll model.

## Layers

```text
┌──────────────────────────────────────────────────────────────┐
│ minicore-tui — terminal and interaction                      │
│ App + reducer, transcript model/layout, rendering, composer  │
└──────────────────────────────┬───────────────────────────────┘
                               │ stdio NDJSON JSON-RPC (Protocol v1)
┌──────────────────────────────▼───────────────────────────────┐
│ minicore-agent — process and backend                          │
│ sessions, store, models, tools, workspace, changes            │
└──────────────────────────────┬───────────────────────────────┘
                               │ in-process
┌──────────────────────────────▼───────────────────────────────┐
│ minicore-runtime — loop and durability semantics              │
└──────────────────────────────────────────────────────────────┘
```

The TUI links neither backend crate. It does not read the Agent store or
workspace, execute tools, call a provider, or recreate an Agent loop. One TUI
process owns exactly one Agent child and one RPC client. Provider credentials
and model catalogs belong to the Agent configuration; the TUI config holds only
local preferences.

## Modules

```text
src/
  main.rs          parse CLI, load config, start the Agent, run the loop
  cli.rs           small flat argument parser
  config.rs        TUI config file (agent, agent_config, theme, editor), ThemeKind
  rpc.rs           Agent child process and JSON-RPC client
  protocol/        serde DTOs only: methods, events, history items, tools,
                   changes, workspace; unknown enum values decode to Unknown
  app/
    mod.rs         App, Msg, main loop glue, input dispatch
    session.rs     session model: transcript, live turn, queue, usage, status
    turn.rs        send, steer, follow-up, cancel, wait; event application
    history.rs     history paging and chunk assembly
    commands.rs    slash command table and execution
    keys.rs        key and mouse events to actions per focused surface
    tasks.rs       spawned async helpers and the app message channel
  transcript/
    block.rs       Block enum, ToolCall model, transcript operations
    layout.rs      per-block line cache, prefix heights, viewport, anchor
  ui/
    mod.rs         draw(frame, &App)
    theme.rs       Palette (colors; plain for NO_COLOR) and Styles (role styles)
    markdown.rs    Markdown to lines, incremental for streaming
    wrap.rs        word wrapping, width, control-character sanitizing
    blocks.rs      render each Block kind, including tool cards
    composer.rs    composer editing model and rendering
    status.rs      status line and footer
    panel.rs       the single bottom-sheet panel style
    sheets.rs      bottom-sheet builders: commands, pickers, approval, find
    picker.rs      fuzzy filter
    views.rs       diff, tool output, context and help views
  terminal.rs      alternate screen, raw mode, panic hook
  clipboard.rs     native clipboard with OSC 52 fallback
  editor.rs        external editor (Ctrl+G)
```

The dependency direction is fixed:

```text
protocol ← rpc ← app → transcript → ui
```

`ui` draws from `&App` and never mutates domain state; it may update layout
caches it owns. `protocol` depends on nothing above it. Rendering code does not
issue RPC calls, and the reducer does not compute layout.

## Process and main loop

`main` parses the CLI, loads the TUI config, and spawns
`minicore-agent --config <path> --stdio` **before** entering the alternate
screen. A spawn failure or an invalid configuration therefore stays an ordinary
terminal message. The TUI then performs the handshake and enters the loop.

```text
loop select! {
  terminal event  -> app.on_input(ev)
  app message     -> app.on_msg(msg)     // RPC results, Agent events, tasks
  tick             -> only while something animates or a deadline is pending
  render deadline  -> draw if dirty, coalesced to at most ~60 fps
}
```

Agent events are drained in batches before drawing, so a burst of deltas
produces one frame. Drawing is skipped when nothing changed. There is one
render path and one owner of the terminal.

### Single writer

All mutable application state lives in `App` and is mutated only on the main
loop. IO runs in spawned tasks that await RPC calls and post results back as
`Msg` values. There is no request-kind registry: each operation is an `async`
function whose completion message carries exactly what the reducer needs.

```rust
enum Msg {
    Agent(AgentEvent),                 // notifications
    Turn(TurnMsg),                     // sent / waited / steered / cancelled
    History { session: String, page: Result<HistoryPage> },
    Sessions(Result<Vec<SessionInfo>>),
    // …
}
```

A result for a different session than the active one is discarded by session
id. Operations that can be superseded by reopening a session are also checked
against one per-session `epoch`, bumped on reopen. No other generation counters
exist.

### RPC client

`Client` is cheap to clone (`Arc` inside):

```rust
client.call::<P, R>(method, params).await -> Result<R, RpcError>
```

Request ids route responses to `oneshot` channels. The transport consists of a
writer task with a bounded queue, a stdout reader with line framing and a 32 MiB
frame bound, a stderr reader that keeps the last ~200 lines of text, and a
child-exit watcher. Notifications are posted to the app channel as
`Msg::Agent`. A request line over 1 MiB is rejected before it is sent, because
the Agent shuts down on an oversized frame.

`RpcError` carries `code`, `kind`, `message` and `retryable`, plus local
`Closed` and `TooLarge` cases. Deferred methods (`turn.wait`, `turn.result`,
`session.compact`, `tool.*`, `workspace.*`, `changes.*`) are ordinary awaits.
The Agent caps concurrent read queries at 4 and deferred operations at 32; a
small semaphore covers query methods, and the few call sites that can hit
`-32019`/`-32020` (history paging, tool output) retry once with a short
backoff.

The handshake calls `agent.ping` and requires `protocol_version == 1` plus the
twelve capabilities actually used: `session.read`, `session.read.display`,
`session.context`, `turn.result`, `tool.read`, `tool.read.display`,
`tool.output`, `workspace.files`, `workspace.search`, `workspace.status`,
`changes.list`, `changes.diff`. Every one is an entry in the Agent's advertised
capability list, so this is a check of capability, not of method name.
`session.presentation` is a method with no capability string, so it is called
unconditionally rather than gated. Shutdown sends `agent.shutdown`, waits up to
3 s, then kills a child that did not exit.

## Domain model

```rust
struct Session {
    info: SessionInfo,
    epoch: u64,
    transcript: Transcript,
    live: Option<LiveTurn>,
    queue: Vec<Queued>,             // steering awaiting apply + local follow-ups
    usage: UsageTotals,
    context: Option<ContextInfo>,
    history: HistoryCursor,
    compaction: Option<CompactionPhase>,
    interaction: Option<PendingInteraction>,
}
```

Only the active session is fully resident. Switching away drops the old
transcript; returning reloads it from the Agent. Switching away from a running
turn asks for confirmation and cancels nothing: the turn keeps running in the
Agent, and the history read on return shows its result.

### Blocks

```rust
enum Block {
    User { text, kind: Prompt | Steering, time },
    Assistant { request: RequestKey, reasoning: Text, text: Text, streaming: bool },
    Tool(ToolCall),
    TurnEnd { outcome, duration, usage, changes: Option<ChangeSummary> },
    Compaction { before, after, summary: Option<String> },
    Notice { level, text },
    EarlierHistory { hidden_items },
}
```

A `ToolCall` is keyed by session, loop, request index and tool-call id; its
state moves through pending, running and a terminal outcome, and its output is
a bounded tail (last 64 KiB or 400 lines). Blocks are addressed by index in the
transcript vector, and tool calls by a `ToolKey` → index map. Each block carries
a `rev` that is bumped on mutation and used only as the layout-cache key.

## Turn lifecycle

The value returned by `turn.wait` is the only thing that ends a turn. Events
are display hints.

1. **Idle submit.** `turn.send`; on success create the live turn, append the
   user block and spawn `turn.wait` immediately. If the Agent answers
   `session_busy` because a post-turn compaction is still running, keep the
   text as a queued follow-up, show `Waiting for compaction` and retry when the
   session reports idle. A draft is never lost.
2. **Events.** `output_delta`, tool events, request events, `steer_progress`,
   `session_state` and interaction events update the live turn. They may arrive
   in any order relative to each other and to responses, so they are applied
   idempotently by key. A non-zero `dropped_before` marks the turn as having a
   gap.
3. **Completion.** When `turn.wait` resolves the turn is over, always. If the
   outcome is not `completed` or the turn had a gap, fetch `turn.result` paged
   and chunk-assembled and **reconcile** its canonical items into the existing
   bounded card model: a canonical `tool_result` becomes the same `ToolCall`
   card, with its full body kept out of the transcript. `turn.result` carries no
   `tool_summaries`, so it is never a second render path and never adopts raw
   bodies directly. Then append the turn-end block, read `changes.list` once for
   the turn scope to fill the change summary, refresh `session.presentation`
   once, and send the next queued follow-up.

   The `(+a −b)` counts in that summary are not in `changes.list`: a
   `ChangeRecord` has only a kind, byte sizes and hashes. They are counted by
   paging each changed file's `changes.diff` to exhaustion, inside the query
   semaphore, with one retry on `-32019`/`-32020`, and only for the first 100
   records of the turn's listing page. That cost is paid once per completed
   turn, never per frame. If a record reports `details_available: false`, its
   diff is binary, truncated or stale, or the listing page overflowed, the
   summary omits the counts and shows only `N files changed`; an unknown count is
   never rendered as zero.
4. **`turn_finished`** only stops the spinner text early. Late deltas for that
   loop are ignored.
5. **Interrupt.** `Esc` sends `turn.cancel`; the UI shows `Interrupting` until
   `turn.wait` resolves with the cancelled outcome.

## History

Opening a session calls `session.open` when needed, then loads the tail in
display view: a probe read at cursor `{0,0}` with `limit: 1` to learn `total`,
`first_item` and the pin, then the last ~60 items (or 512 KiB) from
`max(first_item, total-60)` with that pin. Chunks are assembled per item in the
task, so the reducer receives ready-made items.

The display floor is `first_item`: a read below it is rejected, and a read at
`first_item` spends one limit slot on the summary item. No read ever asks for an
item before `first_item`.

Scrolling to the top loads the previous page with a fresh probe. Items are
append-only, so merging by source index is safe. At the floor the transcript
shows the compaction summary as a block and an `EarlierHistory` row; activating
it starts a canonical read with **its own** probe and pin, and raw bodies are
truncated to the card bounds locally. A stale-pin error (`-32005`) restarts that
one operation with a new probe; it never surfaces as a malformed-history error.

Pins live only inside the operation that uses them: they are never stored on the
session. Every continuation repeats the view, `projection_revision`,
`history_revision` and `captured_end`, because an append or a new summary
invalidates the previous pin.

## Compaction

The status line reports `Compacting context… <phase>` while the Agent reports a
compaction in `session_state.compaction` or in `session.context.current_operation`.
The Agent has no compaction-completed event, so completion is observed from the
`session.compact` result of a compaction the TUI started, or from a
`session.context` read that reports `last_result` / `automatic` for that
operation. Both paths carry an operation id and are deduplicated by it, so a
manual compaction followed by a context read appends exactly one marker. The
marker records the before and after token counts. Loaded blocks stay in the
transcript; the marker is the only change.

## Rendering and performance

### Layout cache

`transcript/layout.rs` stores per block `(rev, width, fold flags) → lines` plus
its height, and a prefix sum of heights that is recomputed lazily from the
first changed block. A frame finds the first visible block by binary search on
the prefix sums and renders only the visible window from cached lines.

Streaming assistant text is split into a stable prefix — everything up to the
last blank line outside a code fence — and a tail. The prefix is rendered once
and cached; each delta re-renders only the tail. Tool card bodies are bounded,
so folded cards are cheap and expanded cards stop at 200 lines.

A folded bash card shows the last output lines of the live turn, because the
live stream delivers them. Display history items do not carry tool bodies —
they expose only `output_line_count` and whether that count is exact — so after
a reload a bash card shows the line count and `open full output` instead of a
tail. The tail is fetched on demand through `tool.output` when the card is
expanded or its full-output view is opened; it is never fetched for every
visible card, which would cost one read per card after each reload.

Selection hit-testing and copy use the visible window and block indices; they
never scan all rows.

### Scrolling

The transcript is either in `Follow` (stick to bottom) or `Anchor { block,
line_offset }`. Any upward scroll switches to `Anchor`, and new content never
moves an anchored view. `Ctrl+End` or reaching the bottom returns to `Follow`.
While anchored, appended content shows a `↓ new output` pill. PageUp/PageDown
move one viewport minus two rows, the wheel moves three rows, and
`Ctrl+↑`/`Ctrl+↓` jump between user messages. Loading earlier history keeps the
anchor.

### Text

Wrapping breaks at whitespace; between CJK or wide characters any break is
allowed; overlong tokens such as URLs and paths hard-break. Indentation of
wrapped list items is preserved, and code blocks wrap by character with a dim
continuation marker. All Agent and tool text is sanitized (control characters
replaced, ANSI stripped) before layout. Width comes from `unicode-width` and
grapheme boundaries from `unicode-segmentation`.

### Budgets

A 140×45 frame renders in at most a few milliseconds regardless of history
length, and a streaming delta costs a fraction of a millisecond in the reducer
for typical messages. Memory is proportional to loaded blocks because tool
output is bounded. These are checked by unit tests over the layout cache and by
the debug timing counters, not by a benchmarking framework.

## UI composition

One screen, one panel style:

```text
transcript (scrollable)
status line (busy only) + one notice line
queue rows (while a turn runs)
composer (1–10 rows, grows with the draft)
footer
```

Bottom-sheet panels (pickers, approval prompts) appear above the composer with
a title row, highlighted selection band and a dim hint row — no box borders.
Full-screen views (diff, tool output, context, help) replace the transcript
area and keep the footer, with a title row and a dim hint row.

Approval and other interactions arrive as `interaction_requested`; the TUI
shows the appropriate sheet (allow/deny, text input or choice list) and answers
with `interaction.answer`. The status line shows `Waiting for approval`.

The footer's model, reasoning and session id come from `SessionInfo`. The
context percentage comes from `session.context`, which is read on open, after a
turn and when the Context view is opened. The git branch is the one value that
is not free, because it costs a workspace query on the Agent side; the TUI takes
it from `session.presentation`, whose `git_branch` caches that answer, and reads
that query on session open and after every turn — never on every frame or every
event. It treats the branch as optional, so the footer shows `—` until the first
answer arrives. The TUI never calls `workspace.status`: `session.presentation`
already reports the branch, and a second source for the same fact would only
invite them to disagree.

On Agent exit or a protocol failure the TUI replaces the screen with a fatal
message containing the reason, the exit status and the last 20 stderr lines;
`q` quits.

## CLI and configuration

```text
minicore-tui [--agent <bin>] [--config <agent.toml>] [--workspace <dir>]
             [--session <id> | --continue] [--profile <id>] [--model <id>]
             [--reasoning <level>] [--theme dark|light] [-h] [-V]
```

The TUI config file (`~/.config/minicore-tui/config.toml` or
`$MINICORE_TUI_CONFIG`) stores `agent`, `agent_config`, `theme` and `editor`.
Command-line values take precedence; `/theme` writes `theme` back.

## Testing

- Unit tests next to pure logic: wrapping, Markdown including incremental
  equivalence with a full render, composer editing, chunk assembly, event
  application, scroll anchoring and the layout cache.
- `TestBackend` render tests for the main screen states (idle, streaming, tool
  cards folded and expanded, a picker, the diff view) compared against plain
  text snapshots.
- One fake-Agent integration test over a child process covering startup, send,
  stream, wait and turn end.
- Real end-to-end runs are manual or scripted through the tmux harness with the
  real model; they are not part of `cargo test`.
