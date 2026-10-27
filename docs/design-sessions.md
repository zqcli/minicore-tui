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
  - `returned: Vec<String>`: refused texts not delivered to a phone, kept while
    the session is in the background or remote control has no recipient;
    restored into the composer, joined by blank lines, when it returns to the
    local foreground.
  - `unseen: bool`: a turn ended (or failed) while it was in the background;
    cleared when it comes to the foreground or any phone opens it; a turn end
    in a session viewed by a phone does not mark it unseen.
  - `waiting_for_compaction`: a send refused while post-turn compaction owned
    the session (formerly on `App`); it counts as `compacting`.
- Each `Session` keeps the transcript-view state it needs when it returns:
  `scroll: Scroll` (the foreground's `App::scroll` is saved into it on leave and
  restored on return). The layout cache is cleared on every switch, as today.

## Routing

- **Agent events**: `on_agent_event` finds the session with `meta.session_id`
  in the foreground or `background` and applies the event to it. Events for a
  session that is not live still only update the catalog (`SessionOpened`).
- **Task results** (`Msg::Session(epoch, op)`): delivered to the live session
  with that epoch, foreground or background; dropped if none (deleted).
- **Running reducer code for a background session**: `App::with_background(index, f)`
  swaps that session into `App::session`, sets `App::background_op = true`,
  runs `f` (the existing `on_op`, event handling, `start_turn`, `end_turn`,
  `flush_queue`, `on_approval_requested`, …), then swaps back and clears the
  flag. `App::scroll` is swapped with the session's own `scroll` around it, so
  a page prepended in the background keeps that session's anchor and the
  foreground's is never touched; the layout cache is not used by reducer code.
  No turn-lifecycle code is duplicated.
- While `background_op` is set:
  - no layout, overlay, selection or composer change: `restore_draft` routes
    remote text or pushes to `Session::returned`; find matches are not moved; results
    that only answer foreground pickers and views (`/session` item count, `@`
    files, diff lists and pages) are dropped;
  - local `notice` prefixes the text with `"<label>": ` (the session label);
    remote session notices are plain for its viewers and prefixed for others;
  - device views are not changed: remote projection finds each viewed live
    session in the foreground or background independently.
- Auto-allow ("allow this tool for the session") answers background approvals
  as it does foreground ones.

## Switching

- `Action::OpenSession(id)` (picker, `/resume`, `--session`, phone
  `open_session`): if `id` is the foreground, nothing; if it is in `background`,
  **switch**; otherwise open as today (`loading_session`, `session.open`, adopt).
  A newly adopted session pushes the previous foreground to `background`; an
  open answer for a session that is already live (a second open of it) is
  ignored. Every change of the session on screen closes pickers and views, whose
  state belongs to the session left.
- **Switch** = save composer text into the leaving session's `draft` and
  `App::scroll` into its `scroll`; move it to the front of `background`; take
  the target out of `background` into `App::session`; restore its `scroll`;
  composer = its `draft` plus its `returned` texts; clear `unseen`,
  `draft`, `returned`; `layout.clear()`; set `App::workspace` to its workspace;
  refresh presentation/branch/context as `adopt` does for an opened session
  (cheap reads, no history reload); close pickers.
- `/new` moves the foreground to `background` (it keeps running) and shows a
  draft; remote `new_session` focuses the requesting device first and changes
  only its draft view and settings. The local draft's composer text is kept in
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
- **Turn ended** in a background session: set `unseen` unless a phone views it; notice
  `"<label>" finished in 3m12s` (the turn-end line's duration format; or the
  outcome words it uses, such as `interrupted` or `failed · rate limited (429)`); the
  long-turn bell rule applies.
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
- `State` gains `"background": {"running": n, "approval": n}` (counts over live
  sessions other than the receiving device's session, including the TUI
  foreground when it differs). Each phone owns its session or draft view;
  focusing it before intents reuses the foreground reducer. The full per-device
  focus, pending-create and message-routing contract is in `design-remote.md`.
- The live fields are applied on every sync next to the `current` marks, outside
  the cached catalog (a lookup per listed session, no rebuild or sort), so a
  background status or `unseen` change changes the serialized `State` and a
  `state` goes out; unchanged background state sends nothing.
- `open_session` on a live session switches instantly (ack after the switch),
  changing only the requesting device's view; `sessions[].current` is per-view.
- A background approval or turn end sends its attention notice only to devices
  not viewing that session; viewers already see its interaction or outcome.
- The TUI remote status screen gains a row `Others` showing
  `2 running · 1 needs approval` when non-zero (omitted otherwise); its phone
  rows name each device's viewed session. The work rows describe the foreground.

## Not in scope

- Answering a background approval without switching.
- Per-session scroll positions on the phone.
- Closing sessions in the Agent (`session.close`) when they leave the foreground.
