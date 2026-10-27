# Architecture

How `minicore-tui` 0.4 is built, and the rules later changes must keep. The
original plan is [design-v0.4.md](design-v0.4.md); where the two differ, this
document and the code win.

## Scope

`minicore-tui` is the terminal frontend of `minicore-agent` (which links
`minicore-runtime`). The Agent crate is linked only to serve as this
executable's `agent` subcommand: `main` hands those arguments to
`minicore_agent::cli::run` before anything else. The TUI talks to that child over
stdio NDJSON JSON-RPC (Protocol v1) and calls no other Agent API. It never reads the Agent store or
workspace file contents, runs a tool, calls a provider or keeps provider credentials;
its two settings files name environment variables, never keys. Remote mode
validates host directory paths and stores only hashes of phone tokens.

## Modules

```text
src/
  main.rs       CLI, startup (config::prepare), spawn the Agent, terminal guard, main loop, frame pacing
  cli.rs        flat argument parser and USAGE
  config/       the TUI directory and everything read from it (see below):
                mod (Source, ConfigError, prepare, Loaded), home, settings
                (config.toml), models (models.toml), compose (the Agent file),
                scaffold (first-run starters)
  remote/       phone protocol, listener, authentication and semantic projection:
                mod (mode, listeners, connection, events), auth, server, tls
                (certificate loading), wire, view, fs (host directories), query
                (Agent reads relayed for one phone's view); web/
                holds the embedded client: plain files and ES modules, no
                build step (layout in design-remote-ui.md)
  rpc.rs        Agent child process and JSON-RPC Client
  protocol/     serde DTOs and constants only: methods, error codes, required
                capabilities, read sizes (mod); event; read (history items, chunk
                assembler, pins); tool; changes; workspace; stamp
  wrap.rs       display width, wrapping, sanitize, truncate; used by app, ui and title
  title.rs      terminal title and bell: Notifier turns a Snapshot into escape bytes
  prompt_history.rs  the prompt history file (JSON lines, atomic rewrite)
  app/          the reducer; all mutable application state
    mod.rs        App, Msg, SessionOp, Scroll, notices, scrolling
    session.rs    Session: transcript, live turn, queue, usage, history window
    actions.rs    Action (keys and commands land here) and the IO each starts
    ops.rs        applies SessionOp results and Agent events
    remote.rs     remote commands, local authorization, intents, projection sync
    tasks.rs      async operations: bootstrap, waits, history reads, changes, export
    keys/         on_key and the per-surface key handlers: mod (approval prompt),
                  composer, completion (the / and @ lists), overlay (pickers, views)
    mouse.rs      on_mouse: wheel, scrollbar, press/drag/word selection, clicks
    selection.rs  Drag and the text a drag covers; word_at for double click
    scrollbar.rs  thumb and jump-label geometry shared by the renderer and the mouse
    picker.rs     fuzzy_match, window and step for every list
    commands.rs   slash-command table, parsing and the unknown-command guard
    composer.rs   draft model: cursor, paste chips, undo, prompt history
    templates.rs  prompt templates from `prompt/commands/*.md`
    notify.rs     feeds title.rs from the app state; the exit hint
    overlay.rs    Overlay (the one open picker or view), Panel, the filters behind
                  the pickers (sessions, models, files)
    diff.rs, tool.rs, find.rs   diff state and reads, card hydration, transcript find
    tests/        reducer tests through the scripted Agent
  transcript/   mod (Transcript: blocks with a revision each)
                block.rs (Block, RequestKey, ChangeSummary)
                tool_call.rs (ToolCall, ToolState, the bounded output tail)
                history.rs (history items to blocks, merge_live, Markdown export)
                layout.rs (per-block line cache, prefix heights, viewport, anchors)
  ui/           mod (draw), transcript (the pane, RenderState), blocks, rails,
                markdown + markdown/stream (incremental renderer), composer (render),
                status (status line, queue, notice, footer), panel + sheets (the one
                bottom-sheet style), theme (Palette, Styles),
                views/{diff, help} (full-screen), views/remote (the remote status
                screen), views/context (sheet report),
                snapshots and perf_tests (test only)
  terminal.rs   alternate screen, raw mode, paste, mouse, kitty keys, panic hook
  clipboard.rs  pbcopy / xclip / clip with an OSC 52 fallback
  editor.rs     external editor for Ctrl+G
  testing.rs    FakeAgent: scripted in-process Agent behind a real Client (test only)
```

Dependency direction: `protocol` ← `rpc` ← `transcript` ← `app` ← `ui`.
`transcript` and `ui` never call the Agent, and `ui` holds no state of its own:
it draws `&App`. The models (composer, selection, pickers, row types of the
views, scrollbar geometry) live in `app`. `ui::draw` takes `&mut App` for
a few writes: the layout cache with its viewport, clearing the selection when
the rows move, and the hit areas of the editor and of the open view. What `app` still imports from `ui`:

- `ui::theme::{Palette, Styles}`: the app owns the active `Styles`.
- `ui::transcript::RenderState`: the type parameter of `App::layout`, the
  streaming renderers the cache hands to the render callback.
- `ui::blocks::{render, INSET, FULL_OUTPUT_HINT}` and `ui::rails::RAIL`: find
  searches the rows a block renders, and a click is matched to a card's hint
  row or an assistant rail by the text drawn there.
- `ui::views::context::text` and `ui::sheets::model_details`: the copy of the
  context report and the model note, which are formatting of what is shown.

## Configuration

All local state sits in one *TUI directory* (`--home`, `$MINICORE_HOME`, else the
executable's directory): `config.toml`, `models.toml`,
`prompt/SYSTEM_PROMPT.md`, `prompt/commands/`, `session/` (the Agent's
`data_dir`), `history.jsonl`. `config::prepare` runs before the terminal is
touched:

1. a fresh directory (neither settings file) gets commented starters and the
   process exits with the paths and what to fill in;
2. `models.toml` becomes `AgentModel`s (provider fields merged into each model),
   then `config.toml` is checked against them: default profile, profile model,
   reasoning against the model's list, tools, prompt files, compaction, loop. Each
   error carries file, key and, from the `toml::Spanned` of the offending value,
   the line;
3. `--profile`/`--model` and the model's `api_key_env` are checked;
4. `AgentFile` is serialized to `session/agent.toml` (mode 0600, atomic, with a
   generated-file header; `system_prompt = { file }` holds the absolute prompt
   path so the Agent reads it) and passed with `--config`.

`--agent-config FILE` skips 1 to 4 and hands the file to the Agent unchanged.
`App` keeps `home`, `composed` and the `[ui]` editor; `/reload` calls
`config::load` and `write` again (an error is a notice and the Agent is not
reloaded), `/theme` edits `[ui] theme` with `toml_edit` so comments survive. The
Agent config is never parsed back: `AgentFile` only serializes.

## Main loop and message flow

`main` resolves the TUI directory, runs `config::prepare` (scaffold, validate,
compose; see Configuration) and spawns the Agent (`rpc::agent_command`: itself
with `agent --config <home>/session/agent.toml --stdio`, or the external
`--agent` / `[agent] binary`) **before** entering the alternate screen, so a
missing binary or a bad configuration stays an ordinary terminal error. It then
starts `tasks::bootstrap` (ping, capability check, models, profiles, sessions)
and loops on one `biased` `select!`: Agent events, `Msg` values from tasks,
terminal input (a task forwards crossterm's `EventStream`; polling it from the
loop costs about 15 `sched_yield` calls per turn; on Windows the task decodes
bracketed paste first, see `terminal/input.rs`), an 80 ms tick (one spinner
frame, armed only while a turn or compaction animates; a flash, the scrollbar or
an info notice sleeps until it expires instead) and the frame deadline (armed
only while dirty). A frame is drawn at
most every 16 ms, so a burst of deltas costs one draw; pointer motion off the
scrollbar draws nothing. `MINICORE_TUI_PERF_STATS` prints the draws per cause
and their times on exit. After bootstrap the loop opens a session once, for
`--session`, else the newest for the workspace with `--continue`. Without one the
app stays in the draft (`app/draft.rs`): `app.session` is `None`, the screen is
the welcome header over an empty transcript, and nothing is created.

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
    Renamed(..),                            // session.rename from the picker
    Notice(NoticeLevel, String),
    DraftEdited(Option<String>),
    Flash(String),                          // `Copied!` over the transcript
    Remote(RemoteEvent),                    // listener admission, intents, queries, disconnects
}
```

`SessionOp` holds the results of everything done for a live session
(`Tail`, `Page`, `Waited`, `Sent`, `Context`, `Presentation`, `Changes`,
`ToolHydrated`, `DiffFile`, …). `App::spawn` and `App::call` stamp each task with
the foreground session's `epoch`; `on_msg` finds that epoch in the foreground or
background and drops a result only when its session is gone. `adopt` assigns
`App::epoch + 1` to a newly opened or created session; a live switch reuses its
epoch. Remote listeners have a separate generation so late handshakes cannot
survive an off/on cycle.

Every session opened in the process stays live (`docs/design-sessions.md`):
`App::session` is the foreground and `App::background` holds the others, most
recently left first. Each keeps its epoch for life, so `Msg::Session` reaches it
by epoch wherever it is, and Agent events reach it by `session_id`; a result for
a deleted session is dropped. Background work runs the ordinary reducer code
through `App::with_background`, which swaps the session into the foreground slot
with its own `scroll`, sets `background_op` and swaps it back. While that flag is
set the view is left alone: `restore_draft` routes remote text or keeps it in `Session::returned`,
find matches and foreground-only answers (item counts, `@` files, diff reads)
are skipped, and `notice` names the session locally. A background turn end marks the
session `unseen` unless a phone views it, and an approval asks for the bell (`App::ring`). Switching
(`show_session`) parks the foreground with its composer text and scroll, brings
the target back with its draft and returned texts, clears the layout cache and
pickers and reads only the footer's figures; the draft keeps its text in
`App::draft_text`. Live states reach the picker, the footer, the status screen
and the phone's session list when drawn or projected, with no timer of their own.

### The draft

A session is created by the first message, never at startup, so the session
list holds no empty sessions. While `app.session` is `None`:

- `App::launch` holds the settings the next session takes: the command-line
  flags, then what `/model`, `/reasoning`, `Ctrl+L`, `Ctrl+P`, `Shift+Tab` and
  `/new <profile>` set, which the footer shows instead of `session.update`.
  The footer resolves them with the profile's defaults (`profile.list`; the
  Agent's default profile comes from `config.toml` and is unknown for a raw
  `--agent-config`) through `App::model_id` and `App::reasoning`.
- The first `Enter` takes the editor's text (and records it in the prompt
  history), parks it in `App::first_message` and sends `session.create`.
  `Msg::Created` adopts the session and sends that text through the ordinary
  `send`, so the user block, timestamps and auto title behave as for any first
  message. A new session has no history, so no tail is read; it would clear the
  prompt block. A failure puts the text back with `restore_draft` and shows the
  error. While `loading_session` is set `Enter` does nothing, so a second
  session cannot start and the text typed meanwhile stays in the editor.
- `/new` calls `show_session(None)`, which parks the open session in the
  background, where it keeps running, and restores the draft's own text.
- Actions and panels that read a session (`Action::needs_session`,
  `Panel::needs_session`) answer one info notice; `@` completion needs
  `workspace.files`, which needs a loaded session, so it answers its own notice.
  The transcript area shows a dim line only while the Agent connects, a session
  opens or one is created.

### Remote control

`--remote` binds before entering the terminal; `/remote on` binds asynchronously.
`Bound::bind` opens one listener per configured address (IPv6 ones v6-only) and
skips an address the host lacks, while a taken port fails; one accept loop serves
every listener, and `enter_remote` reports the skipped addresses as notices.
With `[remote] tls` the certificate is loaded at config validation and again by
every `Bound::bind`; each connection does its TLS handshake before the shared
plain/TLS handler reads the request head.
`RemoteMode` is derived as Off, Waiting or Connected from the listener URL and
the admitted connections (`Remote::phones`); it is not independently mutable.
Waiting still freezes local sending: only `/remote off` restores local control.
The reducer alone admits connections: every signed-in device is admitted, and a
new connection with the same token replaces that device's older one with
`bye replaced`. The 20 remembered devices bound the connections. Connection ids
keep old requests and disconnects from affecting the replacement, and any
admitted connection may send intents. Off and quit send `bye` to every phone
before closing the listener.

While remote mode is on, `ui::draw` draws `views::remote` (the status screen) in
the transcript's place unless a full-screen view is open; the transcript layout
is left untouched and the mouse neither scrolls nor selects there. The dock drops
the Working line and the queue rows, which the status screen shows. The screen
arms no timer: its spinner and elapsed time move only while `App::animating`,
and the sign-in pause countdown refreshes with the next frame.

`on_action` denies local actions by default while remote is active, with an
explicit allowlist for viewing, copying/exporting, draft editing, theme changes,
queue recall, quit and remote commands. Command completion checks before clearing
the draft. Approval sheets neither render nor take the local keyboard; Esc can
still interrupt. Entering remote recalls queued follow-ups into the composer.
Each `Phone` owns a `PhoneView`: a live session id or draft workspace and launch
settings, copied from the foreground on admission. Before an intent one helper
focuses that view through the ordinary live switch or draft path; the phones
never follow one another. The foreground therefore belongs to the last device
that acted (or whose open/create completed), and `/remote off` returns to it.
Draft settings are saved back after each intent and `App::launch` is cleared;
`Remote::draft` keeps the foreground draft's settings for the status screen and
local return without leaking overrides into `adopt` on a later session open.
`Remote::opening` records the pending open/create's connection; only it changes
view on adoption and shows `status.loading`. A pending open allows live-session
work. While a create's first message is pending, all intents are refused so the
global first text and composer cannot be stolen; another device receives
`Another device is opening a session`.

Remote text uses the same submit/send paths without changing local composer
text or history, and the composer's line rules (`App::prompt_text`): templates
expand, built-in commands are refused.

Phone queries (`remote::query`) are Agent reads for the phone's own view that
do not focus it: the reducer checks the view and the per-phone limit of four in
flight, then a spawned task calls the Agent and posts `RemoteEvent::Answered`,
which frees the slot and answers the connection if it is still admitted. Each session keeps its last text-sending connection;
`restore_draft` sends `returned` there if connected, else to its viewers, else
keeps it in `Session::returned` until local control returns. A scoped
`Remote::notice_target` routes synchronous notices to the requester and async
session notices to its viewers plain and to other devices with the session
label. Foreground results/events and `with_background` set that scope;
open/rename/delete results retain their session id too. Labels are resolved only
when a notice is emitted, not on every event. A notice raised by the TUI's own
keys stays in the TUI (`NoticeTarget::Local`, the default); only a refused
sign-in goes to all phones, background attention to non-viewers (with text
free of TUI key hints), and acks only to the requester.

Notices follow four rules. What the user just caused and can see (a title, a
list row, the footer, a chip) gets no message, or at most the TUI flash. What
the user pre-authorized (a tool allowed for the session) is answered silently;
`/session` and `state.session.allowed` list those tools. What the user did not
cause and may need to act on (a background approval or finished turn, a refused
or returned message, a connection problem, an error) is a notice, errors until
dismissed. A decision is recorded only where it explains something: a denial
stays on its tool row. Each event reaches each surface once.
Deleting any viewed session is refused. A successful in-flight deletion also
resets views that opened its target meanwhile to default drafts, so every
session view still names a live session; a remote open reply for a target
removed from the catalog meanwhile is discarded rather than adopted.

`App::workspace` follows the workspace of each adopted session and is the default
for the next draft; a remote `new_session` can choose another existing directory
under the canonical configured roots. It creates no empty session. `--continue`
still selects against the original launch workspace. Session selection and the
phone catalog use canonical path-prefix checks, not file-content reads.

After each paced frame (or its inactive-terminal equivalent), and before exit,
`sync_remote` returns immediately without a phone. Otherwise it groups devices
by viewed session or equal draft settings (and loading ownership), builds and serializes `State` once per distinct view
(`view::Shared`), and each phone's own tracker (`view::View`) diffs that session
through `App::live`, foreground or background. Admission, a view change or a
viewed session epoch change sends a snapshot; otherwise it sends
revision-indexed block upserts and state only when the serialized value differs
from what that phone was last sent. Current-session marks, interaction, usage,
queue and background counts are per-view; catalogs and device locations are
shared. A phone whose queue overflows is dropped alone. Streaming assistants and
running tools whose output only grew use append-only updates until they finish
(or, for a tool, until its tail drops lines or another field changes), when one
full block is sent. The tracker keeps viewed revisions, streaming keys, byte and
line counts (a hash of a running tool's other fields), and the last serialized
state; it is not an event log. Catalogs are cached by generation,
so unchanged frames never rebuild or sort the full session catalog. `LiveTurn::started_ms` is captured once so elapsed
time belongs to the client. The footer and projection share the pure
`app::session::context_percent` calculation. The projection never imports UI code.

Only connection tasks own network timers: a 10 s HTTP head/first-message limit,
a 10 s no-progress limit while writing a reply,
20 s pings and a 60 s idle cutoff. At most eight unauthenticated connections can
wait, messages are limited to 1 MiB and each phone's outbound queue to 512
messages; overflow drops that connection. No remote polling timer wakes the main
loop. Shutdown allows at most 500 ms for `bye` and WebSocket close.
Phones sign in with the password in `remote.json` (generated on first use,
atomically written with Unix mode 0600); a right password makes an OS-random
token that is stored as a SHA-256 hash only after the reducer admits the phone,
so a handshake reaching a stopping listener (answered `bye remote_off`) stores nothing. Five wrong passwords pause sign-in for 30 s,
doubling per further five up to 15 min; `hello` tokens are never paused. Each
refusal reaches the reducer as `RemoteEvent::LoginRefused` for the status screen.
`/remote password` replaces the password, forgets every phone and signs out the
connected ones with `bye signed_out`; the command line never enters the prompt
history. The authenticated connection task runs
directory-picker requests through `remote::fs` using `spawn_blocking`. Each connection's reducer-owned close signal revokes its queued
work and suppresses late replies after replacement or shutdown; filesystem calls
already running may finish. Session checks share `fs::within` and
`fs::canonical_within`; phone-facing paths use `fs::display` to remove Windows
verbatim prefixes. The full phone protocol is specified in `design-remote.md`.

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
   `session_busy` (a post-turn compaction that is really running) the prompt block is removed, the text
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
   up to its turn-end line and no further, so a follow-up already sent stays
   after it. Cards keep the detail, input and output the live events knew,
   thinking and text the items lack stay, and so do blocks no item covers
   (compaction markers). The page's `interrupted` partial reply, when the
   cancel cut a request short, is the durable block of that request and
   replaces the streamed one in place; the streamed text stays only when the
   Agent sent none. In every case `changes.list` for the turn fills the change
   summary, `session.presentation` and `session.context` are refreshed, and the
   next follow-up is sent.
4. **`turn_finished`** only changes the phase text. A wait answer for another
   loop leaves the live turn running.
5. **Interrupt.** `Esc` sends `turn.cancel` once; the status line shows
   `Interrupting` until `turn.wait` resolves. Only the user's own send, `End`
   and the jump label follow the tail; a turn starting or ending leaves an
   anchored view alone. Per-request `request_usage` events add to the footer
   totals as they come; the wait's usage then replaces them.

The `(+a −b)` counts of the turn-end line are not in `changes.list`; they come
from paging `changes.diff` for each of the first 100 records. The turn-end line
of an old turn is rebuilt from its turn record when history is loaded, without
the duration and the counts, which are not stored; its only hint is `/diff`.
When the record carries `interrupted` (the display-only partial reply of a
request a cancel cut short, not a history item), it becomes an assistant block
of that request just before the line, so a reopened session shows the reply the
interrupt left on screen. A record without
details, or with a binary, truncated, stale or unreadable diff, makes the total
unknown, and the line then shows only `N files changed`, never a zero.

Steering: `Enter` while busy queues a `Steer` entry and calls `turn.steer`;
`steer_progress` moves applied steers into the transcript. Refused or unapplied
steers become follow-ups, which `Alt+Enter` (or `Ctrl+Enter`) also queues locally and `Alt+↑` takes
back into an empty draft, all of them, joined by blank lines. The first prompt
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
  leaves the card as is.
  A card whose turn ended, and every card read from history, keeps only the
  200 output rows an expanded card shows (the tail for `bash`, the head for
  other tools; `ToolCall::settle`).
- `/export` reads the whole canonical history the same way and writes Markdown.

## Compaction

`session_state.compaction` and `session.context.current_operation` drive the
`Compacting context · <phase>` status and the `● compacting` footer state. The
Agent has no completed event, so the one-line marker comes from the
`session.compact` result of a compaction the TUI started, or from `last_result`
in a `session.context` read (on open, after each turn, when a compaction ends,
when the Context sheet opens). A compaction between two requests of a turn
reaches neither: it is only in `session.context.automatic.current` and `.last`,
so the context is read every 2 s while a turn runs (`watch_context`, on the
tick), and a `last` that differs from the one the first read of the session
reported becomes a marker. All carry
an operation id and `push_compaction` skips one it already shows. `Esc` outside
a turn sends `session.compact.cancel` for the operation shown; inside a turn it
interrupts the turn, which ends the compaction too.
A `last_result` of a failed automatic compaction becomes one error notice. The
Agent's post-turn check is silent unless it compacts, so only a real compaction
shows as one; a send never triggers a compaction first.
The footer's `ctx` is the reported tokens over `budget.input_budget_tokens`
(over the model window when the Agent reports no budget).
Loaded blocks are never removed. A summary replayed from history is a block of
the tail, shown under its marker with `Ctrl+O`.

## Rendering

A frame touches only what is visible, and cost follows the changed block, not
the history length.

- **Block cache.** `Transcript` keeps a revision per block, taken from one
  process-wide counter and bumped on every mutation, so no two blocks of any
  session share one. `Layout::prepare` keeps the leading blocks whose revision
  is unchanged and looks the others up by revision: a block that moved (a page
  prepended, a turn reconciled in place) keeps its rows and only the prefix
  heights are rebuilt. A new width or fold flag does not re-render anything: it
  marks the affected blocks stale (a cached block records the width, the fold
  and which flags it read, so `Ctrl+O` skips a user message) and `Layout::settle`
  renders the stale ones within a window of rows around the viewport, with a
  margin of one window each way; the rest keep their old height as an estimate.
  A theme change clears the cache explicitly. Tool cards and assistant blocks
  are found by a scan from the end, where live blocks sit; there is no key map.
- **Viewport.** A frame binary-searches the prefix heights and draws only the
  visible rows by reference (`Layout::visible`), no copy. The text a click or
  drag needs is read back from the same cache (`Layout::row_text`), at the
  position the last frame recorded in `Layout::viewport`; the cache only
  changes inside `draw`, so that is what the screen shows. The welcome header
  is cached in front of block 0 (`RenderState::header_rows`), and clicks and the
  find bar skip it; on a draft or an empty transcript it is not in the cache and
  cannot be selected.
- **Streaming Markdown.** A streaming assistant block renders through a
  `StreamRenderer` (`ui/markdown/stream.rs`), keyed by request. It scans line
  by line for the last cut later text cannot change (a blank line outside a
  code fence, once the next line shows that a list, quote or table has ended),
  and keeps the lines before it as shared chunks (`Rows`, `Arc<[Line]>`), so a
  delta hands the cache the committed chunks without copying them and renders
  only the tail. Tests assert that the incremental result equals a full render
  at every byte. Folded thinking shows its newest three rows under an `earlier`
  hint, while it streams and after.
- **Cards.** Tool output is a bounded tail (400 lines, 64 KiB) while a turn runs;
  when it ends (and for any card read from history) a card keeps the 200 rows an
  expanded one shows. A collapsed card is a title row with its facts (duration,
  exit code, `+a −b`; a running card has none), the call's detail, a
  failure's first line and a `... (N lines, click or ctrl+o to expand)` hint,
  or a `running…` chrome row while a running call has no output yet (also the
  body of an expanded one). An expanded card shows a plain meta chrome row
  only when lines are hidden: `… N earlier lines` above a bash tail,
  `… N more lines` below other tools' head, or `… N lines` for unloaded
  history output. A click on a folded card's non-empty row expands it; on an
  expanded card any row collapses it. A drag selects and never reaches the
  click: `click_transcript` runs only on a release at the press cell.
- **Selection.** A row the UI adds around content (hints, timestamps,
  spacers) and a span after content (a card's duration) carry `rails::chrome`,
  a style that changes nothing on screen. `rails::content_cells` gives the cells
  past the rail and its pad, so highlight and copy take only content.
- **Scroll anchoring.** `Scroll` is `Follow` (stick to the bottom) or
  `Anchor { block, offset }`. Folding or unfolding a block pins the view's top
  (`App::pin_view`), so blocks open and close downward. Any upward scroll turns the position into an
  anchor, resolved against the current layout each frame, so growth below it
  moves nothing; reaching the bottom, by scrolling or because content shrank
  under the anchor, returns to `Follow`. While anchored and not
  at the bottom the last row carries a centred ` ↓ Jump to latest message · End `
  label, and a scrollbar overlays the right column for a second after a scroll.
- **Text.** Agent and tool text passes `wrap::sanitize` (control characters and
  ANSI removed) before layout; wrapping never splits a grapheme.

A frame is painted with the cursor hidden and shows it only once it sits at the
editor caret (`terminal::draw_frame`). On a Windows console that speaks ANSI,
`terminal/windows_frames.rs` collects the frame and writes it in one flush inside
a synchronized update, because the console shows every flush.

The transcript and the bottom band leave column 0 as a gutter. The band is plain
rows so the editor can grow: queue rows, the Working line (busy only), the
latest notice, the editor band (4–12 rows, plus the `/` or `@` completion rows
from `sheets::completion`), the footer. A selector sheet (sessions, models,
reasoning, approval, find) replaces the editor band; views replace the
transcript area. A view owns the keys, so the editor under it is drawn dimmed
with the placeholder `view open · esc to return`.

The Git diff view reads `changes.list` with `scope: "workspace"` on open and
passes each opaque `workspace:` reference to `changes.diff`, paging its hunks
without a `comparison` override. The Agent chooses index-to-worktree for
unstaged or untracked entries, otherwise HEAD-to-index. `/diff` takes no
argument; a turn-end click opens the same working-tree view. It needs a loaded
session because the Agent's workspace queries require one. Empty results say
`No changes in the working tree`; `no_repository` says the Git working tree is
unavailable (the Agent also uses this warning when Git is unavailable), and
`status_incomplete` says Git status is incomplete. RPC errors show their message.
Turn-end change counts still read the turn's retained changes independently.

### Composer, templates, title

The draft model is `app/composer.rs` (text, cursor, paste chips, undo, the
prompt history); `ui/composer.rs` only draws it. Every submitted prompt is
appended to `<home>/history.jsonl` (the last 100, written
atomically by `prompt_history.rs`; over 32 KiB stays in memory) and loaded at
start. `app/templates.rs` reads `prompt/commands/*.md` from the TUI directory at
start and on `/reload`; a template is a command that expands `$1`, `$@` and
`${1:-default}` before the text is sent as a normal prompt. `title.rs` derives
the terminal title (`workspace · session · idle|working|approval`) and the bell
(a turn over 10 s ends, or an approval is asked) from a snapshot of the app
state; `app/notify.rs` writes the bytes after each loop turn and prints the
resume hint on exit.

### Footer context figure

The footer shows `ctx N%` only from `session.presentation.context`, as the Agent
reported it for the last request, and `ctx ?` otherwise; the TUI never
substitutes an estimate. `presentation` is read on open, after each turn and
compaction, and on request-usage events at most every 2 s while a turn runs. The
Context sheet also shows the budget's history estimate, labelled as one. The git
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

Plain `cargo test`. Two `tests/*.rs` targets run outside `cfg(test)`:
`local_timezone.rs` checks the production local-time path and
`built_in_agent.rs` serves a composed config from the real built-in Agent. Unit tests sit next to pure
logic (wrapping, Markdown incremental-equals-full, composer, chunk assembly,
layout cache, config, CLI). `src/app/tests` drives a real `App` through
`testing::FakeAgent` over an in-memory pipe with paused time. `ui/snapshots.rs`
compares `TestBackend` frames with `tests/snapshots/*.txt` (`UPDATE_SNAPSHOTS=1`
rewrites them); `tests/fixtures/agent-events.jsonl` feeds the event decoder test.
`node --test tests/remote_web.mjs` imports the client's pure modules (`core.js`,
`markdown.js`) and runs `theme.js` in a sandbox, so protocol handlers, Markdown
and path helpers are tested without browser rendering or a network. Socket tests
exercise the actual listener and the embedded files with in-process clients on
loopback port 0.
Real-model runs are manual.
