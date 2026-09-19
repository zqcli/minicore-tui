# Architecture

`minicore-tui` is a small, concrete frontend rather than a general TUI
framework. Its useful seam is the stdio JSON-RPC process: the Agent owns
sessions, storage, models, tools, and execution; the TUI owns terminal state,
interaction state, rendering, and protocol adaptation.

## Three Layers

```text
┌──────────────────────────────────────────────────────────────┐
│ minicore-tui: terminal/UI layer                              │
│ App state, App::update, keymap, transcript rendering/cache   │
└──────────────────────────────┬───────────────────────────────┘
                               │ stdio NDJSON JSON-RPC
┌──────────────────────────────▼───────────────────────────────┐
│ minicore-agent: process/backend layer                        │
│ session/catalog/store/workspace/provider/tool ownership     │
└──────────────────────────────┬───────────────────────────────┘
                               │ internal Agent implementation
┌──────────────────────────────▼───────────────────────────────┐
│ minicore-runtime: execution semantics (not linked here)     │
│ model/tool loop, durability, cancellation                    │
└──────────────────────────────────────────────────────────────┘
```

The TUI does not link either backend crate. It does not read the Agent store
or workspace, execute shell commands, call a provider, or recreate an Agent
loop. There is one Agent child per TUI process and one RPC client.

## Process Model

`main` parses the flat CLI, validates the config path through the RPC process
constructor, and starts:

```text
minicore-tui
└── minicore-agent --config <path> --stdio
```

The child is spawned before alternate-screen entry. This keeps executable,
configuration, and initial spawn failures on ordinary stderr. The TUI owns
child cleanup. A normal quit sends `agent.shutdown`, waits for the response and
child exit in any order, and restores the terminal. A five-second deadline
kills a non-responsive child. There is no automatic restart or reconnect.

`RpcProcess` has one stdin writer, one stdout reader, one stderr reader, and a
child waiter. The event channel is bounded. Responses and notifications can
arrive interleaved or out of order; request IDs and `App.pending_requests`
provide correlation. Stderr is a bounded in-memory log ring and never becomes
TUI stdout.

## Single Writer

All mutable application state is owned by `App`. `App::update(AppEvent)` is the
only mutation entry point. RPC tasks, the Crossterm `EventStream`, timers, OS
signals, and the command executor either produce an `AppEvent` or perform an
`AppCommand`; they never hold an `App` reference and never mutate UI state.

The important data flow is:

```text
input/RPC/timer/signal
          │
          ▼
      AppEvent
          │
          ▼
   App::update(&mut self)
          │
     ┌────┴─────┐
     ▼          ▼
 App state   AppCommand
                  │
                  ▼
          main-loop side effect
```

`AppCommand::Rpc` carries a request whose ID was registered in the pending map
inside the same update. `KillChild` and `Exit` are the only other commands.
There is no handler registry, effect trait, Redux/Elm layer, or plugin system.

## Turns And Ordering

After `turn.send` succeeds, the app registers `turn.wait` immediately in the
same update. A `turn_started` event may arrive before that response, and output
or tool events may arrive before/after `turn.wait`; exact session and loop references
route them to the right live view. A cancellation uses the same exact `TurnRef` and
still waits for an outcome. If the user submits text while a session is running,
the composer issues `turn.steer`.

Agent events are a live, best-effort view. `dropped_before` marks an event gap,
but the TUI does not add ACK, replay, or deduplication infrastructure. The
wait response, `session.state`, and durable `session.history` are
authoritative. After a wait, the app fetches the durable tail, merges items by
monotonic item index, patches tool results by call ID, clears the gap when the issued gap
revision is still current, and removes the provisional live turn (or transitions to
an unsaved loop banner if persistence failed). Background sessions retain their own
`SessionView` and continue receiving events.

