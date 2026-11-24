# Changelog

## Unreleased

- Redesign the conversation layout without changing the data model; the rules
  are written down in `docs/ui-spec.md`.
- Show each folded tool as one row: state glyph (`●` `✓` `✗` `⊘`) in the
  rail column, real tool name in the content column, subject and right-aligned
  facts, so an opened card keeps the same left edge. A running tool shows its
  last three output lines; a failure says `failed` and keeps its reason in the
  opened card. Step rows stack without blank rows.
- Keep a delegation's whole life in one Agent card: one row folded (with the
  child's newest steps while it runs), and opened, the task, facts, activity,
  the delivered result and the way into the child. Later sends into the child
  and its result no longer add their own cards.
- Fold a finished Turn's successful steps under
  `▸ Worked 42s · 14 tools · 2 files changed · 1 failed`; failures, Agent
  cards and answers stay visible. Click to open; Ctrl+O opens all. Grouping
  also applies to reopened history.
- Fold thinking to one `∴` row with its newest heading; click it to open.
- Replace the header with a welcome card, and the dock with one activity line
  that appears only with content: status and whole minutes on the left, only
  the keys that work now on the right (`N queued · alt+↑ edit`, `N delegated · ctrl+b`). The footer shows
  workspace, branch and Session title on the left and model, reasoning and
  approval on the right; response usage moved to `/context` and `/session`.
- Settle spacing on one grid and rhythm: one blank row between blocks, none
  between steps, one gap row above the editor; compaction and summary markers
  are one divider row; Markdown tables rule only under the header unless a row
  wraps.
- Add Ctrl+B and `/children`: the conversation's delegations, newest first;
  Enter opens the child Session.
- Give sheets one frame (title and count in the top rule, keys in the bottom
  rule, `→` pointer, `✓` current), show Context and Session as full-screen
  views, preview the selected Session, and let F1 help filter as you type.
- Quiet the palette to two colour signals (user accent and failure) with one
  neutral rail grey in both themes.

## 0.12.11 — 2026-11-23

- Embed Host 0.12.11 at immutable revision
  `6badd94e91a8e1ebf6aa04d67dd6e34898ad79cb` (SPEC 4.11.6): transient model
  failures retry while nothing was shown, mid-turn compaction keeps the active
  request, tool failures state their reason and local failures are `failed`,
  bash output reaching the model is bounded with an omission count, and the
  agent tool defaults its targets and inherits the caller's settings.
- Keep `--workspace`/the startup directory for new Sessions; do not open a
  default Session from another workspace at startup.
- Show user messages with their own line breaks and fold long ones; break CJK
  text between characters without stranding punctuation.
- Render delegated results as cards with outcome and final answer; title Agent
  cards by their task with progress and one line per tool; add `/child` and
  Esc-to-parent navigation, and `/cancel`.
- Show Turn duration and file changes on the end line; show saving only when it
  has not happened. Keep one title row for folded and expanded tool cards, count
  real edit replacements, read failures and directory listings as text and mark
  stderr.
- Unify pickers (pointer, current mark, Esc wording, empty states), group the
  command palette with current values, give enumerated commands their choices,
  check arguments before sending while keeping the draft, confirm `/close` and
  Session deletion with `y`, and say plainly why `/compact` waits.
- Make an Esc-cleared draft restorable with Ctrl+Z; render headings, code blocks
  and task lists without Markdown syntax; draw the jump-to-latest row as a bar;
  follow the terminal background (`COLORFGBG`) when no theme is configured.

## 0.12.10 — 2026-11-19

- Keep untitled Sessions under their short ID in `/sessions`, even after
  opening their history. Only a saved title replaces the ID.

## 0.12.9 — 2026-11-19

- Embed Host 0.12.9 at immutable revision
  `e05bb4b890f79ccf51ed1f5f76dc6f2e46e74ecc`. The Responses adapter accepts
  advisory `response.metadata` for the active response without bypassing final
  completion validation. Session pause and resume behavior is unchanged.

## 0.12.8 — 2026-11-18

- Align the TUI and embedded Host package versions to 0.12.8 without changing
  runtime behavior, native formats or the external dependency graph.
- Pin Host 0.12.8 to immutable revision
  `2a88d4771bf7a12c14f6290dbe0b6ce265092886`; update current version documentation
  and the four version-bearing terminal snapshots.

## 0.12.7 — 2026-11-18

- Keep delegated child conversations in the original startup Agent card after
  the tool returns. Show native reasoning, responses, tool activity, partial
  failures and settled completion in bounded recent previews.
- Link later sends to the original card; give a new delegation into the same
  Session its own card. Opening a child preserves the parent's draft and view.
  Released child history is explicitly unavailable, not reconstructed.
- Preserve native identities, original result bodies, keyboard ownership,
  local/global folding, selection and transcript spacing. Read native history
  metadata without restoring child execution or adding a frontend scheduler.
- Embed Host 0.12.6 at immutable revision
  `02c7558b7eff324f2f83557305d921912a4dce74`, following SPEC 4.11.5.
  Keep the existing external dependency graph.

## 0.12.6 — 2026-11-15

- Accept the Unix legacy ESC LF form of Option/Alt+Return as a follow-up in the
  composer. Crossterm reports it as Ctrl+Alt+J; plain Ctrl+J remains a newline.
  Use the existing Host admission and next-turn queue while idle or working.
- Restore macOS terminal profile guidance for Terminal.app, iTerm2, Ghostty and
  kitty, including an iTerm2 mapping limited to Option+Return. Explain that the
  terminal must preserve the modifier and surface this setup in F1 help.
- Retain focused picker and interaction ownership, paste text, ordinary Enter,
  empty-draft behavior and Windows key handling. Keep the immutable Host 0.12.5
  dependency and existing external dependency graph.

## 0.12.5 — 2026-11-15

- Keep real Responses reasoning parts and items on separate logical lines
  during streaming, canonical display and history reopen. Token fragments
  within a part still concatenate, and existing line breaks are preserved.
- Use one separator row between visible transcript blocks. Empty Assistant
  projections from tool-only responses consume no rows while retaining their
  native identities for late-output suppression. Keep blank lines inside tool
  output and code, including after folding, resizing and history reload.
