# Architecture

How `minicore-tui` is built, and the rules later changes must keep. The
original plan is [design-v0.4.md](design-v0.4.md); where the two differ, this
document and the code win.

## Scope

`minicore-tui` is the terminal frontend of `minicore-agent` (which links
`minicore-runtime`). The Agent crate is linked only to serve as this
executable's `agent` subcommand: `main` hands those arguments to
`minicore_agent::cli::run` before anything else. The TUI talks to that child over
stdio NDJSON JSON-RPC (Protocol v4) and calls no other Agent API. It never reads the Agent store or
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
    resident.rs   loaded session collection, captured Target and selected epoch
    session/      one Session: mod (state and coordinates), body (streamed events),
                  completion (terminal facts and canonical bodies), history (window installation)
    operation.rs  typed mutations shared by terminal and phone adapters
    control.rs    authoritative snapshot application and coalesced control reads
    recovery.rs   bounded completion/history discovery, scheduling and result handling
    read.rs       verified history/result paging, assembly and budgets
    local_files.rs serial coalesced history/theme writes and reload composition
    actions.rs    Action (keys and commands land here) and the IO each starts
    ops.rs        applies SessionOp results and Agent events
    remote/       mod (mode, connections and device views), intent (admission and dispatch),
                  pending (captured targets, Agent calls and acknowledgements)
    tasks.rs      task ownership, bootstrap, waits, changes and export
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
    tests/        reducer tests through Harness, with shared phone support
  testing.rs    scripted Agent transport (tests only)
  testing/fixtures.rs  literal wire data shared by tests
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

### Protocol and transport ownership

The handshake requires RPC v4. Open/create responses include a required load and
snapshot, while rename responses contain session information only. Input receipts
must name the requested session/load and an accepted input ID. A malformed receipt
leaves acceptance unknown and cannot trigger an automatic resend.

Responses consume their decoded JSON value; pending calls unregister on cancellation.
Reader, writer and child exit share one terminal failure reason. The stdout reader
alone publishes events and the final connection-close notification, awaiting queue
space instead of dropping closure. Unknown future body events remain extensible;
an unreadable known control snapshot is a protocol failure, never silently ignored.

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
app stays in the draft (`app/draft.rs`): `app.resident.current()` is `None`, the screen is
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

`SessionOp` carries results for a resident epoch. `spawn_for` and `call_for`
attach that epoch when work begins; `on_msg` looks up the same resident and drops
results after it has gone. Each resident owns its ordinary tasks, cancelling
unfinished reads and waits when removed. Recovery reads also retain their shared
slot and byte budgets. Input acceptance and phone acknowledgements have separate
operation owners, so changing the selected view never cancels an accepted input.

`App::resident` contains every loaded session and a separate selected epoch.
Each session is constructed with its immutable Agent `load`; the same identity
is used by commands, snapshots and result reads. Navigation changes selection
and restores the existing composer, scroll and layout behavior. Background
reducers directly receive the target epoch and update that resident's saved
scroll; they never swap the selected session. Results for pickers and diff views
are applied only when their target remains selected. Notices still identify
background sessions, and completion/approval notifications keep their existing
bell and unseen rules.

Local and phone mutations share model/create request preparation and operation
identity allocation. `operation::Operation` executes the request and validates
its reply once. `Outcome` is applied by the reducer; adapters retain
their own draft ownership, feedback and acknowledgement identities. The terminal
continues to follow phone navigation, but operation targets do not depend on that
policy. A phone open/create installs its resident before deciding whether its
unchanged view still owns focus; no temporary selection needs to be undone. Pending phone operations distinguish a
draft, a stored session ID and a captured resident `Target` (session ID, load,
epoch); a delayed result never recaptures a newer resident as its target.

`Session` remains one state owner. Its body module owns early events and request
ordering; completion owns canonical replacement and terminal facts; history owns
window installation and preserved regions. Recovery chooses one ready completion
per scheduler slot and keeps both result and history recovery handling together.
Verified reads carry their original history pin instead of copying its derived
fields, and RPC errors use the read's outer `Result`.

History, theme and reload composition use one coalescing local-file worker.
Only the latest pending history/theme value is retained, blocking filesystem
work runs off the event loop, and writes to the same file cannot finish out of
order. Normal exit flushes this worker. Composer owns editing and in-memory
history only; input acceptance schedules persistence.

### The draft

A session is created by the first message, never at startup, so the session
list holds no empty sessions. While `app.resident.current()` is `None`:

