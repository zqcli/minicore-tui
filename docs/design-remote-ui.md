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
  always one tap away while anything runs; destructive actions confirm.
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
| `unqueue` | `index`, `text` | Removes the queued follow-up `queue[index]` if its text still equals `text` (else `stale`); the text goes back to the requester as `returned`. Steers already sent cannot be taken back (`stale`). |
| `answer` | + optional `message` (deny only) | Denies, then steers `message` into the same turn so the agent hears why. A blank `message`, or one beside anything but an approval's `deny`, is `bad_request`. |
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

## Client (`src/remote/web/index.html`, `sw.js`, `manifest.webmanifest`)

Single self-contained file, no build step, no external requests (CSP stays).
`<script id="core">` stays the pure, tested `globalThis.MC` module and grows;
DOM code is reorganised into small components that update only when their
inputs change (no whole-region re-renders on every `state`). Budget: at most
60 KB gzip for `index.html`.

### Layout and navigation

- **Top bar**: ≡ (badge with the number of *other* sessions that need you),
  title (tap: session info sheet; double tap or tap when scrolled: scroll to
  top), subtitle `workspace · branch`, right side: **⊕ New** and **⋯ session
  menu** (Rename, Changes, Context, Compact, Copy session id, Share/Export,
  Delete). The rename pencil goes away.
- **Drawer** (phone) / **persistent sidebar** (≥ 900 px): header (logo, host,
  ⚙ settings), New session, search, then groups **Needs you** (approval or
  question), **Running**, then Today / Yesterday / Previous 7 days / Older.
  Status by shape and colour: amber hand = needs you, spinner + elapsed =
  running, red ! = failed last turn, dot = new result. The order never changes
  while the drawer is open (rows update in place); it re-sorts when reopened.
  Footer is one line: connection dot + device name, other devices count.
- **Settings** (full-screen sheet): Appearance (System/Light/Dark), text size
  (4 steps), wrap code by default, show thinking expanded, Enter sends (for
  hardware keyboards; default off on touch), keep screen awake while a turn
  runs (Wake Lock), notifications while this page is open (Notification API
  through the service worker, only where supported, asked on first use),
  connected devices with what each views, host and version (`welcome.version`),
  install row, sign out (confirm).
- **Sheets** share one modal manager: focus moves in and back, background is
  `inert`, `aria-modal`, Esc / swipe down closes, the keyboard lift (`--kb`)
  applies to all.

### Composer

- Two-row card at the bottom: auto-growing textarea (max 40 % of the visual
  viewport), then a chip row (at least 44 px tall, so nothing moves when
  the buttons change): **+** (sheet: prompt templates and commands, recent
  prompts, insert `@file`), **model · effort** chip (one chip, middle
  truncated, opens the model sheet), in a draft also **profile** and
  **folder** chips; on the right the send controls: the round **↑ send**
  button when idle, nothing while a turn runs and the composer is empty, and
  while a turn runs with text typed two explicit buttons 8 px apart, both at
  least 44 px tall: secondary **Queue** (follow-up icon) and primary **Steer**
  (↗ icon). Both labels show at every width; the model chip drops its effort
  and truncates in the middle to make room. Hover devices get tooltips with
  the shortcuts.
- **Stop** is a separate, always visible button while a turn runs or
  compaction is under way (in the status strip above the composer, ≥ 44 px),
  never in the send position. Esc stops on hardware keyboards.
- **Steer** sends the text into the running turn (`submit`; the agent reads it
  after the current step); **Queue** sends a follow-up (`queue`; sent when the
  turn ends). Hardware keys mirror the TUI: Enter (with "Enter sends") or
  Cmd/Ctrl+Enter steers while busy and sends when idle; **Tab** or **Alt+Enter**
  queue while busy with text typed and no `/` or `@` list open (the list keeps
  Tab and Enter for completion). The placeholder says so ("Steer the agent… ·
  Tab to queue a follow-up").
- `/` at the start opens a filtered command list: built-ins handled by the
  client (`/new`, `/model`, `/rename`, `/compact`, `/context`, `/changes`,
  `/export`, `/settings`) and `state.commands` templates (sent as text, the
  host expands them). `@` opens a file list (`files` query, debounced) and
  inserts `@path`. Both are keyboard and touch navigable.
- Recent prompts (last 50 per host, `localStorage`), reachable from **+** and
  with ↑ in an empty composer on hardware keyboards.
- `enterkeyhint="send"`, `autocapitalize="sentences"`, autocorrect off while
  the text starts with `/` or `@`; drafts persist per session as today.
- The queue shows above the composer under one caption, "Follow-ups send when
  this turn ends": follow-ups are pills `↳ Follow-up 1` + text (tap to edit =
  unqueue into the composer, ✕ to remove = unqueue and drop), steers the agent
  has not read yet are muted `↗ Steer · waiting for the next step` rows that
  cannot be removed. Three show at first, then "+N more" expands the rest. A
  steer's bubble in the transcript keeps its "↗ Steer" label; a follow-up that
  was sent is an ordinary message. Unconfirmed sends keep their single "Not
  sent · Send again" card; the composer is not filled twice.

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
  Tapping the header or the line expands the full text and back; the "show
  thinking expanded" setting opens it by default.