- Preserve unchanged body caches and align anchors, hit testing and selection
  with actual visible rows; keep partial-response markers at narrow widths.
- Embed Host 0.12.5 at immutable revision
  `355d4b8bac4a3a634cd83a49fbf33fea7b67a262`, following SPEC 4.11.4.
  The provider retains original reasoning replay items and raw validation while
  including emitted separators in canonical reasoning checksums. Storage formats
  and the external dependency graph remain unchanged.

## 0.12.4 — 2026-11-15

- Accept terminal paste in Find and the Sessions, Models and Agents filters.
  Append normalized single-line text, refresh matches and selection, and
  preserve the conversation draft. A changed Session filter clears pending
  deletion confirmation.
- Make forward Delete and nonempty Ctrl+D separate undoable edits, including
  graphemes, selections and complete paste chips. Real deletion clears an
  abandoned redo branch; deleting at the end preserves valid redo.
- Show the existing stable short Session ID on every picker row and reserve
  it before clipping long titles and optional details, including while Sessions
  run and after renaming. Keep full native targets and stored titles unchanged.
- Embed Host 0.12.4 at immutable revision
  `667f0923b827fea9b5fdb60f75a21f5369e44193`, following SPEC 4.11.3.
  This Host release updates the specification and version metadata without
  adding backend execution or storage behavior.

## 0.12.3 — 2026-11-15

- Embed Host 0.12.3 at immutable revision
  `dbaaa8175780759b84d26e0519aa4b27ced5f8f4`, following SPEC 4.11.2.
  Preserve its safe upstream HTTP and stream error messages
  through live failure details, reopened native history and complete Markdown
  export. Keep Host's fault cause in startup, partial reload and shutdown errors.
- Add `/error` to recall and copy the last operation error after its notice is
  dismissed, retaining its original Session identity. Clicking a failed Turn
  opens its complete details; `Ctrl+O` and F1 help explicitly include failures.
- Wrap only the visible prefix of collapsed failures. Bound JSON result head preview
  serialization before allocating discarded output, retain accurate complete
  line counts and point truncated previews to `/export`.
- Verify HTTP and SSE error propagation with a loopback provider and real
  in-process Host, including frontend reopen, pagination, display and export.

## 0.12.2 — 2026-11-15

- Embed Host 0.12.2 at immutable revision
  `2f149145304c7cf6fd3271da937000b6e8593cbd`, with SPEC 4.11.1
  configuration references and flat schema-3 storage. File-managed defaults require their declarations; no snapshot recovery
  or old-data migration is performed. Session configuration stays independent.
- Share independent native-home config/data defaults, read `tui.json` locally,
  and use `config/commands/` without searching the old template path.
- Initialize missing example files when `host.json` is absent, including when
  empty Agent/Role directories already exist; never overwrite user files.
- Protect flat Agent metadata and Session history from diagnostic log output.
  Preserve the existing terminal layout, rendering and interaction behavior.

## 0.12.1 — 2026-11-14

- Embed Host 0.12.1 at immutable revision
  `c7bd16b4e36c94f587aa4802b3298617dc088478`, retaining shared JSON configuration
  and the Host's local fixes.
- Stop terminal event reading during Ctrl+G external editing and restore the
  fullscreen view without a cursor-position query. Retain the edited file and
  report its path if terminal restoration fails.
- Refresh the original resident Session's metadata after rename so catalogs and
  details agree, including when navigation precedes completion.
- Reject invalid explicit `/reasoning` levels with supported values and preserve
  the current setting; only the argument-free command opens the picker.
- Report asynchronous native compaction failures once in the conversation notice,
  including the Host fault and the originating background Session. Preserve the
  reading position; acceptance, success, unchanged and cancellation are not failures.

## 0.12.0 — 2026-11-14

- Embed Host 0.12.0 at immutable revision
  `2f5344d2e8cfb6e49d11cf1b188381264f8ee7fa`, following SPEC 4.11.0.

- Use `--config-dir` for shared JSON configuration and `--data-root` for runtime
  storage. Read local preferences from `tui.json`; store accepted input recall
  in `<data-root>/tui/history.jsonl`. Remove old path flags, TOML and the global
  Agent-defaults file and command.
- Let Host load `host.json`, reusable Roles and keyed Agent declarations at
  startup and directory reload. Preserve native Agent identities and existing
  Session settings; show actual partial reload results without automatic retry.
- Create dynamic Agents by copying the selected Agent's native Role, instructions
  and Session defaults without file management metadata or Session overrides.
- Reuse Host's shared JSON starter generator and retain terminal interaction,
  Session controls and the production resource/diagnostic startup boundary.

## 0.11.0 — 2026-11-14

- Index native history positions and identities per installation batch; preserve
  unchanged block revisions and avoid sorting unchanged or already ordered history.
- Bound tool output allocation before copying, borrow tool input from native
  arguments, and avoid duplicate argument installation and result formatting.
- Use one coalescing catalog reader for actions and observed progress. Separate
  workspace status reads from unrelated controls and skip unconfigured workspaces.
- Count background Sessions without cross-product scans or copied address strings.
  Keep a new Session's first input only in its original creation owner.
- Preserve drafts and recovery files when external-editor output cannot be read.
  Defer the default Host configuration candidate on reopen until explicit reload.
- Remove unused conversion APIs, write-only state and an unused direct dependency.
  Preserve embedded Host 0.11.0, terminal behavior and native recovery contracts.

## 0.10.0 — 2026-11-13

- Embed Host 0.11.0 at immutable revision 32f074c57868533af81b0c6491440bae24b872c8
  and use its typed in-process Rust client. Remove the old Agent subcommand,
  child process, stdio RPC client and frontend protocol DTOs.
- Remove Webpage/phone operation, listeners, authentication, TLS/pairing state,
  browser assets and remote settings from the TUI.
- Split terminal preferences, native Host configuration and new-Agent defaults.
  Preserve saved native configuration; old Agent stores are not migrated.
- Add Agent selection/creation/start/stop/deletion and default management, native
  Session lifecycle/residency/scheduling actions, and revision-checked settings.
