# Architecture

How `minicore-tui` 0.4 is built, and the rules later changes must keep. The
original plan is [design-v0.4.md](design-v0.4.md); where the two differ, this
document and the code win.

## Scope

`minicore-tui` is the terminal frontend of `minicore-agent` (which links
`minicore-runtime`). It talks to one Agent child over stdio NDJSON JSON-RPC
(Protocol v1) and links neither backend crate. It never reads the Agent store or
the workspace, runs a tool, calls a provider or keeps credentials; the TUI config
holds local preferences only.

## Modules

```text
src/
  main.rs       CLI, config, spawn the Agent, terminal guard, main loop
  cli.rs        flat argument parser and USAGE
  config.rs     TUI config file (agent, agent_config, theme, editor), ThemeKind
  rpc.rs        Agent child process and JSON-RPC Client
  protocol/     serde DTOs and constants only: methods, error codes, required
                capabilities, read sizes (mod); event; read (history items, chunk
                assembler, pins); tool; changes; workspace; stamp
  app/          the reducer; all mutable application state
    mod.rs        App, Msg, SessionOp, Scroll, notices, scrolling
    session.rs    Session: transcript, live turn, queue, usage, history window
    actions.rs    Action (keys and commands land here) and the IO each starts
    ops.rs        applies SessionOp results and Agent events
    tasks.rs      async operations: bootstrap, waits, history reads, changes, export
    keys.rs       key, paste and mouse dispatch per focused surface
    commands.rs   slash-command table and parsing
    overlay.rs    Overlay (the one open picker or view) and Panel
    diff.rs, tool.rs, find.rs   state and reads of the diff view, tool view, find
    tests/        reducer tests through the scripted Agent
  transcript/   block.rs (Block, ToolCall, Transcript, history to blocks, export)
                layout.rs (per-block line cache, prefix heights, viewport, anchors)
  ui/           mod (draw), transcript, blocks, rails, markdown, wrap, composer,
                status (status line, queue, notice, footer), panel + sheets + picker
                (the one bottom-sheet style), selection, theme (Palette, Styles),
                views/{diff, tool, context, help}, snapshots (test only)
  terminal.rs   alternate screen, raw mode, paste, mouse, kitty keys, panic hook
  clipboard.rs  pbcopy / xclip / clip with an OSC 52 fallback
  editor.rs     external editor for Ctrl+G
  testing.rs    FakeAgent: scripted in-process Agent behind a real Client (test only)
```

Dependency direction: `protocol` ← `rpc` ← `app` → `transcript` → `ui`.
`transcript` and `ui` never call the Agent. `ui::draw` takes `&mut App` only to
update the layout cache and the rows recorded for copy. `app` reuses `ui` pieces
that are models or row types (`composer`, `sheets`, `views::{diff, tool}`), so
the module graph is not acyclic, but domain state has exactly one writer.

## Main loop and message flow

`main` spawns `minicore-agent --config <path> --stdio` **before** entering the
alternate screen, so a missing binary stays an ordinary terminal error. It then
starts `tasks::bootstrap` (ping, capability check, models, profiles, sessions)
and loops on one `biased` `select!`: Agent events, `Msg` values from tasks,
terminal input, a 100 ms tick (armed only while something animates) and the frame
deadline (armed only while dirty). A frame is drawn at most every 16 ms, so a
burst of deltas costs one draw. After bootstrap the loop opens a session once:
`--session`, else the newest for the workspace with `--continue`, else a new one.

### Single writer

`App` is mutated only on the main loop. IO runs in spawned tasks that await RPC
calls and post one message. There is no request-kind registry.

```rust
enum Msg {
    Agent(Box<AgentEvent>),
    AgentClosed(String),
    Ready(Box<Ready>),                      // catalogs from bootstrap
    Opened(..), Created(..), Deleted(..),   // session lifecycle
    Session(u64, Box<SessionOp>),           // result for the session at epoch .0
    Notice(NoticeLevel, String),
    DraftEdited(Option<String>),
}
```

`SessionOp` holds the results of everything done for the resident session
(`Tail`, `Page`, `Waited`, `Sent`, `Context`, `Presentation`, `Changes`,
`ToolRead`, `DiffFile`, …). `App::spawn` and `App::call` stamp each task with
the resident session's `epoch`; `on_msg` drops a `Session` message whose epoch is
not the resident one. `adopt` makes an opened or created session resident with
`epoch + 1`, so every open or create, even of the same session, orphans what is
still in flight for the old one. There are no other generation counters.

Only the active session is resident; opening another drops the old transcript
and coming back reloads it. A turn keeps running in the Agent meanwhile.

### RPC client

