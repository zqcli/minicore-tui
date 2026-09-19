# Keys And Slash Commands

The keymap is fixed in `src/keymap.rs`. It is pure and compiled into the
binary; there is no user keybinding DSL. All key actions become
`AppEvent`s and are applied by `App::update`.

## Global / Editor Defaults

Focus-local Dock and main-detail bindings below take precedence. Returning to
Editor restores the existing editing and command behavior.

| Key | Behavior |
|---|---|
| `Ctrl+C` | If the composer has text, clear it. If empty, show a hint; press again within 1 second to request shutdown. |
| `Ctrl+D` | Request shutdown only when the composer is empty and the active session is idle. |
| `F1` | Open Help; press `F1` or `Esc` to close it. |
| `Ctrl+R` | Open the session selector. |
| `Ctrl+N` | Open the new-session form. |
| `Ctrl+L` | Open the model selector; updates the active session at a request boundary, or edits a new-session draft. |
| `Shift+Tab` | Open the reasoning selector from the composer; move to the previous form field in a new-session form; close the reasoning selector. |
| `Ctrl+O` | Toggle all durable tool result previews for the active session. |
| `Ctrl+T` | Show or hide durable reasoning runs. |
| `PageUp` / `PageDown` | Scroll the transcript, or page the focused selector/Help/Logs panel. |
| `Ctrl+Home` / `Ctrl+End` | Jump the transcript to the top or tail. |
| `Home` / `End` | Move to the composer line start/end; outside the composer, jump the transcript to the top/tail. |
| `F6` | With the composer dock open, switch focus between the main area and Editor. |
| `Esc` | Dismiss confirmation/search/selection/completion first, then leave a detail; only the conversation root may cancel the active operation. |
| `q` | Quit only from Help or the fatal overlay. In the composer it is an ordinary character. |
| Mouse wheel | Scroll the conversation by one row (five with Alt), a tool detail by three, or move a selector by one item. |

A release event is ignored. Repeated text and cursor events are accepted;
one-shot global shortcuts require a key press.

## Composer

| Key | Behavior |
|---|---|
| `Enter` | Submit non-empty input, or execute a slash command locally. |
| `Shift+Enter` | Insert a newline when the terminal reports Shift. |
| `Ctrl+J` | Insert a newline; reliable fallback for terminals that do not report Shift+Enter. |
| `Ctrl+A` / `Ctrl+E` | Move to the current line start/end. |
| `Ctrl+W` | Delete the previous word. |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo. |
| `Up` / `Down` | Move within multiline input; at the first/last row, navigate input history. |
| `Alt+Up` / `Alt+Down` | Previous/next history entry. |
| `Backspace` / `Delete` / arrows | Standard editing. |
| Bracketed paste | Insert the complete paste as one edit, normalize CRLF/CR to LF, and never submit automatically. |

The process keeps the last 100 non-empty submitted messages in memory. The
composer accepts at most 262144 UTF-8 bytes; near the limit it shows the byte
count and rejects an over-limit insertion. When the session is idle, `Enter`
submits a new turn. While the loop is in a
running model/tool state, `Enter` submits a mid-turn steering message via
`turn.steer`; WaitingForInput and Finishing disable submission. `Esc` remains
the cancellation action only after higher-priority views/selections are closed.

## Tool Detail (v0.3 E1)

The Tool card's existing click-to-fold behavior is unchanged. Its separate
`[详情]` title target opens a read-only main-area detail. The exact alternative is
`/tool <session_id> <loop_id> <request_index> <tool_call_id>`; names and “latest
Bash” are not identities. The session must be the active one.

The detail keeps the same Editor, per-session draft and one-line Footer.
Closing restores the conversation scroll state and never sends a cancel.

| Focus/key | Behavior |
|---|---|
| Main: `Tab` / `Shift+Tab` | Change the available stdout/stderr/result/input tab; no synthetic Changes tab. |
| Main: `PageUp` / `PageDown`, arrows, wheel | Scroll only this stream and stop follow-tail. |
| Main: `End` | Resume this stream's follow-tail. |
| Main: `F5`, `[刷新]`, or `/refresh` from Editor | Explicitly reread facts and the selected stream from byte zero. Retention may prevent recovering the prefix. |
| Main: `Ctrl+Shift+C` or `[复制]` | Copy the safe, loaded stream window (not raw control bytes); partial data is disclosed. |
| `F6` / click Editor | Move focus without submitting anything. Editor retains completion/reasoning keys; its Page keys move its cursor, not the detail. |
| `Esc` / `← 对话` | Return to the conversation without cancelling the tool. |