- Bind sends, cancellation, interaction answers and asynchronous results to their
  original native identities. Retain uncertain acceptance for explicit F8 review.
- Use native observation, fixed byte-range output recovery, canonical history
  positions, fixed pagination windows and complete streaming Markdown export.
- Display complete tool arguments, native progress/delegations, actual retry and
  persistence state, native context facts and paged workspace diffs.
- Preserve the terminal editor, Markdown, selection, scrolling, folding, cached
  rendering and platform-specific input handling. Event delivery is bounded and
  idle views do not tick.
- Restore the terminal before supervising real graceful Host shutdown. Keep
  blocked cleanup visible; force shutdown requires explicit user action.
- Replace old RPC harnesses with real in-process Host regression tests and local
  Chat Completions/Responses provider integration, including persistence/reopen.
  Update Linux/macOS/Windows checks and all four package manifests.

## 0.9.9 — 2026-11-02

- Bundle Agent 0.9.5, preserving one execution owner while a turn finishes and
  a queued successor starts. The Agent RPC and phone protocols are unchanged.
- Keep foreground history loading and export responsive while completion or
  history recovery is stalled, without increasing the shared read-memory budget.
- Share recovery identity capacity across waiting sessions without losing retry
  counts or already-settled results. Capacity warnings clear after
  retained work is admitted again; warnings for missing content remain visible.
  Deleting a resident session also wakes waiting recovery work.
- Keep in-flight result reads when their catalog entries expire or a history
  watermark advances, so verified completed output is still handled.
- Explain completed bodies that cannot be placed: retained entries show a
  persistent retry hint after bounded retries; expired entries report the loss
  without promising that `/recover` can read them again.
- Remove unused pre-install bookkeeping and a test-only session accessor;
  installing a load still refreshes its control snapshot unconditionally.

## 0.9.8 — 2026-11-02

- Adopt Agent 0.9.4 and require Agent RPC v4, including the `workspace.status`
  and `tool.output` capabilities, before session operations. The phone WebSocket
  protocol stays v3.
- Read the workspace branch directly and refresh context independently, keeping
  periodic context updates free of Git queries. Terminal and phone share the
  same per-session branch observation.
- Preserve literal `[truncated]` lines in tool output and transcript search.
  Truncation uses structured metadata; old history without a distinct marker
  identity keeps that line as text.
- Remove unused wire fields, the obsolete `--config` migration diagnostic and
  an unused standalone control probe. Terminal and phone layouts, input behavior,
  control snapshots and result recovery are unchanged.

## 0.9.7 — 2026-11-02

- Adopt Agent 0.9.3 and require Agent RPC v3 before starting session operations.
  Control snapshots own execution, queues and interactions; input receipts and
  lightweight turn results no longer carry duplicate control state.
- Pass structured tool and workspace change references through terminal and
  phone diff queries, preserving their revision and workspace identity.
- Preserve terminal and phone layouts, input behavior, history recovery and
  completed-result reconciliation while removing the obsolete control events.
- Start acceptance-test PTYs without a threaded pre-exec callback, reap owned
  leaders before group checks, and require actual disappearance of macOS zombie
  groups. Failed helpers retain their full diagnostics within the existing bounds.
- Keep loopback test providers independent of reverse DNS, and drain PTY output
  through process exit so terminal tails cannot block macOS fixture cleanup.
- Include revision-pinned backends in packaged `BUILD.txt` manifests.

## 0.9.6 — 2026-11-01

- Reduce web Markdown parsing work for ordinary text, code fences and streaming
  checkpoints while preserving the rendered content.
- Reuse answer DOM during reasoning and recovery-label updates, avoid rebuilding
  already-visible search matches, and compute fold/group signatures once per group.
- Share prepared state and body annotations across connections viewing the
  same session, preserving independent snapshots and recovery updates.

## 0.9.5 — 2026-11-01

- Reduce per-delta work in long conversations and repeated scans when early
  body events become known, preserving their turn boundaries and cache limits.
- Reduce allocations when synchronizing retained conversation content to phones.
- Render only the visible composer rows and avoid invalidating unchanged search
  matches, preserving selection, cursor and expansion behavior.
- Reuse parsed streaming Markdown across fold changes and reduce wrapping work
  for long paragraphs with many styled spans.

- Share local and phone request preparation while keeping their original drafts,
  acknowledgements and focus behavior. Pending phone operations retain an explicit
  draft, stored-session or resident target through delayed replies.
- Simplify recovery errors, retry state and read metadata, and group session body,
  completion and history handling by responsibility. Existing control snapshots,
  body recovery limits and terminal/web interactions remain unchanged.

- Background work and delayed replies target their original resident session
  without changing the selected view or another session's draft and scroll.
- Local and phone mutations share typed execution and receipt validation. A
  session-wide tool approval takes effect only after the Agent accepts it.
- Renaming and deleting idle background sessions use the correct instance and
  storage request fields.
- Malformed control snapshots and stdin failures close the RPC connection visibly,
  even under event pressure; cancelled calls release their pending waiters.
- Completed result reads preserve the Agent's request and usage totals, including
  turns whose wait response was missed, without counting a successor's usage twice.
- Accepted prompt history and theme writes run serially off the event loop and
  flush on orderly exit. Reload composition also runs outside the reducer.

- Overlapping history recovery and pagination keep each source row once,
  including result-only pages and the compaction summary's shared item position.
  Rebuilt history retains scroll anchors, live output and canonical tool details.
- A delayed predecessor wait settles and counts only its own tools; late body
  events no longer change the successor's working line or trim its output.
- Local inputs whose acceptance is unknown remain separately reviewable with F8
  or `/unconfirmed`, without replacing a new draft or entering ordinary prompt
  history. Editing keeps that uncertainty; resending warns about possible
  duplication and uses a new operation against the original target.
- Late steering stays at its Agent-reported request boundary before the original
  turn's terminal and successor, including identical-text inputs and incomplete
  turns. Inputs arriving after canonical installation cannot resurrect that body.
- Quitting waits for the Agent child to finish after its shutdown acknowledgement
  instead of interrupting its final cleanup; an unresponsive child is still
  terminated within a bounded deadline.
