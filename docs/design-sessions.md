# Concurrent sessions design

Switching sessions no longer drops the one you leave. Every session opened in
this TUI process stays live in memory: its turn keeps running in the Agent, its
events keep being applied, its waits and reconciliations keep working, and the
Agent keeps running its queued messages. The session on screen is the *foreground*; the others are *background*.
Switching selects them instantly, without `session.open` or a history reload, so
nothing streamed meanwhile is lost. Keys (Enter, Esc, approvals) act on the
foreground only.

The Agent already runs turns of different sessions concurrently (loaded
sessions live in a map, each with its own input queue) and sends every
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

`App::resident` owns every loaded `Session` and a separate selected epoch.
Each `Session` receives one Agent `load` and local epoch at construction; neither
changes during that resident's lifetime. Navigation only changes selection and
recency. There is no temporary foreground slot for background operations.

Sessions retain their drafts, returned texts, unseen marker and saved scroll.
The selected session uses `App::scroll` and the terminal composer; navigation
parks those values before restoring the next view. Layout invalidation, picker
closing, phone views and attention notices keep the same behavior.

## Routing

- Agent events resolve `session_id` and verify the resident load; unknown session
  events update the catalog or enter the bounded pending-open snapshot buffer.
- `Msg::Session(epoch, op)` resolves the original resident, independently of the
  selected view. Removing a resident cancels its owned reads and waits; already
  queued results are discarded by epoch.
- Reducer functions take an explicit epoch. History insertion updates either
  the selected scroll or the background session's saved scroll. Background
  results cannot change a different view's composer, selection or picker.
- Session notices retain their labels and original device routing. Auto-allow
  uses the same typed interaction operation in foreground and background, and
  a session-wide allow is recorded only after a successful Agent reply.
- Local uncertain inputs and phone acknowledgements retain separate operation
  owners; a navigation change cannot cancel or redirect their acceptance facts.

## Switching

- `Action::OpenSession(id)` (picker, `/resume`, `--session`, phone
  `open_session`): if `id` is the foreground, nothing; if it is resident,
  **switch**; otherwise open as today (`loading_session`, `session.open`, adopt).
  A newly adopted session becomes selected; the previous resident stays loaded.
  A second open of the same load reuses its resident; another load is rejected
  instead of silently replacing the installed instance. Every change of the session on screen closes pickers and views, whose
  state belongs to the session left.
- **Switch** = save composer text into the leaving session's `draft` and
  `App::scroll` into its `scroll`; select the target epoch and update recency;
  restore its `scroll`;
  composer = its `draft` plus its `returned` texts; clear `unseen`,
  `draft`, `returned`; `layout.clear()`; set `App::workspace` to its workspace;
  refresh presentation/branch/context as `adopt` does for an opened session
  (cheap reads, no history reload); close pickers.
- `/new` clears selection (the resident keeps running) and shows a
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

- Deleting a live background session that is idle removes it from `resident`
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
  focusing it before intents preserves terminal-follow behavior, while each
  business operation addresses its explicitly captured resident. The full per-device
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
