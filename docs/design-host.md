# Embedded Host migration

## Scope

TUI 0.12.7 embeds MiniCore Host 0.12.6 at the immutable revision in
`Cargo.toml`. The frontend uses
`HostClient::in_process` and native Host DTOs. Host owns execution, storage,
configuration, tools and credentials. There is no Agent subprocess, replacement
wire protocol, generic backend trait or second business-state owner.

The terminal editor, paste handling, Markdown, tool cards, selection, search,
anchored scrolling, themes, external editor and background Sessions remain.
Webpage, mobile control, browser assets, HTTP/WebSocket listeners, phone
authentication and their configuration and tests are removed. A future WSS
client does not require these removed servers.

## Ownership and identity

- A Session address is the native `(agent_id, session_id)` pair. Navigation
  changes only the frontend selection. Creating another Session does not change
  its Agent's default Session.
- Host identity and incarnation, controller revision, load identity and runner
  revision keep their separate meanings. Async results capture their original
  Session and local epoch; they never follow subsequent navigation.
- State comes from Host snapshots and subscriptions. Mutation receipts report
  admission; they do not synthesize running, saved, closed or deleted state.
- Open/closing/closed, auto/paused and unloaded/loading/loaded/unloading are
  separate. A closed Session cannot be reopened. Loading does not clear pause.
- Agents and Sessions are discovered with all catalog pages. Recent selection
  uses `last_activity_at`, never lexical ID ordering.

## Inputs, tools and observations

- Enter sends steer; the existing follow-up key sends follow-up. Host owns the
  next-step and next-turn queues and their separate FIFO order. Input list reads
  supply queued text. Recall removes the original message and restores its text
  only after confirmed removal.
- Cancellation names the captured Turn or compaction operation. Call timeout
  and client disconnect do not cancel accepted work. Message and operation IDs
  are correlation IDs, not idempotency keys; uncertain inputs are never resent
  automatically.
- Subscriptions install their initial states and output positions. Output uses
  native UTF-8 byte ranges and SDK buffering; gaps are filled to a fixed observed
  end. ResyncRequired ends the old subscription and installs a new baseline.
- Tool cards begin with actual complete arguments. No argument-generation
  preview is implemented. Approval answers keep original interaction, Turn and
  load IDs. Session-wide grants are not fabricated by the frontend.
- Delegated Session and task-message associations are displayed as reported by
  Host. Finishing the parent tool is distinct from finishing the child task.

## Streaming delegated Agent cards

The original startup tool invocation owns one stable card. Its acceptance or
tool completion does not mean that the delegated work has completed. The card
continues to display the child's actual reasoning, text, tool activity and
terminal result after that tool returns. Host owns the delegation association,
its accepted inputs, actual Turn membership and settlement. The frontend does
not infer task completion from a tool receipt, Agent name or current Session
activity, and does not introduce a scheduler or Task lifecycle.

The association is addressed by the current Host incarnation, full child
Session address and first task Message ID. Follow-up inputs belonging to that
delegation continue its original card. A later delegation into the same Session
has a new first Message ID and a separate card; the prior card remains frozen.
Two Sessions under one Agent never share a card. Additional sends show a concise
receipt with a route to the original card. Wait, control, inspect and update
tools retain compact generic receipts; a wait is never assigned to one child.
Creation without an initial task remains an ordinary tool receipt.

Observation starts before dispatch and uses native subscriptions, output ranges
and positioned history. It does not open or select a child Session. A very short
memory child may finish before its association arrives, so bounded observation
preparation must retain its actual recent content. Recovery reads the native
history and respects Load identities, independent revisions and canonical
deduplication. Released memory history is explicitly unavailable; a retained
preview is never advertised as a reconstructed complete transcript.

Cards retain the existing rails, colors and local folding behavior. The header
identifies the Agent, Session and task. Native execution, waiting, approval,
completion and failure facts determine the status. The collapsed preview shows
about six visual rows of recent transcript; the expanded preview shows about
eighteen. Preview limits apply after wrapping, retain tool context labels and
preserve actual newlines and internal blank lines. Completed cards show the
final answer; failures retain partial output and the actual error. More content
is available through an explicit Open child Session action, or a truthful
unavailable-history message. Nested delegations may use a concise child link.