- Completion recovery shares its two read slots fairly across resident sessions.
  A recovered saved tail keeps an explicit warning for unknown missing unsaved
  turns, and late tool-record save facts update already-restored cards.
- Persistence blocking stays visible while idle and alongside an actionable
  interaction. A missing or oversized completed body keeps its turn identity,
  known outcome and saving status instead of disappearing with a notice.
- Bounded history catch-up keeps its verified newer window and `/recover`
  continues the actual missing range. Older pagination cannot hide a missing
  newer suffix; saved-history anchors also preserve order when result reads
  finish out of order, and delayed waits preserve successor token usage.
- Tool text-input questions use a separate answer editor without consuming the
  conversation draft. Answers keep their exact interaction target, and a newer
  prompt cannot be replaced by a delayed old reply. An answered tool question
  finishes as done in live output, history and restored tool details.
- Phone protocol v3 targets operations by session/load and stable input or turn
  identity. Input ACKs wait for the Agent result; unknown acceptance stays
  unconfirmed, and edit text returns only to the originating operation.
- Unknown-turn body and tool events wait in a bounded cache until the Agent
  confirms their identity. Recovery keeps newer live output, unsaved turns and
  drafts, and `/recover` retries an exhausted read without resubmitting input.
- Control state — what is running, the queue, the paused flag, a waiting
  interaction, compaction and a blocked session — comes from one authoritative
  snapshot the Agent pushes (or from `session.state`), applied whole. A turn's
  body, outcome and saved state are still confirmed by `turn.wait` and
  `turn.result`. A missed turn is discovered from the completion catalogue and
  read back, so a session keeps its history even when events were dropped.
- A turn's canonical body is installed once, from a result identity checked on
  every page; a turn the client never streamed is placed where the catalogue says
  it belongs. Its turn-end line says when the Agent could not save it.
- A snapshot arriving during its session's pending open is held (at most two,
  128 MiB total) and installed by that open; overflow forces a fresh
  `session.state` rather than leaving control empty.
- The client admits the Agent's 64 MiB control frame and may send a request up
  to the Agent's 8 MiB ceiling, so a maximum 256 KiB input can actually be
  submitted instead of being refused. No input is truncated to fit a limit.
- History and turn reads keep the existing transcript and report an error or an
  oversized item instead of installing an incomplete or invalid result.

## 0.9.0 — 2026-10-30

- On Windows the welcome header and the remote status screen write the home folder
  as `~`, as on other systems.
- The sessions picker marks a background session whose last turn failed and that
  you have not opened since as `failed` (in the failure colour) instead of `new`.
- An approval or choice prompt takes its answer keys (`y`, `a`, `n`, `Enter`) only
  after half a second, and any other key pressed meanwhile restarts the wait, so
  text you were typing when it appeared cannot answer it. `Esc` and `↑` / `↓` act
  at once.
- While an approval waits, `PageUp`, `PageDown`, `Ctrl+↑` / `Ctrl+↓`, `End`,
  `Ctrl+O` and `Ctrl+T` still scroll and fold the transcript.
- A provider's error code or type can no longer reorder or restyle the failed
  turn's line in the transcript.
- With `--agent-config`, an empty `config.toml` is an error again instead of being
  ignored; a missing one still means defaults.
- The sessions picker finds an untitled background session by a word of its first
  prompt and Ctrl+D names it that way, and the filter also matches the
  workspace folder name.
- The bell no longer rings when you leave a session whose turn is still running,
  and it rings for a long turn's approval even while another session waits for
  one.
- Web client: pin sessions to the top of the session list (a collapsible Pinned
  group, "Pin to top" in a session's ⋯ menu; pins stay on the device), and
  sessions that finished a turn in the last 30 minutes get a green dot and a
  stronger time. A session that just ran now leads its workspace group. The
  open list holds its order, except that a session that starts waiting for
  approval moves to Needs you at once, so a folded group never hides it.
- A session's time in the session picker and on the phone is its last activity:
  it advances when a turn finishes, in the foreground or the background, instead
  of staying at the time the session was opened.
- Web client: the conversation is denser and follows one rhythm. Tool and
  thinking rows are 32 px (36 px on touch) with 2 px between them, 12 px
  separates text, tools and steering inside a turn, and 24 px separates turns.
- Web client: a round ↓ button above the composer jumps to the latest message
  whenever you are scrolled well above the bottom, and becomes the "New output"
  pill when output arrived meanwhile. `End` does the same outside text fields.
- A message is never held back by a compaction that follows the previous turn: the
  Agent compacts only before a turn starts, so the "Waiting for compaction"
  state is gone and a refused message returns to the composer.
- The context view and the phone's context sheet list the last compaction without
  an "(automatic)" or "(manual)" origin, and the turn-start one reads "Last
  automatic compaction"; a turn-start compaction that failed is reported once.
- Web client: when the next approval replaces an open one, its sheet starts
  fresh instead of keeping the deny form and reason typed for the previous one.
- iPhone: tapping the composer no longer zooms the page in and leaves it
  zoomed; the composer sits on the keyboard and returns to the bottom when the
  keyboard closes, and a pinch zoom no longer resizes the app.
- iPhone: the session drawer reaches under the status bar instead of leaving a
  grey strip above it.
- The Agent now keeps each session's queue of messages and the TUI only shows
  it (the new Agent input queue API, `input.push` and friends, replaces
  `turn.send` and `turn.steer`). `Enter` sends a steer, which reaches the running
  turn at its next request or starts a turn when idle; `Alt+Enter` (or
  `Ctrl+Enter`) queues a follow-up that starts a turn of its own once the running
  one has ended. Steers run before follow-ups, and the dock lists the messages in
  that order. A steer leaves the list and appears in the transcript, in order,
  when the running turn takes it, and a turn the Agent starts from the queue gets
  its own wait in the foreground and in background sessions alike.
- An interrupted or failed turn pauses a queue that still holds messages: the
  dock says `paused · enter resumes · alt+↑ edit`, and `Enter` on an empty draft
  resumes it (a draft sends and resumes it, older messages first). `Alt+↑` takes
  every queued message back in the TUI, the web tray offers Edit and Delete on
  every row until the running turn takes it, and a session with queued messages
  cannot be deleted.
- The phone's queue rows no longer carry a `sent` marker, and the `[loop]` key
  `max_pending_steers` is gone from `config.toml`: the Agent no longer has a steer
  limit of its own, only a bound of 32 queued messages per session.

- Confirming an IME candidate with Enter no longer submits the message when
  Safari reports the key outside composition with key code 229.
- Finished work folds separately between steering messages, preserving the
  order of instructions and tool calls with per-segment counts and durations.
- Settings switches use the full row width, with readable descriptions and
  right-aligned controls on phones and desktops.

- Built-in backend: Agent 0.8.2 with Runtime 0.8.0, pinned by tag (0.8.0 before).
  A failed turn now carries what the provider said: its HTTP status, error code
  and type, and the full message. Automatic compaction is decided only before a
  turn starts, from the context the provider last reported, so restarting no
  longer compacts a session early; nothing is compacted automatically when a
  turn ends.
- Workspace-grouped session lists use denser single-line rows with more room
  for titles; desktop row actions replace the time on hover or keyboard focus.
- The composer uses a lighter, compact toolbar: transparent icon and context
  buttons, smaller model text and primary button, with larger touch hit areas.
- Web client: the status strip above the composer follows the running tool.
  The composer toolbar gains an Approvals shield (the tools allowed for the
  session) and the context share, and its
  placeholder says what typing does while a turn runs. Finished steps of the
  running turn recede while the running one stays bright. The session list
  groups sessions by workspace under Needs you and Running, with folding,
  "Show N more" and `Ctrl/⌘+Shift+F` to search.
- A failed turn now says what went wrong: the TUI and the web client show the
  provider's HTTP status and code, a plain explanation with what to do next, and
  the provider's own message (three lines, opened whole with `Ctrl+O` in the TUI
  or "Show full message" and copied with Copy in the web client). The web card
  offers Compact now or Switch model where that is the fix.