Letters, Enter and bracketed paste do not edit or submit the draft while the
detail main area owns focus. `/cancel` from Editor still explicitly stops the
current operation through the existing precise TurnRef/compaction path.

## Workspace Files And Literal Search (v0.3 E2)

Type `@` at a word boundary (start or after whitespace), or use `/files [path
filter]`, to open a temporary file Dock. An email `name@host` and a pasted `@`
remain ordinary text. These interfaces require an already loaded Session;
closed-history browsing offers an explicit Continue, never an automatic open.

| Dock key | Behavior |
|---|---|
| Characters / paste / Backspace / Ctrl+U | Edit or clear the current single-line query/scope; debounce is 150 ms. |
| Tab / Shift+Tab | Switch query and directory/paths input. Files are recursive. |
| Up/Down / PageUp/PageDown | Move through received candidates/matches. Files sort locally while preserving the highlighted path. |
| Enter | Insert the selected file path, enter a selected directory, or preview the selected grep match. |
| F4 / right-click a candidate | Preview a file without inserting anything into Editor. |
| Ctrl+N | Request the next opaque cursor, if present and within local retention bounds. |
| F5 | Explicit fresh scan; deadline never automatically rescans. |
| Ctrl+I (grep only) | Toggle case sensitivity and discard the old cursor/results. |
| Esc | Close this Dock before any main-detail or cancellation action. |

`/grep [literal]` is literal text, not regex or a shell command. Its paths field
accepts one relative path (spaces are literal) or a JSON string array, at most
32 paths. Use an array for paths with leading/trailing spaces or a leading `[`,
for example `["src", "dir with spaces"]`. The bounded field is 4096 bytes.
Both Docks show the last page's partial/scan-complete/stop/skipped facts. They
retain at most 500 entries/matches and 1 MiB of candidate payload, not a project
index or a claim that all files were enumerated.

A chosen path is inserted as readable JSON-quoted text, e.g.
`@"dir with spaces/file.rs"`. Quotes, backslashes and unsafe controls have
reversible escapes. **Only the path is sent if the user later submits it.**
Preview content is never attached to a Prompt, summary, or History, and preview
never calls a model: “引用路径，模型需要时再读”. The last insertion has a transient
source-position mapping for Editor `F4`; any content edit (including undo/redo)
degrades it to ordinary text. No attachment object or persistent attachment
registry is created.

### File Preview

The main-area preview preserves Editor/draft and the conversation anchor.
`F6` switches Main/Editor; the focus is shown. While Main owns focus, Page keys,
arrows and wheel scroll only the file, the blue scrollbar can be dragged, and
`End` follows the loaded tail. `Ctrl+N` / `[更多]` loads another exact range;
`F5` / `[刷新]` / `/refresh` starts a new revision from line 1. `Ctrl+Shift+C`
or `[复制]` copies the displayed immutable safe-source snapshot without line
numbers or inserted soft-wrap newlines. CRLF and no-final-newline are retained;
unsafe controls are made visible. Partial/old snapshots are disclosed.

`Esc` / `← 返回` returns to the retained results Dock or conversation. Neither
closing nor switching a detail cancels execution. Changed revisions stop
paging and retain old content until an explicit refresh. Binary, too-large and
unavailable files show their reason, not a blank successful preview. There is
no local-root reconstruction, local file scanning, or content attachment.

## Session Panel

When the session selector is open, the selected session is stored by its stable
`session_id`; refreshes, sorting, filtering, and late responses do not retarget
an action to a different row.

| Key | Behavior |
|---|---|
| `F2` | Edit the selected title; `Enter` sends `session.rename` and waits for the ACK. |
| `F5` | Refresh the session catalog while preserving the selected ID. |
| `Ctrl+W` | Begin the explicit close confirmation; unsafe busy/blocked/finishing/unsaved or unconfirmed sessions are refused. |
| `Delete` / `Ctrl+D` | Begin deletion; loaded sessions must be closed first, then a dedicated delete confirmation is required. |
| `Enter` | Open the selected session; double-clicking a session row has the same effect. |
| `Mouse click` | Select a row; footer actions remain visible in the panel. |
| `Esc` | Cancel rename/confirmation first, then close the panel. |