- **Tools**: one-line rows: state icon · past-tense verb · muted mono target ·
  right-aligned meta (duration, `+12 −4`, exit code). Consecutive reads and
  searches group ("Read 6 files ›"). Tap expands inline: command, the **end**
  of the output (last 20 lines, scrolled to the bottom), Copy, Wrap, "Open"
  (full-screen output viewer: tail first, wrap toggle, copy, **Load full
  output** via `tool_output`). Failed tools auto-expand their last lines.
  Edits render as diffs (below).
- **Turn folding**: when a turn ends, its tool rows and intermediate thinking
  fold into one summary row "Worked 47 s · 5 tools · +12 −4 ›" placed where
  the turn's work started; the final answer stays visible. Expanding adds a
  trailing "Hide" row; anchoring keeps the tapped row in place. The running
  turn is never folded.
- **Turn end** line stays (completed / interrupted / failed, duration, clock
  time, tokens from `usage`); tapping "+12 −4" opens Changes.
- **Diffs** (tool rows, approvals, Changes): per-file header, line numbers,
  add/remove tints, wrap on by default on phones, horizontal scroll per hunk
  when off.
- **Code blocks**: language label, Copy, Wrap; tables scroll with an edge
  shadow; nothing clips without a hint.
- **Scroll**: auto-follow only near the bottom; otherwise a "↓ New output" pill
  above the composer. Older history keeps the existing paging, with one
  consistent "Load earlier" control; the mounted window is capped at both ends.
- **Search in session** (⋯ → Find, or Cmd/Ctrl+F): highlights matches in loaded
  blocks, next/previous.

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
- The decision stays visible in the transcript on the tool row (allowed /
  denied, by this device or another).

### Changes, context, session info

- **Changes sheet** (⋯ → Changes, turn-end `+n −m`, `/changes`): branch,
  totals, file list with status and counts; tap a file for its diff (`diff`
  query). Pull to refresh.
- **Context sheet** (⋯ → Context, the `% context` in the status strip):
  breakdown bars from `context`, and **Compact now** (confirm).
- **Session info** (tap the title): title, id (copy), workspace, branch,
  profile, model, created/updated, usage totals, other devices viewing it.
- **Export/Share**: the loaded transcript as Markdown through Web Share, else
  copied.

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
- Touch targets ≥ 44 × 44 px with 8 px between destructive and primary.
- Motion: 120–180 ms for small state changes, 220–280 ms for sheets and the
  drawer, `cubic-bezier(.2,.8,.2,1)`; no per-token animation;
  `prefers-reduced-motion` turns shimmer and slides into fades.
- Text size setting scales rem-based type; inputs stay ≥ 16 px.

### Accessibility

`aria-live="polite"` region announcing turn start/end, approvals and errors
(state changes, not tokens); the status strip is updated in place, not
recreated; landmarks and a real heading for the session title; labelled icon
buttons; status never by colour alone.

### iPad and desktop

≥ 900 px: persistent sidebar + transcript; sheets become centred dialogs or a
right-hand panel (Changes, output viewer, context) at ≥ 1200 px. Shortcuts:
Esc stop (or close the top sheet), Cmd/Ctrl+K command list, Cmd/Ctrl+Enter
send or steer (Enter does when the setting is on; Tab or Alt+Enter queue),
Cmd/Ctrl+F find, Alt+↑/↓ previous/next session, `?` shortcut help.

## Kept from v1

Password sign-in and remembered tokens, the connection lifecycle (backoff,
wake on visibility/pageshow/focus/online, unconfirmed sends never resent
automatically), host clock offset, the iOS keyboard handling (`--h`, `--kb`),
safe areas, the PWA (manifest, service worker, install row, theme-color sync),
per-device views, live background sessions, the folder picker and new folder.

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
  binary: every screen in light/dark at 390×844, 360×640 and 1024×768, no
  console errors, the audit's B1/B2/M1 repros pass, streaming cost stays flat
  with answer length.