- Reloading the web client over a slow link is faster: every file carries an
  `ETag` and the browser revalidates it, so an unchanged page costs a few small
  `304` replies instead of the whole download. The page also runs under a
  stricter Content-Security-Policy that no longer allows inline scripts or
  styles.
- In the web client the composer's round button keeps its place and changes
  face: ↑ Send, ■ Stop while a turn runs and nothing is typed, ↗ Steer when text
  is typed during a turn, next to a plain-text **Follow-up** button. The model
  is a plain text button on the right of the composer. Stop takes effect at once;
  it ignores a tap right after it appeared (a double tap on Send) and scrolls,
  and with text typed it is in a menu opened by a long press or right click on
  the button. `Esc` stops the turn with one press. `Alt+Enter` (Option+Enter on
  macOS) queues a follow-up and `Enter`/`Cmd+Enter` steers, like the TUI;
  `Tab` is ordinary focus navigation. "Enter sends" now defaults to on for a
  mouse and keyboard and off on touch devices. Queued messages share one row
  style and keep their send order, with separate steer and follow-up numbering. Collapsed thinking keeps its newest sentence
  on a line under the header that moves up as the agent thinks. On desktop the
  transcript, composer and title share one centred column beside the sidebar,
  with side panels for changes and output, hover states and thin scrollbars.
- Fewer messages: a tool you allowed for the session no longer raises
  `Allowed bash for this session` on every call (the TUI and the web client stay
  silent; `/session` and the web session info list the allowed tools), and the
  web tool row shows no "Allowed" line, only a denial with its reason.
  "Session renamed", "Session deleted", "New session: model …" and "Compaction
  cancelled" are gone or a brief flash, since the title, list and footer already
  show them. The TUI's own hints (`Press Ctrl+C again to quit`, unknown
  commands) no longer reach phones, and phones get background-attention text
  without the TUI's `ctrl+r`. Copy in the web client turns its button into a
  check instead of a toast.
- In the web client the thinking block can be expanded while the agent is still
  thinking: the text grows in place and collapsing returns to the one-line
  preview.
- On Windows the phone's folder picker can browse whole drives: list them in
  `[remote] roots`, for example `roots = ['C:\', 'D:\']` (single quotes in TOML),
  and the picker's Roots level switches between them. `C:`, `C:/` and `c:\` all
  mean the drive root, not the current directory of that drive.
- The TUI directory is `--home`, else `$MINICORE_HOME`, else the executable's
  directory: a directory named `bin` is no longer replaced by its parent. A
  bundle that runs `bin/minicore-tui` now needs `--home <dir>` or
  `MINICORE_HOME`; otherwise it reads and writes its files inside `bin/`.
- `Alt+Enter` queues a follow-up while a turn runs; `Ctrl+Enter` does the same
  in terminals that report it (kitty-protocol terminals, Windows consoles).
  `Enter` steers, so check the dock label where neither key
  reaches the app; `docs/keybindings.md` lists the setup for macOS terminals and
  Windows Terminal. The dock tells the two apart: `↗ steer  …` rows, then
  `↳ follow-up 1  …` numbered in send order, and a hint row (`enter steer ·
  alt+enter follow-up`, `alt+↑ edit queued`) names the keys that act now.
- Steering messages in the transcript are marked `↗ steering`, the arrow the
  dock and the web client use for steers; `↳` now means a follow-up.
- Remote mode listens on the IPv6 loopback too: the default `bind` is
  `127.0.0.1:8765` and `[::1]:8765`, so `http://localhost:8765` works however
  `localhost` resolves. `[remote] bind` takes one address or a list, such as
  `["0.0.0.0:8765", "[::]:8765"]` for every interface on both families. An
  address the computer cannot listen on (no IPv6, an IP it no longer has) is
  skipped with a notice; a taken port is still an error. The status screen
  shows the addresses in a new `Listening` row.
- Remote mode can serve HTTPS and WSS itself: `tls = true` with `tls_cert` and
  `tls_key` naming your own PEM certificate and key (Let's Encrypt, a
  Cloudflare Origin CA certificate for Full (strict), or a self-signed one the
  phone trusts). The files are checked at startup like other settings and read
  again by `/remote on`, so a renewed certificate needs only `/remote off` and
  `/remote on`. A LAN or public bind with TLS no longer needs
  `allow_insecure`.
