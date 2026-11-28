# Architecture

The executable owns one persistent MiniCore Host and one terminal application.
It links Host 0.12.11 at the immutable revision in `Cargo.toml`; all native
components come from that revision. The frontend calls `HostClient::in_process`
and the typed `SessionClient` APIs. There is no frontend protocol model, child
Agent, transport adapter, Webpage service or remote view layer.

## Module boundaries

| Module | Responsibility |
|---|---|
| `main.rs` | Startup, selection, terminal ownership, frame scheduling, signals, graceful Host shutdown |
| `host.rs`, `host/diagnostics.rs` | Directory Host startup, production resources, process control, diagnostics lifetime |
| `config/`, `cli.rs` | Shared configuration paths, local JSON preferences and command-line inputs |
| `app/mod.rs` | Terminal reducer and immutable messages from asynchronous work |
| `app/tasks.rs` | Typed Host reads/mutations, bounded UI delivery, subscription pumps and fixed-window catch-up |
| `app/resident.rs`, `app/live.rs` | Full Session addresses, local epochs, per-Session drafts and foreground navigation |
| `app/session/`, `app/control.rs` | Native snapshot projection, independent revisions, interactions, canonical history and output ranges |
| `app/agent_cards.rs`, `transcript/agent_card.rs`, `ui/agent_cards.rs` | Native delegation observation, bounded child previews and original-startup card rendering |
| `app/actions.rs`, `app/ops.rs` | Captured user intents and results installed into the original target |
| `app/unconfirmed.rs` | Bounded local ownership of sends with pending or unknown acceptance |
| `app/composer.rs`, `app/keys/`, `app/mouse.rs` | Editing, paste, key dispatch, selection and scrolling |
| `app/paste.rs`, `clipboard.rs` | Explicit native clipboard reads, temporary screenshot PNGs and original-editor delivery |
| `app/files.rs` | On-demand `workspace.list` browsing and quoted path references; no local filesystem traversal |
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
editor drafts, user-requested exports and explicit clipboard screenshots saved
as system-temporary PNGs. Screenshot/file paste inserts paths, not attachments.

The `@` browser calls Host `workspace.list` with `include_files` and the active
`workspace_session` and pages only the selected directory. The Host validates
parent/entry navigation and absolute path lookups. A local operation ID prevents
stale replies from replacing a reopened picker. Selection inserts a text
reference without reading file contents, changing the workspace or sending a
prompt.

## Startup and lifetime

Configuration preparation occurs before entering fullscreen. The shared
`host-config` library resolves `--config-dir` and `--data-root` and generates
initial Host/Role/Agent JSON. The TUI adds and reads `tui.json`, then passes the
same resolved directory to `Host::open_directory_configured`. Host alone parses
business files and retains the exclusive data-root lease for its lifetime.

Host reconciles file declarations at startup and explicit directory reload.
Stable declaration keys preserve Agent IDs and default Sessions across restart,
renaming and reintroduction. Bootstrap only reads native catalogs. Explicit
`/agent-new` reads the selected Agent's native configuration and copies it into
a dynamic Agent without source metadata, Session overrides or history. There
is no global frontend Agent template or fallback Agent creation.

`/reload` carries an operation ID and expected version, with no candidate or
path. Full and partial application refresh actual catalogs; partial results
remain visible and are never automatically retried. Templates reload locally;
terminal preferences do not participate in Host configuration publication.
Startup, partial reload and shutdown errors include Host's message as well as
its fault identity. Text printed after terminal restoration is sanitized.

Selection uses full Agent and Session identities and actual `last_activity_at`
values. The current directory overrides newly created Sessions, not an opened
Session's workspace. The shared starter records the initialization workspace
for its default Agent. Startup model/reasoning overrides are consumed once.
Later navigation reads the selected Session's saved settings.

The native Agent catalog owns Session summaries. The picker keeps its ordered
Session identities and reads the corresponding summaries from that catalog;
resident execution state is separate. Titles use a nonblank explicit title, the
bounded first-user-input preview, or the six-character native Session ID. The
preview is separate from rename data and is maintained by Host through its
existing metadata checkpoints. Old records acquire it during normal history
loading, not a picker-triggered per-Session scan. The ID leads the detail column
so long clipped labels remain distinguishable. Search covers both title and
preview, plus Session IDs. Opening history never promotes a preview into a title.

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
view with an older read result. Resident compaction state changes refresh history
and context; a native terminal failure also emits one original-target notice with
the Host fault. Snapshot identity/revision merging and the previous compaction
comparison suppress replay without changing the reading anchor or treating
admission, successful completion, unchanged or cancellation as failure.