- `App::launch` holds the settings the next session takes: the command-line
  flags, then what `/model`, `/reasoning`, `Ctrl+L`, `Ctrl+P`, `Shift+Tab` and
  `/new <profile>` set, which the footer shows instead of `session.configure`.
  The footer resolves them with the profile's defaults (`profile.list`; the
  Agent's default profile comes from `config.toml` and is unknown for a raw
  `--agent-config`) through `App::model_id` and `App::reasoning`.
- The first `Enter` takes the editor's text (recorded in prompt history only
  after confirmed input acceptance), parks it in `App::first_message` and sends `session.create`.
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
still interrupt. Entering remote recalls every queued message (`input.recall`) into the composer.
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
label. Results and events set that scope from their explicit session target;
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
(queue of 64), a stdout reader, a stderr reader (last 200 lines) and a child
waiter. A malformed frame or a child exit ends every pending call and posts
`Incoming::Closed`, which becomes the fatal screen.

Two bounds are derived from what the protocol can legally carry, never from what
a frame happens to contain:

- **From the Agent, `CONTROL_FRAME_MAX_BYTES` = 64 MiB.** A legal control frame — a
  `session_snapshot` for a session whose queue holds 32 maximum inputs of 256 KiB
  each, plus the execution input, a maximum approval and the 256-entry completion
  catalogue, all JSON-escaped — measures 52,237,130 bytes. The reader admits the
  whole frame, envelope and newline included, and the same constant bounds the
  decoder's own tests.
- **To the Agent, `MAX_REQUEST_BYTES` = 8 MiB**, matching the Agent's own
  request ceiling. One `input.push` carrying a maximum
  256 KiB input whose bytes all escape to six characters measures 1,573,049
  bytes, so the local bound must clear it with room for the envelope. A larger
  frame is refused before a byte is written, because the Agent terminates on an
  oversized request line; nothing is truncated to fit.

The reader accumulates a line until the newline or the bound and never stores a
prefix, so a frame is either read whole or reported as unreadable.

Read queries (`session.read`, `turn.result`, `tool.*`, `workspace.*`,
`changes.*`) hold one of 3 client permits, under the Agent's cap of 4. Capacity
refusals (`-32019`, `-32020`) are retried in `turn.wait` (three retries, 700 ms
apart), completion recovery (250 ms, 1 s, 4 s) and `changes.diff` (twice).
Exhausted completion reads remain unsettled; an unrelated snapshot revision
cannot restart their retry budget.

The handshake requires `protocol_version == 4` and the eleven capabilities in
`REQUIRED_CAPABILITIES` (`protocol/mod.rs`), including `workspace.status` and
`tool.output`; `workspace.search` is not used. Older protocols and missing
capabilities are rejected before catalogs or session operations. The phone
WebSocket protocol remains v3. `session.presentation` has no capability
string and is called unconditionally. Shutdown serializes callers and sends
`agent.shutdown` once; its ACK is not child-exit proof. The actual child owner is
joined before pipe tasks are aborted, with a fresh 3 s exit grace after the
separately bounded 3 s ACK wait. On a real timeout that owner kills and reaps the child; a final 1 s bounded fallback
aborts a stuck owner. Cancellation retains the owner for a subsequent shutdown,
and completed shutdown is idempotent.

## Control state

One observation decides what a session is doing. `session.state`, `session.open`
and `session.create` each carry a `SessionSnapshot`, and the Agent pushes the
same shape as the `session_snapshot` event. `App::apply_snapshot` is its only
writer: `execution` (`none`/`starting`/`running`/`finishing`, with the input that
started it), `queue`, `pending_interaction` (with loop/request identity, so a
prompt works without its tool card), `compaction`, `block_reason` and the
completion directory. An installed persistence block remains visible above the
local dock even while idle; a still-actionable interaction keeps its question,
editor and exact-target answer controls below that blocking status.

Identity decides whether an observation applies. A snapshot for another live
session is routed to it; one for a session this client has not loaded is
ignored, because only an explicit open installs an instance. Within the
installed `load` a `revision` equal to or below the applied one is a duplicate or
a delayed observation and is dropped whole — never merged field by field.
`session.open`/`session.create` name the `load`; every mutation of a loaded
instance sends it back, and the Agent rejects a missing or stale one before the
side effect.

Command receipts report their own result: a refusal returns the text, a removal
hands the entry back to its requester, and a push names the accepted input.
They no longer set the queue or execution; each receipt asks for the current
snapshot, so a delayed answer cannot resurrect a consumed entry.

