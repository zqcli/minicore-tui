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
There is no Cell cache. The render budget remains 30 FPS; busy animation now
requests a 33 ms tick with an absolute deadline that unrelated events cannot
postpone. Idle disarms the tick timer, and selection auto-scroll retains its own
50 ms deadline. Large live snapshots still copy history rows.

Live and durable Tool clicks resolve the same full Tool identity and expansion
policy. Live-only folds discard the combined snapshot without invalidating the
durable Markdown cache; overrides survive presentation and history reconciliation.

Scrollbar dragging projects its pending offset into the same `ScrollPosition`
used by body painting, thumb placement and hit testing. The body follows before
release; release commits only if current geometry still matches. The visible-row
budget and marker are frozen during a drag, then resume normal layout on release.
Resize, real viewport changes, focus/session changes, wheel and explicit scroll
commands cancel the drag. Unrelated RPC/repreparation does not. A stationary drag
needs no animation timer, and marker rows cannot select or activate hidden content.
See [stream interaction verification](verification/stream-interaction/README.md).

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

## Explicit Non-Goals

This frontend intentionally does not implement:

- provider access, Agent loop logic, workspace/store parsing, or shell
  execution;
- approval UI, follow-up queues, compaction controls, or live Bash
  stdout/stderr/PTY display;
- MCP, plugins, skills, subagents, remote Agents, session forks/branches, or
  automatic reconnect/restart;
- External Editor and OSC52 copy in v0.2.

Those omissions are backend and product-boundary decisions, not hidden
fallbacks. The complete wire boundary is pinned in
[../docs/rpc-contract.md](rpc-contract.md).