Opening a Session uses the subscription's initial Session state rather than a
second state query. The subscription remains active during history, input and
context reads; its buffered events and already observed Host state merge by
revision when the view is installed. Explicit state refresh and recovery remain
available independently.

Post-bootstrap catalog reads share one coalescing gate. Mutations and Agent
progress request a refresh; requests arriving during a read coalesce into its
successor. Routine Agent/Session refreshes retain the model catalog. Models are
read at bootstrap, after an applied Host reload, on Host resubscription and when
terminal control returns from remote access. These full refreshes use the same
gate, so an in-flight Agent-only read cannot discard a pending model refresh.
Automatic workspace status reads have a separate per-Session gate
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
Refreshing the Session view uses the same observation path. Replacing a subscription aborts and
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

Turn failures use native `Fault.message` and saved `TurnTerminalData.reason`;
the TUI neither parses provider bodies nor classifies provider failures. The
same message flows into display and export. Collapsed failure quotes wrap only
their visible prefix. Tool-result JSON head previews use bounded serialization
and count pretty-printed lines from the parsed structure without a
second complete formatted body. Native records remain the export source.

Transient notices retain two summaries. One separate last-operation-error text
supports `/error` after dismissal, with the captured Session identity when
available. The error detail overlay is also used when clicking a failed Turn;
it owns only text and scroll geometry. Rendering and copying sanitize terminal
control sequences without changing Host's stored message. There is no second
error history, backend error classifier or automatic replay.

Responses reasoning boundaries are emitted by Host's provider and retained in
native canonical text. Markdown rendering preserves those newlines as hard
breaks. The frontend does not infer boundaries from fragments or headings.

The main view and Tasks use the same transcript layout and visible-row
rendering. Expanded Working groups reuse incremental rendering at their
indented width. Rendering uses per-block revisions and layout caches. One
separator row belongs between visible blocks, except between two adjacent step
rows (folded tools and thinking-only responses), which stack. Zero-row entries
consume no height or separator, but retain canonical identities for late-output
suppression. A Turn's grouping (one group per run between user messages: head summary
row with the newest step, hidden members, open state) is presentation metadata
on the transcript, recomputed for the live Turn after each session event; changing it bumps the
affected blocks' revisions, so the cache needs no separate group key. History
blocks carry their Turn on the record source, so grouping also applies after
reopening and after an older page fills in a Turn. Likewise, `link_delegations` marks later
sends into a child and the result it delivers as drawn inside the Agent card
that started the delegation (matched by child Session and task message), and
copies the result and send count onto that card. Parent events, installed
history and newly attached cards update these links. Child output updates the
preview without invalidating the parent card. The
cache stores body rows independently of separators so changes in neighboring
visibility preserve unchanged bodies. Internal tool/code whitespace remains
content; anchors, hits and selection use the resulting visible geometry.
Anchors are rebased when earlier records are inserted, and folding opens downward. Tool
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

Delegated Agent cards stay on the original startup ToolCall. Host supplies the
parent/child identities, first task Message ID, actual accepted inputs and Turn
membership, and settled completion. The Host-wide subscription starts before
dispatch so fast children can be observed without opening them. Native output
ranges and history supply recent child previews; no child body store or second
execution lifecycle is introduced. A later send links to the original card,
while a later delegation into the same Session has a new task identity. Missing
retained history is shown explicitly. Child activity preserves the parent's
draft, focus and reading position. See [the card contract](design-host.md#streaming-delegated-agent-cards).

## Native meaning in the UI

The composer maps Alt/Ctrl+Enter to native `FollowUp` input. On Unix it also
recognizes Ctrl+Alt+J, crossterm's decoded form of legacy ESC LF. This mapping
follows focused-surface routing; overlays and pending interactions retain their
keys, and paste remains text. Ordinary Enter steers and plain Ctrl+J inserts a
newline. Terminal profiles must preserve modifiers; no timer or decoder tries
to reconstruct a modifier from plain CR/LF.

Local query paste belongs to the focused Find, Sessions, Models or Agents
overlay. Single-line normalization reuses terminal sanitization, and pasted and
typed text share the same query-change path for matching and selection reset.
Changing a Session filter clears pending deletion. Other editors and native
interaction answers retain their own paste ownership; a hidden conversation
draft is never edited by a query paste. Actual forward deletion uses the
composer's existing complete undo snapshot, including paste chips and selection;
an end-of-draft no-op preserves valid redo.

The model catalog and reasoning choices come directly from Host. A Session displays its native committed settings; an unopened draft shows only
explicit launch choices.
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