## Completion recovery

The completion directory is what makes missed turns discoverable. Its entries
are the load's contiguous suffix of finished turns, each with the outcome,
persistence, availability and `after_saved_loop_id`. Completion sequence numbers
order retained turns, and the saved-history anchor places them beside durable history.

A snapshot can arrive before an outstanding open/create has returned. Only
while an installation is outstanding are such observations eligible for holding. Up to two such snapshots, totalling at most
`PRE_INSTALL_BUDGET_BYTES` (two frame ceilings, 128 MiB, because two maximum
snapshots exceed 64 MiB), are held and installed by the `session.open` that
matches their session id. Past that budget the oldest are dropped, and because a
dropped hold could otherwise leave control empty, installing a load always asks
for one `session.state` afterwards.

`RecoveryState` (in `app/recovery.rs`) keeps `observed_through` from the directory
and `settled_through` for what has been read or explicitly declared
unreadable. Every unsettled entry is read through `turn.result` with the
load, at most one at a time. Canonical installation or an explicit partial or
unavailable terminal region settles an entry. A partial read is not installed as
canonical; pending and failed reads remain unsettled. A retained complete body
that cannot be placed also remains unsettled. Failures retry three times (250 ms, 1 s, 4 s),
then keep an exhausted annotation and a visible `/recover` hint instead of
claiming the source is unavailable. Partial, unavailable and exhausted bodies
keep a bounded per-turn annotation as well as a dock indicator. An unseen partial
or unavailable completion also gets an identifiable terminal region, its known
outcome and saving fact, without being installed as an empty canonical answer. A body is
installed once, either over the region the turn streamed
into or, for a turn the client never saw, after its `after_saved_loop_id` anchor;
a missing anchor is shown as an ordering gap rather than guessed. A saved-history
anchor is a lower bound even when an earlier unsaved completion was already
reconciled; delayed result arrival cannot move a turn ahead of that anchor. Late
deltas for an installed turn are dropped.

The completion directory is a window. A missing prefix first schedules a saved
history catch-up, bounded to 64 pages or 16 MiB per round. Discovery alone never
advances `settled_through` across that hole. Each bounded round retains its
verified newer suffix and its exact item range, captured end and history revision;
`/recover` reads the adjacent missing range using a freshly validated probe, not
a cached pin. Old-prefix pagination cannot settle an unread newer suffix. Only
coverage of the complete pinned history permits settling the gap, with an explicit
warning that expired unsaved identities may be missing. A failed or bounded-out
read keeps the gap unresolved and `/recover` visible in the dock. Even after a
successful saved fallback, the persistent dock explicitly warns that unknown
unsaved turns may be missing outside the retained completion catalog.
History metadata overlap is not full-body
coverage: a replacement needs the prompt boundary and a verified end boundary;
uncovered regions, later live output and session drafts stay intact.

Recovery identities are admitted before insertion, at most 256 per load and
1024 across residents. At the global limit, the rotating scheduler may defer one
pending identity not yet attempted in its current retry round from another
resident to admit waiting work. Fetching, retrying, exhausted and settled
identities keep their state and attempt counts. If all retained identities are
protected this way, admission waits;
`/recover` in stalled sessions retries failures and can free capacity when they
settle. Successfully deleting a resident also frees its capacity and reschedules
recovery. Capacity backlog clears when retained directory entries are admitted
again, without requiring a new snapshot. Dropped early body events and expired
catalog prefixes have separate gap warnings that backlog progress cannot clear.

Recovery task ownership outlives catalog eviction: an in-flight read still owns
its global slot until its outcome is applied, or its Session drops and aborts
the task. Its descriptor stays within the identity budget even after catalog
eviction or a history-watermark advance, so the fetched body is still handled.
A retired read that fails, stays pending or cannot be placed leaves an explicit
warning that it left the catalog; it is not advertised as retryable or read again.
One epoch cursor rotates admission among resident sessions; each gets
one start under the single shared recovery slot, including history catch-up.
Continuous foreground work or availability changes cannot refill that slot
ahead of other residents. An overdue retry cannot arm an idle wake while the slot is occupied.
Missing final tool facts use up to 64 serial metadata reads within that same
recovery slot/workspace, never raw output downloads; a tool with no final fact
is not labeled succeeded. The remaining facts can still arrive as tool metadata.

