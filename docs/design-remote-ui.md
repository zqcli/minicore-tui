# Phone client v2 design

The phone client (`src/remote/web/`) is redesigned for daily use as a remote
for a coding agent: quieter and more readable, safer around approvals and
stopping, richer (changes, context, full tool output, commands, templates,
`@` files, settings), and good on iPad and desktop. Breaking changes to the
web client and the phone protocol are accepted; the protocol becomes v2.

Inputs: competitive research (ZCode remote control, Happy, Claude Code remote,
Codex/ChatGPT, Cursor iOS, DeepSeek, Kimi, Qwen, Doubao) in
`minicore-remote-build/research/report.md` and the audit of v1 in
`minicore-remote-build/audit/report.md`. This document is the contract; where
the code and this document disagree after it lands, the code wins and this
file is fixed. `docs/design-remote.md` stays the protocol reference and gains
the v2 additions below.

## Principles

- **Real time in the foreground, recoverable in the background.** Reconnects
  never reload the page; a snapshot restores everything.
- **The agent's work is quiet; your decisions are loud.** Tool steps are
  one-line rows and fold away when a turn ends; approvals, failures and
  sessions that need you stand out by shape and colour.
- **Safe by construction.** Nothing approves by focus or a stray key; Stop is
  always reachable while anything runs, takes the send button's place, and is
  guarded against double taps and scrolls; destructive actions confirm.
- **Fewer boxes, clear type.** Flat assistant text, tinted user bubbles, ruled
  tool rows, cards only for approvals and summaries.
- **Phone first, then iPad and desktop**, which get a persistent sidebar and
  keyboard shortcuts.

## Protocol v2 (server, `src/remote/*`, `src/app/remote.rs`)

`wire::VERSION` becomes 2; a v1 client gets the existing `version` error and
shows "Update needed · Reload". All requests act on the requesting device's
own view (its session or draft), as v1 intents do.

### New and changed intents (`{"t":"intent","id","kind",...}` → `ack`)

| kind | fields | effect |
|---|---|---|
| `compact` | — | `session.compact` for the viewed session; refused while a turn runs or compaction is under way (`busy`), and in a draft (`No session`). `cancel` already stops a compaction. |
| `unqueue` | `index`, `text`, `edit` | Removes `queue[index]` if its text still equals `text` (else `stale`). With `edit:true`, text returns to the requester as `returned` once the Agent has removed it; `edit:false` deletes without returning text. A row the running turn took meanwhile is reported as a notice. |
| `answer` | + optional `message` (deny only) | Denies, then pushes `message` as a steer so the agent hears why. A blank `message`, or one beside anything but an approval's `deny`, is `bad_request`. |
| `submit` / `queue` | text | Text that starts with `/name` naming a prompt template is expanded on the host exactly as the TUI does (`$1`, `$@`, …); `//text` sends `/text`. A built-in command line, and a bare `/word` naming nothing (where the composer warns once), are refused with `unknown_command`; the client handles built-in commands itself and never sends them. |

### Queries (`{"t":"query","id","kind",...}` → `{"t":"result","id","ok":true,"data":{...}}` or `{"t":"result","id","ok":false,"error":"..."}`)

Relays of existing Agent RPC for the viewed session, answered only to the
requester. At most 4 queries in flight per connection (more → `busy`); each is
bounded as listed. A query whose session left the device's view still
answers (the client ignores stale replies by id). A draft answers
`No session`. The exact `data` shapes are in `docs/design-remote.md`
("Queries"); they differ from this table in two places: `context` carries the
`/context` figures (`tokens`, `window`, `percent`, `budget`, `history_tokens`,
`trigger_tokens`, `target_tokens`, coverage, last compaction), because the
Agent reports no per-category breakdown, and `changes` rows have no line
counts (each needs a `changes.diff`); `diff` returns `added` and `removed`
instead. `diff` of a path not in the list is `stale`.