- The web client loads over slow links: its files are sent gzipped, and a reply
  is no longer cut off after 10 seconds while it is still arriving, which
  behind Cloudflare showed as error 520.
- The phone client is redesigned. Sessions that need you are listed first with
  a badge on the menu button; finished turns fold their tool steps into one
  "Worked 47 s · 5 tools · +12 −4" row; tool steps are one-line rows whose
  full output can be opened and loaded in full; thinking shows a "Thinking…"
  header with its newest sentence on one line (then "Thought for 12 s"), and
  opens to the full text. The composer has `/` for commands and prompt
  templates (expanded on the computer like in the TUI) and `@` for workspace
  files and recent prompts; queued follow-ups and steers still waiting show
  above the composer, and follow-ups can be edited or removed.
  Approvals say what they ask ("Run command", "Edit src/x.rs") with a rendered
  diff, never approve by focus or a stray key, and Deny can tell the agent why.
  A session menu opens Changes (the Git diff per file), Context with Compact
  now, session info, find in session and export. Settings hold appearance, text
  size, code wrap, Enter sends, keep screen awake and notifications while the
  page is in the background. iPad and desktop get a sidebar and keyboard
  shortcuts (`?`). The phone protocol is now version 2: reload open phone pages
  after updating.

## 0.6.0 — 2026-10-25

- Remote mode: phones and other browsers can drive the TUI. `--remote` or
  `/remote on` starts a listener (loopback by default, configured under
  `[remote]`) that serves a web client. A device signs in once with the
  password shown in the TUI and then sends messages, steers, queues follow-ups,
  answers approvals, stops turns, loads older history, switches model and
  reasoning, opens, renames and deletes sessions, and starts new ones in folders
  it browses (or creates) under the configured roots. See
  `docs/design-remote.md`.
- Several devices can be connected at once, and each works in its own session
  or new-session draft without switching the others. Each device sees who else
  is connected; a session another device has open cannot be deleted. Opening
  the client in a second tab of the same browser takes over that device's
  connection.
- While remote control is on, the TUI shows a status screen instead of the
  transcript: the address, the password, each connected device with the session
  it is on, the session, workspace and model, what the Agent is doing and
  refused sign-ins. The editor keeps its draft but cannot send, Esc still
  interrupts, and quitting asks twice. `/remote password` sets a new password
  and signs every device out; `/remote off` takes control back.
- The web client installs as an app (Safari's Share → Add to Home Screen on
  iPhone, Install app on Android and desktop Chromium) and, over HTTPS, opens
  while the computer is unreachable and reconnects when it is back. The browser
  and status bar colour follow the chosen light or dark appearance, and it works
  behind a Cloudflare Tunnel without changing zone settings. See
  `docs/configuration.md`.
- Sessions you leave keep running: a turn, its follow-ups and its approvals go
  on in the background, and switching back (`Ctrl+R`, `/resume`) is instant
  and shows everything that streamed meanwhile, with the scroll position and
  draft you left. A background turn that ends or needs approval says so in a
  notice (and the bell), the footer counts `2 running · 1 needs approval`, and
  the sessions picker lists live sessions first with `running 2m`,
  `needs approval`, `compacting` or `new`. Each session, and the empty draft,
  keeps its own editor text. A session that is still working cannot be deleted.
- Opening or creating a session makes its workspace the one the next new
  session uses.

## 0.5.5 — 2026-10-24

- `/context` opens as a sheet below the transcript like `/session`, instead of a
  full-screen view.

- `/diff` shows the workspace's live Git changes, including untracked files.
  Turn-end clicks open the same view; turn/session scopes and Tab scope cycling
  are gone.

- The tool output view and `/tool` command are gone. Expanded cards show plain
  hidden-line counts where output is cut; clicking any row collapses the card.

- The partial reply of an interrupted request (reasoning and text, as the Agent
  stored it) is shown again after reopening a session or loading older pages,
  above the `interrupted` turn-end line. At the end of a live turn it replaces
  the streamed block instead of repeating it.
- A running tool card with no output yet shows a dim `running…` row where the
  expand hint goes (and as the body of an expanded card), so a slow first
  output, such as a new PowerShell on Windows, no longer looks stuck. The first
  output line replaces it.
- Built-in backend: Agent 0.8.0 with Runtime 0.8.0, pinned by tag (0.7.5 before). Automatic
  compaction is triggered by tokens only, with no history size limits; bash and
  read output follow pi (2000 lines or 50 KiB); the post-turn check no longer
  shows as a compaction; overflow recovery works at any history size. Thinking from DeepSeek through
  a Responses proxy (the legacy `response.reasoning.delta` event) now shows,
  and providers that report no cache writes still report input tokens.
- The `History is too large to send` notice is gone: Agent 0.8.0 no longer
  compacts before a send and never refuses one for history size. A message
  waits for compaction only while one is really running.
- A session exists only once you send a message. Starting without `--session`
  or `--continue` (or with `--continue` and nothing to continue) shows the
  welcome header over an empty transcript; the first `Enter` creates the
  session and sends the text, and a failed creation puts the text back in the
  editor. Quitting before that leaves no empty session in the list.
- `/new [profile]` leaves the open session for that empty screen instead of
  creating one. Before the first message `/model`, `/reasoning`, `Ctrl+L`,
  `Ctrl+P`, `Shift+Tab` and `/new <profile>` choose the settings the session
  will start with, and the footer shows them; the commands that need a session
  and the `@` file picker say `No session yet: send a message first`.
- Collapsing a card, `Ctrl+O` or a resize no longer leaves blank rows under the
  transcript: the view stops at the end and follows new output again.
- The welcome header has an empty row between its title, its key hints and the
  workspace; hints that wrap stay together as one group.
- Tool cards of consecutive requests are one blank row apart after a turn is
  reconciled or a session is reopened, as they were while streaming; a request
  that only called tools no longer leaves an empty block between them.