Clicking a card changes only its local expansion. Ctrl+O retains its existing
global semantics. Child events do not reset expansion, reorder startup cards,
move focus, alter the parent's draft, scroll anchor or selection, or redirect
Enter, Alt+Enter or Escape. Approvals are answered in the real child Session.
Opening that Session and returning preserves the parent's draft and anchor.
Rendering performs no I/O. Retained preview bodies, recent blocks and unattached
prepared cards have explicit limits. Native disjoint output gaps still require
range recovery; these limits do not claim constant memory for arbitrary missing
ranges. Unchanged bodies are reused and idle cards add no periodic timer.
Native structured tool results are decoded through the existing result types;
temporary JSON decoding memory can grow with the original result. The bounded
preview does not claim a constant-memory parser for arbitrarily large inputs.

Verification uses real in-process Host execution with explicit controlled
models/resources for output after tool return, interleaved children, Session
reuse, follow-up boundaries, short memory children, completion and failure.
Renderer assertions cover the visual limits, newline and tool-label fidelity,
local/global folding, selection and anchors. These fixtures are not evidence of
live provider or physical-terminal behavior.

## Modified Enter on macOS and Unix terminals

The 0.12.6 frontend repair keeps Host's existing input contract. In the
conversation composer, Alt+Enter and Ctrl+Enter request `FollowUp`; on Unix,
legacy ESC LF is decoded by crossterm as Ctrl+Alt+J and requests the same
action. This alias belongs only to the composer key handler, after the existing
focused-surface routing. It does not replace the terminal decoder, create a
local input queue or add a key-sequence timeout. Windows retains its existing
Ctrl+Alt+J editing behavior and paste decoder.

Ordinary Enter continues to request `Steer`. Plain Ctrl+J and Shift+Enter
continue to insert newlines. An empty draft sends no message. Picker/view,
completion, unconfirmed-input and pending-interaction ownership remains intact;
paste is text and cannot trigger a send. When the composer requests a follow-up,
the existing native Host path handles both idle Sessions and next-turn queuing
while a Turn runs. It preserves the original Session and message identity.

Restore terminal-specific Option-as-Meta guidance from the Agent-era frontend
and describe the actual boundary: ESC CR and reported modified Enter already
work, and ESC LF receives the composer alias above. If a terminal sends the
same plain CR or LF as an unmodified key, the frontend cannot recover the lost
modifier. Configure the terminal profile to preserve it instead of changing
ordinary Enter/Ctrl+J semantics or restoring the superseded Tab-send binding.

Acceptance covers the actual crossterm decoding of CR, LF, ESC CR, ESC LF,
modified Enter encodings and bracketed paste, plus decoded key routing into
native `FollowUp` admission while idle and next-turn queuing while working.
Confirm distinct Turn boundaries, empty drafts, newline/editing keys, focused
overlays and pending interactions. Record byte/PTY probes separately from
physical macOS terminal observations; platform CI alone does not prove the
user's terminal profile emits a particular sequence.

## Local editing and Session picker identity

SPEC 4.11.3 assigns these three repairs to the frontend. They add no Host
fields, lifecycle or storage behavior; Host -> Agent -> SessionController
remains the execution ownership chain.

- Terminal paste appends to the active Find query or Sessions, Models or Agents
  filter. The resulting query uses the same refresh path as typed text: picker
  selection resets to the first result, and Find recomputes matches and shows
  the first match. A changed Sessions filter clears pending delete confirmation.
- These query fields are single-line. Each CRLF pair, lone CR or LF becomes one
  space; TAB becomes four spaces. Apply existing terminal sanitization: strip
  ANSI/OSC sequences and BOMs, render other control and bidi-control characters
  with the existing visible escapes, and preserve ordinary Unicode. Paste whose
  normalized text is empty leaves the query, selection and confirmation intact.
  Equivalent typed printable text has the same query and selection result.
- Query paste never accepts a row, runs a command or changes the underlying
  draft. Escape returns to that draft. Existing composer, Palette, Files,
  unconfirmed-input and native interaction-answer paste routes retain their
  ownership and guards; read-only sheets stay read-only. Session rename editing
  is outside this repair.
- Physical Delete and nonempty Ctrl+D make each actual forward deletion a
  separate undo step, invalidating an abandoned redo branch. Undo and redo
  restore text, graphemes, complete paste chips, cursor and selection together.
  Selected deletion remains one step. A forward delete with nothing to remove
  creates no edit and preserves valid redo; empty Ctrl+D keeps its quit meaning.