| kind | fields | Agent RPC | `data` (bounded) |
|---|---|---|---|
| `context` | — | `session.context` | the context breakdown the TUI's `/context` shows: totals, window, percent and categories with token counts |
| `changes` | — | `changes.list` (workspace scope, as `/diff`) | `{branch, files:[{path, status, added, removed}], truncated}`; at most 200 files |
| `diff` | `path` | `changes.diff` | `{path, lines:[...], truncated}`; at most 256 KiB of diff text |
| `tool_output` | `call_id`, `offset` | `tool.output` (`stream: output`) for that call in the viewed transcript | `{text, offset, next_offset, eof, truncated}`; pages of at most 64 KiB |
| `files` | `query` | `workspace.files` | `{paths:[...], truncated}`; at most 50 names, names only, never contents |

### State additions

- `state.commands`: `[{"name","description","hint"}]` the prompt templates
  the TUI knows (name, its description line, argument hint), shared by all
  devices, sorted by name.
- `state.session.branch`: the Git branch the TUI shows for that session's
  workspace, or `null`.
- Unchanged fields stay as they are; `devices[].session`, `user.at`,
  `turn_end.usage`, `tool.output_lines` are now used by the client.

### Running tool output

A running tool block whose new output only adds lines at the end is sent as
`{"i":n,"append":{"output":[...lines]}}` instead of the whole block, like
assistant text. The first string continues the last output line (the line
still being written), the rest are new lines. When the bounded tail drops lines from its start, or anything
else changes, the full `block` is sent. The finished block is always sent once
in full.

## Client (`src/remote/web/`)