Configuration reload is a reducer barrier for new turn work. The public
`/reload` registers `agent.reload` first and, when the active view has a
retained `unsaved_loop`, `live.reference`, or `last_result`, appends at most
one exact-turn internal `turn.wait` without waiting for the reload ACK. That
wait is an execution request outside the staged candidate: it is correlated
normally, is not a stale read, and does not complete or fail `ReloadProgress`.
While the staged catalog/state/presentation/history candidate is in flight,
Enter and direct submit/steer events leave composer text and already-admitted
steer items in place; the FIFO does not issue `turn.send` or `turn.steer`. The
event that finishes or fails the candidate, and a `ReloadWaitTurn` response or
send failure even when it arrives after staging, are barred from advancing the
FIFO in that reducer pass. A later ordinary event may resume the existing
settled/handoff rules. Existing execution requests, including an already-
authorized wait, are not cancelled by the read barrier.

Reads retired at reload start become stale responses. If a retired state,
presentation, or history read could have been the authority for a session,
the session drops the old state snapshot and retains event-gap and
incomplete-history fences. A reload failure starts independent fresh
`session.state` and `session.history` reads; History may clear the gap, but
submit/close/delete remain blocked until a valid state response also restores
state authority. Ordinary submit admission retains its existing state/history-read behavior: an
in-flight ordinary read, loading history, or temporarily absent SessionState
does not become a new global admission fence. Steer has a separate authority
fence: after reload or an uncertain state read, only a matching fresh normal
`session.state` response proving the current `TurnRef` is still `Running` may
release it. Reload-staged `Running` state, ordinary `Idle` notifications, and
fresh `Idle` responses do not release that fence. Once released, explicit and
FIFO Steer may use the authoritative TurnRef through an ordinary History
`event_gap`; the gap still blocks new turns, lifecycle mutations, and
configuration updates, and History reconciliation remains an independent
requirement. The Steer fence applies to the retained live loop, not the
separate fresh-turn handoff: a completed, persisted, history-settled Idle
session may hand off its queued input exactly once under the existing rules.
Reload-retired reads and lifecycle ACKs use the session-scoped
`close_verification_unknown` fence; a matching fresh state response clears it
while the independent history-gap fence remains. Configuration updates retain
their separate calibrated-state and request-boundary rules. Lifecycle requests
already in flight are not retargeted.
Open/create ACKs that arrive during or after staging are marked as uncalibrated
before fresh reads are admitted; other lifecycle ACKs keep the affected session
fenced so the staged snapshot cannot erase the need for recovery. A known
post-ACK catalog/state/history failure reports view-refresh failure rather than
claiming rollback; transport loss leaves the reload outcome unknown.

## Live And Durable State

`SessionView` separates:

- `TranscriptState.items`: the contiguous indexed history source of truth;
- `TranscriptState.blocks`: the render presentation, which may expand one history item into several cards;
- `LiveLoop`: provisional text/reasoning/tool progress across multiple requests for one active loop;
- `UnsavedLoop`: unpersisted turn content preserved when persistence fails;
- `SessionView.config_update`: a model/reasoning update waiting for request-boundary evidence.

Live answer text is rendered as plain wrapped text. Since 0.2.1, live reasoning
uses the same Markdown renderer as durable reasoning, parsing each request's
accumulated reasoning buffer when its conversation snapshot is rebuilt. Within
each live request and durable Assistant block, the recorded part order is preserved.
Neither live path inserts content into or invalidates the durable Markdown cache. A pending local user card may enter
the durable block list for immediate feedback, but send failure/removal and
transcript reconciliation invalidate it correctly.

## Durable Render Cache

Each `TranscriptState` owns at most one `Arc<PreparedDurable>` containing
history lines, section ranges, copy text, and link geometry. Its key is
`(render_revision, width, theme, reasoning_visible, tools_expanded)`. History,
individual folds, acceptance timestamps and relevant durable tool display
changes invalidate the revision. Live deltas do not invalidate this cache.