The non-default `e2e-observe` build feature observes accepted controls and actual
installed canonical transcript regions on the production main path. Its opt-in
trace hashes normalized installed channels, reads displayed saving markers and
records a global ordered inventory of the actual transcript, not just per-loop
hashes and turn-end order. The inventory includes unexpected loops, assistant
channels, tools, user/steering hashes, actual source identities where available,
terminal regions and explicit preserved provenance. User identities travel with
transcript mutations in product source-identity metadata; absent source input IDs remain
null, never guessed from text. Fetched content never filters that projection.
Incomplete target regions emit their actual noncanonical classification and
terminal saving/outcome. It never includes prompt/answer text or provider errors.
The producer fails closed before exceeding 1 MiB per row, 20,000 events or 32 MiB
per trace; it does not truncate a row or continue after dropping observations. Default builds have no observer
state or work. Query counts increment on actual writer enqueue, query gauges
follow permit ownership, and reader reservation gauges/peaks follow charged
workspace ownership through assembled results, cancellation and drop. Reservation
bytes are not allocator RSS. Recovery queries are distinguished by the actual
recovery task's scope, not inferred from method names. Backend-only counters are
not synthesized. A single feature-only application gate can hold one resident
backend message before application while stdio reading and the Agent continue;
it uses a private arm/ready/release handshake with a 30-second deadline. The
feature-only phone drain gate uses the same observer through
`App::observe_phone_gate`. All TUI acceptance variables (trace, application gate
and phone gate) are removed from the Agent child environment.

Text-input interactions follow the Agent's answer_kind, not whether choices are
empty. Session.interaction_editor is separate from the conversation composer and
resets only when the authoritative interaction identity changes. Local answers
capture the exact loop/interaction/load and preserve the Agent's literal text
boundary; neither a receipt nor an older resolution clears a newer editor.
The Runtime's `input_provided` tool result is terminal: live events, canonical
history, display summaries and hydrated execution records all project it as a
done card, without ending the surrounding model turn.

## Turn lifecycle

The control snapshot says what is running; `turn.wait` reports terminal metadata
for its named turn, including outcome, usage and request counts. Successors are
discovered from snapshot execution and completion identities.

1. **Submit.** `Enter` pushes `steer`; `Alt+Enter` pushes `follow_up`. The receipt
   reports acceptance and any start error, but only a snapshot opens or changes
   the live execution region. `start_adopted_turn` registers its wait. A delayed
   predecessor wait cannot keep a successor from becoming live. Only an explicit
   rejection restores unsent text; an unknown or unclassified acceptance does
   not offer an automatic duplicate submission.
2. **Events** of body content (`output_delta`, `tool_*`, `request_started`,
   `input_delivered`) update the live turn's blocks by key, so order does not
   matter. Unknown loops enter an early-body cache (2 MiB and eight identities
   per load, 8 MiB globally), including tools and delivered inputs. Another live
   turn is not proof of identity. A confirmed execution replays its cache once;
   overflow flags recovery rather than creating a visible speculative turn.
   Installed canonical bodies reject late body appends, while tool auxiliary
   metadata can still update an existing card. The Agent's auxiliary commit is a
   `tool_execution` update with `recording: saved` or `failed`, not a new body
   event. Its exact ToolRef/load updates the card after canonical installation;
   a turn's saved marker never implies that auxiliary recording is saved.
   `dropped_before > 0` marks the turn as having a gap. Control changes arrive
   through `session_snapshot`. Late body updates change their own region; only events for
   the current live loop change its working phase and tool label.
3. **`turn.wait` resolves** with terminal metadata for its named loop. A failed
   wait makes one stored-outcome query, then reports an error if no outcome is
   available; it neither invents a terminal result nor polls forever. This fallback
   keeps the wait's captured resident load and checks the response against it,
   rather than silently turning a late wait into an unbound history query. `end_turn`
   updates that region's line, settles and counts only its cards, and replaces
   only that turn's accounted usage contribution, preserving a newer successor's
   streaming output and usage. It never clears the
   snapshot's live execution. Mutation receipts refresh control. The
   completion directory then has the turn, so its body is recovered through
   `turn.result` (paged, chunk-assembled) and **reconciled**: it replaces the
   blocks the turn streamed, up to its turn-end line and no further, so a
   follow-up already sent stays after it, which is where a successor's prompt
   already sits. Cards keep the detail, input and output the live events knew,
   thinking and text the items lack stay, and so do blocks no item covers
   (compaction markers). The page's `interrupted` partial reply, when the
   cancel cut a request short, is the durable block of that request and
   replaces the streamed one in place; the streamed text stays only when the
   Agent sent none. The result's `persistence` and `persistence_uncertain` put
   "not saved" or "save unconfirmed" on the turn-end line. In every case
   `changes.list` for the turn fills the change
   summary, `session.presentation` and `session.context` are refreshed, and a
   background turn is announced.