- Every Sessions picker row displays the existing six-character native
  Session-ID prefix, including unique, duplicate, automatic and fallback names.
  Reserve room for this stable short ID before clipping the title and optional
  details. Filtering, running state, ordering, renaming and reopening the picker
  do not change it. Full `(agent_id, session_id)` identities continue to target
  actions; short IDs are presentation only and need no collision registry.

Acceptance covers all four paste surfaces with existing text, Unicode,
line endings, controls, selection changes, delete confirmation and draft
preservation. Both forward-delete keys must replay `abc` -> `bc` -> Undo `abc`
-> Redo `bc`, including earlier edits, preloaded drafts, redo abandonment,
no-op deletion, graphemes and paste chips. Frame assertions must show the short
IDs when distinct long titles clip alike under the same Agent while idle or
running, as well as narrow layouts, exact duplicate names and renamed rows.
Retain full target and stored-title checks. Record reducer/editor/rendering
results separately from any native-terminal verification; old failure evidence
is not proof that the repaired build passed.

## Reasoning boundaries and visible transcript spacing

SPEC 4.11.4 keeps provider reasoning boundaries in Host and transcript geometry
in the TUI. Responses emits a single LF between distinct nonempty reasoning
parts/items when neither raw side already supplies CR/LF. Chunks within one
part remain contiguous; empty parts and repeated completion snapshots add no
line. The provider preserves raw replay items while checking the actual emitted
reasoning text. The TUI keeps those native newlines as Markdown hard breaks;
streaming and completed/history rendering agree.

Exactly one layout separator belongs between adjacent visible blocks. A block
that renders no rows occupies no row and adds no separator. Empty Assistant
projections for tool-only responses keep their native positions and identities
for canonical-response and late-output checks. A hidden-thinking hint is still
visible content. Preserve meaningful blank lines inside tool output, code and
prose; do not globally collapse whitespace.

Folding, reasoning visibility, resizing, incremental updates and history reload
must keep separators, scroll anchors, hit geometry, search and selection aligned
with actual visible rows. Reuse unchanged body caches. Regression coverage uses
real native history and production-provider fixtures as well as frame assertions;
record any native-terminal observations separately from those fixtures.

## History and configuration

- Native history positions order user, assistant, tool-result, partial,
  summary, compaction and terminal records. History pagination and export use a
  fixed upper bound and native cursors. Repeated observation of one position is
  not duplicate content; repeated text at different positions remains distinct.
- Reported usage is labelled with its actual coverage. Missing usage is unknown,
  not zero. A recent response's token count is not current prompt occupancy or a
  whole-Session total.
- UI preferences remain local in `tui.json`. The shared `--config-dir` is
  passed to Host, which reads `host.json`, Role files and Agent declarations at
  startup and explicit reload. Stable declaration keys preserve identity;
  existing Sessions retain their configuration. Dynamic Agent creation copies
  the selected Agent's native configuration without source management metadata.
- Reload requests contain no candidate or path. Host applies declared changes
  through actual owners and reports full or partial results. The TUI refreshes
  actual state after either outcome and never retries partial work implicitly.
- Session configuration changes bind to later Turns. Workspace queries use the
  addressed Host Session. TUI does not read the backend store or run Git/tools
  to replace missing Host APIs.
- Terminal exit requests controlled Host shutdown and waits for actual cleanup.
  There is no timer that silently upgrades graceful shutdown to force.

## Verification and delivery

The frontend keeps these ownership contracts while removing unused
conversion paths and duplicate input bodies. Catalog reads have one coalescing
owner; workspace status is queried independently of scheduling controls.
History installation indexes native identities within each batch and leaves
unchanged blocks and ordering alone. Tool display limits apply before copying
output; complete native arguments and exported history remain available.

Use real embedded Host tests with explicitly named loopback model fixtures for
execution, streaming, tools, approvals, history and shutdown. Preserve terminal
and editor regression coverage; replace Agent-wire and phone-only tests with
tests for actual Host behavior. Run formatting, lint, tests, documentation and
the native-platform CI gates. Record any unavailable gate accurately. Publish
reviewed commits to `dev` without rewriting history or adding co-author trailers.