- Switching to a model that does not support the current reasoning level no
  longer fails with `invalid session settings`: the switch also sets the
  nearest supported level (the higher one on a tie) and a notice names it, for
  `/model`, the model picker and `Ctrl+P`, and in a draft for the settings the
  session will start with.

## 0.5.4 — 2026-10-22

- Built-in backend: Agent 0.7.4 with Runtime 0.7.4, pinned by release tag.
- A history too large to send is compacted first: the send waits under
  `Compacting context` and then runs. When that compaction fails, the text
  returns to the draft with a notice naming `/compact` and `/new`, and any
  failed automatic compaction now leaves an error notice.
- An open session takes a reloaded system prompt from its next turn, and a
  reopened session takes the current one; the new prompt replaces the old.
- On Windows, copying goes through the Win32 clipboard instead of `clip.exe`,
  which kept a byte order mark in the copied text. Pasted, recalled and drawn
  text drops byte order marks, which shifted the editor caret off its text.

## 0.5.3 — 2026-10-21

- Built-in backend: Agent 0.7.3 with Runtime 0.7.3, pinned by release tag.
- No minimum Rust version is declared or tested; build with current stable.
  Every dependency is at its latest release, including toml 1 and toml_edit
  0.25; the Agent no longer forces shared crates down.
- CI runs Clippy on Linux, macOS and Windows.
- The configuration examples use a public provider URL, and the repository no
  longer ships `reference.png`.

## 0.5.2 — 2026-10-21

### One executable

- The Agent is built into `minicore-tui`: the TUI starts itself as
  `minicore-tui agent --config … --stdio` and still talks to it over stdio
  JSON-RPC, so the process boundary, stderr capture and protocol checks are
  unchanged. Packages hold only `bin/minicore-tui`.
- `--agent` and `[agent] binary` now name an external Agent to run instead of
  the built-in one; there is no `bin/minicore-agent` or `PATH` lookup any more.
- Built-in backend: Agent 0.7.2 (`141396f`) with Runtime 0.7.1 (`c0181d1`).

### Terminal rendering and local time

- Paint with the hardware cursor hidden, position the editor caret before showing
  it, and leave the cursor hidden when painting or positioning fails. This
  prevents stray cursors beside the Working spinner and changing timer digits.
- On a Windows console that speaks ANSI, a frame is written in one flush inside a
  synchronized update. Other terminals and the legacy Windows console flush as
  before.
- Request a steady block cursor, restoring the terminal default on exit, panic
  cleanup and external-editor handoff.
- Bundle IANA time-zone data on platforms without system zoneinfo, notably
  Windows, so user-message timestamps use the system zone instead of silently
  falling back to UTC. Linux and macOS keep reading the system zoneinfo.

### Composer and paste

- Click to place the caret, drag or Shift+click to select, and extend or shrink
  selections with Shift+arrows or Shift+Home/End. Word modifiers select by word;
  graphemes and paste chips remain atomic across wrapping and scrolling.
- Replace selections by typing, pasting or inserting newlines; delete and
  undo/redo selections. Ctrl+C copies a selection and Ctrl+X cuts it, then drop
  it, so a second Ctrl+C quits again; a cut the clipboard would refuse keeps the
  text. Without a selection both keep their previous meaning.
- Selecting inside a recalled prompt no longer loses the draft `↓` returns to,
  and Shift+Home/End with no draft scroll like Home/End again.
- On Windows the console reports a bracketed paste as its keys; the markers are
  decoded so embedded commands, Ctrl+J and trailing newlines stay draft text, and
  a paste whose end marker never comes is released after a second instead of
  swallowing all input. Unbracketed multiline bursts within 30 ms count as one
  paste. Linux and macOS pass crossterm's paste events through with no delay.
- Keep editor hit targets and paste input out of full-screen views, approval
  sheets and completion rows.
- A tool card or thinking block opens and closes downward, whether by click,
  `Ctrl+O` or `Ctrl+T`: the top of the view stays put. `Ctrl+O` and `Ctrl+T`
  confirm with a flash, so the dock does not move the view either.
- Folded thinking always keeps its newest rows under an `earlier` hint, while it
  streams and after, instead of switching to its first rows when it ends.
- A drag in the editor copies on release, like one in the transcript, and the
  editor selection has a stronger background.
- A transcript selection takes only content: rails, padding, card durations,
  expand hints, timestamps, turn-end lines and the welcome header are neither
  highlighted nor copied.

### UI simplification

- Remove the empty editor hint, running-card spinner and live timer, the duplicate
  Working elapsed timer, and footer profile/redundant run-state labels.
- Stop invalidating running-card caches on every animation tick; real tool events
  still refresh them. Preserve the main Working spinner, completed tool durations,
  footer elapsed/model/reasoning/usage/context, queue and allowed-tool indicators,
  and all actual profile and approval behavior.

## 0.5.1 — 2026-10-21

0.5.0 was never tagged; 0.5.1 is the first 0.5 release and includes everything
below.

### Platforms and packaging

- CI uploads a package per platform (Linux x86_64, macOS arm64 and x86_64,
  Windows x86_64): `bin/minicore-tui` with the pinned `bin/minicore-agent`, the
  README, the changelog, the configuration reference and `BUILD.txt`.
- On Windows the bundled Agent is found as `bin/minicore-agent.exe`.
- The backend pins are Agent 0.7.1 (`004dbf2`) and Runtime 0.7.1 (`c0181d1`),
  by full SHA.
- The test suite passes on Windows: render fixtures need no child process,
  configuration tests compare paths the way the platform writes them, and the
  escape-sequence test runs only where crossterm writes sequences.

### Configuration (breaking)

- One TUI directory (`--home`, `$MINICORE_HOME`, else the executable's directory,
  or its parent named `bin`) holds everything: `config.toml` (profiles, `[ui]`,
  `[agent]`, `[loop]`, `[compaction]`), `models.toml` (providers and models),
  `prompt/SYSTEM_PROMPT.md`, `prompt/commands/*.md`, `session/` (all sessions)
  and `history.jsonl`. See [docs/configuration.md](docs/configuration.md).