`ui::transcript::prepare_conversation(&App, width)` remains read-only. It reuses
valid history preparation and composes the current live tail into the shared
`PreparedConversation`. `AppEvent::ConversationPrepared` installs both the
snapshot and its history cache after checking session, revision and display key.
No interior mutability is used. Live updates still copy history rows into the
combined snapshot; this is not a fully incremental or virtualized transcript.

The complete snapshot is retained for Tick, scrolling, selection and ordinary
editor input. No-op mouse motion and identical Viewport feedback return without
marking dirty. Content-bearing events conservatively discard the complete
snapshot, while retaining unaffected history preparation. Theme, width and
fold keys also reject stale snapshots.

The main loop coalesces preparation and actual row measurement inside the draw
budget, rather than doing expensive layout before every input/RPC wait. A mouse
event that needs a missing snapshot installs it once before hit testing. Input
helpers borrow that snapshot; painting clones only the visible rows. Selection,
copying, measurement and link arbitration still share one geometry source.

Prepared lines do not cache terminal Cells: Ratatui still segments/measures
visible graphemes on each draw, even without Paragraph wrapping. Debug builds
optimize dependencies at level 2 and, since 0.2.7, this package at level 1 to also
optimize locally instantiated generic terminal/ANSI code. Debug information and
assertions remain, with the usual optimized-code stepping/variable tradeoffs.
There is no Cell cache. The render budget remains 30 FPS; the Working glyph has
an independent 100 ms monotonic deadline. Selection auto-scroll retains its own
50 ms deadline. Idle disarms periodic ticks; a transient scrollbar can arm one
absolute hide deadline. Stable history uses shared section layouts, not full
history-row copies on each delta (see the refactor acceptance evidence).

Live and durable Tool clicks resolve the same full Tool identity and expansion
policy. Live-only folds discard the combined snapshot without invalidating the
durable Markdown cache; overrides survive presentation and history reconciliation.

The scrollbar follows Pi 0.85.1 fullscreen's default `auto` mode: a full-height
`│` track, `┃` thumb, `█` active thumb, independent dark/light colors, and a 1000 ms
hide deadline after scrolling or hover exit. It overlays the final column without
changing content width. Geometry and pointer mapping use Pi's rounded/clamped
formula; the thumb is at least two rows, bounded by the track height.

Dragging writes the Session offset directly. Track clicks jump using a centered
grab offset; release only ends capture and never remaps its coordinates. Resize,
content changes, wheel input and explicit scrolling retain capture and subsequent
drag events use the current geometry. Focus/session/modal changes or content
fitting the viewport end capture. No second pending position or drag timer exists.
Wheel steps are one row, or five with Alt; PageUp/PageDown overlap four rows.

Scroll inputs prefer the valid prepared layout; otherwise the last viewport
measurement remains usable, like Pi's last published `currentLayout`. Hover and
wheel do not rebuild full history. The bottom status hint is a centered overlay,
not a reserved row; only its actual rectangle blocks text/fold/link hits. MiniCore's
existing status labels remain. Wide-grapheme edges and background colors are
preserved by the same overlay helper. Selection highlighting excludes the bar.
Unchanged pointer, boundary wheel, keyboard scroll and early idle Tick events
avoid repaint when no queued Steer semantics need to advance.
See [scrollbar verification](verification/scrollbar/README.md); the earlier
[stream interaction evidence](verification/stream-interaction/README.md) is historical.

## Markdown

The private pulldown-cmark wrapper owns durable Markdown and live reasoning
styling. Streaming answer text uses `wrap_plain`. Reasoning supplies an italic
base and fills only unspecified foreground colors with the muted theme color,
preserving explicit Markdown heading/list/code colors. `wrap_segments` still makes character-level width
decisions for Unicode, CJK, emoji, combining marks, and line boundaries, but
coalesces adjacent characters with the same effective style into one Span.
This reduces allocations without adding a rope, syntax highlighter, or virtual
DOM.

## Terminal Lifecycle