Plain files, no build step, no external requests, no inline script or style
(the CSP forbids them; see [Code layout](#code-layout)). `core.js` stays the
pure, tested logic module and grows; DOM code is reorganised into small
components that update only when their inputs change (no whole-region
re-renders on every `state`).

### Layout and navigation

- **Top bar**: ≡ (badge with the number of *other* sessions that need you),
  title (tap: session info sheet; double tap or tap when scrolled: scroll to
  top), subtitle `workspace · branch`, right side: **⊕ New** and **⋯ session
  menu** (Rename, Changes, Context, Compact, Copy session id, Share/Export,
  Delete). The rename pencil goes away.
- **Drawer** (phone) / **persistent sidebar** (≥ 900 px): header (logo, host,
  ⚙ settings), New session, search, then groups **Needs you** (approval or
  question), **Pinned**, **Running**, then one group per workspace (folder icon,
  the folder's name, the full path in its tooltip), the group with the newest
  session first and each group's sessions newest first with the relative time
  on the right. Every session is in exactly one group, in that order of
  precedence: a pinned session that needs you is listed under Needs you only.
  A session's time is its last activity (`updated_at`, which the host advances
  when a turn ends), so a session that just ran tops its workspace group and
  that group leads. A group folds with its header (remembered per workspace
  path in `localStorage`, Pinned under its own key), shows five rows and then
  "Show N more", and a search shows every match in every group with a match,
  Pinned included. Status by shape and colour:
  amber hand = needs you, spinner = running, red ! = failed last
  turn, blue dot = new result, green dot = recent (below). Rows are single-line, 36 px on hover devices and
  44 px on touch, with 14/15 px titles and a 16 px status column. Workspace
  headers are 32 px tall. Hover or keyboard focus replaces the relative time
  with the actions button in the same slot; touch keeps a compact actions
  button visible. Pinned rows omit workspace subtitles rather than taking
  another line. The order never changes while the drawer is open
  (rows update in place); it re-sorts when reopened, except that pinning or
  unpinning moves the row at once, and a session that starts waiting on the
  user moves to the top of Needs you (which never folds), so a folded Pinned
  or workspace group cannot hide it; it stays there until the list re-sorts. ⌘/Ctrl+Shift+F focuses
  the search (a hint shows on hover devices; ⌘/Ctrl+K stays the command list).
  Footer is one line: connection dot + device name, other devices count.
- **Pins**: "Pin to top" or "Unpin" in a row's ⋯ menu and in the session menu
  puts a session in **Pinned** (pin icon, a count when folded, newest pin first,
  each row with its own status icon and its folder in the tooltip) and takes it
  out of Running and its workspace group. The pins are an ordered list of ids in
  `localStorage` (`minicore.pins`): no host or protocol change, and each device
  keeps its own working set. A pin whose session left the catalog is dropped when
  the state arrives, unless the catalog holds the host's cap of 50 and may hide
  it. The TUI's session picker has no pins.
- **Recent mark**: a session that is resident in the TUI, idle and whose last
  turn ended less than 30 minutes ago, outside Needs you, Pinned and the open
  session, shows a green dot in the status column (below unseen's blue dot, red
  failed and the spinner) and a stronger time; its row label and the dot's
  tooltip say "finished 3m ago". It is worked out when the list renders from the
  host clock, with no timer, so it lapses at the next state message after the
  half hour.
- **Settings** (full-screen sheet): Appearance (System/Light/Dark), text size
  (4 steps), wrap code by default, show thinking expanded, Enter sends (for
  hardware keyboards; on by default for a fine pointer with hover, off on touch,
  and stored only once chosen), keep screen awake while a turn
  runs (Wake Lock), notifications while this page is open (Notification API
  through the service worker, only where supported, asked on first use),
  connected devices with what each views, host and version (`welcome.version`),
  install row, sign out (confirm).
- **Sheets** share one modal manager: focus moves in and back, background is
  `inert`, `aria-modal`, Esc / swipe down closes, the keyboard lift (`--kb`)
  applies to all.

### Composer

- Two-row card at the bottom: auto-growing textarea (max 40 % of the visual
  viewport), then a 36 px toolbar on hover devices or 40 px on touch: **+** (sheet: prompt
  templates and commands, recent prompts, insert `@file`), a **shield**
  (`Approvals`: a small sheet naming the tools allowed for the session, or
  "Every tool asks first"; a dot on the shield when any is allowed), the
  **context chip** (`23%`, warning colour from 80 %, opens the Context sheet)
  and, in a draft, the **folder** and **profile** chips on the left; on the right the **slot** and
  the **primary button**. Icons are 18 px in transparent 32 px boxes with
  40 px touch hit areas. Context is plain 13 px text; the model and Follow-up
  are 13 px on desktop and 14 px on phones. Hover, focus and press add a subtle
  fill with 8 px corners. The primary is a 32 px circle on hover devices or
  36 px on touch, with a 16 px icon, keeping its place as its face changes:

  | State | Primary | Slot |
  |---|---|---|
  | idle, empty | ↑ Send, disabled | model · effort ⌄ |
  | idle, text | ↑ Send (accent) | model · effort ⌄ |
  | working, empty | ■ Stop (neutral: `--tx` fill, `--bg` icon) | model · effort ⌄ |
  | working, text | ↗ Steer (accent) | ↳ Follow-up (accent text button) |
  | stopping | spinner, disabled | model · effort ⌄ |

  A compaction counts as working with nothing typed: the primary is Stop and
  cancels it. The model text is middle-truncated (16 characters on a phone, 28
  on a wide screen) and opens the model sheet. Hover devices get tooltips with
  the shortcuts.
- **Stop** is the primary button while a turn runs and nothing is typed, and
  stops at once: no arming step. It is guarded against accidents by logic in
  `MC.primaryTap`:
  - The action comes from the live textarea value at the moment of the tap, so
    typing then tapping steers and never stops.
  - A tap within 600 ms after the primary turned into Stop is ignored (the
    second half of a double tap on Send, as the composer empties and the turn
    starts or the steer is acknowledged).
  - A pointer gesture that travelled more than 10 px is a scroll, not a tap.
  With text typed during a turn, Stop stays reachable: a long press (450 ms) on
  the primary, or a right click or the context-menu key, opens a small menu
  above it: ↗ Steer now · Enter, ↳ Queue as follow-up · ⌥/Alt+Enter, ■ Stop
  the turn · Esc. **Esc** (no sheet, list or menu open) stops the turn with a
  single press, like the TUI. The primary's `aria-label` follows its face
  (Send, Steer, Stop the turn, Stopping); a change to Stop, Steer or Stopping
  is announced through the `aria-live` region, typing is not.
- **Steer** queues text for the running turn (`submit`; the agent reads it
  at its next request). **Follow-up** queues a new turn (`queue`). The Agent
  keeps the order: every steer runs before any follow-up. Hardware keys mirror the TUI: Enter (with "Enter sends") or
  Cmd/Ctrl+Enter steers while busy and sends when idle; **Alt+Enter**
  (Option+Enter on macOS; browsers report it, so no settings are needed) queues
  while busy with text typed and no `/` or `@` list open (the list keeps Tab and
  Enter for completion). Tab is plain browser focus navigation, Shift+Enter adds
  a line. The placeholder says what typing does: "Message MiniCore · / for
  commands" when idle, "Enter steers · ⌥Enter queues a follow-up" while a turn
  runs (Alt+Enter off Apple platforms, Ctrl+Enter without "Enter sends"), and
  "Type to steer or queue a follow-up" on a device without a keyboard.
- `/` at the start opens a filtered command list: built-ins handled by the
  client (`/new`, `/model`, `/rename`, `/compact`, `/context`, `/changes`,
  `/export`, `/settings`) and `state.commands` templates (sent as text, the
  host expands them). `@` opens a file list (`files` query, debounced) and
  inserts `@path`. Both are keyboard and touch navigable.
- Recent prompts (last 50 per host, `localStorage`), reachable from **+** and
  with ↑ in an empty composer on hardware keyboards.
- `enterkeyhint="send"`, `autocapitalize="sentences"`, autocorrect off while
  the text starts with `/` or `@`; drafts persist per session as today.
- The queue above the composer lists the Agent's run order (steers first) under
  "Steers join this turn · follow-ups start the next". Every row shares the same
  height, padding and radius. Accent-labelled `↗ Steer N` and neutral
  `↳ Follow-up N` are numbered independently. Every row has pencil **Edit in
  composer** and **Delete** buttons; edits merge the requester-only `returned`
  text through the ordinary composer path. A row leaves the list when the
  running turn takes it. Three rows show at first, then "+N more" expands the
  rest. Taken steers keep their transcript label; either kind starting a new
  turn becomes an ordinary prompt. Unconfirmed sends keep
  their single "Not sent · Send again" card; the composer is not filled twice.

### Transcript

- **User**: tinted bubble; long ones collapse after 12 lines with "Show more";
  actions (long-press / ⋯): Copy, Edit (fill the composer), Resend, time.
- **Assistant**: flat Markdown, rendered **incrementally** while streaming
  (finished top-level blocks are kept; only the open tail is re-parsed). Only
  the block being written shows the caret. Actions under a finished answer:
  Copy, Share (Web Share API where available), and time.
- **Thinking**: collapsed but alive. A muted "Thinking…" header with a shimmer
  and, under it, one line (fixed height, ellipsis at the end) with the newest
  sentence of the reasoning; when a new sentence starts the line slides up
  (≤ 200 ms, none with reduced motion), and text within a sentence only
  updates it in place. When done the header reads "Thought for 12 s ›" (the
  duration when the client watched it live) and the last line stays under it.
  Tapping the header or the line expands the full text and back, also while the
  agent is still thinking: the open text grows in place (capped to a scrolling
  box that follows the end only while the reader is at it), collapsing returns
  to the one line, and the choice survives the turn ending. The "show thinking
  expanded" setting opens it by default.
- **Status strip**: directly above the composer, a spinner, label, elapsed
  time and running tool detail show while a turn, compaction or session opening
  is active. Tool detail follows transcript updates even without a state
  change. Context appears only in the composer toolbar. Finished tool rows of
  the running turn use the secondary text tone and the running row the primary.
- **Tools**: one-line rows: state icon · past-tense verb · muted mono target ·
  right-aligned meta (duration, `+12 −4`, exit code). Consecutive reads and
  searches group ("Read 6 files ›"). Tap expands inline: command, the **end**
  of the output (last 20 lines, scrolled to the bottom), Copy, Wrap, "Open"
  (full-screen output viewer: tail first, wrap toggle, copy, **Load full
  output** via `tool_output`). Failed tools auto-expand their last lines.
  Edits render as diffs (below).
- **Turn folding**: when a turn ends, steering messages split its work into
  segments. Each segment folds its tools and intermediate thinking into a
  "Worked 47 s · 5 tools ›" row in place, with its own tool and failed counts.
  Duration uses the bounding prompt, steering and turn-end clocks when known;
  otherwise it is omitted. With steering, whole-turn change totals stay on the
  turn-end line rather than being attributed to one segment. The final answer stays visible. Expanding adds a
  trailing "Hide" row; anchoring keeps the tapped row in place. The running
  turn is never folded.
- **Turn end** line stays (completed / interrupted / failed, duration, clock
  time, tokens from `usage`); tapping "+12 −4" opens Changes. A failed turn is
  a card built from `turn_end.failure`: a warn icon, the title, the provider's
  `status · code` in mono, duration and time; the hint below; the provider's
  message in a tinted quote box clamped to three lines (a "Show full message"
  toggle where it overflows, and Copy); and a Compact now or Switch model button
  when `failure.action` names one.
- **Diffs** (tool rows, approvals, Changes): per-file header, line numbers,
  add/remove tints, wrap on by default on phones, horizontal scroll per hunk
  when off.
- **Code blocks**: language label, Copy, Wrap; tables scroll with an edge
  shadow; nothing clips without a hint.
- **Scroll**: auto-follow only near the bottom. One control, `#jump`, centred
  above the dock, leads back to the latest content (End does the same when focus
  is not in a text field):

  | State | Shows when | Face |
  |---|---|---|
  | hidden | the reader is within 80 px of the bottom with every newer block mounted, and has not scrolled up by more than min(screen height, 200 px) since | — |
  | at rest | the reader is more than min(screen height, 200 px) above the bottom, or newer blocks are unmounted | 36 px round ↓ button, `aria-label` "Jump to latest" |
  | new output | output arrived since the reader left the bottom | the "↓ New output" pill |

  Showing needs that distance and hiding needs the bottom, so a view settling
  near one line does not flicker; the control is touched only when its state
  changes, never per scroll event. A click jumps at once (remounting newer blocks
  that were unmounted) and hides it. Older history keeps the existing paging,
  with one consistent "Load earlier" control; the mounted window is capped at
  both ends.
- **Search in session** (⋯ → Find, or Cmd/Ctrl+F): highlights matches in loaded
  blocks, next/previous.

### Conversation spacing

Three gaps, set once as custom properties of `:root` and applied between
neighbouring blocks (`#blocks > .b`). They are margins that collapse (the larger
one wins), so a hidden block, such as a folded step or an empty assistant
block, never changes the gap between the blocks around it.

| Level | Property | Value | Between |
|---|---|---|---|
| Step group | `--gap-tight` | 2 px (0 inside a Worked row's own block) | consecutive tool rows; a thinking-only row and the tool rows next to it; a Worked row and its first child, and the last child and "Hide" |
| Kinds in one turn | `--gap-step` | 12 px | assistant text and tools either way; a Worked row or tool and the answer; a steer bubble and the next step; thinking and the text under it; the answer and the turn end line; "Load earlier" and the first block |
| Turns | `--gap-turn` | 24 px | the turn end line and the next prompt; a prompt (not a steer) and the first response |

Step rows (`--row`) are 36 px on touch and 32 px with a hover pointer, which
is also the thinking row's height; touch keeps the full 36 px hit area. A user
bubble's time row (28 px, 36 px hit area) belongs to the bubble, so the turn gap
counts from below it. Expanded tool output keeps 8 px under its box, which
collapses with the next gap.

### Approvals and questions

- Never focus an approving control. On hardware keyboards `y` allow, `a` allow
  for session, `n` deny work only after the sheet has been visible for 600 ms
  and only when focus is not in a text field.
- Title says what is asked: "Run command", "Edit `src/x.rs`", "Write
  `README.md`", from the tool name and detail; the command (with cwd when
  known) or the rendered diff comes first; button labels from
  `interaction.options` when the host provides them.
- Deny opens an optional one-line reason ("Tell the agent why"), sent as
  `answer` + `message`.
- The sheet can be collapsed to a sticky bar above the composer ("Approval
  needed · Review") to read the transcript, and is re-opened from it.
- Choice questions: numbered options, neutral styling (no option is primary
  unless the host marks one).
- A denial stays on the tool row with its reason (by this device, or "Denied
  on another device"). Allowed tools show nothing extra: the row's own state
  says what happened, and a tool allowed for the session is answered by the host
  without any message.

### Changes, context, session info

- **Changes sheet** (⋯ → Changes, turn-end `+n −m`, `/changes`): branch,
  totals, file list with status and counts; tap a file for its diff (`diff`
  query). Pull to refresh.
- **Context sheet** (⋯ → Context, the context chip in the composer):
  breakdown bars from `context`, and **Compact now** (confirm).
- **Session info** (tap the title): title, id (copy), workspace, branch,
  profile, the tools allowed for this session (when any), model, created/updated, usage totals, other devices viewing it.
- **Export/Share**: the loaded transcript as Markdown through Web Share, else
  copied.

### Notices

The same four rules as the TUI (`docs/architecture.md`). Nothing announces what
the user just did and can see: Copy turns its own button into a check for a
moment (a "Copied" toast only where no button is left, such as a menu row or
`/copy`), and renames, deletions and model changes show in the title, list and
chips. A tool allowed for the session runs without any message, and the session
info lists it. Toasts stay for what the user did not cause or needs to act on:
a background approval or finished turn, a returned message, a refused action,
errors (sticky until dismissed), a command or action that cannot run and why.
While offline the send and queue controls are disabled and the banner is the
single signal; other controls toast "Not connected" once. Notice text from the
host arrives without TUI key hints.

### Background sessions and attention

- Badge on ≡, `(n)` title prefix, app badge, and a sticky banner under the top
  bar when another session starts needing you ("“Fix parser” needs approval ·
  Review"), cleared when handled. Finished background turns give a short toast
  with Open. The v1 floating chip goes away (the badge and the drawer groups
  replace it).
- While the page is hidden and notifications are allowed, approvals and
  finished turns raise a notification through the service worker
  (`showNotification`); tapping it focuses the app and opens that session.

### Visual system

- Tokens consolidated: type scale 12 / 13 / 14 / 16 / 18 / 22 px, line height
  1.6 for prose (CJK friendly), weights 400/500/600; radii 8 / 12 / 20 / pill;
  4 px spacing grid; one accent (current blue); amber = needs you, red =
  error/deny, green = success/add.
- Contrast: all text ≥ 4.5:1 (fix `--tx3` in both themes, success text, accent
  on tinted backgrounds).
- Touch targets are at least 44 × 44 px outside the compact composer toolbar,
  whose controls keep at least 40 × 40 px hit areas, and the transcript's step
  rows, which keep 36 px.
- Motion: 120–180 ms for small state changes, 220–280 ms for sheets and the
  drawer, `cubic-bezier(.2,.8,.2,1)`; no per-token animation;
  `prefers-reduced-motion` turns shimmer and slides into fades.
- Text size setting scales rem-based type. Text fields are at least 16 px on
  touch devices, because iOS Safari zooms into a smaller focused field and keeps
  the zoom; the message textarea is 15 px only with a fine pointer and hover.

### Accessibility

`aria-live="polite"` region announcing turn start/end, approvals and errors
(state changes, not tokens); the status strip is updated in place, not
recreated; landmarks and a real heading for the session title; labelled icon
buttons; status never by colour alone.

### iPad and desktop

≥ 900 px: persistent sidebar + transcript; sheets become centred dialogs or a
right-hand panel (Changes, output viewer, context) at ≥ 1200 px, 30 % of the
width between 440 and 600 px. The transcript, the composer and the top bar
share one column (780 px, 860 px from 1700 px) centred beside the sidebar, with
the title left-aligned to it. Desktop pointers get hover states, tooltips from
the buttons' labels and thin themed scrollbars; tables wider than the column
scroll instead of breaking words. Shortcuts:
Esc close the top sheet or menu, else stop the turn, Cmd/Ctrl+K command list,
Cmd/Ctrl+Enter send or steer (Enter does when the setting is on; Alt+Enter queues),
Cmd/Ctrl+F find, Cmd/Ctrl+Shift+F search sessions, Alt+↑/↓ previous/next session, End jump to the
latest message (outside text fields), `?` shortcut help.

## Code layout

Every file is served as it is in the repository (`src/remote/server.rs` embeds
them; a test fails when a file is missing from the table, the page's
`modulepreload` list, or the service worker's precache).

| File | Responsibility |
|---|---|
| `index.html` | Markup only: the icon sprite, screens, the app frame and `<link rel="modulepreload">` for every module |
| `app.css` | All styles, on the custom properties of `:root` (themes are `[data-theme=dark]` overrides) |
| `theme.js` | Classic blocking script: sets `data-theme`, the text size and the theme-color metas before the first paint |
| `core.js` | Pure logic, no DOM, network or timers (a test enforces it): messages, the store and reducer, formatting, tool rows, folding, approvals, composer decisions; Markdown token trees |
| `ui.js` | The DOM kit (`h`, `icon`, `part`, `set`, `keepFocus`), shared rows (`menuRow`, `rowText`, `errBox`), toasts, clipboard, the modal stack and sheets, `actions`; preferences, wake lock, notifications, the install prompt |
| `net.js` | The store `S`, the socket, intents, filesystem requests and queries (correlated by id); the sign-in and status screens and the connection's life: connect, backoff, wake, sign out |
| `sessions.js` | Settings, model, changes, context, output, help and confirm sheets; the drawer or sidebar, session actions, rename, delete, info and the new-session folder picker; banners, the app badge and tab title, and what background sessions ask of this device |
| `conversation.js` | The composer (status strip, queue, primary button, `/` and `@` lists, draft), the transcript (blocks, incremental rendering, folding, the mounted window, scroll anchoring, find) and the approval and question sheet with its bar and keys |
| `main.js` | Message dispatch, the top bar, global keys, lifecycle and boot |

Rules:

- State has one owner. The host's state and blocks live in `S` (`net.js`;
  only its reducer writes them). Everything else is private to the module that
  draws it (the mounted window and folds and the lists in `conversation.js`,
  the modal stack in `ui.js`) and is changed through that
  module's functions (`markDirty`, `markDenied`, `markFailed`, `abandon`).
- Modules import downward only and never form a cycle (a test checks it). A
  call that has to go up, such as the composer opening Find, goes through
  `actions`, which the defining module fills in.
- Rendering is incremental: a `part` re-renders its region only when its input
  changed, and the transcript renders only blocks marked dirty, appending to
  an open assistant block instead of rebuilding it.
  Renders that run on every state or frame write only what changed (`part`,
  `set`), and lists of rows share one delegated listener keyed by `data-k`.
- Nothing is inline: styles come from classes, scripts from files.

## Kept from v1

Password sign-in and remembered tokens, the connection lifecycle (backoff,
wake on visibility/pageshow/focus/online, unconfirmed sends never resent
automatically), host clock offset, the iOS keyboard handling (`--h`, `--kb`),
safe areas, the PWA (manifest, service worker, install row, theme-color sync),
per-device views, live background sessions, the folder picker and new folder.
The keyboard handling ignores pinch zoom: while the visual viewport is scaled
above 1 it leaves `--h` and `--kb` alone, so zooming never resizes the app.
The phone drawer and the scrim are positioned in the page, not fixed: iOS
Safari tints the status bar from a fixed element on the top edge, and with the
scrim fixed it painted grey over the drawer. The drawer surface reaches the top
edge, with its content below the top safe area and its footer above the home
indicator.
The folder picker reads paths the way the host writes them: Unix, Windows drive
roots (`C:\`, `D:\work`, `/` accepted as the same separator, compared without
case) and UNC shares. The Roots crumb is always there so switching drives is
one tap, the Roots view marks drives with a disk icon, and `~` stands for
`C:\Users\name` too.

## Not in scope

Web Push while the page is closed (needs a push service and credentials on the
host), image attachments, editing or forking earlier turns (no Agent RPC),
free-text answers to choice questions, reading file contents.

## Verification

- `tests/remote_web.mjs` covers the new `MC` logic: incremental Markdown
  equivalence, turn folding, tool row verbs/grouping, attention groups and
  stable ordering, command and `@` filtering, template detection, queue pills,
  diff parsing, approval titles, keyboard arming.
- Server tests cover every new intent and query (scoping to the device's
  view, bounds, `stale`/`busy`, template expansion, deny with message, tool
  output append).
- Headless Edge and Playwright WebKit runs against the mock and the real
  binary: every screen in light/dark at 390×844, 360×640 and 1024×768 (and
  desktop windows from 800×900 to 2560×1440 and 125 %/150 % zoom), the session
  list reachable at every width (sidebar from 900 px, the menu button's drawer
  below, resizing across 900 px both ways without a stuck state or lost scroll),
  no console errors, the audit's B1/B2/M1 repros pass, streaming cost stays flat
  with answer length.