- The TUI composes the Agent config from the two files into
  `session/agent.toml` on every start. `--config` is replaced by `--agent-config`
  (a raw file, no composition). `~/.config/minicore-tui/`, `$MINICORE_TUI_CONFIG`
  and `prompts/` are no longer read; move templates to `prompt/commands/`.
- A malformed file, unknown key, dangling reference, missing key variable or
  empty system prompt stops the start with `file:line: key: reason` before the
  screen changes, instead of silently falling back to defaults.
- A first run writes commented starter files and exits. `/reload` composes the
  Agent config again; `/theme` keeps the comments in `config.toml`.

### Transcript

- A user message card shows when it was sent, `21:38 · 10/20/2026` in local
  time, on the row under its text; steering cards add `↳ steering`. Live
  messages take the Agent's `accepted_at`, history the stored timestamp; old
  history without one shows no row.
- Rows added to the dock (a steer, a queued follow-up, a notice, an approval)
  no longer hide the bottom of an anchored view, such as a running tool card:
  the view gives up rows at its top instead.

### Performance

- A running turn costs about half the CPU: the dock layout is plain `Rect`
  arithmetic instead of a constraint solver, rows are painted without ratatui's
  grapheme walk when they are ASCII or a lone narrow character, and terminal
  input no longer spins on `sched_yield`.
- A flash, the scrollbar or an info notice wakes the loop once, when it
  expires, instead of ticking 12 times a second for as long as it shows.
- Measured over an eight-turn, 70-minute coding session (gpt-6-luna, max
  reasoning, 140x45, release build): the TUI holds 7.6–10 MB and uses 0.9 % of a
  core while a turn runs (1.8 % before this release), 0 % idle. pi 0.87.1 with
  pi-rail-ui on the same prompts used 200–280 MB and 6–12 %.

## 0.4.0 — Rewrite

A rewrite of the frontend against `minicore-agent` 0.7.0 (`4102719`) and
`minicore-runtime` 0.7.0 (`b93aea3`) over Protocol v1. The CLI, config file,
keys and commands changed incompatibly. Requires Rust 1.88 or newer.

### Turns

- A turn ends exactly when `turn.wait` resolves; Agent events are only display
  hints. If the wait is lost, the stored `turn.result` ends the turn. A turn can
  no longer stay "incomplete" after a compaction.
- After a gap in events or a turn that did not complete, the turn's durable
  items replace what was streamed, so the transcript matches the store.
- A message sent while the Agent still compacts after a turn is queued and sent
  when the session is idle, instead of being lost.
- `Enter` steers a running turn; `Alt+Enter` queues a follow-up; `Alt+↑` takes
  the queued follow-ups back. Steers the Agent did not apply become follow-ups.
- Approval and choice prompts are answered inline (`y`, `n`, `Enter`).
- The turn-end line shows outcome, duration, requests, tools and, when files
  changed, `N files changed (+a −b)`. If a diff is binary, truncated or
  unreadable, the line gives only the file count rather than a wrong total.
- Untitled sessions are named from their first prompt.

### History and sessions

- The latest items load first; older pages load as you scroll to the top, and
  the history behind a compaction summary is reachable from the
  `earlier items` row. A session that opens on a summary reads back to the
  latest answer.
- Each history read takes its own snapshot pin, so reopening a session no longer
  fails with `invalid_state`; a stale pin is retried once.
- Only the open session is kept in memory; returning to another reloads it. A
  turn keeps running in the Agent while you look at another session.
- `/export` writes the canonical conversation as Markdown.

### Transcript and rendering

- Blocks are rendered once and cached by revision, width and fold state, and
  only visible rows are drawn, so cost no longer grows with history length.
- Streaming Markdown re-renders only the unfinished tail of the message.
- Word wrapping with CJK-aware breaks and preserved list indentation.
- Scrolling is anchored to what you read: streaming output does not move the
  view, and a `Jump to latest message` label shows when you are above the bottom.
- Tool cards are folded by default and bounded: duration, exit code, line
  changes, the output tail of a live `bash` call, and a tool view with the full
  stdout and stderr. After a reload a card shows its line count and a
  `click for full output` row.
- Reasoning folds to its first three rows when finished; `Ctrl+T` collapses it to one row.
- Compaction shows as a status phase and a one-line marker with token counts.
  Loaded content is never removed.

### Interface

- One visual system: a bottom-sheet style for pickers, approval prompts and
  find; full-screen views for diff, tool output, context and help; English copy;
  dark and light palettes, and `NO_COLOR`.
- Command palette with descriptions, `@` file picker, session picker with
  delete, model and reasoning pickers, find in the transcript, drag-to-copy,
  `/copy`, `/export`, `/theme`, `/tool`, `/diff [scope]` and `Ctrl+G` for an
  external editor.
- The footer's `ctx N%` is the context usage the Agent reported for its last
  request; it is `ctx ?` until reported, never an estimate.
- The fatal screen shows the reason and the Agent's stderr tail.
- The composer has grapheme-safe editing, undo/redo, prompt history and paste
  tokens; it no longer depends on `tui-textarea`.

### CLI, configuration and protocol

- Options: `--agent`, `--config`, `--workspace`, `--session`, `--continue`,
  `--profile`, `--model`, `--reasoning`, `--theme`, `-h`, `-V`. `--config` is
  required unless the TUI config sets `agent_config`.
- TUI config at `$MINICORE_TUI_CONFIG`, else `$XDG_CONFIG_HOME` or `~/.config`
  `/minicore-tui/config.toml`, with `agent`, `agent_config`, `theme` and
  `editor` (a list: command and arguments).
- The handshake requires Protocol v1 and ten capabilities (`session.read`,
  `session.read.display`, `session.context`, `turn.result`, `tool.read`,
  `tool.read.display`, `tool.output`, `workspace.files`, `changes.list`,
  `changes.diff`). The Agent is spawned before the alternate screen and shut down
  with a bounded wait.
- Dependencies: ratatui 0.30, crossterm 0.29, pulldown-cmark 0.13. `tui-textarea`,
  `icu_segmenter` and `time` are gone; `tempfile` is test-only.

### Removed

- The v0.3 state machines, request-kind registry, generation counters, test
  suites, scripts and historical specification documents.