`Client` is shared (`Arc`): `call::<P, R>(method, &params).await`, with
responses routed by id to `oneshot` channels. Four tasks own the pipes: a writer
(queue of 64), a stdout reader (32 MiB frame bound), a stderr reader (last 200
lines) and a child waiter. A malformed frame or a child exit ends every pending
call and posts `Incoming::Closed`, which becomes the fatal screen. A request line
over 1 MiB is refused locally because the Agent exits on one.

Read queries (`session.read`, `turn.result`, `tool.*`, `workspace.*`,
`changes.*`) hold one of 3 client permits, under the Agent's cap of 4. Capacity
refusals (`-32019`, `-32020`) are retried only in `turn.wait` (every 700 ms,
indefinitely) and `changes.diff` (twice); elsewhere they become a notice.

The handshake requires `protocol_version == 1` and the ten capabilities in
`REQUIRED_CAPABILITIES` (`protocol/mod.rs`; `workspace.search` and
`workspace.status` are not used). `session.presentation` has no capability
string and is called unconditionally. Shutdown sends `agent.shutdown`, waits up
to 3 s, then aborts the pipe tasks; the child is killed on drop.

## Turn lifecycle

The `turn.wait` result is the only thing that ends a turn; events are display
hints.

1. **Submit.** The user block is appended at once, then `turn.send`. On success
   `start_turn` creates the `LiveTurn` and spawns `turn.wait`. It is the only
   place a wait is registered and does nothing while a turn is live, so a turn
   adopted from `turn_started` or `session.state` is never waited on twice.
   Opening a session reads `session.state` once its tail is installed, so a turn
   still running in the Agent is adopted and waited on. On
   `session_busy` (post-turn compaction) the prompt block is removed, the text
   becomes a queued follow-up, the status line says `Waiting for compaction`, and
   `session.state` is polled every 700 ms until the session accepts a turn. Any
   other refusal puts the text back in the draft.
2. **Events** (`output_delta`, `tool_*`, `request_started`, `steer_progress`,
   `session_state`, `interaction_*`) update the live turn's blocks by key, so
   order does not matter. Deltas and tool starts for a loop that is not live are
   ignored. `dropped_before > 0` marks the turn as having a gap.
3. **`turn.wait` resolves** and the turn is over. `wait_turn` retries capacity
   and busy refusals. If the wait fails otherwise it asks `turn.result` (limit 1)
   for the stored outcome; with none stored it waits again; only if that read
   fails too does the turn end as `Unknown` with an error notice. `end_turn` then
   closes streaming blocks, adds usage to the totals, appends the turn-end block
   and turns unapplied steers into follow-ups. If the outcome is not `completed`
   or the turn had a gap, its durable items are read with `turn.result` (paged,
   chunk-assembled) and **reconciled**: they replace the blocks the turn streamed,
   keep what only live events knew (duration, exit code, expansion) and leave the
   turn-end block last. In every case `changes.list` for the turn fills the change
   summary, `session.presentation` and `session.context` are refreshed, and the
   next follow-up is sent.
4. **`turn_finished`** only changes the phase text. A wait answer for another
   loop leaves the live turn running.
5. **Interrupt.** `Esc` sends `turn.cancel` once; the status line shows
   `Interrupting` until `turn.wait` resolves.

The `(+a −b)` counts of the turn-end line are not in `changes.list`; they come
from paging `changes.diff` for each of the first 100 records. A record without
details, or with a binary, truncated, stale or unreadable diff, makes the total
unknown, and the line then shows only `N files changed`, never a zero.

Steering: `Enter` while busy queues a `Steer` entry and calls `turn.steer`;
`steer_progress` moves applied steers into the transcript. Refused or unapplied
steers become follow-ups, which `Alt+Enter` also queues locally. The first prompt
of an untitled session becomes its title through `session.rename`.

## History

Opening a session reads the tail in the Agent's **display** view. A probe read
(`limit: 1`) returns `total`, `first_item` and the snapshot pin; then
`[max(first_item, total − 60), total)` is read in pages of up to 20 items or
256 KiB, each continuation repeating the pin (`projection_revision`,
`history_revision`, `captured_end`). Chunks are assembled and decoded in the
task, so the reducer gets ready-made items. A pin lives only inside the
operation that took it. A stale pin (`invalid_state`) restarts that operation
once with a fresh probe; a second failure is reported, never as malformed
history.

- **Display floor.** `first_item` is the oldest item the display view serves; the
  item there is the compaction summary, standing in for the original one. Older
  items sit behind it and the transcript shows an `EarlierHistory` row
  (`↑ N earlier items · home to load`).
- **Older pages.** Scrolling to the top, or `Ctrl+Home`, calls `load_older`: a
  display page of 20 items above the loaded window with a new probe or, once the
  window reaches the floor, a **canonical** page with its own probe and pin.
  Items are append-only, so pages merge by source index. New blocks go in front,
  the scroll anchor shifts by the growth and the live turn's indices are rebased.
- **Backfill.** A tail with no assistant answer (an automatic compaction covered
  everything) reads up to four more pages, so the session does not open empty.
