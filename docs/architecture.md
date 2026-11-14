# Architecture

The executable owns one persistent MiniCore Host and one terminal application.
It links Host 0.11.0 at the immutable revision in `Cargo.toml`; all native
components come from that revision. The frontend calls `HostClient::in_process`
and the typed `SessionClient` APIs. There is no frontend protocol model, child
Agent, transport adapter, Webpage service or remote view layer.

## Module boundaries

| Module | Responsibility |
|---|---|
| `main.rs` | Startup, selection, terminal ownership, frame scheduling, signals, graceful Host shutdown |
| `host.rs`, `host/diagnostics.rs` | Production resource preparation, saved Host opening, process control, diagnostics lifetime |
| `config/`, `cli.rs` | TUI paths/preferences, native initial configuration files, explicit command-line inputs |
| `app/mod.rs` | Terminal reducer and immutable messages from asynchronous work |
| `app/tasks.rs` | Typed Host reads/mutations, bounded UI delivery, subscription pumps and fixed-window catch-up |
| `app/resident.rs`, `app/live.rs` | Full Session addresses, local epochs, per-Session drafts and foreground navigation |
| `app/session/`, `app/control.rs` | Native snapshot projection, independent revisions, interactions, canonical history and output ranges |
| `app/actions.rs`, `app/ops.rs` | Captured user intents and results installed into the original target |
| `app/unconfirmed.rs` | Bounded local ownership of sends with pending or unknown acceptance |
| `app/composer.rs`, `app/keys/`, `app/mouse.rs` | Editing, paste, key dispatch, selection and scrolling |
| `app/local_files.rs` | Serialized/coalesced preference and accepted-input history writes |
| `history.rs`, `transcript/` | Native pagination/export, positioned records, tool cards, revisions and layout caching |
| `ui/` | Terminal rendering, sheets and views; no execution owner |
| `terminal.rs`, `terminal/` | Terminal restoration, platform input decoding and frame batching |
| `testing.rs`, `app/tests/` | Real in-process Host reducer harness with named model/resource test doubles |
| `tests/built_in_host.rs` | Production resources against local provider HTTP fixtures, persistence/reopen and shutdown |

Host's native resource builder owns the provider clients and built-in tool
registry. Configuration files name credential sources. The frontend neither
owns a credential cache nor directly edits Host storage or workspace files.
Its file operations are local preferences, prompt templates/history, external
editor drafts and user-requested exports.

## Startup and lifetime

Configuration preparation occurs before entering fullscreen. A new Host root
uses the provided initial configuration; an existing root uses its saved Host
configuration before resources and logging are prepared. The Host retains its
exclusive data-root lease for its full lifetime.

A new Agent is created only when the Host has none or the user explicitly
requests one. Native Agent creation also creates its default Session. Selection
uses full Agent and Session identities and actual `last_activity_at` values.
The current directory is an override for new Session creation, not a rewrite of
an opened Session's workspace. Startup model/reasoning overrides are consumed
once. Later navigation reads the selected Session's saved settings.

The main loop owns the terminal guard and external-editor handoff. It restores
the terminal before supervising shutdown. Graceful shutdown waits for the real
Host `Stopped` lifecycle, including owned execution, scope cleanup and saves.
A shutdown fault is reported without dropping the Host's lease or pretending
cleanup succeeded. An explicit Ctrl+C at the shutdown prompt requests force.
Local UI task cancellation cancels only frontend waiters; accepted Host work
remains owned by Host.

## State and identity

A resident view has a native `SessionRef { agent_id, session_id }`, Host identity,
Host incarnation and local epoch. A new load may replace the runner within the
same Session; it does not replace the Session address. Asynchronous reads and
mutations retain their original client/epoch and full native object references.
Navigation never retargets an in-flight request.

Controller revision and runner revision are different dimensions. Controller
facts advance only with their own revision. Runner facts advance only within
the matching Load. A late completion for Turn A can add Turn A's history but
cannot end or replace a newer active Turn B. A late answer result cannot clear a
new interaction's submitted state. Cancellation names the captured Turn ID,
never whatever Turn happens to be active when a future runs.

Native state snapshots own lifecycle, residency, queues, pauses, pending
interactions, compaction, persistence and blocking. Mutation receipts trigger
reads; they do not synthesize successful execution, delivery, cancellation or
saving. The global state subscription updates nonresident catalog rows; local
resident views also receive completion, message, interaction and output events.
Fresh catalogs are overlaid with already observed state to avoid downgrading the
view with an older read result.

Post-bootstrap catalog reads share one coalescing gate. Mutations and Agent
progress request a refresh; requests arriving during a read coalesce into its
successor. Automatic workspace status reads have a separate per-Session gate
and run on opening or switching, workspace changes and Turn completion.
Unrelated scheduling and model controls do not start an idle status read. A
pending result from a different configuration revision is retried after fresh
Session Info. Sessions with no workspace skip automatic status reads.