The `(+a −b)` counts of the turn-end line are not in `changes.list`; they come
from paging `changes.diff` for each of the first 100 records. The turn-end line
of an old turn is rebuilt from its turn record when history is loaded, without
the duration and the counts, which are not stored; its only hint is `/diff`.
A failed turn's line is followed by `protocol::Failure`'s hint (one table shared
with the phone's `turn_end.failure`) and, when the provider sent one, its
message behind a quote rail; `Ctrl+O` shows all of it instead of three rows.
When the record carries `interrupted` (the display-only partial reply of a
request a cancel cut short, not a history item), it becomes an assistant block
of that request just before the line, so a reopened session shows the reply the
interrupt left on screen. A record without
details, or with a binary, truncated, stale or unreadable diff, makes the total
unknown, and the line then shows only `N files changed`, never a zero.

Input queue: the Agent owns one queue per session (`protocol::input`: entries
with an id, a mode and a text; steers run before follow-ups; a `paused` flag
after an interrupted or failed turn). `Session::queue` only mirrors the snapshot:
every `input.*` answer asks for the current control observation, and the queue
it reports is the only one shown. The mirror drives
the dock rows, the paused hint and the phone's `queue`; nothing in the TUI
dispatches. `Alt+↑` and entering remote ask `input.recall`, and the answer's
texts go back to the composer joined by blank lines. `Enter` on an empty draft
asks `input.resume` while the mirror is paused. The phone's `unqueue` becomes
`input.remove` with stable session/load/input identities. Its ACK is delayed until
the Agent result and returns edit text to that same operation/connection; an
already-consumed entry returns `input_not_queued` without an index/text fallback.

An `input_delivered` event adds steering body blocks by its explicit input ID and
candidate request index, never text equality or the currently active request. A
steer precedes its request's assistant/tool blocks, or stays before its own
terminal if that preparation was never issued; it never crosses into a successor.
Late earlier request/tool events respect these same boundaries. Repeated IDs are
boundedly deduplicated, identical-text IDs remain distinct, and a canonical-closed
turn cannot be resurrected. Source identity/request metadata travels with copied
or merged blocks in all builds; a historical source lacking IDs remains unknown.
Delivery never removes queue entries; only a snapshot changes the mirror. Unknown deliveries
share the bounded early-body cache with deltas and tools. A snapshot-confirmed
execution opens its own prompt from execution.input (which may be null).
Deleting a session that holds queued entries is refused, as the Agent does.
The first prompt of an untitled session becomes its title through `session.rename`.

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

Turn and history reads check the response identity, chunk encoding and byte
continuity, item JSON, cursor progress and the requested item range before any
content is installed. A pending turn is not an empty result. A missing turn,
invalid page, oversized item or exhausted read workspace leaves the existing
transcript in place and reports the problem. RPC v4 result pages repeat the
requested load and immutable result_id. Later requests carry that result_id;
changing identity is an error, expiry is explicit unavailable, and live-to-stored
availability can change without changing the immutable body. Partial and
unavailable bodies retain a bounded per-turn UI annotation, not a canonical
success; exhausted errors remain unsettled and `/recover` starts a fresh round.

At most two history, turn or export reads assemble at once per client. Each
workspace has a 64 MiB budget; recovery can occupy only one of the two slots,
leaving the other available for foreground history and export. The budget includes
conservative reservations for decoded data, metadata and page copies; a completed result holds its slot until the
reducer finishes installing it. An item above the 8 MiB automatic decode limit
produces a partial-read notice rather than an incomplete replacement. This bound
does not include the existing transcript or the RPC client's general buffers.

Each block has one `Origin`, combining the user identity with an optional history
item and part. The history mapper assigns that source as it emits rows; result
reads do not reuse their turn-local item indices as history positions. Record
terminals and the display summary have distinct parts, so a result-only page
keeps an existing terminal and the summary remains separate from the raw item
at the same index. Copied and reconciled rows retain their origins.

Catch-up and ordinary pages overlapping retained source rows use the same
history installation path. Source positions combine partial windows without
deduplicating equal steering text. `loaded_from..loaded_end` records one
continuous loaded window: adjoining windows extend it, while a disjoint newer
window keeps the old rows without claiming the hole is loaded. This display
boundary does not replace the pinned coverage needed to settle a recovery gap.
Source identities also keep scroll anchors on their row through a rebuild.

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
  other tools; `ToolCall::settle`). Tool truncation comes from structured
  execution and summary fields or the local buffer bounds, never result text.
  A literal `[truncated]` line stays visible and searchable, including old
  history whose text does not identify whether it was an Agent marker.