- **Tool cards.** Display items carry per-call summaries (detail, input, state,
  line count) but no bodies. Canonical items carry no summaries, so each card of
  a canonical page is read once with `tool.read {display: true}`; a failed read
  leaves the card as is. The tool view repeats that read and pages `tool.output`
  per stream (`stdout` and `stderr` for `bash`, else `output`), up to 1 MiB each.
- `/export` reads the whole canonical history the same way and writes Markdown.

## Compaction

`session_state.compaction` and `session.context.current_operation` drive the
`Compacting context` status and the `● compacting` footer state. The Agent has
no completed event, so the one-line marker comes from the `session.compact`
result of a compaction the TUI started, or from `last_result` in a
`session.context` read (on open, after each turn, when the Context view opens).
Both carry an operation id and `push_compaction` skips one it already shows.
Loaded blocks are never removed. A summary replayed from history is a block of
the tail, shown under its marker with `Ctrl+T`.

## Rendering

A frame touches only what is visible, and cost follows the changed block, not
the history length.

- **Block cache.** `Transcript` keeps a revision per block, bumped on every
  mutation. `Layout::prepare` re-renders a block only when its revision changed,
  and drops everything when the width or the fold flags (`expanded_tools`,
  `show_reasoning`) change; a theme change clears it explicitly. Prefix heights
  are rebuilt from the first changed block. Tool cards and assistant blocks are
  found by a scan from the end, where live blocks sit; there is no key map.
- **Viewport.** A frame binary-searches the prefix heights and copies only the
  visible rows (`Layout::visible`). The rows drawn are kept in `App::rendered`;
  drag-to-copy and click hit-testing read those and the viewport, never all rows.
- **Streaming Markdown.** A streaming assistant block renders through a
  `StreamRenderer`. It scans line by line for the last cut later text cannot
  change (a blank line outside a code fence, once the next line shows that a
  list, quote or table has ended), caches the lines before it and re-renders only
  the tail per delta. Tests assert that the incremental result equals a full
  render at every byte. While streaming, reasoning shows its last three lines.
- **Cards.** Tool output is a bounded tail (400 lines, 64 KiB). Folded bodies
  take 3 to 8 rows by kind, expanded ones stop at 200; the rest is in the tool
  view. Live `bash` cards show the output tail. Display history has no bodies,
  so a reloaded card shows `… N lines · click for full output` from the line
  count in its summary; that row (or `/tool`) opens the full output.
- **Scroll anchoring.** `Scroll` is `Follow` (stick to the bottom) or
  `Anchor { block, offset }`. Any upward scroll turns the position into an
  anchor, resolved against the current layout each frame, so growth below it
  moves nothing; reaching the bottom returns to `Follow`. While anchored and not
  at the bottom the last row carries a centred ` ↓ Jump to latest message · End `
  label, and a scrollbar overlays the right column for a second after a scroll.
- **Text.** Agent and tool text passes `wrap::sanitize` (control characters and
  ANSI removed) before layout; wrapping never splits a grapheme.

The transcript and the bottom band leave column 0 as a gutter. The band is plain
rows so the editor can grow: queue rows, the Working line (busy only), the
latest notice, the editor band (4–12 rows, plus the `/` or `@` completion rows
from `sheets::completion`), the footer. A selector sheet (sessions, models,
reasoning, approval, find) replaces the editor band; views replace the
transcript area.

### Footer context figure

The footer shows `ctx N%` only from `session.presentation.context`, as the Agent
reported it for the last request, and `ctx ?` otherwise; the TUI never
substitutes an estimate. `presentation` is read on open, after each turn and
compaction, and on request-usage events at most every 2 s while a turn runs. The
Context view also shows the budget's history estimate, labelled as one. The git
branch comes from the same answer, which only caches it after a complete
`workspace.status`, so one runs on open and after each turn; until then the
branch is omitted.

On Agent exit or a protocol failure the screen becomes a fatal message with the
reason and the last 20 stderr lines; `q` or `Esc` quits.

## Code and comment style

- `///` is one sentence, only where the name and signature leave something
  unsaid.
- `//` is only for why (a constraint, an ordering, a trap), at most two lines.
- No narration of what the next lines do and no spec or design citations.

## Testing

Plain `cargo test`; there is no `tests/*.rs` target. Unit tests sit next to pure
logic (wrapping, Markdown incremental-equals-full, composer, chunk assembly,
layout cache, config, CLI). `src/app/tests` drives a real `App` through
`testing::FakeAgent` over an in-memory pipe with paused time. `ui/snapshots.rs`
compares `TestBackend` frames with `tests/snapshots/*.txt` (`UPDATE_SNAPSHOTS=1`
rewrites them); `tests/fixtures/agent-events.jsonl` feeds the event decoder test.
Real-model runs are manual.