## Selectors

| Key | Behavior |
|---|---|
| `Up` / `Down` | Move the highlighted item. |
| `Enter` | Confirm the item or form field. |
| `Esc` | Return to the parent form/composer. |
| Printable characters | Append to the case-insensitive search query. |
| `Backspace` | Remove the last query character. |
| `Ctrl+U` | Clear the query or current editable form field. |
| `PageUp` / `PageDown` | Move by a fixed selector page. |

Session-open failure keeps the selector query and selection. New-session
creation failure keeps all form fields and re-enables the form.

## New-Session Form

The form fields are workspace, profile, model, reasoning, title, and Create.
`Tab` advances; `Shift+Tab` goes back. `Enter` opens a selector for profile,
model, or reasoning, and submits on Create. Workspace and title accept ordinary
text editing. A create request freezes the form until its response arrives.

Model and reasoning selections apply to the draft when the form is open.
With an active session they send `session.update`; the new setting is used only
at a later model request boundary, and the current tool batch keeps its old
configuration.

## Slash Commands

Only input whose first non-whitespace character is `/` is parsed locally.
Unknown commands and invalid arguments produce a local notice and no RPC.

| Command | Behavior |
|---|---|
| `/new [form]` | Quickly create here using recent explicit settings; `form` opens the full form. |
| `/resume` / `/sessions` | Continue a browsed session / open the session selector. |
| `/model` | Open the model selector for a draft or active-session update. |
| `/reasoning` | Open the reasoning selector for a draft or active-session update. |
| `/theme dark` / `/theme light` | Change the local palette; no Agent request. |
| `/clear` | Clear only the local active transcript view and reload it from the Agent; refused while a turn runs. |
| `/help` | Open Help. |
| `/logs` | Open the bounded Agent stderr log panel. |
| `/close [confirm]` | Close the active session; blocked/unsaved/running sessions require `confirm`. |
| `/delete [confirm]` | Delete the active session; destructive state requires `confirm`. |
| `/cancel` | Cancel the active loop with `turn.cancel`; the existing `turn.wait` remains in flight. |
| `/reload` | Send empty `agent.reload` params, then refresh catalogs without reloading history. |
| `/tool <session> <loop> <request_index> <call>` | Open that exact read-only tool detail. |
| `/files [path filter]` | Loaded-workspace file candidates, path insertion and explicit preview. |
| `/grep [literal]` | Loaded-workspace literal search; scope/case are edited in its Dock. |
| `/refresh` | Refresh the active detail, or explicitly refresh the conversation. |
| `/search [full] [literal]` | Search loaded content, or explicitly scan a pinned full session. |
| `/prev` / `/next` / `/latest` | Navigate user prompts. |
| `/copy [last\|message\|code\|selection]` | Copy loaded conversation content; default is the last completed reply. |
| `/export [raw] [path]` | Open the explicit bounded export form. |
| `/rename [title]` | Rename, or open its title dialog. |
| `/compact` / `/context` | Start manual compaction / read the current context snapshot (not the deferred E3 Context detail page). |
| `/settings` / `/editor` | Local preferences / external editing of the current draft only. |
| `/quit` | Request normal Agent shutdown. |

`/cancel` and `/reload` remain local command entries even when a session is
Blocked or Finishing; ordinary prompt/steer/update submissions remain refused.
The internal one-shot `turn.wait` path remains available to the reducer for
retained-result reconciliation.
The following are deliberately not implemented: `!command`, automatic file-content attachments,
`/fork`, `/branch`, `/steer`, `/queue`, `/login`,
`/plugin`, and `/mcp`.

## Status And Limits

`Ctrl+C` twice, `/quit`, idle `Ctrl+D`, or `q` in Help/fatal state enters the
same shutdown state machine. A live turn is cancelled only with its exact
`TurnRef`; the TUI then waits for an outcome and reconciles durable history.

Tools run automatically under the Agent. Bash is not sandboxed. The TUI supports
mid-turn steering via `turn.steer`. Tool detail stdout/stderr are non-PTY byte
streams, not terminal emulation. There is no approval UI, full PTY, or OSC52 copy.
