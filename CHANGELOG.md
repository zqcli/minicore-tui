# Changelog

## Unreleased

- Web client: the conversation is denser and follows one rhythm. Tool and
  thinking rows are 32 px (36 px on touch) with 2 px between them, 12 px
  separates text, tools and steering inside a turn, and 24 px separates turns.
- Web client: a round ↓ button above the composer jumps to the latest message
  whenever you are scrolled well above the bottom, and becomes the "New output"
  pill when output arrived meanwhile. `End` does the same outside text fields.
- A message is never held back by a compaction that follows the previous turn: the
  Agent compacts only before a turn starts, so the "Waiting for compaction"
  state is gone and a refused message returns to the composer.
- Web client: when the next approval replaces an open one, its sheet starts
  fresh instead of keeping the deny form and reason typed for the previous one.
- iPhone: tapping the composer no longer zooms the page in and leaves it
  zoomed; the composer sits on the keyboard and returns to the bottom when the
  keyboard closes, and a pinch zoom no longer resizes the app.
- iPhone: the session drawer reaches under the status bar instead of leaving a
  grey strip above it.
- Queued messages dispatch in send order. Steers behind a follow-up wait until
  that follow-up starts its turn. Unsent steers and follow-ups can be edited or
  deleted in the web tray; `Alt+↑` takes all unsent messages back in the TUI.
  Sent steers are marked and cannot be withdrawn. Refused steers keep their
  place and wait for Agent progress or turn end before retrying. Applied steers
  also keep their order in the transcript.

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