`TerminalGuard` owns alternate-screen/raw-mode/mouse/bracketed-paste state and
has an explicit restore plus best-effort Drop fallback. `PanicHookGuard` restores
the terminal before delegating to the previous panic hook. Hook modification is
skipped while a thread is unwinding because the standard library forbids
`take_hook`/`set_hook` there; preserving the delegating wrapper is safer than
causing a second panic. The main variable declaration order makes terminal
cleanup happen before panic-hook cleanup during unwind.

`TerminalWriter<Stdout>` batches small ANSI writes in a 64 KiB `BufWriter`.
Explicit flush and full-buffer writes preserve order and propagate I/O errors;
this is not whole-frame atomicity. Its Drop discards pending bytes without I/O.
Before explicit restoration or clear-failure rollback, the guard also discards
pending bytes and switches to zero-capacity forwarding: Ratatui's own terminal
Drop can otherwise show the cursor and flush an unfinished frame after shell
restoration. The underlying `Stdout` retains its normal locking behavior.

The main loop multiplexes RPC events, Crossterm `EventStream`, ticks, signals,
shutdown timing, and the render deadline without a biased select. RPC work is
bounded per batch. `dirty` is cleared only by `AppEvent::Rendered`; idle loops
have no render deadline, and busy rendering is capped at 30 FPS while spinner
and expiry work use their own deadlines.

## Tool Detail Ownership (v0.3 E1)

`app/panels.rs` routes the one `MainView::ToolDetail` and concrete
Main/Editor/Dock/Search/Confirmation focus. It uses the existing App request
registry and two `QuerySlots`; a full `ToolKey` owns at most one read/output
request, including across close/reopen and tab changes. Generation/epoch checks
discard stale results but never release a slot before its real response.

`state/tool.rs::ToolFacts` is still the semantic owner. Invocation, Runtime
execution, command termination/output completion, and auxiliary recording are
separate facts. A process command may precede tool.read/execution, so its one
owner is not conditional on receiving either. Terminal execution cannot regress
on a late start; conflicting outcomes schedule an authoritative tool.read.

The detail's four `StreamView` windows hold bounded chunks, at most 1 MiB per
stream and at most 4 MiB for the single open detail (below the 16 MiB global
stream allowance). Closing releases them. Existing result body Arcs are reused
when authoritative output bytes match; the detail does not duplicate that
already-owned body. Tiny process chunks coalesce into capacity-accounted 16 KiB
pages, and each stream also has an independent 128-chunk metadata bound. Shared
in-flight layout snapshots remain immutable through copy-on-write. Only server
raw-byte next_offset continues paging. Event
gaps request the authoritative range; empty gap notices advance to the retained
start without claiming EOF. Incomplete UTF-8 suffixes are withheld before EOF,
invalid bytes are replaced for display, and controls never affect raw cursors.

Process hints are throttled to 250 ms; silent-running reads fall back to 500 ms.
Only the current tab polls. Terminal output still drains to real EOF. A query
error stops the chain with explicit retry, not recursive fallback. Hidden cards
do not fetch full streams. There is no stdout/stderr merged timeline.

The existing single serialized layout worker accepts either conversation or
tool work; there is no second worker/RPC owner or generic panel framework.
Tool text is decoded/sanitized and indexed by grapheme-safe wrap ranges off the
update/draw path; render materializes only visible rows. Its retained text/index
capacity is charged to the existing 48 MiB layout cache budget. Details preserve
the original Editor/Footer and saved conversation scroll state. Card title
detail hits share draw geometry; the original card folding target stays intact.

## Explicit Non-Goals

This frontend intentionally does not implement:

- provider access, Agent loop logic, workspace/store parsing, or shell
  execution;
- approval UI, implicit cross-loop follow-up queues, or full PTY emulation;
- MCP, plugins, skills, subagents, remote Agents, session forks/branches, or
  automatic reconnect/restart;
- OSC52 copy and workspace/changes/context main-area pages (later E slices).

Those omissions are backend and product-boundary decisions, not hidden
fallbacks. The complete wire boundary is pinned in
[../docs/rpc-contract.md](rpc-contract.md).