Agent deletion confirms a captured exact child-Session set. Native scope
validation rejects a changed child set; the frontend does not expand a cascade
in response to that rejection. Close and delete admission are distinct from
completed cleanup and data deletion.

## Observations and recovery

The App message channel is bounded. A slow terminal can backpressure the
subscription reader instead of accumulating an unlimited frontend event queue.
Host's own subscription overflow produces its native `ResyncRequired` signal.
That signal starts a new subscription and reads fresh state/history. Manual
`/recover` uses the same observation path. Replacing a subscription aborts and
drops the old frontend pump; it does not cancel a Turn.

`OutputBuffer` is the native SDK implementation. It tracks source/channel byte
positions, overlap, UTF-8 boundaries, gaps and fixed final positions. Missing
ranges are read with explicit ends; completed streams are replaced by canonical
history records. Once complete content is installed, heavy output buffers are
released and small identities suppress late replay. Tool arguments are accepted
only from complete native invocation/snapshot facts.

Ordinary read failures remain visible and do not replay mutations. A send with
unknown acceptance stays in the F8 owner with its original target and text.
Only explicit user confirmation sends it again, with a new Message ID and a
possible-duplicate warning. Verified acceptance enters local prompt recall;
rejection restores the original draft. Message IDs or text alone do not
identify one history occurrence.

## History, views and memory

History is read through native `HistoryReadRequest` and opaque cursors. A
window has a source, fixed upper bound and ascending record positions; backward
pages are prepended without changing the window. Catch-up verifies all positions
between its prior upper bound and a fresh native tail. Reading older pages does
not imply that a newer gap has been recovered.

`history_position`, `recorded_at`, Turn ID and response index identify canonical
content. They reconcile live output, late results and history rereads without
combining identical text or reused Message IDs. A compaction record is displayed
with its real source/coverage; it does not erase earlier transcript content.
Interrupted partial responses and native terminal records remain distinct.

Each installation batch indexes existing native positions and block identities.
Only matching records are reconciled, and unchanged replays do not reorder
blocks or invalidate layout revisions. The indexes end with the batch, so direct
streamed block updates do not need to maintain a second persistent identity map.

Export reads a separate full forward window and writes pages incrementally.
It is independent of loaded screen history and preserves Host's fixed export
upper bound. Workspace files, status, change records and diff pages also use
typed Session APIs. Fragmented native diff lines preserve their true line
numbers and UTF-8 content.

Rendering uses per-block revisions and visible-row layout caches. Anchors are
rebased when earlier records are inserted, and folding opens downward. Tool
presentation tails and layout/undo caches retain their existing limits. Local
unconfirmed inputs have a named operation/byte budget, and completed output
buffers are released. The transcript itself retains loaded records: memory can
grow with history explicitly loaded or accumulated during a conversation. It
must not silently discard canonical records or reuse a cursor after evicting a
window. This version does not claim constant memory for arbitrary history size.

The main loop has no idle animation timer. Input, Host messages, an active
animation or a known UI expiry schedules frames; a frame cap coalesces bursts.
Rendering does no Host or filesystem I/O. Clipboard selection omits chrome,
status hints, timestamps and layout rails.

## Native meaning in the UI

The model catalog and reasoning choices come directly from Host. Unknown Agent
creation defaults are not displayed as if read from a selected saved Agent.
Session updates use expected configuration revisions, and active Turns keep
their already prepared configuration snapshots. Rename completion refreshes the
catalog and reads metadata for the original resident Session, independent of
foreground selection. The existing identity/revision checks install that read;
the update receipt never supplies synthetic metadata or controller state.

The context view displays actual raw-history counts, compaction information and
last-response usage. Last-response tokens are not a current context-window
percentage. `Persisted`, `Pending`, `Failed` and `NotApplicable` are different
save states. A creation/control/reload receipt in progress is not a success
notification.

Pause/resume affect scheduling; load/unload affect residency; close is a
lifecycle transition. Esc cancels its captured Turn with `keep_inbox: true`.
It does not implicitly pause the Session or erase queued input. Approval allows
one call or denies it; native Session policy changes use `/approval`.

## Verification

Pure editor/layout/Markdown/terminal-input tests remain independent of Host.
Reducer integration tests instantiate real Host and SDK objects, with only the
model/resource boundary replaced by an explicit controllable double. Native
state projection tests deliberately reorder native observations to exercise
identity and revision rules. Production resource integration uses both HTTP
provider protocols and checks durable history after reopening. UI snapshots
prove presentation only; they are not execution evidence.

All build/check/test/documentation commands use `--locked`. CI runs the native
platform paths on Linux, macOS and Windows and builds four distribution targets.
See [AGENTS.md](../AGENTS.md) for the required commands and code rules.
