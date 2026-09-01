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
inside the same update. `KillChild` and `Exit` are the only other commands in
v0.1. There is no handler registry, effect trait, Redux/Elm layer, or plugin
system.

## Turns And Ordering

After `turn.send` succeeds, the app registers `turn.wait` immediately in the
same update. A `turn_started` event may arrive before that response, and output
or tool events may arrive before/after `turn.wait`; exact session, instance,
and turn references route them to the right live view. A cancellation uses the
same exact `TurnRef` and still waits for an outcome.

Agent events are a live, best-effort view. `dropped_before` marks an event gap,
but the TUI does not add ACK, replay, or deduplication infrastructure. The
wait response, `session.state`, and durable `session.transcript` are
authoritative. After a wait, the app fetches the durable tail, merges pages by
sequence, patches tool results by call ID, clears the gap when the issued gap
revision is still current, and removes the provisional live turn. Background
sessions retain their own `SessionView` and continue receiving events.

## Live And Durable State

`SessionView` separates:

- `TranscriptState.blocks`: durable user, assistant, tool, summary, and
  terminal blocks;
- `LiveTurn`: provisional text/reasoning/tool progress for one active turn.

Live text and reasoning are rendered as plain wrapped text. They are never
inserted into the durable Markdown cache. A pending local user card may enter
the durable block list for immediate feedback, but send failure/removal and
transcript reconciliation invalidate it correctly.

## Durable Render Cache

Each `TranscriptState` carries a monotonic `render_revision` and a concrete
`TranscriptRenderCache`. The cache stores one prepared `Vec<Line<'static>>`
for the durable block sequence of that session. Its key contains:

```text
render_revision
width
theme
reasoning_visible
tools_expanded
(turn_id, tool_call_id, expanded) for every durable tool
```

`ui::transcript::prepare_cache(&App, width)` is read-only. It parses and wraps
durable blocks only when the current key is absent. It returns a
`PreparedTranscriptCache`; `main` sends it back through
`AppEvent::TranscriptCachePrepared`, and `App::update` installs it only when
active session and key still match. No `RefCell`, `Mutex`, or other interior
mutability is used.

`render` and `total_lines` consume the same cached durable lines. If a cache is
missing or stale, both have a safe read-only fallback; the normal main loop
prepares before geometry measurement and again before every draw. Header,
notice, dock, and live-turn rows remain cheap per-frame derivations. Width,
theme, reasoning visibility, tool-all expansion, individual tool expansion,
and block mutations change the effective key or clear the cache. Live deltas
do not invalidate durable lines.

## Markdown

The private pulldown-cmark wrapper owns durable Markdown styling. Streaming
content uses `wrap_plain`. `wrap_segments` still makes character-level width
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

The main loop multiplexes RPC events, Crossterm `EventStream`, ticks, signals,
shutdown timing, and the render deadline without a biased select. RPC work is
bounded per batch. `dirty` is cleared only by `AppEvent::Rendered`; idle loops
have no render deadline, and busy rendering is capped at 30 FPS while spinner
and expiry work use their own deadlines.

## Explicit Non-Goals

This frontend intentionally does not implement:

- provider access, Agent loop logic, workspace/store parsing, or shell
  execution;
- approval UI, steering, follow-up queues, compaction controls, or live Bash
  stdout/stderr/PTY display;
- MCP, plugins, skills, subagents, remote Agents, session forks/branches, or
  automatic reconnect/restart;
- current-session model/reasoning hot switching;
- External Editor and OSC52 copy in v0.1.

Those omissions are backend and product-boundary decisions, not hidden
fallbacks. The complete wire boundary is pinned in
[../docs/rpc-contract.md](rpc-contract.md).
