# Concurrent sessions design

Switching sessions no longer drops the one you leave. Every session opened in
this TUI process stays live in memory: its turn keeps running in the Agent, its
events keep being applied, its waits, reconciliations and queued follow-ups keep
working. The session on screen is the *foreground*; the others are *background*.
Switching swaps them instantly, without `session.open` or a history reload, so
nothing streamed meanwhile is lost. Keys (Enter, Esc, approvals) act on the
foreground only.

The Agent already runs turns of different sessions concurrently (loaded
sessions live in a map; `turn.send` reserves only its session) and sends every
session's events on the one stdio stream with `session_id`. It does not change.

This document is the contract for the implementation; where code and document
disagree after it lands, the code wins and this file is fixed.

## Decisions

- **No cap.** Every session opened or created in this process stays live until
  the process exits or the session is deleted. The user controls how many.
- **Approvals are answered in the foreground.** A background approval notifies;
  the user switches to that session to answer it (the context is on screen).
- **Per-session drafts.** Leaving a session keeps its composer text with it;
  coming back restores it. A draft (no session yet) keeps its own text too.
- **Deleting a running background session is refused** (`Stop "<title>" first:
  open it and press Esc`). Deleting the foreground session stays refused.
- **Background approval timeouts** are the Agent's `[loop] prompt_timeout_seconds`.

## Model

```rust
pub struct App {
    pub session: Option<Session>,   // foreground, as today
    pub background: Vec<Session>,   // live, not on screen; most recently left first
    ...
}
```

- Every `Session` keeps its `epoch` for its whole life. `adopt` (open/create of
  a session not live) assigns `App::epoch + 1` as today; switching to a live
  session reuses its epoch, so tasks in flight for it stay valid.
- A session is live in at most one place (foreground or `background`), never twice.
- `Session` gains:
  - `draft: Option<String>`: the composer text saved when it left the foreground.
  - `returned: Vec<String>`: texts the Agent refused while it was in the
    background (the paths that call `restore_draft` today); restored into the
    composer, joined by blank lines, when it comes to the foreground.
  - `unseen: bool`: a turn ended (or failed) while it was in the background;
    cleared when it comes to the foreground.
  - `last_seen_outcome`: not needed; `unseen` suffices.
- Each `Session` keeps the transcript-view state it needs when it returns:
  `scroll: Scroll` (the foreground's `App::scroll` is saved into it on leave and
  restored on return). The layout cache is cleared on every switch, as today.

## Routing

- **Agent events**: `on_agent_event` finds the session with `meta.session_id`
  in the foreground or `background` and applies the event to it. Events for a
  session that is not live still only update the catalog (`SessionOpened`).
- **Task results** (`Msg::Session(epoch, op)`): delivered to the live session
  with that epoch, foreground or background; dropped if none (deleted).
- **Running reducer code for a background session**: `App::with_background(epoch, f)`
  swaps that session into `App::session`, sets `App::background_op = true`,
  runs `f` (the existing `on_op`, event handling, `start_turn`, `end_turn`,
  `flush_queue`, `on_approval_requested`, …), then swaps back and clears the
  flag. The foreground's `scroll` and layout are saved/restored around the swap
  so they cannot be touched. No turn-lifecycle code is duplicated.
- While `background_op` is set:
  - no scroll, layout, overlay, selection or composer change: `restore_draft`
    pushes to `Session::returned` instead; scroll/anchor shifts (older pages,
    `pin_view`, follow on send) are skipped;
  - `notice` prefixes the text with `"<label>": ` (the session label);
  - the remote projection is not touched (it always projects the foreground).
- Auto-allow ("allow this tool for the session") answers background approvals
  as it does foreground ones.

## Switching

- `Action::OpenSession(id)` (picker, `/resume`, `--session`, phone
  `open_session`): if `id` is the foreground, nothing; if it is in `background`,
  **switch**; otherwise open as today (`loading_session`, `session.open`, adopt).
  A newly adopted session pushes the previous foreground to `background`.
- **Switch** = save composer text into the leaving session's `draft` and
  `App::scroll` into its `scroll`; move it to the front of `background`; take
  the target out of `background` into `App::session`; restore its `scroll`;
  composer = its `draft` plus its `returned` texts; clear `unseen`,
  `draft`, `returned`; `layout.clear()`; set `App::workspace` to its workspace;
  refresh presentation/branch/context as `adopt` does for an opened session
  (cheap reads, no history reload); close pickers.
- `/new` (and phone `new_session`) moves the foreground to `background` (it
  keeps running) and shows a draft; the draft's composer text is kept in
  `App::draft_text` while a session is in the foreground and restored on `/new`.
- Opening a session from the draft: the draft's composer text is saved in
  `App::draft_text`.
- Startup `--session`/`--continue` unchanged.
- `loading_session` still blocks a second open/create; switching to a live
  session is synchronous and never blocked by it except while a create's first
  message is pending.

## Background attention

- **Approval requested** in a background session (not auto-allowed): ring the
  bell (respecting `[ui] bell`), mark the terminal title as waiting (the title
  notifier counts background approvals too), notice
  `"<label>" needs approval · ctrl+r`.
- **Turn ended** in a background session: set `unseen`; notice
  `"<label>" finished in 3m 12s` (or `failed` / `interrupted` with the
  outcome word the turn-end line uses); the long-turn bell rule applies.
- **Footer**: when any background session is running, waiting for approval or
  compacting, a segment `2 running · 1 needs approval` (only the non-zero
  parts; `needs approval` in the warning style). It gives way on narrow
  terminals after the remote mark.
- **Sessions picker** (Ctrl+R): each live row's detail starts with its state:
  `running 2m`, `needs approval`, `compacting`, `new` (unseen), before the
  existing parts (`current`, workspace, short id, age). Live sessions sort first
  (foreground, then background in most-recently-left order), then the rest as
  today.
- An idle TUI still does not tick: elapsed times in the picker and footer are
  drawn from `LiveTurn::started` when a frame is drawn; the existing animation
  rule (`App::animating`) extends to "any live session is busy" only for the
  footer spinner if one is shown (prefer no spinner in the footer: static text).

## Deletion

- Deleting a live background session that is idle drops it from `background`
  after the Agent confirms. A running, approval-waiting or compacting one is
  refused with `Stop "<label>" first: open it and press Esc` (local picker and
  phone `delete_session`).

## Remote projection (protocol v1 additions)

- `State.sessions[]` entries gain, for live sessions only:
  `"live": true`, `"status": "running"|"approval"|"compacting"|"idle"`,
  `"started_ms": <Unix ms>|null` (running turn start), `"unseen": bool`.
  Not-live entries have `"live": false` and no other new fields.
- `State` gains `"background": {"running": n, "approval": n}` (counts over
  background sessions only).
- The catalog cache must be invalidated when any background session's
  status/unseen changes (bump `catalog_generation` or fold a background-status
  generation into the cache key), so a `state` goes out; unchanged background
  state sends nothing.
- `open_session` on a live session switches instantly (ack after the switch).
- A background approval or a background turn end forwards its notice as today
  (`notice`), so the phone shows it.
- The TUI remote status screen gains a row `Others` showing
  `2 running · 1 needs approval` when non-zero (omitted otherwise).

## Not in scope

- Answering a background approval without switching.
- Per-session scroll positions on the phone.
- Closing sessions in the Agent (`session.close`) when they leave the foreground.