- `/export` reads the whole canonical history the same way and writes Markdown.

## Compaction

Only snapshot.compaction drives the `Compacting context · <phase>` status and
the `● compacting` footer state; context and command replies cannot write it. The
Agent has no completed event, so the one-line marker comes from the
`session.compact` result of a compaction the TUI started, or from `last_result`
in a `session.context` read (on open, after each turn, when a compaction ends,
when the Context sheet opens). Automatic compaction progress also arrives through the snapshot. Context is
read every 2 s while a turn runs (`watch_context`, on the tick) for its ledger;
a new automatic.last operation becomes a transcript marker, never control. All carry
an operation id and `push_compaction` skips one it already shows. `Esc` outside
a turn sends `session.compact.cancel` for the operation shown; inside a turn it
interrupts the turn, which ends the compaction too.
An `automatic.last` that failed (neither compacted nor cancelled) becomes one
error notice. The Agent's turn-start check is silent unless it compacts, so only
a real compaction shows as a marker; a send never triggers a compaction first.
The footer's `ctx` is the reported tokens over `budget.input_budget_tokens`
(over the model window when the Agent reports no budget).
Loaded blocks are never removed. A summary replayed from history is a block of
the tail, shown under its marker with `Ctrl+O`.

## Rendering

A frame draws visible rows and reuses unchanged block layouts. Preparing the
layout still compares the revision prefix; a cleared cache renders all loaded
blocks once.

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
  only the tail. Width or style changes reset the Markdown cache; folding reuses
  its parsed rows. Tests assert that the incremental result equals a full render
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
  past the rail, its pad and a leading chrome span (a quote rail), so highlight
  and copy take only content.
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
passes each structured `workspace` change reference to `changes.diff`, preserving
its HEAD and entry identity while paging its hunks
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
prompt history); `ui/composer.rs` only draws it. Only positively accepted local
prompts are appended to `<home>/history.jsonl` (the last 100, written
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
branch comes directly from `workspace.status`, read independently of
`session.presentation` on open, foreground activation and after each turn.
Only a complete observation with `repo_available: true` supplies the branch;
a failed, incomplete or non-repository result clears it. Both answers target
the captured resident epoch, and `Session::branch` serves the terminal and
phone projection. Periodic usage refreshes never run Git.

Local sends use `app/unconfirmed.rs`: an operation ID owns the exact target
session/load/epoch/mode and original submission. A global `Msg::LocalPushed`
receipt survives view changes independently of session-body task epochs. Up to
64 operations / 16 MiB are retained, including pending submission undo data; full
capacity rejects before an Agent call. Unknown acceptance is never restored as
unsent, entered into ordinary prompt history, or automatically resent. Rejected
input restores only into the unchanged empty original draft; otherwise it remains
separately reviewable. New equal-text edits are not used as correlation.

F8 or `/unconfirmed` opens the existing sheet-style review. A separate bounded
editor changes a possible retry without replacing the original uncertainty or
new conversation draft. Confirmation modes bind an operation ID, not a list row;
Enter/Esc cancels a possible-duplicate warning and only `y` sends a new-ID retry
to the original live target. Forget is likewise an explicit local decision, not
an Agent cancellation. Old unavailable loads are never silently retargeted.
Retention is process-local, not a persistent ledger. Observer checkpoints expose
hash-only current `local_input_state` and authoritative `delivered_inputs` beside
the unchanged actual global inventory; no new event kind or counter is invented.

On Agent exit or a protocol failure the screen becomes a fatal message with the
reason and the last 20 stderr lines; `q` or `Esc` quits, while F8 still opens the
retained local confirmations for review/copy without resending.

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
`node --test tests/remote_web.mjs` imports the client's pure logic (`core.js`)
and runs production UI functions in a sandbox, so protocol handlers, Markdown,
DOM reuse and path helpers are tested without a browser or a network. Socket tests
exercise the actual listener and the embedded files with in-process clients on
loopback port 0.
Real-model runs are manual.
